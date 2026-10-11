mod analysis;
mod aq;
mod lf;
mod q;
mod threads;

#[cfg(test)]
mod tests;

pub use analysis::{Analyser, FrameAnalysis};
pub use aq::{SEGMENTS, Segmentation, tree_probs};
pub use lf::{MAX_LEVEL, guess_filter_level, pick_filter_level};
pub use q::{crf_to_q, lambda, q_index_for, qstep};
pub use threads::{
    LAG, Row, Sbs, TileLayout, Wavefront, max_log2_tile_cols, min_log2_tile_cols, tile_offset,
};

#[derive(Clone, Debug, PartialEq)]
pub enum RcMode {
    ConstantQ {
        q_index: u8,
    },
    Crf {
        crf: u8,
    },
    Bitrate {
        kbps: u32,
        buffer_ms: u32,
    },
    TwoPass {
        kbps: u32,
        stats: Vec<FrameAnalysis>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aq {
    Off,
    Variance,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RcConfig {
    pub mode: RcMode,
    pub width: usize,
    pub height: usize,
    pub fps_num: u32,
    pub fps_den: u32,
    pub keyint: u32,
    pub min_q: u8,
    pub max_q: u8,
    pub aq: Aq,
}

impl RcConfig {
    pub fn new(mode: RcMode, width: usize, height: usize) -> RcConfig {
        RcConfig {
            mode,
            width,
            height,
            fps_num: 30,
            fps_den: 1,
            keyint: 240,
            min_q: 0,
            max_q: 255,
            aq: Aq::Off,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameKind {
    Key,
    Inter,
    Golden,
    AltRef,
}

impl FrameKind {
    fn slot(self) -> usize {
        match self {
            FrameKind::Key => 0,
            FrameKind::Inter => 1,
            FrameKind::Golden | FrameKind::AltRef => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FramePlan {
    pub base_q_idx: u8,
    pub filter_level: u8,
    pub sharpness: u8,
    pub lambda: u32,
    pub target_bits: u64,
}

const QCOMP: f64 = 0.6;
const EXP: [f64; 3] = [0.85, 1.5, 1.5];
const K0: [f64; 3] = [7.2, 42.0, 42.0];

fn solve(terms: [(f64, f64); 2], bits: f64) -> f64 {
    let f = |s: f64| terms.iter().map(|&(a, e)| a * s.powf(-e)).sum::<f64>();
    let (mut lo, mut hi) = (0.5f64.ln(), 8000f64.ln());
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if f(mid.exp()) > bits {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (0.5 * (lo + hi)).exp()
}

pub struct RateControl {
    cfg: RcConfig,
    analyser: Analyser,
    seg: Segmentation,
    k: [f64; 3],
    seen: [bool; 3],
    frame_bits: f64,
    buffer: f64,
    buffer_size: f64,
    log_cost: f64,
    log_cost_n: f64,
    ratio: f64,
    a_inter: f64,
    a_key: f64,
    remaining: f64,
    frame: usize,
    suffix: Vec<(f64, f64)>,
    geo: f64,
    pending: Option<(FrameKind, u64)>,
}

impl RateControl {
    pub fn new(cfg: RcConfig) -> RateControl {
        let fps = cfg.fps_num.max(1) as f64 / cfg.fps_den.max(1) as f64;
        let geo = match &cfg.mode {
            RcMode::TwoPass { stats, .. } => two_pass_geo(stats, cfg.keyint),
            _ => 1.0,
        };
        let (frame_bits, buffer_size, remaining, suffix) = match &cfg.mode {
            RcMode::Bitrate { kbps, buffer_ms } => {
                let b = *kbps as f64 * 1000.0;
                (
                    b / fps,
                    b * (*buffer_ms).max(100) as f64 / 1000.0,
                    0.0,
                    Vec::new(),
                )
            }
            RcMode::TwoPass { kbps, stats } => {
                let b = *kbps as f64 * 1000.0;
                let total = b * stats.len() as f64 / fps;
                (
                    b / fps,
                    b * 2.0,
                    total,
                    two_pass_suffix(stats, cfg.keyint, geo),
                )
            }
            _ => (0.0, 0.0, 0.0, Vec::new()),
        };
        RateControl {
            analyser: Analyser::new(cfg.width, cfg.height),
            seg: Segmentation::default(),
            k: K0,
            seen: [false; 3],
            frame_bits,
            buffer: 0.0,
            buffer_size,
            log_cost: 0.0,
            log_cost_n: 0.0,
            ratio: 0.5,
            a_inter: 0.0,
            a_key: 0.0,
            remaining,
            frame: 0,
            geo,
            suffix,
            pending: None,
            cfg,
        }
    }

    pub fn analyse(&mut self, y: &[u8], stride: usize) -> FrameAnalysis {
        self.analyser.run(y, stride)
    }

    pub fn segmentation(&self) -> &Segmentation {
        &self.seg
    }

    pub fn plan(&mut self, kind: FrameKind, a: &FrameAnalysis) -> FramePlan {
        let cost = a.cost(kind) as f64;
        if a.has_prev && kind != FrameKind::Key {
            self.ratio = 0.8 * self.ratio + 0.2 * a.inter_ratio().min(1.0);
            self.log_cost += cost.ln();
            self.log_cost_n += 1.0;
        }
        let g = if self.log_cost_n > 0.0 {
            (self.log_cost / self.log_cost_n).exp()
        } else {
            cost
        };
        let boost = self.boost(kind);
        let modulation = ((cost / g).powf(1.0 - QCOMP)).clamp(0.8, 1.25);
        let step_for = |s: f64| match kind {
            FrameKind::Key | FrameKind::Golden | FrameKind::AltRef => s / boost,
            FrameKind::Inter => s * modulation,
        };
        let (step, target) = match &self.cfg.mode {
            RcMode::ConstantQ { q_index } => (qstep(*q_index), 0.0),
            RcMode::Crf { crf } => (step_for(qstep(crf_to_q(*crf))), 0.0),
            RcMode::Bitrate { .. } => {
                let (ek, ei) = (EXP[0], EXP[1]);
                match kind {
                    FrameKind::Key => self.a_key = cost * boost.powf(ek),
                    _ => {
                        let a_now = cost * modulation.powf(-ei);
                        self.a_inter = if self.a_inter == 0.0 {
                            a_now
                        } else {
                            0.9 * self.a_inter + 0.1 * a_now
                        };
                    }
                }
                let a_inter = if self.a_inter > 0.0 {
                    self.a_inter
                } else {
                    cost * self.ratio.clamp(0.05, 1.0)
                };
                let key_share = match self.cfg.keyint {
                    0 => 0.0,
                    n => 1.0 / n as f64,
                };
                let fill = (self.buffer / self.buffer_size).clamp(-0.6, 0.6);
                let want = (self.frame_bits * (1.0 + fill)).max(1.0);
                let s = solve(
                    [
                        (key_share * self.k[0] * self.a_key, ek),
                        ((1.0 - key_share) * self.k[1] * a_inter, ei),
                    ],
                    want,
                );
                let st = step_for(s);
                (st, self.model(kind, cost, st))
            }
            RcMode::TwoPass { stats, .. } => {
                let i = self.frame.min(stats.len().saturating_sub(1));
                let (sk, si) = self.suffix.get(i).copied().unwrap_or((0.0, 0.0));
                let s = solve(
                    [(self.k[0] * sk, EXP[0]), (self.k[1] * si, EXP[1])],
                    self.remaining.max(self.frame_bits * 0.25),
                );
                let st = match kind {
                    FrameKind::Inter => s * two_pass_modulation(cost, self.geo),
                    _ => s / boost,
                };
                (st, self.model(kind, cost, st))
            }
        };
        let mut q = q_index_for(step);
        if !matches!(self.cfg.mode, RcMode::ConstantQ { .. }) {
            q = q.clamp(self.cfg.min_q, self.cfg.max_q.max(self.cfg.min_q));
        }
        if q > 0 && self.cfg.aq == Aq::Variance {
            let (mi_cols, mi_rows) = (self.cfg.width.div_ceil(8), self.cfg.height.div_ceil(8));
            self.seg.variance_aq(
                q,
                mi_cols,
                mi_rows,
                self.analyser.activity(),
                self.analyser.blocks(),
            );
        } else {
            self.seg.disable();
        }
        self.pending = Some((kind, a.cost(kind)));
        FramePlan {
            base_q_idx: q,
            filter_level: guess_filter_level(q, kind),
            sharpness: 0,
            lambda: lambda(q),
            target_bits: target.max(0.0) as u64,
        }
    }

    pub fn update(&mut self, kind: FrameKind, bits: u64, base_q_idx: u8) {
        let cost = match self.pending.take() {
            Some((k, c)) if k == kind => c,
            _ => return,
        };
        let slot = kind.slot();
        let obs = (bits.max(8) as f64 * qstep(base_q_idx).powf(EXP[slot]) / cost as f64).max(1e-9);
        if self.seen[slot] {
            let r = (obs / self.k[slot]).clamp(0.25, 4.0);
            self.k[slot] *= r.powf(0.4);
        } else {
            self.k[slot] = obs;
            self.seen[slot] = true;
            if slot == 2 && !self.seen[1] {
                self.k[1] = obs;
            }
            if slot == 1 && !self.seen[2] {
                self.k[2] = obs;
            }
        }
        match self.cfg.mode {
            RcMode::Bitrate { .. } => {
                self.buffer = (self.buffer + self.frame_bits - bits as f64)
                    .clamp(-self.buffer_size, self.buffer_size);
            }
            RcMode::TwoPass { .. } => {
                self.remaining -= bits as f64;
            }
            _ => {}
        }
        self.frame += 1;
    }

    fn boost(&self, kind: FrameKind) -> f64 {
        let still = 1.0 - self.ratio.clamp(0.0, 1.0);
        match kind {
            FrameKind::Key => 1.95 + 0.9 * still,
            FrameKind::Golden | FrameKind::AltRef => 1.12 + 0.3 * still,
            FrameKind::Inter => 1.0,
        }
    }

    fn model(&self, kind: FrameKind, cost: f64, step: f64) -> f64 {
        let slot = kind.slot();
        self.k[slot] * cost / step.powf(EXP[slot])
    }
}

fn predicted_key(i: usize, keyint: u32) -> bool {
    i == 0 || (keyint > 0 && i.is_multiple_of(keyint as usize))
}

fn two_pass_geo(stats: &[FrameAnalysis], keyint: u32) -> f64 {
    let (mut s, mut n) = (0.0, 0.0);
    for (i, a) in stats.iter().enumerate() {
        if !predicted_key(i, keyint) {
            s += (a.cost(FrameKind::Inter) as f64).ln();
            n += 1.0;
        }
    }
    if n > 0.0 { (s / n).exp() } else { 1.0 }
}

fn two_pass_modulation(cost: f64, geo: f64) -> f64 {
    ((cost / geo).powf(1.0 - QCOMP)).clamp(0.5, 2.0)
}

fn two_pass_suffix(stats: &[FrameAnalysis], keyint: u32, geo: f64) -> Vec<(f64, f64)> {
    let mut out = vec![(0.0, 0.0); stats.len()];
    let (mut sk, mut si) = (0.0, 0.0);
    let mut ratio = 0.5;
    let ratios: Vec<f64> = stats
        .iter()
        .map(|a| {
            if a.has_prev {
                ratio = 0.8 * ratio + 0.2 * a.inter_ratio().min(1.0);
            }
            ratio
        })
        .collect();
    for i in (0..stats.len()).rev() {
        if predicted_key(i, keyint) {
            let boost = 1.3 + 0.6 * (1.0 - ratios[i]);
            sk += stats[i].cost(FrameKind::Key) as f64 * boost.powf(EXP[0]);
        } else {
            let c = stats[i].cost(FrameKind::Inter) as f64;
            si += c * two_pass_modulation(c, geo).powf(-EXP[1]);
        }
        out[i] = (sk, si);
    }
    out
}
