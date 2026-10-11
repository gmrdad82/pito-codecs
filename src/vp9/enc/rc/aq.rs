use super::q::{q_index_for, qstep};

pub const SEGMENTS: usize = 8;
const SCALE: [f64; SEGMENTS] = [0.76, 0.82, 0.88, 0.94, 1.0, 1.06, 1.13, 1.2];
const STRENGTH: f64 = 0.12;

#[derive(Clone, Debug, Default)]
pub struct Segmentation {
    pub enabled: bool,
    pub update_map: bool,
    pub update_data: bool,
    pub abs_delta: bool,
    pub tree_probs: [u8; 7],
    pub q: [i16; SEGMENTS],
    pub lf: [i8; SEGMENTS],
    pub map: Vec<u8>,
    pub mi_cols: usize,
    pub mi_rows: usize,
    block_seg: Vec<u8>,
}

impl Segmentation {
    pub fn segment(&self, mi_row: usize, mi_col: usize) -> u8 {
        if !self.enabled {
            return 0;
        }
        self.map[mi_row * self.mi_cols + mi_col]
    }

    pub fn q_index(&self, base: u8, seg: u8) -> u8 {
        if !self.enabled {
            return base;
        }
        let q = self.q[seg as usize] as i32;
        if self.abs_delta {
            q.clamp(0, 255) as u8
        } else {
            (base as i32 + q).clamp(0, 255) as u8
        }
    }

    pub fn disable(&mut self) {
        self.enabled = false;
        self.update_map = false;
        self.update_data = false;
    }

    pub fn variance_aq(
        &mut self,
        base_q: u8,
        mi_cols: usize,
        mi_rows: usize,
        activity: &[u32],
        blocks: (usize, usize),
    ) {
        let (bw, bh) = blocks;
        if base_q < 8 || bw * bh == 0 {
            self.disable();
            return;
        }
        let n = (bw * bh) as f64;
        let mean_log = activity
            .iter()
            .map(|&a| ((a + 16) as f64).ln())
            .sum::<f64>()
            / n;
        let base_step = qstep(base_q);
        let mut q = [0i16; SEGMENTS];
        let mut prev = i32::MIN;
        for (s, scale) in SCALE.iter().enumerate() {
            let qi = (q_index_for(base_step * scale) as i32)
                .clamp(1, 255)
                .max(prev);
            prev = qi;
            q[s] = (qi - base_q as i32) as i16;
        }
        let edges: [f64; SEGMENTS - 1] =
            std::array::from_fn(|i| ((SCALE[i] * SCALE[i + 1]).sqrt().ln()) / STRENGTH);
        let mut block_seg = std::mem::take(&mut self.block_seg);
        block_seg.resize(bw * bh, 0);
        for (seg, &a) in block_seg.iter_mut().zip(activity) {
            let d = ((a + 16) as f64).ln() - mean_log;
            *seg = edges.partition_point(|&e| e < d) as u8;
        }
        self.map.resize(mi_cols * mi_rows, 0);
        let mut counts = [0u32; SEGMENTS];
        for r in 0..mi_rows {
            let by = (r / 2).min(bh - 1);
            let row = &mut self.map[r * mi_cols..][..mi_cols];
            for (c, m) in row.iter_mut().enumerate() {
                let s = block_seg[by * bw + (c / 2).min(bw - 1)];
                *m = s;
                counts[s as usize] += 1;
            }
        }
        self.block_seg = block_seg;
        self.enabled = true;
        self.update_map = true;
        self.update_data = true;
        self.abs_delta = false;
        self.q = q;
        self.lf = [0; SEGMENTS];
        self.mi_cols = mi_cols;
        self.mi_rows = mi_rows;
        self.tree_probs = tree_probs(&counts);
    }
}

pub fn tree_probs(c: &[u32; SEGMENTS]) -> [u8; 7] {
    let p = |l: u32, r: u32| {
        let t = l + r;
        if t == 0 {
            255
        } else {
            ((l as u64 * 256 + t as u64 / 2) / t as u64).clamp(1, 255) as u8
        }
    };
    [
        p(c[0] + c[1] + c[2] + c[3], c[4] + c[5] + c[6] + c[7]),
        p(c[0] + c[1], c[2] + c[3]),
        p(c[4] + c[5], c[6] + c[7]),
        p(c[0], c[1]),
        p(c[2], c[3]),
        p(c[4], c[5]),
        p(c[6], c[7]),
    ]
}
