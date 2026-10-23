use super::{Resampler, downmix_stereo, gain, mono, resample};
use crate::media::AudioBuffer;

const K: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn buffer(rate: u32, channels: u16, samples: Vec<f32>) -> AudioBuffer {
    let mut b = AudioBuffer::new(rate, channels);
    b.samples = samples;
    b
}

fn impulse(channels: u16, at: usize) -> AudioBuffer {
    buffer(
        48_000,
        channels,
        (0..channels as usize)
            .map(|c| if c == at { 1.0 } else { 0.0 })
            .collect(),
    )
}

#[test]
fn stereo_downmix_matches_ffmpeg_float_rematrix() {
    let table: [(u16, Vec<[f32; 2]>); 5] = [
        (3, vec![[1.0, 0.0], [0.0, 1.0], [K, K]]),
        (4, vec![[1.0, 0.0], [0.0, 1.0], [K, K], [0.5, 0.5]]),
        (5, vec![[1.0, 0.0], [0.0, 1.0], [K, K], [K, 0.0], [0.0, K]]),
        (
            6,
            vec![
                [1.0, 0.0],
                [0.0, 1.0],
                [K, K],
                [0.0, 0.0],
                [K, 0.0],
                [0.0, K],
            ],
        ),
        (
            8,
            vec![
                [1.0, 0.0],
                [0.0, 1.0],
                [K, K],
                [0.0, 0.0],
                [K, 0.0],
                [0.0, K],
                [K, 0.0],
                [0.0, K],
            ],
        ),
    ];
    for (channels, want) in table {
        for (c, w) in want.iter().enumerate() {
            let out = downmix_stereo(&impulse(channels, c));
            assert_eq!(out.channels, 2);
            assert_eq!(out.samples, w, "{channels} channels, channel {c}");
        }
    }
    assert_eq!(downmix_stereo(&impulse(7, 4)).samples, [0.5, 0.5]);
}

#[test]
fn odd_and_wide_layouts_split_evens_left_and_odds_right() {
    for channels in [9u16, 10, 11, 16] {
        let left = channels.div_ceil(2) as f32;
        let right = (channels / 2) as f32;
        let b = buffer(48_000, channels, vec![1.0; channels as usize]);
        let out = downmix_stereo(&b);
        assert!((out.samples[0] - 1.0).abs() < 1e-6, "{channels}");
        assert!((out.samples[1] - 1.0).abs() < 1e-6, "{channels}");
        let first = downmix_stereo(&impulse(channels, 0)).samples;
        assert_eq!(first, [1.0 / left, 0.0]);
        let last = downmix_stereo(&impulse(channels, channels as usize - 1)).samples;
        let want = if channels % 2 == 0 {
            [0.0, 1.0 / right]
        } else {
            [1.0 / left, 0.0]
        };
        assert_eq!(last, want, "{channels}");
    }
}

#[test]
fn full_scale_surround_sums_past_one_and_is_left_unclipped() {
    let six = buffer(48_000, 6, vec![1.0; 6]);
    let out = downmix_stereo(&six);
    let peak = 1.0 + 2.0 * K;
    assert!((out.samples[0] - peak).abs() < 1e-6);
    assert!((out.samples[1] - peak).abs() < 1e-6);
    let m = mono(&six);
    assert!((m.samples[0] - 1.0).abs() < 1e-6);
}

#[test]
fn surround_to_mono_weighs_channels_as_ffmpeg_ac_1_does() {
    let sum = 2.0 + 2.0 * K;
    let want = [K / sum, K / sum, 1.0 / sum, 0.0, 0.5 / sum, 0.5 / sum];
    for (c, w) in want.iter().enumerate() {
        let mut s = vec![0.0; 6];
        s[c] = 1.0;
        let m = mono(&buffer(48_000, 6, s));
        assert!(
            (m.samples[0] - w).abs() < 1e-6,
            "channel {c}: {}",
            m.samples[0]
        );
    }
    let three = mono(&buffer(48_000, 3, vec![1.0, 0.0, 0.0]));
    assert!((three.samples[0] - K / (1.0 + 2.0 * K) * 1.0).abs() < 1e-6);
    let st = mono(&buffer(48_000, 2, vec![1.0, 0.0]));
    assert_eq!(st.samples, [0.5]);
}

#[test]
fn pass_through_cases_keep_the_samples() {
    let st = buffer(44_100, 2, vec![0.1, -0.2, 0.3, -0.4]);
    assert_eq!(downmix_stereo(&st).samples, st.samples);
    let none = buffer(44_100, 0, vec![]);
    assert_eq!(downmix_stereo(&none).channels, 0);
    assert_eq!(mono(&none).channels, 0);
    let one = buffer(44_100, 1, vec![0.5, -0.5]);
    assert_eq!(mono(&one).samples, one.samples);
    assert_eq!(downmix_stereo(&one).samples, [0.5, 0.5, -0.5, -0.5]);
    assert!(mono(&st).samples.iter().all(|s| (s + 0.05).abs() < 1e-6));
    let ragged = buffer(44_100, 3, vec![1.0; 7]);
    assert_eq!(downmix_stereo(&ragged).samples.len(), 4);
    assert_eq!(mono(&ragged).samples.len(), 2);
}

#[test]
fn gain_in_decibels() {
    let mut b = buffer(8_000, 1, vec![0.5, -0.25]);
    gain(&mut b, -6.020_6);
    assert!((b.samples[0] - 0.25).abs() < 1e-4 && (b.samples[1] + 0.125).abs() < 1e-4);
}

#[test]
fn resample_keeps_dc_and_length_for_odd_channel_counts() {
    for (from, to, channels) in [
        (48_000u32, 44_100u32, 3u16),
        (44_100, 48_000, 5),
        (8_000, 48_000, 1),
        (48_000, 7_350, 7),
    ] {
        let frames = 4_801usize;
        let mut b = AudioBuffer::new(from, channels);
        for _ in 0..frames {
            for c in 0..channels {
                b.samples.push(if c % 2 == 0 { 0.5 } else { -0.25 });
            }
        }
        let out = resample(&b, to);
        assert_eq!((out.rate, out.channels), (to, channels));
        let want = (frames as u64 * to as u64).div_ceil(from as u64) as usize;
        assert_eq!(out.frames(), want, "{from}->{to} x{channels}");
        let mid = out.frames() / 2;
        for c in 0..channels as usize {
            let v = out.samples[mid * channels as usize + c];
            let dc = if c % 2 == 0 { 0.5 } else { -0.25 };
            assert!((v - dc).abs() < 1e-3, "{from}->{to} channel {c}: {v}");
        }
    }
}

#[test]
fn resampler_drops_partial_frames_and_streams_like_one_call() {
    let mut whole = AudioBuffer::new(48_000, 3);
    whole.samples = (0..3 * 1000)
        .map(|i| ((i * 37) % 101) as f32 / 101.0 - 0.5)
        .collect();
    let once = resample(&whole, 32_000);
    let mut r = Resampler::new(48_000, 32_000, 3);
    assert_eq!(r.rates(), (48_000, 32_000));
    let mut streamed = Vec::new();
    for part in whole.samples.chunks(3 * 77) {
        streamed.extend(r.process(&buffer(48_000, 3, part.to_vec())).samples);
    }
    streamed.extend(r.flush().samples);
    assert_eq!(streamed, once.samples);
    let mut r = Resampler::new(48_000, 32_000, 3);
    let ragged = r.process(&buffer(48_000, 3, vec![0.25; 3 * 10 + 2]));
    let tail = r.flush();
    assert_eq!(
        (ragged.frames() + tail.frames()) as u64,
        (10u64 * 2).div_ceil(3)
    );
    assert_eq!(ragged.samples.len() % 3, 0);
}

#[test]
fn same_rate_and_channelless_resamples_only_relabel() {
    let b = buffer(48_000, 2, vec![0.1, 0.2]);
    assert_eq!(resample(&b, 48_000).samples, b.samples);
    let none = buffer(48_000, 0, vec![]);
    let out = resample(&none, 22_050);
    assert_eq!((out.rate, out.channels), (22_050, 0));
    let mut r = Resampler::new(0, 0, 0);
    assert_eq!(r.rates(), (1, 1));
    let out = r.process(&buffer(1, 1, vec![0.5; 4]));
    let all = out.frames() + r.flush().frames();
    assert_eq!(all, 4);
}
