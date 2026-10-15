use std::sync::Mutex;

use rayon::prelude::*;

use crate::media::{Frame, PixelFormat, TimeBase};

pub struct Biplanar<'a> {
    pub luma: &'a [u8],
    pub chroma: &'a [u8],
    pub stride: usize,
    pub rows: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub crop: (u32, u32, u32, u32),
    pub bits: u32,
}

fn row(plane: &[u8], stride: usize, y: usize, x: usize, len: usize) -> Result<&[u8], String> {
    let start = y
        .checked_mul(stride)
        .and_then(|s| s.checked_add(x))
        .ok_or("plane offset overflows")?;
    plane
        .get(start..start.checked_add(len).ok_or("plane offset overflows")?)
        .ok_or_else(|| "plane row out of range".to_string())
}

const POOL_FRAMES: usize = 4;
const POOL_BYTES: usize = 64 << 20;

static POOL: Mutex<Vec<Frame>> = Mutex::new(Vec::new());

fn exact(f: &Frame, format: PixelFormat, w: u32, h: u32) -> bool {
    let bps = format.bytes_per_sample();
    let (w, h) = (w as usize, h as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    f.format == format
        && f.width as usize == w
        && f.height as usize == h
        && f.planes
            .iter()
            .zip([(w, h), (cw, ch), (cw, ch)])
            .all(|(p, (pw, ph))| {
                p.width == pw
                    && p.height == ph
                    && p.stride == pw * bps
                    && p.data.len() == pw * bps * ph
            })
}

fn bytes(f: &Frame) -> usize {
    f.planes.iter().map(|p| p.data.len()).sum()
}

fn take(format: PixelFormat, w: u32, h: u32) -> Frame {
    let found = POOL.lock().ok().and_then(|mut pool| {
        let i = pool.iter().position(|f| exact(f, format, w, h))?;
        Some(pool.swap_remove(i))
    });
    match found {
        Some(mut f) => {
            f.pts = 0;
            f.time_base = TimeBase::new(1, 90000);
            f
        }
        None => Frame::new(format, w, h, 0, TimeBase::new(1, 90000)),
    }
}

pub(crate) fn recycle(frame: Frame) {
    if !exact(&frame, frame.format, frame.width, frame.height) || bytes(&frame) > POOL_BYTES {
        return;
    }
    let mut stale = Vec::new();
    if let Ok(mut pool) = POOL.lock() {
        pool.push(frame);
        let mut total: usize = pool.iter().map(bytes).sum();
        while pool.len() > POOL_FRAMES || total > POOL_BYTES {
            let old = pool.remove(0);
            total -= bytes(&old);
            stale.push(old);
        }
    }
    drop(stale);
}

fn bands(rows: usize) -> usize {
    rayon::current_num_threads()
        .min(8)
        .min(rows.div_ceil(64))
        .max(1)
}

type Band<'a> = (usize, ((&'a mut [u8], &'a mut [u8]), &'a mut [u8]));

pub fn to_frame(src: &Biplanar, g: &Geometry) -> Result<Frame, String> {
    let (cx, cy, w, h) = g.crop;
    let (ox, oy, w, h) = (cx as usize, cy as usize, w as usize, h as usize);
    if w == 0 || h == 0 || !matches!(g.bits, 8 | 10) {
        return Err(format!("cannot read a {w}x{h} picture of {} bits", g.bits));
    }
    let bps = if g.bits > 8 { 2 } else { 1 };
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (cox, coy) = (ox / 2, oy / 2);
    let needs_rows = oy.checked_add(h).ok_or("picture size overflows")?;
    let needs_cols = ox
        .checked_add(w)
        .and_then(|c| c.checked_mul(bps))
        .ok_or("picture size overflows")?;
    if src.rows < needs_rows || src.stride < needs_cols || src.stride < (cox + cw) * 2 * bps {
        return Err(format!(
            "decoded surface ({} rows, {} byte rows) is smaller than the {w}x{h} picture at {ox},{oy}",
            src.rows, src.stride
        ));
    }
    let format = if bps == 1 {
        PixelFormat::Yuv420p8
    } else {
        PixelFormat::Yuv420p10
    };
    let mut frame = take(format, w as u32, h as u32);
    let [yp, up, vp] = &mut frame.planes;
    let shift = 16 - g.bits;
    let bands = bands(ch);
    let rows_per = ch.div_ceil(bands).max(1);
    let luma_rows = rows_per * 2;
    let lw = w * bps;
    let cwb = cw * bps;
    let work = |(band, ((ydst, udst), vdst)): Band| -> Result<(), String> {
        let y0 = band * luma_rows;
        for (r, dst) in ydst.chunks_exact_mut(lw).enumerate() {
            let line = row(src.luma, src.stride, oy + y0 + r, ox * bps, lw)?;
            if bps == 1 {
                dst.copy_from_slice(line);
            } else {
                for (d, s) in dst.chunks_exact_mut(2).zip(line.chunks_exact(2)) {
                    let v = u16::from_le_bytes([s[0], s[1]]) >> shift;
                    d.copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        let c0 = band * rows_per;
        for (r, (u, v)) in udst
            .chunks_exact_mut(cwb)
            .zip(vdst.chunks_exact_mut(cwb))
            .enumerate()
        {
            let line = row(
                src.chroma,
                src.stride,
                coy + c0 + r,
                cox * 2 * bps,
                cw * 2 * bps,
            )?;
            if bps == 1 {
                for ((p, u), v) in line.chunks_exact(2).zip(u.iter_mut()).zip(v.iter_mut()) {
                    *u = p[0];
                    *v = p[1];
                }
            } else {
                for ((q, u), v) in line
                    .chunks_exact(4)
                    .zip(u.chunks_exact_mut(2))
                    .zip(v.chunks_exact_mut(2))
                {
                    let a = u16::from_le_bytes([q[0], q[1]]) >> shift;
                    let b = u16::from_le_bytes([q[2], q[3]]) >> shift;
                    u.copy_from_slice(&a.to_le_bytes());
                    v.copy_from_slice(&b.to_le_bytes());
                }
            }
        }
        Ok(())
    };
    let done = if bands == 1 {
        work((0, ((&mut yp.data, &mut up.data), &mut vp.data)))
    } else {
        yp.data
            .par_chunks_mut(luma_rows * lw)
            .zip(up.data.par_chunks_mut(rows_per * cwb))
            .zip(vp.data.par_chunks_mut(rows_per * cwb))
            .enumerate()
            .try_for_each(work)
    };
    match done {
        Ok(()) => Ok(frame),
        Err(e) => {
            recycle(frame);
            Err(e)
        }
    }
}

pub fn from_frame(
    frame: &Frame,
    data: &mut [u8],
    luma: (usize, usize),
    chroma: (usize, usize),
    size: (u32, u32),
) -> Result<(), String> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (sw, sh) = (size.0 as usize, size.1 as usize);
    if w == 0 || h == 0 || w > sw || h > sh {
        return Err(format!("a {w}x{h} frame does not fit a {sw}x{sh} surface"));
    }
    let bps = frame.format.bytes_per_sample();
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let [yp, up, vp] = &frame.planes;
    if yp.stride < w * bps || up.stride < cw * bps || vp.stride < cw * bps {
        return Err("frame planes are narrower than the frame".into());
    }
    if yp.data.len() < (h - 1) * yp.stride + w * bps
        || up.data.len() < (ch - 1) * up.stride + cw * bps
        || vp.data.len() < (ch - 1) * vp.stride + cw * bps
    {
        return Err("frame planes are shorter than the frame".into());
    }
    if luma.0 > chroma.0 || luma.1 < sw * bps || chroma.1 < sw.div_ceil(2) * 2 * bps {
        return Err("unexpected NV12 layout".into());
    }
    let (scw, sch) = (sw.div_ceil(2), sh.div_ceil(2));
    let (head, chroma_plane) = data
        .split_at_mut_checked(chroma.0)
        .ok_or("chroma offset out of range")?;
    let luma_plane = head.get_mut(luma.0..).ok_or("luma offset out of range")?;
    if luma_plane.len() < sh * luma.1 || chroma_plane.len() < (sch - 1) * chroma.1 + scw * 2 * bps {
        return Err("NV12 image smaller than the surface".into());
    }
    let shift = if bps == 2 {
        16 - frame.format.bit_depth() as u32
    } else {
        0
    };
    let bands = bands(sch);
    let rows_per = sch.div_ceil(bands).max(1);
    let work = |(band, (ldst, cdst)): (usize, (&mut [u8], &mut [u8]))| {
        let y0 = band * rows_per * 2;
        for (r, dst) in ldst.chunks_mut(luma.1).enumerate() {
            let y = y0 + r;
            if y >= sh {
                break;
            }
            let sy = y.min(h - 1);
            let src = &yp.data[sy * yp.stride..sy * yp.stride + w * bps];
            let dst = &mut dst[..sw * bps];
            if bps == 1 {
                dst[..w].copy_from_slice(src);
                let last = src[w - 1];
                dst[w..].fill(last);
            } else {
                for (d, s) in dst.chunks_exact_mut(2).zip(src.chunks_exact(2)) {
                    let v = u16::from_le_bytes([s[0], s[1]]) << shift;
                    d.copy_from_slice(&v.to_le_bytes());
                }
                let last = [dst[w * 2 - 2], dst[w * 2 - 1]];
                for d in dst[w * 2..].chunks_exact_mut(2) {
                    d.copy_from_slice(&last);
                }
            }
        }
        let c0 = band * rows_per;
        for (r, dst) in cdst.chunks_mut(chroma.1).enumerate() {
            let y = c0 + r;
            if y >= sch {
                break;
            }
            let sy = y.min(ch - 1);
            let u = &up.data[sy * up.stride..sy * up.stride + cw * bps];
            let v = &vp.data[sy * vp.stride..sy * vp.stride + cw * bps];
            let dst = &mut dst[..scw * 2 * bps];
            if bps == 1 {
                for ((d, &u), &v) in dst.chunks_exact_mut(2).zip(u).zip(v) {
                    d[0] = u;
                    d[1] = v;
                }
                for x in cw..scw {
                    dst[x * 2] = u[cw - 1];
                    dst[x * 2 + 1] = v[cw - 1];
                }
            } else {
                for x in 0..scw {
                    let sx = x.min(cw - 1);
                    let uu = u16::from_le_bytes([u[sx * 2], u[sx * 2 + 1]]) << shift;
                    let vv = u16::from_le_bytes([v[sx * 2], v[sx * 2 + 1]]) << shift;
                    dst[x * 4..x * 4 + 2].copy_from_slice(&uu.to_le_bytes());
                    dst[x * 4 + 2..x * 4 + 4].copy_from_slice(&vv.to_le_bytes());
                }
            }
        }
    };
    if bands == 1 {
        work((0, (luma_plane, chroma_plane)));
    } else {
        luma_plane
            .par_chunks_mut(rows_per * 2 * luma.1)
            .zip(chroma_plane.par_chunks_mut(rows_per * chroma.1))
            .enumerate()
            .for_each(work);
    }
    Ok(())
}
