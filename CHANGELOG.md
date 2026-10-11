# Changelog

## v1.0.0 — 2026-10-11

Every planned format decodes and encodes, on Linux, Windows and macOS. Opus speech encoding (SILK and hybrid) is experimental.

### Added

- `pcod` is a real binary (behind the `cli` feature), released for Linux x86-64, Windows x86-64 and macOS on Apple Silicon, with SHA256SUMS, LICENSE, README and NOTICE in each archive.
- The Opus SILK encoder, experimental: it decodes clean in libopus but doesn't yet sound as good as libopus.
- An "Against ffmpeg" summary in the README, with every win and loss from one benchmark run.
- GPU playback through the media front: `decode_video_gpu` keeps VA-API frames on the GPU, scaled there, exported as DRM PRIME, with CPU frames when the route falls back.
- Ranged audio and a track mix in the media front: `audio_pcm_range` and `audio_mix`.
- VA-API VP9 decode, bit-exact against ffmpeg.
- WebP lossy (VP8) encode, and WebP lossless, alpha and animation encode.
- The Opus encoder's CELT pitch prefilter, tf analysis and spread decision.
- Filters: BGRA and BGRX, interpolated chroma, chroma siting, NEON kernels.
- CI on Linux x64 and arm64, Windows and macOS, with VideoToolbox running on the macOS runner, and a check that a tag matches the crate's version.

### Changed

- H.264 decode: AVX2 deblocking, motion compensation and weighted and bi-prediction into the planes.
- HEVC decode: AVX2 dequantisation, batched luma deblocking, CTB rows published in two parts and row-aligned slices decoded in parallel.
- H.264 encode: mbtree lookahead, graded scene cuts and calibrated CRF.
- Errors name the file they failed on, and every example and pcod print one line and exit 1 instead of panicking.
- Damaged MP4, MKV, AAC, Opus, Ogg, GIF and WebP input returns an error instead of panicking.
- H.264 encode: one slice per frame on frame threads with row-published references.
- VA-API encode: rate control starts with a full one-second buffer, and per-frame quality matches ffmpeg's VA-API encoders.
- VA-API read-back decode copies into host memory ahead of time, faster than ffmpeg at 1080p and 1440p.
- VP9 decode: AVX2 motion compensation, loop filter and inverse transforms, faster entropy decoding.
- The README's routes are the GPU, then our CPU code, with their fallbacks; INSPIRATION.md says why; the charts are drawn per unit with paired bars and a centred ratio chart.

### Fixed

- VideoToolbox outputs pictures in display order with the right timestamps.
- The GPU playback test expects GPU surfaces only on VA-API.
- WebP VP8 token counts.

## v0.1.2 — 2026-10-11

### Fixed

- The crate's version is 0.1.2, matching its tag, so `pcod --version` reports it. Otherwise the same as v0.1.1.

## v0.1.1 — 2026-10-11

### Changed

- The command line is `pcod`, and the README GIF shows it.
- Opus encode: VBR by default, 5 s chunks encoded in parallel and byte-identical to streaming, an Ogg writer.
- H.264 decode: macroblock state in a four-row ring, one colocated row lookup per macroblock.
- The bench has a `--quick` mode.

### Fixed

- Tests write shared media under per-process names, so parallel test processes don't race.
- Media Foundation and VideoToolbox refuse VP9 and Opus instead of failing to build.

## v0.1.0 — 2026-10-11

### Added

- The first release: H.264 and H.265 decode and encode on the CPU and through VA-API (H.265 encode on the GPU only), VP9 decode and a first VP9 encoder, AAC-LC, Opus decode and CELT encode, MP4/MOV and MKV/WebM, GIF, WebP decode, filters and a media front for apps.
