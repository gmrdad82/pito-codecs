use super::dist;
use super::inter;
use super::intra::{EdgeFlags, Edges};
use super::motion::{Search, Subpel, sad_per_bit};
use super::mvcost::{MvCost, MvProbs};
use super::*;
use crate::vp9::frame::{Frame, FrameInfo};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }
    fn below(&mut self, n: usize) -> usize {
        self.next() as usize % n
    }
}

fn texture(rng: &mut Rng, w: usize, h: usize) -> Vec<u8> {
    let mut v = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let base = (x * 3 + y * 5 + (x * y) / 7) % 256;
            v[y * w + x] = (base as u32 + rng.next() % 48).min(255) as u8;
        }
    }
    v
}

#[test]
fn intra_matches_decoder() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (w, h) = (96usize, 80usize);
    for _ in 0..400 {
        let plane = texture(&mut rng, w, h);
        let tx = rng.below(4) as u8;
        let size = 4usize << tx;
        let x = rng.below((w - size) / size + 1) * size;
        let y = rng.below((h - size) / size + 1) * size;
        let f = EdgeFlags {
            have_left: x > 0 && rng.below(4) > 0,
            have_above: y > 0 && rng.below(4) > 0,
            not_on_right: rng.below(2) == 0,
            max_x: (x + rng.below(2 * size + 8)).min(w - 1),
            max_y: (y + rng.below(2 * size + 8)).min(h - 1),
        };
        for mode in 0..10u8 {
            let mut ours = vec![0u8; size * size];
            Edges::build(&plane, w, x, y, tx, f).predict(mode, &mut ours, size);
            let mut theirs: Vec<u16> = plane.iter().map(|&v| v as u16).collect();
            let e = crate::vp9::intra::IntraEdge {
                have_left: f.have_left,
                have_above: f.have_above,
                not_on_right: f.not_on_right,
                tx_size: tx,
                max_x: f.max_x,
                max_y: f.max_y,
                bit_depth: 8,
            };
            crate::vp9::intra::predict(&mut theirs, w, x, y, mode, &e, None);
            for i in 0..size {
                for j in 0..size {
                    assert_eq!(
                        ours[i * size + j] as u16,
                        theirs[(y + i) * w + x + j],
                        "mode {mode} tx {tx} at ({x},{y}) pixel ({j},{i}) flags {f:?}"
                    );
                }
            }
        }
    }
}

struct Padded {
    data: Vec<u8>,
    stride: usize,
    origin: usize,
    w: usize,
    h: usize,
    border: usize,
}

impl Padded {
    fn new(src: &[u8], w: usize, h: usize, border: usize) -> Self {
        let stride = w + 2 * border;
        let mut data = vec![0u8; stride * (h + 2 * border)];
        let origin = border * stride + border;
        for y in 0..h {
            data[origin + y * stride..origin + y * stride + w]
                .copy_from_slice(&src[y * w..y * w + w]);
        }
        extend_borders(&mut data, stride, origin, w, h, border);
        Padded {
            data,
            stride,
            origin,
            w,
            h,
            border,
        }
    }

    fn plane(&self) -> Plane<'_> {
        Plane {
            data: &self.data,
            stride: self.stride,
            origin: self.origin,
            width: self.w,
            height: self.h,
            border: self.border,
        }
    }
}

#[test]
fn inter_matches_decoder() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let (w, h) = (100usize, 70usize);
    let src = texture(&mut rng, w, h);
    let info = FrameInfo {
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
    let full: Vec<u16> = src.iter().map(|&v| v as u16).collect();
    let chroma = vec![0u16; 64 * 64];
    let frame = Frame::from_planes(info, [&full, &chroma, &chroma]);
    let r = crate::vp9::inter::RefPlane {
        frame: &frame,
        plane: 0,
        last_x: w as i32 - 1,
        last_y: h as i32 - 1,
    };
    let padded = Padded::new(&src, w, h, 96);
    let bare = Padded::new(&src, w, h, 0);
    let mut tmp = vec![0u16; 80 * 80];
    let mut win = vec![0u16; 80 * 80];
    let mut pred = vec![0u16; 64 * 64];
    for _ in 0..600 {
        let bw = 4usize << rng.below(5);
        let bh = 4usize << rng.below(5);
        let x16 = rng.below((w + 160) * 16) as i32 - 80 * 16;
        let y16 = rng.below((h + 160) * 16) as i32 - 80 * 16;
        let filter = rng.below(4) as u8;
        let mut theirs = vec![0u16; bw * bh];
        crate::vp9::inter::predict(
            &r,
            x16,
            y16,
            16,
            16,
            bw,
            bh,
            filter,
            8,
            &mut theirs,
            bw,
            false,
            &mut tmp,
            &mut win,
            &mut pred,
        );
        for p in [&padded, &bare] {
            let mut ours = vec![0u8; bw * bh];
            inter::mc(
                &p.plane(),
                x16 as isize,
                y16 as isize,
                bw,
                bh,
                filter,
                &mut ours,
                bw,
            );
            let ours: Vec<u16> = ours.iter().map(|&v| v as u16).collect();
            assert_eq!(
                ours, theirs,
                "{bw}x{bh} at ({x16},{y16})/16 filter {filter} border {}",
                p.border
            );
        }
    }
}

#[test]
fn distortion_kernels_match_scalar() {
    let mut rng = Rng(77);
    let a = texture(&mut rng, 80, 72);
    let b = texture(&mut rng, 96, 72);
    for &(w, h) in &[
        (4, 4),
        (4, 8),
        (8, 4),
        (8, 8),
        (8, 16),
        (16, 8),
        (16, 16),
        (16, 32),
        (32, 16),
        (32, 32),
        (32, 64),
        (64, 32),
        (64, 64),
    ] {
        for _ in 0..4 {
            let (ox, oy) = (rng.below(80 - w + 1), rng.below(72 - h + 1));
            let (pa, pb) = (&a[oy * 80 + ox..], &b[rng.below(96 - w + 1)..]);
            assert_eq!(
                dist::sad(pa, 80, pb, 96, w, h),
                dist::sad_scalar(pa, 80, pb, 96, w, h),
                "sad {w}x{h}"
            );
            assert_eq!(
                dist::sse(pa, 80, pb, 96, w, h),
                dist::sse_scalar(pa, 80, pb, 96, w, h),
                "sse {w}x{h}"
            );
            let s = if w < 8 || h < 8 {
                dist::satd4_scalar(pa, 80, pb, 96, w, h)
            } else {
                dist::satd8_scalar(pa, 80, pb, 96, w, h)
            };
            assert_eq!(dist::satd(pa, 80, pb, 96, w, h), s, "satd {w}x{h}");
        }
    }
}

#[test]
fn motion_search_finds_a_subpel_shift() {
    let (w, h) = (192usize, 128usize);
    let mut smooth = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let fx = x as f64 / 9.0;
            let fy = y as f64 / 7.0;
            smooth[y * w + x] = (128.0 + 60.0 * (fx.sin() + (fy + fx * 0.3).cos())) as u8;
        }
    }
    let reference = Padded::new(&smooth, w, h, 96);
    let truth = Mv::new(-10, 22);
    let (bx, by, bs) = (64usize, 48usize, 32usize);
    let mut src = vec![0u8; bs * bs];
    let x16 = ((bx as isize) << 4) + 2 * truth.col as isize;
    let y16 = ((by as isize) << 4) + 2 * truth.row as isize;
    inter::mc(&reference.plane(), x16, y16, bs, bs, EIGHTTAP, &mut src, bs);
    let cost = MvCost::new(MvProbs::default());
    let s = Search {
        src: &src,
        src_stride: bs,
        reference: reference.plane(),
        x: bx,
        y: by,
        w: bs,
        h: bs,
        limits: inter::MvLimits::new(by / 8, bx / 8, 9, h / 8, w / 8),
        mv_cost: &cost,
        ref_mv: Mv::ZERO,
        allow_hp: true,
        sad_per_bit: sad_per_bit(400),
        range: 16,
        subpel: Subpel::Full,
        filter: EIGHTTAP,
        half: None,
    };
    let m = s.run(&[Mv::ZERO]);
    assert_eq!(m.mv, truth, "{m:?}");
}

fn blk(
    src: &[u8],
    stride: usize,
    x: usize,
    y: usize,
    s: usize,
    tx: u8,
    mx: usize,
    my: usize,
) -> super::mode::IntraBlock<'_> {
    super::mode::IntraBlock {
        src: &src[y * stride + x..],
        src_stride: stride,
        recon: src,
        recon_stride: stride,
        x,
        y,
        w: s,
        h: s,
        tx_size: tx,
        have_left: x > 0,
        have_above: y > 0,
        max_x: mx,
        max_y: my,
    }
}

#[test]
#[ignore]
fn bench() {
    use super::mode::*;
    use std::time::Instant;
    let (w, h) = (1920usize, 1088usize);
    let mut rng = Rng(42);
    let mut a = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let fx = x as f64 / 13.0;
            let fy = y as f64 / 11.0;
            a[y * w + x] = (128.0
                + 50.0 * (fx.sin() + (fy + fx * 0.2).cos())
                + (rng.next() % 16) as f64) as u8;
        }
    }
    let reference = Padded::new(&a, w, h, 96);
    let mut cur = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            cur[y * w + x] = a[(y + 3).min(h - 1) * w + (x + 5).min(w - 1)];
        }
    }
    let cost = MvCost::new(MvProbs::default());
    let rates = [300u32, 400, 200, 900];
    let mut scratch = InterScratch::default();
    let (mi_rows, mi_cols) = (h / 8, w / 8);
    let t = Instant::now();
    let built = super::halfpel::HalfPel::new(&reference.plane(), EIGHTTAP);
    eprintln!(
        "half-pel planes: {:.2} ms per 1080p reference",
        t.elapsed().as_secs_f64() * 1e3
    );
    for half in [None, Some(built)] {
        eprintln!("half-pel planes in search: {}", half.is_some());
        for &(bs, bsize) in &[(64usize, 12u8), (32, 9), (16, 6), (8, 3)] {
            let mut s = f64::MAX;
            let mut n = 0;
            for _ in 0..3 {
                let t = Instant::now();
                n = 0;
                for by in (0..h).step_by(bs) {
                    for bx in (0..w).step_by(bs) {
                        let r = [InterRef {
                            frame: LAST_FRAME,
                            luma: reference.plane(),
                            nearest: Mv::new(-24, -40),
                            near: Mv::ZERO,
                            best: Mv::new(-24, -40),
                            hints: &[],
                            ref_rate: 100,
                            half: half.as_ref(),
                        }];
                        let c = InterChoice {
                            src: &cur[by * w + bx..],
                            src_stride: w,
                            pos: inter::BlockPos {
                                mi_row: by / 8,
                                mi_col: bx / 8,
                                bsize,
                                mi_rows,
                                mi_cols,
                            },
                            w: bs.min(w - bx),
                            h: bs.min(h - by),
                            refs: &r,
                            mode_rates: &rates,
                            is_inter_rate: 50,
                            mv_cost: &cost,
                            allow_hp: true,
                            filter: EIGHTTAP,
                            lambda: 400,
                            range: 16,
                            subpel: if std::env::var_os("SUBPEL_NONE").is_some() {
                                Subpel::None
                            } else {
                                Subpel::Quarter
                            },
                        };
                        std::hint::black_box(best_inter(&c, &mut scratch));
                        n += 1;
                    }
                }
                s = s.min(t.elapsed().as_secs_f64());
            }
            eprintln!(
                "best_inter {bs}x{bs}: {n} blocks, {:.2} ms per 1080p frame, {:.0} fps",
                s * 1e3,
                1.0 / s
            );
        }
    }
    let ym = [100u32; 10];
    let uvm = [[100u32; 10]; 10];
    let mut is = [IntraScratch::default(), IntraScratch::default()];
    let cw = w / 2;
    let cu = vec![128u8; cw * h / 2];
    for &(bs, tx, modes) in &[
        (32usize, 3u8, &ALL_INTRA[..]),
        (16, 2, &ALL_INTRA[..]),
        (8, 1, &ALL_INTRA[..]),
        (16, 2, &FAST_INTRA[..]),
    ] {
        let t = Instant::now();
        for by in (0..h).step_by(bs) {
            for bx in (0..w).step_by(bs) {
                let c = IntraChoice {
                    luma: blk(&cur, w, bx, by, bs, tx, w - 1, h - 1),
                    chroma: [
                        blk(
                            &cu,
                            cw,
                            bx / 2,
                            by / 2,
                            bs / 2,
                            tx.saturating_sub(1),
                            cw - 1,
                            h / 2 - 1,
                        ),
                        blk(
                            &cu,
                            cw,
                            bx / 2,
                            by / 2,
                            bs / 2,
                            tx.saturating_sub(1),
                            cw - 1,
                            h / 2 - 1,
                        ),
                    ],
                    y_mode_rates: &ym,
                    uv_mode_rates: &uvm,
                    lambda: 400,
                    modes,
                };
                std::hint::black_box(best_intra(&c, &mut is));
            }
        }
        let s = t.elapsed().as_secs_f64();
        eprintln!(
            "best_intra {bs}x{bs} ({} modes): {:.2} ms per 1080p frame, {:.0} fps",
            modes.len(),
            s * 1e3,
            1.0 / s
        );
    }
    let mut pred = vec![0u8; 64 * 64];
    for &bs in &[4usize, 8, 16, 32, 64] {
        let iters = 2_000_000 / bs;
        let t = Instant::now();
        for i in 0..iters {
            let x16 = ((i * 37) % 1500 * 16 + 5) as isize;
            let y16 = ((i * 13) % 900 * 16 + 9) as isize;
            inter::mc(
                &reference.plane(),
                x16,
                y16,
                bs,
                bs,
                EIGHTTAP,
                &mut pred,
                64,
            );
        }
        let s = t.elapsed().as_secs_f64();
        let mpix = (iters * bs * bs) as f64 / s / 1e6;
        let t = Instant::now();
        let mut acc = 0u32;
        for i in 0..iters {
            let o = (i * 37) % 1500 + (i * 13) % 900 * w;
            acc = acc.wrapping_add(dist::satd(&cur[o..], w, &pred, 64, bs, bs));
        }
        std::hint::black_box(acc);
        let s2 = t.elapsed().as_secs_f64();
        let t = Instant::now();
        for i in 0..iters {
            let o = (i * 37) % 1500 + (i * 13) % 900 * w;
            acc = acc.wrapping_add(dist::sad(&cur[o..], w, &pred, 64, bs, bs));
        }
        std::hint::black_box(acc);
        let s3 = t.elapsed().as_secs_f64();
        let px = (iters * bs * bs) as f64 / 1e6;
        eprintln!(
            "{bs}x{bs}: mc 2d {mpix:.0} Mpx/s, satd {:.0} Mpx/s, sad {:.0} Mpx/s",
            px / s2,
            px / s3
        );
    }
}

#[test]
fn half_pel_planes_match_mc_and_leave_the_search_unchanged() {
    let mut rng = Rng(99);
    let (w, h) = (150usize, 90usize);
    let src = texture(&mut rng, w, h);
    let reference = Padded::new(&src, w, h, 96);
    let plane = reference.plane();
    for filter in [EIGHTTAP, EIGHTTAP_SHARP] {
        let hp = super::halfpel::HalfPel::new(&plane, filter);
        for _ in 0..300 {
            let bs = 4usize << rng.below(5);
            let x = rng.below(w + 160 - bs) as isize - 80;
            let y = rng.below(h + 160 - bs) as isize - 80;
            if !hp.covers(x, y, bs, bs) {
                continue;
            }
            for (fx, fy) in [(8, 0), (0, 8), (8, 8)] {
                let (x16, y16) = (x * 16 + fx, y * 16 + fy);
                let mut want = vec![0u8; bs * bs];
                inter::mc(&plane, x16, y16, bs, bs, filter, &mut want, bs);
                let (p, ps) = hp.at(x16, y16).unwrap();
                for i in 0..bs {
                    assert_eq!(
                        &p[i * ps..i * ps + bs],
                        &want[i * bs..i * bs + bs],
                        "({x16},{y16}) {bs} filter {filter}"
                    );
                }
            }
        }
    }
}
