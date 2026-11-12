#!/usr/bin/env bash
# 生成测试素材：两档码率（1080p / 480p），带毫秒时间码、B 帧，
# 480p 的关键帧相对 1080p 错开 0.7s，模拟推流端两路不对齐。
set -euo pipefail
cd "$(dirname "$0")/../assets"

DUR="${DUR:-60}"

FONT=""
if [ -f /System/Library/Fonts/Supplemental/Arial.ttf ]; then
  FONT="fontfile=/System/Library/Fonts/Supplemental/Arial.ttf"
fi

tc() {
  local size=$1
  echo "drawtext=${FONT}:text='%{pts\\:hms}':fontsize=${size}:fontcolor=white:box=1:boxcolor=black@0.65:x=40:y=40"
}

encode() {
  local name=$1 size=$2 bitrate=$3 fontsize=$4
  shift 4
  local vf
  vf="$(tc "$fontsize")"
  ffmpeg -y -f lavfi -i "testsrc2=size=${size}:rate=30" \
         -f lavfi -i "sine=frequency=440:sample_rate=48000" \
         -t "$DUR" -vf "$vf" \
         -c:v libx264 -preset veryfast -pix_fmt yuv420p \
         -x264-params "bframes=3:aud=1:repeat-headers=1" \
         -b:v "$bitrate" \
         "$@" \
         -c:a aac -b:a 128k -ac 2 -ar 48000 \
         "${name}.mp4"

  # 抽 Annex-B 视频 ES + ADTS 音频 + 每包时间戳
  ffmpeg -y -i "${name}.mp4" -c:v copy -an -bsf:v h264_mp4toannexb -f h264 "${name}.h264"
  ffmpeg -y -i "${name}.mp4" -c:a copy -vn -f adts "${name}.aac"
  ffprobe -v error -select_streams v:0 \
    -show_entries packet=pts_time,dts_time -of csv=p=0 "${name}.mp4" > "${name}.tsv"

  echo "--- ${name}: $(wc -l < "${name}.tsv" | tr -d ' ') video packets"
}

# 1080p：GOP 2s，关键帧在 0s/2s/4s...
encode v1080 1920x1080 4000k 64 -g 60 -keyint_min 60 -sc_threshold 0
# 480p：不用固定 GOP，强制关键帧在 0.7s/2.7s/4.7s...（与 1080p 错开）
encode v480 854x480 800k 36 -g 9999 -keyint_min 9999 -sc_threshold 0 -force_key_frames "expr:gte(t,0.7+n_forced*2)"

# 校验关键帧位置
echo "--- 1080p 关键帧（秒）:"
ffprobe -v error -select_streams v:0 -show_entries packet=pts_time,flags -of csv=p=0 v1080.mp4 \
  | awk -F, '$2 ~ /K/ {printf "%.2f ", $1} END {print ""}' | head -c 400; echo
echo "--- 480p 关键帧（秒）:"
ffprobe -v error -select_streams v:0 -show_entries packet=pts_time,flags -of csv=p=0 v480.mp4 \
  | awk -F, '$2 ~ /K/ {printf "%.2f ", $1} END {print ""}' | head -c 400; echo

echo "done. assets:"
ls -lh *.h264 *.aac *.tsv
