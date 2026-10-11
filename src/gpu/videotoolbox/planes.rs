use crate::media::{Frame, PixelFormat, Plane, TimeBase};

pub struct Source<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

impl<'a> Source<'a> {
    fn row(&self, y: usize, x: usize, len: usize) -> Result<&'a [u8], String> {
        let start = y * self.stride + x;
        self.data
            .get(start..start + len)
            .ok_or_else(|| "decoded plane is smaller than the picture".to_string())
    }
}

pub struct Target<'a> {
    pub data: &'a mut [u8],
    pub stride: usize,
}

fn fits(len: usize, stride: usize, rows: usize, row: usize) -> bool {
    rows == 0 || (stride >= row && (rows - 1) * stride + row <= len)
}

fn plane(width: usize, height: usize, bytes: usize) -> Plane {
    Plane {
        data: Vec::with_capacity(width * bytes * height),
        stride: width * bytes,
        width,
        height,
    }
}

pub fn to_planar(
    format: PixelFormat,
    luma: Source,
    chroma: Source,
    origin: (usize, usize),
    size: (usize, usize),
    pts: i64,
) -> Result<Frame, String> {
    let bytes = format.bytes_per_sample();
    let ((ox, oy), (w, h)) = (origin, size);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (cx, cy) = (ox / 2, oy / 2);
    let mut y = plane(w, h, bytes);
    let mut u = plane(cw, ch, bytes);
    let mut v = plane(cw, ch, bytes);
    for r in 0..h {
        let row = luma.row(oy + r, ox * bytes, w * bytes)?;
        match format {
            PixelFormat::Yuv420p8 => y.data.extend_from_slice(row),
            PixelFormat::Yuv420p10 => {
                for s in row.chunks_exact(2) {
                    let value = u16::from_ne_bytes([s[0], s[1]]) >> 6;
                    y.data.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
    }
    for r in 0..ch {
        let row = chroma.row(cy + r, cx * 2 * bytes, cw * 2 * bytes)?;
        match format {
            PixelFormat::Yuv420p8 => {
                for p in row.chunks_exact(2) {
                    u.data.push(p[0]);
                    v.data.push(p[1]);
                }
            }
            PixelFormat::Yuv420p10 => {
                for q in row.chunks_exact(4) {
                    let a = u16::from_ne_bytes([q[0], q[1]]) >> 6;
                    let b = u16::from_ne_bytes([q[2], q[3]]) >> 6;
                    u.data.extend_from_slice(&a.to_le_bytes());
                    v.data.extend_from_slice(&b.to_le_bytes());
                }
            }
        }
    }
    Ok(Frame {
        format,
        width: w as u32,
        height: h as u32,
        pts,
        time_base: TimeBase::new(1, 90000),
        planes: [y, u, v],
    })
}

pub fn from_planar(frame: &Frame, luma: Target, chroma: Target) -> Result<(), String> {
    let bytes = frame.format.bytes_per_sample();
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let [y, u, v] = &frame.planes;
    if !fits(y.data.len(), y.stride, h, w * bytes)
        || !fits(u.data.len(), u.stride, ch, cw * bytes)
        || !fits(v.data.len(), v.stride, ch, cw * bytes)
    {
        return Err("the frame's planes are smaller than the picture".into());
    }
    if !fits(luma.data.len(), luma.stride, h, w * bytes)
        || !fits(chroma.data.len(), chroma.stride, ch, cw * 2 * bytes)
    {
        return Err("the encoder's input buffer is smaller than the picture".into());
    }
    let rows = luma
        .data
        .chunks_mut(luma.stride)
        .zip(y.data.chunks(y.stride));
    for (dst, src) in rows.take(h) {
        let (dst, src) = (&mut dst[..w * bytes], &src[..w * bytes]);
        match frame.format {
            PixelFormat::Yuv420p8 => dst.copy_from_slice(src),
            PixelFormat::Yuv420p10 => {
                for (d, s) in dst.chunks_exact_mut(2).zip(src.chunks_exact(2)) {
                    let value = u16::from_le_bytes([s[0], s[1]]) << 6;
                    d.copy_from_slice(&value.to_ne_bytes());
                }
            }
        }
    }
    let rows = chroma
        .data
        .chunks_mut(chroma.stride)
        .zip(u.data.chunks(u.stride))
        .zip(v.data.chunks(v.stride));
    for ((dst, us), vs) in rows.take(ch) {
        let (us, vs) = (&us[..cw * bytes], &vs[..cw * bytes]);
        match frame.format {
            PixelFormat::Yuv420p8 => {
                for ((d, &a), &b) in dst.chunks_exact_mut(2).zip(us).zip(vs) {
                    d[0] = a;
                    d[1] = b;
                }
            }
            PixelFormat::Yuv420p10 => {
                let pairs = us.chunks_exact(2).zip(vs.chunks_exact(2));
                for (d, (a, b)) in dst.chunks_exact_mut(4).zip(pairs) {
                    let a = u16::from_le_bytes([a[0], a[1]]) << 6;
                    let b = u16::from_le_bytes([b[0], b[1]]) << 6;
                    d[..2].copy_from_slice(&a.to_ne_bytes());
                    d[2..].copy_from_slice(&b.to_ne_bytes());
                }
            }
        }
    }
    Ok(())
}
