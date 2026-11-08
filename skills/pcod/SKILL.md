---
name: pcod
description: >
  Use pcod, pito-codecs' command line, for any video, audio or image-sequence work: convert a video
  to MP4, MOV, MKV, WebM, GIF or WebP, cut a clip with --from and --to, join clips, resize or change
  the frame rate, extract audio to WAV or Opus, level or measure loudness (LUFS, EBU R128), draw a
  spectrogram, take stills, a contact sheet, frames on a cadence or a filmstrip, turn PNG frames into
  an MP4, make a test video, probe a file, or decode and encode on the GPU (VA-API, NVENC,
  VideoToolbox). Triggers: pcod, convert video, transcode, trim, cut, join, concat, gif, webp, webm,
  mp4, thumbnail, screenshot of a video, contact sheet, filmstrip, extract frames, loudness, LUFS,
  normalize audio, spectrogram, probe, ffprobe, ffmpeg replacement, HEVC, H.264, VP9, VA-API.
---

# pcod Skill

pcod is the command line of [pito-codecs](https://github.com/gmrdad82/pito-codecs): pure Rust video, audio and image codecs, containers and GPU routes, with no ffmpeg inside. Reach for it instead of ffmpeg for the jobs below.

## Topic Guides

Read the matching guide before starting:

- [`convert.md`](convert.md) - converting, profiles, size and rate, cutting with `--from`/`--to`, join, tracks
- [`audio.md`](audio.md) - loudness measuring and levelling, audio alone (WAV, Opus, raw PCM), synth, spectrum
- [`stills.md`](stills.md) - stills, contact sheets, frame ranges, filmstrips, PNG frames back to an MP4
- [`gpu.md`](gpu.md) - routes, fallbacks, decode and encode, the 352x240 floor

## Command Discovery

The CLI's own help is the source of truth. Do not guess an option.

```sh
pcod --help
pcod <command> --help
pcod profiles
pcod --version
```

Commands: `convert` (the default, so `pcod in out` works), `join`, `profiles`, `probe`, `loudness`, `synth`, `spectrum`, `decode`, `encode`, `frames`, `stills`, `filmstrip`.

## Critical Safety Rules

- An existing output is refused unless `--force`. Never add `--force` to get past a refusal without checking the path is yours to replace. pcod writes a hidden partial file and renames it at the end, so a failed run leaves nothing behind.
- When scripting, use `--json` (`convert`, `probe`, `loudness`, `spectrum`, `stills`, `filmstrip`) and parse it. Never scrape the plain text. With stderr a pipe or a file there is no progress line; `--no-progress` turns it off on a terminal.
- Exit codes: `0` worked; `1` refused or failed, one line on stderr starting `pcod: `; `2` the command line is wrong; `128` plus the signal number when stopped by a signal. Check the code, then read the line.
- pcod never opens a window. Run it headless; do not wrap it in a GUI.
- Never quote speed (fps, seconds) from a busy machine. A run beside other jobs reads 10 to 50% slow; say so or re-run on a quiet one.
- Do not overwrite or delete the input. Write to a new path.
- A source pcod cannot decode is refused by name (10-bit and 4:2:2 H.264 and HEVC, DTS, TrueHD). Report that; do not retry with another route.

## Decision Framework

1. **Know the file.** `pcod probe <file> --json` first when its codec, size, rate or tracks matter.
2. **Pick the command.**
   - File to file, any container: `pcod in out` (the output's extension picks the container).
   - Several clips to one: `pcod join`.
   - A number about the audio: `pcod loudness`; a picture of it: `pcod spectrum`.
   - Pictures out of a video: `stills` (stills, sheets, frame ranges) or `filmstrip`.
   - Pictures into a video: `pcod frames dir/ out.mp4 --fps N` or `pcod dir/ out.webm --fps N`.
   - A known test input: `pcod synth`.
   - Measuring decode or encode speed: `decode`, `encode`.
3. **Pick quality by intent**, not by number: `--profile fast` for a draft, the default `balanced`, `--profile best` for a keeper. Add `--crf` or `--qp` only when asked for a size or quality target.
4. **Run it, check the exit code**, then report the path and what pcod said about the route and any `note` line.

## Example Requests

- "Make this MOV a web MP4" - `pcod master.mov site.mp4 --profile best --crf 23`
- "Make a README GIF of the first 3 seconds" - `pcod clip.mp4 readme.gif --from 0 --to 3 --size 640 --fps 12`
- "Cut 1:02 to 1:10" - `pcod talk.mp4 moment.mp4 --from 1:02 --to 1:10`
- "Level this to YouTube loudness" - `pcod talk.mp4 upload.mp4 --loudness youtube`
- "What loudness is this?" - `pcod loudness trailer.mp4 --json`
- "Pull the audio as a WAV" - `pcod phone.mp4 audio.wav`
- "Give me a contact sheet" - `pcod stills clip.mp4 --count 24 --cols 6 --sheet sheet.png`
- "Join these three" - `pcod join a.mp4 b.mp4 c.mp4 film.mp4`
- "What is in this file?" - `pcod probe clip.mp4 --json`

## Out of Scope

- Editing with overlays, captions or compositing: that is the library's `media` front, not pcod.
- Streaming, DRM, subtitles, and codecs pcod does not decode (DTS, TrueHD, 10-bit H.264 and HEVC).
- Changing pito-codecs' source: this skill is for using the installed `pcod`.
