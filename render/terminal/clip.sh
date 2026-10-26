#!/bin/sh
set -eu
repo=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$1"
cd "$1"
pcod=${PCOD:-}
if [ -z "$pcod" ]; then
  if command -v pcod > /dev/null 2>&1; then
    pcod=pcod
  else
    pcod="$repo/target/release/pcod"
  fi
fi
if [ ! -f clip.mp4 ]; then
  source=speech/clips/solstice.mp4
  if [ ! -f "$source" ] && [ -n "${2:-}" ] && [ -f "$2/clips/solstice.mp4" ]; then
    source="$2/clips/solstice.mp4"
  fi
  if [ ! -f "$source" ]; then
    echo "clip.sh: $1/speech/clips/solstice.mp4 is missing; pass the speech set with --fixtures" >&2
    exit 1
  fi
  "$pcod" "$source" clip.mp4 --from 20 --to 30 --size 1080p --fps 30 --crf 20 \
    --audio-bitrate 160 --no-progress --force > /dev/null
fi
rm -f out.mp4 strip.png
