use super::inter::{h_pass, taps, v_pass};
use super::*;

pub struct HalfPel {
    planes: [Vec<u8>; 3],
    stride: usize,
    origin: usize,
    width: usize,
    height: usize,
    border: usize,
    filter: u8,
}

const MARGIN: isize = 8;

impl HalfPel {
    pub fn new(r: &Plane, filter: u8) -> HalfPel {
        let mut h = HalfPel {
            planes: Default::default(),
            stride: 0,
            origin: 0,
            width: 0,
            height: 0,
            border: 0,
            filter,
        };
        h.update(r, filter);
        h
    }

    pub fn update(&mut self, r: &Plane, filter: u8) {
        assert!(r.border >= 2 * MARGIN as usize);
        let rows = r.height + 2 * r.border;
        let len = rows * r.stride;
        for p in &mut self.planes {
            p.resize(len, 0);
        }
        (self.stride, self.origin) = (r.stride, r.origin);
        (self.width, self.height, self.border, self.filter) = (r.width, r.height, r.border, filter);
        let f = taps(filter, 8);
        let b = r.border as isize;
        let first = r.at(-b, -b);
        let span = r.width + 2 * r.border - 8;
        let xs = (0..span).step_by(64).map(|x| x.min(span - 64));
        let [hp, vp, hvp] = &mut self.planes;
        let s = r.stride;
        let vrows = rows - 8;
        for x in xs.clone() {
            h_pass(
                r.data,
                s,
                first + x,
                64,
                rows - 1,
                f,
                &mut hp[first + x + 3..],
                s,
            );
        }
        for x in xs.clone() {
            v_pass(
                r.data,
                s,
                first + x + 3,
                64,
                vrows,
                f,
                &mut vp[first + 3 * s + x + 3..],
                s,
            );
        }
        for x in xs {
            v_pass(
                hp,
                s,
                first + x + 3,
                64,
                vrows,
                f,
                &mut hvp[first + 3 * s + x + 3..],
                s,
            );
        }
    }

    pub fn filter(&self) -> u8 {
        self.filter
    }

    pub fn covers(&self, x: isize, y: isize, w: usize, h: usize) -> bool {
        let b = self.border as isize - MARGIN;
        x >= -b
            && y >= -b
            && x + w as isize <= self.width as isize + b
            && y + h as isize <= self.height as isize + b
    }

    pub fn at(&self, x16: isize, y16: isize) -> Option<(&[u8], usize)> {
        let (fx, fy) = (x16 & 15, y16 & 15);
        let k = match (fx, fy) {
            (8, 0) => 0,
            (0, 8) => 1,
            (8, 8) => 2,
            _ => return None,
        };
        let o = (self.origin as isize + (y16 >> 4) * self.stride as isize + (x16 >> 4)) as usize;
        Some((&self.planes[k][o..], self.stride))
    }
}
