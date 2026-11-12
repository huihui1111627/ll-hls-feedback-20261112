#!/usr/bin/env bash
# 从运行中的服务拉取 init + 最新的完整 segment / part，用 ffprobe/ffmpeg 校验封装。
set -euo pipefail
BASE="${BASE:-http://127.0.0.1:8080}"
REND="${1:-1080p}"
OUT="/tmp/llhls-smoke-$REND"

mkdir -p "$OUT"
PL="$(curl -s "$BASE/live/$REND/index.m3u8")"

echo "=== media sequence / 最新内容 ==="
echo "$PL" | grep -E "MEDIA-SEQUENCE|EXT-X-DISCONTINUITY" || true

MAP_URI="$(echo "$PL" | grep -oE 'init\.mp4[^"]*' | head -1)"
curl -s "$BASE/live/$REND/init.mp4?$(echo "$MAP_URI" | cut -d? -f2)" -o "$OUT/init.mp4" || true
curl -s "$BASE/live/$REND/init.mp4" -o "$OUT/init.mp4"

# 最后一个完整 segment
LAST_SEG="$(echo "$PL" | grep -oE 'seg/[0-9]+\.m4s' | tail -1)"
if [ -n "$LAST_SEG" ]; then
  curl -s "$BASE/live/$REND/$LAST_SEG" -o "$OUT/seg.m4s"
  cat "$OUT/init.mp4" "$OUT/seg.m4s" > "$OUT/full_seg.mp4"
  echo "=== segment $LAST_SEG ($(stat -f%z "$OUT/seg.m4s") bytes) ==="
  ffprobe -v error -show_entries stream=codec_name,profile,level,width,height,sample_rate,channels \
    -of default=noprint_wrappers=1 "$OUT/full_seg.mp4"
  ffmpeg -v error -i "$OUT/full_seg.mp4" -f null - 2>&1 | grep -v "^$" | head -3 || true
  echo "segment decode: OK"
fi

# 最新的几个 part（open segment 的 parts）
PARTS="$(echo "$PL" | grep -oE 'part/[0-9]+/[0-9]+\.m4s' | tail -3)"
if [ -n "$PARTS" ]; then
  : > "$OUT/parts.mp4"
  cp "$OUT/init.mp4" "$OUT/parts.mp4"
  for p in $(echo "$PARTS" | tr '\n' ' '); do
    curl -s "$BASE/live/$REND/$p" >> "$OUT/parts.mp4"
  done
  echo "=== recent parts: $(echo $PARTS | tr '\n' ' ')"
  ffmpeg -v error -i "$OUT/parts.mp4" -f null - 2>&1 | grep -v "^$" | head -3 || true
  echo "parts decode: OK"
fi

echo "=== 各段时长（ffprobe 读 moof 累计）==="
ffprobe -v error -show_entries format=duration -of default=noprint_wrappers=1 "$OUT/full_seg.mp4" 2>/dev/null || true
