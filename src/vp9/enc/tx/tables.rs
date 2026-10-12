pub use crate::vp9::spec_tables::{COEFBAND_4X4, COEFBAND_8X8PLUS, ENERGY_CLASS};
pub use crate::vp9::tables::{CAT_PROBS, scan};

pub fn bands(tx_size: u8) -> &'static [u8] {
    if tx_size == 0 {
        &COEFBAND_4X4
    } else {
        &COEFBAND_8X8PLUS
    }
}

pub struct ScanSet {
    pub neighbors: Vec<(u16, u16)>,
    pub iscan_t: Vec<u16>,
}

fn build(tx: u8, ty: u8) -> ScanSet {
    let sc = scan(tx, ty);
    let log2n = 2 + tx as usize;
    let n = 1usize << log2n;
    let mut neighbors = vec![(0u16, 0u16); sc.len()];
    let mut iscan_t = vec![0u16; sc.len()];
    for (c, &pos) in sc.iter().enumerate() {
        let pos = pos as usize;
        let (i, j) = (pos >> log2n, pos & (n - 1));
        iscan_t[(j << log2n) | i] = c as u16;
        if c == 0 {
            continue;
        }
        neighbors[c] = if i > 0 && j > 0 {
            let a = ((i - 1) * n + j) as u16;
            let l = (i * n + j - 1) as u16;
            match if tx == 3 { 0 } else { ty } {
                2 => (a, a),
                1 => (l, l),
                _ => (a, l),
            }
        } else if i > 0 {
            let a = ((i - 1) * n + j) as u16;
            (a, a)
        } else {
            let l = (i * n + j - 1) as u16;
            (l, l)
        };
    }
    ScanSet { neighbors, iscan_t }
}

pub fn scan_set(tx_size: u8, tx_type: u8) -> &'static ScanSet {
    use std::sync::LazyLock;
    static ALL: LazyLock<Vec<ScanSet>> =
        LazyLock::new(|| (0..16u8).map(|k| build(k >> 2, k & 3)).collect());
    &ALL[((tx_size.min(3) << 2) | if tx_size >= 3 { 0 } else { tx_type & 3 }) as usize]
}

pub fn neighbors(tx_size: u8, tx_type: u8) -> &'static [(u16, u16)] {
    &scan_set(tx_size, tx_type).neighbors
}
