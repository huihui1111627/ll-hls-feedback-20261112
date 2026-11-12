//! 共享状态：两路码率的切片器注册表 + 流 ID 路由。

use crate::segmenter::{Rendition, RenditionConfig};
use proto::{STREAM_A1080, STREAM_A480, STREAM_V1080, STREAM_V480};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug)]
pub struct StreamRoute {
    pub rendition: usize,
    pub kind: StreamKind,
}

pub struct Registry {
    pub renditions: Vec<Arc<Mutex<Rendition>>>,
    names: Vec<&'static str>,
}

impl Registry {
    pub fn new() -> Self {
        let r1080 = Rendition::new(RenditionConfig {
            name: "1080p",
            bandwidth: 4_328_000,
            seg_target: 180_000,  // 2s
            part_target: 30_000,  // 333ms
            window: 10,
        });
        let r480 = Rendition::new(RenditionConfig {
            name: "480p",
            bandwidth: 928_000,
            seg_target: 180_000,
            part_target: 30_000,
            window: 10,
        });
        Registry {
            renditions: vec![
                Arc::new(Mutex::new(r1080)),
                Arc::new(Mutex::new(r480)),
            ],
            names: vec!["1080p", "480p"],
        }
    }

    pub fn by_name(&self, name: &str) -> Option<Arc<Mutex<Rendition>>> {
        self.names
            .iter()
            .position(|&n| n == name)
            .map(|i| self.renditions[i].clone())
    }

    pub fn route(stream_id: u8) -> Option<StreamRoute> {
        match stream_id {
            STREAM_V1080 => Some(StreamRoute {
                rendition: 0,
                kind: StreamKind::Video,
            }),
            STREAM_V480 => Some(StreamRoute {
                rendition: 1,
                kind: StreamKind::Video,
            }),
            STREAM_A1080 => Some(StreamRoute {
                rendition: 0,
                kind: StreamKind::Audio,
            }),
            STREAM_A480 => Some(StreamRoute {
                rendition: 1,
                kind: StreamKind::Audio,
            }),
            _ => None,
        }
    }
}
