mod fmp4;
mod h264;
mod http;
mod ingest;
mod playlist;
mod segmenter;
mod state;

use clap::Parser;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "llhls-server", about = "自研 LL-HLS 直播服务")]
struct Args {
    /// HTTP 监听地址
    #[arg(long, default_value = "0.0.0.0:8080")]
    http: String,
    /// 推流 TCP 监听地址
    #[arg(long, default_value = "0.0.0.0:9000")]
    ingest: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let registry = Arc::new(state::Registry::new());

    let ingest_reg = registry.clone();
    let ingest_addr = args.ingest.clone();
    tokio::spawn(async move {
        if let Err(e) = ingest::run(ingest_addr, ingest_reg).await {
            tracing::error!("ingest server failed: {e:#}");
        }
    });

    let app = http::router(registry);
    let listener = tokio::net::TcpListener::bind(&args.http).await?;
    tracing::info!("http listening on http://{}", args.http);
    axum::serve(listener, app).await?;
    Ok(())
}
