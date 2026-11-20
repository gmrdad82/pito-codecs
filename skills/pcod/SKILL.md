---
name: pcod
description: >
  Use pcod, pito-codecs' command line, for any video, audio or still-image job: convert a video to MP4,
  MOV, MKV, WebM, GIF or WebP, cut a clip with --from and --to, join clips, resize or change the frame
  rate, extract audio to WAV, Opus or raw PCM, measure or level loudness (LUFS, -14 for YouTube), draw a
  spectrogram, take stills, a contact sheet, a frame range or a filmstrip, crop a frame, turn a folder of
  PNG frames into an MP4, write a test video, probe a file, or time GPU and CPU decode and encode
  (VA-API, NVENC, VideoToolbox). Triggers: pcod, convert video, transcode, trim, cut, join, concat, gif,
  webp, webm, mp4, thumbnail, poster, screenshot of a video, contact sheet, filmstrip, extract frames,
  frames to video, loudness, LUFS, normalize audio, spectrogram, probe, ffprobe, ffmpeg replacement,
  HEVC, H.264, VP9, VA-API.
---

# pcod

pcod is pito-codecs' command line: video, audio and still-image conversion in pure Rust, on the GPU first and its own CPU code second, with no ffmpeg inside. `pcod` alone prints one line per command; `pcod <command> --help` is the full list of flags for one command.

## Commands

| Command | Does | Guide |
| --- | --- | --- |
| `pcod <input> <output>` or `pcod convert` | convert, cut, resize, GIF, WebP, WebM, audio alone, tracks | [`convert.md`](convert.md) |
| `pcod join` | clips in order into one file | [`convert.md`](convert.md) |
| `pcod profiles` | what `fast`, `balanced` and `best` do | [`convert.md`](convert.md) |
| `pcod probe` | what is in a video, audio or image file | below |
| `pcod loudness`, `pcod spectrum`, `pcod synth` | measure loudness, draw a spectrogram, write a test video | [`audio.md`](audio.md) |
| `pcod stills`, `pcod filmstrip`, `pcod frames` | pictures out of a video, and PNG frames back into one | [`stills.md`](stills.md) |
| `pcod decode`, `pcod encode` | GPU and CPU routes and their speed | [`gpu.md`](gpu.md) |
| `pcod skill` | `install`, `status` or `uninstall` this skill in the agents' skill folders | below |

`pcod skill install` writes this skill as files into `~/.claude/skills/pcod` and `~/.agents/skills/pcod` (and Codex's and Pi's when those exist); run it again after updating pcod, and `pcod skill status` says whether it matches.

Read the guide before the job. The examples in every guide run as written, in order, on this test clip:

```sh
pcod synth clip.mp4 --size 640x360 --fps 30 --duration 6 --audio sine:440
pcod probe clip.mp4
pcod probe clip.mp4 --json
```

## Rules

- An existing output is refused (`pcod: <path> already exists; use --force to replace it`, exit 1). `--force` replaces only that file: an output that is also an input is refused even with it. pcod writes a hidden partial file and renames it at the end, so a failed run leaves nothing at the output path.
- Exit codes: `0` worked; `1` refused or failed, one line on stderr starting `pcod: `; `2` the command line is wrong (clap prints `error:` and the usage); `128` plus the signal number when a signal stopped it. Check the code, then read the line.
- For scripts, `--json` (`convert`, `join`, `probe`, `loudness`, `spectrum`, `stills`, `filmstrip`) prints one JSON object on stdout; parse that, not the text. The progress line appears only when stderr is a terminal; `--no-progress` turns it off there.
- A `note` line says pcod did something other than what was asked, such as the GPU failing and the CPU taking over, or a cut re-encoding its whole window. Pass it on.
- pcod reads 8-bit 4:2:0 progressive H.264 and 8 or 10-bit 4:2:0 HEVC, VP9 and AV1. Any other variant (10-bit H.264, 4:2:2 or 4:4:4, interlaced H.264) and audio such as DTS or TrueHD is refused by name. Report it; no flag changes that.
- Quality by intent: `--profile fast` for a draft, the default `balanced`, `--profile best` for a keeper. Add `--crf` (balanced, best) or `--qp` (fast) only for a size or quality target; mixing them is a usage error.

## Probe

Text gives the container, duration, video line, `variant` (8-bit 4:2:0 H.264) and one line per audio track. `--json` adds `width`, `height`, `fps`, `frames`, `codec`, `rotation`, the colour fields, `audio_rate`, `audio_channels` and `alpha`; keys that do not apply are `null`. It also reads WAV, MP3, PNG, JPEG, WebP and GIF files.
