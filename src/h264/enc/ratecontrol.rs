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
                qp = (qp + 0.5).min(51.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn gop(n: usize, cut: Option<usize>) -> Vec<(SliceType, f64)> {
        (0..n)
            .map(|i| {
                let scale = if cut.is_some_and(|c| i >= c) {
                    20.0
                } else {
                    1.0
                };
                let t = if i % 30 == 0 || cut == Some(i) {
                    SliceType::I
                } else if i % 3 == 0 {
                    SliceType::P
                } else {
                    SliceType::B
                };
                let c = match t {
                    SliceType::I => 4e4,
                    SliceType::P => 1e4,
                    SliceType::B => 6e3,
                };
                (t, c * scale)
            })
            .collect()
    }

    fn drive(rc: &mut RateControl, frames: &[(SliceType, f64)], k: f64) -> (Vec<f64>, u64) {
        let mut qps = Vec::new();
        let mut total = 0;
        for &(t, cplx) in frames {
            let qp = rc.frame_qp(t, cplx);
            let bits = (k * cplx.max(1.0) / qp2qscale(qp)) as u64;
            rc.update(t, qp, cplx, bits);
            qps.push(qp);
            total += bits;
        }
        (qps, total)
    }

    fn in_range(qps: &[f64]) -> bool {
        qps.iter()
            .all(|q| q.is_finite() && (0.0..=51.0).contains(q))
    }

    #[test]
    fn qscale_and_qp_are_inverse() {
        assert!((qp2qscale(12.0) - 0.85).abs() < 1e-12);
        for qp in 0..=51 {
            let back = qscale2qp(qp2qscale(qp as f64));
            assert!((back - qp as f64).abs() < 1e-9, "qp {qp} came back {back}");
        }
        assert!((qp2qscale(18.0) / qp2qscale(12.0) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn constant_qp_offsets_b_frames_and_clamps() {
        let mut rc = RateControl::new(RcKind::Qp(30), 30.0, None, None);
        assert_eq!(rc.frame_qp(SliceType::I, 5.0), 30.0);
        assert_eq!(rc.frame_qp(SliceType::P, 5.0), 30.0);
        assert_eq!(rc.frame_qp(SliceType::B, 5.0), 32.0);
        let mut top = RateControl::new(RcKind::Qp(51), 30.0, None, None);
        assert_eq!(top.frame_qp(SliceType::B, 5.0), 51.0);
        let mut zero = RateControl::new(RcKind::Qp(0), 30.0, None, None);
        assert_eq!(zero.frame_qp(SliceType::I, f64::NAN), 0.0);
    }

    #[test]
    fn crf_offsets_by_type_and_clamps() {
        let mut rc = RateControl::new(RcKind::Crf(23.0), 30.0, None, None);
        assert_eq!(rc.frame_qp(SliceType::I, 1e4), 24.0);
        assert_eq!(rc.frame_qp(SliceType::P, 1e4), 26.0);
        assert_eq!(rc.frame_qp(SliceType::B, 1e4), 27.5);
        let mut hi = RateControl::new(RcKind::Crf(51.0), 30.0, None, None);
        assert_eq!(hi.frame_qp(SliceType::B, 1e4), 51.0);
        let mut lo = RateControl::new(RcKind::Crf(-10.0), 30.0, None, None);
        assert_eq!(lo.frame_qp(SliceType::I, 1e4), 0.0);
    }

    #[test]
    fn abr_lands_within_ten_percent_of_target() {
        for kbps in [300, 1000, 4000] {
            let mut rc = RateControl::new(RcKind::Abr { kbps }, 30.0, None, None);
            let frames = gop(75, None);
            let (qps, bits) = drive(&mut rc, &frames, 14.0);
            assert!(in_range(&qps), "{kbps} kbps: {qps:?}");
            let got = bits as f64 * 30.0 / 75.0 / 1000.0;
            let err = (got / kbps as f64 - 1.0).abs();
            assert!(err < 0.10, "{kbps} kbps target, {got:.0} kbps produced");
        }
    }

    #[test]
    fn abr_raises_qp_on_a_scene_cut_and_stays_in_range() {
        let mut rc = RateControl::new(RcKind::Abr { kbps: 1000 }, 30.0, None, None);
        let (qps, _) = drive(&mut rc, &gop(75, Some(40)), 14.0);
        assert!(in_range(&qps), "{qps:?}");
        assert!(
            qps[42] > qps[39],
            "qp before the cut {} after {}",
            qps[39],
            qps[42]
        );
    }

    #[test]
    fn abr_on_a_flat_clip_stays_in_range_and_under_budget() {
        let mut rc = RateControl::new(RcKind::Abr { kbps: 1000 }, 30.0, None, None);
        let flat: Vec<_> = gop(90, None).into_iter().map(|(t, _)| (t, 0.0)).collect();
        let (qps, bits) = drive(&mut rc, &flat, 14.0);
        assert!(in_range(&qps), "{qps:?}");
        let budget = 1000.0 * 1000.0 * 90.0 / 30.0;
        assert!(
            (bits as f64) <= budget,
            "{bits} bits over a {budget} bit budget"
        );
        assert!(qps[60..].iter().all(|&q| q <= qps[0]), "{:?}", &qps[60..]);
    }

    #[test]
    fn abr_extreme_targets_clamp_to_the_qp_range() {
        let mut low = RateControl::new(RcKind::Abr { kbps: 1 }, 30.0, None, None);
        let (qps, _) = drive(&mut low, &gop(60, None), 14.0);
        assert!(in_range(&qps));
        assert!(qps[20..].iter().all(|&q| q == 51.0), "{:?}", &qps[20..]);
        let mut high = RateControl::new(RcKind::Abr { kbps: u32::MAX }, 30.0, None, None);
        let (qps, _) = drive(&mut high, &gop(60, None), 14.0);
        assert!(qps.iter().all(|&q| q == 1.0), "{qps:?}");
        let mut zero_bits = RateControl::new(RcKind::Abr { kbps: 500 }, 30.0, None, None);
        let (qps, _) = drive(&mut zero_bits, &gop(40, None), 0.0);
        assert!(in_range(&qps), "{qps:?}");
    }

    #[test]
    fn vbv_raises_qp_but_never_past_51() {
        let mut cplx = 1.0;
        let mut raised = 0;
        while cplx < 1e10 {
            let mut plain = RateControl::new(RcKind::Abr { kbps: 1000 }, 30.0, None, None);
            let mut vbv = RateControl::new(RcKind::Abr { kbps: 1000 }, 30.0, Some(1000), Some(50));
            let a = plain.frame_qp(SliceType::I, cplx);
            let b = vbv.frame_qp(SliceType::I, cplx);
            assert!(b >= a, "cplx {cplx}: vbv {b} below plain {a}");
            assert!((1.0..=51.0).contains(&b), "cplx {cplx}: vbv qp {b}");
            if b > a {
                raised += 1;
            }
            cplx *= 1.07;
        }
        assert!(raised > 0, "the buffer never bit");
    }

    #[test]
    fn vbv_fill_drains_and_refills_without_going_negative() {
        let mut rc = RateControl::new(RcKind::Abr { kbps: 800 }, 25.0, Some(800), Some(400));
        for i in 0..50 {
            let t = if i % 25 == 0 {
                SliceType::I
            } else {
                SliceType::P
            };
            let qp = rc.frame_qp(t, 2e4);
            assert!((1.0..=51.0).contains(&qp));
            let bits = if i < 10 { 2_000_000 } else { 100 };
            rc.update(t, qp, 2e4, bits);
            assert!(rc.vbv_fill >= 0.0 && rc.vbv_fill <= rc.vbv_size);
        }
        assert_eq!(rc.vbv_fill, rc.vbv_size);
    }
}
