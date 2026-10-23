---
title: "Convert, join, probe, decode, encode, extract frames and draw filmstrips with pcod"
description: "Convert a video or a folder of PNG frames to MP4, MOV, MKV, WebM, GIF or WebP, join clips, look inside a file, decode it on the CPU or the GPU, encode it to H.264 or HEVC, take stills, contact sheets and frames on a cadence as PNG, JPEG or WebP, and draw filmstrips from the command line."
sidebar:
  order: 1
---

pcod is a small command line over the library. `pcod <input> <output>` converts a file or a folder of PNG frames, `join` stitches clips, `probe` shows what is inside a file, `loudness` measures an audio track per EBU R128, `decode` decodes its whole video track and reports the speed, `encode` decodes, encodes and muxes a new MP4, copying the audio, `frames` encodes a folder of PNG frames and a WAV into an MP4, and `stills` writes stills of a video, every frame of a window or one on a cadence, as PNG, JPEG or WebP, and a contact sheet, and `filmstrip` draws tiles of a video on a cadence into one image. Every option is in the [command reference](../../reference/cli/).

The outputs below are from a 10-second 1080p60 H.264 clip cut from NASA's public-domain "A Message of Resilience from the International Space Station", on a 16-core CPU and an AMD card through VA-API. Your speeds will differ.

## Convert

`pcod <input> <output>` converts a video file, or a folder of PNG frames, into the container its output's extension names, with sensible settings and nothing else to learn. `pcod convert <input> <output>` is the same command spelled out.

```sh
pcod master.mov site.mp4 --profile best --crf 23    # a light H.264 for the web
pcod master.mov hero.webm --size 1080p             # VP9 at 1080p
pcod recording.mkv draft.mp4 --profile fast        # a quick draft on the GPU
pcod clip.mp4 readme.gif --size 640 --fps 12       # a looping README GIF
pcod clip.mp4 readme.webp                          # an animated WebP
pcod frames/ trailer.webm --fps 60                 # PNG frames to a transparent WebM
pcod join intro.mp4 scene.mp4 outro.mp4 film.mp4   # one file, copied when the clips match
pcod talk.mp4 upload.mp4 --loudness youtube        # -14 LUFS, the video copied
pcod talk.mp4 moment.mp4 --from 1:02 --to 1:10     # eight seconds, GOPs copied, edges re-encoded
pcod talk.mp4 moment.gif --from 1:02 --to 1:05     # three seconds as a looping GIF
pcod phone.mp4 engine.wav --from 0:12 --to 0:20     # eight seconds of the audio as a WAV
pcod mix.wav master.wav --loudness -14 --linear     # -14 LUFS, the dynamics kept
```

- **Containers.** `mp4`, `mov` and `mkv` get H.264 and AAC, `webm` gets VP9 and Opus, `gif` and `webp` are animated and loop forever, with no audio. Every video output is limited-range BT.709 and tagged so: BT.601, full-range and untagged sources are converted (untagged is read as BT.709), rotation and non-square pixels are applied.
- **Profiles.** `balanced` (the default) and `best` are our H.264 encoder on the CPU at quality settings matched to x264's medium CRF 20 and slow CRF 18, measured by SSIM; `fast` is H.264 on the GPU (VA-API, NVENC, VideoToolbox) at QP 25, and on the CPU where no GPU encoder is found, saying why. For `.webm`, balanced matches libvpx-vp9 at CRF 32 and best at CRF 28. `--crf` takes x264's numbers (libvpx's for `.webm`) and goes with balanced and best, `--qp` with fast. `pcod profiles` lists them all.
- **Size and rate.** `--size` takes a width (the height follows the aspect), `WxH` within 1% of the aspect, or `720p`, `1080p`, `1440p` or `2160p`; pcod never stretches a picture. Resizing is a Lanczos filter in linear light, with premultiplied alpha. `--fps` sets the frame rate: an integer, a ratio such as `24000/1001`, or a decimal, with `23.976`, `29.97`, `47.952` and `59.94` read as the exact `24000/1001`, `30000/1001`, `48000/1001` and `60000/1001`. MP4 and MOV keep the rate as an exact timescale (24000/1001 is 1536000 ticks a second, 64064 a frame), so a 23.976 film never drifts against its 48 kHz audio. GIFs and WebPs are 800 px wide at 12 fps by default, or the source's width and rate where smaller.
- **Alpha.** A folder of RGBA PNG frames, or a WebM with alpha, keeps its alpha in `.webm` (VP9 alpha, read by browsers and ffmpeg), `.gif` and `.webp`; in H.264 it is laid onto black. A folder needs `--fps`.
- **Audio.** AAC-LC 320k stereo at 48 kHz in `mp4`, `mov` and `mkv` (an AAC source is copied when nothing changes its audio), Opus 128k in `webm` (an Opus source is copied). `--audio-bitrate` sets the rate, 32 to 512, and with no video option the video is copied untouched.
- **Tracks.** `--no-audio` writes no audio track; with no video option the video is copied sample for sample, the index first, as `ffmpeg -map 0:v -c copy -movflags +faststart` does. `--add-audio bed.wav` keeps the video and the first audio track of an MP4 or MOV as they are and adds the WAV as another AAC track (160 kbps unless `--audio-bitrate`), cutting the output to the shorter of the video and the WAV; `--title Voice,Music` names each audio track in `trak/udta/name`, the box ffmpeg's `-metadata:s:a:N title=` writes and ffprobe reads as the `name` tag. A `.webm` output is refused with `--add-audio`.
- **Loudness.** `--loudness youtube` levels the audio to -14 LUFS under a -2 dBTP true-peak ceiling in two EBU R128 passes; any target from -70 to -5 LUFS works too, with `--true-peak` for the ceiling (-1 dBTP by default). When the gain to the target would push peaks past the ceiling, a lookahead true-peak limiter holds them, and pcod raises the gain until the limited audio reaches the target, which flattens a mix whose loud passages hit the ceiling. `--linear` keeps the dynamics instead: the gain to the target, then the limiter on the peaks alone, so the result can land under the target. An encoded output (AAC or Opus) is checked after a trial encode and the ceiling lowered, up to eight times, until the encoded audio stays under it; an output whose encoded audio still peaks over the ceiling is not written. With no video option the video is copied untouched. pcod prints the loudness before and after, and `--json` carries both (`"linear"` in its loudness parameters).
- **Ranges.** `--from` and `--to` (seconds, `m:ss` or `h:mm:ss`) cut a window of a video into any output: the first frame at or after `--from` to the last before `--to`, the audio cut to the same window and in sync, the output starting at 0. `--to` defaults to the end. With no video option and a container that carries the source's codec (H.264, HEVC or VP9), the cut copies every whole GOP inside the window and re-encodes only the frames from `--from` to the first keyframe and from the last keyframe to `--to`, on the CPU (on the GPU for HEVC); it says how many frames it copied and re-encoded. A video option (`--profile`, `--size`, `--fps`, `--crf`, `--qp`, `--tune`) or a WebM from H.264 re-encodes the whole window, and so does an MP4 or MOV from VP9 or from an MKV or WebM source, as a plain convert does, and a source our encoders can't match at the edges (10-bit or 4:2:2 H.264, 4:4:4 or 12-bit HEVC, VP9 past profile 0). The audio is AAC, or Opus in `.webm`, encoded from the window. GIFs and WebPs are decoded straight from the window. `--tune animation` re-encodes the whole window into `mp4`, `mov` or `mkv`; any other tune, or `animation` into `.webm`, is refused with a range. A WebM with alpha is decoded with its alpha from the first frame of the window to the last, so `.webm`, `.gif` and `.webp` keep the alpha, and its audio is re-encoded from the window. A folder of PNG frames takes its frames in the window at its `--fps`. `--to` not after `--from`, or `--from` at or past the end, is refused; `join` takes whole clips.
- **Audio alone.** A `.wav` output is the source's first audio track (AAC, Opus, MP3, AC-3, E-AC-3 or PCM) decoded to 16-bit PCM; the source can be a video, an M4A or MKA, a WAV or an MP3. It comes out stereo at the source's rate when it is 44.1 or 48 kHz and 48 kHz otherwise. `--from` and `--to` cut a range (seconds, `m:ss` or `h:mm:ss`), `--mono` writes one channel, `--rate 44100` or `--rate 48000` resamples, and `--loudness` levels the range itself, measured in the output's own channels and rate. Video options are refused.
- **Join.** `pcod join` stitches clips in order. When every clip matches the first (codec, size, rate, colour, headers and audio) it is a stream copy; otherwise, or with any video option, it re-encodes them to the first clip's size and rate, letterboxing a clip of another shape. It says which, and why.
- **Existing outputs** are refused unless `--force` is given. pcod writes a hidden partial file beside the output and renames it into place at the end, so a failed or stopped run leaves nothing behind.

### Scripts and agents

With stderr a pipe or a file, pcod draws no progress line, so stdout carries only the result: the output's path (and how a join was made), or one JSON manifest with `--json`, with each file's sha256 and size, the parameters and the time it took. `--no-progress` turns the line off on a terminal. `pcod probe <input> --json` prints what pcod reads from a file, with the same keys as the probe of pconv, the converter pcod replaced, plus `chroma` ("4:2:0", "4:2:2", "4:4:4", "4:4:0" or "4:0:0"), `interlaced` and `variant`, the plain name of the video such as "10-bit 4:2:2 H.264". All three are `null` without video; when the stream's parameters can't be read, `chroma` is `null` and `interlaced` reads `false`.

Exit codes: `0` it worked; `1` refused or failed, with one line on stderr starting `pcod: `; `2` the command line is wrong, answered the way clap answers; `128` plus the signal number when Ctrl-C, SIGTERM or SIGHUP stopped it (130 for Ctrl-C on Windows).

## Loudness

`loudness` measures one audio track (`--track`, the first by default) at its own rate and channels, per ITU-R BS.1770-4 and EBU R128: the gated integrated loudness, the loudness range, the true peak (4x oversampled below 88.2 kHz), the sample peak, and the short-term (3 s) and momentary (400 ms) maxima. It reads a video, an M4A or MKA, a WAV or an MP3, so a delivery can be checked as it will ship. `--json` prints the same numbers, unrounded.

```sh
pcod loudness trailer.mp4
```

```text
audio           track 0, aac 2 ch 48000 Hz, 40.00 s
integrated       -14.03 LUFS
range             12.37 LU
true peak         -1.06 dBTP
sample peak       -1.06 dBFS
short-term max    -6.64 LUFS
momentary max     -4.49 LUFS
```

That trailer came from `pcod frames` with `--loudness -14`; its AAC holds the -1 dBTP ceiling.

Five- and six-channel tracks weigh the surrounds by 1.41 and leave out the LFE, read as the fourth channel.

## Probe

```sh
pcod probe clip.mp4
```

```text
container  MP4
duration   10.0 s
video      h264 High 1920x1080 59.94 fps
variant    8-bit 4:2:0 H.264
audio      aac 2 ch 48000 Hz
```

## Decode

`--route` picks the GPU (the default) or the CPU; `--threads` caps the CPU threads.

```sh
pcod decode clip.mp4 --route cpu
pcod decode clip.mp4 --route gpu
```

```text
route   CPU (16 threads)
decoded 600 frames in 0.88 s: 686 fps
route   GPU (VA-API, radeonsi)
decoded 600 frames in 0.48 s: 1260 fps
```

Without a GPU decoder for the file's codec, `--route gpu` decodes on the CPU and says why on a `note` line.

## Encode

`--codec` is `h264` (the default) or `hevc`, `--crf` sets the quality (23 by default; lower is better and larger), and `--route` picks the GPU or the CPU for both the decode and the encode. On the GPU, frames go from the decoder to the encoder without leaving the card.

```sh
pcod encode clip.mp4 out.mp4 --codec hevc --route gpu
```

```text
decode  GPU (VA-API, radeonsi)
encode  HEVC GPU (VA-API, radeonsi)
encoded 600 frames in 1.14 s: 526 fps
wrote   out.mp4 (2.7 MB)
```

HEVC encodes on the GPU only. Asked for it on the CPU, pcod encodes H.264 and says so:

```sh
pcod encode clip.mp4 out.mp4 --codec hevc --route cpu
```

```text
decode  CPU (16 threads)
note    HEVC encodes on the GPU only; encoding H.264 on the CPU instead
encode  H.264 CPU (16 threads)
encoded 600 frames in 6.29 s: 95 fps
wrote   out.mp4 (4.2 MB)
```

## Frames to an MP4

`frames` encodes a folder of PNG frames, such as a game's rendered trailer, and an optional WAV into an MP4: H.264 (or HEVC with `--codec hevc`), 8-bit 4:2:0 BT.709, AAC at 48 kHz stereo, with the index at the front (faststart). The frames are the folder's `.png` files in name order (`000001.png`, `000002.png`, ...), all one even size. `--fps` takes an integer, a ratio such as `24000/1001` or a decimal (`23.976`, `29.97`, `47.952` and `59.94` are the exact NTSC ratios). The PNGs are decoded on every core ahead of the encoder. The encode goes to the GPU first and falls back to H.264 on the CPU, saying why on a `note` line.

```sh
pcod frames frames trailer.mp4 --fps 60 --audio trailer.wav
```

```text
frames  600 PNG 1920x1080 at 60 fps (10.00 s), 16 decode threads
audio   trailer.wav: 10.00 s, AAC 48 kHz stereo at 192 kbps
encode  H.264 GPU
encoded 600 frames in 1.07 s: 560 fps
wrote   trailer.mp4 (7.5 MB)
```

An existing output is refused unless `--force`; the MP4 is written to a hidden partial file and renamed into place at the end, so a failed run keeps the old one. `--crf 0` is near-lossless, not bit-exact (57 to 68 dB luma PSNR measured): pcod's encoders have no lossless mode.

`--audio-bitrate` sets the AAC rate: 192 kbps by default, 320 kbps with `--loudness`. `--loudness` levels the WAV, with `--true-peak` and `--linear` as for a conversion, and the ceiling holds for the AAC in the MP4, not just for the WAV: AAC adds 1 to 2 dB of true peak, so pcod encodes a trial, lowers the limiter's ceiling until the decoded AAC stays under it, prints the MP4's loudness at the end, and removes the MP4 with an error if its AAC still peaks over the ceiling. On a dense, limited master the lower rates cost loudness: AAC rings more at 192 kbps, so the ceiling holds only with the level pulled down. A generated 20 s mix of kicks, bass and pink noise, levelled with `--loudness -14 --true-peak -1 --linear`, lands at -17.6 LUFS at 192 kbps, -15.9 at 256 and -13.9 at 320, each under -1.3 dBTP. Use 320 kbps or more for loud masters.

These are the same 10 seconds of the NASA clip, written out as 1080p PNG frames. The WAV can be 8, 16, 24 or 32-bit PCM or float, at any rate, mono, stereo or more channels. pcod converts it to 48 kHz stereo and cuts or pads it with silence to the length of the video. Without `--audio`, the MP4 carries a silent stereo track. A missing frame number, or a frame of another size, stops the encode:

```text
pcod: gap: the frames jump from 000001.png to 000003.png
```

## Stills and contact sheets

`stills` takes N evenly spaced stills (`--count`, 12 by default) or stills at given times (`--at`, comma separated: seconds, `m:ss` or `h:mm:ss`), each scaled to `--height` (360 by default) keeping its shape. `--out` writes `still-001.png`, `still-002.png`, ... into a folder, `--sheet` writes one contact sheet of `--rows` by `--cols` stills on black with `--pad` pixels (8 by default) between and around them, and you can give both. Without `--rows` and `--cols` the sheet is about square; with both and no `--count`, it takes rows times columns stills. Each still is the exact frame at its time; `--seek keyframe` takes the keyframe at or before it instead, which is faster on long GOPs. MP4, MOV, MKV and WebM work, on the GPU route first.

```sh
pcod stills clip.mp4 --count 24 --cols 6 --sheet sheet.png
```

```text
     1.250 s  640x360
     ...
sheet  6x4 stills, 3896x1480  sheet.png
24 stills in 0.37 s
```

`--json` prints one line instead, with each still's time (the frame shown), the time asked, its size and its PNG's path (null without `--out`), and the sheet:

```sh
pcod stills clip.mkv -n 6 --cols 3 --height 240 --seek keyframe --sheet sheet.png --json
```

```text
{"file":"clip.mkv","height":240,"seek":"keyframe","stills":[{"index":0,"time":0.000000,"asked":1.666667,"width":426,"height":240,"path":null},...],"sheet":{"path":"sheet.png","width":1310,"height":504,"rows":2,"cols":3,"pad":8},"seconds":0.021}
```

A time past the end of the video stops with one line, `pcod: clip.mp4: 65.000 s is past the end of the video (20.000 s)`.

### Frame ranges

`--from` and `--to` write every frame of a window instead: the first frame at or after `--from` to the last before `--to` (the end by default), into `--out` as `000000.png`, `000001.png`, ..., the names `pcod frames` reads, so a range goes back into an MP4 with `pcod frames`; like other stills a range is 360 pixels high by default, so give `--size` (the source's own size, `1920x1080` for a 1080p video) for a round trip at full size. The frames come at the source's rate, or at `--fps` with the frame whose start is nearest each slot (ties go to the earlier frame, so a `pcod --fps` re-encode of the same window, which keeps the frame each slot rounds to, can differ by one frame at some slots); each is scaled to `--size` (a width, `WxH` within 1% of the aspect, or `720p` to `2160p`) or to `--height` keeping its shape. The video is decoded once from the keyframe before `--from`, on the GPU route first (`--route cpu` for the CPU), and each PNG is written as its frame decodes, on every core, so memory stays flat however long the range. A range of more than 100,000 frames is refused unless `--force`; a folder that already holds numbered frames is refused too unless `--force`, which replaces them, and a folder with any other PNG is always refused for PNG frames, since `pcod frames` would read it too. A range that fails partway, on a full disk or an unwritable folder, stops at once and removes the frames it wrote. `--count`, `--at`, `--sheet` and `--seek` don't go with a range.

```sh
pcod stills clip.mp4 --from 1.0 --to 1.5 --out frames
pcod stills clip.mp4 --from 0:10 --to 0:20 --fps 12 --size 640 --out reel --json
pcod frames frames back.mp4 --fps 24
```

`--every` takes one frame per interval instead of `--fps` (`2s`, `500ms`, `1.5` or `0:01`; the frame nearest each slot, on the source's timeline from the start of the window), and `--fps` or `--every` with no `--from` and `--to` covers the whole video; `--from 0` takes every frame of it. `--format` writes the frames as `png` (the default), `jpeg` (`jpg`, baseline 4:2:0; a WebM with alpha is flattened onto black, as `filmstrip` flattens it onto `--background`) or `webp` (lossy), named `000000.jpg` or `000000.webp`, with `--quality` from 1 to 100 (85 by default) for JPEG and WebP; PNG ignores `--quality` and says so once. `--format` and `--quality` need `--out`; a contact sheet stays PNG.

```sh
pcod stills clip.mp4 --every 2s --size 1080p --out posters
```

It prints how many frames it wrote, their size and rate, the window and the speed in frames per second with the time to the first image; `--json` prints the same as one line: `file`, `from`, `to`, `first` and `last` (the times of the first and last frame taken), `count`, `fps`, `rate`, `every` (the interval in seconds, or null), `format`, `quality` (null for PNG), `width`, `height`, `dir`, `digits`, `first_png` (the seconds to the first image), `seconds`, `frames_per_second` and `files` (each frame's name, in order), with `expected`, the frame count the probed length promised. A stream that ends before its probed length stops at its last real frame and says so, `note: the stream ends before its probed length: 59 of 60 frames written`; a window the stream never reaches is refused, `the stream ends before 9.000 s`.

## Filmstrips

`filmstrip` draws tiles of a video into one image: a tile every `--every` (`2s`, `500ms`, `1.5` or `0:01`, as for `stills`) from the start of the window, or `--count` tiles (10 by default) spaced evenly over it, with `--from` and `--to` narrowing the window. Each tile is scaled down to `--height` (90 by default) keeping its shape (never up: a shorter video keeps its own size), all from one pass over the video, on the GPU route first (`--route cpu` for the CPU). By default each tile is the keyframe at or before its time, which is fast and can sit before `--from`; its label shows the time it really shows. `--seek exact` takes the exact frame instead. The tiles sit in one row, or wrap into a grid with `--wrap` tiles per row or at `--max-width` pixels, with `--gap` pixels (4 by default) of `--background` (`#000000`) between and around them, and `--timecodes` writes each tile's time under it. The output's extension picks PNG, JPEG or WebP. A filmstrip over 268 megapixels is refused, and so is an existing output unless `--force`.

```sh
pcod filmstrip clip.mp4 strip.png --every 2s --seek exact --timecodes
```

```
strip.png 824x120, 5 tiles of 160x90 at 0:00 0:02 0:04 0:06 0:08
```

`--json` prints the same as one line: `file`, `output`, `width`, `height`, `tile_width`, `tile_height`, `tiles`, `cols`, `rows`, `seek`, `times` (the time each tile shows), `asked` (the time each tile was asked for), `bytes` and `seconds`.

## When something fails

A file pcod can't read, or a failing route, ends with one line on stderr and exit code 1:

```text
pcod: missing.mp4: No such file or directory
```

A command line it can't parse prints clap's error and usage on stderr with exit code 2; `pcod --help` and `pcod help <command>` print the full help.

pcod's output is plain text for people; conversions, joins, `probe`, `stills` and `filmstrip` also print JSON with `--json`. For anything beyond these commands, [use the library](../library/).
