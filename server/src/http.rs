//! HTTP 输出：master/media playlist、init、segment、part，含 LL-HLS blocking reload。

use crate::playlist;
use crate::state::Registry;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{timeout, Instant};

const INDEX_HTML: &str = include_str!("../static/index.html");
const HLS_JS: &[u8] = include_bytes!("../static/hls.min.js");

pub fn router(registry: Arc<Registry>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/hls.min.js", get(hls_js))
        .route("/live/master.m3u8", get(master))
        .route("/live/{rend}/index.m3u8", get(playlist))
        .route("/live/{rend}/init.mp4", get(init_mp4))
        .route("/live/{rend}/seg/{file}", get(segment))
        .route("/live/{rend}/part/{msn}/{file}", get(part))
        .with_state(registry)
}

async fn index() -> Response {
    let mut r = Response::new(Body::from(INDEX_HTML));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    cors(r)
}

async fn hls_js() -> Response {
    let mut r = Response::new(Body::from(HLS_JS));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/javascript"),
    );
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    cors(r)
}

async fn master(State(reg): State<Arc<Registry>>) -> Response {
    let mut variants = Vec::new();
    for r in &reg.renditions {
        let g = r.lock().await;
        let (w, h) = g
            .video_cfg
            .as_ref()
            .map(|v| (v.width as u32, v.height as u32))
            .unwrap_or_else(|| default_resolution(g.cfg.name));
        variants.push(playlist::VariantInfo {
            name: g.cfg.name.to_string(),
            bandwidth: g.cfg.bandwidth,
            codec_string: g.codec_string.clone(),
            width: w,
            height: h,
        });
    }
    playlist_response(playlist::master(&variants))
}

fn default_resolution(name: &str) -> (u32, u32) {
    match name {
        "1080p" => (1920, 1080),
        _ => (854, 480),
    }
}

#[derive(Deserialize)]
struct PlaylistQuery {
    #[serde(rename = "_HLS_msn")]
    msn: Option<u64>,
    #[serde(rename = "_HLS_part")]
    part: Option<u32>,
}

async fn playlist(
    State(reg): State<Arc<Registry>>,
    Path(rend): Path<String>,
    Query(q): Query<PlaylistQuery>,
) -> Response {
    let Some(r) = reg.by_name(&rend) else {
        return not_found("no such rendition");
    };
    let want = q.msn.map(|m| (m, q.part.unwrap_or(0)));

    // 先订阅版本变更再检查条件，避免检查与等待之间的丢唤醒
    let mut rx = r.lock().await.watch_tx.subscribe();
    let deadline = Instant::now() + Duration::from_secs(3);

    loop {
        let (body, ready) = {
            let g = r.lock().await;
            let ready = match want {
                // 断流中（1s 无新数据）直接给当前列表，别让客户端挂到超时
                Some((m, p)) => g.is_satisfied(m, p) || g.is_stalled(1000),
                None => true,
            };
            (playlist::media(&g), ready)
        };
        if ready {
            return playlist_response(body);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return playlist_response(body);
        }
        // 超时也返回当前列表，客户端会带上新的 _HLS_msn 再问
        let _ = timeout(left, rx.changed()).await;
    }
}

async fn init_mp4(State(reg): State<Arc<Registry>>, Path(rend): Path<String>) -> Response {
    let Some(r) = reg.by_name(&rend) else {
        return not_found("no such rendition");
    };
    let g = r.lock().await;
    match g.init() {
        Some((_, data)) => bytes_response(data, "video/mp4", "public, max-age=3600"),
        None => not_found("init not ready"),
    }
}

async fn segment(
    State(reg): State<Arc<Registry>>,
    Path((rend, file)): Path<(String, String)>,
) -> Response {
    let Some(r) = reg.by_name(&rend) else {
        return not_found("no such rendition");
    };
    let Some(msn) = file.strip_suffix(".m4s").and_then(|s| s.parse::<u64>().ok()) else {
        return not_found("bad segment name");
    };
    let g = r.lock().await;
    match g.get_segment(msn) {
        Some(data) => bytes_response(data, "video/mp4", "public, max-age=30"),
        None => not_found("segment not found"),
    }
}

async fn part(
    State(reg): State<Arc<Registry>>,
    Path((rend, msn, file)): Path<(String, String, String)>,
) -> Response {
    let Some(r) = reg.by_name(&rend) else {
        return not_found("no such rendition");
    };
    let Some(msn) = msn.parse::<u64>().ok() else {
        return not_found("bad msn");
    };
    let Some(idx) = file.strip_suffix(".m4s").and_then(|s| s.parse::<u32>().ok()) else {
        return not_found("bad part name");
    };

    // PRELOAD-HINT 预取的 part 可能还没产出，挂起等它
    let mut rx = r.lock().await.watch_tx.subscribe();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let hit = {
            let g = r.lock().await;
            g.get_part(msn, idx)
        };
        if let Some(data) = hit {
            return bytes_response(data, "video/mp4", "public, max-age=5");
        }
        let waiting_room = {
            let g = r.lock().await;
            g.next_part_hint()
        };
        if waiting_room != (msn, idx) {
            return not_found("part not found");
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return not_found("part not ready");
        }
        let _ = timeout(left, rx.changed()).await;
    }
}

fn bytes_response(data: Arc<Bytes>, content_type: &'static str, cache: &'static str) -> Response {
    let mut r = Response::new(Body::from(data.as_ref().clone()));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    cors(r)
}

fn playlist_response(body: String) -> Response {
    let mut r = Response::new(Body::from(body));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.apple.mpegurl"),
    );
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store"),
    );
    cors(r)
}

fn not_found(msg: &str) -> Response {
    let r = (StatusCode::NOT_FOUND, msg.to_string()).into_response();
    cors(r)
}

fn cors(mut r: Response) -> Response {
    r.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    r
}
