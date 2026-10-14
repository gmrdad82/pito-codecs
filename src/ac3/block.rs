use super::bits::Bits;
use super::header::Header;
use super::tables::{
    BAPTAB, BNDSZ, BNDTAB, DBPBTAB, EXP2, FASTDEC, FASTGAIN, FLOORTAB, GROUP3, GROUP5, GROUP11,
    HTH, LATAB, MASKTAB, QUANTIZATION_BITS, SLOWDEC, SLOWGAIN, SPX_ATTEN,
};
use crate::media::{Error, Result};

pub const FBW: usize = 5;
pub const CPL: usize = 5;
pub const LFE: usize = 6;
pub const CHANNELS: usize = 7;
pub const N: usize = 256;

fn invalid(what: &str) -> Error {
    Error::Invalid(format!("AC-3 {what}"))
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Delta {
    pub nseg: u8,
    pub seg: [[u8; 3]; 8],
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct BaKey {
    start: u16,
    end: u16,
    zero: bool,
    sdcy: u8,
    fdcy: u8,
    sgain: u8,
    dbpb: u8,
    floor: u8,
    csnr: u8,
    fsnr: u8,
    fgain: u8,
    fleak: u8,
    sleak: u8,
    fscod: u8,
    delta: Delta,
}

#[derive(Clone)]
pub struct Chan {
    pub exp: [u8; N],
    pub bap: [u8; N],
    pub psd: [i16; N],
    pub mask: [i16; 50],
    pub delay: [f32; N],
    pub end: usize,
    pub blksw: bool,
    pub dith: bool,
    pub cpl: bool,
    pub spx: bool,
    pub gain: f32,
    pub fsnr: u8,
    pub fgain: u8,
    pub delta: Delta,
    pub new_exp: bool,
    key: Option<BaKey>,
    pub cpl_coord: [f32; 18],
    pub spx_coord: [f32; 18],
    pub spx_nblend: [f32; 18],
    pub spx_sblend: [f32; 18],
    pub spx_atten: Option<u8>,
}

impl Chan {
    fn new() -> Chan {
        Chan {
            exp: [24; N],
            bap: [0; N],
            psd: [0; N],
            mask: [0; 50],
            delay: [0.0; N],
            end: 0,
            blksw: false,
            dith: false,
            cpl: false,
            spx: false,
            gain: 1.0,
            fsnr: 0,
            fgain: 4,
            delta: Delta::default(),
            new_exp: false,
            key: None,
            cpl_coord: [0.0; 18],
            spx_coord: [0.0; 18],
            spx_nblend: [0.0; 18],
            spx_sblend: [0.0; 18],
            spx_atten: None,
        }
    }
}

#[derive(Clone)]
pub struct State {
    pub ch: [Chan; CHANNELS],
    pub cpl_in_use: bool,
    pub phsflginu: bool,
    pub cpl_begf: u8,
    pub cpl_begf_mant: usize,
    pub cpl_endf_mant: usize,
    pub cpl_nsubbnd: usize,
    pub cpl_nbnd: usize,
    pub cpl_bndstrc: [bool; 18],
    pub cpl_phsflg: [bool; 18],
    pub rematflg: [bool; 4],
    pub nremat: usize,
    pub sdcycod: u8,
    pub fdcycod: u8,
    pub sgaincod: u8,
    pub dbpbcod: u8,
    pub floorcod: u8,
    pub csnr: u8,
    pub cpl_fleak: u8,
    pub cpl_sleak: u8,
    pub spx_in_use: bool,
    pub spx_strtf: u8,
    pub spx_begin: usize,
    pub spx_end: usize,
    pub spx_bndstrc: [bool; 18],
    pub spx_nbnds: usize,
    pub spx_bndsz: [usize; 18],
    pub spx_lfsr: u32,
    pub dither: u32,
    pub nodither: bool,
    pub blk: usize,
    pub spec: Box<[[[f32; N]; CHANNELS]; 6]>,
    pub short: [[bool; CHANNELS]; 6],
    pub gain: [[f32; CHANNELS]; 6],
    #[cfg(test)]
    pub marks: Vec<usize>,
    #[cfg(test)]
    pub holes: Vec<usize>,
}

impl State {
    pub fn new() -> State {
        State {
            ch: std::array::from_fn(|_| Chan::new()),
            cpl_in_use: false,
            phsflginu: false,
            cpl_begf: 0,
            cpl_begf_mant: 0,
            cpl_endf_mant: 0,
            cpl_nsubbnd: 0,
            cpl_nbnd: 0,
            cpl_bndstrc: [false; 18],
            cpl_phsflg: [false; 18],
            rematflg: [false; 4],
            nremat: 0,
            sdcycod: 2,
            fdcycod: 1,
            sgaincod: 1,
            dbpbcod: 2,
            floorcod: 7,
            csnr: 0,
            cpl_fleak: 0,
            cpl_sleak: 0,
            spx_in_use: false,
            spx_strtf: 0,
            spx_begin: 0,
            spx_end: 0,
            spx_bndstrc: [false; 18],
            spx_nbnds: 0,
            spx_bndsz: [0; 18],
            spx_lfsr: 0x4A5B_6C7D,
            dither: 0x1234,
            nodither: false,
            blk: 0,
            spec: Box::new([[[0.0; N]; CHANNELS]; 6]),
            short: [[false; CHANNELS]; 6],
            gain: [[1.0; CHANNELS]; 6],
            #[cfg(test)]
            marks: Vec::new(),
            #[cfg(test)]
            holes: Vec::new(),
        }
    }
}

pub fn dynrng(v: u8) -> f32 {
    let x = ((v >> 5) as i32 ^ 4) - 4;
    let y = (32 + (v & 0x1F) as i32) as f32 / 64.0;
    if x + 1 >= 0 {
        y * (1u32 << (x + 1)) as f32
    } else {
        y * EXP2[(-(x + 1)) as usize]
    }
}

pub fn remat_bands(cplinu: bool, cplbegf: u8) -> usize {
    if !cplinu || cplbegf > 2 {
        4
    } else if cplbegf > 0 {
        3
    } else {
        2
    }
}

pub fn cpl_coords(b: &mut Bits, nbnd: usize, out: &mut [f32; 18]) -> Result<()> {
    let mstr = b.read(2)? as usize;
    for c in out.iter_mut().take(nbnd) {
        let e = b.read(4)? as usize;
        let m = b.read(4)?;
        let mant = if e == 15 {
            m as f32 / 16.0
        } else {
            (m + 16) as f32 / 32.0
        };
        *c = mant * EXP2[e + 3 * mstr];
    }
    Ok(())
}

pub fn exponents(
    b: &mut Bits,
    strategy: u8,
    absexp: u8,
    ngrps: usize,
    first: usize,
    end: usize,
    exp: &mut [u8; N],
) -> Result<()> {
    let size = match strategy {
        1 => 1,
        2 => 2,
        _ => 4,
    };
    let mut prev = absexp as i32;
    let mut bin = first;
    for _ in 0..ngrps {
        let g = b.read(7)? as i32;
        if g >= 125 {
            return Err(invalid("exponent group out of range"));
        }
        for d in [g / 25, (g % 25) / 5, g % 5] {
            prev += d - 2;
            let e = prev.clamp(0, 24) as u8;
            for _ in 0..size {
                if bin < end {
                    exp[bin] = e;
                }
                bin += 1;
            }
        }
    }
    Ok(())
}

pub fn chan_exponents(b: &mut Bits, strategy: u8, end: usize, exp: &mut [u8; N]) -> Result<()> {
    let absexp = b.read(4)? as u8;
    exp[0] = absexp;
    let ngrps = if end == 0 {
        0
    } else {
        match strategy {
            1 => (end - 1) / 3,
            2 => (end - 1 + 3) / 6,
            _ => (end - 1 + 9) / 12,
        }
    };
    exponents(b, strategy, absexp, ngrps, 1, end, exp)
}

pub fn cpl_exponents(b: &mut Bits, s: &mut State, strategy: u8) -> Result<()> {
    let absexp = (b.read(4)? << 1) as u8;
    let size = [1, 1, 2, 4][strategy as usize & 3];
    let (start, end) = (s.cpl_begf_mant, s.cpl_endf_mant);
    let ngrps = (end - start) / (size * 3);
    exponents(b, strategy, absexp, ngrps, start, end, &mut s.ch[CPL].exp)?;
    s.ch[CPL].new_exp = true;
    Ok(())
}

pub fn lfe_exponents(b: &mut Bits, s: &mut State) -> Result<()> {
    let c = &mut s.ch[LFE];
    let absexp = b.read(4)? as u8;
    c.exp[0] = absexp;
    exponents(b, 1, absexp, 2, 1, 7, &mut c.exp)?;
    c.new_exp = true;
    Ok(())
}

pub fn delta(b: &mut Bits, d: &mut Delta) -> Result<()> {
    let n = b.read(3)? as usize + 1;
    d.nseg = n as u8;
    for seg in d.seg.iter_mut().take(n) {
        *seg = [b.read(5)? as u8, b.read(4)? as u8, b.read(3)? as u8];
    }
    Ok(())
}

pub fn allocate(s: &mut State, nfchans: usize, lfeon: bool, fscod: u8) {
    let zero = s.csnr == 0
        && s.ch[..nfchans].iter().all(|c| c.fsnr == 0)
        && (!s.cpl_in_use || s.ch[CPL].fsnr == 0)
        && (!lfeon || s.ch[LFE].fsnr == 0);
    let run = |s: &mut State, ch: usize, start: usize, end: usize| {
        let c = &s.ch[ch];
        let cpl = ch == CPL;
        let key = BaKey {
            start: start as u16,
            end: end as u16,
            zero,
            sdcy: s.sdcycod,
            fdcy: s.fdcycod,
            sgain: s.sgaincod,
            dbpb: s.dbpbcod,
            floor: s.floorcod,
            csnr: s.csnr,
            fsnr: c.fsnr,
            fgain: c.fgain,
            fleak: if cpl { s.cpl_fleak } else { 0 },
            sleak: if cpl { s.cpl_sleak } else { 0 },
            fscod,
            delta: if ch == LFE { Delta::default() } else { c.delta },
        };
        if c.new_exp || c.key != Some(key) {
            if zero {
                s.ch[ch].bap.fill(0);
                if end > start {
                    psd_only(&mut s.ch[ch], start, end);
                }
            } else {
                bit_allocation(s, ch, start, end, fscod);
            }
            s.ch[ch].key = Some(key);
        }
        s.ch[ch].new_exp = false;
    };
    for ch in 0..nfchans {
        let end = s.ch[ch].end;
        run(s, ch, 0, end);
    }
    if s.cpl_in_use {
        let (start, end) = (s.cpl_begf_mant, s.cpl_endf_mant);
        run(s, CPL, start, end);
    }
    if lfeon {
        run(s, LFE, 0, 7);
    }
}

fn psd_only(c: &mut Chan, start: usize, end: usize) {
    for bin in start..end {
        c.psd[bin] = (3072 - ((c.exp[bin] as i32) << 7)) as i16;
    }
}

#[inline]
fn logadd(a: i32, b: i32) -> i32 {
    let c = a - b;
    let addr = ((c.abs() >> 1) as usize).min(255);
    if c >= 0 {
        a + LATAB[addr] as i32
    } else {
        b + LATAB[addr] as i32
    }
}

#[inline]
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

fn bit_allocation(s: &mut State, ch: usize, start: usize, end: usize, fscod: u8) {
    if end <= start {
        return;
    }
    let cpl = ch == CPL;
    let lfe = ch == LFE;
    let sdecay = SLOWDEC[s.sdcycod as usize];
    let fdecay = FASTDEC[s.fdcycod as usize];
    let sgain = SLOWGAIN[s.sgaincod as usize];
    let dbknee = DBPBTAB[s.dbpbcod as usize];
    let floor = FLOORTAB[s.floorcod as usize];
    let (fleak, sleak) = if cpl {
        ((s.cpl_fleak as i32) << 8, (s.cpl_sleak as i32) << 8)
    } else {
        (0, 0)
    };
    let c = &mut s.ch[ch];
    let fgain = FASTGAIN[c.fgain as usize];
    let snroffset = (((s.csnr as i32 - 15) << 4) + c.fsnr as i32) << 2;
    psd_only(c, start, end);
    let bndstrt = MASKTAB[start] as usize;
    let bndend = MASKTAB[end - 1] as usize + 1;
    let mut bndpsd = [0i32; 51];
    {
        let mut j = start;
        let mut k = bndstrt;
        loop {
            let lastbin = (BNDTAB[k] as usize + BNDSZ[k] as usize).min(end);
            let mut v = c.psd[j] as i32;
            j += 1;
            while j < lastbin {
                v = logadd(v, c.psd[j] as i32);
                j += 1;
            }
            bndpsd[k] = v;
            k += 1;
            if end <= lastbin {
                break;
            }
        }
    }
    let mut fastleak = fleak + 768;
    let mut slowleak = sleak + 768;
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
    if !lfe && c.delta.nseg > 0 {
        let mut band = 0usize;
        for seg in &c.delta.seg[..c.delta.nseg as usize] {
            band += seg[0] as usize;
            let d = seg[2] as i32;
            let delta = if d >= 4 { (d - 3) << 7 } else { (d - 4) << 7 };
            for _ in 0..seg[1] {
                if band < 50 {
                    mask[band] += delta;
                }
                band += 1;
            }
        }
    }
    for bin in bndstrt..bndend {
        c.mask[bin] = mask[bin].clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
    let mut i = start;
    let mut j = bndstrt;
    loop {
        let lastbin = (BNDTAB[j] as usize + BNDSZ[j] as usize).min(end);
        let m = ((mask[j] - snroffset - floor).max(0) & 0x1fe0) + floor;
        while i < lastbin {
            let addr = ((c.psd[i] as i32 - m) >> 5).clamp(0, 63) as usize;
            c.bap[i] = BAPTAB[addr];
            i += 1;
        }
        if i >= end {
            break;
        }
        j += 1;
    }
}

#[inline]
pub fn dither(lfsr: &mut u32) -> f32 {
    let s = *lfsr;
    let bit = ((s >> 15) ^ (s >> 13) ^ (s >> 12) ^ (s >> 10)) & 1;
    let s = ((s << 1) | bit) & 0xFFFF;
    *lfsr = s;
    (s as i32 - 0x8000) as f32 * (0.707 / 32768.0)
}

const KIND: [u8; 16] = [0, 1, 2, 4, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4];

const GROUP_BITS: [u32; 4] = [0, 5, 7, 7];
const GROUP_LEN: [u8; 4] = [0, 3, 3, 2];

struct Direct {
    bits: [u32; 16],
    shift: [u32; 16],
    mul: [f32; 16],
    add: [f32; 16],
}

const DIRECT: Direct = {
    let mut d = Direct {
        bits: [0; 16],
        shift: [0; 16],
        mul: [0.0; 16],
        add: [0.0; 16],
    };
    d.bits[3] = 3;
    d.mul[3] = 2.0 / 7.0;
    d.add[3] = -6.0 / 7.0;
    d.bits[5] = 4;
    d.mul[5] = 2.0 / 15.0;
    d.add[5] = -14.0 / 15.0;
    let mut bap = 6;
    while bap < 16 {
        let n = QUANTIZATION_BITS[bap] as u32;
        d.bits[bap] = n;
        d.shift[bap] = 32 - n;
        d.mul[bap] = EXP2[n as usize - 1];
        bap += 1;
    }
    d
};

#[derive(Default)]
pub struct Groups {
    vals: [[f32; 3]; 4],
    left: [u8; 4],
}

impl Groups {
    #[inline(always)]
    pub fn mantissa(&mut self, b: &mut Bits, bap: u8) -> f32 {
        let bap = bap as usize & 15;
        let k = KIND[bap] as usize;
        if k == 4 || k == 0 {
            let raw = b.take0(DIRECT.bits[bap]);
            let sh = DIRECT.shift[bap];
            ((raw << sh) as i32 >> sh) as f32 * DIRECT.mul[bap] + DIRECT.add[bap]
        } else {
            if self.left[k] == 0 {
                let code = b.take(GROUP_BITS[k]) as usize & 127;
                self.vals[k] = match k {
                    1 => GROUP3[code & 31],
                    2 => GROUP5[code],
                    _ => {
                        let [a, c] = GROUP11[code];
                        [a, c, 0.0]
                    }
                };
                self.left[k] = GROUP_LEN[k];
            }
            self.left[k] -= 1;
            self.vals[k][(GROUP_LEN[k] - 1 - self.left[k]) as usize]
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn chan_mantissas(
    b: &mut Bits,
    g: &mut Groups,
    c: &Chan,
    out: &mut [f32; N],
    start: usize,
    end: usize,
    dith: bool,
    lfsr: &mut u32,
) -> Result<()> {
    let end = end.min(N);
    for bin in start..end {
        let bap = c.bap[bin];
        let mut v = g.mantissa(b, bap);
        if bap == 0 && dith {
            v = dither(lfsr);
        }
        out[bin] = v * EXP2[c.exp[bin] as usize];
    }
    out[end.max(start)..].fill(0.0);
    b.check()
}

pub fn mantissas(s: &mut State, b: &mut Bits, nfchans: usize, lfeon: bool) -> Result<()> {
    let mut g = Groups::default();
    let mut got_cpl = false;
    let nodither = s.nodither;
    let spec = &mut s.spec[s.blk];
    for ch in 0..nfchans {
        let c = &s.ch[ch];
        let dith = c.dith && !nodither;
        chan_mantissas(b, &mut g, c, &mut spec[ch], 0, c.end, dith, &mut s.dither)?;
        if s.cpl_in_use && c.cpl && !got_cpl {
            let (start, end) = (s.cpl_begf_mant, s.cpl_endf_mant);
            chan_mantissas(
                b,
                &mut g,
                &s.ch[CPL],
                &mut spec[CPL],
                start,
                end,
                false,
                &mut s.dither,
            )?;
            got_cpl = true;
        }
    }
    if lfeon {
        chan_mantissas(
            b,
            &mut g,
            &s.ch[LFE],
            &mut spec[LFE],
            0,
            7,
            false,
            &mut s.dither,
        )?;
    }
    Ok(())
}

pub fn decouple(s: &mut State, nfchans: usize, acmod: u8) {
    let (start, end) = (s.cpl_begf_mant, s.cpl_endf_mant.min(N));
    let nsub = s.cpl_nsubbnd.min(18);
    let mut sbnd2bnd = [0usize; 18];
    let mut bnd = 0usize;
    for (sbnd, slot) in sbnd2bnd.iter_mut().enumerate().take(nsub) {
        if sbnd > 0 && !s.cpl_bndstrc[sbnd] {
            bnd += 1;
        }
        *slot = bnd;
    }
    let nodither = s.nodither;
    for ch in 0..nfchans {
        if !s.ch[ch].cpl {
            continue;
        }
        let dith = s.ch[ch].dith && !nodither;
        for (sb, &band) in sbnd2bnd.iter().enumerate().take(nsub) {
            let mut coord = s.ch[ch].cpl_coord[band.min(17)] * 8.0;
            if acmod == 2 && ch == 1 && s.phsflginu && s.cpl_phsflg[band.min(17)] {
                coord = -coord;
            }
            let lo = start + sb * 12;
            let hi = (lo + 12).min(end);
            for bin in lo..hi {
                let spec = &mut s.spec[s.blk];
                let v = if dith && s.ch[CPL].bap[bin] == 0 {
                    dither(&mut s.dither) * EXP2[s.ch[CPL].exp[bin] as usize]
                } else {
                    spec[CPL][bin]
                };
                spec[ch][bin] = v * coord;
            }
        }
    }
}

pub fn rematrix(s: &mut State) {
    let last = if s.cpl_in_use {
        37 + 12 * s.cpl_begf as usize
    } else {
        253
    };
    let bands = [(13, 25), (25, 37), (37, 61), (61, last.max(61))];
    let end = s.ch[0].end.min(s.ch[1].end);
    for (i, &(lo, hi)) in bands.iter().enumerate().take(s.nremat) {
        if !s.rematflg[i] {
            continue;
        }
        let hi = hi.min(end);
        let (a, rest) = s.spec[s.blk].split_at_mut(1);
        let (l, r) = (&mut a[0], &mut rest[0]);
        for bin in lo..hi {
            let (x, y) = (l[bin], r[bin]);
            l[bin] = x + y;
            r[bin] = x - y;
        }
    }
}

fn spx_noise(lfsr: &mut u32) -> f32 {
    let mut x = *lfsr;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *lfsr = x;
    ((x as f32 / u32::MAX as f32) * 2.0 - 1.0) * 1.732_050_8
}

fn spx_notch(coeffs: &mut [f32; N], at: usize, code: u8) {
    let row = SPX_ATTEN[(code & 0x1F) as usize];
    for (i, tap) in [row[0], row[1], row[2], row[1], row[0]].iter().enumerate() {
        if let Some(c) = coeffs.get_mut(at + i) {
            *c *= tap;
        }
    }
}

pub fn spectral_extension(s: &mut State, nfchans: usize) {
    let nbnds = s.spx_nbnds;
    if !s.spx_in_use || nbnds == 0 {
        return;
    }
    let copystart = 25 + 12 * s.spx_strtf as usize;
    let copyend = 25 + 12 * s.spx_begin;
    let begin = copyend;
    let end = 25 + 12 * s.spx_end;
    if copyend <= copystart || end <= begin {
        return;
    }
    let mut map = [0usize; N];
    let mut wrap = [false; 18];
    let mut n = 0usize;
    let mut idx = copystart;
    for bnd in 0..nbnds {
        let size = s.spx_bndsz[bnd];
        let mut wrapped = false;
        if idx + size > copyend {
            idx = copystart;
            wrapped = true;
        }
        for _ in 0..size {
            if idx == copyend {
                idx = copystart;
                wrapped = true;
            }
            if n < N {
                map[n] = idx;
                n += 1;
            }
            idx += 1;
        }
        wrap[bnd] = bnd > 0 && wrapped;
    }
    for ch in 0..nfchans {
        if !s.ch[ch].spx {
            continue;
        }
        let c = &mut s.ch[ch];
        let coeffs = &mut s.spec[s.blk][ch];
        for (i, &src) in map[..n].iter().enumerate() {
            let at = begin + i;
            if at < N && src < N {
                coeffs[at] = coeffs[src];
            }
        }
        if let Some(code) = c.spx_atten {
            if begin >= 2 {
                spx_notch(coeffs, begin - 2, code);
            }
            let mut band = begin;
            for (bnd, &w) in wrap.iter().enumerate().take(nbnds) {
                if w && band >= 2 {
                    spx_notch(coeffs, band - 2, code);
                }
                band += s.spx_bndsz[bnd];
            }
        }
        let mut lo = begin;
        for bnd in 0..nbnds {
            let size = s.spx_bndsz[bnd];
            let hi = (lo + size).min(N);
            let lo_c = lo.min(hi);
            let energy: f64 = coeffs[lo_c..hi].iter().map(|&v| v as f64 * v as f64).sum();
            let rms = if size > 0 {
                (energy / size as f64).sqrt() as f32
            } else {
                0.0
            };
            let nscale = rms * c.spx_nblend[bnd];
            let sblend = c.spx_sblend[bnd];
            let coord = c.spx_coord[bnd] * 32.0;
            for bin in lo_c..hi {
                let noise = spx_noise(&mut s.spx_lfsr);
                coeffs[bin] = (coeffs[bin] * sblend + noise * nscale) * coord;
            }
            lo += size;
        }
        c.end = c.end.max(end);
    }
}

pub fn read_block_ac3(s: &mut State, b: &mut Bits, h: &Header, blk: usize) -> Result<()> {
    let nfchans = h.nfchans;
    let acmod = h.acmod;
    for ch in 0..nfchans {
        s.ch[ch].blksw = b.flag()?;
    }
    for ch in 0..nfchans {
        s.ch[ch].dith = b.flag()?;
        #[cfg(test)]
        if s.ch[ch].dith {
            s.marks.push(b.pos() - 1);
        }
    }
    if b.flag()? {
        let g = dynrng(b.read(8)? as u8);
        for ch in 0..nfchans {
            s.ch[ch].gain = g;
        }
        s.ch[LFE].gain = g;
    } else if blk == 0 {
        for ch in 0..nfchans {
            s.ch[ch].gain = 1.0;
        }
        s.ch[LFE].gain = 1.0;
    }
    if acmod == 0 {
        if b.flag()? {
            s.ch[1].gain = dynrng(b.read(8)? as u8);
        } else if blk == 0 {
            s.ch[1].gain = 1.0;
        }
    }
    if b.flag()? {
        s.cpl_in_use = b.flag()?;
        if s.cpl_in_use {
            for ch in 0..nfchans {
                s.ch[ch].cpl = b.flag()?;
            }
            s.phsflginu = acmod == 2 && b.flag()?;
            s.cpl_begf = b.read(4)? as u8;
            let endf = b.read(4)? as usize;
            let nsub = 3 + endf as i32 - s.cpl_begf as i32;
            if nsub < 1 {
                return Err(invalid("coupling range ends before it starts"));
            }
            s.cpl_nsubbnd = nsub as usize;
            s.cpl_bndstrc[0] = false;
            for bnd in 1..s.cpl_nsubbnd {
                s.cpl_bndstrc[bnd] = b.flag()?;
            }
            s.cpl_begf_mant = 37 + 12 * s.cpl_begf as usize;
            s.cpl_endf_mant = 37 + 12 * (endf + 3);
            s.cpl_nbnd = s.cpl_nsubbnd
                - s.cpl_bndstrc[1..s.cpl_nsubbnd]
                    .iter()
                    .filter(|&&v| v)
                    .count();
        } else {
            for ch in 0..nfchans {
                s.ch[ch].cpl = false;
            }
        }
    } else if blk == 0 {
        return Err(invalid("first block without a coupling strategy"));
    }
    if s.cpl_in_use {
        let mut any = false;
        for ch in 0..nfchans {
            if s.ch[ch].cpl && b.flag()? {
                any = true;
                let nbnd = s.cpl_nbnd;
                cpl_coords(b, nbnd, &mut s.ch[ch].cpl_coord)?;
            }
        }
        if acmod == 2 && s.phsflginu && any {
            for bnd in 0..s.cpl_nbnd {
                s.cpl_phsflg[bnd] = b.flag()?;
            }
        }
    }
    if acmod == 2 {
        if b.flag()? {
            s.nremat = remat_bands(s.cpl_in_use, s.cpl_begf);
            for i in 0..s.nremat {
                s.rematflg[i] = b.flag()?;
            }
        } else if blk == 0 {
            s.nremat = 0;
        }
    }
    let cplexpstr = if s.cpl_in_use { b.read(2)? as u8 } else { 0 };
    let mut chexpstr = [0u8; FBW];
    for e in chexpstr.iter_mut().take(nfchans) {
        *e = b.read(2)? as u8;
    }
    let lfeexpstr = h.lfeon && b.flag()?;
    let mut chbwcod = [0u8; FBW];
    for ch in 0..nfchans {
        if chexpstr[ch] != 0 && !s.ch[ch].cpl {
            chbwcod[ch] = b.read(6)? as u8;
            if chbwcod[ch] > 60 {
                return Err(invalid("channel bandwidth code above 60"));
            }
        }
    }
    if s.cpl_in_use && cplexpstr != 0 {
        cpl_exponents(b, s, cplexpstr)?;
    }
    for ch in 0..nfchans {
        if chexpstr[ch] != 0 {
            let end = if s.ch[ch].cpl {
                s.cpl_begf_mant
            } else {
                37 + 3 * (chbwcod[ch] as usize + 12)
            };
            let c = &mut s.ch[ch];
            c.end = end;
            chan_exponents(b, chexpstr[ch], end, &mut c.exp)?;
            c.new_exp = true;
            b.skip(2)?;
        } else if blk == 0 {
            return Err(invalid("first block reuses exponents"));
        } else if s.ch[ch].cpl {
            s.ch[ch].end = s.cpl_begf_mant;
        }
    }
    if h.lfeon {
        s.ch[LFE].end = 7;
        if lfeexpstr {
            lfe_exponents(b, s)?;
        }
    }
    if b.flag()? {
        s.sdcycod = b.read(2)? as u8;
        s.fdcycod = b.read(2)? as u8;
        s.sgaincod = b.read(2)? as u8;
        s.dbpbcod = b.read(2)? as u8;
        s.floorcod = b.read(3)? as u8;
    } else if blk == 0 {
        return Err(invalid("first block without bit allocation parameters"));
    }
    if b.flag()? {
        s.csnr = b.read(6)? as u8;
        if s.cpl_in_use {
            s.ch[CPL].fsnr = b.read(4)? as u8;
            s.ch[CPL].fgain = b.read(3)? as u8;
        }
        for ch in 0..nfchans {
            s.ch[ch].fsnr = b.read(4)? as u8;
            s.ch[ch].fgain = b.read(3)? as u8;
        }
        if h.lfeon {
            s.ch[LFE].fsnr = b.read(4)? as u8;
            s.ch[LFE].fgain = b.read(3)? as u8;
        }
    } else if blk == 0 {
        return Err(invalid("first block without SNR offsets"));
    }
    if s.cpl_in_use && b.flag()? {
        s.cpl_fleak = b.read(3)? as u8;
        s.cpl_sleak = b.read(3)? as u8;
    }
    read_delta(s, b, nfchans, blk)?;
    if b.flag()? {
        let n = b.read(9)? as usize;
        b.skip(n * 8)?;
    }
    allocate(s, nfchans, h.lfeon, h.fscod);
    mantissas(s, b, nfchans, h.lfeon)
}

pub fn read_delta(s: &mut State, b: &mut Bits, nfchans: usize, blk: usize) -> Result<()> {
    if b.flag()? {
        let cplmode = if s.cpl_in_use { b.read(2)? } else { 3 };
        let mut modes = [3u32; FBW];
        for m in modes.iter_mut().take(nfchans) {
            *m = b.read(2)?;
        }
        let order =
            std::iter::once((CPL, cplmode)).chain(modes.into_iter().enumerate().take(nfchans));
        for (ch, mode) in order {
            match mode {
                1 => delta(b, &mut s.ch[ch].delta)?,
                2 => s.ch[ch].delta.nseg = 0,
                _ => {}
            }
        }
    } else if blk == 0 {
        for c in s.ch.iter_mut() {
            c.delta.nseg = 0;
        }
    }
    Ok(())
}

pub fn dsp(s: &mut State, nfchans: usize, acmod: u8) {
    if s.cpl_in_use {
        decouple(s, nfchans, acmod);
    }
    if acmod == 2 {
        rematrix(s);
    }
    spectral_extension(s, nfchans);
}

impl State {
    pub fn mark(&mut self, nfchans: usize, lfeon: bool) {
        let blk = self.blk;
        for ch in (0..nfchans).chain(lfeon.then_some(LFE)) {
            self.short[blk][ch] = self.ch[ch].blksw && ch != LFE;
            self.gain[blk][ch] = self.ch[ch].gain;
        }
    }
}
