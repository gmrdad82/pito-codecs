---
title: "Use the library"
description: "The media front picks the GPU first and the CPU second for every job, and says why whenever it falls back."
sidebar:
  order: 2
---

`pito_codecs::media` is the front most apps need: it opens MP4, MOV, MKV and WebM files, probes them, takes stills, decodes audio as PCM and exports edits, and picks the route for each job by itself. [Install](../../install/) the library first.

## Probe a file

```rust
use pito_codecs::media::{Prefs, probe};

let prefs = Prefs::default();
let info = probe("clip.mp4".as_ref(), &prefs)?;
println!(
    "{} {}x{} at {}/{} fps, {:.1} s, {} audio track(s)",
    info.video_codec_name, info.width, info.height, info.fps.0, info.fps.1, info.duration, info.audio.len()
);
```

```text
h264 1920x1080 at 60000/1001 fps, 10.0 s, 1 audio track(s)
```

`Probe` also carries the profile, bit depth, rotation (`shown_size()` gives the size as displayed), the frame count when the container knows it, and an `AudioTrack` per audio track with its codec, channels, rate, language and title. `unsupported()` names any codec in the file that pito-codecs can't decode.

Every time in the media front counts from the file's start, as ffmpeg's input times do: time 0 is the earliest start over the file's streams, `duration` runs from there to the end, and the times you pass to stills, `decode_video`, `decode_video_gpu`, the audio calls and export pieces, and the frame `pts` you get back, are on that clock. A file cut with its original timestamps kept (`-copyts`) therefore reads like any other. `start` gives the absolute time of time 0 in seconds, and `video_start` and each `AudioTrack`'s `start` give that stream's own first time when the reader knows it; an audio track that starts after time 0 reads as silence until its first sample.

## Routes

Every decode and encode runs on one of three routes, `Route::Gpu`, `Route::Cpu` or `Route::Ffmpeg`. The order is always the GPU first, then our own CPU code; ffmpeg is used only when the app's settings pick it and finds it installed. `Prefs` holds those settings:

```rust
pub struct Prefs {
    pub decode: Choice,
    pub encode: Choice,
    pub hevc_encode_fallback: Vec<Fallback>,
}

pub enum Choice { Auto, Gpu, Cpu, Ffmpeg(UserFfmpeg) }
```

- `Choice::Auto` (the default) and `Choice::Gpu` try the GPU, then the CPU.
- `Choice::Cpu` stays on the CPU.
- `Choice::Ffmpeg(UserFfmpeg { path, codec })` runs the user's own ffmpeg first, with an optional encoder name such as `libx264`, and falls back to the GPU and the CPU when it is missing or can't do the job. `Prefs::ffmpeg()` picks it for both directions.
- `hevc_encode_fallback` says where an HEVC encode goes when no GPU HEVC encoder is there: H.264 on the GPU, then H.264 on the CPU, by default. An empty list makes such an encode fail instead.

## Ask which route a job takes

`Caps::detect` looks at what this machine has; `pick_decode` and `pick_encode` return the route the front would take, with the reasons for every fallback. `Picked::said()` puts it in one line an app can show:

```rust
use pito_codecs::media::{Caps, Choice, Codec, Prefs, pick_decode, pick_encode};

let prefs = Prefs::default();
let caps = Caps::detect(&prefs);
println!("decode: {}", pick_decode(Codec::H264, &prefs, &caps)?.said());
println!("encode: {}", pick_encode(Codec::Hevc, &prefs, &caps)?.said());

let cpu = Prefs { decode: Choice::Cpu, encode: Choice::Cpu, ..Prefs::default() };
println!("encode on the CPU: {}", pick_encode(Codec::Hevc, &cpu, &Caps::detect(&cpu))?.said());

let strict = Prefs { encode: Choice::Cpu, hevc_encode_fallback: vec![], ..Prefs::default() };
if let Err(e) = pick_encode(Codec::Hevc, &strict, &Caps::detect(&strict)) {
    println!("no fallback: {e}");
}
```

On a Linux machine with an AMD card:

```text
decode: H.264 GPU
encode: HEVC GPU
encode on the CPU: H.264 CPU (HEVC encodes on the GPU only; encoding H.264 on the CPU instead)
no fallback: unsupported: can't encode HEVC: HEVC encodes on the GPU only
```

You don't need to call these before a job: every function of the front picks its route the same way. They are there so an app can show the route in its settings or status line.

## Reasons travel with the result

Jobs that can fall back report what happened: `Video` and `GpuVideo` (from `decode_video` and `decode_video_gpu`) carry `route` and `reasons`, an export returns them in `Exported::reasons`, and `AudioStream` carries its `route`. Each reason is a plain sentence, such as "no GPU HEVC decoder on this machine" or "the GPU encoder failed (…); encoding H.264 on the CPU instead".

## Errors

Every call returns `pito_codecs::media::Result<T>`. A malformed or truncated file returns an error, never a panic: every parser and decoder is fuzzed. The `Error` variants say what kind of failure it was (`Invalid` data, `Unsupported` input, a `Backend` such as a GPU driver, `Io`, or `Cancelled` by a progress callback) and display as one plain line.

Next: [first frames and filmstrips](../stills/), [audio as PCM](../audio/), [smart-cut exports](../export/).
