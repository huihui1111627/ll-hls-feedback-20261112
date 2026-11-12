//! H.264 码流解析：Annex-B 分 NAL、SPS 解析（Exp-Golomb）、AVCC 转换。
//! 只实现封装需要的最小子集，不做解码。

use anyhow::{bail, Result};

pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map(|b| b & 0x1F).unwrap_or(0)
}

/// 把 Annex-B 字节流切成 NAL 单元（返回的切片不含 start code）。
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0;
    let mut cur: Option<usize> = None;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && (data[i + 2] == 1) {
            if let Some(s) = cur.take() {
                nals.push(trim_zeros(&data[s..i]));
            }
            cur = Some(i + 3);
            i += 3;
        } else if i + 4 <= data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            if let Some(s) = cur.take() {
                nals.push(trim_zeros(&data[s..i]));
            }
            cur = Some(i + 4);
            i += 4;
        } else {
            i += 1;
        }
    }
    if let Some(s) = cur {
        nals.push(trim_zeros(&data[s..]));
    }
    nals.into_iter().filter(|n| !n.is_empty()).collect()
}

/// 去掉 NAL 末尾的 0（start code 对齐零），但保留至少 1 字节。
fn trim_zeros(mut b: &[u8]) -> &[u8] {
    while b.len() > 1 && b[b.len() - 1] == 0 {
        b = &b[..b.len() - 1];
    }
    b
}

/// Annex-B AU -> AVCC（4 字节大端长度前缀）样本数据。
/// 顺带返回遇到的最新 SPS/PPS（用于刷新 avcC）。
pub fn au_to_avcc(data: &[u8]) -> (Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>, bool) {
    let nals = split_annexb(data);
    let mut out = Vec::with_capacity(data.len() + 4 * nals.len());
    let mut sps = None;
    let mut pps = None;
    let mut key = false;
    for nal in nals {
        match nal_type(nal) {
            NAL_AUD => continue, // AUD 不进 mp4
            NAL_SPS => sps = Some(nal.to_vec()),
            NAL_PPS => pps = Some(nal.to_vec()),
            NAL_IDR => key = true,
            _ => {}
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    (out, sps, pps, key)
}

/// 去掉 emulation prevention 字节（00 00 03 -> 00 00）。
fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize, // bit 位置
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }
    fn bit(&mut self) -> Result<u32> {
        if self.pos / 8 >= self.data.len() {
            bail!("bitreader overflow");
        }
        let b = (self.data[self.pos / 8] >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Ok(b as u32)
    }
    fn bits(&mut self, n: usize) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }
    fn ue(&mut self) -> Result<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                bail!("ue overflow");
            }
        }
        Ok((((1u64 << zeros) - 1) + self.bits(zeros as usize)? as u64) as u32)
    }
    fn se(&mut self) -> Result<i32> {
        let k = self.ue()? as i64;
        Ok(if k % 2 == 0 { -(k / 2) } else { (k + 1) / 2 } as i32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sps {
    pub profile_idc: u8,
    pub level_idc: u8,
    pub width: u32,
    pub height: u32,
}

impl Sps {
    /// 解析 SPS NAL（含 nal header 字节）。
    pub fn parse(nal: &[u8]) -> Result<Sps> {
        if nal_type(nal) != NAL_SPS {
            bail!("not an SPS");
        }
        let data = rbsp(&nal[1..]);
        let mut r = BitReader::new(&data);
        let profile_idc = r.bits(8)? as u8;
        let _constraint = r.bits(8)?;
        let level_idc = r.bits(8)? as u8;
        let _sps_id = r.ue()?;
        let mut chroma_format_idc = 1u32;
        if matches!(
            profile_idc,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            chroma_format_idc = r.ue()?;
            if chroma_format_idc == 3 {
                r.bit()?; // separate_colour_plane_flag
            }
            r.ue()?; // bit_depth_luma_minus8
            r.ue()?; // bit_depth_chroma_minus8
            r.bit()?; // qpprime_y_zero_transform_bypass_flag
            if r.bit()? == 1 {
                // seq_scaling_matrix_present_flag
                let n = if chroma_format_idc == 3 { 12 } else { 8 };
                for i in 0..n {
                    if r.bit()? == 1 {
                        skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                    }
                }
            }
        }
        r.ue()?; // log2_max_frame_num_minus4
        let poc_type = r.ue()?;
        if poc_type == 0 {
            r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
        } else if poc_type == 1 {
            r.bit()?; // delta_pic_order_always_zero_flag
            r.se()?; // offset_for_non_ref_pic
            r.se()?; // offset_for_top_to_bottom_field
            let n = r.ue()?;
            for _ in 0..n {
                r.se()?;
            }
        }
        r.ue()?; // max_num_ref_frames
        r.bit()?; // gaps_in_frame_num_value_allowed_flag
        let pic_width_in_mbs_minus1 = r.ue()?;
        let pic_height_in_map_units_minus1 = r.ue()?;
        let frame_mbs_only_flag = r.bit()?;
        if frame_mbs_only_flag == 0 {
            r.bit()?; // mb_adaptive_frame_field_flag
        }
        r.bit()?; // direct_8x8_inference_flag
        let (mut crop_x, mut crop_y) = (0, 0);
        if r.bit()? == 1 {
            // frame_cropping_flag
            let left = r.ue()?;
            let right = r.ue()?;
            let top = r.ue()?;
            let bottom = r.ue()?;
            crop_x = left + right;
            crop_y = top + bottom;
        }
        // 裁剪单位与 chroma 格式/帧场编码有关，这里按常见的 4:2:0 处理
        let (sub_x, sub_y) = if chroma_format_idc == 1 {
            (2, 2)
        } else if chroma_format_idc == 2 {
            (2, 1)
        } else {
            (1, 1)
        };
        let height_scale = if frame_mbs_only_flag == 1 { 1 } else { 2 };
        let width = (pic_width_in_mbs_minus1 + 1) * 16 - crop_x * sub_x;
        let height =
            (pic_height_in_map_units_minus1 + 1) * 16 * height_scale - crop_y * sub_y * height_scale;
        Ok(Sps {
            profile_idc,
            level_idc,
            width,
            height,
        })
    }
}

fn skip_scaling_list(r: &mut BitReader, size: usize) -> Result<()> {
    let mut last = 8i64;
    let mut next = 8i64;
    for _ in 0..size {
        if next != 0 {
            let delta = r.se()? as i64;
            next = (last + delta + 256) % 256;
        }
        last = if next == 0 { last } else { next };
    }
    Ok(())
}

/// 生成 avcC（AVCDecoderConfigurationRecord）内容。
pub fn avcc_record(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(1); // configurationVersion
    out.push(sps.get(1).copied().unwrap_or(100)); // profile
    out.push(sps.get(2).copied().unwrap_or(0)); // profile compat
    out.push(sps.get(3).copied().unwrap_or(40)); // level
    out.push(0xFF); // 6bit reserved + lengthSizeMinusOne=3
    out.push(0xE0 | 1); // 3bit reserved + numOfSPS
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1); // numOfPPS
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    out
}

/// avc1.XXXXXX 形式的 codec 字符串（取自 SPS 第 2~4 字节）。
pub fn avc_codec_string(sps: &[u8]) -> String {
    format!(
        "avc1.{:02x}{:02x}{:02x}",
        sps.get(1).copied().unwrap_or(100),
        sps.get(2).copied().unwrap_or(0),
        sps.get(3).copied().unwrap_or(40)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_basic() {
        // 两个 NAL：00 00 01 65 ... | 00 00 00 01 41 ...
        let data = [
            0, 0, 1, 0x65, 1, 2, 3, 0, 0, 0, 1, 0x41, 4, 5, 0, 0, 0, 1, 0x41, 6,
        ];
        let nals = split_annexb(&data);
        assert_eq!(nals.len(), 3);
        assert_eq!(nal_type(nals[0]), NAL_IDR);
        assert_eq!(nals[0], &[0x65, 1, 2, 3]);
        assert_eq!(nals[2], &[0x41, 6]);
    }

    #[test]
    fn avcc_convert() {
        let data = [0, 0, 0, 1, 0x67, 9, 9, 0, 0, 1, 0x65, 1, 2];
        let (out, sps, _pps, key) = au_to_avcc(&data);
        assert!(key);
        assert!(sps.is_some());
        // 4字节长度 + 3字节sps + 4字节长度 + 3字节idr
        assert_eq!(out.len(), 4 + 3 + 4 + 3);
        assert_eq!(&out[0..4], &3u32.to_be_bytes());
    }

    #[test]
    fn parse_known_sps() {
        let sps: Vec<u8> =
            (0..27).map(|i| u8::from_str_radix(&"67640028acd940780227e5c044000003000400000300f03c60c658"[i * 2..i * 2 + 2], 16).unwrap()).collect();
        let p = Sps::parse(&sps).unwrap();
        assert_eq!((p.width, p.height), (1920, 1080));
    }

    #[test]
    fn rbsp_strip() {
        assert_eq!(rbsp(&[1, 0, 0, 3, 5, 0, 0, 0, 3, 1]), vec![1, 0, 0, 5, 0, 0, 0, 1]);
    }

    /// 用 ffmpeg 生成的真实 SPS 验证解析（assets 不存在则跳过）。
    #[test]
    fn parse_real_sps() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../assets/v1080.h264");
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let nals = split_annexb(&data);
        let sps_nal = nals.iter().find(|n| nal_type(n) == NAL_SPS).expect("no sps");
        let sps = Sps::parse(sps_nal).expect("parse sps");
        assert_eq!((sps.width, sps.height), (1920, 1080));
    }
}
