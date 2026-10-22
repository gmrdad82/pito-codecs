use super::{downmix, gain};
use crate::media::{AudioBuffer, Error};

const H: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn impulse(channels: u16, at: usize) -> AudioBuffer {
    let mut b = AudioBuffer::new(48_000, channels);
    b.samples = (0..channels as usize)
        .map(|c| if c == at { 1.0 } else { 0.0 })
        .collect();
    b
}

fn response(channels: u16, to: u16) -> Vec<Vec<f32>> {
    (0..channels as usize)
        .map(|c| downmix(&impulse(channels, c), to).unwrap().samples)
        .collect()
}

fn close(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

#[test]
fn stereo_weights_follow_the_layout_and_keep_unity_gain() {
    let n = 1.0 + 2.0 * H;
    let cases: [(u16, Vec<[f32; 2]>); 6] = [
        (1, vec![[1.0, 1.0]]),
        (
            3,
            vec![
                [1.0 / (1.0 + H), 0.0],
                [0.0, 1.0 / (1.0 + H)],
                [H / (1.0 + H), H / (1.0 + H)],
            ],
        ),
        (
            4,
            vec![
                [1.0 / (1.0 + H), 0.0],
                [0.0, 1.0 / (1.0 + H)],
                [H / (1.0 + H), 0.0],
                [0.0, H / (1.0 + H)],
            ],
        ),
        (
            5,
            vec![
                [1.0 / n, 0.0],
                [0.0, 1.0 / n],
                [H / n, H / n],
                [H / n, 0.0],
                [0.0, H / n],
            ],
        ),
        (
            6,
            vec![
                [1.0 / n, 0.0],
                [0.0, 1.0 / n],
                [H / n, H / n],
                [0.0, 0.0],
                [H / n, 0.0],
                [0.0, H / n],
            ],
        ),
        (8, {
            let m = 1.0 + 3.0 * H;
            vec![
                [1.0 / m, 0.0],
                [0.0, 1.0 / m],
                [H / m, H / m],
                [0.0, 0.0],
                [H / m, 0.0],
                [0.0, H / m],
                [H / m, 0.0],
                [0.0, H / m],
            ]
        }),
    ];
    for (channels, want) in cases {
        let got = response(channels, 2);
        for (c, w) in want.iter().enumerate() {
            assert!(
                close(&got[c], w),
                "{channels} channels, channel {c}: {:?} vs {w:?}",
                got[c]
            );
        }
    }
    let seven = response(7, 2);
    let m = 1.0 + 2.0 * H + 0.5;
    assert!(close(&seven[4], &[0.5 / m, 0.5 / m]), "{:?}", seven[4]);
    let twelve = response(12, 2);
    let side = 1.0 + 5.0 * H;
    assert!(close(&twelve[0], &[1.0 / side, 0.0]));
    assert!(close(&twelve[10], &[H / side, 0.0]));
    assert!(close(&twelve[11], &[0.0, H / side]));
}

#[test]
fn full_scale_input_never_mixes_past_full_scale() {
    for channels in 1..=12u16 {
        for to in [1u16, 2] {
            let mut b = AudioBuffer::new(44_100, channels);
            b.samples = vec![1.0; channels as usize * 3];
            let out = downmix(&b, to).unwrap();
            assert_eq!(out.samples.len(), 3 * to as usize);
            assert!(
                out.samples.iter().all(|s| s.abs() <= 1.0 + 1e-6),
                "{channels} -> {to}: {:?}",
                out.samples
            );
            b.samples = vec![-1.0; channels as usize * 3];
            let out = downmix(&b, to).unwrap();
            assert!(out.samples.iter().all(|s| s.abs() <= 1.0 + 1e-6));
        }
    }
}

#[test]
fn mono_takes_the_mean_of_the_stereo_weights() {
    let n = 1.0 + 2.0 * H;
    let got: Vec<f32> = response(6, 1).into_iter().map(|v| v[0]).collect();
    let want = [0.5 / n, 0.5 / n, H / n, 0.0, 0.5 * H / n, 0.5 * H / n];
    assert!(close(&got, &want), "{got:?}");
    let mut st = AudioBuffer::new(8_000, 2);
    st.samples = vec![1.0, 0.0, 0.25, 0.75, -1.0, 1.0];
    assert_eq!(downmix(&st, 1).unwrap().samples, [0.5, 0.5, 0.0]);
}

#[test]
fn mono_up_to_stereo_duplicates_and_wider_upmixes_are_refused() {
    let mut m = AudioBuffer::new(16_000, 1);
    m.samples = vec![0.5, -0.25, 1.0];
    let s = downmix(&m, 2).unwrap();
    assert_eq!((s.rate, s.channels), (16_000, 2));
    assert_eq!(s.samples, [0.5, 0.5, -0.25, -0.25, 1.0, 1.0]);
    let back = downmix(&s, 1).unwrap();
    assert_eq!(back.samples, m.samples);
    for (from, to) in [(1u16, 6u16), (2, 6), (6, 3), (6, 4), (1, 0)] {
        let b = impulse(from, 0);
        match downmix(&b, to) {
            Err(Error::Unsupported(msg)) => assert!(msg.contains("only mono and stereo"), "{msg}"),
            other => panic!("{from} -> {to}: {other:?}"),
        }
    }
    let six = impulse(6, 2);
    assert_eq!(downmix(&six, 6).unwrap().samples, six.samples);
}

#[test]
fn empty_partial_and_channelless_input() {
    let none = AudioBuffer::new(48_000, 0);
    assert!(matches!(downmix(&none, 2), Err(Error::Invalid(_))));
    let empty = AudioBuffer::new(48_000, 6);
    let out = downmix(&empty, 2).unwrap();
    assert!(out.samples.is_empty());
    let mut ragged = AudioBuffer::new(48_000, 6);
    ragged.samples = vec![1.0; 6 + 4];
    assert_eq!(downmix(&ragged, 2).unwrap().samples.len(), 2);
    assert_eq!(downmix(&ragged, 1).unwrap().samples.len(), 1);
}

#[test]
fn gain_scales_in_decibels() {
    let mut b = AudioBuffer::new(48_000, 2);
    b.samples = vec![0.25, -0.5];
    gain(&mut b, 20.0 * 2f32.log10());
    assert!(close(&b.samples, &[0.5, -1.0]));
    gain(&mut b, -20.0);
    assert!(close(&b.samples, &[0.05, -0.1]));
    gain(&mut b, 0.0);
    assert!(close(&b.samples, &[0.05, -0.1]));
    gain(&mut b, f32::NEG_INFINITY);
    assert_eq!(b.samples, [0.0, -0.0]);
}
