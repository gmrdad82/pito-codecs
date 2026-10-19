# Changelog

## v1.16.0 — 2026-10-19

### Added

- `h264::Encoder::memory(width, height, threads)` estimates the CPU H.264 encoder's peak memory for a picture size.

### Changed

- The CPU H.264 encoder keeps at most 1 GiB of pictures in flight, so very large frames no longer take one frame per core: a 6344x3968 encode runs 6 frame threads, and 1080p and 4K encode as fast as before.
- `pcod frames` sizes its PNG decoders to 1 GiB (three at 6K), and refuses in one line, before encoding, when its memory estimate is over the process's address-space limit. 586 frames of 6K PNGs at CRF 14 on the CPU now peak at 3.75 GB instead of over 8 GB, flat in the frame count, at the same speed.
- Checks and releases run on the maintainer's machines; the repository runs no GitHub Actions.

### Fixed

- The GIF encoder no longer overflows an integer on a frame where nothing changed, in builds with overflow checks.

## v1.15.1 — 2026-10-19

### Fixed

- `--loudness` with `--true-peak` now holds the ceiling in the encoded AAC of a dense mix with sharp transients: the trial encode lowers the limiter up to eight times instead of twice (a film score came out at -0.68 dBTP for a -1 ceiling). pcod refuses to write an output whose encoded audio still peaks over the ceiling, and `pcod frames` prints the trial encode's loudness.

## v1.15.0 — 2026-10-18

### Added

- `pcod loudness <file>` measures an audio track per EBU R128 and ITU-R BS.1770-4: integrated loudness, loudness range, true peak, sample peak, and the short-term and momentary maxima, with `--json`. It reads the track at its own rate and channels and agrees with ffmpeg's ebur128 to 0.1.
- `--linear` keeps the dynamics when levelling: the gain to the target, then the true-peak limiter on the peaks alone, without raising the gain to make up what the limiter takes (the result can land under the target).
- `pcod frames` takes `--loudness`, `--true-peak`, `--linear` and `--audio-bitrate`. The ceiling holds for the AAC in the MP4, not just for the WAV: pcod encodes a trial, lowers the limiter until the decoded AAC stays under the ceiling, and prints the MP4's loudness.
- WAV, bare MP3 and audio-only M4A, MKA and WebM files are read as inputs, so `pcod in.wav out.wav --loudness -14` works.
- Library: `meter_loudness`, `level_pcm`, `level_with`, `plan_level_styled` and `LevelStyle`.

### Changed

- A re-encoded conversion with `--loudness` checks the true peak after a trial AAC or Opus encode, so the encoded audio stays under the ceiling.
- The README's install lines fetch the latest release.

## v1.14.3 — 2026-10-17

### Fixed

- VP9 stills and frames from an MP4 whose colour tags sit only in its vpcC box now use the file's colour matrix and range on the graphics card and the processor, so they no longer come out with shifted colours; tagged VP9 in MKV and WebM now follows its colour tags on the processor too.

## v1.14.2 — 2026-10-17

### Fixed

- AV1 stills decoded on the graphics card now use the colour matrix the stream declares, so they match the processor's stills exactly (a BT.601 file at 1080p was converted as BT.709 before). Keyframe stills on the card use the stream's colours for every codec.
- The first page of AV1 exact stills is fast again: it starts on one graphics-card decoder and the processor while the other card decoders open in the background (first page about 136 ms, was 181-203 ms); later pages keep the four-decoder speed.

## v1.14.1 — 2026-10-16

### Changed

- Exact stills of an AV1 video decode on the GPU in every worker, where only the first one did before: six pages of four exact stills from a 1080p AV1 file take 2.4 s instead of 3.2 s on an AMD card, with the same pictures.
- Exact stills and export pre-roll skip AV1 frames that no other frame references, as they already did for H.264 and HEVC.

## v1.14.0 — 2026-10-16

### Added

- An `audio` feature: `default-features = false, features = ["audio", "mp3", "opus"]` builds probing and audio decoding (`audio_pcm_range`, `audio_stream`) without any video decoder, GPU code or filters.
- `mp3::decode_file` decodes a bare .mp3 file gapless: ID3 and APE tags are skipped and the encoder delay and padding from its LAME or Xing header are trimmed, matching ffmpeg sample for sample. `mp3::vbr_tag` reads that header.

### Changed

- A decode set to the CPU, and every audio decode, no longer opens a GPU device to look for decoders.

## v1.13.0 — 2026-10-16

### Added

- `pcod <video> <out.wav>` writes a video's audio (AAC, Opus, MP3, AC-3, E-AC-3 or PCM) as a 16-bit PCM WAV: `--from` and `--to` cut a range (seconds or m:ss), `--mono` writes one channel, `--rate 44100|48000` resamples, and `--loudness` levels the range like the other outputs.
- The library can measure and level a range of a track at any rate and channel count (`AudioSpan`, `decode_audio_span`, `plan_level_span`).

## v1.12.1 — 2026-10-16

### Fixed

- MP4 files with B-frames no longer show one frame twice in players that read them at a constant rate: every sample keeps its own duration, and the edit list carries the start shift, as ffmpeg's own MP4 writer does.

## v1.12.0 — 2026-10-16

### Added

- `pcod <input> <output>` converts a video, or a folder of PNG frames, by the output's extension: MP4, MOV and MKV get H.264 and AAC, WebM gets VP9 and Opus with alpha kept, GIF and WebP loop and keep alpha. It takes pconv's options (`--profile fast|balanced|best`, `--size`, `--fps`, `--crf`, `--qp`, `--tune`, `--loudness`, `--true-peak`, `--audio-bitrate`, `--force`, `--json`, `--no-progress`) with no ffmpeg anywhere, converts BT.601, full-range and untagged sources to tagged BT.709, applies rotation and pixel aspect, and resizes in linear light.
- `pcod join` joins clips by stream copy when they match and by a letterboxed re-encode when they don't, saying which and why.
- `--loudness LUFS|youtube` levels the audio in two EBU R128 passes and copies the video untouched; the loudness before and after is printed.
- `pcod probe --json`, `pcod profiles`, a `--json` manifest with each file's sha256, and a one-line progress bar on a terminal.
- Against pconv 1.0.1 on a 30 s 1080p clip: GIF 4.9 s against 17.4 s at the same size, WebP 6.3 s against 8.2 s, WebM 19.9 s against 22.2 s and 13% smaller, the GPU profile 2.3 s against 2.8 s. The CPU H.264 profiles take 1.3 to 1.4 times as long as x264 and make larger files at higher quality.

## v1.11.0 — 2026-10-16

### Added

- AV1 decode. AV1 in MP4, MKV and WebM probes, shows its first frame, takes stills and decodes on the graphics card through VA-API (8 and 10-bit, film grain), and on the processor everywhere else with a decoder ported from rav1d; every frame of the test streams matches dav1d bit for bit. A 1080p first frame takes 26 to 71 ms on the card and 38 to 46 ms on the processor, against 89 to 119 ms through ffmpeg with dav1d. VideoToolbox (Apple M3 and later) and Media Foundation decode AV1 too, built but untested on that hardware.
- VP9 with alpha in WebM, written and read the way libvpx does it, so transparent web video made here plays in browsers and ffmpeg, and transparent WebM from elsewhere opens with its alpha: `export_rgba` and `decode_alpha`.
- `media::join` joins clips in order: a stream copy when every clip matches the first, otherwise a re-encode to the first clip's size and rate that letterboxes other shapes and gives silent clips silence.
- `media::level` levels a file's audio to a loudness target (EBU R128, two passes, a true-peak limiter when gain alone would clip), copying the video untouched; `plan_level` and `Leveler` do the same inside an export.
- Linear-light Lanczos resize with premultiplied alpha (`filter::Light::Linear`), so downscaled pictures keep their brightness and transparent edges get no dark fringes.

### Changed

- GIFs store only the part of each frame that changed beyond a small noise tolerance and redraw the whole frame on a slow fade or pan; Sierra-2-4A dithering. Animated WebP stores lossy frames as changed sub-rectangles too, up to a fifth smaller.
- The AAC encoder shapes its noise with TNS only where it helps, so levelled audio no longer overshoots its true-peak ceiling.

## v1.10.0 — 2026-10-16

### Added

- `pcod stills` takes N evenly spaced stills of a video, or stills at given times, at a height, and writes them as PNGs and/or one contact sheet (rows by columns, with padding); `--json` lists each still's time, size and path. A 24-still sheet of a 10-minute 1080p file takes about 0.6 s, where ffmpeg's tile filter takes about 6 s.
- `png::encode` writes an RGB PNG, with a fast deflate ported from fdeflate.

## v1.9.0 — 2026-10-15

### Added

- `Stills::keep(n)` caps how many decoders a stills handle runs and keeps, and `Stills::trim()` drops the kept ones, so an app can trade a little speed for much less memory: keeping 4 decoders halves the peak memory of a 1080p file's stills, and 27 stills then take 22 ms instead of 11 ms.

## v1.8.0 — 2026-10-15

### Changed

- Smart cuts are faster when two cut points fall in one GOP, as they do when silences are cut out: both share one decode from the keyframe. On the GPU, 40 cuts of a 1080p H.264 file take 4.3 s instead of 6.2 s, and an edit of 46 pieces with the gaps between them removed 6.4 s instead of 9.6 s, every frame unchanged.

## v1.7.0 — 2026-10-15

### Added

- `Stills`: open a file once with `Stills::open(path, height, prefs)`, then ask for stills again and again with `frames(times, seek, each)`; it keeps its decoders between calls and hands each still over as soon as it is decoded. A 24-still filmstrip of a 1080p file takes 11 ms instead of 23 ms through separate calls (17 ms instead of 30 ms from MKV), with every still identical.

### Changed

- Exact stills skip the pictures before each target that nothing refers to and that can't be shown: 6 exact stills of a 1080p file take 310 ms instead of 520 ms.

### Fixed

- An MKV audio track stored minutes away from its video, as some remuxes leave it, now seeks by its own timestamps: reading 2 s of such a track an hour in takes 4 to 6 ms instead of up to 1.9 s.

## v1.6.1 — 2026-10-15

### Changed

- The README shows 1.6.0's faster seeks and smart cuts, re-measured run by run against 1.4.2 on a quieter machine: opening a file 10.5 s in takes 30 to 40 ms on the GPU instead of 44 to 52 ms, and 40 short cuts take 4.6 s instead of 5.9 s on the GPU for an H.264 source and 4.8 s instead of 6.9 s for HEVC (5.2 and 7.5 s on the CPU, from 5.7 and 8.7 s), every frame unchanged.

## v1.6.0 — 2026-10-15

### Changed

- Smart cuts and seeks no longer decode the pictures between the keyframe and the cut point that nothing else uses, on the GPU and the CPU: 40 cuts of a 1080p file with B-frames take 6.1 s instead of 7.7 s on the GPU (HEVC source 4.8 s instead of 6.9 s), with every frame unchanged.
- The GPU HEVC encoder no longer writes frame-rate timing into its stream headers, so no driver can misread it; the frame rate still reaches the driver and the container.

### Fixed

- An MP4 or Matroska file of very small audio frames, such as long stretches of silent AAC, now reads every frame instead of being cut short or refused.

## v1.5.0 — 2026-10-15

### Added

- `gpu::EncoderConfig` has `sar` and `color`, and there is a public `gpu::Color`: struct literals that don't end in `..Default::default()` need the two new fields.

### Changed

- The first frame on the card comes out as soon as it is decoded: 1440p first frame on the GPU 3.5 ms for H.264 and 3.2 ms for HEVC, 1080p GPU playback's first frame about twice as fast.
- A GPU export runs on its own VA-API display, so it no longer shares a driver lock with playback in the same app.
- A GPU smart cut builds one encoder and one decoder per worker for the whole export instead of new ones for every cut.
- Frames read back from the card are reused instead of allocated anew, and their copy runs on the shared thread pool.
- README speed numbers come from the 2026-10-15 run.

### Fixed

- Frames re-encoded on the card around a cut carry the source's aspect ratio and colour tags, so they match the copied frames.
- A stuck GPU job returns an error after 5 seconds instead of hanging the app.
- Malformed H.264 and HEVC parameter sets and slice headers are refused instead of overflowing.
- A crafted MKV or fragmented MP4 can no longer make the readers load the whole file or allocate many times its size.

## v1.4.2 — 2026-10-15

### Fixed

- H.264 encodes on AMD and other Mesa graphics cards no longer hang at 59.94 fps and similar rates (4K among the sizes hit): the stream's header leaves the frame rate to the container, which Mesa's header reader handles.

## v1.4.1 — 2026-10-15

### Changed

- Listing an MKV's keyframes reads its Cues instead of the whole file, with the same list: a 4.2 GB file on a network share answers in about half a second instead of 35 s. Files without complete Cues are still read in full.

## v1.4.0 — 2026-10-15

### Added

- The README shows the PITO MD wordmark at its Licence section.

### Changed

- An MKV or WebM file opens for video from its headers and Cues instead of reading every cluster: a 4.4 GB file on a network share shows its first frame in about 110 ms, reading 6.3 MB, and callers opening the same file at once share one read.
- Media times count from the file's start, and `Probe` carries that start, so a stream that starts late lines up with the others.

### Fixed

- VP9 sources export on the GPU through VA-API with or without an overlay; files whose frames keep several references used to fail with "no free VA-API surface for VP9".
- The `ac3` feature builds without `hevc`, `vp9`, `gif`, `webp` or `opus`.

## v1.3.0 — 2026-10-14

### Added

- AC-3 and E-AC-3 sound in MKV and MP4 decodes natively, ported from oxideav-ac3.
- MP3, MP2 and MP1 sound in MKV, MP4 and MOV decodes natively, ported from minimp3.
- VP9 sources export to VP9 in MP4 with smart cuts: the middles are copied and only the frames before each piece's first keyframe are encoded.
- `pcod frames` turns a folder of PNG frames and an optional WAV into an H.264 and AAC MP4 with faststart, tagged BT.709.
- PNG decode, ported from zune-png, and a WAV reader.
- On VA-API with Mesa, an export with an overlay stays on the GPU: decoded, overlaid by a video-processing blend and encoded without a copy to system memory.
- Every Release carries a docs bundle for the PITO MD docs site, with guides and a generated command reference.

### Changed

- The brand now reads PITO MD.
- A caption or watermark layer costs what its painted area costs: about 1 ms per change at 1080p instead of 12 ms or more, with the same output.
- Exports decode on the GPU whenever they encode on the GPU, smart-cut edges included.
- VA-API frame readback waits until a frame is no longer a reference, so it no longer holds up decoding.
- The README shows a section, a chart and a picture per feature.
- `Codec` gains `Mp3`, `Ac3` and `Eac3`, and `CodecConfig` gains `Vp9`, so code that matches them exhaustively needs new arms.

### Fixed

- `export_frames` stops after `Canvas.frames` frames instead of running until the frame callback ends.

## v1.2.1 — 2026-10-12

### Fixed

- An audio range or stream from an MKV PCM track starts on the right sample: a block whose millisecond timestamp rounds to the start could make the seek land after it and shift the range by up to one block.

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
