---
title: "pito-codecs"
description: "Video and audio codecs in pure Rust, on the CPU and the GPU, with no ffmpeg inside."
sidebar:
  order: 1
---

![A terminal running pcod on a 1080p H.264 clip: it probes the file, decodes it on the CPU and on the GPU through VA-API with the speed of each, then encodes it to HEVC on the GPU.](images/decode.gif)

**Video and audio codecs in pure Rust, on the CPU and the GPU, with no ffmpeg inside.** H.264, H.265, VP9, AAC, AC-3, E-AC-3, Opus, MP3 and FLAC; MP4, MOV, MKV and WebM; GIF and WebP. One MIT Cargo package, a feature per part, two direct dependencies and no C linked.

It comes as a Rust library and as `pcod`, a small command line over it that probes, decodes and encodes MP4 files.

- **First frame 7× to 21× faster than ffmpeg:** 4.1 ms against 87.4 ms for a 1440p H.264 file on the CPU.
- **Filmstrips 8× to 12× faster:** 12 stills at 240 px in 29 to 72 ms.
- **Frame-accurate smart cuts 1.7× to 2.9× faster** than an ffmpeg re-encode: only the frames around each cut are encoded.
- **GPU decode and encode on VA-API 1.1× to 1.3× faster** than ffmpeg's, and frames can stay on the card from decode to encode.
- **Overlays composed on the card:** a marked or captioned export uses 18× to 23× less CPU time than decoding and blending on the CPU.
- **Small:** a 5.7 MB command line, and a library that pulls in 8 crates on Linux.

![pito-codecs against ffmpeg 9.0.1, times faster or slower per job](images/chart-overview.png)

Every speed number here was measured on one machine (AMD Ryzen 9 9950X, AMD Radeon RX 9060 XT on Mesa's radeonsi VA-API driver, Linux, ffmpeg 9.0.1) on public-domain NASA footage. The repository's README gives the input, the commands and the load for each.

## Where to start

- [Install](install/) pcod or add the library to a Rust project.
- [Probe, decode and encode with pcod](guides/pcod/) from a terminal.
- [Use the library](guides/library/): the `media` front picks the fastest route for each job and says why when it falls back.
- [First frames and filmstrips](guides/stills/), [audio as PCM](guides/audio/), [smart-cut exports](guides/export/), [overlays and captions](guides/overlays/) and [exports of your own frames](guides/frames/).
- [GPU routes per vendor and system](guides/gpu/): what is tested and what is built but untested.
- [Low-level codecs](guides/low-level/): H.264 decode and encode, GIF and WebP without the front.
- The [command reference](reference/cli/), the [formats](reference/formats/) and the [API reference](api/pito_codecs/index.html).

## Licence

The code is MIT licensed, © Catalin Ilinca. The PITO MD name and its logos are © Catalin Ilinca, all rights reserved, and are not covered by the MIT licence. pito-codecs is the media layer of [PITO MD](https://pitomd.com); its source is at [github.com/gmrdad82/pito-codecs](https://github.com/gmrdad82/pito-codecs).
