# ALAC test fixtures

Small ALAC files for `tests/alac.rs`. They are part of this repository and under its MIT licence: the audio is synthetic, made by `pcm.rs` (`source(channels, depth, frames, salt)`, integer arithmetic only, so it is the same on every platform), and was encoded once with ffmpeg 9.0.2's ALAC encoder. No recorded audio is in them.

Each file was made from `source(...)` written as interleaved little-endian PCM (16-bit, or the low 3 bytes of each sample for 24-bit), with

```
ffmpeg -f s16le|s24le -ar <rate> -ac <channels> [-ch_layout 5.1] -i <pcm> -c:a alac -sample_fmt s16p|s32p [args] -fflags +bitexact -map_metadata -1 <file>
```

and ffmpeg decodes every one of them back to that PCM byte for byte. The test rebuilds the PCM from `source(...)`, checks its FNV-1a hash below, and compares the decode with it.

| File | Channels | Bits | Rate | Frames | Salt | PCM hash (FNV-1a 64) | Extra args | What it covers |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| s16-mono.m4a | 1 | 16 | 44100 | 5097 | 1 | 0x38fe2ad837cbf5bf | | ISO sample entry, the general predictor, a partial last frame |
| s16-stereo.m4a | 2 | 16 | 44100 | 5097 | 4 | 0xfaf2b69324978db4 | | a matrixed pair (mixRes 1) at 16 bits |
| s24-stereo.mov | 2 | 24 | 48000 | 5097 | 3 | 0xd4e884790573c49b | | QuickTime v1 entry with `wave`, bytes shifted by 1, a matrixed pair |
| s24-stereo-96k.mov | 2 | 24 | 96000 | 5097 | 4 | 0xfc82224ff7b56653 | | QuickTime v2 entry, 96 kHz |
| s24-mono-96k.m4a | 1 | 24 | 96000 | 5097 | 5 | 0x819b20c96df9ec7a | | 96 kHz in an ISO entry whose rate field overflows, so probe reads the cookie |
| s16-stereo.mkv | 2 | 16 | 48000 | 5097 | 6 | 0x24d4c6d2e6499652 | | Matroska `A_ALAC` with the bare 24-byte cookie |
| s24-stereo-order8.mkv | 2 | 24 | 48000 | 5097 | 7 | 0xc44526122dddf212 | `-min_prediction_order 8 -max_prediction_order 8` | the order-8 predictor |
| s16-stereo-order4.m4a | 2 | 16 | 44100 | 5097 | 8 | 0xa7b4ec3e9b756457 | `-min_prediction_order 4 -max_prediction_order 4` | the order-4 predictor |
| s16-5.1.m4a | 6 | 16 | 48000 | 1501 | 9 | 0xc48882702336762b | `-ch_layout 5.1` on the input | ALAC channel order to WAVE order |
| s24-mono-verbatim.mov | 1 | 24 | 44100 | 2000 | 10 | 0xe111114e9b0b5ba7 | `-compression_level 0` | escaped (verbatim) frames |
