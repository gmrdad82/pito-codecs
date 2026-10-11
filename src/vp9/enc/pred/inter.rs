use super::isa::dispatch;
use super::*;
use crate::vp9::spec_tables::SUBPEL_FILTERS;

const WIN: usize = 64 + 7;

#[inline(always)]
fn taps(filter: u8, frac: usize) -> &'static [i16; 8] {
    let o = ((filter as usize & 3) * 16 + frac) * 8;
    SUBPEL_FILTERS[o..o + 8].try_into().unwrap()
}

pub fn mc(
    r: &Plane,
    x16: isize,
    y16: isize,
    w: usize,
    h: usize,
    filter: u8,
    dst: &mut [u8],
    ds: usize,
) {
    let (x0, y0) = (x16 >> 4, y16 >> 4);
    let (fx, fy) = ((x16 & 15) as usize, (y16 & 15) as usize);
    if r.inside(x0 - 3, y0 - 3, w + 7, h + 7) {
        filter_block(
            r.data,
            r.stride,
            r.at(x0 - 3, y0 - 3),
            w,
            h,
            filter,
            fx,
            fy,
            dst,
            ds,
        );
    } else {
        mc_edge(r, x0, y0, w, h, filter, fx, fy, dst, ds);
    }
}

#[cold]
fn mc_edge(
    r: &Plane,
    x0: isize,
    y0: isize,
    w: usize,
    h: usize,
    filter: u8,
    fx: usize,
    fy: usize,
    dst: &mut [u8],
    ds: usize,
) {
    let mut win = [0u8; WIN * WIN + 16];
    let b = r.border as isize;
    let (lx, ly) = (r.width as isize - 1 + b, r.height as isize - 1 + b);
    for row in 0..h + 7 {
        let yy = (y0 - 3 + row as isize).clamp(-b, ly);
        let base = r.at(0, yy) as isize;
        let d = &mut win[row * WIN..row * WIN + w + 7];
        for (c, v) in d.iter_mut().enumerate() {
            let xx = (x0 - 3 + c as isize).clamp(-b, lx);
            *v = r.data[(base + xx) as usize];
        }
    }
    filter_block(&win, WIN, 0, w, h, filter, fx, fy, dst, ds);
}

pub fn filter_block(
    src: &[u8],
    ss: usize,
    so: usize,
    w: usize,
    h: usize,
    filter: u8,
    fx: usize,
    fy: usize,
    dst: &mut [u8],
    ds: usize,
) {
    assert!(matches!(w, 4 | 8 | 16 | 32 | 64) && h > 0 && h <= 64);
    assert!(src.len() >= so + (h + 6) * ss + w + 7);
    assert!(dst.len() >= (h - 1) * ds + w);
    match (fx, fy) {
        (0, 0) => {
            for i in 0..h {
                let s = so + (i + 3) * ss + 3;
                dst[i * ds..i * ds + w].copy_from_slice(&src[s..s + w]);
            }
        }
        (_, 0) => h_pass(src, ss, so + 3 * ss, w, h, taps(filter, fx), dst, ds),
        (0, _) => v_pass(src, ss, so + 3, w, h, taps(filter, fy), dst, ds),
        _ => {
            let (f0, f1) = (taps(filter, fx), taps(filter, fy));
            match w {
                4 | 8 => {
                    let mut tmp = [0u8; 8 * WIN + 16];
                    h_pass(src, ss, so, w, h + 7, f0, &mut tmp, w);
                    v_pass(&tmp, w, 0, w, h, f1, dst, ds);
                }
                16 => {
                    let mut tmp = [0u8; 16 * WIN + 16];
                    h_pass(src, ss, so, w, h + 7, f0, &mut tmp, w);
                    v_pass(&tmp, w, 0, w, h, f1, dst, ds);
                }
                _ => {
                    let mut tmp = [0u8; 64 * WIN + 16];
                    h_pass(src, ss, so, w, h + 7, f0, &mut tmp, w);
                    v_pass(&tmp, w, 0, w, h, f1, dst, ds);
                }
            }
        }
    }
}

fn h_pass(
    src: &[u8],
    ss: usize,
    so: usize,
    w: usize,
    h: usize,
    f: &[i16; 8],
    dst: &mut [u8],
    ds: usize,
) {
    #[cfg(target_arch = "x86_64")]
    if super::isa::avx2() && src.len() >= so + (h - 1) * ss + w + super::mc_avx2::OVERREAD {
        unsafe { super::mc_avx2::h_pass(src.as_ptr().add(so), ss, w, h, f, dst.as_mut_ptr(), ds) };
        return;
    }
    h_pass_any(src, ss, so, w, h, f, dst, ds)
}

fn v_pass(
    src: &[u8],
    ss: usize,
    so: usize,
    w: usize,
    h: usize,
    f: &[i16; 8],
    dst: &mut [u8],
    ds: usize,
) {
    #[cfg(target_arch = "x86_64")]
    if super::isa::avx2() {
        unsafe { super::mc_avx2::v_pass(src.as_ptr().add(so), ss, w, h, f, dst.as_mut_ptr(), ds) };
        return;
    }
    v_pass_any(src, ss, so, w, h, f, dst, ds)
}

#[inline(always)]
fn narrow(acc: u16) -> u8 {
    if acc < 0xC000 {
        (acc >> 7).min(255) as u8
    } else {
        0
    }
}

dispatch! {
    fn h_pass_any(src: &[u8], ss: usize, so: usize, w: usize, h: usize, f: &[i16; 8], dst: &mut [u8], ds: usize) {
        match w {
            4 => h_rows::<4>(src, ss, so, h, f, dst, ds),
            8 => h_rows::<8>(src, ss, so, h, f, dst, ds),
            16 => h_rows::<16>(src, ss, so, h, f, dst, ds),
            32 => h_rows::<32>(src, ss, so, h, f, dst, ds),
            _ => h_rows::<64>(src, ss, so, h, f, dst, ds),
        }
    }
}

dispatch! {
    fn v_pass_any(src: &[u8], ss: usize, so: usize, w: usize, h: usize, f: &[i16; 8], dst: &mut [u8], ds: usize) {
        match w {
            4 => v_rows::<4>(src, ss, so, h, f, dst, ds),
            8 => v_rows::<8>(src, ss, so, h, f, dst, ds),
            16 => v_rows::<16>(src, ss, so, h, f, dst, ds),
            32 => v_rows::<32>(src, ss, so, h, f, dst, ds),
            _ => v_rows::<64>(src, ss, so, h, f, dst, ds),
        }
    }
}

#[inline(always)]
fn h_rows<const W: usize>(
    src: &[u8],
    ss: usize,
    so: usize,
    h: usize,
    f: &[i16; 8],
    dst: &mut [u8],
    ds: usize,
) {
    let c: [u16; 8] = f.map(|v| v as u16);
    for i in 0..h {
        let s: &[u8; 72] = match src[so + i * ss..].first_chunk::<72>() {
            Some(s) => s,
            None => {
                let mut t = [0u8; 72];
                let rest = &src[so + i * ss..];
                let n = rest.len().min(W + 7);
                t[..n].copy_from_slice(&rest[..n]);
                h_row::<W>(&t, &c, &mut dst[i * ds..i * ds + W]);
                continue;
            }
        };
        h_row::<W>(s, &c, &mut dst[i * ds..i * ds + W]);
    }
}

#[inline(always)]
fn h_row<const W: usize>(s: &[u8; 72], c: &[u16; 8], o: &mut [u8]) {
    let mut acc = [64u16; W];
    for t in 0..8 {
        let p: &[u8; W] = s[t..t + W].try_into().unwrap();
        for j in 0..W {
            acc[j] = acc[j].wrapping_add(c[t].wrapping_mul(p[j] as u16));
        }
    }
    let o: &mut [u8; W] = o.try_into().unwrap();
    for j in 0..W {
        o[j] = narrow(acc[j]);
    }
}

#[inline(always)]
fn v_rows<const W: usize>(
    src: &[u8],
    ss: usize,
    so: usize,
    h: usize,
    f: &[i16; 8],
    dst: &mut [u8],
    ds: usize,
) {
    let c: [u16; 8] = f.map(|v| v as u16);
    for i in 0..h {
        let mut acc = [64u16; W];
        for t in 0..8 {
            let b = so + (i + t) * ss;
            let p: &[u8; W] = src[b..b + W].try_into().unwrap();
            for j in 0..W {
                acc[j] = acc[j].wrapping_add(c[t].wrapping_mul(p[j] as u16));
            }
        }
        let o: &mut [u8; W] = (&mut dst[i * ds..i * ds + W]).try_into().unwrap();
        for j in 0..W {
            o[j] = narrow(acc[j]);
        }
    }
}

dispatch! {
    pub fn average(a: &[u8], b: &[u8], w: usize, h: usize, dst: &mut [u8], ds: usize) {
        for i in 0..h {
            let (ra, rb) = (&a[i * w..i * w + w], &b[i * w..i * w + w]);
            for ((d, &x), &y) in dst[i * ds..i * ds + w].iter_mut().zip(ra).zip(rb) {
                *d = ((x as u16 + y as u16 + 1) >> 1) as u8;
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MvLimits {
    pub row_min: i32,
    pub row_max: i32,
    pub col_min: i32,
    pub col_max: i32,
}

impl MvLimits {
    pub fn new(mi_row: usize, mi_col: usize, bsize: u8, mi_rows: usize, mi_cols: usize) -> Self {
        let bh = (BLOCK_H[bsize as usize] / 8).max(1) as i32;
        let bw = (BLOCK_W[bsize as usize] / 8).max(1) as i32;
        let (r, c) = (mi_row as i32, mi_col as i32);
        let to_top = -(r * 8 * 16);
        let to_bottom = (mi_rows as i32 - bh - r) * 8 * 16;
        let to_left = -(c * 8 * 16);
        let to_right = (mi_cols as i32 - bw - c) * 8 * 16;
        let spel_left = (4 + bw * 8) << 4;
        let spel_top = (4 + bh * 8) << 4;
        let row_min = to_top - spel_top;
        let col_min = to_left - spel_left;
        MvLimits {
            row_min: (row_min + 1) >> 1,
            row_max: (to_bottom + spel_top - 16).max(row_min) >> 1,
            col_min: (col_min + 1) >> 1,
            col_max: (to_right + spel_left - 16).max(col_min) >> 1,
        }
    }

    pub fn clamp(&self, mv: Mv) -> Mv {
        Mv::new(
            (mv.row as i32).clamp(self.row_min, self.row_max) as i16,
            (mv.col as i32).clamp(self.col_min, self.col_max) as i16,
        )
    }

    pub fn contains(&self, mv: Mv) -> bool {
        (self.row_min..=self.row_max).contains(&(mv.row as i32))
            && (self.col_min..=self.col_max).contains(&(mv.col as i32))
    }
}

pub struct BlockPos {
    pub mi_row: usize,
    pub mi_col: usize,
    pub bsize: u8,
    pub mi_rows: usize,
    pub mi_cols: usize,
}

fn clamped_mv16(p: &BlockPos, mv: Mv, sx: usize, sy: usize) -> (isize, isize) {
    let bh = (BLOCK_H[p.bsize as usize] / 8).max(1) as i32;
    let bw = (BLOCK_W[p.bsize as usize] / 8).max(1) as i32;
    let (r, c) = (p.mi_row as i32, p.mi_col as i32);
    let to_top = (-(r * 8 * 16)) >> sy;
    let to_bottom = ((p.mi_rows as i32 - bh - r) * 8 * 16) >> sy;
    let to_left = (-(c * 8 * 16)) >> sx;
    let to_right = ((p.mi_cols as i32 - bw - c) * 8 * 16) >> sx;
    let spel_left = (4 + ((bw * 8) >> sx)) << 4;
    let spel_top = (4 + ((bh * 8) >> sy)) << 4;
    let row = ((2 * mv.row as i32) >> sy).clamp(
        to_top - spel_top,
        (to_bottom + spel_top - 16).max(to_top - spel_top),
    );
    let col = ((2 * mv.col as i32) >> sx).clamp(
        to_left - spel_left,
        (to_right + spel_left - 16).max(to_left - spel_left),
    );
    (row as isize, col as isize)
}

pub fn predict_plane(
    refs: &[Plane],
    mvs: &[Mv],
    p: &BlockPos,
    plane: usize,
    filter: u8,
    dst: &mut [u8],
    ds: usize,
) {
    let (sx, sy) = if plane > 0 { (1, 1) } else { (0, 0) };
    let w = (BLOCK_W[p.bsize as usize] >> sx).max(4);
    let h = (BLOCK_H[p.bsize as usize] >> sy).max(4);
    let x = ((p.mi_col * 8) >> sx) as isize;
    let y = ((p.mi_row * 8) >> sy) as isize;
    let (r0, c0) = clamped_mv16(p, mvs[0], sx, sy);
    if mvs.len() < 2 {
        mc(
            &refs[0],
            (x << 4) + c0,
            (y << 4) + r0,
            w,
            h,
            filter,
            dst,
            ds,
        );
        return;
    }
    let mut a = [0u8; 64 * 64];
    let mut b = [0u8; 64 * 64];
    mc(
        &refs[0],
        (x << 4) + c0,
        (y << 4) + r0,
        w,
        h,
        filter,
        &mut a,
        w,
    );
    let (r1, c1) = clamped_mv16(p, mvs[1], sx, sy);
    mc(
        &refs[1],
        (x << 4) + c1,
        (y << 4) + r1,
        w,
        h,
        filter,
        &mut b,
        w,
    );
    average(&a, &b, w, h, dst, ds);
}

pub fn predict_block(
    refs: &[[Plane; 3]],
    mvs: &[Mv],
    p: &BlockPos,
    filter: u8,
    dst: [&mut [u8]; 3],
    ds: [usize; 3],
) {
    for (plane, d) in dst.into_iter().enumerate() {
        let r: [Plane; 2] = [refs[0][plane], refs[refs.len() - 1][plane]];
        predict_plane(&r[..mvs.len().min(2)], mvs, p, plane, filter, d, ds[plane]);
    }
}
