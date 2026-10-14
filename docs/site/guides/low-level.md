---
title: "Low-level codecs"
description: "Use the codecs and containers directly, without the front - decode an MP4's H.264 track, encode H.264, and read and write GIF and WebP."
sidebar:
  order: 9
---

Under the `media` front, every codec and container is a module of its own: `h264`, `hevc`, `vp9`, `aac`, `ac3`, `mp3`, `opus`, `mp4`, `mkv`, `gif`, `webp`, `filter` and `gpu`. Use them when you want to drive the decode yourself. The [API reference](../../api/pito_codecs/index.html) lists every type.

## Decode an MP4's H.264 track

```rust
use pito_codecs::{h264, mp4};

let file = mp4::Reader::open("clip.mp4")?;
let video = file.video().ok_or("no video track")?;
let track = &file.tracks[video];
let mut decoder = h264::Decoder::with_extradata(&track.extradata().unwrap_or_default())?;
let mut frames = Vec::new();
for index in 0..track.samples.len() {
    frames.extend(decoder.decode(&file.packet(video, index)?)?);
}
frames.extend(decoder.flush()?);
```

```text
decoded 600 frames, 1920x1080, in 0.79 s
```

Each `Frame` holds three planes (`planes[0]` luma, then the two chroma planes, each with its `stride`), a `format` such as `PixelFormat::Yuv420p8`, and a `pts` in its `time_base`. The decoder hands frames out in display order and may hold some back until later packets arrive; `flush` returns the rest. `media::to_rgba` turns a frame into RGBA.

## Encode H.264

```rust
use pito_codecs::h264::{Encoder, EncoderConfig};
use pito_codecs::media::Rate;

let mut encoder = Encoder::new(EncoderConfig {
    width: 1920,
    height: 1080,
    fps: (60_000, 1001),
    rate: Rate::Crf(20),
    ..Default::default()
})?;
let avcc = encoder.avcc();
for frame in &frames {
    for packet in encoder.encode(frame)? {
        sink.write(&packet.data);
    }
}
for packet in encoder.flush()? {
    sink.write(&packet.data);
}
```

```text
encoded 120 frames into 120 packets, 1072669 bytes, avcC 48 bytes, in 1.37 s
```

`sink` stands for wherever the packets go. `EncoderConfig` also sets the `profile`, the `preset` (speed, balanced or quality), the `gop`, the number of `b_frames`, the `threads`, and the `bitstream`: length-prefixed for MP4 with `avcc()` as its configuration (the default), or `Bitstream::AnnexB` for a raw `.h264` file. `Rate` is `Crf(n)`, `Qp(n)`, `Bitrate(kbps)` (constant) or `Vbr { kbps, max_kbps }`.

## Decode on the GPU

```rust
use pito_codecs::gpu::{self, HwDecoder};
use pito_codecs::media::Codec;

let mut decoder = HwDecoder::new(Codec::H264, &track.extradata().unwrap_or_default())?;
let frames = decoder.decode(&file.packet(video, 0)?)?;
```

`HwDecoder::new` fails when no GPU decoder for the codec is there, so check `gpu::available()` first or fall back to the CPU decoder; `HwEncoder` is its encoding twin. The [GPU guide](../gpu/) lists what each system has.

## GIF

```rust
use pito_codecs::gif::{self, Dither, EncodeOptions};

let data = gif::encode(&stills, &vec![4; stills.len()], EncodeOptions {
    colors: 128,
    dither: Dither::Ordered,
    ..Default::default()
})?;
let back = gif::decode(&data)?;
```

```text
gif: 30 frames, 1669056 bytes, 38.3 ms
gif decoded back: 30 frames
```

Frames are `Rgba`, delays are in hundredths of a second. The encoder builds Oklab palettes (global or per frame), dithers with ordered or Floyd-Steinberg patterns, and stores only the changed area of each frame; `gif::Encoder` takes frames one at a time.

![An animated GIF of a NASA clip, encoded by pito-codecs](../images/animated.gif)

## WebP

```rust
use pito_codecs::webp::{self, AnimEncoder, Options};

let still = webp::encode(&image, &Options::lossless())?;
let mut anim = AnimEncoder::new(480, 270, Options::lossy(75.0), 0)?;
for frame in &stills {
    anim.add(frame, 40)?;
}
let data = anim.finish()?;
let info = webp::info(&data)?;
```

```text
webp: 532888 bytes, 64.8 ms
webp info: 480x270, 30 frames, animated true
```

Lossless (VP8L) and lossy (VP8) stills and animations, with alpha; durations are in milliseconds, and the last argument of `AnimEncoder::new` is the loop count (0 loops forever). `webp::decode` and `webp::decode_animation` read them back, bit-exact with libwebp.
