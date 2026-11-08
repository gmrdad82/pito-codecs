# Convert, cut and join

`pcod <input> <output>` (or `pcod convert ...`) converts a video file or a folder of PNG frames. The output's extension picks the container:

| Extension | Video | Audio |
| --- | --- | --- |
| `mp4`, `mov`, `mkv` | H.264 | AAC-LC 320k stereo 48 kHz |
| `webm` | VP9 | Opus 128k |
| `gif`, `webp` | animated, loop forever | none |
| `wav`, `opus`, `s16` | none | see [`audio.md`](audio.md) |

Video output is limited-range BT.709, tagged so. Rotation and non-square pixels are applied.

## Profiles

`pcod profiles` lists them. Default is `balanced`.

- `fast`: H.264 on the GPU (VA-API, NVENC, VideoToolbox) at QP 25, on the CPU where no GPU encoder exists, saying why. Quality knob: `--qp`.
- `balanced`: our H.264 on the CPU, matched to x264 medium CRF 20. Quality knob: `--crf`.
- `best`: matched to x264 slow CRF 18. Quality knob: `--crf`.
- `.webm`: balanced is libvpx CRF 32, best is CRF 28; `--crf` takes libvpx's numbers.
- Light web: `--profile best --crf 23` (`.webm`: `--crf 40`).

`--crf` goes with balanced and best, `--qp` with fast. `--tune` takes film, animation, grain, stillimage, fastdecode or zerolatency.

## Size and rate

- `--size`: a width (height follows), `WxH` within 1% of the aspect, or `720p`, `1080p`, `1440p`, `2160p`. pcod never stretches.
- `--fps`: 1 to 240; integer, ratio (`24000/1001`) or decimal (`23.976`, `29.97`, `47.952`, `59.94` read as the exact NTSC ratios).
- GIF and WebP default to 800 px wide at 12 fps, or the source's where smaller.
- A folder of PNG frames needs `--fps`. RGBA frames keep alpha in `.webm`, `.gif`, `.webp`; H.264 lays it on black.

## Cutting with --from and --to

`--from` and `--to` take seconds, `m:ss` or `h:mm:ss`; `--to` defaults to the end. The window runs from the first frame at or after `--from` to the last before `--to`, the audio cut in sync, the output starting at 0.

- **Copies** (fast, GOPs kept): no video option, and the output container carries the source's codec (H.264, HEVC or VP9). Only the frames before the first keyframe and after the last keyframe in the window are re-encoded. pcod says how many frames it copied and re-encoded.
- **Re-encodes the whole window**: any video option (`--profile`, `--size`, `--fps`, `--crf`, `--qp`, `--tune`), a WebM from H.264, an MP4 or MOV from VP9 or from an MKV or WebM source, or a source whose edges our encoders cannot match.
- `--to` not after `--from`, or `--from` at or past the end, is refused.
- `--tune animation` re-encodes into mp4, mov, mkv; any other tune with a range is refused.

## Tracks

- With no video option the video is copied untouched, index first (faststart).
- `--no-audio` writes no audio track.
- `--add-audio bed.wav` keeps the video and first audio track and adds the WAV as another AAC track (160 kbps unless `--audio-bitrate`), cut to the shorter; `--title Voice,Music` names the tracks. `.mp4` or `.mov` only; `.webm` is refused.
- `--audio-bitrate` 32 to 512 kbps.

## Loudness while converting

`--loudness youtube` (-14 LUFS, -2 dBTP) or a number from -70 to -5, `--true-peak` for the ceiling (default -1), `--linear` to keep dynamics. See [`audio.md`](audio.md).

## Join

`pcod join a.mp4 b.mp4 c.mp4 out.mp4`. When every clip matches the first (codec, size, rate, colour, headers, audio) it is a stream copy; otherwise, or with any video option, it re-encodes to the first clip's size and rate, letterboxing other shapes. It prints which, and why.

## Scripting

`--json` prints one manifest: each file's sha256 and size, the parameters, the time taken. Existing outputs need `--force`.
