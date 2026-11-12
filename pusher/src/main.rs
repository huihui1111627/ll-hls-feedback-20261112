//! 测试推流工具：读 ffmpeg 产出的 H.264 Annex-B / ADTS AAC / 时间戳文件，
//! 按自定义帧协议通过 TCP 推进服务端。支持循环、模拟断流。

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use clap::Parser;
use proto::{FrameHeader, CODEC_AAC, CODEC_H264, FLAG_CONFIG, FLAG_KEYFRAME};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::{sleep_until, Instant};

#[derive(Parser)]
#[command(name = "pusher", about = "LL-HLS 测试推流工具")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:9000")]
    addr: String,
    /// Annex-B H.264 文件（须带 AUD，建议 x264 aud=1:repeat-headers=1）
    #[arg(long)]
    video: String,
    /// ffprobe 导出的每包 pts_time,dts_time CSV
    #[arg(long)]
    video_ts: String,
    /// ADTS AAC 文件
    #[arg(long)]
    audio: String,
    #[arg(long, default_value_t = 0)]
    video_id: u8,
    #[arg(long, default_value_t = 2)]
    audio_id: u8,
    /// 循环推流
    #[arg(long, default_value_t = false)]
    r#loop: bool,
    /// 模拟断流，格式 "开始秒:时长秒"，可重复，例如 --drop 30:5
    #[arg(long = "drop")]
    drops: Vec<String>,
    /// 倍速（调试用）
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
}

struct VideoAu {
    dts: i64,
    pts: i64,
    key: bool,
    data: Bytes,
}

struct AudioFrame {
    dts: i64,
    data: Bytes,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(args))
}

async fn run(args: Args) -> Result<()> {
    let video_data = std::fs::read(&args.video)
        .with_context(|| format!("read {}", args.video))?;
    let ts_data = std::fs::read_to_string(&args.video_ts)
        .with_context(|| format!("read {}", args.video_ts))?;
    let audio_data = std::fs::read(&args.audio)
        .with_context(|| format!("read {}", args.audio))?;

    let aus = build_video_aus(&video_data, &ts_data)?;
    let (audio_frames, asc, sample_rate, channels) = build_audio_frames(&audio_data)?;

    tracing::info!(
        "loaded {} video AUs ({} keyframes), {} audio frames, asc={:02x?} {}Hz {}ch",
        aus.len(),
        aus.iter().filter(|a| a.key).count(),
        audio_frames.len(),
        asc,
        sample_rate,
        channels
    );

    let drops: Vec<(f64, f64)> = args
        .drops
        .iter()
        .map(|s| {
            let (a, b) = s.split_once(':').context("drop 格式应为 开始秒:时长秒")?;
            Ok((a.parse::<f64>()?, b.parse::<f64>()?))
        })
        .collect::<Result<Vec<_>>>()?;

    // 合并音视频事件，按 dts 排序
    #[derive(Clone, Copy)]
    enum Ev {
        V(usize),
        A(usize),
    }
    let mut events: Vec<(i64, Ev)> = Vec::with_capacity(aus.len() + audio_frames.len());
    for (i, au) in aus.iter().enumerate() {
        events.push((au.dts, Ev::V(i)));
    }
    for (i, f) in audio_frames.iter().enumerate() {
        events.push((f.dts, Ev::A(i)));
    }
    events.sort_by_key(|(dts, _)| *dts);

    let start = Instant::now();
    let mut sock: Option<TcpStream> = None;
    let mut base_offset: i64 = 0; // 循环时时间轴续接
    let mut drop_state: Option<f64> = None; // 当前断流结束的流内时间（秒）

    'outer: loop {
        let loop_start_sec = base_offset as f64 / 90000.0;
        for (dts, ev) in &events {
            let dts = dts + base_offset;
            let stream_sec = dts as f64 / 90000.0 / args.speed;
            let loop_sec = stream_sec - loop_start_sec; // 每轮循环内的相对时间
            // 首帧 dts 可能为负（B 帧延迟），按 0 处理
            sleep_until(start + Duration::from_secs_f64(stream_sec.max(0.0))).await;

            // 断流调度
            if let Some(end) = drop_state {
                if stream_sec < end {
                    continue; // 断流窗口内的内容直接丢弃
                }
                drop_state = None;
            }
            if let Some((begin, dur)) = drops
                .iter()
                .find(|(b, d)| loop_sec >= *b && loop_sec < *b + *d)
                .copied()
            {
                if sock.is_some() {
                    tracing::warn!(at = begin, "simulating disconnect for {}s", dur);
                    sock = None; // drop 关闭连接
                    drop_state = Some((stream_sec - loop_sec) + begin + dur); // 绝对流内时间
                    continue;
                }
            }

            // 确保连接 + 音频配置
            if sock.is_none() {
                match connect_with_retry(&args.addr).await {
                    Ok(s) => {
                        sock = Some(s);
                        if !send_frame(sock.as_mut().unwrap(), args.audio_id, FLAG_CONFIG, 0, 0, &asc).await? {
                            sock = None;
                            continue;
                        }
                        tracing::info!("connected to {}", args.addr);
                    }
                    Err(e) => {
                        tracing::error!("connect failed: {e}, retrying...");
                        continue;
                    }
                }
            }

            match ev {
                Ev::V(i) => {
                    let au = &aus[*i];
                    let flags = if au.key { FLAG_KEYFRAME } else { 0 };
                    let ok = send_frame(
                        sock.as_mut().unwrap(),
                        args.video_id,
                        flags,
                        au.pts + base_offset,
                        au.dts + base_offset,
                        &au.data,
                    )
                    .await?;
                    if !ok {
                        sock = None;
                    }
                }
                Ev::A(i) => {
                    let f = &audio_frames[*i];
                    let ok = send_frame(
                        sock.as_mut().unwrap(),
                        args.audio_id,
                        0,
                        f.dts + base_offset,
                        f.dts + base_offset,
                        &f.data,
                    )
                    .await?;
                    if !ok {
                        sock = None;
                    }
                }
            }


        }

        if !args.r#loop {
            break 'outer;
        }
        // 循环：时间轴继续往后接
        base_offset = *events.last().map(|(d, _)| d).unwrap_or(&0) + base_offset + 3000;
    }

    if let Some(mut s) = sock {
        let _ = s.shutdown().await;
    }
    Ok(())
}

async fn connect_with_retry(addr: &str) -> Result<TcpStream> {
    let mut backoff = 200u64;
    loop {
        match TcpStream::connect(addr).await {
            Ok(s) => {
                s.set_nodelay(true).ok();
                return Ok(s);
            }
            Err(_) => {
                sleep_until(Instant::now() + Duration::from_millis(backoff)).await;
                backoff = (backoff * 2).min(2000);
            }
        }
    }
}

async fn send_frame(
    sock: &mut TcpStream,
    stream_id: u8,
    flags: u8,
    pts: i64,
    dts: i64,
    data: &[u8],
) -> Result<bool> {
    let codec = if flags & FLAG_CONFIG != 0 {
        CODEC_AAC
    } else if stream_id <= 1 {
        CODEC_H264
    } else {
        CODEC_AAC
    };
    let h = FrameHeader {
        stream_id,
        codec,
        flags,
        pts,
        dts,
        len: data.len() as u32,
    };
    let res = async {
        sock.write_all(&h.encode()).await?;
        sock.write_all(data).await
    }
    .await;
    // 断开的连接不致命 —— 返回 false 让调用方下次重连
    Ok(res.is_ok())
}

/// 按 AUD 切分 access unit，并从 ffprobe 的 pts/dts 表取时间戳。
fn build_video_aus(data: &[u8], ts_data: &str) -> Result<Vec<VideoAu>> {
    let ranges = nal_ranges(data);
    if ranges.is_empty() {
        bail!("no NAL units found in video file");
    }

    // 按 AUD(9) 分组；没有 AUD 时退化为「遇到切片的 first_mb_in_slice==0 就新开一个 AU」
    let first_aud = ranges.iter().find_map(|&(s, e)| {
        let nal_start = skip_start_code(data, s);
        (e > nal_start && data[nal_start] & 0x1F == 9).then_some(s)
    });
    let has_aud = first_aud.is_some();

    let mut groups: Vec<(usize, usize)> = Vec::new(); // (byte start, byte end)
    // 有 AUD 时从第一个 AUD 开始（x264 会在首个 AUD 前写一个 SEI，不属于任何 AU）
    let mut cur_start = first_aud.unwrap_or(ranges[0].0);
    let mut seen_slice = false;
    for &(s, e) in &ranges {
        let ns = skip_start_code(data, s);
        if e <= ns {
            continue;
        }
        let t = data[ns] & 0x1F;
        let is_new_au = if has_aud {
            t == 9 && s != cur_start
        } else {
            (t == 1 || t == 5) && first_mb_is_zero(&data[ns + 1..e]) && seen_slice
        };
        if is_new_au {
            groups.push((cur_start, s));
            cur_start = s;
            seen_slice = false;
        }
        if t == 1 || t == 5 {
            seen_slice = true;
        }
    }
    groups.push((cur_start, data.len()));

    let ts = parse_ts_csv(ts_data)?;
    if groups.len() != ts.len() {
        bail!(
            "AU count {} != timestamp rows {} (ffprobe 可能没把每个 AU 算成包)",
            groups.len(),
            ts.len()
        );
    }
    let mut aus = Vec::with_capacity(groups.len());
    for (i, &(s, e)) in groups.iter().enumerate() {
        let key = nal_has_type(data, s, e, 5);
        aus.push(VideoAu {
            dts: ts[i].1,
            pts: ts[i].0,
            key,
            data: Bytes::copy_from_slice(&data[s..e]),
        });
    }
    Ok(aus)
}

fn nal_has_type(data: &[u8], start: usize, end: usize, want: u8) -> bool {
    let mut i = start;
    while i + 3 <= end {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let ns = i + 3;
            if ns < end && data[ns] & 0x1F == want {
                return true;
            }
            i = ns;
        } else {
            i += 1;
        }
    }
    false
}

fn first_mb_is_zero(payload: &[u8]) -> bool {
    // slice header 去掉 emulation prevention 后的第一个 ue 是 first_mb_in_slice
    let mut rbsp = Vec::with_capacity(payload.len().min(32));
    let mut zeros = 0;
    for &b in payload.iter().take(64) {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        rbsp.push(b);
    }
    // ue: leading zeros >= 1 且数值为 0 -> 第一个 bit 就是 1
    rbsp.first().map(|b| b & 0x80 != 0).unwrap_or(false)
}

/// 返回每个 NAL 的 (start code 起点, NAL 结束)。中间 00 被视作前缀。
fn nal_ranges(data: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut cur: Option<usize> = None;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = cur.take() {
                out.push((s, i));
            }
            cur = Some(i);
            i += 3;
        } else if i + 4 <= data.len()
            && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1
        {
            if let Some(s) = cur.take() {
                out.push((s, i));
            }
            cur = Some(i);
            i += 4;
        } else {
            i += 1;
        }
    }
    if let Some(s) = cur {
        out.push((s, data.len()));
    }
    out
}

fn skip_start_code(data: &[u8], start: usize) -> usize {
    if start + 3 < data.len() && data[start..start + 3] == [0, 0, 1] {
        start + 3
    } else {
        start + 4
    }
}

/// CSV：每行两点 pts_time,dts_time（秒），转为 90kHz。
fn parse_ts_csv(text: &str) -> Result<Vec<(i64, i64)>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split(',');
        let (Some(pts), Some(dts)) = (parts.next(), parts.next()) else {
            bail!("bad ts row: {line}");
        };
        if pts == "N/A" || dts == "N/A" {
            continue;
        }
        let p: f64 = pts.parse().with_context(|| format!("bad pts {pts}"))?;
        let d: f64 = dts.parse().with_context(|| format!("bad dts {dts}"))?;
        out.push(((p * 90000.0).round() as i64, (d * 90000.0).round() as i64));
    }
    Ok(out)
}

/// 解析 ADTS：返回 (帧列表, ASC, 采样率, 声道数)。
fn build_audio_frames(data: &[u8]) -> Result<(Vec<AudioFrame>, Vec<u8>, u32, u16)> {
    let mut frames = Vec::new();
    let mut asc = None;
    let mut rate = 48000u32;
    let mut channels = 2u16;
    let mut pos = 0usize;
    let mut idx = 0i64;
    while pos + 7 <= data.len() {
        if data[pos] != 0xFF || (data[pos + 1] & 0xF0) != 0xF0 {
            bail!("ADTS sync lost at {pos}");
        }
        let protection_absent = data[pos + 1] & 0x01;
        let profile = (data[pos + 2] >> 6) & 0x03;
        let freq_idx = (data[pos + 2] >> 2) & 0x0F;
        let chan = ((data[pos + 2] & 0x01) << 2) | ((data[pos + 3] >> 6) & 0x03);
        let frame_len = (((data[pos + 3] & 0x03) as usize) << 11)
            | ((data[pos + 4] as usize) << 3)
            | ((data[pos + 5] as usize) >> 5);
        let header_len = if protection_absent == 1 { 7 } else { 9 };
        if frame_len < header_len || pos + frame_len > data.len() {
            bail!("bad ADTS frame length at {pos}");
        }
        if asc.is_none() {
            rate = match freq_idx {
                3 => 48000,
                4 => 44100,
                5 => 32000,
                _ => 44100,
            };
            channels = chan as u16;
            let aot = profile + 1;
            asc = Some(vec![
                (aot << 3) | (freq_idx >> 1),
                ((freq_idx & 1) << 7) | ((chan as u8) << 3),
            ]);
            tracing::info!("ADTS: {}Hz {}ch asc={:02x?}", rate, channels, asc);
        }
        let dts = idx * 1920; // 1024 samples @48kHz -> 90kHz
        frames.push(AudioFrame {
            dts,
            data: Bytes::copy_from_slice(&data[pos + header_len..pos + frame_len]),
        });
        pos += frame_len;
        idx += 1;
    }
    let asc = asc.context("no ADTS frames found")?;
    Ok((frames, asc, rate, channels))
}
