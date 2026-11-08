# Stills, frames and filmstrips

All three read MP4, MOV, MKV and WebM, on the GPU route first (`--route cpu` forces the CPU). `--json` prints one line.

## Stills and contact sheets

```sh
pcod stills clip.mp4 --count 24 --cols 6 --sheet sheet.png
pcod stills clip.mp4 --at 0:05,1:10 --out shots --height 720
```

- `--count` N evenly spaced stills (12 by default) or `--at` comma-separated times.
- `--height` per still (360 by default), shape kept.
- `--out DIR` writes `still-001.png`, ...; `--sheet FILE` writes one contact sheet of `--rows` by `--cols` with `--pad` pixels (8) between. Give both if you want both.
- Each still is the exact frame; `--seek keyframe` takes the keyframe at or before instead (faster on long GOPs).
- A time past the end stops with one line.

## Frame ranges

`--from` and `--to` write every frame of a window into `--out` as `000000.png`, `000001.png`, ..., the names `pcod frames` reads.

```sh
pcod stills clip.mp4 --from 1.0 --to 1.5 --out frames --size 1920x1080
pcod stills clip.mp4 --every 2s --size 1080p --out posters --format jpeg --quality 85
```

- A range is 360 px high by default: give `--size` for the source's own size.
- `--fps` or `--every` (`2s`, `500ms`, `1.5`, `0:01`) thins the frames; with no `--from`/`--to` they cover the whole video.
- `--format png|jpeg|webp`, `--quality` 1 to 100 (85) for JPEG and WebP; both need `--out`. Sheets stay PNG.
- More than 100,000 frames, or a folder that already holds numbered frames, is refused without `--force`; a folder with any other PNG is always refused.
- `--count`, `--at`, `--sheet` and `--seek` do not go with a range.

## Filmstrip

```sh
pcod filmstrip clip.mp4 strip.png --every 2s --seek exact --timecodes
pcod filmstrip clip.mp4 strip.webp --count 20 --wrap 10 --height 120
```

- A tile every `--every`, or `--count` tiles (10) over the window; `--from`/`--to` narrow it.
- `--height` 90 by default, never scaled up. Default seek is the keyframe at or before; `--seek exact` takes the exact frame.
- One row, or `--wrap` tiles per row or `--max-width` pixels; `--gap` (4) of `--background` (`#000000`); `--timecodes` labels tiles.
- The output's extension picks PNG, JPEG or WebP. Over 268 megapixels, or an existing output without `--force`, is refused.

## PNG frames to a video

```sh
pcod frames frames trailer.mp4 --fps 60 --audio trailer.wav
pcod frames/ trailer.webm --fps 60
```

`pcod frames` writes H.264 (or `--codec hevc`) 8-bit 4:2:0 BT.709 with AAC 48 kHz stereo, faststart. Frames are the folder's `.png` files in name order, all one even size; a gap in numbering stops the encode. `--audio` takes a WAV of any rate; it is cut or padded with silence to the video. Without it the MP4 carries a silent track. `--loudness`, `--true-peak`, `--linear`, `--audio-bitrate` work as in [`convert.md`](convert.md).

## Still images

`pcod in.png out.webp` converts one still (`--quality`, `--lossless`); `--crop W:H:X:Y` crops a PNG to a PNG.
