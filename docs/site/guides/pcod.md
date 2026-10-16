---
title: "Probe, decode, encode, take stills and make MP4s from frames with pcod"
description: "Look inside an MP4, decode it on the CPU or the GPU, encode it to H.264 or HEVC, and turn a folder of PNG frames and a WAV into an MP4, and take stills and contact sheets from the command line."
sidebar:
  order: 1
---

pcod is a small command line over the library. `probe` shows what is inside an MP4, `decode` decodes its whole video track and reports the speed, `encode` decodes, encodes and muxes a new MP4, copying the audio, `frames` encodes a folder of PNG frames and a WAV into an MP4, and `stills` writes stills of a video as PNGs and a contact sheet. Every option is in the [command reference](../../reference/cli/).

The outputs below are from a 10-second 1080p60 H.264 clip cut from NASA's public-domain "A Message of Resilience from the International Space Station", on a 16-core CPU and an AMD card through VA-API. Your speeds will differ.

## Probe

```sh
pcod probe clip.mp4
```

```text
container  MP4
duration   10.0 s
video      h264 High 1920x1080 59.94 fps
audio      aac 2 ch 48000 Hz
```

## Decode

`--route` picks the GPU (the default) or the CPU; `--threads` caps the CPU threads.

```sh
pcod decode clip.mp4 --route cpu
pcod decode clip.mp4 --route gpu
```

```text
route   CPU (16 threads)
decoded 600 frames in 0.88 s: 686 fps
route   GPU (VA-API, radeonsi)
decoded 600 frames in 0.48 s: 1260 fps
```

Without a GPU decoder for the file's codec, `--route gpu` decodes on the CPU and says why on a `note` line.

## Encode

`--codec` is `h264` (the default) or `hevc`, `--crf` sets the quality (23 by default; lower is better and larger), and `--route` picks the GPU or the CPU for both the decode and the encode. On the GPU, frames go from the decoder to the encoder without leaving the card.

```sh
pcod encode clip.mp4 out.mp4 --codec hevc --route gpu
```

```text
decode  GPU (VA-API, radeonsi)
encode  HEVC GPU (VA-API, radeonsi)
encoded 600 frames in 1.14 s: 526 fps
wrote   out.mp4 (2.7 MB)
```

HEVC encodes on the GPU only. Asked for it on the CPU, pcod encodes H.264 and says so:

```sh
pcod encode clip.mp4 out.mp4 --codec hevc --route cpu
```

```text
decode  CPU (16 threads)
note    HEVC encodes on the GPU only; encoding H.264 on the CPU instead
encode  H.264 CPU (16 threads)
encoded 600 frames in 6.29 s: 95 fps
wrote   out.mp4 (4.2 MB)
```

## Frames to an MP4

`frames` encodes a folder of PNG frames, such as a game's rendered trailer, and an optional WAV into an MP4: H.264 (or HEVC with `--codec hevc`), 8-bit 4:2:0 BT.709, AAC at 48 kHz stereo, with the index at the front (faststart). The frames are the folder's `.png` files in name order (`000001.png`, `000002.png`, ...), all one even size. `--fps` takes an integer, a ratio such as `30000/1001` or a decimal. The PNGs are decoded on every core ahead of the encoder. The encode goes to the GPU first and falls back to H.264 on the CPU, saying why on a `note` line.

```sh
pcod frames frames trailer.mp4 --fps 60 --audio trailer.wav
```

```text
frames  600 PNG 1920x1080 at 60 fps (10.00 s), 16 decode threads
audio   trailer.wav: 10.00 s, AAC 48 kHz stereo
encode  H.264 GPU
encoded 600 frames in 1.07 s: 560 fps
wrote   trailer.mp4 (7.5 MB)
```

These are the same 10 seconds of the NASA clip, written out as 1080p PNG frames. The WAV can be 8, 16, 24 or 32-bit PCM or float, at any rate, mono, stereo or more channels. pcod converts it to 48 kHz stereo and cuts or pads it with silence to the length of the video. Without `--audio`, the MP4 carries a silent stereo track. A missing frame number, or a frame of another size, stops the encode:

```text
pcod: gap: the frames jump from 000001.png to 000003.png
```

## Stills and contact sheets

`stills` takes N evenly spaced stills (`--count`, 12 by default) or stills at given times (`--at`, comma separated: seconds, `m:ss` or `h:mm:ss`), each scaled to `--height` (360 by default) keeping its shape. `--out` writes `still-001.png`, `still-002.png`, ... into a folder, `--sheet` writes one contact sheet of `--rows` by `--cols` stills on black with `--pad` pixels (8 by default) between and around them, and you can give both. Without `--rows` and `--cols` the sheet is about square; with both and no `--count`, it takes rows times columns stills. Each still is the exact frame at its time; `--seek keyframe` takes the keyframe at or before it instead, which is faster on long GOPs. MP4, MOV, MKV and WebM work, on the GPU route first.

```sh
pcod stills clip.mp4 --count 24 --cols 6 --sheet sheet.png
```

```text
     1.250 s  640x360
     ...
sheet  6x4 stills, 3896x1480  sheet.png
24 stills in 0.37 s
```

`--json` prints one line instead, with each still's time (the frame shown), the time asked, its size and its PNG's path (null without `--out`), and the sheet:

```sh
pcod stills clip.mkv -n 6 --cols 3 --height 240 --seek keyframe --sheet sheet.png --json
```

```text
{"file":"clip.mkv","height":240,"seek":"keyframe","stills":[{"index":0,"time":0.000000,"asked":1.666667,"width":426,"height":240,"path":null},...],"sheet":{"path":"sheet.png","width":1310,"height":504,"rows":2,"cols":3,"pad":8},"seconds":0.021}
```

A time past the end of the video stops with one line, `pcod: clip.mp4: 65.000 s is past the end of the video (20.000 s)`.

## When something fails

A file pcod can't read, or a failing route, ends with one line on stderr and exit code 1:

```text
pcod: missing.mp4: No such file or directory
```

A command line it can't parse prints clap's error and usage on stderr with exit code 2; `pcod --help` and `pcod help <command>` print the full help.

pcod's output is plain text for people; only `stills` has a JSON mode (`--json`). For anything beyond these five commands, [use the library](../library/).
