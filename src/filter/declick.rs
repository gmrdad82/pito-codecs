const HP_HZ: f64 = 1500.0;
const SMOOTH: f64 = 0.0005;
const MAX_WIDTH: f64 = 0.008;
const SPAN_CAP: f64 = 0.012;
const FLANK_NEAR: f64 = 0.0005;
const FLANK_FAR: f64 = 0.006;
const CONTEXT_FAR: f64 = 0.040;
const MARGIN: f64 = 0.0004;
const EDGE_FRACTION: f32 = 0.25;
const FLANK_FRACTION: f32 = 1.0 / 3.0;
const ISOLATION: f64 = 0.020;
const ISOLATION_FRACTION: f32 = 0.5;
const TAIL_FRACTION: f32 = 0.03;
const PERIODIC_MISS: f64 = 0.12;
const FLOOR: f32 = 2e-3;
const BACKGROUND_FLOOR: f32 = 1e-5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strength {
    Light,
    #[default]
    Medium,
    Strong,
}

impl Strength {
    pub fn parse(text: &str) -> Option<Strength> {
        match text {
            "light" => Some(Strength::Light),
            "medium" => Some(Strength::Medium),
            "strong" => Some(Strength::Strong),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Strength::Light => "light",
            Strength::Medium => "medium",
            Strength::Strong => "strong",
        }
    }

    pub fn above_db(self) -> f64 {
        match self {
            Strength::Light => 20.0,
            Strength::Medium => 14.0,
            Strength::Strong => 9.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Click {
    pub at: usize,
    pub start: usize,
    pub end: usize,
    pub level_db: f64,
    pub above_db: f64,
}

impl Click {
    pub fn seconds(&self, rate: u32) -> f64 {
        self.at as f64 / f64::from(rate.max(1))
    }
}

fn samples_of(seconds: f64, rate: u32) -> usize {
    ((seconds * f64::from(rate)).round() as usize).max(1)
}

fn highpass(mono: &[f32], rate: u32) -> Vec<f32> {
    let w = 2.0 * std::f64::consts::PI * HP_HZ / f64::from(rate);
    let alpha = w.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
    let cos = w.cos();
    let a0 = 1.0 + alpha;
    let b0 = (1.0 + cos) / 2.0 / a0;
    let b1 = -(1.0 + cos) / a0;
    let a1 = -2.0 * cos / a0;
    let a2 = (1.0 - alpha) / a0;
    let pass = |input: &mut Vec<f64>| {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for v in input.iter_mut() {
            let x = *v;
            let y = b0 * x + b1 * x1 + b0 * x2 - a1 * y1 - a2 * y2;
            x2 = x1;
            x1 = x;
            y2 = y1;
            y1 = y;
            *v = y;
        }
    };
    let mut buffer: Vec<f64> = mono.iter().map(|&s| f64::from(s)).collect();
    pass(&mut buffer);
    buffer.reverse();
    pass(&mut buffer);
    buffer.reverse();
    buffer.into_iter().map(|v| v as f32).collect()
}

fn prefix(values: impl Iterator<Item = f32>) -> Vec<f64> {
    let mut sums = vec![0.0f64];
    let mut total = 0.0;
    for v in values {
        total += f64::from(v);
        sums.push(total);
    }
    sums
}

fn mean(sums: &[f64], from: usize, to: usize) -> Option<f32> {
    let to = to.min(sums.len() - 1);
    (from < to).then(|| ((sums[to] - sums[from]) / (to - from) as f64) as f32)
}

pub fn detect(mono: &[f32], rate: u32, strength: Strength) -> Vec<Click> {
    let n = mono.len();
    if rate < 8000 || n < 64 {
        return Vec::new();
    }
    let hp = highpass(mono, rate);
    let magnitude = prefix(hp.iter().map(|v| v.abs()));
    let half = (samples_of(SMOOTH, rate) / 2).max(1);
    let env: Vec<f32> = (0..n)
        .map(|i| mean(&magnitude, i.saturating_sub(half), (i + half + 1).min(n)).unwrap_or(0.0))
        .collect();
    let sums = prefix(env.iter().copied());
    let cap = samples_of(SPAN_CAP, rate);
    let widest = samples_of(MAX_WIDTH, rate);
    let near = samples_of(FLANK_NEAR, rate);
    let far = samples_of(FLANK_FAR, rate);
    let context = samples_of(CONTEXT_FAR, rate);
    let margin = samples_of(MARGIN, rate);
    let need = 10f32.powf(strength.above_db() as f32 / 20.0);
    let reach = half * 2;
    let mut found = Vec::new();
    let mut i = 1;
    while i + 1 < n {
        let h = env[i];
        let peak = h >= FLOOR
            && h >= env[i - 1]
            && h > env[i + 1]
            && (i.saturating_sub(reach)..(i + reach + 1).min(n)).all(|j| env[j] <= h);
        if !peak {
            i += 1;
            continue;
        }
        let mut left = i;
        while left > 0 && env[left - 1] > h * EDGE_FRACTION && i - left < cap {
            left -= 1;
        }
        let mut right = i;
        while right + 1 < n && env[right + 1] > h * EDGE_FRACTION && right - i < cap {
            right += 1;
        }
        if i - left >= cap || right - i >= cap || right - left + 1 > widest {
            i += 1;
            continue;
        }
        let quiet = h * FLANK_FRACTION;
        let before = mean(&sums, left.saturating_sub(far), left.saturating_sub(near));
        let after = mean(&sums, right + 1 + near, right + 1 + far);
        if before.is_some_and(|m| m > quiet) || after.is_some_and(|m| m > quiet) {
            i += 1;
            continue;
        }
        let wide = samples_of(ISOLATION, rate);
        let loudest = |from: usize, to: usize| {
            env[from.min(n)..to.min(n)]
                .iter()
                .fold(0.0f32, |m, v| m.max(*v))
        };
        let crowded = loudest(left.saturating_sub(wide), left.saturating_sub(near))
            .max(loudest(right + 1 + near, right + 1 + wide));
        if crowded > h * ISOLATION_FRACTION {
            i += 1;
            continue;
        }
        let around_before = mean(
            &sums,
            left.saturating_sub(context),
            left.saturating_sub(far),
        );
        let around_after = mean(&sums, right + 1 + far, right + 1 + context);
        let background = match (around_before, around_after) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => {
                i += 1;
                continue;
            }
        }
        .max(BACKGROUND_FLOOR);
        if h < background * need {
            i += 1;
            continue;
        }
        let tail = (h * TAIL_FRACTION).max(background * 1.5);
        let (mut first, mut last) = (left, right);
        while first > 0 && env[first - 1] > tail && left - first < cap {
            first -= 1;
        }
        while last + 1 < n && env[last + 1] > tail && last - right < cap {
            last += 1;
        }
        let top = hp[left..=right].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        found.push(Click {
            at: i,
            start: first.saturating_sub(margin),
            end: (last + 1 + margin).min(n),
            level_db: 20.0 * f64::from(top.max(1e-9)).log10(),
            above_db: 20.0 * f64::from(h / background).log10(),
        });
        i = last + far;
    }
    found
}

fn levinson(r: &[f64], order: usize) -> (Vec<f64>, f64) {
    let mut a = vec![0.0; order + 1];
    a[0] = 1.0;
    let mut error = r[0];
    for m in 1..=order {
        if error <= 0.0 {
            break;
        }
        let mut acc = r[m];
        for j in 1..m {
            acc += a[j] * r[m - j];
        }
        let k = -acc / error;
        let previous = a.clone();
        for j in 1..m {
            a[j] = previous[j] + k * previous[m - j];
        }
        a[m] = k;
        error *= 1.0 - k * k;
    }
    (a, error)
}

fn linear(x: &mut [f32], start: usize, end: usize) {
    let (a, b) = (
        x.get(start.wrapping_sub(1)).copied().unwrap_or(0.0),
        x.get(end).copied().unwrap_or(0.0),
    );
    let m = (end - start + 1) as f32;
    for (k, v) in x[start..end].iter_mut().enumerate() {
        *v = a + (b - a) * (k + 1) as f32 / m;
    }
}

fn periodic(x: &mut [f32], start: usize, end: usize, rate: u32) -> bool {
    let n = x.len();
    let window = samples_of(0.015, rate);
    let (low, high) = (samples_of(0.0025, rate), samples_of(0.014, rate));
    if start < window + high || end + window + high > n {
        return false;
    }
    let energy: f64 = x[start - window..start]
        .iter()
        .chain(&x[end..end + window])
        .map(|v| f64::from(*v).powi(2))
        .sum();
    if energy <= 1e-12 {
        return false;
    }
    let mut best = (f64::INFINITY, 0usize);
    for lag in low..=high {
        let mut miss = 0.0f64;
        for t in start - window..start {
            miss += f64::from(x[t] - x[t - lag]).powi(2);
        }
        for t in end..end + window {
            miss += f64::from(x[t] - x[t + lag]).powi(2);
        }
        if miss < best.0 {
            best = (miss, lag);
        }
    }
    if best.0 > PERIODIC_MISS * energy {
        return false;
    }
    let lag = best.1;
    let m = end - start;
    let mut forward = vec![0.0f32; m];
    let mut backward = vec![0.0f32; m];
    for k in 0..m {
        let t = start + k;
        forward[k] = if t >= lag + start {
            forward[t - lag - start]
        } else {
            x[t - lag]
        };
    }
    for k in (0..m).rev() {
        let t = start + k;
        backward[k] = if t + lag < end {
            backward[t + lag - start]
        } else {
            x[t + lag]
        };
    }
    for k in 0..m {
        let w = (k as f32 + 0.5) / m as f32;
        x[start + k] = forward[k] * (1.0 - w) + backward[k] * w;
    }
    true
}

fn repair(x: &mut [f32], start: usize, end: usize, rate: u32) {
    let n = x.len();
    if start >= end || end > n {
        return;
    }
    if periodic(x, start, end, rate) {
        return;
    }
    let order = ((rate / 750) as usize).clamp(8, 64);
    let reach = samples_of(0.020, rate);
    let m = end - start;
    if start < order + 4 * order || end + order + 4 * order > n {
        linear(x, start, end);
        return;
    }
    let left = &x[start.saturating_sub(reach)..start];
    let right = &x[end..(end + reach).min(n)];
    let mut r = vec![0.0f64; order + 1];
    for part in [left, right] {
        for k in 0..=order {
            let mut acc = 0.0f64;
            for t in k..part.len() {
                acc += f64::from(part[t]) * f64::from(part[t - k]);
            }
            r[k] += acc;
        }
    }
    if r[0] <= 1e-12 {
        linear(x, start, end);
        return;
    }
    r[0] *= 1.0 + 1e-6;
    let (a, _) = levinson(&r, order);
    let mut autocorrelation = vec![0.0f64; order + 1];
    for (k, slot) in autocorrelation.iter_mut().enumerate() {
        *slot = (0..=order - k).map(|i| a[i] * a[i + k]).sum();
    }
    let mut known: Vec<f64> = x[start - order..end + order]
        .iter()
        .map(|&v| f64::from(v))
        .collect();
    for v in &mut known[order..order + m] {
        *v = 0.0;
    }
    let errors: Vec<f64> = (0..m + order)
        .map(|t| (0..=order).map(|j| a[j] * known[order + t - j]).sum())
        .collect();
    let rhs: Vec<f64> = (0..m)
        .map(|i| -(0..=order).map(|k| a[k] * errors[i + k]).sum::<f64>())
        .collect();
    let width = order + 1;
    let mut factor = vec![0.0f64; m * width];
    let ridge = autocorrelation[0] * 1e-9;
    for i in 0..m {
        for j in i.saturating_sub(order)..=i {
            let mut sum = autocorrelation[i - j];
            if i == j {
                sum += ridge;
            }
            for k in i.saturating_sub(order).max(j.saturating_sub(order))..j {
                sum -= factor[i * width + (i - k)] * factor[j * width + (j - k)];
            }
            factor[i * width + (i - j)] = if i == j {
                sum.max(1e-12).sqrt()
            } else {
                sum / factor[j * width]
            };
        }
    }
    let mut y = vec![0.0f64; m];
    for i in 0..m {
        let mut sum = rhs[i];
        for k in i.saturating_sub(order)..i {
            sum -= factor[i * width + (i - k)] * y[k];
        }
        y[i] = sum / factor[i * width];
    }
    let mut u = vec![0.0f64; m];
    for i in (0..m).rev() {
        let mut sum = y[i];
        for k in i + 1..(i + order + 1).min(m) {
            sum -= factor[k * width + (k - i)] * u[k];
        }
        u[i] = sum / factor[i * width];
    }
    let side = samples_of(0.002, rate);
    let rms = |part: &[f32]| {
        (part.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / part.len().max(1) as f64).sqrt()
    };
    let target = rms(&x[start - side..start]).min(rms(&x[end..(end + side).min(n)]));
    let have: f64 = u.iter().map(|v| v * v).sum();
    let want = target * target * m as f64;
    if want > have {
        let mut seed = (start as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
        let mut z: Vec<f64> = (0..m)
            .map(|_| {
                let mut total = 0.0;
                for _ in 0..12 {
                    seed = seed
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    total += (seed >> 11) as f64 / (1u64 << 53) as f64;
                }
                total - 6.0
            })
            .collect();
        for i in (0..m).rev() {
            let mut sum = z[i];
            for k in i + 1..(i + order + 1).min(m) {
                sum -= factor[k * width + (k - i)] * z[k];
            }
            z[i] = sum / factor[i * width];
        }
        let weight: f64 = z.iter().map(|v| v * v).sum();
        if weight > 0.0 {
            let c = ((want - have) / weight).sqrt();
            for (slot, w) in u.iter_mut().zip(&z) {
                *slot += c * w;
            }
        }
    }
    if u.iter().any(|v| !v.is_finite()) {
        linear(x, start, end);
        return;
    }
    let edge = samples_of(0.001, rate);
    let level = |part: &[f32]| {
        (part.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / part.len().max(1) as f64).sqrt()
    };
    let ceiling =
        1.3 * level(&x[start - edge..start]).max(level(&x[end..(end + edge).min(n)])) + 1e-9;
    let made = level_f64(&u);
    let scale = if made > ceiling { ceiling / made } else { 1.0 };
    for (slot, v) in x[start..end].iter_mut().zip(u) {
        let v = v * scale;
        *slot = v as f32;
    }
}

fn level_f64(v: &[f64]) -> f64 {
    (v.iter().map(|x| x * x).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

pub fn declick(samples: &mut [f32], channels: usize, rate: u32, strength: Strength) -> Vec<Click> {
    if channels == 0 || rate == 0 {
        return Vec::new();
    }
    let frames = samples.len() / channels;
    let mono: Vec<f32> = if channels == 1 {
        samples.to_vec()
    } else {
        samples
            .chunks_exact(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    let clicks = detect(&mono, rate, strength);
    if clicks.is_empty() {
        return clicks;
    }
    let mut lane = vec![0.0f32; frames];
    for c in 0..channels {
        for (f, slot) in lane.iter_mut().enumerate() {
            *slot = samples[f * channels + c];
        }
        for click in &clicks {
            repair(&mut lane, click.start, click.end, rate);
        }
        for (f, v) in lane.iter().enumerate() {
            samples[f * channels + c] = *v;
        }
    }
    clicks
}

#[cfg(test)]
#[path = "declick_tests.rs"]
mod tests;
