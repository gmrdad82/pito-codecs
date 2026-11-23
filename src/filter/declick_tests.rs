use super::*;

const RATE: u32 = 48_000;
const SECONDS: usize = 6;

struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
    }
}

fn at(seconds: f64) -> usize {
    (seconds * f64::from(RATE)) as usize
}

fn ramp(t: f64, start: f64, end: f64, attack: f64, release: f64) -> f64 {
    if t < start || t >= end {
        return 0.0;
    }
    let up = ((t - start) / attack).min(1.0);
    let down = ((end - t) / release).min(1.0);
    let e = |x: f64| 0.5 - 0.5 * (std::f64::consts::PI * x).cos();
    e(up) * e(down)
}

fn voice() -> Vec<f32> {
    let mut noise = Noise(7);
    let mut out = vec![0.0f32; RATE as usize * SECONDS];
    let words = [
        (0.30, 0.80, 118.0),
        (1.30, 1.90, 142.0),
        (2.50, 3.10, 104.0),
        (3.70, 4.30, 156.0),
        (4.80, 5.40, 125.0),
    ];
    let mut last = 0.0f32;
    for (i, slot) in out.iter_mut().enumerate() {
        let t = i as f64 / f64::from(RATE);
        let mut v = 0.0f64;
        for &(a, b, f0) in &words {
            let env = ramp(t, a, b, 0.012, 0.04);
            if env > 0.0 {
                let tau = 2.0 * std::f64::consts::PI;
                let phase = tau * f0 * (t + 0.03 * (1.0 - (tau * 4.0 * t).cos()) / (tau * 4.0));
                let mut s = 0.0;
                for k in 1..=40 {
                    let kf = f64::from(k);
                    s += (phase * kf + 0.9 * kf * kf).sin() / kf.powf(2.0);
                }
                v += env * 0.25 * s;
            }
        }
        let n = noise.next();
        let hiss = ramp(t, 1.0, 1.22, 0.006, 0.03) + ramp(t, 3.3, 3.55, 0.006, 0.03);
        let rough = n - last;
        last = n;
        v += hiss * 0.05 * f64::from(rough);
        let burst =
            ramp(t, 2.2, 2.212, 0.0005, 0.008) * 0.3 + ramp(t, 2.212, 2.26, 0.002, 0.04) * 0.03;
        v += burst * f64::from(noise.next());
        v += 1e-4 * f64::from(noise.next());
        *slot = v as f32;
    }
    out
}

fn spike(x: &mut [f32], seconds: f64, width_ms: f64, amplitude: f32, hz: f64) {
    let start = at(seconds);
    let len = (width_ms / 1000.0 * f64::from(RATE)) as usize;
    for k in 0..len {
        let p = k as f64 / len as f64;
        let hann = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * p).cos();
        let t = k as f64 / f64::from(RATE);
        x[start + k] += amplitude * (hann * (2.0 * std::f64::consts::PI * hz * t).sin()) as f32;
    }
}

const CLICKS: [(f64, f64, f32, f64); 8] = [
    (0.15, 2.0, 0.08, 3200.0),
    (0.95, 4.0, 0.1, 2600.0),
    (1.296, 3.0, 0.12, 3600.0),
    (1.6, 1.0, 0.12, 4200.0),
    (2.0, 6.0, 0.09, 2400.0),
    (2.496, 2.0, 0.1, 3000.0),
    (3.9, 5.0, 0.1, 2800.0),
    (5.0, 3.0, 0.15, 3400.0),
];

fn spiked(clean: &[f32]) -> Vec<f32> {
    let mut x = clean.to_vec();
    for &(t, w, a, hz) in &CLICKS {
        spike(&mut x, t, w, a, hz);
    }
    x
}

fn rms_db(x: &[f32]) -> f64 {
    let e = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64;
    10.0 * e.max(1e-20).log10()
}

#[test]
fn a_clean_voice_with_plosives_and_fricatives_comes_out_sample_identical() {
    let clean = voice();
    for strength in [Strength::Light, Strength::Medium, Strength::Strong] {
        let mut x = clean.clone();
        let found = declick(&mut x, 1, RATE, strength);
        assert!(found.is_empty(), "{strength:?} found {found:?}");
        assert!(x == clean, "{strength:?}");
    }
}

#[test]
fn every_spike_is_found_and_repaired_to_within_1_db_of_the_clean_signal() {
    let clean = voice();
    let dirty = spiked(&clean);
    let mut x = dirty.clone();
    let found = declick(&mut x, 1, RATE, Strength::Medium);
    assert_eq!(found.len(), CLICKS.len(), "{found:?}");
    for (click, &(t, w, _, _)) in found.iter().zip(&CLICKS) {
        let (a, b) = (at(t), at(t + w / 1000.0));
        assert!(
            click.start <= a + 48 && click.end + 48 >= b,
            "{t}: {click:?}"
        );
        assert!((click.seconds(RATE) - t).abs() < 0.008, "{t}: {click:?}");
        let (lo, hi) = (a.saturating_sub(at(0.01)), b + at(0.01));
        let was = rms_db(&clean[lo..hi]);
        let now = rms_db(&x[lo..hi]);
        assert!(
            (now - was).abs() < 1.0,
            "{t}: clean {was:.2} dB repaired {now:.2} dB"
        );
        let err: Vec<f32> = (click.start..click.end).map(|i| x[i] - clean[i]).collect();
        let hurt: Vec<f32> = (click.start..click.end)
            .map(|i| dirty[i] - clean[i])
            .collect();
        assert!(
            rms_db(&err) < rms_db(&hurt) - 6.0,
            "{t}: error {:.1} dB, click {:.1} dB",
            rms_db(&err),
            rms_db(&hurt)
        );
    }
    assert!(
        detect(&x, RATE, Strength::Strong).is_empty(),
        "a click is left"
    );
    let touched: Vec<bool> = {
        let mut m = vec![false; x.len()];
        for c in &found {
            m[c.start..c.end].iter_mut().for_each(|v| *v = true);
        }
        m
    };
    for i in 0..x.len() {
        if !touched[i] {
            assert_eq!(x[i], dirty[i], "sample {i} changed outside a repair");
        }
    }
}

#[test]
fn the_plosive_and_the_onsets_keep_their_level() {
    let clean = voice();
    let mut x = spiked(&clean);
    declick(&mut x, 1, RATE, Strength::Medium);
    for (a, b) in [(2.19, 2.3), (1.29, 1.4), (1.0, 1.25), (3.3, 3.6)] {
        let (a, b) = (at(a), at(b));
        let touched = CLICKS
            .iter()
            .any(|&(t, w, _, _)| at(t) < b + at(0.01) && at(t + w / 1000.0) + at(0.01) > a);
        let d = (rms_db(&x[a..b]) - rms_db(&clean[a..b])).abs();
        assert!(d < if touched { 1.0 } else { 1e-9 }, "{a}..{b}: {d}");
    }
}

#[test]
fn a_stereo_track_is_repaired_in_both_channels() {
    let clean = voice();
    let dirty = spiked(&clean);
    let mut x: Vec<f32> = dirty.iter().flat_map(|&v| [v, v]).collect();
    let found = declick(&mut x, 2, RATE, Strength::Medium);
    assert_eq!(found.len(), CLICKS.len());
    let mut mono = dirty.clone();
    declick(&mut mono, 1, RATE, Strength::Medium);
    for i in 0..mono.len() {
        assert!((x[2 * i] - mono[i]).abs() < 1e-6);
        assert!((x[2 * i + 1] - mono[i]).abs() < 1e-6);
    }
}

#[test]
fn stronger_settings_find_at_least_what_lighter_ones_do_and_a_run_repeats() {
    let dirty = spiked(&voice());
    let counts: Vec<usize> = [Strength::Light, Strength::Medium, Strength::Strong]
        .iter()
        .map(|&s| declick(&mut dirty.clone(), 1, RATE, s).len())
        .collect();
    assert!(
        counts[0] <= counts[1] && counts[1] <= counts[2],
        "{counts:?}"
    );
    let mut a = dirty.clone();
    let mut b = dirty.clone();
    declick(&mut a, 1, RATE, Strength::Medium);
    declick(&mut b, 1, RATE, Strength::Medium);
    assert!(a == b);
}

#[test]
fn a_repaired_click_reads_its_time_and_level() {
    let mut x = spiked(&voice());
    let found = declick(&mut x, 1, RATE, Strength::Medium);
    let first = found[0];
    assert!((first.seconds(RATE) - 0.15).abs() < 0.004);
    assert!((-40.0..-10.0).contains(&first.level_db), "{first:?}");
    assert!(first.above_db >= Strength::Medium.above_db());
}

#[test]
fn degenerate_input_is_left_alone() {
    let mut none: Vec<f32> = Vec::new();
    assert!(declick(&mut none, 1, RATE, Strength::Medium).is_empty());
    let mut few = vec![0.5f32; 10];
    assert!(declick(&mut few, 1, RATE, Strength::Medium).is_empty());
    assert!(declick(&mut few, 0, RATE, Strength::Medium).is_empty());
    assert!(declick(&mut few, 1, 0, Strength::Medium).is_empty());
    let mut silent = vec![0.0f32; 48_000];
    assert!(declick(&mut silent, 1, RATE, Strength::Strong).is_empty());
    assert!(silent.iter().all(|v| *v == 0.0));
}

#[test]
fn strengths_parse_by_name() {
    assert_eq!(Strength::parse("light"), Some(Strength::Light));
    assert_eq!(
        Strength::parse("strong").map(Strength::name),
        Some("strong")
    );
    assert_eq!(Strength::parse("hard"), None);
    assert_eq!(Strength::default(), Strength::Medium);
}
