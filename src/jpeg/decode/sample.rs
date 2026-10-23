const Y_CF: i32 = 16384;
const CR_CF: i32 = 22970;
const CB_CF: i32 = 29032;
const G_CR: i32 = -11700;
const G_CB: i32 = -5638;
const PREC: i32 = 14;
const RND: i32 = (1 << (PREC - 1)) - 1;

fn mix(near: u8, far: u8) -> u8 {
    ((3 * u16::from(near) + u16::from(far) + 2) >> 2) as u8
}

pub fn horizontal(input: &[u8], out: &mut [u8]) {
    let w = input.len();
    if w == 0 || out.len() < 2 * w {
        return;
    }
    if w == 1 {
        out[..2].fill(input[0]);
        return;
    }
    out[0] = input[0];
    out[1] = mix(input[0], input[1]);
    for (o, i) in out[2..2 * w - 2].chunks_exact_mut(2).zip(input.windows(3)) {
        o[0] = mix(i[1], i[0]);
        o[1] = mix(i[1], i[2]);
    }
    out[2 * w - 2] = mix(input[w - 1], input[w - 2]);
    out[2 * w - 1] = input[w - 1];
}

pub fn vertical(cur: &[u8], far: &[u8], out: &mut [u8]) {
    for ((o, &c), &f) in out.iter_mut().zip(cur).zip(far) {
        *o = mix(c, f);
    }
}

pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub width: usize,
    pub height: usize,
}

impl Plane<'_> {
    fn row(&self, y: usize) -> &[u8] {
        let y = y.min(self.height.saturating_sub(1));
        let at = y * self.stride;
        self.data.get(at..at + self.width).unwrap_or(&[])
    }
}

pub struct Upsampler {
    line: Vec<u8>,
    rows: Vec<u8>,
}

impl Upsampler {
    pub fn new(src: &Plane, fx: usize, width: usize) -> super::Result<Upsampler> {
        let wide = if fx == 1 {
            0
        } else {
            (src.width * fx).max(width)
        };
        Ok(Upsampler {
            line: super::zeroed(wide, 0u8)?,
            rows: super::zeroed(src.width, 0u8)?,
        })
    }

    pub fn row<'a>(&'a mut self, src: &'a Plane, fx: usize, fy: usize, y: usize) -> &'a [u8] {
        let sy = y / fy;
        let cur = src.row(sy);
        match (fx, fy) {
            (1, 2) | (2, 2) => {
                let far = if y.is_multiple_of(2) {
                    src.row(sy.saturating_sub(1))
                } else {
                    src.row(sy + 1)
                };
                vertical(cur, far, &mut self.rows);
                if fx == 1 {
                    return &self.rows;
                }
                horizontal(&self.rows, &mut self.line);
            }
            (1, _) => return cur,
            (2, 1) => horizontal(cur, &mut self.line),
            _ => {
                for (o, &v) in self.line.chunks_exact_mut(fx).zip(cur) {
                    o.fill(v);
                }
            }
        }
        &self.line
    }
}

fn clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

pub fn ycbcr(y: &[u8], cb: &[u8], cr: &[u8], out: &mut [u8]) {
    for (((px, &y), &cb), &cr) in out.chunks_exact_mut(4).zip(y).zip(cb).zip(cr) {
        let cb = i32::from(cb) - 128;
        let cr = i32::from(cr) - 128;
        let y0 = i32::from(y) * Y_CF + RND;
        px[0] = clamp((y0 + cr * CR_CF) >> PREC);
        px[1] = clamp((y0 + cr * G_CR + cb * G_CB) >> PREC);
        px[2] = clamp((y0 + cb * CB_CF) >> PREC);
        px[3] = 255;
    }
}

pub fn grey(y: &[u8], out: &mut [u8]) {
    for (px, &y) in out.chunks_exact_mut(4).zip(y) {
        px.copy_from_slice(&[y, y, y, 255]);
    }
}
