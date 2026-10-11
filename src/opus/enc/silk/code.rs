use super::analysis::sym_bits;
use crate::opus::range::RangeEncoder;
use crate::opus::silk::tables::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cond {
    Independently,
    Conditionally,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Indices {
    pub signal_type: usize,
    pub quant_offset_type: usize,
    pub gains: [i32; 4],
    pub nlsf: [i32; 17],
    pub nlsf_interp_q2: i32,
    pub lag_index: i32,
    pub contour_index: usize,
    pub per_index: usize,
    pub ltp_index: [usize; 4],
    pub ltp_scale_index: usize,
    pub seed: i32,
}

pub(crate) struct Ctx {
    pub vad: bool,
    pub cond: Cond,
    pub fs_khz: usize,
    pub nb_subfr: usize,
    pub lpc_order: usize,
    pub ec_prev_signal_type: usize,
    pub ec_prev_lag_index: i32,
}

trait Sink {
    fn icdf(&mut self, s: usize, icdf: &[u8]);
}

struct Bits(f32);

impl Sink for Bits {
    fn icdf(&mut self, s: usize, icdf: &[u8]) {
        self.0 += sym_bits(icdf, s);
    }
}

impl Sink for RangeEncoder {
    fn icdf(&mut self, s: usize, icdf: &[u8]) {
        RangeEncoder::icdf(self, s, icdf, 8);
    }
}

fn nlsf_stage2_sym<S: Sink>(out: &mut S, cb: usize, v: i32) {
    let t = v + 4;
    if t <= 0 {
        out.icdf(0, NLSF_STAGE2[cb]);
        out.icdf((-t) as usize, &NLSF_EXT);
    } else if t >= 8 {
        out.icdf(8, NLSF_STAGE2[cb]);
        out.icdf((t - 8) as usize, &NLSF_EXT);
    } else {
        out.icdf(t as usize, NLSF_STAGE2[cb]);
    }
}

fn indices_to<S: Sink>(out: &mut S, ind: &Indices, ctx: &Ctx, with_gains: bool) {
    let ix = ind.signal_type * 2 + ind.quant_offset_type;
    if ctx.vad {
        out.icdf(ix - 2, &TYPE_ACTIVE);
    } else {
        out.icdf(ix, &TYPE_INACTIVE);
    }
    if with_gains {
        gains_to(out, ind, ctx);
    }
    let wb = ctx.lpc_order == 16;
    let s1 = (ind.signal_type >> 1) + if wb { 2 } else { 0 };
    let i1 = ind.nlsf[0] as usize;
    out.icdf(i1, NLSF_STAGE1[s1]);
    for i in 0..ctx.lpc_order {
        let cb = if wb {
            8 + NLSF_SEL_WB[i1][i] as usize
        } else {
            NLSF_SEL_NBMB[i1][i] as usize
        };
        nlsf_stage2_sym(out, cb, ind.nlsf[i + 1]);
    }
    if ctx.nb_subfr == 4 {
        out.icdf(ind.nlsf_interp_q2 as usize, &NLSF_INTERP);
    }
    if ind.signal_type == 2 {
        let mut absolute = true;
        if ctx.cond == Cond::Conditionally && ctx.ec_prev_signal_type == 2 {
            let d = ind.lag_index - ctx.ec_prev_lag_index;
            if (-8..=11).contains(&d) {
                out.icdf((d + 9) as usize, &PITCH_DELTA);
                absolute = false;
            } else {
                out.icdf(0, &PITCH_DELTA);
            }
        }
        if absolute {
            let half = ctx.fs_khz as i32 >> 1;
            out.icdf((ind.lag_index / half) as usize, &PITCH_HIGH);
            let low = match ctx.fs_khz {
                8 => PITCH_LOW[0],
                12 => PITCH_LOW[1],
                _ => PITCH_LOW[2],
            };
            out.icdf((ind.lag_index % half) as usize, low);
        }
        let contour = match (ctx.fs_khz, ctx.nb_subfr) {
            (8, 2) => PITCH_CONTOUR[0],
            (8, _) => PITCH_CONTOUR[1],
            (_, 2) => PITCH_CONTOUR[2],
            _ => PITCH_CONTOUR[3],
        };
        out.icdf(ind.contour_index, contour);
        out.icdf(ind.per_index, &LTP_PERIODICITY);
        for k in 0..ctx.nb_subfr {
            out.icdf(ind.ltp_index[k], LTP_FILTER[ind.per_index]);
        }
        if ctx.cond == Cond::Independently {
            out.icdf(ind.ltp_scale_index, &LTP_SCALE);
        }
    }
    out.icdf(ind.seed as usize, &UNIFORM4);
}

fn gains_to<S: Sink>(out: &mut S, ind: &Indices, ctx: &Ctx) {
    if ctx.cond == Cond::Conditionally {
        out.icdf(ind.gains[0] as usize, &DELTA_GAIN);
    } else {
        out.icdf((ind.gains[0] >> 3) as usize, GAIN_MSB[ind.signal_type]);
        out.icdf((ind.gains[0] & 7) as usize, &UNIFORM8);
    }
    for k in 1..ctx.nb_subfr {
        out.icdf(ind.gains[k] as usize, &DELTA_GAIN);
    }
}

pub(crate) fn indices_bits(ind: &Indices, ctx: &Ctx) -> f32 {
    let mut b = Bits(0.0);
    indices_to(&mut b, ind, ctx, false);
    b.0
}

pub(crate) fn gains_bits(ind: &Indices, ctx: &Ctx) -> f32 {
    let mut b = Bits(0.0);
    gains_to(&mut b, ind, ctx);
    b.0
}

pub(crate) fn write_indices(rc: &mut RangeEncoder, ind: &Indices, ctx: &Ctx) {
    indices_to(rc, ind, ctx, true);
}

pub(crate) struct PulsePlan {
    rate_level: usize,
    nls: [u8; 20],
    sums: [u8; 20],
    blocks: usize,
}

fn shell_to<S: Sink>(out: &mut S, p: &[i32]) {
    let n = p.len();
    if n == 1 {
        return;
    }
    let total: i32 = p.iter().sum();
    if total == 0 {
        return;
    }
    let h = n / 2;
    let left: i32 = p[..h].iter().sum();
    let tab = match n {
        16 => SHELL_16[(total - 1) as usize],
        8 => SHELL_8[(total - 1) as usize],
        4 => SHELL_4[(total - 1) as usize],
        _ => SHELL_2[(total - 1) as usize],
    };
    out.icdf(left as usize, tab);
    shell_to(out, &p[..h]);
    shell_to(out, &p[h..]);
}

fn count_table(t: usize, rate_level: usize) -> &'static [u8] {
    match t {
        0 => PULSE_COUNT[rate_level],
        10 => PULSE_COUNT[10],
        _ => PULSE_COUNT[9],
    }
}

fn plan_pulses(pulses: &[i32], frame_length: usize, signal_type: usize) -> (PulsePlan, f32) {
    let blocks = frame_length.div_ceil(16);
    let mut plan = PulsePlan {
        rate_level: 0,
        nls: [0; 20],
        sums: [0; 20],
        blocks,
    };
    let mut fixed = 0f32;
    let mut abs = [0i32; 16];
    for i in 0..blocks {
        let blk = &pulses[i * 16..i * 16 + 16];
        let mut nls = 0u8;
        loop {
            let s: i32 = blk.iter().map(|v| v.abs() >> nls).sum();
            if s <= 16 {
                plan.sums[i] = s as u8;
                break;
            }
            nls += 1;
        }
        plan.nls[i] = nls;
        for (a, v) in abs.iter_mut().zip(blk) {
            *a = v.abs() >> nls;
        }
        let mut b = Bits(0.0);
        shell_to(&mut b, &abs);
        fixed += b.0;
        fixed += nls as f32 * 16.0 * 0.98;
        for t in 1..nls as usize {
            fixed += sym_bits(count_table(t, 0), 17);
        }
        if nls > 0 {
            fixed += sym_bits(count_table(nls as usize, 0), plan.sums[i] as usize);
        }
        let s = plan.sums[i] as i32 | (nls as i32) << 5;
        if s > 0 {
            fixed += blk.iter().filter(|v| **v != 0).count() as f32
                * sign_bits(signal_type, (s & 0x1f).min(6) as usize);
        }
    }
    let mut best = f32::MAX;
    for rl in 0..9 {
        let mut b = sym_bits(RATE_LEVEL[signal_type >> 1], rl);
        for i in 0..blocks {
            b += if plan.nls[i] > 0 {
                sym_bits(PULSE_COUNT[rl], 17)
            } else {
                sym_bits(PULSE_COUNT[rl], plan.sums[i] as usize)
            };
        }
        if b < best {
            best = b;
            plan.rate_level = rl;
        }
    }
    (plan, best + fixed)
}

fn sign_bits(signal_type: usize, n: usize) -> f32 {
    let tab = EXC_SIGN[7 * (signal_type << 1) + n];
    0.5 * (sym_bits(tab, 0) + sym_bits(tab, 1))
}

pub(crate) fn pulses_bits(pulses: &[i32], frame_length: usize, signal_type: usize) -> f32 {
    plan_pulses(pulses, frame_length, signal_type).1
}

pub(crate) fn write_pulses(
    rc: &mut RangeEncoder,
    pulses: &[i32],
    frame_length: usize,
    signal_type: usize,
    quant_offset_type: usize,
) {
    let (plan, _) = plan_pulses(pulses, frame_length, signal_type);
    rc.icdf(plan.rate_level, RATE_LEVEL[signal_type >> 1], 8);
    for i in 0..plan.blocks {
        for t in 0..plan.nls[i] as usize {
            rc.icdf(17, count_table(t, plan.rate_level), 8);
        }
        let t = plan.nls[i] as usize;
        rc.icdf(plan.sums[i] as usize, count_table(t, plan.rate_level), 8);
    }
    let mut abs = [0i32; 16];
    for i in 0..plan.blocks {
        if plan.sums[i] > 0 {
            for (a, v) in abs.iter_mut().zip(&pulses[i * 16..i * 16 + 16]) {
                *a = v.abs() >> plan.nls[i];
            }
            shell_to(rc, &abs);
        }
    }
    for i in 0..plan.blocks {
        let nls = plan.nls[i] as u32;
        if nls > 0 {
            for v in &pulses[i * 16..i * 16 + 16] {
                let a = v.unsigned_abs();
                for b in (0..nls).rev() {
                    rc.icdf(((a >> b) & 1) as usize, &EXC_LSB, 8);
                }
            }
        }
    }
    let base = 7 * (quant_offset_type + (signal_type << 1));
    let len = (frame_length + 8) >> 4;
    for i in 0..len {
        let p = plan.sums[i] as i32 | (plan.nls[i] as i32) << 5;
        if p > 0 {
            let tab = EXC_SIGN[base + ((p & 0x1f) as usize).min(6)];
            for &q in &pulses[i * 16..i * 16 + 16] {
                if q != 0 {
                    rc.icdf(usize::from(q > 0), tab, 8);
                }
            }
        }
    }
}
