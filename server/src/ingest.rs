//! TCP 推流接入：解析帧头，分发给对应的切片器。

use crate::h264;
use crate::state::{Registry, StreamKind};
use bytes::Bytes;
use proto::{FrameHeader, CODEC_AAC, FLAG_CONFIG, FLAG_KEYFRAME, HEADER_LEN};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

const MAX_FRAME: u32 = 16 * 1024 * 1024;

pub async fn run(addr: String, registry: Arc<Registry>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("ingest listening on {addr}");
    loop {
        let (stream, peer) = listener.accept().await?;
        let reg = registry.clone();
        tokio::spawn(async move {
            tracing::info!(%peer, "ingest connection accepted");
            if let Err(e) = handle(stream, reg).await {
                tracing::info!(%peer, "ingest connection closed: {e}");
            }
        });
    }
}

async fn handle(mut stream: tokio::net::TcpStream, registry: Arc<Registry>) -> anyhow::Result<()> {
    stream.set_nodelay(true).ok();
    let mut buf = [0u8; HEADER_LEN];
    let mut dispatched: Vec<usize> = Vec::new();

    let result = async {
        loop {
            stream.read_exact(&mut buf).await?;
            let Some(header) = FrameHeader::decode(&buf) else {
                anyhow::bail!("bad frame header");
            };
            if header.len > MAX_FRAME {
                anyhow::bail!("frame too large: {}", header.len);
            }
            let mut payload = vec![0u8; header.len as usize];
            stream.read_exact(&mut payload).await?;

            let Some(route) = Registry::route(header.stream_id) else {
                anyhow::bail!("unknown stream id {}", header.stream_id);
            };
            let rendition = registry.renditions[route.rendition].clone();

            if !dispatched.contains(&route.rendition) {
                // 这一路子流第一次出现：建立新会话（断流重连的 discontinuity 在这里触发）
                rendition.lock().await.begin_session();
                dispatched.push(route.rendition);
            }

            let data = Bytes::from(payload);
            let mut r = rendition.lock().await;
            match (route.kind, header.codec, header.flags & FLAG_CONFIG != 0) {
                (_, CODEC_AAC, true) => {
                    if let Some((rate, channels)) = parse_asc(&data) {
                        r.set_audio_config(data.to_vec(), rate, channels);
                    }
                }
                (StreamKind::Audio, CODEC_AAC, false) => {
                    r.push_audio(header.pts, header.dts, data);
                }
                (StreamKind::Video, _, false) => {
                    let (avcc, sps, pps, key) = h264::au_to_avcc(&data);
                    let key = key || header.flags & FLAG_KEYFRAME != 0;
                    r.push_video(
                        header.pts,
                        header.dts,
                        key,
                        Bytes::from(avcc),
                        sps,
                        pps,
                    );
                }
                _ => anyhow::bail!("unexpected codec/stream combo"),
            }
        }
    }
    .await;

    // 连接断开：把当前 part 收尾
    for idx in dispatched {
        registry.renditions[idx].lock().await.end_session();
    }
    result
}

/// 从 AudioSpecificConfig 提取采样率和声道数。
fn parse_asc(asc: &[u8]) -> Option<(u32, u16)> {
    if asc.len() < 2 {
        return None;
    }
    let freq_idx = (asc[0] >> 2) & 0x0F;
    let chan = ((asc[0] & 0x01) << 2) | ((asc[1] >> 6) & 0x03);
    let rate = match freq_idx {
        0 => 96000,
        1 => 88200,
        2 => 64000,
        3 => 48000,
        4 => 44100,
        5 => 32000,
        6 => 24000,
        7 => 22050,
        8 => 16000,
        9 => 12000,
        10 => 11025,
        11 => 8000,
        12 => 7350,
        _ => return None,
    };
    Some((rate, chan.into()))
}
