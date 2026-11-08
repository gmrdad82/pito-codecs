# Audio: loudness, audio alone, synth, spectrum

## Measure loudness

```sh
pcod loudness trailer.mp4
pcod loudness trailer.mp4 --track 1 --json
```

Measures one audio track (`--track`, first by default) per ITU-R BS.1770-4 and EBU R128: integrated loudness, loudness range, true peak, sample peak, short-term and momentary maxima. Reads a video, M4A, MKA, WAV or MP3. `--json` gives the unrounded numbers.

## Level loudness

On a convert: `--loudness youtube` (-14 LUFS under a -2 dBTP ceiling) or any target from -70 to -5.

```sh
pcod talk.mp4 upload.mp4 --loudness youtube
pcod mix.wav master.wav --loudness -14 --linear
```

- Two EBU R128 passes, then a lookahead true-peak limiter holds the ceiling (`--true-peak`, -1 dBTP default).
- `--linear` keeps the dynamics: gain to the target, then the limiter on peaks alone, so the result can land under the target.
- Encoded output (AAC, Opus) is checked after a trial encode and the ceiling lowered until the encoded audio stays under it. An output whose encoded audio still peaks over is not written.
- With no video option the video is copied untouched. pcod prints loudness before and after; `--json` carries both.
- Loud masters want 320 kbps or more.

## Audio alone

A `.wav` output is the source's first audio track decoded to 16-bit PCM (AAC, Opus, MP3, AC-3, E-AC-3, PCM; from a video, M4A, MKA, WAV or MP3).

```sh
pcod phone.mp4 engine.wav --from 0:12 --to 0:20
pcod talk.mp4 voice.opus --audio-bitrate 64
pcod talk.mp4 pcm.s16 --track 1 --rate 16000 --mono
```

- Stereo at the source's rate when it is 44.1 or 48 kHz, else 48 kHz. `--mono` one channel; `--rate 44100|48000` resamples (`.s16` or `-`: 8000 to 768000).
- `.opus`: `--audio-bitrate` 16 to 510. `.s16` or `-` writes raw PCM to stdout; `--track N` numbers streams as the container does.
- Video options are refused with an audio-only output.

## Synth: a test video

```sh
pcod synth test.mp4 --size 1280x720 --fps 30 --duration 5 --audio sine:440,silence
```

Colour bars, a moving box, a frame counter drawn in digits and as 24 bits, and tones, so every frame is identified by its pixels. Sizes are even, 128x96 to 8192x8192. `--audio` entries are `sine:<hz>` (-18 dBFS) or `silence`, one stereo track each. Deterministic: same command, same build, same bytes. Containers: mp4, mov, mkv (H.264, AAC), webm (VP9, Opus; 48000 Hz only). Refuses an existing output without `--force`.

## Spectrum: a spectrogram PNG

```sh
pcod spectrum clip.mp4 spec.png --max-hz 12000 --width 1200 --height 360 --floor-db -90 --track 0
```

The track is mixed to mono and read at its own rate. Brightness is dB against a full-scale sine from 0 down to `--floor-db` (-200 to -6); frequency is linear from 0 to `--max-hz`. Input: MP4, MOV, M4A, MKV, MKA, WebM, WAV, MP3, Ogg Opus. `--json` prints size, length and the decode, draw and PNG times. Existing PNG refused without `--force`.
