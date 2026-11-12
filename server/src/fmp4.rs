//! 纯手写 fMP4 封装：init segment（ftyp+moov）和 media segment（styp+moof+mdat）。
//! 视频 track id=1，音频 track id=2，两个 track 的 timescale 都是 90000。

use bytes::Bytes;

pub const TIMESCALE: u32 = 90000;
pub const VIDEO_TRACK_ID: u32 = 1;
pub const AUDIO_TRACK_ID: u32 = 2;

#[derive(Clone)]
pub struct VideoConfig {
    pub width: u16,
    pub height: u16,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

#[derive(Clone)]
pub struct AudioConfig {
    /// AudioSpecificConfig（通常 2 字节）
    pub asc: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u16,
}

pub struct Sample {
    pub dts: i64,
    /// composition time offset = pts - dts（90kHz，B 帧 > 0）
    pub cts: i32,
    pub dur: u32,
    pub key: bool,
    pub data: Bytes,
}

// ---------- box 基础 ----------

fn bx(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(body);
    out
}

fn full_bx(name: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + body.len());
    b.push(version);
    b.extend_from_slice(&flags.to_be_bytes()[1..4]);
    b.extend_from_slice(body);
    bx(name, &b)
}

fn u16be(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
fn u32be(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}
fn u64be(v: u64) -> [u8; 8] {
    v.to_be_bytes()
}

const UNITY_MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

// ---------- init segment ----------

pub fn init_segment(video: &VideoConfig, audio: &AudioConfig) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&ftyp());
    let mut moov_body = Vec::new();
    moov_body.extend_from_slice(&mvhd());
    moov_body.extend_from_slice(&trak_video(video));
    moov_body.extend_from_slice(&trak_audio(audio));
    moov_body.extend_from_slice(&mvex());
    out.extend_from_slice(&bx(b"moov", &moov_body));
    out
}

fn ftyp() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"iso8");
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(b"iso8avc1mp41dash");
    bx(b"ftyp", &b)
}

fn styp() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"msdh");
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(b"msdhmsix");
    bx(b"styp", &b)
}

fn mvhd() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&u32be(0)); // creation + modification
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u32be(1000)); // movie timescale
    b.extend_from_slice(&u32be(0)); // duration
    b.extend_from_slice(&u32be(0x0001_0000)); // rate 1.0
    b.extend_from_slice(&u16be(0x0100)); // volume 1.0
    b.extend_from_slice(&u16be(0));
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u32be(0));
    for m in UNITY_MATRIX {
        b.extend_from_slice(&u32be(m));
    }
    for _ in 0..6 {
        b.extend_from_slice(&u32be(0));
    }
    b.extend_from_slice(&u32be(3)); // next_track_id
    full_bx(b"mvhd", 0, 0, &b)
}

fn tkhd(track_id: u32, width: u32, height: u32, volume: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u32be(track_id));
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u32be(0)); // duration
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u16be(0)); // layer
    b.extend_from_slice(&u16be(0)); // alternate group
    b.extend_from_slice(&u16be(volume));
    b.extend_from_slice(&u16be(0));
    for m in UNITY_MATRIX {
        b.extend_from_slice(&u32be(m));
    }
    b.extend_from_slice(&u32be(width << 16));
    b.extend_from_slice(&u32be(height << 16));
    full_bx(b"tkhd", 0, 0x3, &b) // enabled | in_movie
}

fn mdhd() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u32be(TIMESCALE));
    b.extend_from_slice(&u32be(0)); // duration
    b.extend_from_slice(&u16be(0x55C4)); // language "und"
    b.extend_from_slice(&u16be(0));
    full_bx(b"mdhd", 0, 0, &b)
}

fn hdlr(kind: &[u8; 4], name: &str) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(kind);
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(&u32be(0));
    b.extend_from_slice(name.as_bytes());
    b.push(0);
    full_bx(b"hdlr", 0, 0, &b)
}

fn dinf() -> Vec<u8> {
    let url = full_bx(b"url ", 0, 1, &[]); // self-contained
    let mut dref_body = Vec::new();
    dref_body.extend_from_slice(&u32be(1));
    dref_body.extend_from_slice(&url);
    let dref = full_bx(b"dref", 0, 0, &dref_body);
    bx(b"dinf", &dref)
}

fn empty_stbl_entries() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&full_bx(b"stts", 0, 0, &u32be(0)));
    b.extend_from_slice(&full_bx(b"stsc", 0, 0, &u32be(0)));
    let mut stsz = Vec::new();
    stsz.extend_from_slice(&u32be(0));
    stsz.extend_from_slice(&u32be(0));
    b.extend_from_slice(&full_bx(b"stsz", 0, 0, &stsz));
    b.extend_from_slice(&full_bx(b"stco", 0, 0, &u32be(0)));
    b
}

fn trak_video(cfg: &VideoConfig) -> Vec<u8> {
    // stsd / avc1
    let mut avc1 = Vec::new();
    avc1.extend_from_slice(&[0; 6]);
    avc1.extend_from_slice(&u16be(1)); // data_reference_index
    avc1.extend_from_slice(&[0; 16]); // pre_defined + reserved
    avc1.extend_from_slice(&u16be(cfg.width));
    avc1.extend_from_slice(&u16be(cfg.height));
    avc1.extend_from_slice(&u32be(0x0048_0000)); // horizresolution 72dpi
    avc1.extend_from_slice(&u32be(0x0048_0000));
    avc1.extend_from_slice(&u32be(0));
    avc1.extend_from_slice(&u16be(1)); // frame_count
    avc1.extend_from_slice(&[0; 32]); // compressorname
    avc1.extend_from_slice(&u16be(0x0018)); // depth
    avc1.extend_from_slice(&u16be(0xFFFF)); // pre_defined = -1
    avc1.extend_from_slice(&bx(b"avcC", &crate::h264::avcc_record(&cfg.sps, &cfg.pps)));

    let mut stsd_body = Vec::new();
    stsd_body.extend_from_slice(&u32be(1));
    stsd_body.extend_from_slice(&bx(b"avc1", &avc1));
    let mut stbl_body = Vec::new();
    stbl_body.extend_from_slice(&full_bx(b"stsd", 0, 0, &stsd_body));
    stbl_body.extend_from_slice(&empty_stbl_entries());
    let stbl = bx(b"stbl", &stbl_body);

    let vmhd = full_bx(b"vmhd", 0, 1, &[0; 8]);
    let mut minf_body = Vec::new();
    minf_body.extend_from_slice(&vmhd);
    minf_body.extend_from_slice(&dinf());
    minf_body.extend_from_slice(&stbl);
    let minf = bx(b"minf", &minf_body);

    let mut mdia_body = Vec::new();
    mdia_body.extend_from_slice(&mdhd());
    mdia_body.extend_from_slice(&hdlr(b"vide", "VideoHandler"));
    mdia_body.extend_from_slice(&minf);
    let mdia = bx(b"mdia", &mdia_body);

    let mut trak_body = Vec::new();
    trak_body.extend_from_slice(&tkhd(
        VIDEO_TRACK_ID,
        cfg.width as u32,
        cfg.height as u32,
        0,
    ));
    trak_body.extend_from_slice(&mdia);
    bx(b"trak", &trak_body)
}

fn trak_audio(cfg: &AudioConfig) -> Vec<u8> {
    // esds
    let mut dsi = Vec::new();
    dsi.push(0x05);
    write_descr_len(&mut dsi, cfg.asc.len());
    dsi.extend_from_slice(&cfg.asc);

    let mut dcd = Vec::new();
    dcd.push(0x04);
    write_descr_len(&mut dcd, 13 + dsi.len());
    dcd.push(0x40); // objectTypeIndication: MPEG-4 AAC
    dcd.push(0x15); // streamType audio(5)<<2 | upstream 0 | reserved 1
    dcd.extend_from_slice(&[0x06, 0x00, 0x00]); // bufferSizeDB = 1536
    dcd.extend_from_slice(&u32be(128_000)); // maxBitrate
    dcd.extend_from_slice(&u32be(128_000)); // avgBitrate
    dcd.extend_from_slice(&dsi);

    let mut slc = Vec::new();
    slc.push(0x06);
    slc.push(1);
    slc.push(0x02); // predefined

    let mut es = Vec::new();
    es.push(0x03);
    write_descr_len(&mut es, 3 + dcd.len() + slc.len());
    es.extend_from_slice(&u16be(AUDIO_TRACK_ID as u16)); // ES_ID
    es.push(0); // flags
    es.extend_from_slice(&dcd);
    es.extend_from_slice(&slc);
    let esds = full_bx(b"esds", 0, 0, &es);

    let mut mp4a = Vec::new();
    mp4a.extend_from_slice(&[0; 6]);
    mp4a.extend_from_slice(&u16be(1)); // data_reference_index
    mp4a.extend_from_slice(&u16be(0)); // version
    mp4a.extend_from_slice(&u16be(0)); // revision
    mp4a.extend_from_slice(&u32be(0)); // vendor
    mp4a.extend_from_slice(&u16be(cfg.channels));
    mp4a.extend_from_slice(&u16be(16)); // samplesize
    mp4a.extend_from_slice(&u16be(0));
    mp4a.extend_from_slice(&u16be(0));
    mp4a.extend_from_slice(&u32be(cfg.sample_rate << 16));
    mp4a.extend_from_slice(&esds);

    let mut stsd_body = Vec::new();
    stsd_body.extend_from_slice(&u32be(1));
    stsd_body.extend_from_slice(&bx(b"mp4a", &mp4a));
    let mut stbl_body = Vec::new();
    stbl_body.extend_from_slice(&full_bx(b"stsd", 0, 0, &stsd_body));
    stbl_body.extend_from_slice(&empty_stbl_entries());
    let stbl = bx(b"stbl", &stbl_body);

    let smhd = full_bx(b"smhd", 0, 0, &[0; 4]);
    let mut minf_body = Vec::new();
    minf_body.extend_from_slice(&smhd);
    minf_body.extend_from_slice(&dinf());
    minf_body.extend_from_slice(&stbl);
    let minf = bx(b"minf", &minf_body);

    let mut mdia_body = Vec::new();
    mdia_body.extend_from_slice(&mdhd());
    mdia_body.extend_from_slice(&hdlr(b"soun", "SoundHandler"));
    mdia_body.extend_from_slice(&minf);
    let mdia = bx(b"mdia", &mdia_body);

    let mut trak_body = Vec::new();
    trak_body.extend_from_slice(&tkhd(AUDIO_TRACK_ID, 0, 0, 0x0100));
    trak_body.extend_from_slice(&mdia);
    bx(b"trak", &trak_body)
}

fn write_descr_len(out: &mut Vec<u8>, len: usize) {
    // MPEG-4 可扩展长度编码，每字节 7 bit，高位续传
    let mut stack = [0u8; 4];
    let mut n = 0;
    let mut v = len;
    loop {
        stack[n] = (v & 0x7F) as u8;
        v >>= 7;
        n += 1;
        if v == 0 || n == 4 {
            break;
        }
    }
    for i in (0..n).rev() {
        let cont = if i == 0 { 0 } else { 0x80 };
        out.push(stack[i] | cont);
    }
}

fn mvex() -> Vec<u8> {
    let mut body = Vec::new();
    for id in [VIDEO_TRACK_ID, AUDIO_TRACK_ID] {
        let mut t = Vec::new();
        t.extend_from_slice(&u32be(id));
        t.extend_from_slice(&u32be(1)); // default_sample_description_index
        t.extend_from_slice(&u32be(0)); // default_sample_duration
        t.extend_from_slice(&u32be(0)); // default_sample_size
        t.extend_from_slice(&u32be(0)); // default_sample_flags
        body.extend_from_slice(&full_bx(b"trex", 0, 0, &t));
    }
    bx(b"mvex", &body)
}

// ---------- media segment ----------

/// 生成一个 media segment / part：styp + moof + mdat。
/// 视频样本在前、音频样本在后，共用一个 mdat。
pub fn media_segment(seq: u32, video: &[Sample], audio: &[Sample]) -> Vec<u8> {
    let video_len: usize = video.iter().map(|s| s.data.len()).sum();
    // 先按 data_offset=0 构建一遍 moof 拿到长度，再用真实 offset 重建
    let moof0 = moof(seq, video, audio, 0, 0);
    let moof_len = moof0.len();
    let v_off = (moof_len + 8) as i32;
    let a_off = v_off + video_len as i32;
    let moof = moof(seq, video, audio, v_off, a_off);
    debug_assert_eq!(moof.len(), moof_len);

    let mdat_len = 8 + video_len + audio.iter().map(|s| s.data.len()).sum::<usize>();
    let mut out = Vec::with_capacity(styp().len() + moof.len() + mdat_len);
    out.extend_from_slice(&styp());
    out.extend_from_slice(&moof);
    out.extend_from_slice(&u32be(mdat_len as u32));
    out.extend_from_slice(b"mdat");
    for s in video {
        out.extend_from_slice(&s.data);
    }
    for s in audio {
        out.extend_from_slice(&s.data);
    }
    out
}

fn moof(seq: u32, video: &[Sample], audio: &[Sample], v_off: i32, a_off: i32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&full_bx(b"mfhd", 0, 0, &u32be(seq)));
    if !video.is_empty() {
        body.extend_from_slice(&traf(VIDEO_TRACK_ID, video, v_off, true));
    }
    if !audio.is_empty() {
        body.extend_from_slice(&traf(AUDIO_TRACK_ID, audio, a_off, false));
    }
    bx(b"moof", &body)
}

fn traf(track_id: u32, samples: &[Sample], data_offset: i32, is_video: bool) -> Vec<u8> {
    let tfhd = full_bx(b"tfhd", 0, 0x020000, &u32be(track_id)); // default-base-is-moof

    let mut tfdt_body = Vec::new();
    tfdt_body.extend_from_slice(&u64be(samples[0].dts as u64));
    let tfdt = full_bx(b"tfdt", 1, 0, &tfdt_body);

    // trun: data-offset | sample-duration | sample-size | sample-flags | sample-cts
    let mut trun_body = Vec::new();
    trun_body.extend_from_slice(&u32be(samples.len() as u32));
    trun_body.extend_from_slice(&data_offset.to_be_bytes());
    for s in samples {
        trun_body.extend_from_slice(&u32be(s.dur));
        trun_body.extend_from_slice(&u32be(s.data.len() as u32));
        let flags = if is_video && !s.key {
            0x0101_0000 // depends_on=1, non-sync
        } else {
            0x0200_0000 // sync / audio
        };
        trun_body.extend_from_slice(&u32be(flags));
        trun_body.extend_from_slice(&s.cts.to_be_bytes());
    }
    // version 1：cts 为有符号
    let trun = full_bx(b"trun", 1, 0x000F01, &trun_body);

    let mut body = Vec::new();
    body.extend_from_slice(&tfhd);
    body.extend_from_slice(&tfdt);
    body.extend_from_slice(&trun);
    bx(b"traf", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vcfg() -> VideoConfig {
        VideoConfig {
            width: 1920,
            height: 1080,
            // 真实 SPS/PPS 结构不影响 box 布局测试
            sps: vec![0x67, 0x64, 0x00, 0x28, 0xAC, 0xD9, 0x40, 0x78],
            pps: vec![0x68, 0xEB, 0xE3, 0xCB],
        }
    }
    fn acfg() -> AudioConfig {
        AudioConfig {
            asc: vec![0x11, 0x90],
            sample_rate: 48000,
            channels: 2,
        }
    }

    fn sample(dts: i64, dur: u32, key: bool, size: usize) -> Sample {
        Sample {
            dts,
            cts: if key { 0 } else { 3000 },
            dur,
            key,
            data: Bytes::from(vec![0xAB; size]),
        }
    }

    #[test]
    fn init_has_all_boxes() {
        let init = init_segment(&vcfg(), &acfg());
        assert!(init.windows(4).any(|w| w == b"ftyp"));
        assert!(init.windows(4).any(|w| w == b"moov"));
        assert!(init.windows(4).any(|w| w == b"avcC"));
        assert!(init.windows(4).any(|w| w == b"esds"));
        assert!(init.windows(4).any(|w| w == b"mvex"));
        // 第一个 box 是 ftyp 且尺寸自洽
        let sz = u32::from_be_bytes(init[0..4].try_into().unwrap()) as usize;
        assert_eq!(&init[4..8], b"ftyp");
        assert!(sz < init.len());
    }

    #[test]
    fn segment_offsets_point_into_mdat() {
        let v = vec![sample(0, 3000, true, 100), sample(3000, 3000, false, 50)];
        let a = vec![sample(0, 1920, true, 20), sample(1920, 1920, true, 20)];
        let seg = media_segment(1, &v, &a);

        // 找 moof / mdat
        let moof_pos = seg.windows(4).position(|w| w == b"moof").unwrap() - 4;
        let moof_len = u32::from_be_bytes(seg[moof_pos..moof_pos + 4].try_into().unwrap()) as usize;
        let mdat_pos = seg.windows(4).position(|w| w == b"mdat").unwrap() - 4;
        assert_eq!(mdat_pos, moof_pos + moof_len);
        let mdat_len = u32::from_be_bytes(seg[mdat_pos..mdat_pos + 4].try_into().unwrap()) as usize;
        assert_eq!(mdat_len as usize, 8 + 100 + 50 + 20 + 20);
        assert_eq!(seg.len(), mdat_pos + mdat_len as usize);

        // 解析两个 trun 的 data_offset，验证指向 mdat 载荷内正确位置
        let moof = &seg[moof_pos..moof_pos + moof_len];
        let trun_positions: Vec<usize> = moof
            .windows(4)
            .enumerate()
            .filter(|(_, w)| *w == b"trun")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(trun_positions.len(), 2);
        let mut offsets = Vec::new();
        for p in trun_positions {
            // trun box: size(4) type(4) ver/flags(4) sample_count(4) data_offset(4)
            let box_start = p - 4;
            let off = i32::from_be_bytes(
                moof[box_start + 16..box_start + 20].try_into().unwrap(),
            );
            offsets.push(off as usize);
        }
        // 视频 traf 在前：offset = moof_len + 8
        assert_eq!(offsets[0], moof_len + 8);
        // 音频 traf：offset = moof_len + 8 + 视频数据长度
        assert_eq!(offsets[1], moof_len + 8 + 150);
        // data_offset 是相对 moof 起点的，换算到 segment 内应指向 mdat 载荷
        assert_eq!(moof_pos + offsets[0], mdat_pos + 8);
        assert_eq!(moof_pos + offsets[1], mdat_pos + 8 + 150);
    }

    #[test]
    fn empty_audio_ok() {
        let v = vec![sample(0, 3000, true, 10)];
        let seg = media_segment(2, &v, &[]);
        assert!(seg.windows(4).any(|w| w == b"moof"));
        // 只有一个 traf
        assert_eq!(seg.windows(4).filter(|w| *w == b"traf").count(), 1);
    }
}
