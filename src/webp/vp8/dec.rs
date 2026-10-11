use super::bits::BoolDecoder;
use super::dsp::{self, B_PRED, BPS};
use super::filter::{self, Strength};
use super::tables::*;
use super::yuv;
use crate::media::{Error, Result};

pub type Probs = [[[[u8; 11]; 3]; 17]; 4];

fn invalid(s: &str) -> Error {
    Error::Invalid(format!("webp: VP8 {s}"))
}

#[derive(Clone, Copy, Default)]
pub struct Quant {
    pub y1: [i32; 2],
    pub y2: [i32; 2],
    pub uv: [i32; 2],
}

pub fn quant(q: i32, d: &[i32; 5]) -> Quant {
    let dc = |i: i32| DC_Q[i.clamp(0, 127) as usize] as i32;
    let ac = |i: i32| AC_Q[i.clamp(0, 127) as usize] as i32;
    Quant {
        y1: [dc(q + d[0]), ac(q)],
        y2: [dc(q + d[1]) * 2, (ac(q + d[2]) * 155 / 100).max(8)],
        uv: [dc(q + d[3]).min(132), ac(q + d[4])],
    }
}

pub fn expand(p: &[[[[u8; 11]; 3]; 8]; 4]) -> Probs {
    let mut out = [[[[0u8; 11]; 3]; 17]; 4];
    for t in 0..4 {
        for (n, b) in BANDS.iter().enumerate() {
            out[t][n] = p[t][*b];
        }
    }
    out
}

pub struct Header<'a> {
    pub width: usize,
    pub height: usize,
    pub mb_w: usize,
    pub simple: bool,
    pub segment_map: bool,
    pub segment_probs: [u8; 3],
    pub quant: [Quant; 4],
    pub strength: [[Strength; 2]; 4],
    pub probs: Probs,
    pub skip_prob: Option<u8>,
    pub first: BoolDecoder<'a>,
    pub parts: Vec<&'a [u8]>,
}

pub fn size(data: &[u8]) -> Result<(usize, usize)> {
    if data.len() < 10 {
        return Err(invalid("frame too short"));
    }
    if data[0] & 1 != 0 {
        return Err(invalid("frame is not a key frame"));
    }
    if data[3..6] != [0x9d, 0x01, 0x2a] {
        return Err(invalid("start code"));
    }
    let w = (u16::from_le_bytes([data[6], data[7]]) & 0x3fff) as usize;
    let h = (u16::from_le_bytes([data[8], data[9]]) & 0x3fff) as usize;
    if w == 0 || h == 0 {
        return Err(invalid("frame size"));
    }
    Ok((w, h))
}

pub fn header(data: &[u8]) -> Result<Header<'_>> {
    let (width, height) = size(data)?;
    let tag = data[0] as u32 | (data[1] as u32) << 8 | (data[2] as u32) << 16;
    if (tag >> 1) & 7 > 3 {
        return Err(invalid("version"));
    }
    let first_size = (tag >> 5) as usize;
    let rest = &data[10..];
    if first_size > rest.len() {
        return Err(invalid("first partition is truncated"));
    }
    let mut br = BoolDecoder::new(&rest[..first_size]);
    let tail = &rest[first_size..];
    br.flag();
    br.flag();
    let mut seg_enabled = false;
    let mut seg_abs = false;
    let mut seg_map = false;
    let mut seg_q = [0i32; 4];
    let mut seg_lf = [0i32; 4];
    let mut seg_probs = [255u8; 3];
    if br.flag() {
        seg_enabled = true;
        seg_map = br.flag();
        if br.flag() {
            seg_abs = br.flag();
            for q in &mut seg_q {
                *q = br.optional_signed(7);
            }
            for l in &mut seg_lf {
                *l = br.optional_signed(6);
            }
        }
        if seg_map {
            for p in &mut seg_probs {
                *p = if br.flag() { br.literal(8) as u8 } else { 255 };
            }
        }
    }
    let simple = br.flag();
    let level = br.literal(6) as i32;
    let sharpness = br.literal(3) as u8;
    let mut ref_delta = 0;
    let mut mode_delta = 0;
    let delta = br.flag();
    if delta && br.flag() {
        for i in 0..4 {
            let v = br.optional_signed(6);
            if i == 0 {
                ref_delta = v;
            }
        }
        for i in 0..4 {
            let v = br.optional_signed(6);
            if i == 0 {
                mode_delta = v;
            }
        }
    }
    let nparts = 1usize << br.literal(2);
    let base_q = br.literal(7) as i32;
    let mut dq = [0i32; 5];
    for d in &mut dq {
        *d = br.optional_signed(4);
    }
    br.flag();
    let mut coeff = COEFF_PROBS;
    for (t, bands) in coeff.iter_mut().enumerate() {
        for (b, ctxs) in bands.iter_mut().enumerate() {
            for (c, probs) in ctxs.iter_mut().enumerate() {
                for (i, p) in probs.iter_mut().enumerate() {
                    if br.get(COEFF_UPDATE_PROBS[t][b][c][i]) {
                        *p = br.literal(8) as u8;
                    }
                }
            }
        }
    }
    let skip_prob = br.flag().then(|| br.literal(8) as u8);

    let sizes_len = 3 * (nparts - 1);
    if tail.len() < sizes_len {
        return Err(invalid("partition sizes are truncated"));
    }
    let mut parts = Vec::with_capacity(nparts);
    let mut at = sizes_len;
    for p in 0..nparts {
        let len = if p + 1 < nparts {
            let s = &tail[3 * p..3 * p + 3];
            s[0] as usize | (s[1] as usize) << 8 | (s[2] as usize) << 16
        } else {
            tail.len().saturating_sub(at)
        };
        let start = at.min(tail.len());
        let end = (at + len).min(tail.len());
        parts.push(&tail[start..end]);
        at += len;
    }

    let mut quant = [Quant::default(); 4];
    let mut strength = [[Strength::default(); 2]; 4];
    for s in 0..4 {
        let q = if !seg_enabled {
            base_q
        } else if seg_abs {
            seg_q[s]
        } else {
            base_q + seg_q[s]
        };
        quant[s] = self::quant(q, &dq);
        let mut lv = if !seg_enabled {
            level
        } else if seg_abs {
            seg_lf[s]
        } else {
            level + seg_lf[s]
        };
        lv = lv.clamp(0, 63);
        for (i4, st) in strength[s].iter_mut().enumerate() {
            let mut l = lv;
            if delta {
                l += ref_delta;
                if i4 == 1 {
                    l += mode_delta;
                }
            }
            *st = filter::strength(l, sharpness, i4 == 1);
        }
    }
    Ok(Header {
        width,
        height,
        mb_w: width.div_ceil(16),
        simple,
        segment_map: seg_map,
        segment_probs: seg_probs,
        quant,
        strength,
        probs: expand(&coeff),
        skip_prob,
        first: br,
        parts,
    })
}

#[derive(Clone, Copy, Default)]
pub struct Mb {
    pub ymode: u8,
    pub uvmode: u8,
    pub segment: u8,
    pub skip: bool,
    pub bmodes: [u8; 16],
}

#[inline(always)]
fn large(br: &mut BoolDecoder, p: &[u8; 11]) -> i32 {
    if !br.get(p[3]) {
        if !br.get(p[4]) {
            2
        } else {
            3 + br.get(p[5]) as i32
        }
    } else if !br.get(p[6]) {
        if !br.get(p[7]) {
            5 + br.get(159) as i32
        } else {
            7 + 2 * br.get(165) as i32 + br.get(145) as i32
        }
    } else {
        let (cat, base): (&[u8], i32) = if !br.get(p[8]) {
            if !br.get(p[9]) {
                (&CAT3, 11)
            } else {
                (&CAT4, 19)
            }
        } else if !br.get(p[10]) {
            (&CAT5, 35)
        } else {
            (&CAT6, 67)
        };
        let mut v = 0;
        for &c in cat {
            v = (v << 1) | br.get(c) as i32;
        }
        base + v
    }
}

#[inline(always)]
pub fn coeffs(
    br: &mut BoolDecoder,
    probs: &[[[u8; 11]; 3]; 17],
    ctx: usize,
    dq: [i32; 2],
    first: usize,
    out: &mut [i16],
) -> usize {
    let out = &mut out[..16];
    let mut n = first;
    let mut p = &probs[n][ctx];
    while n < 16 {
        if !br.get(p[0]) {
            return n;
        }
        while !br.get(p[1]) {
            n += 1;
            if n == 16 {
                return 16;
            }
            p = &probs[n][0];
        }
        let v = if !br.get(p[2]) {
            p = &probs[n + 1][1];
            1
        } else {
            let v = large(br, p);
            p = &probs[n + 1][2];
            v
        };
        let q = if n > 0 { dq[1] } else { dq[0] };
        out[ZIGZAG[n]] = br.get_signed(v).wrapping_mul(q) as i16;
        n += 1;
    }
    16
}

pub const Y_OFF: usize = BPS + 8;
pub const UV_OFF: usize = BPS + 8;

pub struct Work {
    pub y: [u8; BPS * 17],
    pub u: [u8; BPS * 9],
    pub v: [u8; BPS * 9],
}

impl Work {
    pub fn new() -> Self {
        Work {
            y: [0; BPS * 17],
            u: [0; BPS * 9],
            v: [0; BPS * 9],
        }
    }
}

pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub mb_w: usize,
    pub mb_h: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub ys: usize,
    pub uvs: usize,
}

impl Frame {
    pub fn new(width: usize, height: usize) -> Self {
        let mb_w = width.div_ceil(16);
        let mb_h = height.div_ceil(16);
        let ys = mb_w * 16;
        let uvs = mb_w * 8;
        Frame {
            width,
            height,
            mb_w,
            mb_h,
            y: vec![0; ys * mb_h * 16],
            u: vec![0; uvs * mb_h * 8],
            v: vec![0; uvs * mb_h * 8],
            ys,
            uvs,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct MbData {
    pub mb: Mb,
    pub ac: u32,
    pub strength: Strength,
}

pub struct RowData {
    pub mbs: Vec<MbData>,
    pub coeffs: Vec<i16>,
}

impl RowData {
    pub fn new(mb_w: usize) -> Self {
        RowData {
            mbs: vec![MbData::default(); mb_w],
            coeffs: vec![0; mb_w * 384],
        }
    }
}

pub struct Parser<'a> {
    pub hdr: Header<'a>,
    parts: Vec<BoolDecoder<'a>>,
    intra_t: Vec<u8>,
    nz_t: Vec<u32>,
}

impl<'a> Parser<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let hdr = header(data)?;
        let parts = hdr.parts.iter().map(|p| BoolDecoder::new(p)).collect();
        let mb_w = hdr.mb_w;
        Ok(Parser {
            parts,
            intra_t: vec![0; mb_w * 4],
            nz_t: vec![0; mb_w],
            hdr,
        })
    }

    fn modes(&mut self, intra_l: &mut [u8; 4], mx: usize) -> Mb {
        let br = &mut self.hdr.first;
        let segment = if self.hdr.segment_map {
            br.tree(&SEGMENT_TREE, &self.hdr.segment_probs)
        } else {
            0
        };
        let skip = match self.hdr.skip_prob {
            Some(p) => br.get(p),
            None => false,
        };
        let ymode = br.tree(&KF_YMODE_TREE, &KF_YMODE_PROBS);
        let mut bmodes = [0u8; 16];
        let top = &mut self.intra_t[mx * 4..mx * 4 + 4];
        if ymode == B_PRED {
            for (i, m) in bmodes.iter_mut().enumerate() {
                let a = top[i & 3] as usize;
                let l = intra_l[i >> 2] as usize;
                *m = br.tree(&BMODE_TREE, &KF_BMODE_PROBS[a][l]);
                top[i & 3] = *m;
                intra_l[i >> 2] = *m;
            }
        } else {
            let b = [dsp::B_DC, dsp::B_VE, dsp::B_HE, dsp::B_TM][ymode as usize];
            top.fill(b);
            intra_l.fill(b);
        }
        let uvmode = br.tree(&UV_MODE_TREE, &KF_UV_MODE_PROBS);
        Mb {
            ymode,
            uvmode,
            segment,
            skip,
            bmodes,
        }
    }

    fn residuals(
        br: &mut BoolDecoder,
        probs: &Probs,
        q: &Quant,
        mb: &Mb,
        tnz: &mut u32,
        lnz: &mut u32,
        c: &mut [i16],
    ) -> (u32, bool) {
        let mut b = br.clone();
        let mut t = *tnz;
        let mut l = *lnz;
        let mut ac = 0u32;
        let mut any = false;
        let c = &mut c[..384];
        let (first, ty) = if mb.ymode != B_PRED {
            let ctx = ((t >> 8) & 1) + ((l >> 8) & 1);
            let mut dc = [0i16; 16];
            let n = coeffs(&mut b, &probs[1], ctx as usize, q.y2, 0, &mut dc);
            let nz = (n > 0) as u32;
            t = (t & !0x100) | nz << 8;
            l = (l & !0x100) | nz << 8;
            any |= n > 0;
            if n > 1 {
                dsp::iwht(&dc, c);
            } else if n == 1 {
                let v = ((dc[0] as i32 + 3) >> 3) as i16;
                for i in 0..16 {
                    c[i * 16] = v;
                }
            }
            (1, 0)
        } else {
            (0, 3)
        };
        let py = &probs[ty];
        for by in 0..4 {
            let mut lb = (l >> by) & 1;
            for bx in 0..4 {
                let ctx = ((t >> bx) & 1) + lb;
                let k = by * 4 + bx;
                let n = coeffs(
                    &mut b,
                    py,
                    ctx as usize,
                    q.y1,
                    first,
                    &mut c[k * 16..k * 16 + 16],
                );
                lb = (n > first) as u32;
                t = (t & !(1 << bx)) | lb << bx;
                ac |= ((n > 1) as u32) << k;
                any |= n > first;
            }
            l = (l & !(1 << by)) | lb << by;
        }
        let pc = &probs[2];
        for k0 in [16usize, 20] {
            let shift = if k0 == 16 { 4 } else { 6 };
            for by in 0..2 {
                let mut lb = (l >> (shift + by)) & 1;
                for bx in 0..2 {
                    let ctx = ((t >> (shift + bx)) & 1) + lb;
                    let k = k0 + by * 2 + bx;
                    let n = coeffs(
                        &mut b,
                        pc,
                        ctx as usize,
                        q.uv,
                        0,
                        &mut c[k * 16..k * 16 + 16],
                    );
                    lb = (n > 0) as u32;
                    t = (t & !(1 << (shift + bx))) | lb << (shift + bx);
                    ac |= ((n > 1) as u32) << k;
                    any |= n > 0;
                }
                l = (l & !(1 << (shift + by))) | lb << (shift + by);
            }
        }
        *br = b;
        *tnz = t;
        *lnz = l;
        (ac, any)
    }

    pub fn row(&mut self, my: usize, out: &mut RowData) {
        let mut intra_l = [0u8; 4];
        let mut nz_l = 0u32;
        let part = my % self.parts.len();
        for mx in 0..self.hdr.mb_w {
            let mb = self.modes(&mut intra_l, mx);
            let (ac, any) = if mb.skip {
                let keep = if mb.ymode == B_PRED { 0x100 } else { 0 };
                self.nz_t[mx] &= keep;
                nz_l &= keep;
                (0, false)
            } else {
                Self::residuals(
                    &mut self.parts[part],
                    &self.hdr.probs,
                    &self.hdr.quant[mb.segment as usize],
                    &mb,
                    &mut self.nz_t[mx],
                    &mut nz_l,
                    &mut out.coeffs[mx * 384..mx * 384 + 384],
                )
            };
            let mut strength =
                self.hdr.strength[mb.segment as usize][(mb.ymode == B_PRED) as usize];
            strength.inner |= any;
            out.mbs[mx] = MbData { mb, ac, strength };
        }
    }
}

pub struct Recon {
    mb_w: usize,
    top_y: Vec<u8>,
    top_u: Vec<u8>,
    top_v: Vec<u8>,
    work: Box<Work>,
}

impl Recon {
    pub fn new(mb_w: usize) -> Self {
        Recon {
            mb_w,
            top_y: vec![0; mb_w * 16 + 4],
            top_u: vec![0; mb_w * 8],
            top_v: vec![0; mb_w * 8],
            work: Box::new(Work::new()),
        }
    }

    #[inline(always)]
    fn add(c: &mut [i16], ac: u32, b: usize, dst: &mut [u8], off: usize) {
        if ac & (1 << b) != 0 {
            dsp::idct_add(&c[b * 16..b * 16 + 16], dst, off);
            c[b * 16..b * 16 + 16].fill(0);
        } else if c[b * 16] != 0 {
            dsp::dc_add(c[b * 16], dst, off);
            c[b * 16] = 0;
        }
    }

    fn mb(&mut self, d: &MbData, c: &mut [i16], mx: usize, my: usize, tl: &mut [u8; 3]) {
        let mb = &d.mb;
        let ac = d.ac;
        let w = &mut *self.work;
        let last = mx + 1 == self.mb_w;
        if my == 0 {
            w.y[Y_OFF - BPS - 1..Y_OFF - BPS + 20].fill(127);
            w.u[UV_OFF - BPS - 1..UV_OFF - BPS + 8].fill(127);
            w.v[UV_OFF - BPS - 1..UV_OFF - BPS + 8].fill(127);
        } else {
            let t = Y_OFF - BPS;
            w.y[t - 1] = if mx == 0 { 129 } else { tl[0] };
            w.y[t..t + 16].copy_from_slice(&self.top_y[mx * 16..mx * 16 + 16]);
            if last {
                let v = self.top_y[mx * 16 + 15];
                w.y[t + 16..t + 20].fill(v);
            } else {
                w.y[t + 16..t + 20].copy_from_slice(&self.top_y[mx * 16 + 16..mx * 16 + 20]);
            }
            let t = UV_OFF - BPS;
            w.u[t - 1] = if mx == 0 { 129 } else { tl[1] };
            w.v[t - 1] = if mx == 0 { 129 } else { tl[2] };
            w.u[t..t + 8].copy_from_slice(&self.top_u[mx * 8..mx * 8 + 8]);
            w.v[t..t + 8].copy_from_slice(&self.top_v[mx * 8..mx * 8 + 8]);
        }
        if mx == 0 {
            for r in 0..16 {
                w.y[Y_OFF + r * BPS - 1] = 129;
            }
            for r in 0..8 {
                w.u[UV_OFF + r * BPS - 1] = 129;
                w.v[UV_OFF + r * BPS - 1] = 129;
            }
        }
        if mb.ymode == B_PRED {
            let t = Y_OFF - BPS + 16;
            let ar = [w.y[t], w.y[t + 1], w.y[t + 2], w.y[t + 3]];
            for r in [3, 7, 11] {
                w.y[Y_OFF + r * BPS + 16..Y_OFF + r * BPS + 20].copy_from_slice(&ar);
            }
            for b in 0..16 {
                let off = Y_OFF + (b >> 2) * 4 * BPS + (b & 3) * 4;
                dsp::pred4(mb.bmodes[b], &mut w.y, off);
                Self::add(c, ac, b, &mut w.y, off);
            }
        } else {
            dsp::pred_luma(mb.ymode, &mut w.y, Y_OFF, my > 0, mx > 0);
            for b in 0..16 {
                let off = Y_OFF + (b >> 2) * 4 * BPS + (b & 3) * 4;
                Self::add(c, ac, b, &mut w.y, off);
            }
        }
        dsp::pred_chroma(mb.uvmode, &mut w.u, UV_OFF, my > 0, mx > 0);
        dsp::pred_chroma(mb.uvmode, &mut w.v, UV_OFF, my > 0, mx > 0);
        for j in 0..4 {
            let off = UV_OFF + (j >> 1) * 4 * BPS + (j & 1) * 4;
            Self::add(c, ac, 16 + j, &mut w.u, off);
            Self::add(c, ac, 20 + j, &mut w.v, off);
        }
        tl[0] = self.top_y[mx * 16 + 15];
        tl[1] = self.top_u[mx * 8 + 7];
        tl[2] = self.top_v[mx * 8 + 7];
        let b = Y_OFF + 15 * BPS;
        self.top_y[mx * 16..mx * 16 + 16].copy_from_slice(&w.y[b..b + 16]);
        let b = UV_OFF + 7 * BPS;
        self.top_u[mx * 8..mx * 8 + 8].copy_from_slice(&w.u[b..b + 8]);
        self.top_v[mx * 8..mx * 8 + 8].copy_from_slice(&w.v[b..b + 8]);
    }

    fn store(
        &mut self,
        mx: usize,
        y: &mut [u8],
        u: &mut [u8],
        v: &mut [u8],
        ys: usize,
        uvs: usize,
    ) {
        let w = &mut *self.work;
        for r in 0..16 {
            y[r * ys + mx * 16..r * ys + mx * 16 + 16]
                .copy_from_slice(&w.y[Y_OFF + r * BPS..Y_OFF + r * BPS + 16]);
            w.y[Y_OFF + r * BPS - 1] = w.y[Y_OFF + r * BPS + 15];
        }
        for r in 0..8 {
            u[r * uvs + mx * 8..r * uvs + mx * 8 + 8]
                .copy_from_slice(&w.u[UV_OFF + r * BPS..UV_OFF + r * BPS + 8]);
            v[r * uvs + mx * 8..r * uvs + mx * 8 + 8]
                .copy_from_slice(&w.v[UV_OFF + r * BPS..UV_OFF + r * BPS + 8]);
            w.u[UV_OFF + r * BPS - 1] = w.u[UV_OFF + r * BPS + 7];
            w.v[UV_OFF + r * BPS - 1] = w.v[UV_OFF + r * BPS + 7];
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn row(
        &mut self,
        my: usize,
        data: &mut RowData,
        y: &mut [u8],
        u: &mut [u8],
        v: &mut [u8],
        ys: usize,
        uvs: usize,
    ) {
        let mut tl = [0u8; 3];
        for mx in 0..self.mb_w {
            let d = data.mbs[mx];
            self.mb(
                &d,
                &mut data.coeffs[mx * 384..mx * 384 + 384],
                mx,
                my,
                &mut tl,
            );
            self.store(mx, y, u, v, ys, uvs);
        }
    }
}

struct Raw(*mut u8, usize);

unsafe impl Send for Raw {}
unsafe impl Sync for Raw {}

impl Raw {
    fn new(v: &mut [u8]) -> Self {
        Raw(v.as_mut_ptr(), v.len())
    }

    #[allow(clippy::mut_from_ref)]
    unsafe fn rows(&self, stride: usize, a: usize, b: usize) -> &mut [u8] {
        let (a, b) = (a * stride, (b * stride).min(self.1));
        assert!(a <= b);
        unsafe { std::slice::from_raw_parts_mut(self.0.add(a), b - a) }
    }
}

struct Shared {
    y: Raw,
    u: Raw,
    v: Raw,
    ys: usize,
    uvs: usize,
    width: usize,
    height: usize,
    simple: bool,
}

impl Shared {
    unsafe fn recon(&self, r: &mut Recon, my: usize, data: &mut RowData) {
        unsafe {
            r.row(
                my,
                data,
                self.y.rows(self.ys, my * 16, my * 16 + 16),
                self.u.rows(self.uvs, my * 8, my * 8 + 8),
                self.v.rows(self.uvs, my * 8, my * 8 + 8),
                self.ys,
                self.uvs,
            )
        }
    }

    unsafe fn filter_row(&self, my: usize, st: &[MbData]) {
        let yr = (my * 16).saturating_sub(4);
        let cr = (my * 8).saturating_sub(4);
        let mut p = unsafe {
            filter::Planes {
                y: self.y.rows(self.ys, yr, my * 16 + 16),
                u: self.u.rows(self.uvs, cr, my * 8 + 8),
                v: self.v.rows(self.uvs, cr, my * 8 + 8),
                ys: self.ys,
                uvs: self.uvs,
                yr,
                cr,
            }
        };
        for (mx, d) in st.iter().enumerate() {
            filter::filter_mb_fast(&mut p, mx, my, d.strength, self.simple);
        }
    }

    unsafe fn convert(&self, a: usize, b: usize, out: &mut [u8]) {
        let yr = a;
        let cr = a.div_ceil(2).saturating_sub(1);
        let ce = (b - 1) / 2 + 1;
        let p = unsafe {
            yuv::Yuv {
                y: self.y.rows(self.ys, yr, b),
                u: self.u.rows(self.uvs, cr, ce),
                v: self.v.rows(self.uvs, cr, ce),
                ys: self.ys,
                uvs: self.uvs,
                yr,
                cr,
                width: self.width,
                height: self.height,
            }
        };
        yuv::to_rgba_rows(&p, a..b, out);
    }
}

fn band_end(my: usize, mb_h: usize, height: usize) -> usize {
    if my + 1 == mb_h { height } else { my * 16 + 9 }
}

const RING: usize = 6;

pub fn decode_rgba(data: &[u8], out: &mut [u8]) -> Result<()> {
    let mut parser = Parser::new(data)?;
    let mut f = Frame::new(parser.hdr.width, parser.hdr.height);
    if out.len() != f.width * f.height * 4 {
        return Err(invalid("output size"));
    }
    let (mb_w, mb_h) = (f.mb_w, f.mb_h);
    let sh = Shared {
        y: Raw::new(&mut f.y),
        u: Raw::new(&mut f.u),
        v: Raw::new(&mut f.v),
        ys: f.ys,
        uvs: f.uvs,
        width: f.width,
        height: f.height,
        simple: parser.hdr.simple,
    };
    let mut bands = Vec::with_capacity(mb_h);
    let mut rest = out;
    let mut a = 0;
    for my in 0..mb_h {
        let b = band_end(my, mb_h, f.height);
        let (band, tail) = rest.split_at_mut((b - a) * f.width * 4);
        bands.push((a, b, band));
        rest = tail;
        a = b;
    }
    let mut recon = Recon::new(mb_w);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    if threads < 3 || mb_w * mb_h < 300 {
        let mut row = RowData::new(mb_w);
        for (my, (a, b, band)) in bands.into_iter().enumerate() {
            parser.row(my, &mut row);
            unsafe {
                sh.recon(&mut recon, my, &mut row);
                sh.filter_row(my, &row.mbs);
                sh.convert(a, b, band);
            }
        }
        return Ok(());
    }
    let sh = &sh;
    std::thread::scope(|s| {
        use std::sync::mpsc::channel;
        let (free_tx, free_rx) = channel::<RowData>();
        let (full_tx, full_rx) = channel::<(usize, RowData)>();
        let (done_tx, done_rx) = channel::<(usize, RowData)>();
        let (band_tx, band_rx) = channel::<(usize, usize, &mut [u8])>();
        for _ in 0..RING {
            free_tx.send(RowData::new(mb_w)).ok();
        }
        s.spawn(move || {
            for (my, mut row) in full_rx {
                unsafe { sh.recon(&mut recon, my, &mut row) };
                if done_tx.send((my, row)).is_err() {
                    break;
                }
            }
        });
        s.spawn(move || {
            let mut bands = bands.into_iter();
            for (my, row) in done_rx {
                unsafe { sh.filter_row(my, &row.mbs) };
                free_tx.send(row).ok();
                if let Some(b) = bands.next() {
                    band_tx.send(b).ok();
                }
            }
        });
        s.spawn(move || {
            for (a, b, band) in band_rx {
                unsafe { sh.convert(a, b, band) };
            }
        });
        for my in 0..mb_h {
            let Ok(mut row) = free_rx.recv() else { break };
            parser.row(my, &mut row);
            if full_tx.send((my, row)).is_err() {
                break;
            }
        }
        drop(full_tx);
    });
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
pub fn decode(data: &[u8]) -> Result<Frame> {
    let mut parser = Parser::new(data)?;
    let mut f = Frame::new(parser.hdr.width, parser.hdr.height);
    let mut recon = Recon::new(f.mb_w);
    let mut row = RowData::new(f.mb_w);
    let sh = Shared {
        y: Raw::new(&mut f.y),
        u: Raw::new(&mut f.u),
        v: Raw::new(&mut f.v),
        ys: f.ys,
        uvs: f.uvs,
        width: f.width,
        height: f.height,
        simple: parser.hdr.simple,
    };
    for my in 0..f.mb_h {
        parser.row(my, &mut row);
        unsafe {
            sh.recon(&mut recon, my, &mut row);
            sh.filter_row(my, &row.mbs);
        }
    }
    Ok(f)
}
