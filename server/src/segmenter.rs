//! 单路码率的切片状态机：收帧 -> B帧时间戳处理 -> 切 part/segment -> 维护滑动窗口。
//! 断流重连通过 begin_session + discontinuity + 时间戳 rebase 处理。

use crate::fmp4::{self, AudioConfig, Sample, VideoConfig};
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

pub const TICKS_PER_SEC: i64 = 90000;
/// AAC 48kHz 一帧 1024 采样 = 1920 个 90kHz tick
const AAC_FRAME_TICKS: i64 = 1920;

pub struct RenditionConfig {
    pub name: &'static str,
    pub bandwidth: u32,
    pub seg_target: i64,  // 90kHz
    pub part_target: i64, // 90kHz
    pub window: usize,    // 保留的完整 segment 数
}

pub struct Part {
    pub index: u32,
    pub start_dts: i64,
    pub duration: i64,
    pub independent: bool,
    pub data: Arc<Bytes>,
}

pub struct Segment {
    pub msn: u64,
    #[allow(dead_code)]
    pub start_dts: i64,
    pub duration: i64,
    pub discontinuity: bool,
    pub pdt_ms: i64,
    pub parts: Vec<Part>,
    pub data: Arc<Bytes>,
}

struct OpenSegment {
    start_dts: i64,
    discontinuity: bool,
    pdt_ms: i64,
    parts: Vec<Part>,
    cur_v: Vec<Sample>,
    cur_a: Vec<Sample>,
    part_start_dts: i64,
}

pub struct Rendition {
    pub cfg: RenditionConfig,
    pub video_cfg: Option<VideoConfig>,
    pub audio_cfg: Option<AudioConfig>,
    pub codec_string: String,
    init_gen: u64,
    init: Option<Arc<Bytes>>,
    segments: VecDeque<Segment>,
    open: Option<OpenSegment>,
    msn_base: u64,
    moof_seq: u32,
    // 输入时间戳 -> 输出时间轴的映射（音视频各自维护）
    v_offset: i64,
    a_offset: i64,
    seen_v: bool,
    seen_a: bool,
    last_v_out: i64,
    last_a_out: i64,
    frame_dur: i64,
    // PROGRAM-DATE-TIME 锚点：wall = anchor_wall_ms + (dts - anchor_dts)/90000
    anchor_wall_ms: i64,
    anchor_dts: i64,
    disc_pending: bool,
    drop_until_key: bool,
    ever_had_content: bool,
    pending_audio: Vec<Sample>,
    last_input_ms: i64,
    pub version: u64,
    pub watch_tx: watch::Sender<u64>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Rendition {
    pub fn new(cfg: RenditionConfig) -> Self {
        Rendition {
            cfg,
            video_cfg: None,
            audio_cfg: None,
            codec_string: "avc1.640028,mp4a.40.2".to_string(),
            init_gen: 0,
            init: None,
            segments: VecDeque::new(),
            open: None,
            msn_base: 0,
            moof_seq: 1,
            v_offset: 0,
            a_offset: 0,
            seen_v: false,
            seen_a: false,
            last_v_out: 0,
            last_a_out: 0,
            frame_dur: 3000, // 默认 30fps
            anchor_wall_ms: 0,
            anchor_dts: 0,
            disc_pending: false,
            drop_until_key: false,
            ever_had_content: false,
            pending_audio: Vec::new(),
            last_input_ms: 0,
            version: 0,
            watch_tx: watch::channel(0).0,
        }
    }

    fn bump_version(&mut self) {
        self.version += 1;
        // watch 是广播语义，500 个等在 blocking reload 上的客户端都会醒
        self.watch_tx.send_replace(self.version);
    }

    /// 新的推流连接建立。已有内容的话：标记 discontinuity、
    /// 丢弃非关键帧直到下一个 IDR（断流期间解码器参考帧已失效）。
    pub fn begin_session(&mut self) {
        if self.ever_had_content {
            self.disc_pending = true;
            self.drop_until_key = true;
            self.seen_v = false;
            self.seen_a = false;
            self.pending_audio.clear();
            tracing::info!(rend = self.cfg.name, "new ingest session, discontinuity pending");
        }
    }

    /// 推流连接断开。把当前 part 收尾，给客户端一个干净的尾巴。
    pub fn end_session(&mut self) {
        if self.open.is_some() {
            self.close_part();
        }
    }

    pub fn set_audio_config(&mut self, asc: Vec<u8>, sample_rate: u32, channels: u16) {
        let changed = match &self.audio_cfg {
            Some(c) => c.asc != asc,
            None => true,
        };
        if changed {
            self.audio_cfg = Some(AudioConfig {
                asc,
                sample_rate,
                channels,
            });
            self.rebuild_init();
        }
    }

    fn maybe_set_video_config(&mut self, sps: Option<Vec<u8>>, pps: Option<Vec<u8>>) {
        let (Some(sps), Some(pps)) = (sps, pps) else { return };
        let changed = match &self.video_cfg {
            Some(c) => c.sps != sps || c.pps != pps,
            None => true,
        };
        if !changed {
            return;
        }
        if let Ok(parsed) = crate::h264::Sps::parse(&sps) {
            self.video_cfg = Some(VideoConfig {
                width: parsed.width as u16,
                height: parsed.height as u16,
                sps: sps.clone(),
                pps,
            });
            self.codec_string = format!(
                "{},mp4a.40.2",
                crate::h264::avc_codec_string(&sps)
            );
            self.rebuild_init();
        }
    }

    fn rebuild_init(&mut self) {
        if let (Some(v), Some(a)) = (&self.video_cfg, &self.audio_cfg) {
            let bytes = fmp4::init_segment(v, a);
            self.init = Some(Arc::new(Bytes::from(bytes)));
            self.init_gen += 1;
            self.bump_version();
        }
    }

    /// 推入一个视频 access unit（已转 AVCC）。
    pub fn push_video(
        &mut self,
        pts: i64,
        dts: i64,
        key: bool,
        avcc: Bytes,
        sps: Option<Vec<u8>>,
        pps: Option<Vec<u8>>,
    ) {
        self.maybe_set_video_config(sps, pps);
        self.last_input_ms = now_ms();

        if self.drop_until_key && !key {
            return;
        }
        if self.drop_until_key && key {
            // 断流后第一个 IDR：强行结束旧 segment，从干净的关键帧重新开始
            self.drop_until_key = false;
            self.force_close_segment();
        }

        let out_dts = self.map_video_dts(dts);
        if self.last_v_out > 0 {
            let d = out_dts - self.last_v_out;
            if (1..=9000).contains(&d) {
                self.frame_dur = d;
            }
        }

        // GOP 边界且时长够了 -> 切 segment
        if key {
            let should_cut = self
                .open
                .as_ref()
                .map(|o| out_dts - o.start_dts >= self.cfg.seg_target)
                .unwrap_or(false);
            if should_cut {
                self.close_segment();
            }
        }
        self.ensure_open(out_dts);

        // part 边界
        let need_part_cut = self
            .open
            .as_ref()
            .map(|o| !o.cur_v.is_empty() && out_dts - o.part_start_dts >= self.cfg.part_target)
            .unwrap_or(false);
        if need_part_cut {
            self.close_part();
            if let Some(open) = self.open.as_mut() {
                open.part_start_dts = out_dts;
            }
        }

        let cts = (pts - dts).clamp(0, 1 << 30) as i32;
        let open = self.open.as_mut().unwrap();
        open.cur_v.push(Sample {
            dts: out_dts,
            cts,
            dur: 0, // 渲染时按相邻样本差值回填
            key,
            data: avcc,
        });
        self.last_v_out = out_dts;
        self.ever_had_content = true;
    }

    /// 推入一帧 raw AAC。
    pub fn push_audio(&mut self, pts: i64, dts: i64, data: Bytes) {
        self.last_input_ms = now_ms();
        if self.drop_until_key {
            return;
        }
        let out_dts = self.map_audio_dts(dts);
        let s = Sample {
            dts: out_dts,
            cts: (pts - dts).clamp(0, 1 << 30) as i32,
            dur: 0,
            key: true,
            data,
        };
        if self.open.is_none() {
            // 音频比第一个视频帧先到，暂存（防止一直没视频时无限增长）
            if self.pending_audio.len() < 100 {
                self.pending_audio.push(s);
            }
        } else {
            self.open.as_mut().unwrap().cur_a.push(s);
        }
        self.last_a_out = out_dts;
    }

    fn map_video_dts(&mut self, dts: i64) -> i64 {
        if !self.seen_v {
            self.v_offset = if self.last_v_out > 0 {
                // 重连：接续旧时间轴
                self.last_v_out + self.frame_dur - dts
            } else {
                -dts
            };
            self.seen_v = true;
            self.anchor_wall_ms = now_ms();
            self.anchor_dts = dts + self.v_offset;
        }
        let mut out = dts + self.v_offset;
        if out <= self.last_v_out && self.last_v_out > 0 {
            // 时间戳回退（推流端重启等）：整体平移保持单调
            self.v_offset += self.last_v_out + self.frame_dur - out;
            out = dts + self.v_offset;
            self.anchor_wall_ms = now_ms();
            self.anchor_dts = out;
        }
        out
    }

    fn map_audio_dts(&mut self, dts: i64) -> i64 {
        if !self.seen_a {
            self.a_offset = if self.last_a_out > 0 {
                self.last_a_out + AAC_FRAME_TICKS - dts
            } else {
                -dts
            };
            self.seen_a = true;
        }
        let mut out = dts + self.a_offset;
        if out <= self.last_a_out && self.last_a_out > 0 {
            self.a_offset += self.last_a_out + AAC_FRAME_TICKS - out;
            out = dts + self.a_offset;
        }
        out
    }

    fn ensure_open(&mut self, start_dts: i64) {
        if self.open.is_none() {
            let pdt_ms = self.anchor_wall_ms + (start_dts - self.anchor_dts) * 1000 / TICKS_PER_SEC;
            self.open = Some(OpenSegment {
                start_dts,
                discontinuity: std::mem::take(&mut self.disc_pending),
                pdt_ms,
                parts: Vec::new(),
                cur_v: Vec::new(),
                cur_a: Vec::new(),
                part_start_dts: start_dts,
            });
            // 把先于视频到达的音频挪进来
            if !self.pending_audio.is_empty() {
                let mut pending = std::mem::take(&mut self.pending_audio);
                self.open.as_mut().unwrap().cur_a.append(&mut pending);
            }
        }
    }

    /// 把当前累积的样本渲染成一个 part。
    fn close_part(&mut self) {
        let Some(open) = &mut self.open else { return };
        if open.cur_v.is_empty() && open.cur_a.is_empty() {
            return;
        }
        fix_durations(&mut open.cur_v, self.frame_dur as u32);
        fix_durations(&mut open.cur_a, AAC_FRAME_TICKS as u32);
        let cur_v = std::mem::take(&mut open.cur_v);
        let cur_a = std::mem::take(&mut open.cur_a);
        let start = first_dts(&cur_v).or_else(|| first_dts(&cur_a)).unwrap_or(0);
        let end = cur_v
            .last()
            .map(|s| s.dts + s.dur as i64)
            .unwrap_or(start)
            .max(cur_a.last().map(|s| s.dts + s.dur as i64).unwrap_or(start));
        let independent = cur_v.first().map(|s| s.key).unwrap_or(false);
        let data = fmp4::media_segment(self.moof_seq, &cur_v, &cur_a);
        self.moof_seq += 1;
        let index = open.parts.len() as u32;
        open.parts.push(Part {
            index,
            start_dts: start,
            duration: (end - start).max(1),
            independent,
            data: Arc::new(Bytes::from(data)),
        });
        self.bump_version();
    }

    /// 关闭当前 segment（调用前保证在关键帧边界或强制收尾）。
    fn close_segment(&mut self) {
        self.close_part();
        let Some(open) = self.open.take() else { return };
        if open.parts.is_empty() {
            return;
        }
        let start = open.start_dts;
        let end = open
            .parts
            .last()
            .map(|p| p.start_dts + p.duration)
            .unwrap_or(start);
        let mut data = Vec::new();
        for p in &open.parts {
            data.extend_from_slice(&p.data);
        }
        let msn = self.msn_base + self.segments.len() as u64;
        self.segments.push_back(Segment {
            msn,
            start_dts: start,
            duration: (end - start).max(1),
            discontinuity: open.discontinuity,
            pdt_ms: open.pdt_ms,
            parts: open.parts,
            data: Arc::new(Bytes::from(data)),
        });
        while self.segments.len() > self.cfg.window {
            self.segments.pop_front();
            self.msn_base += 1;
        }
        self.bump_version();
    }

    fn force_close_segment(&mut self) {
        if self.open.is_some() {
            self.close_segment();
        }
    }

    // ---------- 查询接口（playlist / HTTP 用） ----------

    pub fn init(&self) -> Option<(u64, Arc<Bytes>)> {
        self.init.clone().map(|b| (self.init_gen, b))
    }

    /// 当前（未闭合）segment 的 MSN。
    pub fn cur_msn(&self) -> u64 {
        self.msn_base + self.segments.len() as u64
    }

    pub fn msn_base(&self) -> u64 {
        self.msn_base
    }

    pub fn segments(&self) -> &VecDeque<Segment> {
        &self.segments
    }

    pub fn open_parts(&self) -> &[Part] {
        self.open.as_ref().map(|o| o.parts.as_slice()).unwrap_or(&[])
    }

    pub fn open_discontinuity(&self) -> bool {
        self.open.as_ref().map(|o| o.discontinuity).unwrap_or(false)
    }


    pub fn get_segment(&self, msn: u64) -> Option<Arc<Bytes>> {
        if msn < self.msn_base {
            return None;
        }
        self.segments
            .get((msn - self.msn_base) as usize)
            .map(|s| s.data.clone())
    }

    pub fn get_part(&self, msn: u64, index: u32) -> Option<Arc<Bytes>> {
        if msn == self.cur_msn() {
            return self
                .open
                .as_ref()
                .and_then(|o| o.parts.get(index as usize))
                .map(|p| p.data.clone());
        }
        if msn >= self.msn_base && msn < self.cur_msn() {
            return self
                .segments
                .get((msn - self.msn_base) as usize)
                .and_then(|s| s.parts.get(index as usize))
                .map(|p| p.data.clone());
        }
        None
    }

    /// 下一个将要产出的 part（用于 PRELOAD-HINT 和请求挂起）。
    pub fn next_part_hint(&self) -> (u64, u32) {
        (self.cur_msn(), self.open_parts().len() as u32)
    }

    /// 推流已超过 threshold 没有新数据（断流中），此时不应让客户端干等。
    pub fn is_stalled(&self, threshold_ms: i64) -> bool {
        self.ever_had_content && now_ms() - self.last_input_ms > threshold_ms
    }

    /// blocking reload 判定：播放列表是否已推进到 (msn, part) 之后。
    pub fn is_satisfied(&self, msn: u64, part: u32) -> bool {
        if msn < self.msn_base {
            return true; // 太旧了，直接给当前列表让客户端自己追
        }
        if msn > self.cur_msn() {
            return false;
        }
        if msn < self.cur_msn() {
            return true; // 该 segment 已闭合
        }
        self.open_parts().len() > part as usize
    }

}

fn first_dts(samples: &[Sample]) -> Option<i64> {
    samples.first().map(|s| s.dts)
}

/// 按相邻样本 dts 差回填时长，最后一个用默认值。
fn fix_durations(samples: &mut [Sample], default: u32) {
    let n = samples.len();
    for i in 0..n {
        let next = samples.get(i + 1).map(|s| s.dts);
        samples[i].dur = match next {
            Some(d) => (d - samples[i].dts).clamp(1, i64::MAX) as u32,
            None => default,
        };
    }
}
