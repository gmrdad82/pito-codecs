# Stills, frame ranges, filmstrips, frames to video

`stills` and `filmstrip` read MP4, MOV, MKV and WebM, on the GPU first (`--route cpu` forces the CPU). `stills` also reads an animated GIF and a folder of PNG frames in name order (with `--fps` for their rate).

## Stills and contact sheets

```sh
pcod stills clip.mp4 --count 24 --cols 6 --sheet sheet.png
pcod stills clip.mp4 --at 0:01,0:05 --out shots --height 720
pcod stills clip.mp4 --count 4 --out four --seek keyframe --json
```

- Give `--out DIR` (writes `still-001.png`, `still-002.png`, ...), `--sheet FILE.png`, or both; with neither it is a usage error.
- `--count` N evenly spaced stills (12 by default), or `--at` comma-separated times.
- `--height` per still (360 by default, shape kept, never scaled up).
- A sheet is `--rows` by `--cols` (to fit by default) with `--pad` pixels (8) between and around the stills, on black. It stays PNG.
- Each still is the exact frame; `--seek keyframe` takes the keyframe at or before the time instead (faster on long GOPs).
- A time past the end is refused with one line.

## Frame ranges

`--from`, `--to`, `--fps` or `--every` switch `stills` to ranges: every frame of the window goes into `--out` as `000000.png`, `000001.png`, ..., the names `pcod frames` reads.

```sh
pcod stills clip.mp4 --from 1 --to 1.5 --out frames --size 640x360
pcod stills clip.mp4 --every 2s --size 720p --out posters --format jpeg --quality 85
pcod stills clip.mp4 --from 1 --to 1.2 --out cropped --crop 320:180:0:0
pcod stills clip.mp4 --from 1 --to 1.2 --fps 30 --hold 1s --out held
```

- A range is 360 px high by default; give `--size` (a width, `WxH` or `720p` to `2160p`) for another size.
- `--fps` or `--every` (`2s`, `500ms`, `1.5`, `0:01`) thins the frames, the nearest frame for each; with no `--from` or `--to` they cover the whole video.
- `--format png|jpeg|webp`, with `--quality` 1 to 100 (85) for jpeg and webp. A sheet stays PNG.
- `--crop W:H:X:Y` keeps one rectangle of each frame, in the pixels the frame is shown at, with no scaling; it does not go with `--size`, and a rectangle outside the frame is refused.
- `--hold 1s` repeats the last written frame for that long at the `--fps` rate; it needs `--fps`.
- Refused: a folder that already holds numbered frames (pass `--force` to replace them), a folder with any other PNG in it, and more than 100,000 frames without `--force`.
- `--count`, `--at` and `--sheet` do not go with a range.

## Filmstrip

```sh
pcod filmstrip clip.mp4 strip.png --every 2s --seek exact --timecodes
pcod filmstrip clip.mp4 strip.webp --count 20 --wrap 10 --height 120
```

- A tile every `--every`, or `--count` tiles (10 by default) spread over the window; `--from` and `--to` narrow it.
- `--height` is 90 by default and never scales up. The default seek is the keyframe at or before each time (the time its label shows); `--seek exact` takes the exact frame.
- One row, or `--wrap` tiles per row, or rows no wider than `--max-width` pixels. `--gap` pixels (4) of `--background` (`#000000`) sit between and around the tiles; `--timecodes` writes each tile's time under it.
- The output's extension picks PNG, JPEG or WebP. Over 268 megapixels, or an existing output without `--force`, is refused.

## PNG frames to a video

```sh
pcod frames frames trailer.mp4 --fps 30 --audio bed.wav
pcod frames frames trailer-hevc.mp4 --fps 30 --codec hevc --crf 20 --audio-bitrate 256
pcod frames frames barred.mp4 --fps 30 --progress bar,position=bottom
pcod frames/ trailer.webm --fps 30
```

`pcod frames <dir> <output.mp4> --fps N` encodes the folder's `.png` files in name order, all one even size, as 8-bit 4:2:0 BT.709 H.264 (or `--codec hevc`), faststart. A gap in the numbering, a frame of another size and a missing WAV each stop it with one line.

- `--audio` takes a WAV of any rate, cut or padded with silence to the video; without it the MP4 carries a silent stereo track. AAC at 192 kbps (320 with `--loudness`); `--audio-bitrate` 32 to 512.
- `--audio-copy FILE` keeps another file's sound as it is: the first audio track of an MP4, MOV or MKV, its AAC packets muxed in with no decode and no re-encode, so loudness and true peak stay bit for bit (a short over a finished -14 LUFS film keeps its -1 dBTP ceiling). It is cut to the video's length; a shorter track just ends and the video runs on in silence, with a `note`. It refuses `--audio`, `--loudness` and a codec `.mp4` can't carry, naming it (Matroska takes AAC only).
- `--progress SPEC` draws a progress bar over the video as it encodes, filling over the folder's frames: see [`convert.md`](convert.md) for the spec.
- `--crf` is 0 to 51 and 18 by default; 0 is near-lossless, not bit-exact.
- `--route cpu` forces the CPU; `--threads` caps the PNG decoders (one per core, at most 16).
- HEVC encodes on the GPU only; without a GPU HEVC encoder the file is H.264, and a `note` says why.
- For `.webm`, `.mov`, `.mkv` or alpha, use convert on the folder: `pcod frames/ trailer.webm --fps 30`. RGBA frames keep their alpha in `.webm`, `.gif` and `.webp`, and the MP4 drops it.

## Still images

```sh
pcod cropped/000000.png poster.png --crop 100:100:10:10
pcod poster.png poster.webp --quality 80
pcod poster.png poster-lossless.webp --lossless
pcod poster.webp poster-back.png
```

A PNG, JPEG or WebP still converts to a still `.webp` (lossy at `--quality` 0 to 100, 75 by default, or `--lossless`, keeping alpha, at the source's size or `--size`) or to a `.png`; `--crop W:H:X:Y` cuts one PNG to a PNG. A JPEG is turned upright by its EXIF orientation first.
