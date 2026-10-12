# Changelog

## v1.2.0 — 2026-10-12

Exports from the app's own frames at any size, a public blend without copies, export progress that follows real work, GPU stills about 3x faster, and the audit's second hardening wave.

### Added

- `export_frames` writes caller-made frames at their own size and frame rate to MP4, with the same GPU-first H.264 routing as `export` and premixed AAC sample-exact against the video.
- `Blender` is public: kept across frames with the same `Arc<Rgba>` overlay, it blends without copying it.

### Changed

- Export progress counts copied frames as they're written and encoded frames as they're encoded, then the final file pass, so it rises steadily to 1.0.
- GPU keyframe stills read the decoded VA-API picture directly instead of through the streaming staging path: time to first frame on the GPU about 3x faster (10.5 to 3.6 ms at 1440p in a same-run comparison).
- HEVC pictures above level 6.2's luma size (8192x4352) are refused, and VA-API builds only the tile boundaries it sends.

### Fixed

- MKV resync never rescans bytes it has scanned, so a damaged file can't make a seek take quadratic time; warnings are capped.
- MP4 sample tables are bounded by their chunk table and the file's bytes across tracks.
- `packet` on the MP4 and MKV readers looks samples up without indexing; H.264 slice-group maps are capped by the picture size; EBML floats of an odd size are ignored.

## v1.1.1 — 2026-10-12

Hardening for files from anywhere: malformed headers are refused instead of crashing or allocating without bound, and every parser is fuzzed on CI.

### Added

- `media::Pcm`, the PCM sample converter the readers use.
- Fuzzing on GitHub CI for the MP4 and MKV readers, probe, the H.264, HEVC and VP9 decoders, AAC, Opus and PCM: a short run on every push and a longer one nightly.

### Fixed

- H.264 crop offsets are checked without wrapping, so a picture is never sliced outside its planes; the HEVC conformance window always stays inside the picture.
- An indexed MKV open keeps at most 2^26 samples per track and no more samples than half the file's bytes; laced frames need at least one byte each.
- Fragmented MP4 runs of empty samples are bounded.

## v1.1.0 — 2026-10-12

One home for each shared public type. Every old path still compiles, marked deprecated.

### Added

- `media::length_prefixed_to_annexb`, `media::threads` and `mp4::AvcConfig::parameter_sets_annexb`, which `pcod` now uses instead of its own copies.

### Changed

- `Rgba`, `Rate` and `Bitstream` each live once in `media`. `filter::Rgba`, `webp::Rgba`, `h264::Rate`, `gpu::Rate`, `h264::Bitstream` and `gpu::Bitstream` are deprecated aliases of them, so existing code keeps compiling and values are interchangeable.
- `media::Rate` and `media::Bitstream` derive `Hash`, and `media::Rgba` has `stride()`.

## v1.0.5 — 2026-10-12

Recordings of 100 GB and more: memory that doesn't grow with the file, fast probes with or without Cues, and exports that use about a quarter of the memory.

### Added

- `audio_mix_stream` streams the mix of every audio track, with the same rules as `audio_mix`.

### Changed

- `probe` of an MKV without Cues reads its head and tail: milliseconds instead of a pass over the whole file. `Probe.frames` is `None` there too; `frame_count` gives the count.
- `keyframes` and `frame_count` read block headers in one sequential pass with bounded memory, and seeks in a Cue-less MKV bisect its clusters.
- The full MKV index the media front builds holds video blocks only, about half the memory.
- Exports write each re-encoded edge as it finishes, and a track mix streams in one-second steps, so a long export's peak memory stays near 0.2 GB.
- Audio reads use large sequential windows, at disk speed.

### Fixed

- An exported MP4's mvhd, tkhd, mdhd and edit-list durations reach the last frame's end; they could be two frames short.
- Every feature builds on its own.

## v1.0.4 — 2026-10-12

Exports of MKV recordings smart-cut, every join is exact, and probing a large MKV reads only its head.

### Added

- `frame_count` gives a file's exact frame count on demand.
- `Exported.reasons` says why a piece or a whole export was re-encoded instead of copied.

### Changed

- MKV sources smart-cut into MP4: the stream between cuts is copied, and only the frames around each cut are re-encoded.
- `probe` of an MKV with Cues reads its head, Cues, first blocks and last cluster instead of every block: a few milliseconds whatever the file size. `Probe.frames` is `None` there; `frame_count` gives the exact count.
- VA-API H.264 and HEVC refuse pictures below 352x240, which then run on the CPU: smaller ones faulted AMD's video engine.

### Fixed

- The ffmpeg route's join gives every part its exact duration, so a long edit can't drift; copied MKV parts start on the right frame, tiny parts open, and the audio mix trims to exact sample counts.
- A track mix ends on the video's last sample instead of up to one AAC frame past it, and reads MKV audio without ffmpeg.

## v1.0.3 — 2026-10-12

Long MKV recordings open, play audio and give stills without reading the whole file.

### Added

- `audio_stream` streams a track's audio in sample-aligned blocks from any start time, with the same resample and downmix as `audio_pcm_range`, from MP4 and MKV (PCM, AAC, Opus) or through ffmpeg.
- `frames_at_seek` with `Seek::Exact` gives the frame at the asked time, decoding forward from its keyframe; `Seek::Keyframe` keeps the fast keyframe still.
- `export_with_audio` takes the app's own premixed audio, one buffer per piece, and writes it as AAC, sample-exact against the video.
- `first_frame`, `frames_at` and filmstrips read MKV and WebM, seeking through their Cues.

### Changed

- Audio and stills open an MKV from its head (SeekHead, Tracks, Cues) instead of indexing every block: about 0.1 ms instead of hundreds of milliseconds to seconds on long recordings.

### Fixed

- A range that runs past the end of a track returns only the samples the track has, so a buffer shorter than asked means the track ended; before, it was padded with silence.

## v1.0.2 — 2026-10-12

Native audio from every container we read, and a simpler crate: about 7,200 lines fewer, with every output byte-identical to v1.0.1.

### Added

- MKV, WebM, MP4 and MOV audio in PCM (s16, s24, s32, f32, f64), AAC and Opus decodes natively through `audio_pcm`, `audio_pcm_range` and `audio_mix`.
- `AudioTrack::native`, `Probe::video_native` and `Probe::unsupported` say which streams a file holds that pito-codecs doesn't decode, by codec name.
- A Formats table in the README, with the formats it doesn't read.

### Changed

- The GPU routes parse H.264 and HEVC with the CPU decoders' own parameter sets, slices and DPB, including their overflow checks.
- The H.264, VP9, Opus and AAC encoders use their decoders' spec tables and helpers instead of copies; one CABAC table set serves H.264 and HEVC.
- One AVX2 check, one reader cache, one smart-cut plan, one route picker and one remux planner across the crate.

### Fixed

- AAC and Opus in MKV line up with the source to the sample (MKV's millisecond block times had put them up to 24 frames off).

## v1.0.1 — 2026-10-12

H.264, HEVC and VP9 decode speed-ups and MKV and WebM playback, with one benchmark run on this code behind every chart and number in the README.

### Added

- MKV and WebM play through `decode_video` and `decode_video_gpu`, on VA-API or the CPU.

### Changed

- VP9 decode: 8-bit streams decode into 8-bit planes, and motion vector candidates read their neighbours in place.
- VP9 decode: AVX2 loop filter specialised by width and direction, with its decisions per cell pair shared by U and V.
- VP9 VA-API decode starts each shown frame's read-back copy early.
- H.264 decode: inter prediction compiled per ISA, with one row wait per block.
- H.264 decode: spatial direct reads colocated motion only when it matters.
- HEVC: 10-bit output rows are copied as bytes on little-endian targets.
- bench: `--only` takes a full row id.
- README: every Speed chart comes before the table, and the README, CHANGELOG and SECURITY.md say what the code does today.

### Fixed

- VP9: a failing tile or loop-filter worker becomes an error, never a panic.
- VP9 lints clean on arm64 targets.

## v1.0.0 — 2026-10-11

Every format in the README's Status decodes and encodes, on Linux, Windows and macOS. Opus speech encoding (SILK and hybrid) is experimental.

### Added

- `pcod` is a real binary (behind the `cli` feature), released for Linux x86-64, Windows x86-64 and macOS on Apple Silicon, with SHA256SUMS, LICENSE, README and NOTICE in each archive.
- The Opus SILK encoder, experimental: it decodes clean in libopus but doesn't sound as good as libopus.
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
