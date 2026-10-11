use crate::vp9::frame::Frame;
use crate::vp9::tables::SUBPEL_FILTERS;

#[inline(always)]
fn taps(filter: u8, frac: usize) -> &'static [i16] {
    let o = (filter as usize * 16 + frac) * 8;
    &SUBPEL_FILTERS[o..o + 8]
}

pub struct RefPlane<'a> {
    pub frame: &'a Frame,
    pub plane: usize,
    pub last_x: i32,
    pub last_y: i32,
}

impl RefPlane<'_> {
    #[inline]
    fn rows(&self, y0: isize, y1: isize) -> (&[u16], usize) {
        let f = self.frame;
        let g = &f.geo[self.plane];
        f.wait_plane_row(self.plane, y1 - 1);
        let start = (g.origin as isize + y0 * g.stride as isize - g.border_x as isize) as usize;
        let len = (y1 - y0) as usize * g.stride;
        (&f.plane(self.plane)[start..start + len], g.border_x)
    }

    #[inline]
    fn clamped_row(&self, y: i32) -> &[u16] {
        let y = y.clamp(0, self.last_y) as isize;
        let (rows, bx) = self.rows(y, y + 1);
        &rows[bx..]
    }
}

crate::vp9::isa::dispatch! {
#[allow(clippy::too_many_arguments)]
pub fn predict(
    r: &RefPlane,
    x: i32,
    y: i32,
    step_x: i32,
    step_y: i32,
    w: usize,
    h: usize,
    filter: u8,
    bit_depth: u8,
    out: &mut [u16],
    os: usize,
    tmp: &mut [u16],
    win: &mut [u16],
) {
    if step_x == 16 && step_y == 16 {
        predict_unscaled(r, x, y, w, h, filter, bit_depth, out, os, tmp, win);
    } else {
        predict_scaled(
            r, x, y, step_x, step_y, w, h, filter, bit_depth, out, os, tmp,
        );
    }
}
}

fn fetch_window(r: &RefPlane, x0: i32, y0: i32, w: usize, h: usize, win: &mut [u16]) -> usize {
    let ws = w + 7;
    for row in 0..h + 7 {
        let src = r.clamped_row(y0 - 3 + row as i32);
        let dst = &mut win[row * ws..row * ws + ws];
        let xs = x0 - 3;
        for (c, d) in dst.iter_mut().enumerate() {
            *d = src[(xs + c as i32).clamp(0, r.last_x) as usize];
        }
    }
    ws
}

#[allow(clippy::too_many_arguments)]
fn predict_unscaled(
    r: &RefPlane,
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    filter: u8,
    bit_depth: u8,
    out: &mut [u16],
    os: usize,
    tmp: &mut [u16],
    win: &mut [u16],
) {
    let (x0, y0) = (x >> 4, y >> 4);
    let (fx, fy) = ((x & 15) as usize, (y & 15) as usize);
    let max = (1i32 << bit_depth) - 1;
    let g = &r.frame.geo[r.plane];
    let inside = x0 - 3 >= -(g.border_x as i32)
        && y0 - 3 >= -(g.border_y as i32)
        && x0 + w as i32 + 5 <= (g.stride - g.border_x) as i32
        && y0 + h as i32 + 5 <= (g.aligned_h + g.border_y) as i32
        && r.last_x == g.width as i32 - 1
        && r.last_y == g.height as i32 - 1;
    let (src, ss, so): (&[u16], usize, usize) = if inside {
        let (rows, bx) = r.rows((y0 - 3) as isize, (y0 + h as i32 + 5) as isize);
        (rows, g.stride, (bx as i32 + x0 - 3) as usize)
    } else {
        let ws = fetch_window(r, x0, y0, w, h, win);
        (&win[..], ws, 0)
    };
    match (fx, fy) {
        (0, 0) => {
            for i in 0..h {
                let s = so + (i + 3) * ss + 3;
                out[i * os..i * os + w].copy_from_slice(&src[s..s + w]);
            }
        }
        (_, 0) => {
            let f = taps(filter, fx);
            for i in 0..h {
                filter_h(
                    &src[so + (i + 3) * ss..],
                    f,
                    max,
                    &mut out[i * os..i * os + w],
                );
            }
        }
        (0, _) => {
            let f = taps(filter, fy);
            for i in 0..h {
                filter_v(
                    &src[so + i * ss + 3..],
                    ss,
                    f,
                    max,
                    &mut out[i * os..i * os + w],
                );
            }
        }
        _ => {
            let fh = taps(filter, fx);
            let fv = taps(filter, fy);
            for i in 0..h + 7 {
                filter_h(&src[so + i * ss..], fh, max, &mut tmp[i * w..i * w + w]);
            }
            for i in 0..h {
                filter_v(&tmp[i * w..], w, fv, max, &mut out[i * os..i * os + w]);
            }
        }
    }
}

#[inline(always)]
fn filter_h(s: &[u16], f: &[i16], max: i32, o: &mut [u16]) {
    match o.len() {
        4 => filter_h_n::<4>(s, f, max, o),
        8 => filter_h_n::<8>(s, f, max, o),
        16 => filter_h_n::<16>(s, f, max, o),
        32 => filter_h_n::<32>(s, f, max, o),
        64 => filter_h_n::<64>(s, f, max, o),
        _ => {
            for (j, d) in o.iter_mut().enumerate() {
                let acc: i32 = (0..8).map(|t| f[t] as i32 * s[j + t] as i32).sum();
                *d = ((acc + 64) >> 7).clamp(0, max) as u16;
            }
        }
    }
}

#[inline(always)]
fn filter_h_n<const W: usize>(s: &[u16], f: &[i16], max: i32, o: &mut [u16]) {
    let Some(o) = o.first_chunk_mut::<W>() else {
        return;
    };
    if max == 255 {
        let mut acc = [64u16; W];
        for t in 0..8 {
            let c = f[t] as u16;
            let Some(p) = s.get(t..).and_then(|s| s.first_chunk::<W>()) else {
                return;
            };
            for j in 0..W {
                acc[j] = acc[j].wrapping_add(c.wrapping_mul(p[j]));
            }
        }
        narrow8(&acc, o);
        return;
    }
    let mut acc = [64i32; W];
    for t in 0..8 {
        let c = f[t] as i32;
        let Some(p) = s.get(t..).and_then(|s| s.first_chunk::<W>()) else {
            return;
        };
        for j in 0..W {
            acc[j] += c * p[j] as i32;
        }
    }
    for j in 0..W {
        o[j] = (acc[j] >> 7).clamp(0, max) as u16;
    }
}

#[inline(always)]
fn narrow8<const W: usize>(acc: &[u16; W], o: &mut [u16; W]) {
    for j in 0..W {
        o[j] = if acc[j] < 0xC000 {
            (acc[j] >> 7).min(255)
        } else {
            0
        };
    }
}

#[inline(always)]
fn filter_v(s: &[u16], ss: usize, f: &[i16], max: i32, o: &mut [u16]) {
    match o.len() {
        4 => filter_v_n::<4>(s, ss, f, max, o),
        8 => filter_v_n::<8>(s, ss, f, max, o),
        16 => filter_v_n::<16>(s, ss, f, max, o),
        32 => filter_v_n::<32>(s, ss, f, max, o),
        64 => filter_v_n::<64>(s, ss, f, max, o),
        _ => {
            for (j, d) in o.iter_mut().enumerate() {
                let acc: i32 = (0..8).map(|t| f[t] as i32 * s[j + t * ss] as i32).sum();
                *d = ((acc + 64) >> 7).clamp(0, max) as u16;
            }
        }
    }
}

#[inline(always)]
fn filter_v_n<const W: usize>(s: &[u16], ss: usize, f: &[i16], max: i32, o: &mut [u16]) {
    let Some(o) = o.first_chunk_mut::<W>() else {
        return;
    };
    if max == 255 {
        let mut acc = [64u16; W];
        for t in 0..8 {
            let c = f[t] as u16;
            let Some(p) = s.get(t * ss..).and_then(|s| s.first_chunk::<W>()) else {
                return;
            };
            for j in 0..W {
                acc[j] = acc[j].wrapping_add(c.wrapping_mul(p[j]));
            }
        }
        narrow8(&acc, o);
        return;
    }
    let mut acc = [64i32; W];
    for t in 0..8 {
        let c = f[t] as i32;
        let Some(p) = s.get(t * ss..).and_then(|s| s.first_chunk::<W>()) else {
            return;
        };
        for j in 0..W {
            acc[j] += c * p[j] as i32;
        }
    }
    for j in 0..W {
        o[j] = (acc[j] >> 7).clamp(0, max) as u16;
    }
}

#[allow(clippy::too_many_arguments)]
fn predict_scaled(
    r: &RefPlane,
    x: i32,
    y: i32,
    step_x: i32,
    step_y: i32,
    w: usize,
    h: usize,
    filter: u8,
    bit_depth: u8,
    out: &mut [u16],
    os: usize,
    tmp: &mut [u16],
) {
    let max = (1i32 << bit_depth) - 1;
    let ih = ((((h as i32 - 1) * step_y + 15) >> 4) + 8) as usize;
    for row in 0..ih {
        let src = r.clamped_row((y >> 4) + row as i32 - 3);
        for c in 0..w {
            let p = x + step_x * c as i32;
            let f = taps(filter, (p & 15) as usize);
            let mut acc = 0i32;
            for t in 0..8 {
                acc +=
                    f[t] as i32 * src[((p >> 4) + t as i32 - 3).clamp(0, r.last_x) as usize] as i32;
            }
            tmp[row * w + c] = ((acc + 64) >> 7).clamp(0, max) as u16;
        }
    }
    for rr in 0..h {
        let p = (y & 15) + step_y * rr as i32;
        let f = taps(filter, (p & 15) as usize);
        let base = (p >> 4) as usize;
        for c in 0..w {
            let mut acc = 0i32;
            for t in 0..8 {
                acc += f[t] as i32 * tmp[(base + t) * w + c] as i32;
            }
            out[rr * os + c] = ((acc + 64) >> 7).clamp(0, max) as u16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_path_with_unit_step_matches_unscaled() {
        let (w, h) = (40usize, 30usize);
        let data: Vec<u16> = (0..w * h)
            .map(|i| ((i * 37 + (i / w) * 11) % 256) as u16)
            .collect();
        let info = crate::vp9::frame::FrameInfo {
            width: w as u32,
            height: h as u32,
            ss_x: true,
            ss_y: true,
            bit_depth: 8,
            color_space: 0,
            color_range: false,
            render_width: w as u32,
            render_height: h as u32,
            key: true,
            intra_only: false,
        };
        let chroma = vec![0u16; w.div_ceil(2) * h.div_ceil(2)];
        let frame = Frame::from_planes(info, [&data, &chroma, &chroma]);
        let r = RefPlane {
            frame: &frame,
            plane: 0,
            last_x: w as i32 - 1,
            last_y: h as i32 - 1,
        };
        let mut tmp = vec![0u16; 80 * 80];
        for filter in 0..4u8 {
            for &(x, y) in &[
                (0, 0),
                (5, 3),
                (16 * 7 + 5, 16 * 9 + 11),
                (-40, -3),
                (16 * 36 + 1, 16 * 25 + 15),
                (16 * 10, 16 * 10 + 8),
            ] {
                for &(bw, bh) in &[(4usize, 4usize), (8, 8), (16, 8)] {
                    let mut a = vec![0u16; bw * bh];
                    let mut b = vec![0u16; bw * bh];
                    predict_unscaled(
                        &r,
                        x,
                        y,
                        bw,
                        bh,
                        filter,
                        8,
                        &mut a,
                        bw,
                        &mut tmp,
                        &mut [0u16; 71 * 71],
                    );
                    predict_scaled(&r, x, y, 16, 16, bw, bh, filter, 8, &mut b, bw, &mut tmp);
                    assert_eq!(a, b, "filter {filter} pos ({x},{y}) {bw}x{bh}");
                }
            }
        }
    }
}
