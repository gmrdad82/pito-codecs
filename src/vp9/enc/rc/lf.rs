use super::FrameKind;
use super::q::qstep;

pub const MAX_LEVEL: u8 = 63;

pub fn guess_filter_level(base_q_idx: u8, kind: FrameKind) -> u8 {
    if base_q_idx == 0 {
        return 0;
    }
    let s = qstep(base_q_idx).sqrt();
    let g = match kind {
        FrameKind::Key => 1.45 * s - 3.5,
        FrameKind::Golden | FrameKind::AltRef => 1.5 * s - 3.0,
        FrameKind::Inter => 1.55 * s - 3.0,
    };
    g.round().clamp(0.0, MAX_LEVEL as f64) as u8
}

pub fn pick_filter_level(
    base_q_idx: u8,
    kind: FrameKind,
    sse_at: Option<&mut dyn FnMut(u8) -> u64>,
) -> u8 {
    let guess = guess_filter_level(base_q_idx, kind);
    let Some(sse_at) = sse_at else {
        return guess;
    };
    if base_q_idx == 0 {
        return 0;
    }
    let mut cache = [u64::MAX; MAX_LEVEL as usize + 1];
    let mut probe = |l: u8, f: &mut dyn FnMut(u8) -> u64| {
        if cache[l as usize] == u64::MAX {
            cache[l as usize] = f(l);
        }
        cache[l as usize]
    };
    let mut best = guess;
    let mut best_sse = probe(best, sse_at);
    let mut step = (guess / 4).max(2);
    while step > 0 {
        let mut moved = false;
        for l in [best.saturating_sub(step), (best + step).min(MAX_LEVEL)] {
            if l == best {
                continue;
            }
            let s = probe(l, sse_at);
            if s < best_sse || (s == best_sse && l < best) {
                best_sse = s;
                best = l;
                moved = true;
            }
        }
        if !moved {
            step /= 2;
        }
    }
    best
}
