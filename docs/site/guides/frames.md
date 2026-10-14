---
title: "Export frames you render yourself"
description: "Encode frames your app makes, at your own size and frame rate, with your own audio, to an MP4."
sidebar:
  order: 7
---

`export_frames` writes frames your code makes, such as a vertical cut or a composed scene, to an MP4 at the size and frame rate of a `Canvas`, with premixed AAC audio sample-exact against the video. It routes the encode like `export`: the GPU first, then H.264 on the CPU.

```rust
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub frames: Option<u64>,
}
```

The width and height must be even. `frames` is the frame count: the export ends after that many frames or when the frame callback returns `None`, whichever comes first, and with `None` it ends only on the callback's `None`.

## Three seconds of vertical video

```rust
use pito_codecs::media::{
    AudioBuffer, Canvas, ExportOptions, Frame, PixelFormat, TimeBase, VideoFrame, export_frames,
};

let canvas = Canvas { width: 1080, height: 1920, fps: (30, 1), frames: Some(90) };
let done = export_frames(
    "vertical.mp4".as_ref(),
    canvas,
    ExportOptions::default(),
    |n, spent| {
        if n == 90 {
            return Ok(None);
        }
        let mut frame = spent.unwrap_or_else(|| {
            Frame::new(PixelFormat::Yuv420p8, canvas.width, canvas.height, 0, TimeBase::new(1, 30))
        });
        frame.planes[0].data.fill((16 + (n * 2) % 220) as u8);
        frame.planes[1].data.fill(128);
        frame.planes[2].data.fill(128);
        Ok(Some(VideoFrame::Cpu(frame)))
    },
    |_frames, samples| {
        let mut buf = AudioBuffer::new(48_000, 2);
        buf.samples = vec![0.0; samples * 2];
        Ok(buf)
    },
    |_| true,
)?;
```

```text
90 frames, 0 copied, 90 encoded, smart false, video H.264 GPU
```

## The callbacks

- **Frames:** called with the frame's index and the last frame back (`None` at first), so you can draw into it instead of allocating. Return `VideoFrame::Cpu` with an 8-bit 4:2:0 `Frame` of the canvas size, or `VideoFrame::Gpu` with a frame already on the card, which goes to a GPU encoder without a copy. Return `Ok(None)` to end. The export sets each frame's timestamp.
- **Audio:** called about once a second of video, from a thread of its own, with the number of frames so far and the number of 48 kHz samples that cover them since the last call. Return exactly that many interleaved stereo samples, `samples * 2` floats.
- **Progress:** as for `export`; returning `false` cancels.

`ExportOptions.video` may be `Codec::H264` or `Codec::Hevc`, and `ExportOptions.overlay` draws a layer on the frames as in [Overlays and captions](../overlays/). A user's ffmpeg is never used here: the reasons say that frames made by the caller are encoded by our own encoders.
