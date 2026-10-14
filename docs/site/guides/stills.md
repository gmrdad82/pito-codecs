---
title: First frames and filmstrips
description: Show a file's first picture at once, take a strip of stills, or decode on to the exact frame at a time.
sidebar:
  order: 3
---

Three functions of the `media` front return pictures as `Rgba` (8-bit RGBA, `width`, `height`, `data`), scaled to the height you ask for with the width following the picture's shape and rotation.

## The first frame

```rust
use pito_codecs::media::{Prefs, first_frame};

let still = first_frame("clip.mp4".as_ref(), 240, &Prefs::default())?;
```

```text
first frame 426x240 in 52.9 ms
```

That time is a cold start in a fresh process on a busy machine; the README's measured run gives 4.1 ms for 1440p H.264 on the CPU and 12.4 ms for HEVC on the GPU, 7× to 21× sooner than ffmpeg.

![Time to first frame at 1440p, pito-codecs against ffmpeg](../images/chart-first-frame.png)

## A filmstrip

`frames_at` takes a still at each time you give, in seconds. Each still is the keyframe at or before its time, which is what a filmstrip wants, and only the packets it needs are read. The stills are decoded in parallel.

```rust
use pito_codecs::media::{Prefs, frames_at, probe};

let prefs = Prefs::default();
let info = probe("clip.mp4".as_ref(), &prefs)?;
let times: Vec<f64> = (0..12).map(|i| info.duration * i as f64 / 12.0).collect();
let strip = frames_at("clip.mp4".as_ref(), &times, 120, &prefs)?;
```

```text
12 stills at 212x120 in 57.7 ms
```

In MKV and WebM, stills seek through the file's Cues, so a still late in a long recording costs the same as one at the start.

![Twelve stills across a NASA clip, taken by frames_at](../images/filmstrip.gif)

![Filmstrip stills per second, pito-codecs against ffmpeg](../images/chart-filmstrip.png)

## The exact frame

`frames_at_seek` with `Seek::Exact` decodes on from the keyframe to the frame shown at each time. Times inside one group of pictures share one decode.

```rust
use pito_codecs::media::{Prefs, Seek, frames_at_seek};

let exact = frames_at_seek("clip.mp4".as_ref(), &[2.5, 2.6, 7.25], 240, Seek::Exact, &Prefs::default())?;
```

```text
3 exact stills in 300.0 ms
```

`Seek::Keyframe` is the default and is what `frames_at` uses. An exact still costs more the further its time is from the keyframe before it: the clip above has a keyframe only every few seconds.

## Save a still

Encode a still as WebP or GIF with the library's own encoders:

```rust
use pito_codecs::webp;

let data = webp::encode(&still, &webp::Options::lossy(80.0))?;
std::fs::write("first.webp", &data)?;
```

[Low-level codecs](../low-level/) has more on GIF and WebP. To read every frame rather than stills, use `decode_video` (frames in system memory) or `decode_video_gpu` (frames that stay on the card when the GPU decodes); both start at any time and can scale to a height.
