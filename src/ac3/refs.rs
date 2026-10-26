#![allow(dead_code, clippy::needless_range_loop)]

pub mod window {
    pub const RUN: usize = 8;
    pub const EVERY: usize = 24;

    pub fn body(samples: &[f32], channels: usize) -> Vec<f32> {
        samples
            .chunks(RUN * channels.max(1))
            .step_by(EVERY)
            .flatten()
            .copied()
            .collect()
    }

    pub fn tail(samples: &[f32], channels: usize) -> &[f32] {
        &samples[samples.len().saturating_sub(RUN * channels.max(1))..]
    }

    pub fn window(samples: &[f32], channels: usize) -> Vec<f32> {
        let mut out = body(samples, channels);
        out.extend_from_slice(tail(samples, channels));
        out
    }
}

pub const AREA: &str = "ac3";
pub const SAMPLES: usize = 2 * 1536 - 256;
pub const DELAY: usize = 256;
pub const BOXED: [&str; 2] = ["ac3-51", "ac3-stereo"];
pub const BOX_EXTS: [&str; 2] = ["mkv", "mp4"];
pub const NFCHANS: [usize; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

#[derive(Debug, Clone, Copy)]
pub struct Case {
    pub name: &'static str,
    pub rate: u32,
    pub acmod: u8,
    pub lfe: bool,
    pub kbps: u32,
    pub bandwidth: u8,
    pub coupling: bool,
    pub remat: bool,
    pub short: bool,
}

const fn case(name: &'static str, rate: u32, acmod: u8, lfe: bool, kbps: u32) -> Case {
    Case {
        name,
        rate,
        acmod,
        lfe,
        kbps,
        bandwidth: 4,
        coupling: false,
        remat: acmod == 2,
        short: false,
    }
}

pub const MAIN: [Case; 3] = [
    case("ac3-51", 48_000, 7, true, 448),
    case("ac3-stereo", 44_100, 2, false, 192),
    case("ac3-mono", 48_000, 1, false, 96),
];

pub const EDGES: [Case; 10] = [
    case("edge-ac3-mono-32k", 32_000, 1, false, 64),
    Case {
        remat: false,
        ..case("edge-ac3-stereo-32k", 32_000, 2, false, 128)
    },
    case("edge-ac3-21", 48_000, 2, true, 192),
    case("edge-ac3-30", 44_100, 3, false, 256),
    case("edge-ac3-21back", 48_000, 4, false, 256),
    case("edge-ac3-31", 32_000, 5, false, 320),
    case("edge-ac3-22", 48_000, 6, false, 320),
    case("edge-ac3-31-lfe", 44_100, 5, true, 384),
    Case {
        short: true,
        ..case("edge-ac3-50-short", 48_000, 7, false, 448)
    },
    Case {
        coupling: true,
        bandwidth: 40,
        ..case("edge-ac3-51-cpl", 48_000, 7, true, 384)
    },
];

pub fn cases() -> impl Iterator<Item = &'static Case> {
    MAIN.iter().chain(EDGES.iter())
}

pub fn named(name: &str) -> &'static Case {
    cases()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no AC-3 case {name}"))
}

impl Case {
    pub fn stream(&self) -> String {
        format!("{}.ac3", self.name)
    }

    pub fn plain(&self) -> String {
        format!("{}.plain", self.name)
    }

    pub fn decoded(&self) -> String {
        format!("{}.f32", self.name)
    }

    pub fn nfchans(&self) -> usize {
        NFCHANS[self.acmod as usize]
    }

    pub fn channels(&self) -> u16 {
        (self.nfchans() + usize::from(self.lfe)) as u16
    }

    pub fn fscod(&self) -> u8 {
        match self.rate {
            48_000 => 0,
            44_100 => 1,
            _ => 2,
        }
    }

    pub fn spread(&self) -> bool {
        matches!(self.acmod, 4 | 5)
    }
}

pub fn boxed(name: &str, ext: &str) -> String {
    format!("box-{name}.{ext}")
}

pub fn signal(c: &Case, n: usize) -> Vec<f32> {
    let channels = c.channels() as usize;
    let mut seed = 0x1234_5678u32;
    let mut out = Vec::with_capacity(n * channels);
    for i in 0..n {
        let t = i as f32 / c.rate as f32;
        for ch in 0..channels {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            let lfe = c.lfe && ch == channels - 1;
            let f = if lfe { 70.0 } else { 180.0 * (ch as f32 + 1.0) };
            let high = if c.coupling && !lfe {
                0.05 * (std::f32::consts::TAU * (9000.0 + 700.0 * ch as f32) * t).sin()
            } else {
                0.0
            };
            let s = 0.3 * (std::f32::consts::TAU * f * t).sin()
                + 0.1 * (std::f32::consts::TAU * (2100.0 + 300.0 * ch as f32) * t).sin()
                + high
                + 1e-4 * noise;
            out.push(s);
        }
    }
    out
}

const BITRATES: [u32; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

pub fn frame_bytes(rate: u32, kbps: u32) -> Option<(u8, usize)> {
    let index = BITRATES.iter().position(|&b| b == kbps)?;
    let code = (index * 2) as u8;
    let bytes = match rate {
        48_000 => kbps as usize * 4,
        32_000 => kbps as usize * 6,
        44_100 => (kbps as usize * 1000 * 1536 / 44_100 / 16) * 2,
        _ => return None,
    };
    Some((code, bytes))
}

fn kbd() -> [f64; 256] {
    fn i0(x: f64) -> f64 {
        let (mut sum, mut term) = (1.0, 1.0);
        let q = x * x / 4.0;
        for k in 1..64 {
            term *= q / (k * k) as f64;
            sum += term;
            if term < sum * 1e-17 {
                break;
            }
        }
        sum
    }
    let mut w = [0f64; 257];
    let mut total = 0.0;
    for (n, v) in w.iter_mut().enumerate() {
        let r = 2.0 * n as f64 / 256.0 - 1.0;
        total += i0(std::f64::consts::PI * 5.0 * (1.0 - r * r).max(0.0).sqrt());
        *v = total;
    }
    std::array::from_fn(|n| (w[n] / total).sqrt())
}

fn synth(x: &[f64; 256], w: &[f64; 256]) -> [f64; 512] {
    use std::f64::consts::PI;
    let pre = |k: usize| {
        let a = 2.0 * PI * (8.0 * k as f64 + 1.0) / 4096.0;
        (-a.cos(), -a.sin())
    };
    let mut zr = [0f64; 128];
    let mut zi = [0f64; 128];
    for k in 0..128 {
        let (cr, ci) = pre(k);
        let (a, b) = (x[255 - 2 * k], x[2 * k]);
        zr[k] = a * cr - b * ci;
        zi[k] = b * cr + a * ci;
    }
    let mut re = [0f64; 128];
    let mut im = [0f64; 128];
    for n in 0..128 {
        let (mut sr, mut si) = (0.0, 0.0);
        for k in 0..128 {
            let a = 2.0 * PI * (k * n % 128) as f64 / 128.0;
            let (c, s) = (a.cos(), a.sin());
            sr += zr[k] * c - zi[k] * s;
            si += zr[k] * s + zi[k] * c;
        }
        let (c, s) = pre(n);
        re[n] = sr * c - si * s;
        im[n] = si * c + sr * s;
    }
    let row = |m: usize| {
        let n = m >> 1;
        match (m >> 7, m & 1) {
            (0, 0) => -im[64 + n],
            (0, _) => re[63 - n],
            (1 | 2, 0) => -re[n - 64],
            (1 | 2, _) => im[191 - n],
            (3, 0) => im[n - 192],
            _ => -re[319 - n],
        }
    };
    std::array::from_fn(|m| {
        let win = if m < 256 { w[m] } else { w[511 - m] };
        2.0 * row(m) * win
    })
}

pub struct Mdct {
    basis: Vec<[f64; 512]>,
    scale: f64,
}

impl Mdct {
    pub fn new() -> Mdct {
        let w = kbd();
        let basis: Vec<[f64; 512]> = (0..256)
            .map(|k| {
                let mut x = [0f64; 256];
                x[k] = 1.0;
                synth(&x, &w)
            })
            .collect();
        let scale = basis[0].iter().map(|v| v * v).sum::<f64>();
        Mdct { basis, scale }
    }

    pub fn forward(&self, seg: &[f64]) -> [f64; 256] {
        std::array::from_fn(|k| {
            self.basis[k]
                .iter()
                .zip(seg)
                .map(|(b, x)| b * x)
                .sum::<f64>()
                / self.scale
        })
    }

    pub fn inverse(&self, x: &[f64; 256]) -> [f64; 512] {
        let mut out = [0f64; 512];
        for (k, b) in self.basis.iter().enumerate() {
            if x[k] != 0.0 {
                for (o, v) in out.iter_mut().zip(b) {
                    *o += x[k] * v;
                }
            }
        }
        out
    }
}

impl Default for Mdct {
    fn default() -> Mdct {
        Mdct::new()
    }
}

#[path = "tables.rs"]
pub(crate) mod tables;

use tables::{
    BAPTAB, BNDSZ, BNDTAB, DBPBTAB, FASTDEC, FASTGAIN, FLOORTAB, HTH, LATAB, MASKTAB, SLOWDEC,
    SLOWGAIN,
};

pub const SDCY: u8 = 2;
pub const FDCY: u8 = 1;
pub const SGAIN: u8 = 1;
pub const DBPB: u8 = 2;
pub const FLOOR: u8 = 7;
pub const FGAIN: u8 = 4;

fn logadd(a: i32, b: i32) -> i32 {
    let c = a - b;
    let addr = ((c.abs() >> 1) as usize).min(255);
    if c >= 0 {
        a + LATAB[addr] as i32
    } else {
        b + LATAB[addr] as i32
    }
}

fn lowcomp(a: i32, b0: i32, b1: i32, bin: usize) -> i32 {
    if bin < 7 {
        if b0 + 256 == b1 {
            384
        } else if b0 > b1 {
            (a - 64).max(0)
        } else {
            a
        }
    } else if bin < 20 {
        if b0 + 256 == b1 {
            320
        } else if b0 > b1 {
            (a - 64).max(0)
        } else {
            a
        }
    } else {
        (a - 128).max(0)
    }
}

pub fn allocate(
    exp: &[u8],
    start: usize,
    end: usize,
    fscod: u8,
    snr: u16,
    cpl: bool,
    lfe: bool,
) -> Vec<u8> {
    let mut bap = vec![0u8; end];
    if end <= start {
        return bap;
    }
    let sdecay = SLOWDEC[SDCY as usize];
    let fdecay = FASTDEC[FDCY as usize];
    let sgain = SLOWGAIN[SGAIN as usize];
    let dbknee = DBPBTAB[DBPB as usize];
    let floor = FLOORTAB[FLOOR as usize];
    let fgain = FASTGAIN[FGAIN as usize];
    let (csnr, fsnr) = (i32::from(snr >> 4), i32::from(snr & 15));
    let snroffset = (((csnr - 15) << 4) + fsnr) << 2;
    let psd: Vec<i32> = (0..end).map(|b| 3072 - (i32::from(exp[b]) << 7)).collect();
    let bndstrt = MASKTAB[start] as usize;
    let bndend = MASKTAB[end - 1] as usize + 1;
    let mut bndpsd = [0i32; 51];
    let mut j = start;
    let mut k = bndstrt;
    loop {
        let lastbin = (BNDTAB[k] as usize + BNDSZ[k] as usize).min(end);
        let mut v = psd[j];
        j += 1;
        while j < lastbin {
            v = logadd(v, psd[j]);
            j += 1;
        }
        bndpsd[k] = v;
        k += 1;
        if end <= lastbin {
            break;
        }
    }
    let mut fastleak = 768;
    let mut slowleak = 768;
    let mut excite = [0i32; 50];
    if cpl || bndstrt != 0 {
        for bin in bndstrt..bndend {
            fastleak = (fastleak - fdecay).max(bndpsd[bin] - fgain);
            slowleak = (slowleak - sdecay).max(bndpsd[bin] - sgain);
            excite[bin] = fastleak.max(slowleak);
        }
    } else {
        let bp = |i: usize| bndpsd[i.min(50)];
        let mut lc = 0;
        if bndend > 0 {
            lc = lowcomp(lc, bp(0), bp(1), 0);
            excite[0] = bp(0) - fgain - lc;
        }
        if bndend > 1 {
            lc = lowcomp(lc, bp(1), bp(2), 1);
            excite[1] = bp(1) - fgain - lc;
        }
        let mut begin = 7.min(bndend);
        for bin in 2..7.min(bndend) {
            let last = lfe && bin == 6;
            if !last {
                lc = lowcomp(lc, bp(bin), bp(bin + 1), bin);
            }
            fastleak = bp(bin) - fgain;
            slowleak = bp(bin) - sgain;
            excite[bin] = fastleak - lc;
            if !last && bp(bin) <= bp(bin + 1) {
                begin = bin + 1;
                break;
            }
        }
        for bin in begin..22.min(bndend) {
            if !(lfe && bin == 6) {
                lc = lowcomp(lc, bp(bin), bp(bin + 1), bin);
            }
            fastleak = (fastleak - fdecay).max(bp(bin) - fgain);
            slowleak = (slowleak - sdecay).max(bp(bin) - sgain);
            excite[bin] = (fastleak - lc).max(slowleak);
        }
        for bin in 22..bndend {
            fastleak = (fastleak - fdecay).max(bp(bin) - fgain);
            slowleak = (slowleak - sdecay).max(bp(bin) - sgain);
            excite[bin] = fastleak.max(slowleak);
        }
    }
    let hth = &HTH[fscod as usize];
    let mut mask = [0i32; 50];
    for bin in bndstrt..bndend {
        let mut exc = excite[bin];
        if bndpsd[bin] < dbknee {
            exc += (dbknee - bndpsd[bin]) >> 2;
        }
        mask[bin] = exc.max(hth[bin] as i32);
    }
    let mut i = start;
    let mut j = bndstrt;
    loop {
        let lastbin = (BNDTAB[j] as usize + BNDSZ[j] as usize).min(end);
        let m = ((mask[j] - snroffset - floor).max(0) & 0x1fe0) + floor;
        while i < lastbin {
            let addr = ((psd[i] - m) >> 5).clamp(0, 63) as usize;
            bap[i] = BAPTAB[addr];
            i += 1;
        }
        if i >= end {
            break;
        }
        j += 1;
    }
    bap
}

const QBITS: [u32; 16] = [0, 5, 7, 3, 7, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16];
const LEVELS: [i32; 6] = [0, 3, 5, 7, 11, 15];

pub fn quantize(bap: u8, m: f64) -> u32 {
    let m = m.clamp(-1.0, 1.0);
    match bap {
        0 => 0,
        1..=5 => {
            let l = LEVELS[bap as usize];
            (((m * l as f64 + (l - 1) as f64) / 2.0).round() as i32).clamp(0, l - 1) as u32
        }
        _ => {
            let n = QBITS[bap as usize];
            let top = 1i64 << (n - 1);
            let v = (m * top as f64).round() as i64;
            (v.clamp(-top, top - 1) as u32) & ((1u32 << n) - 1)
        }
    }
}

pub fn level(bap: u8, code: u32) -> f64 {
    match bap {
        0 => 0.0,
        1..=5 => {
            let l = LEVELS[bap as usize];
            (2 * code as i32 - (l - 1)) as f64 / l as f64
        }
        _ => {
            let n = QBITS[bap as usize];
            let v = ((code << (32 - n)) as i32 >> (32 - n)) as f64;
            v / (1u64 << (n - 1)) as f64
        }
    }
}

#[derive(Default)]
pub struct Bits {
    pub bytes: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    pub fn put(&mut self, bits: u32, v: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                self.bytes.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    pub fn pos(&self) -> usize {
        self.bytes.len() * 8 + self.n as usize
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put(pad, 0);
        }
        self.bytes
    }
}

pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub fn crc1(frame: &[u8], five_eighths: usize) -> u16 {
    let mut v = u32::from(crc16(&frame[4..five_eighths]));
    for _ in 0..8 * (five_eighths - 4) + 16 {
        v = if v & 1 != 0 {
            (v ^ 0x1_8005) >> 1
        } else {
            v >> 1
        };
    }
    v as u16
}

fn smooth(e: &mut [u8]) {
    for i in 1..e.len() {
        e[i] = e[i].min(e[i - 1] + 2);
    }
    for i in (0..e.len().saturating_sub(1)).rev() {
        e[i] = e[i].min(e[i + 1] + 2);
    }
}

fn natural(v: f64) -> u8 {
    let a = v.abs();
    if a < 1.0 / f64::from(1u32 << 12) {
        return 24;
    }
    let e = (-(a.log2())).floor();
    (e.max(0.0) as u8).min(24)
}

struct Exps {
    strategy: u8,
    first: u8,
    deltas: Vec<u8>,
    exp: Vec<u8>,
}

fn channel_exps(spans: &[&[f64; 256]], end: usize, strategy: u8) -> Exps {
    let size = [1, 1, 2, 4][strategy as usize];
    let mut nat: Vec<u8> = (0..end)
        .map(|k| spans.iter().map(|s| natural(s[k])).min().unwrap_or(24))
        .collect();
    nat[0] = nat[0].min(15);
    let ngroups = (end - 1).div_ceil(size);
    let mut g: Vec<u8> = vec![nat[0]];
    for j in 0..ngroups {
        let lo = 1 + j * size;
        let hi = (lo + size).min(end);
        g.push(nat[lo..hi].iter().copied().min().unwrap_or(24));
    }
    let padded = g.len() - 1;
    let groups3 = padded.div_ceil(3) * 3;
    while g.len() - 1 < groups3 {
        let last = *g.last().unwrap_or(&24);
        g.push(last);
    }
    smooth(&mut g);
    g[0] = g[0].min(15);
    smooth(&mut g);
    let mut exp = vec![0u8; end];
    exp[0] = g[0];
    for (j, &v) in g[1..].iter().enumerate() {
        for k in 0..size {
            let bin = 1 + j * size + k;
            if bin < end {
                exp[bin] = v;
            }
        }
    }
    let deltas = g
        .windows(2)
        .map(|p| (p[1] as i32 - p[0] as i32 + 2) as u8)
        .collect();
    Exps {
        strategy,
        first: g[0],
        deltas,
        exp,
    }
}

fn cpl_exps(spans: &[&[f64; 256]], start: usize, end: usize) -> Exps {
    let mut g: Vec<u8> = (start..end)
        .map(|k| spans.iter().map(|s| natural(s[k])).min().unwrap_or(24))
        .collect();
    smooth(&mut g);
    let first = g[0] & !1;
    let mut all = vec![first];
    all.extend_from_slice(&g);
    let mut exp = vec![24u8; end];
    exp[start..end].copy_from_slice(&g);
    let deltas = all
        .windows(2)
        .map(|p| (p[1] as i32 - p[0] as i32 + 2) as u8)
        .collect();
    Exps {
        strategy: 1,
        first,
        deltas,
        exp,
    }
}

fn put_deltas(b: &mut Bits, deltas: &[u8]) {
    for d in deltas.chunks(3) {
        let get = |i: usize| u32::from(d.get(i).copied().unwrap_or(2));
        b.put(7, get(0) * 25 + get(1) * 5 + get(2));
    }
}

struct Mant {
    bap: u8,
    code: u32,
}

fn put_mantissas(b: &mut Bits, list: &[Mant]) {
    let mut used = vec![false; list.len()];
    for i in 0..list.len() {
        let m = &list[i];
        match m.bap {
            0 => {}
            1 | 2 | 4 => {
                if used[i] {
                    continue;
                }
                let len = if m.bap == 4 { 2 } else { 3 };
                let mut group = vec![m.code];
                used[i] = true;
                for (j, n) in list.iter().enumerate().skip(i + 1) {
                    if group.len() == len {
                        break;
                    }
                    if n.bap == m.bap && !used[j] {
                        group.push(n.code);
                        used[j] = true;
                    }
                }
                let l = LEVELS[m.bap as usize] as u32;
                while group.len() < len {
                    group.push((l - 1) / 2);
                }
                let code = group.iter().fold(0, |a, &c| a * l + c);
                b.put(QBITS[m.bap as usize], code);
            }
            _ => b.put(QBITS[m.bap as usize], m.code),
        }
    }
}

const REMAT: [(usize, usize); 4] = [(13, 25), (25, 37), (37, 61), (61, 253)];

pub struct Frame {
    pub bytes: Vec<u8>,
    pub dither: Vec<usize>,
}

pub struct Writer<'a> {
    pub case: &'a Case,
    mdct: Mdct,
}

impl<'a> Writer<'a> {
    pub fn new(case: &'a Case) -> Writer<'a> {
        Writer {
            case,
            mdct: Mdct::new(),
        }
    }

    pub fn encode(&self, pcm: &[f32], dither: bool) -> Result<Vec<Frame>, String> {
        let c = self.case;
        let ch = c.channels() as usize;
        let n = pcm.len() / ch;
        let blocks = (n + DELAY).div_ceil(256);
        let frames = blocks.div_ceil(6);
        let mut planes = vec![vec![0f64; frames * 1536 + 512]; ch];
        for i in 0..n {
            for (k, p) in planes.iter_mut().enumerate() {
                p[DELAY + i] = f64::from(pcm[i * ch + k]);
            }
        }
        let mut out = Vec::with_capacity(frames);
        for f in 0..frames {
            let spec: Vec<Vec<[f64; 256]>> = planes
                .iter()
                .map(|p| {
                    (0..6)
                        .map(|blk| {
                            let at = (f * 6 + blk) * 256;
                            self.mdct.forward(&p[at..at + 512])
                        })
                        .collect()
                })
                .collect();
            out.push(self.frame(&spec, dither)?);
        }
        Ok(out)
    }

    fn frame(&self, spec: &[Vec<[f64; 256]>], dither: bool) -> Result<Frame, String> {
        let (mut lo, mut hi) = (1u16, 1023u16);
        self.frame_at(spec, dither, lo)?;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.frame_at(spec, dither, mid).is_ok() {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        self.frame_at(spec, dither, lo)
    }

    fn frame_at(&self, spec: &[Vec<[f64; 256]>], dither: bool, snr: u16) -> Result<Frame, String> {
        let c = self.case;
        let nf = c.nfchans();
        let lfe = c.lfe;
        let (code, size) = frame_bytes(c.rate, c.kbps).ok_or(format!(
            "{}: no AC-3 frame size for {} kbps",
            c.name, c.kbps
        ))?;
        let mut b = Bits::default();
        let mut dith = Vec::new();
        b.put(16, 0x0b77);
        b.put(16, 0);
        b.put(2, u32::from(c.fscod()));
        b.put(6, u32::from(code));
        b.put(5, 8);
        b.put(3, 0);
        b.put(3, u32::from(c.acmod));
        if c.acmod & 1 != 0 && c.acmod != 1 {
            b.put(2, 0);
        }
        if c.acmod & 4 != 0 {
            b.put(2, 0);
        }
        if c.acmod == 2 {
            b.put(2, 0);
        }
        b.put(1, u32::from(lfe));
        b.put(5, 27);
        b.put(3, 0);
        b.put(2, 0);
        b.put(3, 0);
        let cpl_begf = 3u32;
        let cpl_endf = 8u32;
        let cpl_start = 37 + 12 * cpl_begf as usize;
        let cpl_end = 37 + 12 * (cpl_endf as usize + 3);
        let nsub = (3 + cpl_endf - cpl_begf) as usize;
        let full_end = 37 + 3 * (usize::from(c.bandwidth) + 12);
        let end = if c.coupling { cpl_start } else { full_end };
        let mut cpl_spec: Vec<[f64; 256]> = vec![[0f64; 256]; 6];
        let mut coords = vec![[[0f64; 18]; 5]; 2];
        let mut coded: Vec<Vec<[f64; 256]>> = spec.to_vec();
        if c.coupling {
            for half in 0..2 {
                for band in 0..nsub {
                    let lo = cpl_start + band * 12;
                    let hi = lo + 12;
                    let mut ecpl = 0f64;
                    let mut ech = [0f64; 5];
                    for blk in half * 3..half * 3 + 3 {
                        for bin in lo..hi {
                            let sum: f64 = (0..nf).map(|k| spec[k][blk][bin]).sum();
                            cpl_spec[blk][bin] = sum;
                            ecpl += sum * sum;
                            for (k, e) in ech.iter_mut().enumerate().take(nf) {
                                *e += spec[k][blk][bin].powi(2);
                            }
                        }
                    }
                    for k in 0..nf {
                        coords[half][k][band] = if ecpl > 0.0 {
                            ((ech[k] / ecpl).sqrt() / 8.0).min(0.99)
                        } else {
                            0.0
                        };
                    }
                }
            }
        }
        if c.remat {
            for blk in 0..6 {
                for &(lo, hi) in &REMAT {
                    for bin in lo..hi.min(end) {
                        let (l, r) = (spec[0][blk][bin], spec[1][blk][bin]);
                        coded[0][blk][bin] = (l + r) / 2.0;
                        coded[1][blk][bin] = (l - r) / 2.0;
                    }
                }
            }
        }
        let mut exps: Vec<Vec<u8>> = vec![Vec::new(); nf];
        let mut baps: Vec<Vec<u8>> = vec![Vec::new(); nf];
        let mut cpl_exp: Vec<u8> = Vec::new();
        let mut cpl_bap: Vec<u8> = Vec::new();
        let mut lfe_exp: Vec<u8> = Vec::new();
        let mut lfe_bap: Vec<u8> = Vec::new();
        let fscod = c.fscod();
        for blk in 0..6 {
            let new = blk == 0 || blk == 3;
            for _ in 0..nf {
                b.put(1, u32::from(c.short && (blk == 2 || blk == 4)));
            }
            for _ in 0..nf {
                if dither {
                    dith.push(b.pos());
                }
                b.put(1, u32::from(dither));
            }
            b.put(1, 0);
            if c.acmod == 0 {
                b.put(1, 0);
            }
            if blk == 0 {
                b.put(1, 1);
                b.put(1, u32::from(c.coupling));
                if c.coupling {
                    for _ in 0..nf {
                        b.put(1, 1);
                    }
                    if c.acmod == 2 {
                        b.put(1, 0);
                    }
                    b.put(4, cpl_begf);
                    b.put(4, cpl_endf);
                    for _ in 1..nsub {
                        b.put(1, 0);
                    }
                }
            } else {
                b.put(1, 0);
            }
            if c.coupling {
                for k in 0..nf {
                    b.put(1, u32::from(new));
                    if new {
                        b.put(2, 0);
                        for band in 0..nsub {
                            let v = coords[blk / 3][k][band];
                            let (e, m) = coord(v);
                            b.put(4, e);
                            b.put(4, m);
                        }
                    }
                }
            }
            if c.acmod == 2 {
                b.put(1, u32::from(blk == 0));
                if blk == 0 {
                    for _ in 0..4 {
                        b.put(1, u32::from(c.remat));
                    }
                }
            }
            let span: Vec<usize> = if blk < 3 {
                (0..3).collect()
            } else {
                (3..6).collect()
            };
            let strategy = |k: usize| if blk == 0 { 1u8 } else { 2 + (k % 2) as u8 };
            if c.coupling {
                b.put(2, if new { 1 } else { 0 });
            }
            for k in 0..nf {
                b.put(2, if new { u32::from(strategy(k)) } else { 0 });
            }
            if lfe {
                b.put(1, u32::from(new));
            }
            if new {
                for _ in 0..nf {
                    if !c.coupling {
                        b.put(6, u32::from(c.bandwidth));
                    }
                }
            }
            if new && c.coupling {
                let refs: Vec<&[f64; 256]> = span.iter().map(|&s| &cpl_spec[s]).collect();
                let e = cpl_exps(&refs, cpl_start, cpl_end);
                b.put(4, u32::from(e.first >> 1));
                put_deltas(&mut b, &e.deltas);
                cpl_bap = allocate(&e.exp, cpl_start, cpl_end, fscod, snr, true, false);
                cpl_exp = e.exp;
            }
            if new {
                for (k, slot) in exps.iter_mut().enumerate() {
                    let refs: Vec<&[f64; 256]> = span.iter().map(|&s| &coded[k][s]).collect();
                    let e = channel_exps(&refs, end, strategy(k));
                    b.put(4, u32::from(e.first));
                    put_deltas(&mut b, &e.deltas);
                    b.put(2, 0);
                    baps[k] = allocate(&e.exp, 0, end, fscod, snr, false, false);
                    *slot = e.exp;
                }
                if lfe {
                    let refs: Vec<&[f64; 256]> = span.iter().map(|&s| &coded[nf][s]).collect();
                    let e = channel_exps(&refs, 7, 1);
                    b.put(4, u32::from(e.first));
                    put_deltas(&mut b, &e.deltas);
                    lfe_bap = allocate(&e.exp, 0, 7, fscod, snr, false, true);
                    lfe_exp = e.exp;
                }
            }
            b.put(1, u32::from(blk == 0));
            if blk == 0 {
                b.put(2, u32::from(SDCY));
                b.put(2, u32::from(FDCY));
                b.put(2, u32::from(SGAIN));
                b.put(2, u32::from(DBPB));
                b.put(3, u32::from(FLOOR));
            }
            b.put(1, u32::from(blk == 0));
            if blk == 0 {
                b.put(6, u32::from(snr >> 4));
                let parts = nf + usize::from(c.coupling) + usize::from(lfe);
                for _ in 0..parts {
                    b.put(4, u32::from(snr & 15));
                    b.put(3, u32::from(FGAIN));
                }
            }
            if c.coupling {
                b.put(1, u32::from(blk == 0));
                if blk == 0 {
                    b.put(3, 0);
                    b.put(3, 0);
                }
            }
            b.put(1, 0);
            b.put(1, 0);
            let mut list = Vec::new();
            let mut got_cpl = false;
            for k in 0..nf {
                for (bin, &e) in exps[k].iter().enumerate().take(end) {
                    let bp = baps[k][bin];
                    list.push(Mant {
                        bap: bp,
                        code: quantize(bp, coded[k][blk][bin] * f64::from(1u32 << e)),
                    });
                }
                if c.coupling && !got_cpl {
                    for (bin, &e) in cpl_exp.iter().enumerate().take(cpl_end).skip(cpl_start) {
                        let bp = cpl_bap[bin];
                        list.push(Mant {
                            bap: bp,
                            code: quantize(bp, cpl_spec[blk][bin] * f64::from(1u32 << e)),
                        });
                    }
                    got_cpl = true;
                }
            }
            if lfe {
                for (bin, &e) in lfe_exp.iter().enumerate().take(7) {
                    let bp = lfe_bap[bin];
                    list.push(Mant {
                        bap: bp,
                        code: quantize(bp, coded[nf][blk][bin] * f64::from(1u32 << e)),
                    });
                }
            }
            put_mantissas(&mut b, &list);
        }
        let used = b.pos();
        let room = size * 8 - 18;
        if used > room {
            return Err(format!(
                "{}: {used} bits in a {}-bit frame at {} kbps; raise its bitrate",
                c.name,
                size * 8,
                c.kbps
            ));
        }
        while b.pos() < size * 8 - 16 {
            b.put(1, 0);
        }
        let mut bytes = b.finish();
        bytes.extend_from_slice(&[0, 0]);
        let five = (size / 2 / 2 + size / 2 / 8) * 2;
        let c1 = crc1(&bytes, five);
        bytes[2..4].copy_from_slice(&c1.to_be_bytes());
        let c2 = crc16(&bytes[five..size - 2]);
        bytes[size - 2..].copy_from_slice(&c2.to_be_bytes());
        Ok(Frame {
            bytes,
            dither: dith,
        })
    }
}

fn coord(v: f64) -> (u32, u32) {
    for e in 0..15u32 {
        let s = v * f64::from(1u32 << e);
        if s >= 0.5 {
            return (e, (s * 32.0 - 16.0).round().clamp(0.0, 15.0) as u32);
        }
    }
    (
        15,
        (v * f64::from(1u32 << 15) * 16.0).round().clamp(0.0, 15.0) as u32,
    )
}

pub fn stream(c: &Case, dither: bool) -> Result<(Vec<u8>, Vec<usize>), String> {
    let pcm = signal(c, SAMPLES);
    let frames = Writer::new(c).encode(&pcm, dither)?;
    let mut bytes = Vec::new();
    let mut marks = Vec::new();
    for f in frames {
        let base = bytes.len() * 8;
        marks.extend(f.dither.iter().map(|m| base + m));
        bytes.extend(f.bytes);
    }
    Ok((bytes, marks))
}

pub fn plain(c: &Case) -> Result<Vec<u8>, String> {
    let (mut bytes, marks) = stream(c, true)?;
    for m in marks {
        bytes[m / 8] &= !(0x80 >> (m % 8));
    }
    Ok(bytes)
}
