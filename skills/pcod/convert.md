# Convert, cut, join

`pcod <input> <output>` (the same as `pcod convert <input> <output>`) reads a video, an animated GIF, a folder of PNG frames or a PNG, JPEG or WebP still, and writes what the output's extension names:

| Output | Writes |
| --- | --- |
| `mp4`, `mov`, `mkv` | H.264 and AAC-LC 320k stereo 48 kHz; an AAC source is copied when nothing changes its audio |
| `webm` | VP9 and Opus 128k; alpha kept |
| `gif`, `webp` | animated, loops forever, no audio, 800 px wide at 12 fps or the source's where smaller |
| `wav`, `opus` (or `ogg`), `s16` (or `-`) | the audio alone, see [`audio.md`](audio.md) |
| still `webp`, `png` | one picture from a PNG, JPEG or WebP still, see [`stills.md`](stills.md) |

Video comes out limited-range BT.709, tagged so, with rotation and non-square pixels applied.

```sh
pcod clip.mp4 site.mp4 --profile best --crf 23
pcod clip.mp4 draft.mp4 --profile fast --size 720p --fps 24
pcod clip.mp4 readme.gif --from 0 --to 3 --size 480 --fps 12
pcod clip.mp4 loop.webm --size 320
pcod loop.webm back.mp4
pcod readme.gif readme.mp4
```

## Profiles

`pcod profiles` prints them. `balanced` is the default.

- `fast`: H.264 on the GPU at QP 25, on the CPU where no GPU encoder is found (a `note` says why). Knob: `--qp`.
- `balanced`: H.264 on the CPU, CRF 20 on x264's scale. Knob: `--crf`.
- `best`: H.264 on the CPU, CRF 18. Knob: `--crf`.
- `.webm`: VP9, CRF 32 for `balanced` and 28 for `best`, on libvpx's scale.
- Light web: `--profile best --crf 23` (`.webm`: `--crf 40`). Screen recordings: `--tune animation`.
- `--tune` takes film, animation, grain, stillimage, fastdecode or zerolatency.
- `.gif` and `.webp` take a palette or lossy quality from the profile; `best` spends more effort.

## Size and rate

- `--size`: a width (the height follows), `WxH` within 1% of the source's shape, or `720p`, `1080p`, `1440p`, `2160p`. Nothing else: `360p` is a usage error, and a shape off by more than 1% is refused. pcod never stretches, and it does scale up.
- `--fps`: 1 to 240, an integer, a ratio (`24000/1001`) or a decimal (`29.97` and `23.976` read as the exact NTSC ratios).
- A folder of PNG frames needs `--fps`.

## Cutting

`--from` and `--to` take seconds, `m:ss` or `h:mm:ss`; `--to` defaults to the end. The window runs from the first frame at or after `--from` to the last before `--to`, audio cut in sync, output starting at 0.

```sh
pcod clip.mp4 moment.mp4 --from 0:01 --to 0:03
pcod clip.mp4 moment-small.mp4 --from 0:01 --to 0:03 --size 320
pcod clip.mp4 moment-anim.mp4 --from 0:01 --to 0:03 --tune animation
```

- With no video option the cut copies whole GOPs and re-encodes only the frames at its edges, when the container carries the source's codec. pcod prints `cut 1.000 s to 3.000 s, 60 frames: 28 copied, 32 re-encoded at the edges`.
- A video option (`--profile`, `--size`, `--fps`, `--crf`, `--qp`, `--tune animation`) re-encodes the whole window, and so does a source or output pcod cannot match at the edges (an MP4 or MOV from VP9, say); the `cut` line and a `note` say so.
- Refused: `--to` not after `--from`, `--from` at or past the end, and any `--tune` but `animation` with a window.

## Tracks

```sh
pcod clip.mp4 silent.mkv --no-audio
pcod clip.mp4 bed.wav
pcod clip.mp4 voiced.mp4 --add-audio bed.wav --title Voice,Music
```

- `--no-audio` writes the video alone; with no video option it is copied as it is, index first.
- `--add-audio WAV` keeps the video and the first audio track as they are and adds the WAV as another AAC track (160 kbps, `--audio-bitrate` sets it), the file cut to the shorter of the two. `.mp4` and `.mov` only; `.webm` is refused. `--title` names each audio track in order.
- `--audio-bitrate` is 32 to 512 kbps.
- `--loudness`, `--true-peak` and `--linear` level the audio as it converts: see [`audio.md`](audio.md).

## Join

```sh
pcod clip.mp4 part1.mp4 --from 0 --to 2
pcod clip.mp4 part2.mp4 --from 2 --to 4
pcod join part1.mp4 part2.mp4 film.mp4
```

`pcod join <clip> <clip>... <output>` takes two or more clips. When every clip matches the first (codec, size, rate, colour, audio) it is a stream copy; otherwise, or with any video option, it re-encodes to the first clip's size and rate, letterboxing other shapes. It prints which, and why. It takes the same options as convert.

## Scripting

`--json` prints one manifest: the source's sha256, `params` (profile, encoder, quality, size, fps, audio), `device`, `seconds`, each file's sha256 and bytes, and a `cut` object for a window.
