#!/bin/sh
set -eu
mkdir -p "$1"
cd "$1"
if [ ! -f clip.mp4 ]; then
  if [ ! -f speech/clips/solstice.mp4 ]; then
    echo "clip.sh: $1/speech/clips/solstice.mp4 is missing" >&2
    exit 1
  fi
  ffmpeg -hide_banner -loglevel error -y \
    -ss 20 -t 10 -i speech/clips/solstice.mp4 \
    -vf "scale=1920:1080:flags=lanczos,fps=30" \
    -c:v libx264 -preset medium -crf 20 -pix_fmt yuv420p \
    -c:a aac -b:a 160k -ar 48000 -ac 2 \
    -movflags +faststart clip.mp4
fi
rm -f out.mp4
