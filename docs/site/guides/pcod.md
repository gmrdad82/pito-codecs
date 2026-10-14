---
title: Probe, decode and encode with pcod
description: Look inside an MP4, decode it on the CPU or the GPU, and encode it to H.264 or HEVC from the command line.
sidebar:
  order: 1
---

pcod is a small command line over the library. It works on MP4 files: `probe` shows what is inside, `decode` decodes the whole video track and reports the speed, and `encode` decodes, encodes and muxes a new MP4, copying the audio. Every option is in the [command reference](../../reference/cli/).

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

## When something fails

A file pcod can't read, or a failing route, ends with one line on stderr and exit code 1:

```text
pcod: missing.mp4: No such file or directory
```

A command line it can't parse prints clap's error and usage on stderr with exit code 2; `pcod --help` and `pcod help <command>` print the full help.

pcod's output is plain text for people; it has no JSON mode. For anything beyond these three commands, [use the library](../library/).
