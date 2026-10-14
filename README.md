<p align="center"><img src="docs/images/decode.gif" width="800" alt="A terminal running pcod on a 1080p H.264 clip: it probes the file, decodes it on the CPU and on the GPU through VA-API with the speed of each, then encodes it to HEVC on the GPU."></p>

# pito-codecs

[![CI](https://github.com/gmrdad82/pito-codecs/actions/workflows/ci.yml/badge.svg)](https://github.com/gmrdad82/pito-codecs/actions/workflows/ci.yml) [![Version](https://img.shields.io/github/v/tag/gmrdad82/pito-codecs)](https://github.com/gmrdad82/pito-codecs/tags)

**Video and audio codecs in pure Rust, on the CPU and the GPU, with no ffmpeg inside.** H.264, H.265, VP9, AAC, AC-3, E-AC-3, Opus and MP3; MP4, MOV, MKV and WebM; GIF and WebP. One MIT Cargo package, a feature per part, two direct dependencies and no C linked.

- **First frame 7× to 21× faster than ffmpeg:** 4.1 ms against 87.4 ms for a 1440p H.264 file on the CPU.
- **Filmstrips 8× to 12× faster:** 12 stills at 240 px in 29 to 72 ms.
- **Frame-accurate smart cuts 1.7× to 2.9× faster** than an ffmpeg re-encode: only the frames around each cut are encoded.
- **GPU decode and encode on VA-API 1.1× to 1.3× faster** than ffmpeg's, and frames can stay on the card from decode to encode.
- **Overlays composed on the card:** a marked or captioned export uses 18× to 23× less CPU time than decoding and blending on the CPU.
- **Small:** a 5.7 MB command line, and a library that pulls in 8 crates on Linux.

![pito-codecs against ffmpeg 9.0.1, times faster or slower per job](docs/images/chart-overview.png)

Every speed number on this page was measured on one machine (AMD Ryzen 9 9950X, AMD Radeon RX 9060 XT on Mesa's radeonsi VA-API driver, Linux, ffmpeg 9.0.1) on public-domain NASA footage. [How it was measured](#how-it-was-measured) gives the input, the commands and the load for each.

## Install and first use

pcod, the command line over the library, comes with every [GitHub Release](https://github.com/gmrdad82/pito-codecs/releases/latest) for Linux x86-64, Windows x86-64 and macOS on Apple Silicon.

Linux x86-64:

```sh
v=v1.3.0; curl -fsSLO "https://github.com/gmrdad82/pito-codecs/releases/download/$v/pcod-$v-linux-x86_64.tar.gz"
tar -xzf "pcod-$v-linux-x86_64.tar.gz" pcod && install -Dm755 pcod ~/.local/bin/pcod
```

macOS on Apple Silicon:

```sh
v=v1.3.0; curl -fsSLO "https://github.com/gmrdad82/pito-codecs/releases/download/$v/pcod-$v-macos-arm64.tar.gz"
tar -xzf "pcod-$v-macos-arm64.tar.gz" pcod && sudo install -m755 pcod /usr/local/bin/pcod
```

Windows x86-64, in PowerShell:

```powershell
$v="v1.3.0"; Invoke-WebRequest "https://github.com/gmrdad82/pito-codecs/releases/download/$v/pcod-$v-windows-x86_64.zip" -OutFile pcod.zip
Expand-Archive pcod.zip "$env:LOCALAPPDATA\pcod" -Force; [Environment]::SetEnvironmentVariable("Path", [Environment]::GetEnvironmentVariable("Path", "User") + ";$env:LOCALAPPDATA\pcod", "User")
```

Then, on any MP4, or on a folder of PNG frames:

```sh
pcod probe clip.mp4
pcod decode clip.mp4 --route gpu
pcod encode clip.mp4 out.mp4 --codec hevc --route gpu
pcod frames frames/ trailer.mp4 --fps 60 --audio trailer.wav
```

`SHA256SUMS` in each Release lists every archive's checksum. To build pcod from source anywhere, Linux arm64 included, with Rust 1.97 or newer: `cargo install --locked --git https://github.com/gmrdad82/pito-codecs --tag v1.3.0 pito-codecs --bin pcod`.

In a Rust project, add the library with the parts you need (all are on by default):

```toml
[dependencies]
pito-codecs = { git = "https://github.com/gmrdad82/pito-codecs", tag = "v1.3.0", default-features = false, features = ["h264", "hevc", "vp9", "aac", "mp4", "mkv", "gpu", "filter", "opus", "ac3", "mp3"] }
```

and let it pick the fastest route for each job:

```rust
use pito_codecs::media::{Prefs, first_frame, probe};

let prefs = Prefs::default();
let info = probe("clip.mp4".as_ref(), &prefs)?;
let still = first_frame("clip.mp4".as_ref(), 240, &prefs)?;
println!("{}x{} at {}/{} fps, first frame {}x{}", info.width, info.height, info.fps.0, info.fps.1, still.width, still.height);
```

The GPU route needs nothing extra: libva is loaded at run time on Linux (Mesa's or Intel's driver), VideoToolbox is part of macOS and Media Foundation part of Windows. Without them everything decodes and encodes on the CPU. On Windows, H.265 decode through Media Foundation needs Microsoft's HEVC Video Extensions, and N editions need the Media Feature Pack.

## Features

### Instant first frame

Opening a file and showing its first picture takes 4.1 ms for 1440p H.264 on the CPU and 12.4 ms for HEVC on the GPU. That is 7.4× to 21.5× sooner than ffmpeg, whose time includes starting its process, which is what an app calling it pays.

![Time to first frame at 1440p, pito-codecs against ffmpeg](docs/images/chart-first-frame.png)

### Filmstrips

`frames_at` takes stills at any times, from the keyframe at or before each one, and reads only what it needs; in MKV and WebM it seeks through the file's Cues, so a still late in a long recording costs the same as one at the start. `frames_at_seek` with `Seek::Exact` decodes on to the exact frame. This strip of 12 stills was taken by `frames_at` and written by our own GIF encoder (`examples/readme_shots.rs`), in 63 to 272 ms for the stills across runs on a shared machine and 27 ms for the GIF:

![Twelve stills across a NASA clip, taken by frames_at](docs/images/filmstrip.gif)

![Filmstrip stills per second, pito-codecs against ffmpeg](docs/images/chart-filmstrip.png)

### Decode on the CPU and the GPU

H.264 (Baseline, Main and High), H.265 (Main and Main 10, tiles and WPP) and VP9 (profiles 0 to 3) decode bit-exact with ffmpeg on the CPU, with threads across rows and frames and SIMD picked at run time. On the GPU, VA-API on Linux and VideoToolbox on macOS decode the same streams bit-exact. A frame read back from VA-API is copied only once no later picture refers to it, so the copies don't hold up decoding.

![GPU decode through VA-API, pito-codecs against ffmpeg](docs/images/chart-decode-gpu.png)

On the CPU, ffmpeg's hand-written assembly still decodes H.264 1.3× to 1.5× faster; HEVC is level on one thread:

![CPU decode, pito-codecs against ffmpeg](docs/images/chart-decode-cpu.png)

### Encode and transcode on the GPU

H.264 and H.265 encode through VA-API, VideoToolbox and NVENC, and a transcode keeps every frame on the GPU from decode to encode. Our VA-API encodes run 1.1× to 1.3× faster than ffmpeg's at the same settings.

![GPU encode and transcode through VA-API, pito-codecs against ffmpeg](docs/images/chart-encode-gpu.png)

The CPU H.264 encoder (CABAC, B-frames, CRF, QP, CBR and VBR) has no hand-written SIMD, and libx264 is 1.5× to 3.4× faster. H.265 encodes on the GPU only, and without a GPU HEVC encoder the front falls back to H.264 and says why.

![CPU H.264 encode against libx264](docs/images/chart-encode-cpu.png)

### Smart-cut export

`export` and `export_with_audio` write an edit from pieces of one or more files. They copy the middle of each piece as it is and re-encode only the frames around each cut, so the result is frame-accurate and 1.7× to 2.9× faster than ffmpeg re-encoding every frame. ffmpeg's stream copy is faster still, but it encodes nothing and can only cut on keyframes.

![Smart-cut export in times real time](docs/images/chart-export.png)

VP9 sources cut the same way into MP4 (`vp09`): packets are copied as they are, superframes included, and only the frames before each piece's first keyframe are encoded, in parallel keyframe-led chunks. A 3-piece, frame-accurate cut of a 720p60 VP9 file ran at 1365 to 3285 frames a second with the machine at load 20, against about 500 for re-encoding it to H.264 on the GPU.

### Overlays composed on the card

An export can draw an RGBA overlay on every frame: a watermark, or karaoke captions that change at every word. On VA-API with Mesa's driver, the overlay is blended on the card by a VA-API video-processing pass straight into the encoder's input surface. Frames go from decode to encode without a copy to system memory, and the overlay is uploaded only when it changes, and only its painted area. The graphics queue is never used. Other GPU encoders draw the overlay on the CPU.

This frame's caption and corner mark were drawn as one RGBA layer and blended by `media::Blender` in 1.8 ms at 1280x720 (`examples/readme_shots.rs`):

![A NASA frame with a caption and a corner mark, blended by pito-codecs](docs/images/overlay.gif)

![Export with an overlay: composed on the card against decoded and blended on the CPU](docs/images/chart-gpu-export.png)

On the CPU, a caption change costs what its painted area costs: building the layer takes 0.5 to 0.9 ms at 1080p and 1.7 to 2.3 ms at 4K, 8 and 10-bit, bit-exact with the earlier full-frame pass that took 22.5 to 135 ms.

![Caption layer rebuild per change, now against 1.2.1](docs/images/chart-overlay.png)

### Audio

AAC-LC decode and encode, Opus decode and encode (CELT; SILK and hybrid speech encode are experimental), and AC-3, E-AC-3, MP3, MP2, MP1 and PCM decode, from MP4, MOV, MKV and WebM. `audio_pcm_range` returns any time range as PCM, sample-exact; `audio_stream` hands it over in one-second blocks; `audio_mix_stream` mixes several tracks with a gain each. Loudness (EBU R128) and a true-peak limiter come with it.

![Audio in times real time, pito-codecs against ffmpeg](docs/images/chart-audio.png)

- **AC-3 and E-AC-3** (ported from oxideav-ac3): 10 minutes of 5.1 at 448 kbps decode in 0.56 to 0.59 s on one core against ffmpeg's 0.55 to 0.87 s, and to stereo through `audio_pcm_range` in 0.28 to 0.33 s against ffmpeg's 0.68 to 1.01 s. Within 2e-5 of ffmpeg's float decoder.
- **MP3** (ported from minimp3): 10 minutes of stereo at 192 kbps decode in 0.32 s on one thread against ffmpeg's 0.30 to 0.38 s, and in 0.16 s from MKV through `audio_pcm_range` against ffmpeg's 0.31 s. Within 4.4e-6 of ffmpeg's mp3float.

### GIF and WebP

GIF (Oklab palettes, ordered or Floyd-Steinberg dither, frame differencing) and WebP (lossless and lossy, alpha, animation, bit-exact with libwebp on decode) read and write stills and animations. These 30 frames at 480x270 were encoded by our GIF encoder in 29 ms (1.9 MB, ordered dither, 128 colours); the same frames as lossy animated WebP take 42 ms and 406 KB ([animated.webp](docs/images/animated.webp)):

![An animated GIF of a NASA clip, encoded by pito-codecs](docs/images/animated.gif)

### Scale and filters

Scaling (bilinear, bicubic, Lanczos) runs 1.7× to 2.6× faster than ffmpeg's swscale on all threads. Colour (BT.601, 709 and 2020), rotate, flip, crop, pad, frame-rate change, overlays, resampling, downmix, gain, loudness and a limiter sit between decode and encode.

![Scaling in frames per second, pito-codecs against ffmpeg](docs/images/chart-scale.png)

## Formats

| Part | Decode | Encode |
| --- | --- | --- |
| H.264 | CPU: Baseline, Main and High, progressive 8-bit 4:2:0, CAVLC and CABAC, bit-exact with ffmpeg. GPU: VA-API and VideoToolbox for the same profiles, bit-exact; Media Foundation. | CPU: Baseline, Main and High with CABAC and B-frames; CRF, QP, CBR and VBR. GPU: VA-API High; VideoToolbox Main and High; NVENC. |
| H.265 | CPU: Main and Main 10, tiles and WPP, bit-exact with ffmpeg. GPU: VA-API and VideoToolbox, Main and Main 10, bit-exact; Media Foundation. | GPU only: VA-API Main; VideoToolbox Main and Main 10; NVENC. Without a GPU HEVC encoder it falls back to H.264 and says why. |
| VP9 | CPU: profiles 0 to 3 (8, 10 and 12-bit; 4:2:0, 4:2:2 and 4:4:4), tiles, bit-exact with ffmpeg and libvpx. GPU: VA-API for 8 and 10-bit 4:2:0, bit-exact. | CPU: profile 0, with motion search, trellis, rate control, AQ and tile threads; ffmpeg and libvpx decode it bit-exact to our reconstruction. |
| AAC | AAC-LC, mono to 7.1. | AAC-LC, CBR and VBR. |
| AC-3 and E-AC-3 | Mono to 5.1 at 32, 44.1 and 48 kHz with coupling and rematrixing, within 2e-5 of ffmpeg's float decoder. Spectral extension, AHT and 7.1 through E-AC-3 dependent substreams are built but untested, since ffmpeg's encoder makes none of them; enhanced coupling and the reduced sample rates are refused. | — |
| Opus | SILK, CELT, hybrid and multistream; the RFC 8251 conformance vectors decode to the reference's final range on every packet. | CELT, VBR and CBR, 6 to 510 kbps. **Experimental:** SILK and hybrid speech decode clean in libopus but sound worse than libopus at low rates. |
| MP3, MP2, MP1 | MPEG-1, 2 and 2.5 audio, layers I, II and III, mono and stereo, CBR and VBR, within about 1e-6 of ffmpeg's mp3float. | — |
| MP4 and MOV | Tracks, sample tables, rotation, edit lists, fragmented files. | MP4 or MOV, faststart or fragmented; remux of a time range without re-encoding. |
| MKV and WebM | Tracks, lacing, Cues, seeking. | H.264, HEVC, VP9, Opus and AAC; WebM's codec rules enforced; remux. |
| GIF | GIF87a and 89a, interlace, disposal, transparency, loops. | Oklab palettes, ordered or Floyd-Steinberg dither, frame differencing. |
| WebP | Lossless VP8L and lossy VP8, alpha, animation; bit-exact with libwebp. | Lossless VP8L and lossy VP8, alpha, animation. |
| PNG | Grey, grey and alpha, RGB, RGBA and palette with tRNS, 1 to 16-bit, Adam7, all five filters; to 8-bit RGBA, equal to ffmpeg's decode. | — |
| WAV | PCM 8, 16, 24 and 32-bit, float 32 and 64-bit, any rate and channel count. | — |

Not covered: DTS, FLAC, Vorbis, ALAC, AV1, ProRes, DNxHD, MPEG-2 and VP8 video, and subtitle tracks.

## GPUs and systems

The front tries the GPU first and our own CPU code second, for every decode and every encode, and every fallback says why in a plain reason an app can show. What each GPU route has been run on:

| Route | System | Decode | Encode | Tested on |
| --- | --- | --- | --- | --- |
| VA-API, AMD (Mesa radeonsi) | Linux | H.264, H.265, VP9 | H.264, H.265; overlays composed on the card | AMD Radeon RX 9060 XT (RDNA 4), before every release |
| VA-API, Intel (iHD) | Linux | H.264, H.265, VP9 | H.264, H.265 | Built, untested on Intel hardware |
| VideoToolbox | macOS | H.264, H.265 | H.264, H.265 | GitHub's Apple Silicon runners on every push: decode bit-exact with ffmpeg, encodes decoded back |
| NVENC | Linux, Windows | — | H.264, H.265 | Built and unit-tested, untested on NVIDIA hardware |
| Media Foundation | Windows | H.264, H.265 | — | Built and unit-tested, untested on GPU hardware |

The CPU codecs run and are tested on every push on Linux x86-64 and arm64, Windows x86-64 and macOS on Apple Silicon, with AVX2 kernels on x86-64 and NEON or portable paths on arm64.

## Small and simple

- **Two direct dependencies** for the library on Linux, rayon and libloading, 8 crates in all; clap only for pcod, behind the `cli` feature. No C or C++ is linked, and libva is loaded at run time, so a binary starts on a machine without it.
- **pcod is 5.7 MB** (v1.3.0, Linux x86-64, release build, stripped).
- **About 134,000 lines of Rust** under `src/`, one package with a feature per part.
- **Graceful on bad input:** every parser and decoder is fuzzed on CI, and malformed files return errors instead of panicking.

## Use the library

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
use pito_codecs::h264::{Encoder, EncoderConfig};
use pito_codecs::media::Rate;

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
use pito_codecs::media::{
    Prefs, Seek, audio_pcm_range, audio_stream, first_frame, frames_at, frames_at_seek, probe,
};

let prefs = Prefs::default();
let info = probe("clip.mp4".as_ref(), &prefs)?;
let still = first_frame("clip.mp4".as_ref(), 240, &prefs)?;
let strip = frames_at("recording.mkv".as_ref(), &[60.0, 5400.0], 120, &prefs)?;
let exact = frames_at_seek("recording.mkv".as_ref(), &[61.5], 240, Seek::Exact, &prefs)?;
let slice = audio_pcm_range("recording.mkv".as_ref(), 0, 30.0, 60.0, 48_000, 2, &prefs)?;
for block in audio_stream("recording.mkv".as_ref(), 0, 0.0, 16_000, 1, &prefs)? {
    let block = block?;
}
```

`frames_at` shows the keyframe at or before each time, which is what a filmstrip wants; `frames_at_seek` with `Seek::Exact` decodes on to the frame shown at that time, and decodes a GOP once for all the times inside it. A range past the end of the track returns only the samples it has, so a buffer shorter than asked means the track ended. `audio_stream` hands the audio over in one-second blocks as it decodes and ends at the track's end. In MKV and WebM, stills and audio seek through the file's Cues, so a still or a slice late in a long recording reads only the clusters around it.

Export an edit with audio you mixed yourself: the callback gets each piece's index and its exact sample count and returns that many 48 kHz stereo samples, which our AAC encoder turns into one track as long as the video to the sample:

```rust
let done = export_with_audio("edit.mp4".as_ref(), &pieces, ExportOptions::default(), |piece, samples| mixer.render(piece, samples), |_| true)?;
```

Export frames you made yourself, at your own size and rate: the frame callback gets each frame's index and the last frame back to reuse and returns `None` to end, and the audio callback gets the frames so far and the samples that cover them:

```rust
let canvas = Canvas { width: 1080, height: 1920, fps: (60, 1), frames: Some(600) };
let done = export_frames("vertical.mp4".as_ref(), canvas, ExportOptions::default(), |n, spent| comp.render(n, spent), |frames, samples| mixer.render(frames, samples), |_| true)?;
```

Draw one RGBA overlay on many frames without copying it each time: a `Blender` keeps the overlay it was given, and an `Arc` of the same image draws straight away.

```rust
let mut blender = Blender::new();
blender.draw(&mut frame, &watermark);
```

`pcod` (`src/bin/pcod.rs`) is a small command line over the library, and it is what the GIF at the top shows: `pcod decode clip.mp4 --route gpu`, or from a checkout `cargo run --release --bin pcod -- decode clip.mp4 --route gpu`. `pcod frames <dir> <out.mp4> --fps 60 --audio sound.wav` encodes a folder of PNG frames in name order and a WAV into an H.264 (or HEVC) and AAC MP4, BT.709 and faststart, decoding the PNGs on every core ahead of the encoder. It is behind the `cli` feature, on by default, so a library user with `default-features = false` never builds clap.


## How it was measured

**Against ffmpeg** (the overview and the first-frame, filmstrip, decode, encode, export, scale and audio charts): one run of `examples/bench.rs` on 2026-10-11 at commit `415391d`, each job back to back with the system ffmpeg on the same file, on an AMD Ryzen 9 9950X (16 cores) and an AMD Radeon RX 9060 XT on Mesa 26.2 radeonsi through VA-API, Linux, ffmpeg 9.0.1. The input is 20 s of NASA's public-domain "A Message of Resilience from the International Space Station" (720p60), scaled to 1080p and 1440p and encoded with libx264 at CRF 20 and libx265 at CRF 23; the audio is NASA's public-domain solstice interview. The command is `pgpu run -- cargo run --release --example bench -- --runs 1`, which writes `tmp/bench/results.json`. The machine was shared with other work (load average 4.5 at the start, up to 18 during the run), so the absolute numbers are floors; 17 rows another GPU job disturbed were measured again right after with `--only` into the same file.

**Today's additions** (the overlay charts, the VP9 cut, AC-3 and MP3) were each measured once, back to back against the baseline named, on the same machine while it was busy with other work, at the load given beside each; their absolute numbers are floors, and the ratios are same-run comparisons.

- Overlay layer rebuild: `cargo run --release --example media_speed -- captions`, `filter::Overlay::new` on a caption band about 10% painted, against v1.2.1's implementation, load 42 to 61.
- Export with an overlay: `media_speed <file> xport`, a 130 s 1920x804 and a 30 s 1080p H.264 export with a corner mark or captions, GPU-resident against CPU decode and blend with GPU encode, load about 90.
- VP9 smart cut: `media_speed <file> spans vp9 …`, a 3600-frame 720p60 VP9 file at 3 Mb/s, 3 pieces, 2967 frames, keyframes every 120 or 240 frames, load about 20.
- AC-3: 10 minutes of 5.1 at 448 kbps against `ffmpeg -threads 1 -benchmark` and `ffmpeg -ac 2`, load 20 to 50. MP3: 10 minutes of stereo at 192 kbps against ffmpeg's mp3float, load 33.

The charts are drawn by [pito-compare](https://github.com/gmrdad82/pito-compare) from the data files in [docs/charts](docs/charts): `pcmp docs/charts/<name>.json`.

<details>
<summary>Every row of the 2026-10-11 run</summary>

| Job | pito-codecs | ffmpeg | pito-codecs is |
| --- | --- | --- | --- |
| H.264 1080p decode, CPU, 1 thread | 376 fps | 558 fps | 1.5× slower |
| H.264 1080p decode, CPU, all threads | 1959 fps | 2551 fps | 1.3× slower |
| HEVC 1080p decode, CPU, 1 thread | 426 fps | 418 fps | about the same |
| HEVC 1080p decode, CPU, all threads | 1479 fps | 1687 fps | 1.1× slower |
| H.264 1440p decode, CPU, 1 thread | 195 fps | 286 fps | 1.5× slower |
| H.264 1440p decode, CPU, all threads | 1089 fps | 1495 fps | 1.4× slower |
| HEVC 1440p decode, CPU, 1 thread | 255 fps | 247 fps | about the same |
| HEVC 1440p decode, CPU, all threads | 789 fps | 1231 fps | 1.6× slower |
| H.264 1080p decode, VA-API, frames stay on the GPU | 1058 fps | 1023 fps | about the same |
| H.264 1080p decode, VA-API, frames read back | 814 fps | 694 fps | 1.2× faster |
| HEVC 1080p decode, VA-API, frames stay on the GPU | 1115 fps | 1068 fps | about the same |
| HEVC 1080p decode, VA-API, frames read back | 795 fps | 677 fps | 1.2× faster |
| H.264 1440p decode, VA-API, frames stay on the GPU | 1053 fps | 1011 fps | about the same |
| H.264 1440p decode, VA-API, frames read back | 507 fps | 465 fps | 1.1× faster |
| HEVC 1440p decode, VA-API, frames stay on the GPU | 1100 fps | 1049 fps | about the same |
| HEVC 1440p decode, VA-API, frames read back | 471 fps | 428 fps | 1.1× faster |
| H.264 1080p encode, CPU, CRF 23, against libx264 medium * | 191 fps | 328 fps | 1.7× slower |
| H.264 1080p encode, CPU, CRF 23, against libx264 veryfast * | 184 fps | 626 fps | 3.4× slower |
| H.264 1080p encode, VA-API, QP 23 | 645 fps | 499 fps | 1.3× faster |
| HEVC 1080p encode, VA-API, QP 23 | 839 fps | 686 fps | 1.2× faster |
| H.264 1440p encode, CPU, CRF 23, against libx264 medium * | 121 fps | 181 fps | 1.5× slower |
| H.264 1440p encode, CPU, CRF 23, against libx264 veryfast * | 121 fps | 396 fps | 3.3× slower |
| H.264 1440p encode, VA-API, QP 23 | 412 fps | 371 fps | 1.1× faster |
| HEVC 1440p encode, VA-API, QP 23 | 545 fps | 476 fps | 1.1× faster |
| H.264 1440p GPU transcode, frames stay on the GPU | 309 fps | 300 fps | about the same |
| HEVC 1440p GPU transcode, frames stay on the GPU | 379 fps | 371 fps | about the same |
| H.264 1440p time to first frame, CPU (ffmpeg includes starting its process) | 4.1 ms | 87.4 ms | 21.5× faster |
| H.264 1440p time to first frame, GPU (ffmpeg includes starting its process) | 11.6 ms | 85.7 ms | 7.4× faster |
| HEVC 1440p time to first frame, CPU (ffmpeg includes starting its process) | 8.1 ms | 106.3 ms | 13.1× faster |
| HEVC 1440p time to first frame, GPU (ffmpeg includes starting its process) | 12.4 ms | 100.7 ms | 8.1× faster |
| H.264 1080p filmstrip, 12 stills at 240 px, CPU | 414 stills/s | 46 stills/s | 8.9× faster |
| H.264 1080p filmstrip, 12 stills at 240 px, GPU first | 407 stills/s | 47 stills/s | 8.7× faster |
| H.264 1440p filmstrip, 12 stills at 240 px, CPU | 268 stills/s | 29 stills/s | 9.4× faster |
| H.264 1440p filmstrip, 12 stills at 240 px, GPU first | 263 stills/s | 33 stills/s | 8.0× faster |
| HEVC 1080p filmstrip, 12 stills at 240 px, CPU | 364 stills/s | 44 stills/s | 8.2× faster |
| HEVC 1080p filmstrip, 12 stills at 240 px, GPU first | 359 stills/s | 32 stills/s | 11.4× faster |
| HEVC 1440p filmstrip, 12 stills at 240 px, CPU | 257 stills/s | 21 stills/s | 12.4× faster |
| HEVC 1440p filmstrip, 12 stills at 240 px, GPU first | 167 stills/s | 22 stills/s | 7.6× faster |
| Smart-cut export 1080p, 3 cuts, against ffmpeg re-encoding every frame (both frame-accurate) † | 11.9× real time | 7.2× real time | 1.7× faster |
| Smart-cut export 1080p, 3 cuts, against ffmpeg's stream copy (keyframe cuts only, not frame-accurate) † | 11.9× real time | 69.4× real time | 5.8× slower |
| Smart-cut export 1440p, 3 cuts, against ffmpeg re-encoding every frame (both frame-accurate) † | 8.7× real time | 3.0× real time | 2.9× faster |
| Smart-cut export 1440p, 3 cuts, against ffmpeg's stream copy (keyframe cuts only, not frame-accurate) † | 8.7× real time | 52.7× real time | 6.0× slower |
| Scale 1440p to 1080p, bicubic | 2934 fps | 1430 fps | 2.1× faster |
| Scale 1440p to 720p, bicubic | 2537 fps | 1221 fps | 2.1× faster |
| Scale 1080p to 720p, bicubic | 4351 fps | 1695 fps | 2.6× faster |
| Scale 1080p to 1440p, bicubic | 3459 fps | 2093 fps | 1.7× faster |
| AAC decode, 48 kHz stereo | 1003× real time | 1861× real time | 1.9× slower |
| AAC encode, 128 kbps stereo | 228× real time | 130× real time | 1.8× faster |
| EBU R128 loudness, 48 kHz stereo | 3200× real time | 559× real time | 5.7× faster |

\* libx264 is hand-written assembly; our CPU H.264 encoder has no hand-written SIMD. † Our export copies the middle of each piece and re-encodes only around each cut, so it is frame-accurate; a stream copy encodes nothing and cuts only on keyframes.

</details>

## Part of PITO MD

pito-codecs is the media layer of [PITO MD](https://pitomd.com): Splicer, PITO MD's desk for cutting talking-head videos from their transcripts, opens, plays, cuts and exports through it. Its open siblings:

- [prec](https://github.com/gmrdad82/prec) records the terminal clip at the top of this page, and [pconv](https://github.com/gmrdad82/pconv) turns it into the GIF.
- [pito-compare](https://github.com/gmrdad82/pito-compare) draws every chart on this page.
- [pgpu](https://github.com/gmrdad82/pgpu) shares one GPU between the benchmarks and the other jobs on the test machine.
- [pfx](https://github.com/gmrdad82/pfx) and [Manfred](https://github.com/gmrdad82/manfred) render and build scenes; [pito-tui](https://github.com/gmrdad82/pito-tui) is the shell of PITO MD's terminal apps.

## Build and test

```
cargo build --release
cargo test --release
```

The tests use the system ffmpeg as an oracle when it is installed and skip those checks when it is not, and the GPU tests skip cleanly without a device. `bin/gate` runs rustfmt, clippy with warnings as errors and the tests, then a release build (`--fast` skips the build). `bin/docs-bundle vX.Y.Z [out dir]` builds `docs-vX.Y.Z.tar.gz`, the documentation each Release carries: the guides in `docs/site`, a command reference made from pcod's own help, the API reference from `cargo doc` and the changelog.

[CI](.github/workflows/ci.yml) runs on every push and pull request. One job checks formatting and runs clippy for Linux x64 and arm64, Windows and macOS. Then four runners test in parallel: Linux x64 and Linux arm64 (Debian with its ffmpeg), Windows x64 and macOS on Apple Silicon, each with every test under nextest in the `ci` profile (the release optimisation without LTO, so the test binaries link quickly), the doc tests and a full release build. On a `v` tag it also checks that the tag matches the crate's version. What that covers:

- **CPU codecs, containers and filters** on x86-64 with the AVX2 kernels, and on arm64 with the portable paths and the filters' NEON kernels, on Linux, Windows and macOS.
- **VideoToolbox** on macOS: H.264 and H.265 decode bit-exact with ffmpeg, H.264 and H.265 encode decoded back by ffmpeg, bitrate modes, and a transcode that scales on the GPU.
- **Media Foundation and NVENC** only build and run their unit tests, since the runners have no GPU.
- **VA-API** runs on the maintainer's AMD machine before each release, with the benchmarks.

## Contributing

Issues are welcome at [github.com/gmrdad82/pito-codecs/issues](https://github.com/gmrdad82/pito-codecs/issues). Useful ones include a file that decodes wrong or slowly (with a small sample, where you can share one), a route that falls back when it shouldn't, or a format you need. Pull requests aren't taken. Please read the [code of conduct](CODE_OF_CONDUCT.md) first, and report security issues privately, as [SECURITY.md](SECURITY.md) describes.

## Credits

Parts are adapted from [FilmCraft](https://github.com/storytold/filmcraft) (MIT OR Apache-2.0, taken under MIT) and from libwebp (BSD-3-Clause), oxideav-ac3 (MIT) and minimp3 (CC0). The VA-API and NVENC declarations follow the MIT-licensed libva and NVIDIA headers. Permissive projects such as libvpx and libopus (BSD-3-Clause) may be ported with their notices; the GPL and LGPL ones (FFmpeg, x264, x265, libde265) are never copied, and ffmpeg serves only as the test oracle and the benchmark reference. The ITU-T H.264 and H.265 Recommendations, ATSC A/52, the VP9 specification, RFC 6716, RFC 6386, the GIF89a specification and the ISO MP4 and AAC standards are the sources. Origins are listed in [INSPIRATION.md](INSPIRATION.md) and their notices in [NOTICE](NOTICE). The demo and benchmark clips are public-domain NASA footage from PITO MD's fixture sets.

## Licence

<img src="assets/marks/pitomd-wordmark.svg" width="320" alt="PITO MD">

The code is MIT licensed ([LICENSE](LICENSE)), © Catalin Ilinca. The PITO MD name and its logos are © Catalin Ilinca, all rights reserved, and are not covered by the MIT licence ([TRADEMARKS.md](TRADEMARKS.md)).
