---
title: "Replacing png, image, zune-jpeg, hound, lewton and ebur128"
description: "The call in each outside crate beside the call in pito-codecs, what each feature compiles, how the results compare, and what stays impossible."
sidebar:
  order: 10
---

From 5.1.0, pito-codecs does what apps use these six crates for, as library calls with no command line and no other outside crate: `png` and the PNG parts of `image`, `zune-jpeg` and the JPEG parts of `image`, `hound`, `lewton` and `ebur128`. This page lists every call shape the crates were used for, the call that replaces it, and where the two differ. Where it says "equal", a test reads or writes the same input with the outside crate (a dev-dependency only) and compares the bytes or samples.

```toml
[dependencies]
pito-codecs = { git = "https://github.com/gmrdad82/pito-codecs", tag = "v5.1.2", default-features = false, features = ["png"] }
```

## Features

Each module stands alone. A project that takes one feature builds that module and `media` (the shared types and the WAV code), and nothing else: `cargo tree` shows no outside crate for any of them. Clean build times are `cargo build --release --lib --no-default-features --features <name>` after `cargo clean -p pito-codecs --release`, sccache off, `-j 8`, on a shared 32-thread machine under load 33 to 60, one run each; sccache is off because it would answer from its cache in 50 ms.

| Feature | Compiles | Outside crates | Clean build |
|---|---|---|---|
| `png` | `pito_codecs::png`: decoder (all colour types, 1 to 16 bits, interlaced), encoder, metadata; about 2,800 lines | none | 0.4 s |
| `jpeg` | `pito_codecs::jpeg`: baseline and progressive decoder, restart intervals, encoder; AVX2 paths with scalar fallbacks; about 3,000 lines | none | 0.4 s |
| `wav` | `pito_codecs::media::wav`: reader and writer; about 500 lines | none | 0.2 s |
| `vorbis` | `pito_codecs::vorbis`: decoder and the streaming `Stream`; about 5,000 lines | none | 0.6 s |
| `loudness` | `pito_codecs::loudness`: `LoudnessMeter`; about 750 lines | none | 0.5 s |
| `image` | `pito_codecs::image`: sniffing open, decode, encode and size over `png` and `jpeg`; 70 lines | none | 0.7 s |

`filter` still has its own `filter::LoudnessMeter` path; it is the same type, now defined in `loudness`, and `filter` turns on the parallel channel filter that `loudness` alone leaves serial.

## png

`png::decode(bytes)` was already there and returns 8-bit RGBA. 5.1.0 adds the native pictures, so 16 bits, grey and grey-alpha stay what the file says.

| In `png` | In pito-codecs |
|---|---|
| `Decoder::new(reader)`, `set_transformations(EXPAND)`, `read_info()`, `next_frame(&mut buf)` | `png::Decoder::new().decode_picture(bytes, Transform::Expand)?` returns `Picture { width, height, color, depth, data }`: palettes become RGB or RGBA, bits below 8 become 8, a `tRNS` chunk becomes alpha, 16-bit samples stay 16-bit big-endian as the file stores them |
| `normalize_to_color8()`, or `EXPAND \| STRIP_16` | `Transform::Normalize8`: the same, with 16-bit samples cut to their high byte |
| `Transformations::IDENTITY` | `Transform::Identity`: the file's own samples and colour type; the palette and `tRNS` come from `png::metadata` |
| `reader.output_buffer_size()` and `next_frame(&mut buf)` into your own buffer | `png::output(bytes, transform)?` gives `Output { width, height, color, depth, row_bytes, bytes }`, then `decoder.decode_slice(bytes, transform, &mut buf)?` fills the first `bytes` of `buf` and leaves the rest alone; a short buffer is an error |
| 16-bit samples as `u16` | `picture.samples16()`; `Picture::from_u16(w, h, color, &samples)` goes the other way |
| `reader.info().size()`, `.bit_depth`, `.color_type`, `.interlaced` | `png::info(bytes)?` returns `Info { width, height, depth, color, interlaced }` from the header alone |
| `info.srgb`, `info.gama_chunk`, `info.chrm_chunk`, `info.pixel_dims` | `png::metadata(bytes)?` returns `Meta { srgb, gamma, chromaticities, pixel_dims, palette, transparency, text }`; gamma and chromaticities are the file's scaled integers (45455 for 0.45455) |
| `info.uncompressed_latin1_text`, `compressed_latin1_text`, `utf8_text` | `Meta::text`: `tEXt`, `zTXt` and `iTXt` (compressed or not) as `Text { keyword, text, language }` |
| `Encoder::new(w, h)`, `set_color`, `set_depth`, `write_header()`, `write_image_data(&data)` | `png::encode_picture(&Picture, &Options)?` returns the file's bytes; `Picture` carries `Color::{Grey, GreyAlpha, Rgb, Rgba}` at depth 8 or 16, so grey, grey-alpha and RGB are written as such |
| `set_compression(Fast)`, `Default`, `High` | `Options { compression: Compression::{Fast, Balanced, High}, .. }`; `Fast` is the quick encoder `png::encode` always used (Paeth on every row, no matching), `Balanced` and `High` pick a filter per row, match repeats and write dynamic Huffman blocks (`High` searches deeper); big pictures are compressed in pieces on every core, and the bytes do not depend on how many |
| `set_source_srgb(Perceptual)` | `Options { srgb: Some(0), .. }` (0 perceptual, 1 relative, 2 saturation, 3 absolute) |
| `set_source_gamma(ScaledFloat::from_scaled(n))` | `Options { gamma: Some(n), .. }` |
| `set_source_chromaticities(..)` | `Options { chromaticities: Some([wx, wy, rx, ry, gx, gy, bx, by]), .. }` as scaled integers |
| `set_pixel_dims(..)` | `Options { pixel_dims: Some((x, y, unit)), .. }` |
| `add_text_chunk`, `add_itxt_chunk` | `Options { text: vec![(key, value)], .. }`: `tEXt` when every character is Latin-1, `iTXt` otherwise |
| 8-bit RGBA or RGB straight from `media::Rgba` | `png::encode_rgba(&rgba)` and `png::encode(&rgba)` (RGB, alpha dropped), as before |
| 16-bit to 8-bit with rounding (`image`'s `to_rgba8()`) | `picture.to_rgba_rounded()`; `picture.to_rgba()` takes the high byte, which is what `STRIP_16` does |

Equal means: for 44 files (every colour type and depth the format has, plain and interlaced, with and without a `tRNS` key or palette alpha, from the repository's own reference set) the picture from each of the three transforms equals the `png` 0.18 reader's output bit for bit, colour type and depth included (123 comparisons; `Identity` is not offered on an interlaced file below 8 bits a pixel and says so). The 16-bit files also match the ImageMagick `rgba64` references the repository already holds. Every colour type at 8 and 16 bits, at all three compression levels, is written and read back by `png` 0.18 with the same samples. 3,000 random corruptions and truncations of a file go through every new entry point without a panic, and `fuzz/fuzz_targets/png.rs` runs them under cargo-fuzz.

Differences: a `tRNS` chunk on a palette image that lists more entries than the palette holds turns the output to RGBA and applies the first entries, as `png` does (the older `png::decode` ignores such a chunk, as ImageMagick does). An ancillary chunk (`tRNS`, text, colour) with a bad CRC is skipped, never fatal; a bad CRC on a critical chunk is an error, as before. `High` is not `Best`: it searches 512 candidates a position and parses lazily, so files can be a little larger than `png`'s best level.

## image

`image` is used for three things: opening and saving PNG and JPEG, `RgbaImage` as a pixel buffer, and `imageops`.

| In `image` | In pito-codecs |
|---|---|
| `image::open(path)?.to_rgba8()` | `image::open(path)?` returns `media::Rgba` (`image` is `pito_codecs::image`, the module, not the crate) |
| `load_from_memory(bytes)`, `load_from_memory_with_format` | `image::decode(bytes)?` sniffs PNG or JPEG; `png::decode` and `jpeg::decode` when the format is known |
| `ImageReader::new(..).with_guessed_format()`, `ImageFormat::Png` | `image::sniff(bytes)` returns `Some(Format::Png)` or `Some(Format::Jpeg)` |
| `image_dimensions(path)`, `into_dimensions()` | `image::dimensions(bytes)?` reads the header only |
| `img.save("x.png")`, `PngEncoder::new_with_quality(..)` | `image::save(path, &rgba, Output::PngRgba)`; `Output::Png` writes RGB; for compression levels use `png::encode_picture` |
| `JpegEncoder::new_with_quality(w, q)`, `save("x.jpg")` | `image::save(path, &rgba, Output::Jpeg(q))`, or `image::encode(&rgba, Output::Jpeg(q))` for bytes; quality 1 to 100 |
| `RgbaImage::from_raw(w, h, vec)` | `media::Rgba::from_raw(w, h, vec)` returns `None` unless the length is `w * h * 4` |
| `RgbaImage::new(w, h)` | `media::Rgba::new(w, h)` |
| `img.get_pixel(x, y)`, `put_pixel(x, y, p)` | `rgba.pixel(x, y)` returns `Option<[u8; 4]>`; `rgba.put(x, y, [r, g, b, a])` returns whether it was inside |
| `img.into_raw()`, `as_raw()`, `width()`, `height()` | the public fields `data`, `width`, `height` |
| `imageops::crop_imm(&img, x, y, w, h).to_image()` | `rgba.crop(x, y, w, h)`, clamped at the edges as `crop_imm` is |
| `imageops::replace(&mut a, &b, x, y)` | `a.replace(&b, x, y)`, with negative and overhanging offsets clipped |

Equal means: `image::load_from_memory` and `image::decode` agree byte for byte on a PNG and on a JPEG; `image_dimensions` agrees; files saved by one open in the other with equal pixels; `crop` and `replace` equal `crop_imm` and `replace` at six offsets including negative and overhanging ones. `image` 0.25 converts 16-bit pictures with a rounding formula; `to_rgba_rounded` is that formula, and a test over every 37th 16-bit value shows the two equal.

## zune-jpeg

| In `zune-jpeg` | In pito-codecs |
|---|---|
| `JpegDecoder::new_with_options(ZCursor::new(bytes), options)` with `jpeg_set_out_colorspace(RGBA)`, `decode()` | `jpeg::decode(bytes)?` returns `media::Rgba`; grey files come out as grey in all four channels with alpha 255 |
| decoding frame after frame into one buffer | `jpeg::decode_into(bytes, &mut rgba)?` reuses the buffer's allocation |
| `dimensions()` after `decode_headers()` | `jpeg::decode::info(bytes)?` returns width, height, sampling, whether it is progressive, orientation and precision without decoding |
| baseline, progressive, restart intervals, 4:4:4, 4:2:2, 4:2:0, 4:4:0, grey | all decoded |

The decoder's inverse transform and the chroma-to-RGB step now run AVX2 on x86-64 with runtime detection (`PITO_CODECS_NO_SIMD` turns them off), with the scalar code as the fallback elsewhere. A frame decodes to RGBA in, best of 200 frames of the same 1,920 by 1,080 file, on a shared 32-thread machine (load 30), with the version before this release in the first column of the 4:2:0 row for scale:

| 1080p frame | pito-codecs | zune-jpeg 0.5.15 |
|---|---|---|
| 4:2:0 (a webcam's usual MJPEG) | 6.17 ms (was 17.95 ms in 5.0.1) | 6.27 ms |
| 4:2:2 | 7.91 ms | 6.90 ms |
| 4:4:4 | 9.56 ms | 8.71 ms |
| grey | 5.55 ms | 4.78 ms |

60 frames a second is 16.7 ms a frame, so a 4:2:0 or 4:2:2 stream decodes with more than a 2x margin on one thread. The two decoders are within 10% of each other; the numbers are from a contended machine and are not a claim about either decoder on a quiet one, so run `cargo run --release --example jpeg_speed --no-default-features --features jpeg -- your.jpg` for your own.

Equal means: on the 118 reference JPEGs the repository holds (cjpeg and our encoder's files, baseline, progressive and with restart intervals, 4:4:4, 4:2:2, 4:2:0 and grey, 7 to 64 pixels a side) the RGBA output equals zune-jpeg's byte for byte, and so does a 1,920 by 1,080 frame. Where it differs, pito-codecs follows libjpeg, which the repository's references record:

- On a size that is not a multiple of the chroma block (a width not divisible by 16 in 4:2:0 or 4:2:2, a height not divisible by 16 in 4:2:0), the last column and the last row of RGB differ from zune-jpeg's, because pito-codecs repeats the last chroma sample the way libjpeg does and zune-jpeg blends in the padding. A test shows the differences are on that column and row only; on a photograph they are a step or two.
- Three baseline files whose components arrive in separate scans decode correctly (within one of `djpeg`) and wrongly in zune-jpeg 0.5.15 (mean error of 83 on a 0-255 scale in two of them).

## hound

| In `hound` | In pito-codecs |
|---|---|
| `WavReader::open(path)`, `WavReader::new(reader)` | `wav::Reader::open(path)?`, `wav::Reader::new(reader)?` over any `Read + Seek` |
| `reader.spec()` | `reader.layout`: `channels`, `rate`, `bits`, `float()` |
| `reader.duration()` | `reader.layout.frames()` |
| `samples::<f32>()` on a float file, or `samples::<i32>()` scaled by `1 / 2^(bits-1)` | `reader.read(frames, &mut Vec<f32>)?` returns the frames read, in blocks you choose; 8 bits is `(b - 128) / 128`, 16, 24 and 32 divide by `2^(bits-1)`, the same scale |
| `samples::<i32>()`, `samples::<i16>()` | `reader.read_ints(frames, &mut Vec<i32>)?`; 8-bit samples are signed (`b - 128`) as hound's are |
| `WavReader::seek(time)` | `reader.seek(frame)` and `reader.position()` |
| a whole file | `wav::decode(bytes)?` returns an `AudioBuffer` |
| `WavSpec { channels, sample_rate, bits_per_sample, sample_format }` | `Format::{Int8, Int16, Int24, Int32, Float32}` and the rate and channel count |
| `WavWriter::create(path, spec)`, `WavWriter::new(writer, spec)` | `wav::Writer::create(path, rate, channels, format)?`, `wav::Writer::new(writer, rate, channels, format)?` over any `Write + Seek` |
| `write_sample(i16)`, `write_sample(i32)` | `writer.write_ints(&[i32])?` in the file's own range, clipped at its ends; interleaved whole frames |
| `write_sample(f32)` into a float file | `writer.write(&[f32])?` |
| the float to integer step an app writes by hand (`clamp`, scale by 32767 or 8388607, round) | `writer.write(&[f32])?` into an integer format does exactly that, including `clamp` and NaN as 0 |
| `finalize()` | `writer.finish()?` patches the sizes, pads an odd data chunk and returns the writer |
| one-shot | `wav::encode(&audio_buffer, format)?` returns the bytes |

Equal means: files written here in every format at 1, 2, 3 and 6 channels are read by hound with the same spec, duration and samples; files hound writes in every format at 1, 2 and 5 channels are read whole, in blocks and as integers here with the same samples, and a file written in odd-sized blocks equals one written in one call byte for byte. 2,000 corruptions and truncations of a file go through the reader and the writer's inputs without a panic (`fuzz/fuzz_targets/wav.rs`).

Writing: 16 bits and under with at most two channels is a plain PCM `fmt` chunk, float with at most two channels is a float `fmt` with a `fact` chunk, anything wider or with more channels is `WAVE_FORMAT_EXTENSIBLE`, as the format requires. A file over 4 GiB is refused (`unsupported`); RF64 is read, not written.

## lewton

| In `lewton` | In pito-codecs |
|---|---|
| `OggStreamReader::new(Cursor::new(bytes))` | `vorbis::Stream::from_bytes(&bytes)?` |
| `OggStreamReader::new(BufReader::new(file))`, `Box<dyn Read + Seek>` | `vorbis::Stream::new(source)?` for any `Read + Seek`; `Stream::open(path)?` for a file; a `Cursor<Arc<[u8]>>` works too |
| `reader.ident_hdr.audio_sample_rate`, `audio_channels` | `stream.rate()`, `stream.channels()` |
| `reader.comment_hdr` | `stream.comments` |
| `read_dec_packet_itl()` returning `Option<Vec<i16>>` | `stream.next_block()?` returns `Option<&[f32]>`, one block of interleaved samples in -1 to 1, `None` at the end; divide nothing, the samples are floats |
| `get_last_absgp()` and trimming the last block | the stream trims the end by the last page's granule position and the start by the first audio page's, as `vorbis::decode` does; `stream.total_frames()` gives the length up front when the last page is intact |
| opening the file again to loop | `stream.rewind()?` starts again from the top without reopening, keeping the source |
| frames so far | `stream.position()` |

Equal means: on the 10 Ogg Vorbis files the repository records (1 to 8 channels, 22.05 to 48 kHz) the concatenated blocks equal `vorbis::decode`'s samples exactly, and land on libvorbisfile's samples within the references' tolerances; a file repaged with one, two, five and eleven packets a page, and with a lead of 300 frames the granule positions trim, streams the same samples as the whole decode, before and after a rewind; any `Read + Seek` source gives the same samples; against lewton the rate, channel count and length are equal on every file and the samples are within 1.5 steps of a 16-bit sample on every file of one or two channels. Truncation ends the stream on its last whole page; random corruption never panics (`fuzz/fuzz_targets/vorbis_stream.rs`).

Differences: for more than two channels lewton hands the samples in Vorbis order (left, centre, right, ...), pito-codecs in WAVE order (left, right, centre, LFE, ...), like every other decoder here. Chained streams are refused by name (`unsupported`). A page with a bad checksum is an error in `Stream`; `vorbis::decode` still skips such a page with a warning.

## ebur128

| In `ebur128` | In pito-codecs |
|---|---|
| `EbuR128::new(channels, rate, Mode::I \| Mode::M \| Mode::S \| Mode::LRA \| Mode::SAMPLE_PEAK \| Mode::TRUE_PEAK)` | `LoudnessMeter::ebur128(rate, channels)?`: note the order. It reads as ebur128 does, and measures everything, so there are no mode flags. Add `.windows()?` when you read the momentary, short-term or window loudness at an arbitrary frame. `LoudnessMeter::new(rate, channels)?` is the meter pcod uses and reads differently, see Differences |
| `add_frames_f32(&[f32])` | `meter.add(&[f32])?`, interleaved; `meter.process(&AudioBuffer)?` as before. Other sample types convert to `f32` first |
| `loudness_global()` | `meter.integrated()`; `f64::NEG_INFINITY` when nothing passes the gate, as in ebur128 |
| `loudness_momentary()` | `meter.momentary_now()?` after any number of frames (needs `windows()`); `meter.momentary()` is the value at the last whole 100 ms |
| `loudness_shortterm()` | `meter.short_term_now()?`, or `meter.short_term()` at the last 100 ms |
| `loudness_window(ms)` | `meter.window(ms)?` for 1 to 3000 ms; a clip shorter than a gating block reads as the window over its whole length |
| `sample_peak(c)` | `meter.sample_peak_of(c)` returns `Option<f64>`, linear; `meter.sample_peak()` is the largest in dB |
| `true_peak(c)` | `meter.true_peak_of(c)`, linear; with `ebur128()` the same 48-coefficient interpolator (4x below 96 kHz, 2x below 192 kHz, none above); `meter.true_peak()` is the largest in dB |
| `loudness_range()` | `meter.range()`: short-term blocks every second, as ebur128 takes them; `0.0` before the first block (3 s in) |
| `relative_threshold()` | `meter.threshold()`; `-70.0` when nothing passed the absolute gate, as in ebur128 |
| `set_channel_map(..)` | `meter.set_weights(&[f64])`, one weight a channel (0 leaves a channel out, 1.41 is a surround) |
| the two together | `meter.summary()` returns integrated, range, both peaks, and the loudest momentary and short-term |

Equal means, for a meter made with `LoudnessMeter::ebur128`, with ebur128 as a dev-dependency and the same samples fed to both in blocks:

- On a 7.3 s signal with a tone, noise, a quiet middle and impulses, at 22.05, 44.1, 48, 88.2, 95.999, 96 and 192 kHz with 1, 2, 3, 4, 5 and 6 channels, in blocks from 441 to 19,200 frames, after every block the momentary, short-term and five window readings (1, 50, 400, 1,234 and 3,000 ms) differ from ebur128's by less than 1e-6 LU; at the end the integrated loudness, the loudness range and the relative threshold differ by less than 1e-6 LU, the sample peaks by less than 1e-7, and silence reads minus infinity and a zero peak in both.
- The true peak is the same filter: ebur128's 48 coefficients (a Hann-windowed sinc, four phases of 12 taps below 96 kHz, two of 24 below 192 kHz, none from 192 kHz up, where the true peak is the sample peak). On 33 signals (22.05, 44.1, 48, 88.2, 96 and 192 kHz with 1 to 6 channels, three seeds, 9.1 s each, every channel) the largest difference in linear peak measured is 0, bit for bit, and the tests hold it under 1e-6. The signals pfx's `peak.rs` uses (a 12 kHz tone at 0, 45, 60 and 67.5 degrees, a full-scale 45 degree tone, 1, 17 and 9.6 kHz tones, a two-sample full-scale click and its hot mix) read the same peak per channel, and so does the chime's 100 ms-block short-term maximum with a second of padding.
- The loudness range and the integrated loudness use ebur128's own rule: every block above the absolute gate is kept (one number per 100 ms, one per second for the range), the range takes the 10th and 95th percentile of the short-term blocks above minus 20 LU from their mean, and the integrated takes the mean above minus 10 LU. On those 33 signals the largest differences measured are 9e-10 LU on the range and 4e-8 LU on the integrated loudness (the K-weighting filter's rounding at 192 kHz). On programmes of 12, 31 and 63.5 s with quiet and loud passages the range is over 1 LU and equal; a steady tone reads under 0.01 LU in both, and a clip under 3 s reads 0 in both.
- inbox0's call (`Mode::I | SAMPLE_PEAK | M`, then `loudness_window` for a clip too short to gate) reads the same integrated loudness, sample peak and window; up to 40,000 frames of NaN, infinity, subnormal, full-range and random-bit samples at ten rates from 1 Hz to 400 kHz go through every reading without a panic (`fuzz/fuzz_targets/loudness.rs` adds the same under cargo-fuzz).

Differences: `LoudnessMeter::new` is the meter `pcod loudness` and its levelling use, and it reads three things differently from `ebur128()`. Its true peak uses a 32-tap Kaiser-windowed interpolator, which reads broadband material higher (white noise at 44.1 kHz: -4.94 dBTP against -5.78, which is why pcod keeps it: its ceiling checks would otherwise pass mixes that peak over the ceiling) and an impulse train up to 0.12 dB lower; its loudness range takes the short-term blocks every 100 ms, so it can differ by 1 LU or more; and four channels weigh all 1 where `ebur128()` weighs them left, right, surround, surround. The momentary, short-term and window readings and the sample peaks are the same in both, and the integrated loudness differs by under 1e-6 LU. A project replacing ebur128 calls `ebur128()` and nothing else changes. The default weights for 7 and 8 channels are, in both meters, the BS.1770 7.1 layout (surround on channels 6 to 8), where ebur128 gives channels 7 and 8 no weight; `set_weights(&[1.0, 1.0, 1.0, 0.0, 1.41, 1.41, 0.0, 0.0])` makes them equal (a test does), and up to 6 channels the defaults are ebur128's. The meter keeps one `f64` per 100 ms for the integrated loudness and one per second for the range, the same unbounded list ebur128 keeps by default, about 290 KB an hour. The meter keeps no rate converter or channel remap of its own.

## What remains impossible

- `image::imageops::resize`, `overlay`, `blur` and `RgbaImage::from_fn`, `from_pixel` and pixel iterators are image processing, not codecs. pito-codecs has `filter::Scaler` (Bilinear, Bicubic, Lanczos) and the overlay filter, which work on `Frame`s and are not byte for byte what `imageops` makes; there is no blur and no blend of one `Rgba` onto another. An app that needs those keeps `image` for them, or fills `Rgba` in a loop.
- `Rgba32FImage` and the other `image` float and 16-bit buffer types: `Picture::samples16` covers 16-bit samples, floats have no PNG or JPEG file anyway.
- PNG: writing a palette image, writing an interlaced image, APNG, and the per-row `FilterType` choice (`Fast` is Paeth, `Balanced` and `High` choose per row). Writing row by row through a stream (`write_image_data` in pieces, `stream_writer`) is not offered; the whole picture is encoded at once.
- JPEG: CMYK, YCCK, lossless and arithmetic-coded files are refused by name, and a 12-bit file reports `decodable: false` from `info`; zune-jpeg's other output colourspaces (YCbCr, luma only) are not offered, the output is RGBA.
- WAV: more than 4 GiB, RF64 writing, and sample sizes that are not 8, 16, 24 or 32 bits.
- Vorbis: seeking to a frame (rewind to the top is there), chained streams, and lewton's i16 output (divide by 32768 yourself if you need it back).
- ebur128: mode flags and `set_max_window` (not needed; every reading is always on), `Mode::HISTOGRAM` (the integrated loudness and range are exact, never binned), the seed and reset calls, and sample types other than `f32`.
