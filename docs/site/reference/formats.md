---
title: Formats
description: The codecs, containers and picture formats pito-codecs reads and writes, and what pcod prints.
sidebar:
  order: 2
---

## Codecs and containers

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

Not covered: DTS, FLAC, Vorbis, ALAC, AV1, ProRes, DNxHD, MPEG-2 and VP8 video, and subtitle tracks. An app can still open such files through the user's own ffmpeg, when its `Prefs` pick it ([routes](../../guides/library/#routes)).

## What each job reads and writes

- **pcod** reads MP4 files (H.264 or H.265 video) and writes MP4 with H.264 or H.265 video, copying the audio track.
- **The `media` front** reads MP4, MOV, MKV and WebM. Stills come back as `Rgba`, audio as 32-bit float `AudioBuffer`s at the rate and channel count asked for, and frames as `Frame`s in their decoded pixel format (8 or 10-bit 4:2:0 for most files).
- **Exports** (`export`, `export_with_audio`, `export_frames`) write MP4 with H.264, H.265 or VP9 (`vp09`) video and AAC-LC audio.

## What pcod prints

pcod prints plain text for people, one `label value` pair a line, as the [pcod guide](../../guides/pcod/) shows; it has no JSON or other machine-readable output. Errors are one line on stderr, `pcod: <what failed and why>`, with exit code 1, and a command line it can't parse gets clap's error with exit code 2. An app that needs structured results calls the library, whose `Probe`, `Exported` and `Picked` are plain Rust structs.

## GPU routes

What each GPU route decodes and encodes, per vendor and system, is in [GPU routes](../../guides/gpu/).
