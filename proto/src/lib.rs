//! 推流端与服务端之间的 TCP 帧协议。
//!
//! 帧布局（大端）：
//! ```text
//! [magic: u16][ver: u8][stream_id: u8][codec: u8][flags: u8]
//! [pts: i64][dts: i64][len: u32][payload: len 字节]
//! ```
//! 时间戳统一 90kHz。视频 payload 是一个完整 access unit（Annex-B），
//! 音频 payload 是一帧 raw AAC；flags 带 CONFIG 时 payload 为编码配置
//! （AAC 的 AudioSpecificConfig），连接建立后应首先发送。

pub const MAGIC: u16 = 0x4C48; // "LH"
pub const VERSION: u8 = 1;

pub const FLAG_KEYFRAME: u8 = 1 << 0;
pub const FLAG_CONFIG: u8 = 1 << 1;

pub const CODEC_H264: u8 = 1;
pub const CODEC_AAC: u8 = 2;

pub const STREAM_V1080: u8 = 0;
pub const STREAM_V480: u8 = 1;
pub const STREAM_A1080: u8 = 2;
pub const STREAM_A480: u8 = 3;

pub const HEADER_LEN: usize = 2 + 1 + 1 + 1 + 1 + 8 + 8 + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub stream_id: u8,
    pub codec: u8,
    pub flags: u8,
    pub pts: i64,
    pub dts: i64,
    pub len: u32,
}

impl FrameHeader {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..2].copy_from_slice(&MAGIC.to_be_bytes());
        b[2] = VERSION;
        b[3] = self.stream_id;
        b[4] = self.codec;
        b[5] = self.flags;
        b[6..14].copy_from_slice(&self.pts.to_be_bytes());
        b[14..22].copy_from_slice(&self.dts.to_be_bytes());
        b[22..26].copy_from_slice(&self.len.to_be_bytes());
        b
    }

    pub fn decode(buf: &[u8]) -> Option<FrameHeader> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let magic = u16::from_be_bytes([buf[0], buf[1]]);
        if magic != MAGIC || buf[2] != VERSION {
            return None;
        }
        Some(FrameHeader {
            stream_id: buf[3],
            codec: buf[4],
            flags: buf[5],
            pts: i64::from_be_bytes(buf[6..14].try_into().ok()?),
            dts: i64::from_be_bytes(buf[14..22].try_into().ok()?),
            len: u32::from_be_bytes(buf[22..26].try_into().ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = FrameHeader {
            stream_id: STREAM_V1080,
            codec: CODEC_H264,
            flags: FLAG_KEYFRAME,
            pts: 123_456,
            dts: 120_000,
            len: 999,
        };
        let enc = h.encode();
        assert_eq!(FrameHeader::decode(&enc), Some(h));
        assert_eq!(FrameHeader::decode(&enc[..10]), None);
        let mut bad = enc;
        bad[0] = 0;
        assert_eq!(FrameHeader::decode(&bad), None);
    }
}
