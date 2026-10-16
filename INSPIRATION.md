# Inspiration

## Why

I wanted something smaller and faster, written natively in Rust, for the PITO MD family of apps and my games. One library each app can carry, with no external ffmpeg to download, bundle or keep in step, and the GPU used first.

It is my own Rust, built for speed, that I can change and release on my own schedule, with fast iterations, instead of waiting on ffmpeg's and other projects' releases. Everything taken in is permissively licensed, so pito-codecs stays MIT.

## Origins

The codecs here are written in Rust. Parts are adapted from permissively licensed projects, each named below with the tag or commit it was taken from; their copyright and licence texts are kept in `NOTICE`.

| Origin | Licence taken | Tag and commit | What came from it |
| --- | --- | --- | --- |
| [FilmCraft](https://github.com/storytold/filmcraft) | MIT (FilmCraft is MIT OR Apache-2.0) | v0.6.0, `2ff3f5e266838700c292a35aaf762e3d4dcef980` | See the module lines in `NOTICE`. |
| [libva](https://github.com/intel/libva) headers | MIT (Intel) | libva 2.23 (VA-API 1.23) | The VA-API declarations in `src/gpu/vaapi/ffi.rs`. |
| NVIDIA Video Codec SDK header `nvEncodeAPI.h` | MIT (NVIDIA) | API 12.1 | The NVENC declarations in `src/gpu/nvenc/ffi.rs`. |
| RFC 6386 reference decoder (Section 20) | BSD-3-Clause (Google) | RFC 6386 (November 2011) | VP8 loop filter, intra prediction, token and transform details in `src/webp/vp8/`. |
| [libwebp](https://chromium.googlesource.com/webm/libwebp) | BSD-3-Clause (Google) | v1.6.0, `4fa21912338357f89e4fd51cf2368325b59e9bd9` | YUV and RGB conversion, the fancy upsampler, the SSE2 loop filter layout, forward transforms and the quality scale in `src/webp/vp8/`. |
| [oxideav-ac3](https://github.com/OxideAV/oxideav-ac3) | MIT (Karpelès Lab Inc.) | 0.0.11, `99228831d2b29a35fe2b2035118a18f8cb0cae59` | The AC-3 and E-AC-3 decoder in `src/ac3/`: frame and BSI parsing, exponents, bit allocation, mantissas, coupling, rematrixing, spectral extension, AHT and the IMDCT structure. |
| [Mesa](https://gitlab.freedesktop.org/mesa/mesa) VA-API frontend and video compositor | MIT | mesa-26.2.2 | Read for behaviour, no code copied: how its video processor composes several pipeline buffers in one picture, blends premultiplied RGBA onto NV12 and P010, and sites chroma, which `src/gpu/vaapi/va.rs` follows to draw export overlays on the GPU. |
| [minimp3](https://github.com/lieff/minimp3) | CC0 1.0 (public domain) | `ea99364f61c14656440e8d77e9c233ccf3124633` | The MPEG-1, 2 and 2.5 audio decoder (layers I, II and III) in `src/mp3/`, with its tables and SSE and NEON layout; the container packet front, the channel matching and the granule-0 scfsi mask are new. |
| [zune-png](https://github.com/etemesi254/zune-image) | MIT OR Apache-2.0 OR Zlib (Caleb Etemesi) | 0.4.10, `aaf40839ed2596fa83091ccfb49629eb5d8efd91` | The scanline de-filters with their SSE2 and SSE4.1 kernels, the chunk checks, bit unpacking, palette and tRNS expansion and Adam7 geometry in `src/png/`; the RGBA row loop is new. |
| [zune-inflate](https://github.com/etemesi254/zune-image) | MIT OR Apache-2.0 OR Zlib (Caleb Etemesi), a port of [libdeflate](https://github.com/ebiggers/libdeflate) (MIT) | 0.2.54, `69502ce83fdfecdd0beefd677e2abb3781b29d98` | The whole-buffer zlib and deflate decoder in `src/png/inflate.rs`: decode tables, fast and slow loops, match copies. |
| [fdeflate](https://github.com/image-rs/fdeflate) | MIT OR Apache-2.0 (The image-rs Developers) | 0.3.7, `c365c7e6ffa81feb2e1fb762eed7299f05c9b0ca` | The PNG encoder's deflate in `src/png/encode.rs`: the fixed Huffman code tuned for filtered rows, its stored dynamic-block header, zero runs and the bit writer; the Paeth pass and chunk writer are new. |
| [adler2](https://github.com/oyvindln/adler2) | 0BSD OR MIT OR Apache-2.0 | 2.0.1, `89a031a0f42eeff31c70dc598b398cbf31f1680f` | The four-lane Adler-32 in `src/png/inflate.rs`. |
| [rav1d](https://github.com/memorysafety/rav1d) | BSD-2-Clause (VideoLAN and dav1d authors, Internet Security Research Group) | `0c851fa046d6a36430570459283e5886fbdd8a66` | The AV1 decoder in `src/av1/cpu/`: OBU and header parsing, the entropy decoder and CDFs, reconstruction (intra, inter, transforms), loop filter, CDEF, loop restoration, film grain, reference MVs and frame and tile threading, with the Rust fallback kernels; the frame front is new. |

## Specifications

- The ITU-T H.264 and H.265 Recommendations, and the ISO/IEC 14496 parts for AAC and the MP4 file format.
- The VP9 Bitstream and Decoding Process Specification (v0.6, WebM project).
- The VP Codec ISO Media File Format Binding (v1.0, WebM project) for the `vp09` sample entry and its `vpcC` box, and the WebM project's VP9 levels for the level it carries.
- RFC 6716 and RFC 8251 (Opus), RFC 7845 (Ogg Opus), RFC 8794 and RFC 9559 (EBML and Matroska) and the WebM container guidelines.
- RFC 9649 (WebP: the container, VP8L lossless and ALPH, written from it alone) and RFC 6386 (VP8 lossy) for `src/webp/`.
- CompuServe's GIF89a specification (1990), the only source for `src/gif/`.
- RFC 1950 and RFC 1951 (zlib and deflate) and the W3C PNG specification for `src/png/`; Microsoft's RIFF WAVE format notes, the only source for `src/media/wav.rs`.
- ATSC A/52 and ETSI TS 102 366 (AC-3 and E-AC-3), which oxideav-ac3 follows; the KBD window in `src/ac3/` is computed from the A/52 definition.
- ITU-R BS.1770-4 and EBU Tech 3341 and 3342 (loudness), and ITU-R BT.601, BT.709 and BT.2020 (colour).
- The paper "A novel macroblock-tree algorithm for high-performance optimization of dependent video coding in H.264/AVC" (2009), whose published description is the only source for the lookahead QP offsets in `src/h264/enc/mbtree.rs`.

## Read, never copied

FFmpeg, x264, x265 and libde265 are GPL or LGPL. They are read for understanding only: no code from them is copied, translated line by line, linked or bundled. FFmpeg appears only as the test oracle and the benchmark reference.

## What may come in

- Code under MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC or zlib may be adapted. Each origin gets its copyright and licence text in `NOTICE` and a row above with its tag or commit. Apache-2.0-only code also carries its own NOTICE text, if it has one, and `NOTICE` says which modules were changed.
- MPL-2.0 code is not taken.
- GPL and LGPL code is never copied.
- libvpx (BSD-3-Clause, with Google's additional patent grant for VP8 and VP9) and libopus (BSD-3-Clause, the RFC 6716 reference) may be ported, encoders included, with their copyright and licence text in `NOTICE`.
- Cisco's openh264 (BSD-2-Clause), Kvazaar (BSD-3-Clause) and dav1d (BSD-2-Clause) may be adapted with their notices. openh264's patent licence covers only Cisco's own binaries, not code built from it.
- Test media is generated by the tests or comes with a clear licence; none is committed with unclear terms.
