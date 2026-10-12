use crate::h264::enc::nal::SliceType;

pub fn qp2qscale(qp: f64) -> f64 {
    0.85 * 2f64.powf((qp - 12.0) / 6.0)
}
pub fn qscale2qp(q: f64) -> f64 {
    12.0 + 6.0 * (q / 0.85).log2()
}

const QCOMP: f64 = 0.6;
const CRF_TREE_SHIFT: f64 = 3.0;

fn type_idx(t: SliceType) -> usize {
    match t {
        SliceType::I => 0,
        SliceType::P => 1,
        SliceType::B => 2,
    }
}

fn type_factor(t: SliceType) -> f64 {
    match t {
        SliceType::I => 1.0 / 1.4,
        SliceType::P => 1.0,
        SliceType::B => 1.3,
    }
}

#[derive(Clone, Debug)]
pub enum RcKind {
    Qp(u8),
    Crf(f32),
    Abr { kbps: u32 },
}

pub struct RateControl {
    kind: RcKind,
    fps: f64,
    coef: [f64; 3],
    coef_n: [u32; 3],
    total_bits: f64,
    frames: u64,
    sum_s: f64,
    vbv_max_bits_per_frame: f64,
    vbv_size: f64,
    vbv_fill: f64,
    prior: Option<(f64, f64)>,
    inter_s: f64,
    inter_n: u64,
}

impl RateControl {
    pub fn new(
        kind: RcKind,
        fps: f64,
        vbv_maxrate_kbps: Option<u32>,
        vbv_buf_kbits: Option<u32>,
    ) -> Self {
        let max = vbv_maxrate_kbps.map_or(0.0, |k| k as f64 * 1000.0 / fps);
        let size = vbv_buf_kbits.map_or(0.0, |k| k as f64 * 1000.0);
        RateControl {
            kind,
            fps,
            coef: [1.6, 1.4, 1.2],
            coef_n: [0; 3],
            total_bits: 0.0,
            frames: 0,
            sum_s: 0.0,
            vbv_max_bits_per_frame: max,
            vbv_size: size,
            vbv_fill: size * 0.9,
            prior: None,
            inter_s: 0.0,
            inter_n: 0,
        }
    }

    pub fn frame_qp(&mut self, t: SliceType, cplx: f64) -> f64 {
        let ti = type_idx(t);
        let cplx = cplx.max(1.0);
        let qp = match &self.kind {
            RcKind::Qp(q) => {
                let q = *q as f64;
                return if t == SliceType::B {
                    (q + 2.0).min(51.0)
                } else {
                    q
                };
            }
            RcKind::Crf(c) => {
                let c = *c as f64 + CRF_TREE_SHIFT;
                return (c + match t {
                    SliceType::I => -2.0,
                    SliceType::P => 0.0,
                    SliceType::B => 1.5,
                })
                .clamp(0.0, 51.0);
            }
            RcKind::Abr { kbps } => {
                let bpf = *kbps as f64 * 1000.0 / self.fps;
                let s_here = self.coef[ti] * cplx.powf(QCOMP) / type_factor(t);
                let fps = self.fps;
                let (pn0, c0) = *self.prior.get_or_insert_with(|| {
                    (
                        fps.max(1.0),
                        if t == SliceType::I { 0.4 * cplx } else { cplx },
                    )
                });
                let per_frame = if self.inter_n > 0 {
                    self.inter_s / self.inter_n as f64
                } else {
                    self.coef[1] * c0.powf(QCOMP)
                };
                let fade = (1.0 - self.frames as f64 / pn0).max(0.0);
                let (pn, ps) = (pn0 * fade, pn0 * fade * per_frame);
                let s = self.sum_s + s_here + ps;
                let wanted = bpf * (self.frames as f64 + 1.0 + pn);
                let mut rf = s / wanted;
                let abr_buffer = bpf * self.fps.max(1.0);
                let overflow = (1.0 + (self.total_bits - bpf * self.frames as f64) / abr_buffer)
                    .clamp(0.5, 2.0);
                rf *= overflow;
                let q = rf * cplx.powf(1.0 - QCOMP) * type_factor(t);
                qscale2qp(q)
            }
        };
        let mut qp = qp.clamp(1.0, 51.0);
        if self.vbv_size > 0.0 {
            let fill = self.vbv_fill;
            for _ in 0..60 {
                let pred = self.coef[ti] * cplx / qp2qscale(qp);
                if pred <= fill * 0.8 || qp >= 51.0 {
                    break;
                }
                qp += 0.5;
            }
        }
        qp
    }

    pub fn update(&mut self, t: SliceType, qp: f64, cplx: f64, bits: u64) {
        let ti = type_idx(t);
        let cplx = cplx.max(1.0);
        let q = qp2qscale(qp);
        let c = bits as f64 * q / cplx;
        let n = self.coef_n[ti];
        let w = if n == 0 {
            1.0
        } else {
            0.35f64.max(1.0 / (n + 1) as f64)
        };
        self.coef[ti] = self.coef[ti] * (1.0 - w) + c * w;
        self.coef_n[ti] += 1;
        for k in 0..3 {
            if self.coef_n[k] == 0 {
                self.coef[k] = self.coef[ti] * [1.2, 1.0, 0.85][k] / [1.2, 1.0, 0.85][ti];
            }
        }
        let s_frame = self.coef[ti] * cplx.powf(QCOMP) / type_factor(t);
        self.sum_s += s_frame;
        if t != SliceType::I {
            self.inter_s += s_frame;
            self.inter_n += 1;
        }
        self.total_bits += bits as f64;
        self.frames += 1;
        if self.vbv_size > 0.0 {
            self.vbv_fill =
                (self.vbv_fill - bits as f64 + self.vbv_max_bits_per_frame).min(self.vbv_size);
            if self.vbv_fill < 0.0 {
                self.vbv_fill = 0.0;
            }
        }
    }
}
