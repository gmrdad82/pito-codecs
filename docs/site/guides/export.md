---
title: Export an edit with smart cuts
description: Write pieces of one or more files to a new MP4, copying the middle of each piece and re-encoding only around each cut.
sidebar:
  order: 5
---

`export` writes an edit, a list of `Piece`s, to an MP4. Each piece is a time range of a file:

```rust
pub struct Piece {
    pub path: PathBuf,
    pub from: f64,
    pub to: f64,
}
```

The export is frame-accurate and fast: where it can, it copies the middle of each piece as it is and re-encodes only the frames from each cut to the next keyframe. That is a **smart** export, and it ran 1.7× to 2.9× faster than ffmpeg re-encoding every frame.

![Smart-cut export in times real time](../images/chart-export.png)

## A three-piece cut

```rust
use pito_codecs::media::{ExportOptions, Piece, export};

let clip = std::path::PathBuf::from("clip.mp4");
let pieces = vec![
    Piece { path: clip.clone(), from: 0.5, to: 2.75 },
    Piece { path: clip.clone(), from: 4.1, to: 6.3 },
    Piece { path: clip.clone(), from: 8.0, to: 9.6 },
];
let done = export("edit.mp4".as_ref(), &pieces, ExportOptions::default(), |progress| {
    println!("{:?} {:.0}%", progress.stage, progress.done * 100.0);
    true
})?;
println!("{} frames, {} copied, {} encoded, smart {}, video {}", done.frames, done.copied, done.encoded, done.smart, done.video.said());
for r in &done.reasons {
    println!("  reason: {r}");
}
```

On the 10-second 1080p60 clip from the [pcod guide](../pcod/):

```text
362 frames, 200 copied, 162 encoded, smart true, video H.264 GPU
  reason: frames stay on the GPU from decode to encode, overlay included, through VA-API
```

The progress callback gets a `Progress` with the `stage` (`Cutting`, `Encoding`, `Mixing`, `Joining`), `done` from 0.0 to 1.0 and the encoder's route; returning `false` cancels the export with `Error::Cancelled`. The pieces can come from different files.

## ExportOptions

```rust
pub struct ExportOptions {
    pub video: Codec,
    pub crf: u8,
    pub audio_kbps: u32,
    pub audio_gains: Vec<f32>,
    pub overlay: Option<Overlay>,
    pub prefs: Prefs,
}
```

`video` is the output codec (`Codec::H264` by default, `Codec::Hevc` or `Codec::Vp9`), `crf` the quality of the encoded frames (18 by default), `audio_kbps` the AAC rate (192), `audio_gains` a gain per source audio track for the mix, `overlay` a picture drawn on every frame ([Overlays and captions](../overlays/)) and `prefs` the [routes](../library/#routes).

## What the reasons say

`Exported` returns the frame counts, whether the export was smart, the video route as `Picked`, and `reasons`: one sentence for each thing that made the export slower than the best case, or that an app may want to show. The export is re-encoded whole, and says why, when:

- an overlay is drawn on every frame;
- the source codec differs from the export's, such as an H.264 source exported as HEVC: "the source video is h264, the export HEVC, so the whole export is re-encoded";
- the pieces' files differ in frame rate, size, bit depth or rotation;
- a VP9 source is not profile 0 at 8 bits, since our VP9 encoder writes only that;
- the picture has an odd width or height, which is evened;
- our MP4 and MKV readers can't index a source to copy from it.

Other reasons tell the route: a GPU encoder that failed and was replaced by H.264 on the CPU, an HEVC fallback, or that frames stay on the GPU from decode to encode.

## VP9

A VP9 source in WebM or MKV, exported with `video: Codec::Vp9`, cuts the same way into MP4 (`vp09`): packets are copied as they are, superframes included, and only the frames before each piece's first keyframe are encoded, on the CPU, in parallel keyframe-led chunks.

```rust
let opts = ExportOptions { video: Codec::Vp9, ..ExportOptions::default() };
let done = export("edit-vp9.mp4".as_ref(), &pieces_of_the_webm, opts, |_| true)?;
```

```text
362 frames, 158 copied, 204 encoded, smart true, video VP9 CPU
```

## With your own audio

`export` copies and mixes the sources' audio. To write audio you mixed yourself, `export_with_audio` calls back for each piece with its index and its exact sample count, and takes that many 48 kHz stereo samples, which our AAC encoder turns into one track as long as the video to the sample:

```rust
use pito_codecs::media::{AudioBuffer, ExportOptions, export_with_audio};

let done = export_with_audio(
    "edit.mp4".as_ref(),
    &pieces,
    ExportOptions::default(),
    |piece, samples| {
        let mut buf = AudioBuffer::new(48_000, 2);
        buf.samples = mixer.render(piece, samples);
        Ok(buf)
    },
    |_| true,
)?;
```

Here `mixer` stands for your own code; it must return `samples` frames of interleaved stereo, `samples * 2` floats.
