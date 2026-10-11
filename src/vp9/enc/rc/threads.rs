use std::sync::atomic::{AtomicUsize, Ordering};

pub const MIN_TILE_WIDTH_B64: usize = 4;
pub const MAX_TILE_WIDTH_B64: usize = 64;
pub const LAG: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileLayout {
    pub log2_cols: u8,
    pub log2_rows: u8,
    pub mi_cols: usize,
    pub mi_rows: usize,
    pub cols: Vec<(usize, usize)>,
    pub rows: Vec<(usize, usize)>,
}

pub fn min_log2_tile_cols(sb_cols: usize) -> u8 {
    let mut l = 0;
    while (MAX_TILE_WIDTH_B64 << l) < sb_cols {
        l += 1;
    }
    l
}

pub fn max_log2_tile_cols(sb_cols: usize) -> u8 {
    let mut l = 1;
    while (sb_cols >> l) >= MIN_TILE_WIDTH_B64 {
        l += 1;
    }
    l - 1
}

pub fn tile_offset(i: usize, mis: usize, log2: u8) -> usize {
    let sbs = mis.div_ceil(8);
    (((i * sbs) >> log2) << 3).min(mis)
}

impl TileLayout {
    pub fn new(mi_cols: usize, mi_rows: usize, threads: usize) -> TileLayout {
        let want = threads.max(1).next_power_of_two().trailing_zeros() as u8;
        TileLayout::with_log2(mi_cols, mi_rows, want, 0)
    }

    pub fn with_log2(mi_cols: usize, mi_rows: usize, log2_cols: u8, log2_rows: u8) -> TileLayout {
        let sb_cols = mi_cols.div_ceil(8);
        let lo = min_log2_tile_cols(sb_cols);
        let hi = max_log2_tile_cols(sb_cols).max(lo);
        let log2_cols = log2_cols.clamp(lo, hi);
        let log2_rows = log2_rows.min(2);
        let span = |n: usize, mis: usize, log2: u8| {
            (0..n)
                .map(|i| (tile_offset(i, mis, log2), tile_offset(i + 1, mis, log2)))
                .collect::<Vec<_>>()
        };
        TileLayout {
            log2_cols,
            log2_rows,
            mi_cols,
            mi_rows,
            cols: span(1 << log2_cols, mi_cols, log2_cols),
            rows: span(1 << log2_rows, mi_rows, log2_rows),
        }
    }

    pub fn sb_cols(&self) -> usize {
        self.mi_cols.div_ceil(8)
    }

    pub fn sb_rows(&self) -> usize {
        self.mi_rows.div_ceil(8)
    }

    pub fn tile_row_of(&self, sb_row: usize) -> usize {
        let mi = sb_row * 8;
        self.rows.iter().position(|&(_, e)| mi < e).unwrap_or(0)
    }
}

pub struct Wavefront {
    layout: TileLayout,
    progress: Vec<AtomicUsize>,
    next: AtomicUsize,
}

pub struct Row<'a> {
    pub tile_col: usize,
    pub tile_row: usize,
    pub sb_row: usize,
    pub sb_start: usize,
    pub sb_end: usize,
    above: Option<&'a AtomicUsize>,
    me: &'a AtomicUsize,
}

impl Row<'_> {
    pub fn wait(&self, sb_col: usize) {
        let Some(above) = self.above else {
            return;
        };
        let need = (sb_col + LAG).min(self.sb_end) - self.sb_start;
        let mut spins = 0u32;
        while above.load(Ordering::Acquire) < need {
            if spins < 256 {
                std::hint::spin_loop();
                spins += 1;
            } else {
                std::thread::yield_now();
            }
        }
    }

    pub fn publish(&self, sb_col: usize) {
        self.me.store(sb_col + 1 - self.sb_start, Ordering::Release);
    }

    pub fn sbs(&self) -> Sbs<'_, '_> {
        Sbs {
            row: self,
            at: self.sb_start,
        }
    }
}

pub struct Sbs<'r, 'a> {
    row: &'r Row<'a>,
    at: usize,
}

impl Iterator for Sbs<'_, '_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        if self.at > self.row.sb_start {
            self.row.publish(self.at - 1);
        }
        if self.at >= self.row.sb_end {
            return None;
        }
        self.row.wait(self.at);
        self.at += 1;
        Some(self.at - 1)
    }
}

struct Finish<'a>(&'a AtomicUsize, usize);

impl Drop for Finish<'_> {
    fn drop(&mut self) {
        self.0.store(self.1, Ordering::Release);
    }
}

impl Wavefront {
    pub fn new(layout: TileLayout) -> Wavefront {
        let n = layout.cols.len() * layout.sb_rows();
        Wavefront {
            layout,
            progress: (0..n).map(|_| AtomicUsize::new(0)).collect(),
            next: AtomicUsize::new(0),
        }
    }

    pub fn layout(&self) -> &TileLayout {
        &self.layout
    }

    pub fn jobs(&self) -> usize {
        self.progress.len()
    }

    pub fn run<S: Send>(&self, workers: &mut [S], f: impl Fn(&mut S, &Row) + Sync) {
        for p in &self.progress {
            p.store(0, Ordering::Relaxed);
        }
        self.next.store(0, Ordering::Relaxed);
        let work = |w: &mut S| {
            loop {
                let j = self.next.fetch_add(1, Ordering::Relaxed);
                if j >= self.progress.len() {
                    break;
                }
                let row = self.row(j);
                let _done = Finish(row.me, row.sb_end - row.sb_start);
                f(w, &row);
            }
        };
        match workers {
            [] => {}
            [w] => work(w),
            _ => rayon::in_place_scope(|s| {
                let (first, rest) = workers.split_first_mut().unwrap();
                for w in rest {
                    let work = &work;
                    s.spawn(move |_| work(w));
                }
                work(first);
            }),
        }
    }

    fn row(&self, j: usize) -> Row<'_> {
        let tcs = self.layout.cols.len();
        let (sb_row, tile_col) = (j / tcs, j % tcs);
        let (c0, c1) = self.layout.cols[tile_col];
        Row {
            tile_col,
            tile_row: self.layout.tile_row_of(sb_row),
            sb_row,
            sb_start: c0 / 8,
            sb_end: c1.div_ceil(8),
            above: (sb_row > 0).then(|| &self.progress[j - tcs]),
            me: &self.progress[j],
        }
    }
}
