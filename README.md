<p align="center"><img src="docs/images/decode.gif" width="800" alt="A terminal running pcod on a 1080p H.264 clip: it probes the file, decodes it at about 1,440 fps on the CPU and about 1,700 fps on the GPU through VA-API, then encodes it to HEVC on the GPU at about 620 fps."></p>

# pito-codecs

[![CI](https://github.com/gmrdad82/pito-codecs/actions/workflows/ci.yml/badge.svg)](https://github.com/gmrdad82/pito-codecs/actions/workflows/ci.yml) [![Version](https://img.shields.io/github/v/tag/gmrdad82/pito-codecs)](https://github.com/gmrdad82/pito-codecs/tags)

pito-codecs is a pure Rust library of fast video, audio and image codecs and containers that run on the CPU and on the GPU. It covers H.264, H.265, VP9, AAC, Opus, MP4, MKV and WebM, GIF and WebP. It is part of [PITO](https://pitomd.com) and ships as one MIT Cargo package with a feature per part. No ffmpeg is linked or bundled: ffmpeg serves only as a test oracle here, and as an optional route an app can offer when its user has ffmpeg installed.

## Status

`v0.1.0` is the first release. The H.264, H.265, AAC, MP4 and filter parts and the app front are its stable core. The newer formats work and are tested, and they get faster and fuller in each minor release up to `1.0.0`.

| Part | Decode | Encode |
| --- | --- | --- |
| H.264 | CPU: Baseline, Main and High, progressive 8-bit 4:2:0, CAVLC and CABAC, bit-exact with ffmpeg. GPU: VA-API and VideoToolbox for the same profiles, bit-exact. | CPU: Baseline, Main and High with CABAC and B-frames; CRF, QP, CBR and VBR. GPU: VA-API High; VideoToolbox Main and High. |
| H.265 | CPU: Main and Main 10, tiles and WPP, bit-exact with ffmpeg. GPU: VA-API and VideoToolbox, Main and Main 10, bit-exact. | GPU only: VA-API Main; VideoToolbox Main and Main 10. Without a GPU HEVC encoder it falls back to H.264 and says why. |
| VP9 | CPU: profiles 0 to 3 (8, 10 and 12-bit; 4:2:0, 4:2:2 and 4:4:4), tiles, bit-exact with ffmpeg and libvpx. | CPU: profile 0, with motion search, trellis, rate control, AQ and tile threads. ffmpeg and libvpx decode it bit-exact to our reconstruction. Still slow. |
| AAC | AAC-LC, mono to 7.1. | AAC-LC, CBR and VBR. |
| Opus | SILK, CELT, hybrid and multistream; the RFC 8251 conformance vectors decode to the reference's final range on every packet. | CELT, VBR and CBR, 6 to 510 kbps, decoded clean by libopus. SILK and hybrid are in progress. |
| MP4 and MOV | Tracks, sample tables, rotation, edit lists, fragmented files. | MP4 or MOV, faststart or fragmented; remux of a time range without re-encoding. |
| MKV and WebM | Tracks, lacing, Cues, seeking. | H.264, HEVC, VP9, Opus and AAC; WebM's codec rules enforced; remux. |
| GIF | GIF87a and 89a, interlace, disposal, transparency, loops. | Oklab palettes, ordered or Floyd-Steinberg dither, frame differencing. |
| WebP | Lossless VP8L and lossy VP8, alpha, animation; bit-exact with libwebp. | In progress (lossless first). |
| Filters | | Scale (bilinear, bicubic, Lanczos), BT.601/709/2020 colour, rotate, flip, crop, pad, fps change, RGBA overlay, resample, downmix, gain, EBU R128 loudness, true-peak limiter, normalise. |

The GPU route is tested on Linux with VA-API, on an AMD Radeon RX 9060 XT with Mesa's radeonsi driver, and on macOS with VideoToolbox on GitHub's Apple Silicon runners, where CI checks its decode bit-exact with ffmpeg and decodes its encodes back on every push. NVENC (Linux and Windows) and Media Foundation (Windows) compile for their targets and pass their unit tests, but CI's runners have no GPU, so they stay untested until they run on their hardware.

Known gaps in `v0.1.0`, being worked on now:

- **CPU decode speed.** Our H.264 and H.265 decoders run at 0.3 to 0.7 times ffmpeg's speed on the CPU; the GPU route is faster than ffmpeg's.
- **CPU H.264 encode quality and speed.** Our CPU H.264 encoder trails libx264 on both.

## What it is

- **Speed first.** Decoders are threaded across rows and frames, SIMD kernels are picked at run time, and buffers are pooled. GPU frames stay on the GPU from decode to encode.
- **Video:** H.264 and H.265 decode and encode on the CPU and the GPU (H.265 encodes on the GPU only), and VP9 decode and encode on the CPU.
- **Audio:** AAC-LC and Opus decode and encode.
- **Containers:** MP4, MOV, MKV and WebM read, write and remux; Ogg Opus.
- **Images:** GIF and WebP, still and animated.
- **Filters** between decode and encode (`filter`): scaling, colour, geometry, frame rate, overlays, resampling and loudness.
- **A front for apps** (`media`), which picks the fastest working route for each call. It can probe a file, give the first frame or stills for a filmstrip, play a file, pull its audio as PCM over any time range or as a stereo mix of its tracks with a gain each, and export with smart cuts, which copy the middle of each piece and re-encode only the edges.

## Install

Add it as a git dependency at a tag, with the features you need (all are on by default):

```toml
[dependencies]
pito-codecs = { git = "https://github.com/gmrdad82/pito-codecs", tag = "v0.1.2", default-features = false, features = ["h264", "mp4"] }
```

| Feature | What it holds |
| --- | --- |
| `media` | The shared types (`Codec`, `Frame`, `Packet`, `AudioBuffer`, `Error`) and the front for apps. |
| `h264` | H.264 decode and encode on the CPU. |
| `hevc` | H.265 decode on the CPU, and hvcC parsing. |
| `vp9` | VP9 decode and encode on the CPU. |
| `aac` | AAC-LC decode and encode. |
| `opus` | Opus decode and encode, Ogg Opus. |
| `mp4` | MP4 and MOV read, write and remux. |
| `mkv` | Matroska and WebM read, write and remux. |
| `gif` | GIF decode and encode. |
| `webp` | WebP decode and encode. |
| `gpu` | The GPU routes: VA-API on Linux (libva is loaded at run time, so a binary still starts without it); NVENC, VideoToolbox and Media Foundation behind `cfg(target_os)`. |
| `filter` | Scale, colour, geometry, fps, overlay, resample, loudness. |

Rust 1.97 or newer, edition 2024.

## Use

Decode an MP4's H.264 track on the CPU:

```rust
use pito_codecs::{h264, mp4};

let file = mp4::Reader::open("clip.mp4")?;
let video = file.video().expect("a video track");
let track = &file.tracks[video];
let mut decoder = h264::Decoder::with_extradata(&track.extradata().unwrap_or_default())?;
for index in 0..track.samples.len() {
    for frame in decoder.decode(&file.packet(video, index)?)? {
        println!("{} {}x{}", frame.pts, frame.width, frame.height);
    }
}
let rest = decoder.flush()?;
```

Encode frames to H.264 at CRF 18:

```rust
use pito_codecs::h264::{Encoder, EncoderConfig, Rate};

let mut encoder = Encoder::new(EncoderConfig { width: 1920, height: 1080, fps: (60, 1), rate: Rate::Crf(18), ..Default::default() })?;
let avcc = encoder.avcc();
let packets = encoder.encode(&frame)?;
```

Decode on the GPU when one is there:

```rust
use pito_codecs::gpu::{self, HwDecoder};
use pito_codecs::media::Codec;

println!("{:?}", gpu::available());
let mut decoder = HwDecoder::new(Codec::Hevc, &hvcc)?;
let frames = decoder.decode(&packet)?;
```

Or let the front pick the route:

```rust
use pito_codecs::media::{Prefs, first_frame, probe};

let prefs = Prefs::default();
let info = probe("clip.mp4".as_ref(), &prefs)?;
let still = first_frame("clip.mp4".as_ref(), 240, &prefs)?;
```

`examples/pcod.rs` is a small command line over the library, and it is what the GIF above shows: `cargo run --release --example pcod -- decode clip.mp4 --route gpu`.

## Routes

- GPU first, then the CPU code here. A user-provided ffmpeg is used only when the app's settings pick it, per decode and per encode.
- H.265 encodes on the GPU only. Without a GPU HEVC encoder it falls back to H.264 on the GPU, then H.264 on the CPU, and says why.
- Every fallback comes with a plain reason the app can show.

## Speed

Each pair below was measured back to back against the system ffmpeg on the same file. The machine:

- **CPU:** AMD Ryzen 9 9950X (16 cores).
- **GPU:** AMD Radeon RX 9060 XT on Mesa 26.2 radeonsi through VA-API, on Linux.
- **ffmpeg:** 9.0.1.

The input is 20 s of NASA's public-domain "A Message of Resilience from the International Space Station" (720p60). It was scaled to 1080p and 1440p and encoded with libx264 at CRF 20 and libx265 at CRF 23. The command is `pgpu run -- cargo run --release --example bench -- --runs 3`, which writes `tmp/bench/results.json` and the charts below. Each number is the median of three runs. The machine was busy with other builds (load average 34 to 38), so the absolute numbers are floors.

![Speed against ffmpeg, per job](docs/images/bench-speedup.png)

| Job | pito-codecs | ffmpeg |
| --- | --- | --- |
| H.264 1080p decode, VA-API, frames stay on the GPU | 1059 fps | 899 fps |
| H.264 1440p decode, VA-API, frames stay on the GPU | 1085 fps | 905 fps |
| HEVC 1440p decode, VA-API, frames stay on the GPU | 1099 fps | 804 fps |
| H.264 1080p decode, VA-API, frames read back | 447 fps | 278 fps |
| H.264 1080p decode, CPU, all threads | 545 fps | 971 fps |
| HEVC 1080p decode, CPU, all threads | 384 fps | 897 fps |
| H.264 1440p encode, VA-API, QP 23 | 413 fps | 342 fps |
| HEVC 1440p encode, VA-API, QP 23 | 534 fps | 294 fps |
| H.264 1440p GPU transcode, frames stay on the GPU | 313 fps | 274 fps |
| H.264 1080p encode, CPU, CRF 23 (ffmpeg: libx264 medium) | 83 fps | 189 fps |
| H.264 1440p time to first frame, CPU (ffmpeg includes process start) | 8 ms | 150 ms |
| H.264 1440p filmstrip, 12 stills at 240 px, GPU first | 205 stills/s | 26 stills/s |
| Smart-cut export 1080p, 3 cuts, frame-accurate (ffmpeg: full re-encode) | 7.2x real time | 4.9x real time |
| Scale 1440p to 1080p, bicubic | 1683 fps | 778 fps |
| AAC encode, 128 kbps stereo | 211x real time | 116x real time |
| EBU R128 loudness, 48 kHz stereo | 3007x real time | 502x real time |

The CPU decoders have improved since this run: H.264 1440p decode on 16 threads is now about 926 fps and HEVC 1080p about 717 fps, while VP9 and Opus are newer still. The next release reruns the benchmark on a quiet machine.

![Decode frames per second](docs/images/bench-decode.png)
![Encode frames per second](docs/images/bench-encode.png)
![First frame, filmstrip, smart cut and scale](docs/images/bench-pipeline.png)
![Audio real-time factors](docs/images/bench-audio.png)

## Roadmap

Each minor release up to `1.0.0` lands as its group is finished and measured:

- CPU decode at or past ffmpeg's speed for H.264, H.265 and VP9, with hand-written AVX2 kernels.
- The CPU H.264 encoder at libx264's quality per bit.
- VP9 decode through VA-API, and a faster VP9 encoder.
- Opus SILK and hybrid encode for speech.
- WebP lossless and lossy encode.
- `1.0.0` comes when every format here decodes and encodes, the API is settled and these numbers are current.

## Build and test

```
cargo build --release
cargo test --release
```

The tests use the system ffmpeg as an oracle when it is installed and skip those checks when it is not, and the GPU tests skip cleanly without a device. `bin/gate` runs rustfmt, clippy with warnings as errors and the tests, then a release build (`--fast` skips the build).

[CI](.github/workflows/ci.yml) runs on every push and pull request. One job checks formatting and runs clippy for Linux x64 and arm64, Windows and macOS. Then four runners test in parallel: Linux x64 and Linux arm64 (Debian with its ffmpeg), Windows x64 and macOS on Apple Silicon, each with every test in release under nextest, the doc tests and a release build. On a `v` tag it also checks that the tag matches the crate's version. What that covers:

- **CPU codecs, containers and filters** on x86-64 with the AVX2 kernels, and on arm64 with the portable paths and the filters' NEON kernels, on Linux, Windows and macOS.
- **VideoToolbox** on macOS: H.264 and H.265 decode bit-exact with ffmpeg, H.264 and H.265 encode decoded back by ffmpeg, bitrate modes, and a transcode that scales on the GPU.
- **Media Foundation and NVENC** only build and run their unit tests, since the runners have no GPU.
- **VA-API** runs on the maintainer's AMD machine before each release, with the benchmarks.

## Contributing

Issues are welcome at [github.com/gmrdad82/pito-codecs/issues](https://github.com/gmrdad82/pito-codecs/issues). Useful ones include a file that decodes wrong or slowly (with a small sample, where you can share one), a route that falls back when it shouldn't, or a format you need. Pull requests aren't taken. Please read the [code of conduct](CODE_OF_CONDUCT.md) first, and report security issues privately, as [SECURITY.md](SECURITY.md) describes.

## Credits

Parts are adapted from [FilmCraft](https://github.com/storytold/filmcraft) (MIT OR Apache-2.0, taken under MIT) and from libwebp (BSD-3-Clause). The VA-API and NVENC declarations follow the MIT-licensed libva and NVIDIA headers. FFmpeg, libvpx, libopus and x264 were never copied; ffmpeg serves as a test oracle and as an optional route the user provides. The ITU-T H.264 and H.265 Recommendations, the VP9 specification, RFC 6716, RFC 6386, the GIF89a specification and the ISO MP4 and AAC standards are the sources. Origins are listed in [INSPIRATION.md](INSPIRATION.md) and their notices in [NOTICE](NOTICE). The demo and benchmark clips are public-domain NASA footage from PITO's fixture sets.

## Licence

The code is MIT licensed ([LICENSE](LICENSE)), © Catalin Ilinca. The PITO name and its logos are © Catalin Ilinca, all rights reserved, and are not covered by the MIT licence ([TRADEMARKS.md](TRADEMARKS.md)).
