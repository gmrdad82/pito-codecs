use super::code::{Cond, Indices};
use crate::opus::silk::lpc::{
    MAX_LPC_ORDER, div32_varq, inverse32_varq, nlsf_decode, nlsf2a, rshift_round, sat16, smlawb,
    smulwb, smulww,
};
use crate::opus::silk::tables::{
    CB_NB_10, CB_NB_20, CB_WB_10, CB_WB_20, LTP_TAPS_0, LTP_TAPS_1, LTP_TAPS_2,
};

pub(crate) const MAX_FRAME: usize = 320;
const MAX_SUBFR: usize = 80;
const LTP_ORDER: usize = 5;
const OUT_BUF: usize = MAX_FRAME + 2 * MAX_SUBFR;
pub(crate) const SHAPE_ORDER: usize = 16;
const HARM_BUF: usize = 512;

#[derive(Clone)]
pub(crate) struct DecState {
    pub fs_khz: usize,
    pub nb_subfr: usize,
    pub frame_length: usize,
    pub subfr_length: usize,
    pub ltp_mem_length: usize,
    pub lpc_order: usize,
    pub prev_gain_q16: i32,
    pub s_lpc_q14: [i32; MAX_LPC_ORDER],
    pub out_buf: [i16; OUT_BUF],
    pub last_gain_index: i32,
    pub first_frame_after_reset: bool,
    pub prev_nlsf_q15: [i32; MAX_LPC_ORDER],
    pub ec_prev_signal_type: usize,
    pub ec_prev_lag_index: i32,
    pub shv: [f32; SHAPE_ORDER],
    pub harm: [f32; HARM_BUF],
    pub harm_pos: usize,
}

impl DecState {
    pub(crate) fn new() -> DecState {
        DecState {
            fs_khz: 0,
            nb_subfr: 4,
            frame_length: 0,
            subfr_length: 0,
            ltp_mem_length: 0,
            lpc_order: 0,
            prev_gain_q16: 65536,
            s_lpc_q14: [0; MAX_LPC_ORDER],
            out_buf: [0; OUT_BUF],
            last_gain_index: 10,
            first_frame_after_reset: true,
            prev_nlsf_q15: [0; MAX_LPC_ORDER],
            ec_prev_signal_type: 0,
            ec_prev_lag_index: 0,
            shv: [0.0; SHAPE_ORDER],
            harm: [0.0; HARM_BUF],
            harm_pos: 0,
        }
    }

    pub(crate) fn set_fs(&mut self, fs_khz: usize, nb_subfr: usize) {
        self.nb_subfr = nb_subfr;
        self.subfr_length = 5 * fs_khz;
        let frame_length = nb_subfr * self.subfr_length;
        if self.fs_khz != fs_khz {
            self.ltp_mem_length = 20 * fs_khz;
            self.lpc_order = if fs_khz == 16 { 16 } else { 10 };
            self.first_frame_after_reset = true;
            self.last_gain_index = 10;
            self.out_buf = [0; OUT_BUF];
            self.s_lpc_q14 = [0; MAX_LPC_ORDER];
            self.shv = [0.0; SHAPE_ORDER];
            self.harm = [0.0; HARM_BUF];
        }
        self.fs_khz = fs_khz;
        self.frame_length = frame_length;
    }
}

#[derive(Default, Clone)]
pub(crate) struct Ctrl {
    pub gains_q16: [i32; 4],
    pub pred_coef_q12: [[i32; MAX_LPC_ORDER]; 2],
    pub pitch_l: [i32; 4],
    pub ltp_coef_q14: [i32; 4 * LTP_ORDER],
    pub ltp_scale_q14: i32,
    pub interp: bool,
}

pub(crate) fn log2lin(in_log_q7: i32) -> i32 {
    if in_log_q7 < 0 {
        return 0;
    }
    if in_log_q7 >= 3967 {
        return i32::MAX;
    }
    let out = 1i32 << (in_log_q7 >> 7);
    let frac = in_log_q7 & 0x7F;
    if in_log_q7 < 2048 {
        out + ((out * smlawb(frac, frac * (128 - frac), -174)) >> 7)
    } else {
        out + (out >> 7) * smlawb(frac, frac * (128 - frac), -174)
    }
}

pub(crate) fn gain_q16(index: i32) -> i32 {
    log2lin((smulwb(0x1D1C71, index) + 2090).min(3967))
}

pub(crate) fn gain_step(prev: i32, sym: i32, absolute: bool) -> i32 {
    let v = if absolute {
        sym.max(prev - 16)
    } else {
        let tmp = sym - 4;
        let thr = 2 * 36 - 64 + prev;
        if tmp > thr {
            prev + (tmp << 1) - thr
        } else {
            prev + tmp
        }
    };
    v.clamp(0, 63)
}

pub(crate) fn quantize_gains(want: &[f32], prev: i32, cond: Cond, out: &mut [i32; 4]) -> [i32; 4] {
    let mut p = prev;
    let mut idx = [0i32; 4];
    for (k, &w) in want.iter().enumerate() {
        if k == 0 && cond == Cond::Independently {
            let s = (w.round() as i32).clamp(0, 63);
            out[0] = s;
            p = gain_step(p, s, true);
        } else {
            let mut best = (f32::MAX, 0, p);
            for s in 0..41 {
                let v = gain_step(p, s, false);
                let e = (v as f32 - w).abs();
                if e < best.0 {
                    best = (e, s, v);
                }
            }
            out[k] = best.1;
            p = best.2;
        }
        idx[k] = p;
    }
    idx
}

pub(crate) fn decode_params(st: &mut DecState, ind: &Indices, cond: Cond) -> Ctrl {
    let mut ctrl = Ctrl::default();
    let nb = st.nb_subfr;
    let mut prev = st.last_gain_index;
    for k in 0..nb {
        prev = gain_step(prev, ind.gains[k], k == 0 && cond != Cond::Conditionally);
        ctrl.gains_q16[k] = gain_q16(prev);
    }
    st.last_gain_index = prev;
    let order = st.lpc_order;
    let nlsf = nlsf_decode(&ind.nlsf, order == 16);
    ctrl.pred_coef_q12[1] = nlsf2a(&nlsf[..order]);
    let interp_q2 = if st.first_frame_after_reset {
        4
    } else {
        ind.nlsf_interp_q2
    };
    if interp_q2 < 4 {
        let mut n0 = [0i32; MAX_LPC_ORDER];
        for i in 0..order {
            n0[i] = st.prev_nlsf_q15[i] + ((interp_q2 * (nlsf[i] - st.prev_nlsf_q15[i])) >> 2);
        }
        ctrl.pred_coef_q12[0] = nlsf2a(&n0[..order]);
    } else {
        ctrl.pred_coef_q12[0] = ctrl.pred_coef_q12[1];
    }
    ctrl.interp = interp_q2 < 4;
    st.prev_nlsf_q15 = nlsf;
    if ind.signal_type == 2 {
        let fs = st.fs_khz as i32;
        let min_lag = 2 * fs;
        let max_lag = 18 * fs;
        let lag = min_lag + ind.lag_index;
        for k in 0..nb {
            let off = match (st.fs_khz, nb) {
                (8, 2) => CB_NB_10[ind.contour_index][k],
                (8, _) => CB_NB_20[ind.contour_index][k],
                (_, 2) => CB_WB_10[ind.contour_index][k],
                _ => CB_WB_20[ind.contour_index][k],
            };
            ctrl.pitch_l[k] = (lag + off).clamp(min_lag, max_lag);
        }
        for k in 0..nb {
            let ix = ind.ltp_index[k];
            let taps = match ind.per_index {
                0 => LTP_TAPS_0[ix],
                1 => LTP_TAPS_1[ix],
                _ => LTP_TAPS_2[ix],
            };
            for i in 0..LTP_ORDER {
                ctrl.ltp_coef_q14[k * LTP_ORDER + i] = taps[i] << 7;
            }
        }
        ctrl.ltp_scale_q14 = [15565, 12288, 8192][ind.ltp_scale_index];
    }
    ctrl
}

fn lpc_analysis_filter(out: &mut [i16], input: &[i16], b: &[i32], d: usize) {
    let len = out.len();
    for ix in d..len {
        let mut acc: i32 = 0;
        for j in 0..d {
            acc = acc.wrapping_add((input[ix - 1 - j] as i32).wrapping_mul(b[j]));
        }
        let v = ((input[ix] as i32) << 12).wrapping_sub(acc);
        out[ix] = sat16(rshift_round(v, 12)) as i16;
    }
    for v in out.iter_mut().take(d.min(len)) {
        *v = 0;
    }
}

pub(crate) struct Shaping {
    pub syn: [[f32; SHAPE_ORDER]; 4],
    pub harm: [f32; 4],
    pub lag: [usize; 4],
    pub lambda: f32,
}

pub(crate) fn nsq(
    st: &mut DecState,
    ctrl: &Ctrl,
    signal_type: usize,
    quant_offset_type: usize,
    seed_init: i32,
    x: &[f32],
    shaping: &Shaping,
    pulses: &mut [i32],
    xq: &mut [i16],
) {
    let l = st.frame_length;
    let sub = st.subfr_length;
    let order = st.lpc_order;
    let ltp_mem = st.ltp_mem_length;
    let voiced = signal_type == 2;
    let mut s_ltp = [0i16; MAX_FRAME];
    let mut s_ltp_q15 = [0i32; 2 * MAX_FRAME];
    let mut s_lpc = [0i32; MAX_SUBFR + MAX_LPC_ORDER];
    let offset_q10 = [[100, 240], [32, 100]][signal_type >> 1][quant_offset_type];
    let off = offset_q10 as f32 / 1024.0;
    let mut seed = seed_init;
    s_lpc[..MAX_LPC_ORDER].copy_from_slice(&st.s_lpc_q14);
    let mut ltp_buf_idx = ltp_mem;
    let lambda = shaping.lambda;
    for k in 0..st.nb_subfr {
        let a_q12 = &ctrl.pred_coef_q12[k >> 1];
        let b_q14 = &ctrl.ltp_coef_q14[k * LTP_ORDER..(k + 1) * LTP_ORDER];
        let gain_q10 = ctrl.gains_q16[k] >> 6;
        let mut inv_gain_q31 = inverse32_varq(ctrl.gains_q16[k], 47);
        let gain_adj_q16 = if ctrl.gains_q16[k] != st.prev_gain_q16 {
            let g = div32_varq(st.prev_gain_q16, ctrl.gains_q16[k], 16);
            for v in s_lpc[..MAX_LPC_ORDER].iter_mut() {
                *v = smulww(g, *v);
            }
            g
        } else {
            1 << 16
        };
        st.prev_gain_q16 = ctrl.gains_q16[k];
        let lag = ctrl.pitch_l[k];
        if voiced {
            let lag_u = lag as usize;
            if k == 0 || (k == 2 && ctrl.interp) {
                let start_idx =
                    ltp_mem as isize - lag as isize - order as isize - (LTP_ORDER / 2) as isize;
                let start_idx = start_idx.max(0) as usize;
                if k == 2 {
                    st.out_buf[ltp_mem..ltp_mem + 2 * sub].copy_from_slice(&xq[..2 * sub]);
                }
                let src_off = start_idx + k * sub;
                lpc_analysis_filter(
                    &mut s_ltp[start_idx..ltp_mem],
                    &st.out_buf[src_off..src_off + ltp_mem - start_idx],
                    a_q12,
                    order,
                );
                if k == 0 {
                    inv_gain_q31 = smulwb(inv_gain_q31, ctrl.ltp_scale_q14) << 2;
                }
                for i in 0..(lag_u + LTP_ORDER / 2).min(ltp_mem) {
                    s_ltp_q15[ltp_buf_idx - i - 1] =
                        smulwb(inv_gain_q31, s_ltp[ltp_mem - i - 1] as i32);
                }
            } else if gain_adj_q16 != 1 << 16 {
                for i in 0..(lag_u + LTP_ORDER / 2).min(ltp_buf_idx) {
                    let p = ltp_buf_idx - i - 1;
                    s_ltp_q15[p] = smulww(gain_adj_q16, s_ltp_q15[p]);
                }
            }
        }
        let scale = 16_777_216.0 / gain_q10.max(1) as f32;
        let syn = &shaping.syn[k];
        let harm = shaping.harm[k];
        let hl = shaping.lag[k].clamp(2, HARM_BUF - 2);
        for i in 0..sub {
            let n = k * sub + i;
            seed = seed.wrapping_mul(196_314_165).wrapping_add(907_633_515);
            let mut ltp_pred = 0i32;
            if voiced {
                let p = ltp_buf_idx as isize - lag as isize + (LTP_ORDER / 2) as isize;
                let g = |o: isize| -> i32 {
                    if p - o >= 0 {
                        s_ltp_q15[(p - o) as usize]
                    } else {
                        0
                    }
                };
                let mut pred = 2i32;
                pred = smlawb(pred, g(0), b_q14[0]);
                pred = smlawb(pred, g(1), b_q14[1]);
                pred = smlawb(pred, g(2), b_q14[2]);
                pred = smlawb(pred, g(3), b_q14[3]);
                pred = smlawb(pred, g(4), b_q14[4]);
                ltp_pred = pred;
            }
            let mut lpc_pred = (order >> 1) as i32;
            for j in 0..order {
                lpc_pred = smlawb(lpc_pred, s_lpc[MAX_LPC_ORDER + i - 1 - j], a_q12[j]);
            }
            let mut fb = 0f32;
            for j in 0..SHAPE_ORDER {
                fb += syn[j] * st.shv[j];
            }
            let mut lt = 0f32;
            if harm != 0.0 {
                let hp = (st.harm_pos + HARM_BUF - hl) % HARM_BUF;
                let hm = (hp + HARM_BUF - 1) % HARM_BUF;
                let hn = (hp + 1) % HARM_BUF;
                lt = harm * (0.5 * st.harm[hp] + 0.25 * (st.harm[hm] + st.harm[hn]));
            }
            fb += lt;
            let target = x[n] + fb;
            let mut u = target * scale
                - lpc_pred.saturating_mul(16) as f32
                - if voiced { (ltp_pred << 1) as f32 } else { 0.0 };
            u /= 16384.0;
            if seed < 0 {
                u = -u;
            }
            let c = ((u - off).round() as i32).clamp(-1024, 1024);
            let mut best_q = 0;
            let mut best_cost = f32::MAX;
            for q in [c - 1, c, c + 1, 0] {
                let e = if q > 0 {
                    q as f32 - 0.078125 + off
                } else if q < 0 {
                    q as f32 + 0.078125 + off
                } else {
                    off
                };
                let d = u - e;
                let cost = d * d + lambda * q.unsigned_abs() as f32;
                if cost < best_cost {
                    best_cost = cost;
                    best_q = q;
                }
            }
            let q = best_q.clamp(-1024, 1024);
            let mut e = q << 14;
            if e > 0 {
                e -= 80 << 4;
            } else if e < 0 {
                e += 80 << 4;
            }
            e += offset_q10 << 4;
            if seed < 0 {
                e = -e;
            }
            pulses[n] = q;
            seed = seed.wrapping_add(q);
            let res = if voiced {
                let r = e.wrapping_add(ltp_pred << 1);
                s_ltp_q15[ltp_buf_idx] = r << 1;
                ltp_buf_idx += 1;
                r
            } else {
                e
            };
            let v = res.saturating_add(lpc_pred.saturating_mul(16));
            s_lpc[MAX_LPC_ORDER + i] = v;
            let y = sat16(rshift_round(smulww(v, gain_q10), 8)) as i16;
            xq[n] = y;
            let yf = y as f32;
            st.shv.copy_within(0..SHAPE_ORDER - 1, 1);
            st.shv[0] = yf - lt;
            st.harm[st.harm_pos] = yf;
            st.harm_pos = (st.harm_pos + 1) % HARM_BUF;
        }
        s_lpc.copy_within(sub..sub + MAX_LPC_ORDER, 0);
    }
    st.s_lpc_q14.copy_from_slice(&s_lpc[..MAX_LPC_ORDER]);
    let mv = ltp_mem - l;
    st.out_buf.copy_within(l..l + mv, 0);
    st.out_buf[mv..mv + l].copy_from_slice(&xq[..l]);
}
