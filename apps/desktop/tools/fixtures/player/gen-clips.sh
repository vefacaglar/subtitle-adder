#!/usr/bin/env bash
# Generates the sa-player test clips with the FFmpeg CLI (same commands as crates/sa-player/tests/common/mod.rs,
# which generates them on demand). Usage: gen-clips.sh <out-dir> [ffmpeg]
set -euo pipefail
out="${1:?usage: gen-clips.sh <out-dir> [ffmpeg]}"
ff="${2:-${FFMPEG_DIR:+$FFMPEG_DIR/bin/}ffmpeg}"
mkdir -p "$out"
q=(-y -hide_banner -loglevel error)
src() { echo "-f lavfi -i testsrc2=size=$1:rate=$2 -f lavfi -i sine=frequency=440:sample_rate=48000"; }
# Small H.264/AAC (3 s, 320x240, 30 fps)
"$ff" "${q[@]}" $(src 320x240 30) -t 3 -c:v libx264 -preset ultrafast -g 30 -pix_fmt yuv420p -c:a aac -ac 2 "$out/small-h264.mp4"
# Video only (no audio)
"$ff" "${q[@]}" -f lavfi -i testsrc2=size=320x240:rate=30 -t 3 -c:v libx264 -preset ultrafast -g 30 -pix_fmt yuv420p "$out/small-video-only.mp4"
# Phone-style rotation (display matrix: 90 degrees)
"$ff" "${q[@]}" -display_rotation:v:0 90 -i "$out/small-h264.mp4" -c copy "$out/small-rotated.mp4"
# Audio only
"$ff" "${q[@]}" -f lavfi -i sine=frequency=440:sample_rate=48000 -t 3 -c:a pcm_s16le "$out/tone.wav"
# 1080p clips for the dropped-frame measurement (10 s)
"$ff" "${q[@]}" $(src 1920x1080 60) -t 10 -c:v libx264 -preset veryfast -g 120 -pix_fmt yuv420p -c:a aac "$out/1080p60-h264.mp4"
"$ff" "${q[@]}" $(src 1920x1080 30) -t 10 -c:v libx265 -preset ultrafast -x265-params log-level=error -g 60 -pix_fmt yuv420p -c:a aac "$out/1080p30-hevc.mkv"
"$ff" "${q[@]}" $(src 1920x1080 30) -t 10 -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -row-mt 1 -b:v 4M -g 60 -pix_fmt yuv420p -c:a libopus "$out/1080p30-vp9.mkv"
"$ff" "${q[@]}" $(src 1920x1080 60) -t 10 -c:v libx265 -preset ultrafast -x265-params log-level=error -g 120 -pix_fmt yuv420p10le -c:a aac "$out/1080p60-hevc10.mp4"
# Resolution change mid-stream: two H.264 MPEG-TS segments (320x240, then 640x360) joined byte-wise
"$ff" "${q[@]}" -f lavfi -i testsrc2=size=320x240:rate=30 -t 2 -c:v libx264 -preset ultrafast -g 30 -pix_fmt yuv420p -output_ts_offset 0 "$out/seg-a.ts"
"$ff" "${q[@]}" -f lavfi -i testsrc2=size=640x360:rate=30 -t 2 -c:v libx264 -preset ultrafast -g 30 -pix_fmt yuv420p -output_ts_offset 2 "$out/seg-b.ts"
cat "$out/seg-a.ts" "$out/seg-b.ts" > "$out/resolution-change.ts" && rm -f "$out/seg-a.ts" "$out/seg-b.ts"
echo "clips written to $out"
