# Audio: loudness, de-click, audio alone, test video, spectrogram

`--track` counts differently by command. `loudness` and `spectrum` count audio tracks from 0. Convert's `--track` (for `.s16` or `-`, and for `--declick`) numbers streams as the container does, so in an MP4 with the video first the first audio track is `1`; a number with no audio stream is refused and the line names the streams that exist.

## Measure loudness

```sh
pcod loudness clip.mp4
pcod loudness clip.mp4 --track 0 --json
```

Measures one audio track per ITU-R BS.1770-4 and EBU R128: integrated loudness (LUFS), loudness range (LU), true peak and sample peak (dBTP, dBFS), and the short-term (3 s) and momentary (400 ms) maxima. The file is a video, M4A, MKA, WAV or MP3. `--json` gives unrounded numbers plus `max_volume` and `duration`; a silent track reads `null` for integrated, peaks and maxima.

## Level loudness

```sh
pcod clip.mp4 upload.mp4 --loudness youtube
pcod bed.wav master.wav --loudness -14 --linear
pcod clip.mp4 voice.opus --loudness -16 --true-peak -1.5
```

- `--loudness youtube` is -14 LUFS under a -2 dBTP ceiling; any number from -70 to -5 is a target. Two EBU R128 passes, then a true-peak limiter holds the ceiling (`--true-peak`, -1 dBTP by default).
- `--linear` keeps the dynamics: gain to the target, then the limiter on peaks alone, so the result can land under the target.
- pcod prints `loudness before` and `loudness after` (in `--json` too). With no video option the video is copied untouched. `pcod frames` takes the same three options.

## De-click

```sh
pcod clip.mp4 clean.mp4 --declick
pcod bed.wav clean.wav --declick strong --declick-list
pcod clip.mp4 clean-upload.mp4 --declick --loudness youtube
pcod clip.mp4 clean-track.mp4 --declick --track 1 --json
```

- `--declick` finds mouth clicks, tongue smacks and lip pops (spikes of 1 to 6 ms well above the track's own high-passed level around them) and repairs each by interpolating across it. Speech, plosives, fricatives and a word's onset are left as they were: only a lone transient is touched, every sample outside a repair is unchanged, and a track with no click comes out sample for sample.
- `--declick light|medium|strong` sets how far above its surroundings a spike must stand: 20, 14 or 9 dB (medium by default). Try `strong` when a click you hear is not reported; `light` when it touches too much.
- It works on a video's audio (the video is copied untouched, the audio re-encoded), a WAV or an `.opus`, from the first audio stream or the `--track N` you give (numbered as for `.s16`). A video takes it whole: cut first, then de-click; a `.wav` takes `--from` and `--to`.
- With `--loudness` the de-click runs first, then the level.
- pcod prints `declick: N clicks repaired`, and `--json` has `declick.repaired`; `--declick-list` adds each click's time and level (the high-passed peak in dBFS, and how far above its surroundings in dB), so a person can listen at those times.
- Not with `join`, `--no-audio`, `--add-audio`, a `.gif`, a `.webp` or a `.s16`.

## Audio alone

```sh
pcod clip.mp4 engine.wav --from 0:01 --to 0:03
pcod clip.mp4 mono.wav --mono --rate 44100
pcod clip.mp4 voice64.opus --audio-bitrate 64
pcod clip.mp4 pcm.s16 --track 1 --rate 16000 --mono
pcod clip.mp4 - --track 1 --mono > pcm-stdout.s16
```

- `.wav`: the first audio track as 16-bit PCM, stereo at the source's rate when it is 44100 or 48000 Hz, else 48000. `--mono` for one channel; `--rate` 44100 or 48000.
- `.opus` or `.ogg`: Ogg Opus at 48000 Hz, 64k stereo or 32k mono; `--audio-bitrate` 16 to 510.
- `.s16` or `-` (stdout): one stream as raw 16-bit little-endian PCM from `--from` to its end; `--track`, `--mono` and `--rate` (8000 to 768000, the stream's own by default).
- A video option (`--size`, `--profile`, `--fps`) with an audio-only output is a usage error.

## Test video

```sh
pcod synth test.mkv --size 1280x720 --fps 30000/1001 --duration 0:04 --audio sine:440,silence
pcod synth test.webm --size 320x240 --fps 24 --duration 2 --audio sine:880
```

Seven colour bars with a moving box, and under them the frame number in white digits and as 24 bits, so every frame is told apart by its pixels. `--audio` takes `sine:<hz>` (-18 dBFS) or `silence`, one stereo track each, at `--rate` (48000 by default; WebM takes only 48000). Sizes are even, from 128x96 to 8192x8192. mp4, mov and mkv carry H.264 and AAC, webm VP9 and Opus. The same command on the same build writes the same bytes. An existing output needs `--force`.

## Spectrogram

```sh
pcod spectrum clip.mp4 spec.png
pcod spectrum clip.mp4 spec-wide.png --max-hz 8000 --width 1600 --height 400 --floor-db -80 --track 0 --json
```

The track is mixed to mono and read at its own rate. Brightness is dB against a full-scale sine, from 0 down to `--floor-db` (-90 by default, -200 to -6); frequency is linear from 0 to `--max-hz` (12000). The PNG is 1200x360 by default; `--width` is 320 to 16384. The file is a video, M4A, MKA, WebM, WAV, MP3 or Ogg Opus. `--json` prints the size, length and the decode, draw and PNG times.
