//! LL-HLS 播放列表渲染：master.m3u8 和 media playlist。

use crate::segmenter::{Rendition, TICKS_PER_SEC};
use std::fmt::Write as _;

pub struct VariantInfo {
    pub name: String,
    pub bandwidth: u32,
    pub codec_string: String,
    pub width: u32,
    pub height: u32,
}

pub fn master(variants: &[VariantInfo]) -> String {
    let mut s = String::new();
    s.push_str("#EXTM3U\n");
    s.push_str("#EXT-X-VERSION:3\n");
    s.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");
    for v in variants {
        let _ = writeln!(
            s,
            "#EXT-X-STREAM-INF:BANDWIDTH={},RESOLUTION={}x{},CODECS=\"{}\"",
            v.bandwidth, v.width, v.height, v.codec_string
        );
        let _ = writeln!(s, "{}/index.m3u8", v.name);
    }
    s
}

/// 渲染单路码率的媒体播放列表。
pub fn media(r: &Rendition) -> String {
    let mut s = String::new();
    s.push_str("#EXTM3U\n");
    s.push_str("#EXT-X-VERSION:9\n");

    let part_target = r.cfg.part_target as f64 / TICKS_PER_SEC as f64;
    let seg_target = r.cfg.seg_target as f64 / TICKS_PER_SEC as f64;
    let _ = writeln!(s, "#EXT-X-TARGETDURATION:{}", target_duration(r));
    let _ = writeln!(s, "#EXT-X-MEDIA-SEQUENCE:{}", r.msn_base());
    let _ = writeln!(
        s,
        "#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,HOLD-BACK={:.1},PART-HOLD-BACK={:.1}",
        (seg_target * 3.0).max(6.0),
        part_target * 3.0
    );
    let _ = writeln!(s, "#EXT-X-PART-INF:PART-TARGET={:.3}", part_target);

    let gen = r.init().map(|(g, _)| g).unwrap_or(0);
    let mut map_written = false;
    let write_map = |s: &mut String, map_written: &mut bool| {
        if !*map_written {
            let _ = writeln!(s, "#EXT-X-MAP:URI=\"init.mp4?g={}\"", gen);
            *map_written = true;
        }
    };

    for seg in r.segments() {
        write_map(&mut s, &mut map_written);
        if seg.discontinuity {
            s.push_str("#EXT-X-DISCONTINUITY\n");
        }
        let _ = writeln!(
            s,
            "#EXT-X-PROGRAM-DATE-TIME:{}",
            fmt_pdt(seg.pdt_ms)
        );
        let _ = writeln!(s, "#EXTINF:{:.3},", secs(seg.duration));
        let _ = writeln!(s, "seg/{}.m4s", seg.msn);
    }

    let cur_msn = r.cur_msn();
    let parts = r.open_parts();
    if !parts.is_empty() || !r.segments().is_empty() {
        write_map(&mut s, &mut map_written);
    }
    if !parts.is_empty() && r.open_discontinuity() {
        s.push_str("#EXT-X-DISCONTINUITY\n");
    }
    for p in parts {
        let indep = if p.independent { ",INDEPENDENT=YES" } else { "" };
        let _ = writeln!(
            s,
            "#EXT-X-PART:DURATION={:.3},URI=\"part/{}/{}.m4s\"{}",
            secs(p.duration),
            cur_msn,
            p.index,
            indep
        );
    }

    // 预加载提示：指向正在产出的下一个 part
    let next_idx = parts.len() as u32;
    let _ = writeln!(
        s,
        "#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part/{}/{}.m4s\"",
        cur_msn, next_idx
    );
    s
}

fn target_duration(r: &Rendition) -> u64 {
    let max_seg = r
        .segments()
        .iter()
        .map(|s| s.duration)
        .max()
        .unwrap_or(r.cfg.seg_target);
    let max_part = r
        .open_parts()
        .iter()
        .map(|p| p.duration)
        .max()
        .unwrap_or(0);
    let max_ticks = max_seg.max(max_part).max(r.cfg.seg_target);
    ((max_ticks as f64 / TICKS_PER_SEC as f64).ceil() as u64).max(1)
}

fn secs(ticks: i64) -> f64 {
    ticks as f64 / TICKS_PER_SEC as f64
}

/// unix 毫秒 -> ISO8601 UTC，例如 2026-11-11T08:00:00.123Z
pub fn fmt_pdt(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        millis
    )
}

/// Howard Hinnant 的 days -> (y,m,d) 算法（公历，含闰年规则）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdt_format() {
        // 1970-01-01T00:00:00.000Z
        assert_eq!(fmt_pdt(0), "1970-01-01T00:00:00.000Z");
        // 2024-02-29T12:34:56.789Z
        let ms = 1709210096789i64;
        assert_eq!(fmt_pdt(ms), "2024-02-29T12:34:56.789Z");
    }
}
