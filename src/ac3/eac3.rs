use super::aht::{self, BLOCKS};
use super::bits::Bits;
use super::block::{self, CHANNELS, CPL, FBW, Groups, LFE, N, State};
use super::header::{Header, Kind};
use crate::media::{Error, Result};

const fn mask(bits: &[usize]) -> [bool; 18] {
    let mut t = [false; 18];
    let mut i = 0;
    while i < bits.len() {
        t[bits[i]] = true;
        i += 1;
    }
    t
}

const SPX_BNDSTRC: [bool; 18] = mask(&[8, 10, 12, 14, 16]);
const CPL_BNDSTRC: [bool; 18] = mask(&[8, 10, 11, 13, 14, 15, 16, 17]);

const R: u8 = 0;
const D15: u8 = 1;
const D25: u8 = 2;
const D45: u8 = 3;

const FRAME_EXP: [[u8; 6]; 32] = [
    [D15, R, R, R, R, R],
    [D15, R, R, R, R, D45],
    [D15, R, R, R, D25, R],
    [D15, R, R, R, D45, D45],
    [D25, R, R, D25, R, R],
    [D25, R, R, D25, R, D45],
    [D25, R, R, D45, D25, R],
    [D25, R, R, D45, D45, D45],
    [D25, R, D15, R, R, R],
    [D25, R, D25, R, R, D45],
    [D25, R, D25, R, D25, R],
    [D25, R, D25, R, D45, D45],
    [D25, R, D45, D25, R, R],
    [D25, R, D45, D25, R, D45],
    [D25, R, D45, D45, D25, R],
    [D25, R, D45, D45, D45, D45],
    [D45, D15, R, R, R, R],
    [D45, D15, R, R, R, D45],
    [D45, D25, R, R, D25, R],
    [D45, D25, R, R, D45, D45],
    [D45, D25, R, D25, R, R],
    [D45, D25, R, D25, R, D45],
    [D45, D25, R, D45, D25, R],
    [D45, D25, R, D45, D45, D45],
    [D45, D45, D15, R, R, R],
    [D45, D45, D25, R, R, D45],
    [D45, D45, D25, R, D25, R],
    [D45, D45, D25, R, D45, D45],
    [D45, D45, D45, D25, R, R],
    [D45, D45, D45, D25, R, D45],
    [D45, D45, D45, D45, D25, R],
    [D45, D45, D45, D45, D45, D45],
];

fn invalid(what: &str) -> Error {
    Error::Invalid(format!("E-AC-3 {what}"))
}

#[derive(Default)]
struct AudFrm {
    ahte: bool,
    snroffststr: u8,
    blkswe: bool,
    dithflage: bool,
    bamode: bool,
    frmfgaincode: bool,
    dbaflde: bool,
    skipflde: bool,
    cplinu: [bool; 6],
    cplstre: [bool; 6],
    chexpstr: [[u8; FBW]; 6],
    cplexpstr: [u8; 6],
    lfeexpstr: [bool; 6],
    frmcsnroffst: u8,
    frmfsnroffst: u8,
    ahtinu: [bool; CHANNELS],
    spxatten: [Option<u8>; FBW],
    #[cfg(test)]
    flag_at: usize,
}

fn audfrm(b: &mut Bits, h: &Header) -> Result<AudFrm> {
    let mut a = AudFrm::default();
    let blocks = h.blocks;
    let nfchans = h.nfchans;
    let expstre = if blocks == 6 {
        let e = b.flag()?;
        a.ahte = b.flag()?;
        e
    } else {
        true
    };
    a.snroffststr = b.read(2)? as u8;
    let transproce = b.flag()?;
    a.blkswe = b.flag()?;
    a.dithflage = b.flag()?;
    #[cfg(test)]
    let flag_at = b.pos() - 1;
    a.bamode = b.flag()?;
    a.frmfgaincode = b.flag()?;
    a.dbaflde = b.flag()?;
    a.skipflde = b.flag()?;
    let spxattene = b.flag()?;
    let mut ncplblks = 0;
    if h.acmod > 1 {
        a.cplstre[0] = true;
        a.cplinu[0] = b.flag()?;
        let mut last = a.cplinu[0];
        ncplblks += last as usize;
        for blk in 1..blocks {
            a.cplstre[blk] = b.flag()?;
            if a.cplstre[blk] {
                last = b.flag()?;
            }
            a.cplinu[blk] = last;
            ncplblks += last as usize;
        }
    }
    if expstre {
        for blk in 0..blocks {
            if a.cplinu[blk] {
                a.cplexpstr[blk] = b.read(2)? as u8;
            }
            for ch in 0..nfchans {
                a.chexpstr[blk][ch] = b.read(2)? as u8;
            }
        }
    } else {
        if h.acmod > 1 && ncplblks > 0 {
            let row = FRAME_EXP[b.read(5)? as usize];
            a.cplexpstr[..blocks].copy_from_slice(&row[..blocks]);
        }
        for ch in 0..nfchans {
            let row = FRAME_EXP[b.read(5)? as usize];
            for blk in 0..blocks {
                a.chexpstr[blk][ch] = row[blk];
            }
        }
    }
    if h.lfeon {
        for blk in 0..blocks {
            a.lfeexpstr[blk] = b.flag()?;
        }
    }
    if h.kind == Kind::Independent && (blocks == 6 || b.flag()?) {
        b.skip(5 * nfchans)?;
    }
    if a.ahte {
        let ncplregs = (0..BLOCKS)
            .filter(|&blk| a.cplstre[blk] || a.cplexpstr[blk] != R)
            .count();
        if ncplblks == 6 && ncplregs == 1 {
            a.ahtinu[CPL] = b.flag()?;
        }
        for ch in 0..nfchans {
            if (0..BLOCKS).filter(|&blk| a.chexpstr[blk][ch] != R).count() == 1 {
                a.ahtinu[ch] = b.flag()?;
            }
        }
        if h.lfeon && (0..BLOCKS).filter(|&blk| a.lfeexpstr[blk]).count() == 1 {
            a.ahtinu[LFE] = b.flag()?;
        }
    }
    if a.snroffststr == 0 {
        a.frmcsnroffst = b.read(6)? as u8;
        a.frmfsnroffst = b.read(4)? as u8;
    }
    if transproce {
        for _ in 0..nfchans {
            if b.flag()? {
                b.skip(18)?;
            }
        }
    }
    if spxattene {
        for ch in 0..nfchans {
            if b.flag()? {
                a.spxatten[ch] = Some(b.read(5)? as u8);
            }
        }
    }
    if blocks != 1 && b.flag()? {
        let words = (h.bytes / 2).max(1);
        let bits = 4 + words.next_power_of_two().trailing_zeros() as usize;
        b.skip((blocks - 1) * bits)?;
    }
    #[cfg(test)]
    {
        a.flag_at = flag_at;
    }
    Ok(a)
}

fn exp_end(s: &State, ch: usize, chbwcod: u8) -> usize {
    if s.ch[ch].cpl {
        s.cpl_begf_mant
    } else if s.ch[ch].spx {
        25 + 12 * s.spx_begin
    } else {
        37 + 3 * (chbwcod as usize + 12)
    }
}

pub fn decode(s: &mut State, b: &mut Bits, h: &Header) -> Result<()> {
    let a = audfrm(b, h)?;
    let nfchans = h.nfchans;
    let lfeon = h.lfeon;
    let acmod = h.acmod;
    let mut firstcplcos = [true; FBW];
    let mut firstspxcos = [true; FBW];
    let mut firstcplleak = true;
    let mut first_cpl_strategy = true;
    s.cpl_in_use = false;
    s.spx_in_use = false;
    for ch in 0..FBW {
        s.ch[ch].cpl = false;
        s.ch[ch].spx = false;
        s.ch[ch].spx_atten = a.spxatten[ch];
    }
    let mut cache: Vec<[[f32; N]; BLOCKS]> = Vec::new();
    let mut slot = [usize::MAX; CHANNELS];
    let mut pending = a.ahtinu;
    for blk in 0..h.blocks {
        s.blk = blk;
        for ch in 0..nfchans {
            s.ch[ch].blksw = a.blkswe && b.flag()?;
        }
        #[cfg(test)]
        {
            if blk == 0 {
                s.holes = vec![a.flag_at];
            }
            s.holes.push(b.pos());
        }
        for ch in 0..nfchans {
            s.ch[ch].dith = !a.dithflage || b.flag()?;
            #[cfg(test)]
            if a.dithflage && s.ch[ch].dith {
                s.marks.push(b.pos() - 1);
            }
        }
        if b.flag()? {
            let g = block::dynrng(b.read(8)? as u8);
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
                s.ch[1].gain = block::dynrng(b.read(8)? as u8);
            } else if blk == 0 {
                s.ch[1].gain = 1.0;
            }
        }
        if blk == 0 || b.flag()? {
            spx_strategy(s, b, h, &mut firstspxcos)?;
        }
        if s.spx_in_use {
            spx_coords(s, b, nfchans, &mut firstspxcos)?;
        }
        let cplinu = a.cplinu[blk];
        if a.cplstre[blk] {
            if cplinu {
                if b.flag()? {
                    return Err(Error::Unsupported("E-AC-3 enhanced coupling".into()));
                }
                if acmod == 2 {
                    s.ch[0].cpl = true;
                    s.ch[1].cpl = true;
                } else {
                    for ch in 0..nfchans {
                        s.ch[ch].cpl = b.flag()?;
                    }
                }
                s.phsflginu = acmod == 2 && b.flag()?;
                s.cpl_begf = b.read(4)? as u8;
                let endf = if s.spx_in_use {
                    s.spx_begin as i32 - 4
                } else {
                    b.read(4)? as i32
                };
                let nsub = 3 + endf - s.cpl_begf as i32;
                if !(1..=18).contains(&nsub) {
                    return Err(invalid("coupling range ends before it starts"));
                }
                s.cpl_nsubbnd = nsub as usize;
                if b.flag()? {
                    s.cpl_bndstrc = [false; 18];
                    for bnd in 1..s.cpl_nsubbnd {
                        s.cpl_bndstrc[bnd] = b.flag()?;
                    }
                } else if first_cpl_strategy {
                    s.cpl_bndstrc = [false; 18];
                    for bnd in 1..s.cpl_nsubbnd {
                        s.cpl_bndstrc[bnd] = CPL_BNDSTRC
                            .get(s.cpl_begf as usize + bnd)
                            .copied()
                            .unwrap_or(false);
                    }
                }
                first_cpl_strategy = false;
                s.cpl_begf_mant = 37 + 12 * s.cpl_begf as usize;
                s.cpl_endf_mant = (37 + 12 * (endf + 3)) as usize;
                s.cpl_nbnd = s.cpl_nsubbnd
                    - s.cpl_bndstrc[1..s.cpl_nsubbnd]
                        .iter()
                        .filter(|&&v| v)
                        .count();
                s.cpl_in_use = true;
            } else {
                for ch in 0..nfchans {
                    s.ch[ch].cpl = false;
                }
                firstcplcos = [true; FBW];
                firstcplleak = true;
                s.phsflginu = false;
                s.cpl_in_use = false;
            }
        }
        if cplinu {
            let mut any = false;
            for ch in 0..nfchans {
                if s.ch[ch].cpl {
                    let coe = if firstcplcos[ch] {
                        firstcplcos[ch] = false;
                        true
                    } else {
                        b.flag()?
                    };
                    if coe {
                        any = true;
                        let nbnd = s.cpl_nbnd;
                        block::cpl_coords(b, nbnd, &mut s.ch[ch].cpl_coord)?;
                    }
                } else {
                    firstcplcos[ch] = true;
                }
            }
            if acmod == 2 && s.phsflginu && any {
                for bnd in 0..s.cpl_nbnd {
                    s.cpl_phsflg[bnd] = b.flag()?;
                }
            }
        }
        if acmod == 2 && (blk == 0 || b.flag()?) {
            s.nremat = if cplinu {
                block::remat_bands(true, s.cpl_begf)
            } else if s.spx_in_use && s.spx_begin < 4 {
                3
            } else {
                4
            };
            for i in 0..s.nremat {
                s.rematflg[i] = b.flag()?;
            }
        }
        let mut chbwcod = [0u8; FBW];
        for ch in 0..nfchans {
            if a.chexpstr[blk][ch] != R && !s.ch[ch].cpl && !s.ch[ch].spx {
                chbwcod[ch] = b.read(6)? as u8;
                if chbwcod[ch] > 60 {
                    return Err(invalid("channel bandwidth code above 60"));
                }
            }
        }
        if cplinu && a.cplexpstr[blk] != R {
            block::cpl_exponents(b, s, a.cplexpstr[blk])?;
        }
        for ch in 0..nfchans {
            let end = exp_end(s, ch, chbwcod[ch]);
            if a.chexpstr[blk][ch] != R {
                let c = &mut s.ch[ch];
                c.end = end;
                block::chan_exponents(b, a.chexpstr[blk][ch], end, &mut c.exp)?;
                c.new_exp = true;
                b.skip(2)?;
            } else if blk == 0 {
                return Err(invalid("first block reuses exponents"));
            } else if s.ch[ch].cpl || s.ch[ch].spx {
                s.ch[ch].end = end;
            }
        }
        if lfeon {
            s.ch[LFE].end = 7;
            if a.lfeexpstr[blk] {
                block::lfe_exponents(b, s)?;
            } else if blk == 0 {
                return Err(invalid("first block reuses LFE exponents"));
            }
        }
        if a.bamode && b.flag()? {
            s.sdcycod = b.read(2)? as u8;
            s.fdcycod = b.read(2)? as u8;
            s.sgaincod = b.read(2)? as u8;
            s.dbpbcod = b.read(2)? as u8;
            s.floorcod = b.read(3)? as u8;
        } else if blk == 0 {
            s.sdcycod = 2;
            s.fdcycod = 1;
            s.sgaincod = 1;
            s.dbpbcod = 2;
            s.floorcod = 7;
        }
        if a.snroffststr == 0 {
            if blk == 0 {
                s.csnr = a.frmcsnroffst;
                for c in s.ch.iter_mut() {
                    c.fsnr = a.frmfsnroffst;
                }
            }
        } else if blk == 0 || b.flag()? {
            s.csnr = b.read(6)? as u8;
            if a.snroffststr == 1 {
                let f = b.read(4)? as u8;
                for c in s.ch.iter_mut() {
                    c.fsnr = f;
                }
            } else {
                if cplinu {
                    s.ch[CPL].fsnr = b.read(4)? as u8;
                }
                for ch in 0..nfchans {
                    s.ch[ch].fsnr = b.read(4)? as u8;
                }
                if lfeon {
                    s.ch[LFE].fsnr = b.read(4)? as u8;
                }
            }
        }
        if a.frmfgaincode && b.flag()? {
            if cplinu {
                s.ch[CPL].fgain = b.read(3)? as u8;
            }
            for ch in 0..nfchans {
                s.ch[ch].fgain = b.read(3)? as u8;
            }
            if lfeon {
                s.ch[LFE].fgain = b.read(3)? as u8;
            }
        } else {
            for c in s.ch.iter_mut() {
                c.fgain = 4;
            }
        }
        if h.kind == Kind::Independent && b.flag()? {
            b.skip(10)?;
        }
        if cplinu {
            let leak = if firstcplleak {
                firstcplleak = false;
                true
            } else {
                b.flag()?
            };
            if leak {
                s.cpl_fleak = b.read(3)? as u8;
                s.cpl_sleak = b.read(3)? as u8;
            }
        }
        if a.dbaflde {
            block::read_delta(s, b, nfchans, blk)?;
        } else if blk == 0 {
            for c in s.ch.iter_mut() {
                c.delta.nseg = 0;
            }
        }
        if a.skipflde && b.flag()? {
            let n = b.read(9)? as usize;
            b.skip(n * 8)?;
        }
        block::allocate(s, nfchans, lfeon, h.fscod);
        if a.ahte {
            mixed(
                s,
                b,
                nfchans,
                lfeon,
                cplinu,
                &mut pending,
                &mut slot,
                &mut cache,
            )?;
            for ch in (0..CHANNELS).filter(|&ch| slot[ch] != usize::MAX) {
                let src = &cache[slot[ch]][blk];
                let (lo, hi) = if ch == CPL {
                    (s.cpl_begf_mant, s.cpl_endf_mant.min(N))
                } else {
                    (0, s.ch[ch].end.min(N))
                };
                let out = &mut s.spec[blk][ch];
                out.fill(0.0);
                out[lo.min(hi)..hi].copy_from_slice(&src[lo.min(hi)..hi]);
            }
        } else {
            block::mantissas(s, b, nfchans, lfeon)?;
        }
        block::dsp(s, nfchans, acmod);
        s.mark(nfchans, lfeon);
    }
    #[cfg(test)]
    s.holes.push(b.pos());
    Ok(())
}

fn spx_strategy(s: &mut State, b: &mut Bits, h: &Header, first: &mut [bool; FBW]) -> Result<()> {
    let nfchans = h.nfchans;
    s.spx_in_use = b.flag()?;
    if !s.spx_in_use {
        for ch in 0..nfchans {
            s.ch[ch].spx = false;
            first[ch] = true;
        }
        return Ok(());
    }
    if h.acmod == 1 {
        s.ch[0].spx = true;
    } else {
        for ch in 0..nfchans {
            s.ch[ch].spx = b.flag()?;
        }
    }
    s.spx_strtf = b.read(2)? as u8;
    let begf = b.read(3)? as usize;
    let endf = b.read(3)? as usize;
    s.spx_begin = if begf < 6 { begf + 2 } else { begf * 2 - 3 };
    s.spx_end = if endf < 3 { endf + 5 } else { endf * 2 + 3 };
    if s.spx_end <= s.spx_begin || s.spx_end > 17 {
        return Err(invalid("spectral extension range"));
    }
    if b.flag()? {
        s.spx_bndstrc = [false; 18];
        for bnd in s.spx_begin + 1..s.spx_end {
            s.spx_bndstrc[bnd] = b.flag()?;
        }
    } else if first[..nfchans].iter().all(|&f| f) {
        s.spx_bndstrc = SPX_BNDSTRC;
    }
    let mut n = 1;
    let mut size = [0usize; 18];
    size[0] = 12;
    for bnd in s.spx_begin + 1..s.spx_end {
        if s.spx_bndstrc[bnd] {
            size[n - 1] += 12;
        } else {
            size[n] = 12;
            n += 1;
        }
    }
    s.spx_nbnds = n;
    s.spx_bndsz = size;
    Ok(())
}

fn spx_coords(s: &mut State, b: &mut Bits, nfchans: usize, first: &mut [bool; FBW]) -> Result<()> {
    let begin = 25 + 12 * s.spx_begin;
    let end = (25 + 12 * s.spx_end) as f32;
    for ch in 0..nfchans {
        if !s.ch[ch].spx {
            first[ch] = true;
            continue;
        }
        let coe = if first[ch] {
            first[ch] = false;
            true
        } else {
            b.flag()?
        };
        if !coe {
            continue;
        }
        let noffset = b.read(5)? as f32 / 32.0;
        let mstr = b.read(2)? as usize;
        let mut at = begin as f32;
        let c = &mut s.ch[ch];
        for bnd in 0..s.spx_nbnds {
            let size = s.spx_bndsz[bnd] as f32;
            let ratio = ((at + 0.5 * size) / end - noffset).clamp(0.0, 1.0);
            c.spx_nblend[bnd] = ratio.sqrt();
            c.spx_sblend[bnd] = (1.0 - ratio).sqrt();
            at += size;
        }
        for bnd in 0..s.spx_nbnds {
            let e = b.read(4)? as usize;
            let m = b.read(2)? as f32;
            let v = if e == 15 { m / 4.0 } else { (m + 4.0) / 8.0 };
            c.spx_coord[bnd] = v * super::tables::EXP2[e + 3 * mstr];
        }
    }
    Ok(())
}

struct Aht<'a> {
    pending: &'a mut [bool; CHANNELS],
    slot: &'a mut [usize; CHANNELS],
    cache: &'a mut Vec<[[f32; N]; BLOCKS]>,
}

impl Aht<'_> {
    fn decode(
        &mut self,
        s: &State,
        b: &mut Bits,
        ch: usize,
        start: usize,
        end: usize,
    ) -> Result<()> {
        let snroffset = (((s.csnr as i32 - 15) << 4) + s.ch[ch].fsnr as i32) << 2;
        self.cache.push([[0.0; N]; BLOCKS]);
        let k = self.cache.len() - 1;
        aht::decode(
            b,
            &s.ch[ch],
            start,
            end,
            snroffset,
            s.floorcod,
            &mut self.cache[k],
        )?;
        self.slot[ch] = k;
        self.pending[ch] = false;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn mixed(
    s: &mut State,
    b: &mut Bits,
    nfchans: usize,
    lfeon: bool,
    cplinu: bool,
    pending: &mut [bool; CHANNELS],
    slot: &mut [usize; CHANNELS],
    cache: &mut Vec<[[f32; N]; BLOCKS]>,
) -> Result<()> {
    let mut g = Groups::default();
    let mut got_cpl = false;
    let nodither = s.nodither;
    let mut a = Aht {
        pending,
        slot,
        cache,
    };
    for ch in (0..nfchans).chain(lfeon.then_some(LFE)) {
        let end = s.ch[ch].end;
        if a.pending[ch] {
            a.decode(s, b, ch, 0, end)?;
        } else if a.slot[ch] == usize::MAX {
            let c = &s.ch[ch];
            let dith = c.dith && !nodither && ch != LFE;
            let out = &mut s.spec[s.blk][ch];
            block::chan_mantissas(b, &mut g, c, out, 0, end, dith, &mut s.dither)?;
        }
        if ch < FBW && cplinu && s.ch[ch].cpl && !got_cpl {
            got_cpl = true;
            let (start, end) = (s.cpl_begf_mant, s.cpl_endf_mant);
            if a.pending[CPL] {
                a.decode(s, b, CPL, start, end)?;
            } else if a.slot[CPL] == usize::MAX {
                let out = &mut s.spec[s.blk][CPL];
                block::chan_mantissas(
                    b,
                    &mut g,
                    &s.ch[CPL],
                    out,
                    start,
                    end,
                    false,
                    &mut s.dither,
                )?;
            }
        }
    }
    Ok(())
}
