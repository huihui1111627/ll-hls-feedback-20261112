#!/usr/bin/env bash
# 一键起 LL-HLS 服务和两路测试推流。
# 可选环境变量：DROP=12:4 ./scripts/run_demo.sh
set -euo pipefail
cd "$(dirname "$0")/.."

if [ ! -f assets/v1080.h264 ] || [ ! -f assets/v480.h264 ]; then
  echo "==> 生成测试素材"
  ./scripts/gen_assets.sh
fi

if ! command -v cargo >/dev/null; then
  if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
fi
command -v cargo >/dev/null || { echo "需要先安装 Rust: https://rustup.rs"; exit 1; }
cargo build --release

HTTP_ADDR="${HTTP_ADDR:-0.0.0.0:8080}"
INGEST_ADDR="${INGEST_ADDR:-0.0.0.0:9000}"

./target/release/llhls-server --http "$HTTP_ADDR" --ingest "$INGEST_ADDR" &
SERVER=$!
sleep 1

INGEST_PORT="${INGEST_ADDR##*:}"
DROP_ARGS=()
if [ -n "${DROP:-}" ]; then DROP_ARGS=(--drop "$DROP"); fi

./target/release/pusher --addr "127.0.0.1:${INGEST_PORT}" \
  --video assets/v1080.h264 --video-ts assets/v1080.tsv --audio assets/v1080.aac \
  --video-id 0 --audio-id 2 --loop "${DROP_ARGS[@]}" &
P1=$!
./target/release/pusher --addr "127.0.0.1:${INGEST_PORT}" \
  --video assets/v480.h264 --video-ts assets/v480.tsv --audio assets/v480.aac \
  --video-id 1 --audio-id 3 --loop "${DROP_ARGS[@]}" &
P2=$!

trap 'kill $SERVER $P1 $P2 2>/dev/null' EXIT
echo
echo "浏览器打开 http://${HTTP_ADDR#*:}/  （画面左上角时间码 vs 页面时钟 = 端到端延迟）"
echo "如果端口被占用，用 HTTP_ADDR=0.0.0.0:8090 INGEST_ADDR=0.0.0.0:9001 换个端口"
wait
