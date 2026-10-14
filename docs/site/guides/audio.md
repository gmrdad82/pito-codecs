---
title: Audio as PCM
description: Decode any time range of an audio track to PCM, stream it in one-second blocks, or mix several tracks.
sidebar:
  order: 4
---

The `media` front decodes audio tracks to 32-bit float PCM at the rate and channel count you ask for, resampling and downmixing on the way. The result is an `AudioBuffer` with `samples` (interleaved), `rate` and `channels`.

## Which codecs

AAC-LC, Opus (SILK, CELT, hybrid and multistream), AC-3, E-AC-3, MP3, MP2, MP1 and PCM, from MP4, MOV, MKV and WebM. `AudioTrack::native()` says whether pito-codecs decodes a track itself. A track in another codec (FLAC, Vorbis, DTS, ALAC) decodes only through the user's ffmpeg, when the app's `Prefs` pick it.

```rust
use pito_codecs::media::{Prefs, probe};

let info = probe("tracks.mkv".as_ref(), &Prefs::default())?;
for t in &info.audio {
    println!("track {}: {} {} ch {} Hz, ours: {}", t.index, t.codec, t.channels, t.rate, t.native());
}
```

```text
track 0: ac3 2 ch 48000 Hz, ours: true
track 1: mp3 2 ch 48000 Hz, ours: true
```

Track numbers count audio tracks only, from 0.

## A time range

`audio_pcm_range(path, track, from, to, rate, channels, prefs)` returns the samples from `from` to `to` seconds, sample-exact. A range past the end of the track returns only the samples it has, so a buffer shorter than asked means the track ended. `audio_pcm` decodes a whole track to mono or stereo.

```rust
use pito_codecs::media::{Prefs, audio_pcm_range};

let slice = audio_pcm_range("tracks.mkv".as_ref(), 0, 2.0, 4.0, 48_000, 2, &Prefs::default())?;
```

```text
track 0, 2 s to 4 s: 96000 samples a channel at 48000 Hz, 2 channels
```

## A stream of blocks

`audio_stream` decodes from a time onwards and hands the audio over in one-second blocks as it goes, which suits a transcriber or a player. It ends at the track's end. Here, track 1 as 16 kHz mono, as speech recognisers want it:

```rust
use pito_codecs::media::{Prefs, audio_stream};

let stream = audio_stream("tracks.mkv".as_ref(), 1, 0.0, 16_000, 1, &Prefs::default())?;
for block in stream {
    let block = block?;
}
```

```text
track 1 as 16 kHz mono: 10 blocks, 160000 samples
```

## Several tracks mixed

`audio_mix_stream(path, gains, from, rate, prefs)` mixes every audio track with a gain each (by track number; a missing gain is 1.0, a gain of 0 leaves the track out) into stereo blocks. `audio_mix` does the same for a range in one buffer.

```rust
use pito_codecs::media::{Prefs, audio_mix_stream};

for block in audio_mix_stream("tracks.mkv".as_ref(), &[1.0, 0.5], 5.0, 48_000, &Prefs::default())? {
    let block = block?;
}
```

In MKV and WebM, ranges and streams seek through the file's Cues, so a slice late in a long recording reads only the clusters around it.

## Speed

![Audio in times real time, pito-codecs against ffmpeg](../images/chart-audio.png)

Ten minutes of 5.1 AC-3 at 448 kbps decode in 0.56 to 0.59 s on one core, and ten minutes of stereo MP3 at 192 kbps in 0.32 s on one thread, level with ffmpeg; through `audio_pcm_range` to stereo both run about twice as fast as ffmpeg.

To write audio, [export an edit with audio](../export/#with-your-own-audio); the AAC and Opus encoders are also there on their own in `pito_codecs::aac` and `pito_codecs::opus::enc`.
