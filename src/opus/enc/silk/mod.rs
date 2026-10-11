mod analysis;
mod code;
mod nlsf;
mod nsq;
mod resample;

use super::Bandwidth;
use crate::opus::range::RangeEncoder;
use crate::opus::silk::lpc::nlsf2a;
use crate::opus::silk::tables::{MID_ONLY, STEREO_PRED_JOINT, UNIFORM3, UNIFORM5};
use analysis::{
    MAX_ORDER, a_q12_to_f32, a2nlsf, burg, lpc_residual, ltp_search, nlsf_weights, pitch_search,
};
use code::{
    Cond, Ctx, Indices, gains_bits, indices_bits, pulses_bits, write_indices, write_pulses,
};
use nsq::{DecState, MAX_FRAME, SHAPE_ORDER, Shaping, decode_params, nsq, quantize_gains};
use resample::Downsampler;

const X_HIST: usize = 320;
const X_BUF: usize = X_HIST + MAX_FRAME;
const PULSE_BUF: usize = MAX_FRAME + 16;
const MAX_FRAMES: usize = 3;
const NSQ_LAMBDA: f32 = 0.7;
const SHAPE_ANA: f32 = 0.8;
const SHAPE_SYN: f32 = 0.88;
const HARM_ANA: f32 = 0.0;
const HARM_SYN: f32 = 0.0;
const TILT: f32 = 0.0;
const LPC_HISTORY: f32 = 0.0;
const NLSF_LAMBDA: f32 = 0.0025;
const VOICED_CORR: f32 = 0.45;
const LTP_MIN_GAIN: f32 = 0.8;
const DELTA_MIN: i32 = -16;
const DELTA_MAX: i32 = 4;
const OFFSET_VOICED: f32 = -1.0;
const OFFSET_CORR: f32 = 0.0;
const OFFSET_UNVOICED: f32 = 0.0;
const OFFSET_INACTIVE: f32 = 3.0;
const QUALITY_STEP: f32 = 0.1;

struct Trial {
    bits: f32,
    st: DecState,
    syms: [i32; 4],
    pulses: [i32; PULSE_BUF],
    #[cfg(test)]
    xq: [i16; MAX_FRAME],
}

#[derive(Clone, Copy)]
struct Highpass {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Highpass {
    fn new(fc: f32, fs: f32) -> Highpass {
        let k = (std::f32::consts::PI * fc / fs).tan();
        let s2 = std::f32::consts::SQRT_2;
        let norm = 1.0 / (1.0 + s2 * k + k * k);
        Highpass {
            b: [norm, -2.0 * norm, norm],
            a: [2.0 * (k * k - 1.0) * norm, (1.0 - s2 * k + k * k) * norm],
            z: [0.0; 2],
        }
    }

    fn run(&mut self, x: &mut [f32]) {
        for v in x.iter_mut() {
            let y = self.b[0] * *v + self.z[0];
            self.z[0] = self.b[1] * *v - self.a[0] * y + self.z[1];
            self.z[1] = self.b[2] * *v - self.a[1] * y;
            *v = y;
        }
    }
}

struct Vad {
    floor: f32,
    hang: u32,
}

impl Vad {
    fn update(&mut self, db: f32) -> bool {
        if db < self.floor {
            self.floor = db;
        } else {
            self.floor += 0.08;
        }
        let active = db > self.floor + 9.0 && db > -62.0;
        if active {
            self.hang = 8;
            true
        } else if self.hang > 0 {
            self.hang -= 1;
            true
        } else {
            false
        }
    }
}

struct FrameOut {
    ind: Indices,
    ctx: Ctx,
    pulses: [i32; PULSE_BUF],
}

impl FrameOut {
    fn new() -> FrameOut {
        FrameOut {
            ind: Indices::default(),
            ctx: Ctx {
                vad: false,
                cond: Cond::Independently,
                fs_khz: 16,
                nb_subfr: 4,
                lpc_order: 16,
                ec_prev_signal_type: 0,
                ec_prev_lag_index: 0,
            },
            pulses: [0; PULSE_BUF],
        }
    }
}

struct Channel {
    st: DecState,
    x: [f32; X_BUF],
    e: [f32; X_BUF],
    hp: Highpass,
    vad: Vad,
    prev_lag: i32,
    coding: f32,
    #[cfg(test)]
    log: Vec<(f32, i16)>,
}

impl Channel {
    fn new() -> Channel {
        Channel {
            st: DecState::new(),
            x: [0.0; X_BUF],
            e: [0.0; X_BUF],
            hp: Highpass::new(60.0, 16000.0),
            vad: Vad {
                floor: -60.0,
                hang: 0,
            },
            prev_lag: 0,
            coding: 0.5,
            #[cfg(test)]
            log: Vec::new(),
        }
    }

    fn encode_frame(
        &mut self,
        cond: Cond,
        seed: i32,
        quality: f32,
        lambda: f32,
        cap: f32,
        out: &mut FrameOut,
    ) -> f32 {
        let fs = self.st.fs_khz;
        let l = self.st.frame_length;
        let sub = self.st.subfr_length;
        let nb = self.st.nb_subfr;
        let d = self.st.lpc_order;
        let start = X_HIST;
        let end = start + l;

        let energy = self.x[start..end].iter().map(|v| v * v).sum::<f32>() / l as f32;
        let db = 10.0 * (energy / (32768.0 * 32768.0) + 1e-12).log10();
        let vad = self.vad.update(db);

        let mut a = [0f32; MAX_ORDER];
        let seg = ((tune("SILK_SEG", LPC_HISTORY) * sub as f32) as usize).min(start);
        burg(&self.x[start - seg..end], d, &mut a[..d]);
        let mut g = 0.999f32;
        for v in a[..d].iter_mut() {
            *v *= g;
            g *= 0.999;
        }
        let mut target = [0i32; MAX_ORDER];
        a2nlsf(&mut a[..d], &mut target[..d]);
        let mut w = [0f32; MAX_ORDER];
        nlsf_weights(&target[..d], &mut w[..d]);

        let hist = 18 * fs + 2 + LTP_TAPS_HALF;
        let r0 = start - hist;
        lpc_residual(&self.x, &a[..d], r0, &mut self.e[r0..end]);
        let mut ncorr = [[0f32; 280]; 4];
        let pitch = if vad {
            Some(pitch_search(
                &self.e,
                start,
                fs,
                nb,
                self.prev_lag,
                &mut ncorr,
            ))
        } else {
            None
        };
        let mut signal_type = match &pitch {
            None => 0,
            Some(p) if p.corr > tune("SILK_VCORR", VOICED_CORR) => 2,
            Some(_) => 1,
        };

        let mut ind = Indices::default();
        let mut aq = [0f32; MAX_ORDER];
        let mut ltp = None;
        loop {
            nlsf::quantize(
                &target[..d],
                &w[..d],
                signal_type,
                NLSF_LAMBDA,
                &mut ind.nlsf,
            );
            let nq = crate::opus::silk::lpc::nlsf_decode(&ind.nlsf, d == 16);
            let a_q12 = nlsf2a(&nq[..d]);
            a_q12_to_f32(&a_q12[..d], &mut aq[..d]);
            lpc_residual(&self.x, &aq[..d], r0, &mut self.e[r0..end]);
            if signal_type != 2 {
                break;
            }
            let p = pitch.as_ref().map(|p| p.lags).unwrap_or([0; 4]);
            let r = ltp_search(&self.e, start, sub, nb, &p, 0.95, 0.5 * sub as f32 / 2.0);
            let err: f32 = r.err[..nb].iter().sum();
            let en: f32 = r.energy[..nb].iter().sum();
            if err > tune("SILK_LTPMIN", LTP_MIN_GAIN) * en {
                signal_type = 1;
                continue;
            }
            ltp = Some(r);
            break;
        }

        let mut base = [0f32; 4];
        for k in 0..nb {
            let half = (tune("SILK_GWIN", 0.5) * sub as f32) as usize;
            let a0 = start + k * sub - half;
            let a1 = (start + (k + 1) * sub + half).min(end);
            let s = &self.e[a0..a1];
            let en = s.iter().map(|v| v * v).sum::<f32>();
            let rms = (en / s.len() as f32).max(1.0).sqrt();
            base[k] = (128.0 * (rms.log2() + 16.0) - 2090.0) / (0x1D1C71 as f32 / 65536.0);
        }

        ind.signal_type = signal_type;
        ind.quant_offset_type = match signal_type {
            0 => tune("SILK_QOFFI", 1.0) as usize,
            1 => tune("SILK_QOFFU", 0.0) as usize,
            _ => 0,
        };
        ind.nlsf_interp_q2 = 4;
        ind.seed = seed;
        if let (Some(p), Some(r)) = (&pitch, &ltp) {
            ind.lag_index = p.lag_index;
            ind.contour_index = p.contour;
            ind.per_index = r.per_index;
            ind.ltp_index = r.ltp_index;
            ind.ltp_scale_index = 0;
        }
        let ctx = Ctx {
            vad,
            cond,
            fs_khz: fs,
            nb_subfr: nb,
            lpc_order: d,
            ec_prev_signal_type: self.st.ec_prev_signal_type,
            ec_prev_lag_index: self.st.ec_prev_lag_index,
        };
        let fixed = indices_bits(&ind, &ctx);

        let coding = self.coding;
        let g_ana = tune("SILK_GANA", SHAPE_ANA) - tune("SILK_GANAC", 0.0) * coding;
        let g_syn = tune("SILK_GSYN", SHAPE_SYN) + tune("SILK_GSYNC", 0.0) * coding;
        let mut a_ana = [0f32; MAX_ORDER];
        let mut a_syn = [0f32; MAX_ORDER];
        let (mut pa, mut ps) = (g_ana, g_syn);
        for j in 0..d {
            a_ana[j] = a[j] * pa;
            a_syn[j] = a[j] * ps;
            pa *= g_ana;
            ps *= g_syn;
        }
        let comp = tune("SILK_GMUL", 1.0) * pred_gain(&a_ana[..d]) / pred_gain(&a_syn[..d]);
        let voiced = signal_type == 2;
        let corr = pitch.as_ref().map(|p| p.corr).unwrap_or(0.0);
        let (f_ana, f_syn) = if voiced {
            (
                tune("SILK_FANA", HARM_ANA) * corr,
                tune("SILK_FSYN", HARM_SYN) * corr,
            )
        } else {
            (0.0, 0.0)
        };
        let tilt = tune("SILK_TILTM", TILT)
            * if voiced {
                0.25 + 0.2625 * tune("SILK_V", 1.0)
            } else {
                0.25
            };
        let mut lags = [0usize; 4];
        if voiced && let Some(p) = &pitch {
            for k in 0..nb {
                lags[k] = (p.lags[k].max(2) as usize).min(start - d - 4);
            }
        }
        let lmax = lags[..nb].iter().copied().max().unwrap_or(0);
        let s0 = start - lmax - 3;
        let mut sh = [0f32; X_BUF];
        for n in s0..end {
            let mut p = 0f32;
            for j in 0..d {
                p += a_ana[j] * self.x[n - 1 - j];
            }
            sh[n] = self.x[n] - p;
        }
        let mut xin = [0f32; MAX_FRAME];
        let wl = |n: usize, k: usize| -> f32 {
            let lag = lags[k];
            if f_ana == 0.0 || lag == 0 {
                sh[n]
            } else {
                sh[n] - f_ana * (0.5 * sh[n - lag] + 0.25 * (sh[n - lag - 1] + sh[n - lag + 1]))
            }
        };
        let mut wp = wl(start - 1, 0);
        for i in 0..l {
            let w = wl(start + i, i / sub);
            xin[i] = comp * (w - tilt * wp);
            wp = w;
        }
        let mut syn = [0f32; SHAPE_ORDER];
        syn[..d].copy_from_slice(&a_syn[..d]);
        let shaping = Shaping {
            syn: [syn; 4],
            harm: [f_syn; 4],
            lag: lags,
            lambda: lambda
                * if signal_type == 2 {
                    1.0
                } else {
                    tune("SILK_LAMU", 1.0)
                },
        };

        let run = |dlt: i32| -> Trial {
            let mut want = [0f32; 4];
            for k in 0..nb {
                want[k] = base[k] + dlt as f32;
            }
            let mut syms = [0i32; 4];
            quantize_gains(&want[..nb], self.st.last_gain_index, cond, &mut syms);
            let mut trial = ind;
            trial.gains = syms;
            let mut st = self.st.clone();
            let ctrl = decode_params(&mut st, &trial, cond);
            let mut pulses = [0i32; PULSE_BUF];
            let mut xq = [0i16; MAX_FRAME];
            nsq(
                &mut st,
                &ctrl,
                signal_type,
                trial.quant_offset_type,
                seed,
                &xin[..l],
                &shaping,
                &mut pulses,
                &mut xq,
            );
            let bits = fixed + gains_bits(&trial, &ctx) + pulses_bits(&pulses, l, signal_type);
            Trial {
                bits,
                st,
                syms,
                pulses,
                #[cfg(test)]
                xq,
            }
        };
        let corr = pitch.as_ref().map(|p| p.corr).unwrap_or(0.0);
        let off = match signal_type {
            2 => tune("SILK_OFFV", OFFSET_VOICED) - tune("SILK_OFFC", OFFSET_CORR) * corr,
            1 => tune("SILK_OFFU", OFFSET_UNVOICED),
            _ => tune("SILK_OFFI", OFFSET_INACTIVE),
        };
        let mut dlt = ((quality + off).round() as i32).clamp(DELTA_MIN, DELTA_MAX);
        let mut t = run(dlt);
        let mut tries = 1;
        while t.bits > cap && dlt < DELTA_MAX && tries < 6 {
            let step = (((t.bits - cap) / 40.0).ceil() as i32).max(1);
            dlt = (dlt + step).min(DELTA_MAX);
            t = run(dlt);
            tries += 1;
        }
        self.st = t.st;
        ind.gains = t.syms;
        self.st.ec_prev_signal_type = signal_type;
        if signal_type == 2 {
            self.st.ec_prev_lag_index = ind.lag_index;
            self.prev_lag = pitch.as_ref().map(|p| p.lags[nb - 1]).unwrap_or(0);
        } else {
            self.prev_lag = 0;
        }
        self.st.first_frame_after_reset = false;
        out.ind = ind;
        out.ctx = ctx;
        out.pulses = t.pulses;
        #[cfg(test)]
        for i in 0..l {
            self.log.push((self.x[start + i], t.xq[i]));
        }
        self.x.copy_within(l..l + X_HIST, 0);
        t.bits
    }
}

fn tune(name: &str, v: f32) -> f32 {
    #[cfg(test)]
    if let Ok(s) = std::env::var(name) {
        return s.parse().unwrap_or(v);
    }
    let _ = name;
    v
}

fn pred_gain(a: &[f32]) -> f32 {
    let d = a.len();
    let mut h = [0f32; 256];
    h[0] = 1.0;
    let mut e = 1.0f32;
    for n in 1..256 {
        let mut v = 0f32;
        for j in 0..d.min(n) {
            v += a[j] * h[n - 1 - j];
        }
        h[n] = v;
        e += v * v;
    }
    e.sqrt()
}

const LTP_TAPS_HALF: usize = 2 + 2;

pub(crate) struct SilkEncoder {
    channels: usize,
    fs_khz: usize,
    down: Option<Downsampler>,
    ch: Channel,
    frames: Vec<FrameOut>,
    buf: Vec<f32>,
    counter: u32,
    reservoir: f32,
    quality: f32,
    #[cfg(test)]
    types: Vec<usize>,
}

impl SilkEncoder {
    pub(crate) fn new(channels: usize) -> SilkEncoder {
        SilkEncoder {
            channels,
            fs_khz: 0,
            down: None,
            ch: Channel::new(),
            frames: (0..MAX_FRAMES).map(|_| FrameOut::new()).collect(),
            buf: vec![0.0; 960],
            counter: 0,
            reservoir: 0.0,
            quality: 0.0,
            #[cfg(test)]
            types: Vec::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.fs_khz = 0;
        self.down = None;
        self.ch = Channel::new();
    }

    pub(crate) fn encode(
        &mut self,
        rc: &mut RangeEncoder,
        pcm: &[f32],
        bw: Bandwidth,
        target_bits: u32,
    ) {
        let fs = match bw {
            Bandwidth::Narrowband => 8,
            Bandwidth::Mediumband => 12,
            _ => 16,
        };
        if fs != self.fs_khz {
            self.fs_khz = fs;
            self.down = Some(Downsampler::new(fs));
            self.ch.hp = Highpass::new(60.0, fs as f32 * 1000.0);
        }
        let nch = self.channels;
        let n48 = pcm.len() / nch;
        let ms = n48 / 48;
        let nb = if ms == 10 { 2 } else { 4 };
        let nframes = (ms / 20).max(1);
        let n = n48 / (48 / fs);
        let down = self.down.get_or_insert_with(|| Downsampler::new(fs));
        let it = pcm
            .chunks_exact(nch)
            .map(|f| f.iter().sum::<f32>() * 32768.0 / nch as f32);
        down.process(it, &mut self.buf[..n]);
        self.ch.hp.run(&mut self.buf[..n]);
        self.ch.st.set_fs(fs, nb);
        let header = (nframes + 1) * nch;
        let stereo = if nch == 2 { 9.0 } else { 0.0 };
        let per = (target_bits as f32 - header as f32) / nframes as f32 - stereo;
        let l = self.ch.st.frame_length;
        self.ch.coding = ((per * 20.0 / ms.max(10) as f32 - 200.0) / 400.0).clamp(0.0, 1.0);
        for f in 0..nframes {
            self.ch.x[X_HIST..X_HIST + l].copy_from_slice(&self.buf[f * l..(f + 1) * l]);
            let cond = if f == 0 {
                Cond::Independently
            } else {
                Cond::Conditionally
            };
            let seed = (self.counter & 3) as i32;
            self.counter = self.counter.wrapping_add(1);
            let lambda =
                tune("SILK_LAMBDA", NSQ_LAMBDA) * (self.quality - DELTA_MAX as f32).max(0.0).exp2();
            let used = self.ch.encode_frame(
                cond,
                seed,
                self.quality,
                lambda,
                3.0 * per,
                &mut self.frames[f],
            );
            self.reservoir = (self.reservoir + per - used).clamp(-20.0 * per, 20.0 * per);
            self.quality += tune("SILK_QSTEP", QUALITY_STEP) * (used.max(1.0) / per).log2() / 0.2
                - 0.01 * self.reservoir / per;
            self.quality = self
                .quality
                .clamp(DELTA_MIN as f32 - 4.0, DELTA_MAX as f32 + 8.0);
        }
        for f in 0..nframes {
            rc.bit_logp(self.frames[f].ctx.vad, 1);
        }
        rc.bit_logp(false, 1);
        if nch == 2 {
            for _ in 0..nframes {
                rc.bit_logp(false, 1);
            }
            rc.bit_logp(false, 1);
        }
        for f in 0..nframes {
            if nch == 2 {
                rc.icdf(12, &STEREO_PRED_JOINT, 8);
                for _ in 0..2 {
                    rc.icdf(1, &UNIFORM3, 8);
                    rc.icdf(2, &UNIFORM5, 8);
                }
                rc.icdf(1, &MID_ONLY, 8);
            }
            let fo = &self.frames[f];
            write_indices(rc, &fo.ind, &fo.ctx);
            write_pulses(
                rc,
                &fo.pulses,
                l,
                fo.ind.signal_type,
                fo.ind.quant_offset_type,
            );
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
