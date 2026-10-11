use super::tables::*;
use crate::vp9::frame::{MiInfo, Mv};

pub struct MvEnv<'a, F: Fn(usize, usize) -> MiInfo> {
    pub mi: F,
    pub prev: Option<&'a [MiInfo]>,
    pub mi_rows: usize,
    pub mi_cols: usize,
    pub tile_start: usize,
    pub tile_end: usize,
    pub sign_bias: [bool; 4],
    pub allow_hp: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefMvs {
    pub nearest: Mv,
    pub near: Mv,
    pub best: Mv,
    pub list: [Mv; 2],
    pub mode_context: u8,
}

pub fn use_mv_hp(mv: Mv) -> bool {
    ((mv.row as i32).abs() >> 3) < COMPANDED_MVREF_THRESH
        && ((mv.col as i32).abs() >> 3) < COMPANDED_MVREF_THRESH
}

struct List {
    mv: [Mv; 2],
    n: usize,
}

impl List {
    fn add(&mut self, mv: Mv) {
        if self.n >= 2 || (self.n > 0 && mv == self.mv[0]) {
            return;
        }
        self.mv[self.n] = mv;
        self.n += 1;
    }

    fn same(&mut self, m: &MiInfo, rf: i8) {
        for j in 0..2 {
            if m.ref_frame[j] == rf {
                self.add(m.mv[j][3]);
                return;
            }
        }
    }

    fn diff(&mut self, m: &MiInfo, rf: i8, bias: &[bool; 4]) {
        let mvs = [m.mv[0][3], m.mv[1][3]];
        let scale = |f: i8, mv: Mv| {
            if bias[f as usize] != bias[rf as usize] {
                Mv::new(-(mv.row as i32), -(mv.col as i32))
            } else {
                mv
            }
        };
        if m.ref_frame[0] > INTRA_FRAME && m.ref_frame[0] != rf {
            self.add(scale(m.ref_frame[0], mvs[0]));
        }
        if m.ref_frame[1] > INTRA_FRAME && m.ref_frame[1] != rf && mvs[0] != mvs[1] {
            self.add(scale(m.ref_frame[1], mvs[1]));
        }
    }
}

fn clamp_row(e_rows: usize, r: usize, bsize: u8, v: i32, border: i32) -> i32 {
    let bh = NUM_8X8_HIGH[bsize as usize] as i32;
    let top = -((r as i32 * 8) * 8);
    let bottom = ((e_rows as i32 - bh - r as i32) * 8) * 8;
    v.clamp(top - border, (bottom + border).max(top - border))
}

fn clamp_col(e_cols: usize, c: usize, bsize: u8, v: i32, border: i32) -> i32 {
    let bw = NUM_8X8_WIDE[bsize as usize] as i32;
    let left = -((c as i32 * 8) * 8);
    let right = ((e_cols as i32 - bw - c as i32) * 8) * 8;
    v.clamp(left - border, (right + border).max(left - border))
}

impl<F: Fn(usize, usize) -> MiInfo> MvEnv<'_, F> {
    fn inside(&self, r: isize, c: isize) -> bool {
        r >= 0
            && (r as usize) < self.mi_rows
            && c >= self.tile_start as isize
            && (c as usize) < self.tile_end
    }

    pub fn find(&self, r: usize, c: usize, bsize: u8, rf: i8, block: i32) -> ([Mv; 2], u8) {
        let mut l = List {
            mv: [Mv::ZERO; 2],
            n: 0,
        };
        let mut different = false;
        let mut counter = 0usize;
        let search = &MV_REF_BLOCKS[bsize as usize * 16..bsize as usize * 16 + 16];
        let (ri, ci) = (r as isize, c as isize);
        for i in 0..2 {
            let (cr, cc) = (ri + search[i * 2] as isize, ci + search[i * 2 + 1] as isize);
            if self.inside(cr, cc) {
                let m = (self.mi)(cr as usize, cc as usize);
                different = true;
                counter += MODE_2_COUNTER[m.y_mode as usize] as usize;
                for j in 0..2 {
                    if m.ref_frame[j] == rf {
                        let idx = if block >= 0 {
                            IDX_N_COLUMN_TO_SUBBLOCK[block as usize]
                                [(search[i * 2 + 1] == 0) as usize]
                                as usize
                        } else {
                            3
                        };
                        l.add(m.mv[j][idx]);
                        break;
                    }
                }
            }
        }
        for i in 2..8 {
            let (cr, cc) = (ri + search[i * 2] as isize, ci + search[i * 2 + 1] as isize);
            if self.inside(cr, cc) {
                different = true;
                l.same(&(self.mi)(cr as usize, cc as usize), rf);
            }
        }
        let prev = self.prev.map(|p| p[r * self.mi_cols + c]);
        if let Some(p) = &prev {
            l.same(p, rf);
        }
        if different {
            for i in 0..8 {
                let (cr, cc) = (ri + search[i * 2] as isize, ci + search[i * 2 + 1] as isize);
                if self.inside(cr, cc) {
                    l.diff(&(self.mi)(cr as usize, cc as usize), rf, &self.sign_bias);
                }
            }
        }
        if let Some(p) = &prev {
            l.diff(p, rf, &self.sign_bias);
        }
        let ctx = COUNTER_TO_CONTEXT[counter.min(18)];
        let list = l.mv.map(|mv| {
            Mv::new(
                clamp_row(self.mi_rows, r, bsize, mv.row as i32, MV_BORDER),
                clamp_col(self.mi_cols, c, bsize, mv.col as i32, MV_BORDER),
            )
        });
        (list, ctx)
    }

    pub fn best(&self, r: usize, c: usize, bsize: u8, list: [Mv; 2]) -> [Mv; 2] {
        list.map(|mv| {
            let (mut dr, mut dc) = (mv.row as i32, mv.col as i32);
            if !self.allow_hp || !use_mv_hp(mv) {
                if dr & 1 != 0 {
                    dr += if dr > 0 { -1 } else { 1 };
                }
                if dc & 1 != 0 {
                    dc += if dc > 0 { -1 } else { 1 };
                }
            }
            let border = (BORDERINPIXELS - INTERP_EXTEND) << 3;
            Mv::new(
                clamp_row(self.mi_rows, r, bsize, dr, border),
                clamp_col(self.mi_cols, c, bsize, dc, border),
            )
        })
    }

    pub fn refs(&self, r: usize, c: usize, bsize: u8, rf: i8) -> RefMvs {
        let (list, mode_context) = self.find(r, c, bsize, rf, -1);
        let b = self.best(r, c, bsize, list);
        RefMvs {
            nearest: b[0],
            near: b[1],
            best: b[0],
            list,
            mode_context,
        }
    }
}
