use crate::hevc::pel::Pel;
use crate::hevc::picture::Frame;
use crate::hevc::simd;
use crate::hevc::spec_tables::{CHROMA_FILTER, LUMA_FILTER};

const WIN: usize = 64 + 7;

pub struct McScratch<P: Pel> {
    win: Vec<P>,
    tmp: Vec<i16>,
}

impl<P: Pel> Default for McScratch<P> {
    fn default() -> Self {
        McScratch {
            win: vec![P::default(); WIN * WIN],
            tmp: vec![0; WIN * WIN],
        }
    }
}

#[inline(always)]
fn filt_w<const T: usize, const W: usize, S: Copy + Into<i32>>(
    src: &[S],
    step: usize,
    f: &[i8; T],
    shift: u32,
    out: &mut [i16],
) {
    let mut acc = [0i32; W];
    for k in 0..T {
        let c = f[k] as i32;
        let Some(p) = src.get(k * step..).and_then(|s| s.first_chunk::<W>()) else {
            return;
        };
        for j in 0..W {
            acc[j] += c * p[j].into();
        }
    }
    let Some(o) = out.first_chunk_mut::<W>() else {
        return;
    };
    for j in 0..W {
        o[j] = (acc[j] >> shift) as i16;
    }
}

#[inline(always)]
fn filt<const T: usize, S: Copy + Into<i32>>(
    src: &[S],
    step: usize,
    f: &[i8; T],
    shift: u32,
    out: &mut [i16],
    w: usize,
) {
    match w {
        2 => filt_w::<T, 2, S>(src, step, f, shift, out),
        4 => filt_w::<T, 4, S>(src, step, f, shift, out),
        6 => filt_w::<T, 6, S>(src, step, f, shift, out),
        8 => filt_w::<T, 8, S>(src, step, f, shift, out),
        12 => filt_w::<T, 12, S>(src, step, f, shift, out),
        16 => filt_w::<T, 16, S>(src, step, f, shift, out),
        24 => filt_w::<T, 24, S>(src, step, f, shift, out),
        32 => filt_w::<T, 32, S>(src, step, f, shift, out),
        48 => filt_w::<T, 48, S>(src, step, f, shift, out),
        64 => filt_w::<T, 64, S>(src, step, f, shift, out),
        _ => {
            for (j, o) in out[..w].iter_mut().enumerate() {
                let s: i32 = (0..T).map(|k| f[k] as i32 * src[j + k * step].into()).sum();
                *o = (s >> shift) as i16;
            }
        }
    }
}

#[inline(always)]
fn filt_lines_w<const T: usize, const W: usize, P: Pel>(
    lines: &[&[P]],
    x: usize,
    f: &[i8; T],
    shift: u32,
    out: &mut [i16],
) {
    let mut acc = [0i32; W];
    for k in 0..T {
        let c = f[k] as i32;
        let Some(p) = lines
            .get(k)
            .and_then(|l| l.get(x..))
            .and_then(|s| s.first_chunk::<W>())
        else {
            return;
        };
        for j in 0..W {
            acc[j] += c * p[j].into();
        }
    }
    let Some(o) = out.first_chunk_mut::<W>() else {
        return;
    };
    for j in 0..W {
        o[j] = (acc[j] >> shift) as i16;
    }
}

#[inline(always)]
fn filt_lines<const T: usize, P: Pel>(
    lines: &[&[P]],
    x: usize,
    f: &[i8; T],
    shift: u32,
    out: &mut [i16],
    w: usize,
) {
    match w {
        2 => filt_lines_w::<T, 2, P>(lines, x, f, shift, out),
        4 => filt_lines_w::<T, 4, P>(lines, x, f, shift, out),
        6 => filt_lines_w::<T, 6, P>(lines, x, f, shift, out),
        8 => filt_lines_w::<T, 8, P>(lines, x, f, shift, out),
        12 => filt_lines_w::<T, 12, P>(lines, x, f, shift, out),
        16 => filt_lines_w::<T, 16, P>(lines, x, f, shift, out),
        24 => filt_lines_w::<T, 24, P>(lines, x, f, shift, out),
        32 => filt_lines_w::<T, 32, P>(lines, x, f, shift, out),
        48 => filt_lines_w::<T, 48, P>(lines, x, f, shift, out),
        64 => filt_lines_w::<T, 64, P>(lines, x, f, shift, out),
        _ => {
            for (j, o) in out[..w].iter_mut().enumerate() {
                let s: i32 = (0..T).map(|k| f[k] as i32 * lines[k][x + j].into()).sum();
                *o = (s >> shift) as i16;
            }
        }
    }
}

pub(crate) struct Taps<'a, const T: usize> {
    pub fx: usize,
    pub fy: usize,
    pub hf: &'a [i8; T],
    pub vf: &'a [i8; T],
    pub shift1: u32,
    pub shift3: u32,
}

#[inline(always)]
pub(crate) fn interpolate<const T: usize, P: Pel>(
    lines: &[&[P]],
    tmp: &mut [i16],
    t: &Taps<T>,
    w: usize,
    h: usize,
    out: &mut [i16],
) {
    if simd::mc::interp::<T, P>(
        lines,
        tmp,
        (t.fx, t.fy),
        (t.hf, t.vf),
        (t.shift1, t.shift3),
        w,
        h,
        out,
    ) {
        return;
    }
    let m = T / 2 - 1;
    if t.fx == 0 && t.fy == 0 {
        for r in 0..h {
            let line = &lines[r + m][m..m + w];
            for (o, &s) in out[r * w..r * w + w].iter_mut().zip(line) {
                *o = (s.into() << t.shift3) as i16;
            }
        }
    } else if t.fy == 0 {
        for r in 0..h {
            filt(lines[r + m], 1, t.hf, t.shift1, &mut out[r * w..], w);
        }
    } else if t.fx == 0 {
        for r in 0..h {
            filt_lines(&lines[r..], m, t.vf, t.shift1, &mut out[r * w..], w);
        }
    } else {
        for r in 0..h + T - 1 {
            filt(lines[r], 1, t.hf, t.shift1, &mut tmp[r * WIN..], w);
        }
        for r in 0..h {
            filt(&tmp[r * WIN..], WIN, t.vf, 6, &mut out[r * w..], w);
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn predict<const T: usize, P: Pel>(
    f: &Frame,
    plane: usize,
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    taps: &Taps<T>,
    out: &mut [i16],
    sc: &mut McScratch<P>,
) {
    let (ww, wh) = (w + T - 1, h + T - 1);
    let mut lines: [&[P]; WIN] = [&[]; WIN];
    if !f.lines(plane, x0, y0, ww, wh, &mut lines) {
        f.window(plane, x0, y0, ww, wh, &mut sc.win, WIN);
        for (r, l) in lines.iter_mut().enumerate().take(wh) {
            *l = &sc.win[r * WIN..r * WIN + ww];
        }
    }
    interpolate::<T, P>(&lines[..wh], &mut sc.tmp, taps, w, h, out);
}

crate::hevc::isa::dispatch! {
pub fn mc_luma<P: Pel>(
    f: &Frame,
    x: i32,
    y: i32,
    mv: [i16; 2],
    w: usize,
    h: usize,
    out: &mut [i16],
    sc: &mut McScratch<P>,
) {
    let bd = f.bit_depth;
    let (fx, fy) = ((mv[0] & 3) as usize, (mv[1] & 3) as usize);
    let xi = x + (mv[0] as i32 >> 2);
    let yi = y + (mv[1] as i32 >> 2);
    let taps = Taps {
        fx,
        fy,
        hf: &LUMA_FILTER[fx],
        vf: &LUMA_FILTER[fy],
        shift1: bd.min(12) - 8,
        shift3: 14 - bd,
    };
    predict::<8, P>(f, 0, xi - 3, yi - 3, w, h, &taps, out, sc);
}
}

crate::hevc::isa::dispatch! {
pub fn mc_chroma<P: Pel>(
    f: &Frame,
    c: usize,
    x: i32,
    y: i32,
    mv: [i16; 2],
    w: usize,
    h: usize,
    out: &mut [i16],
    sc: &mut McScratch<P>,
) {
    let bd = f.bit_depth_c;
    let (fx, fy) = ((mv[0] & 7) as usize, (mv[1] & 7) as usize);
    let xi = x + (mv[0] as i32 >> 3);
    let yi = y + (mv[1] as i32 >> 3);
    let taps = Taps {
        fx,
        fy,
        hf: &CHROMA_FILTER[fx],
        vf: &CHROMA_FILTER[fy],
        shift1: bd.min(12) - 8,
        shift3: 14 - bd,
    };
    predict::<4, P>(f, c + 1, xi - 1, yi - 1, w, h, &taps, out, sc);
}
}

crate::hevc::isa::dispatch! {
pub fn put_uni<P: Pel>(p: &[i16], w: usize, h: usize, bd: u32, dst: &mut [P], ds: usize) {
    if simd::mc::put_uni(p, w, h, bd, dst, ds) {
        return;
    }
    let shift = 14 - bd;
    let off = if shift > 0 { 1 << (shift - 1) } else { 0 };
    let max = (1i32 << bd) - 1;
    for (d, s) in dst.chunks_mut(ds).zip(p[..w * h].chunks(w)) {
        for (d, &s) in d[..w].iter_mut().zip(s) {
            *d = P::from_i32(((s as i32 + off) >> shift).max(0).min(max));
        }
    }
}
}

crate::hevc::isa::dispatch! {
pub fn put_bi<P: Pel>(p0: &[i16], p1: &[i16], w: usize, h: usize, bd: u32, dst: &mut [P], ds: usize) {
    if simd::mc::put_bi(p0, p1, w, h, bd, dst, ds) {
        return;
    }
    let shift = 15 - bd;
    let off = 1 << (shift - 1);
    let max = (1i32 << bd) - 1;
    for ((d, a), b) in dst
        .chunks_mut(ds)
        .zip(p0[..w * h].chunks(w))
        .zip(p1[..w * h].chunks(w))
    {
        for ((d, &a), &b) in d[..w].iter_mut().zip(a).zip(b) {
            *d = P::from_i32(((a as i32 + b as i32 + off) >> shift).max(0).min(max));
        }
    }
}
}

crate::hevc::isa::dispatch! {
pub fn put_weighted_uni<P: Pel>(
    p: &[i16],
    w: usize,
    h: usize,
    bd: u32,
    log2wd: u32,
    wt: i32,
    o: i32,
    dst: &mut [P],
    ds: usize,
) {
    if simd::mc::put_weighted_uni(p, w, h, bd, log2wd, wt, o, dst, ds) {
        return;
    }
    let max = (1i32 << bd) - 1;
    for r in 0..h {
        for c in 0..w {
            let v = p[r * w + c] as i32 * wt;
            let v = if log2wd >= 1 {
                ((v + (1 << (log2wd - 1))) >> log2wd) + o
            } else {
                v + o
            };
            dst[r * ds + c] = P::from_i32(v.max(0).min(max));
        }
    }
}
}

crate::hevc::isa::dispatch! {
pub fn put_weighted_bi<P: Pel>(
    p0: &[i16],
    p1: &[i16],
    w: usize,
    h: usize,
    bd: u32,
    log2wd: u32,
    w0: i32,
    w1: i32,
    o0: i32,
    o1: i32,
    dst: &mut [P],
    ds: usize,
) {
    if simd::mc::put_weighted_bi(p0, p1, w, h, bd, log2wd, (w0, w1), (o0, o1), dst, ds) {
        return;
    }
    let max = (1i32 << bd) - 1;
    for r in 0..h {
        for c in 0..w {
            let i = r * w + c;
            let v =
                (p0[i] as i32 * w0 + p1[i] as i32 * w1 + ((o0 + o1 + 1) << log2wd)) >> (log2wd + 1);
            dst[r * ds + c] = P::from_i32(v.max(0).min(max));
        }
    }
}
}
