use crate::hevc::inter::{self, Taps};
use crate::hevc::pel::Pel;
use crate::hevc::spec_tables::{CHROMA_FILTER, LUMA_FILTER};
use crate::hevc::transform;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn i16(&mut self, lim: i32) -> i16 {
        ((self.next() % (2 * lim as u64 + 1)) as i32 - lim) as i16
    }

    fn pick<T: Copy>(&mut self, s: &[T]) -> T {
        s[self.below(s.len())]
    }
}

fn both<R: PartialEq + std::fmt::Debug>(mut f: impl FnMut() -> R) -> (R, R) {
    super::force_off(true);
    let scalar = f();
    super::force_off(false);
    (scalar, f())
}

#[test]
fn kernels_match_scalar() {
    if !super::on() {
        return;
    }
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    transforms(&mut rng);
    mc::<u8>(&mut rng, 8);
    mc::<u16>(&mut rng, 10);
    put::<u8>(&mut rng, 8);
    put::<u16>(&mut rng, 10);
}

fn transforms(rng: &mut Rng) {
    for _ in 0..4000 {
        let n = rng.pick(&[4, 8, 16, 32]);
        let dst = n == 4 && rng.below(2) == 0;
        let (mx, my) = (rng.below(n), rng.below(n));
        let lim = rng.pick(&[64, 2048, 32767]);
        let mut c = vec![0i16; n * n];
        for y in 0..=my {
            for x in 0..=mx {
                if rng.below(3) > 0 {
                    c[y * n + x] = rng.i16(lim);
                }
            }
        }
        let bd_shift = 20 - rng.pick(&[8, 10]);
        let (a, b) = both(|| {
            let mut c = c.clone();
            transform::inverse_transform(&mut c, n, dst, mx, my, bd_shift);
            c
        });
        assert_eq!(a, b, "inverse {n}x{n} dst={dst} max=({mx},{my})");
        residual::<u8>(rng, n, 255, &a);
        residual::<u16>(rng, n, 1023, &a);
    }
}

fn residual<P: Pel>(rng: &mut Rng, n: usize, max: i32, c: &[i16]) {
    let stride = n + rng.below(9);
    let plane: Vec<P> = (0..stride * n)
        .map(|_| P::from_i32(rng.below(max as usize + 1) as i32))
        .collect();
    let (a, b) = both(|| {
        let mut p = plane.clone();
        transform::add_residual(&mut p, stride, n, c, max);
        p
    });
    assert_eq!(a, b, "add_residual {n}");
}

fn mc<P: Pel>(rng: &mut Rng, bd: u32) {
    for _ in 0..3000 {
        if rng.below(2) == 0 {
            let w = rng.pick(&[4, 8, 12, 16, 24, 32, 48, 64]);
            let h = rng.pick(&[4, 8, 12, 16, 24, 32, 48, 64]);
            let (fx, fy) = (rng.below(4), rng.below(4));
            interp::<8, P>(rng, bd, w, h, &LUMA_FILTER[fx], &LUMA_FILTER[fy], fx, fy);
        } else {
            let w = rng.pick(&[2, 4, 6, 8, 12, 16, 24, 32]);
            let h = rng.pick(&[2, 4, 6, 8, 12, 16, 24, 32]);
            let (fx, fy) = (rng.below(8), rng.below(8));
            interp::<4, P>(
                rng,
                bd,
                w,
                h,
                &CHROMA_FILTER[fx],
                &CHROMA_FILTER[fy],
                fx,
                fy,
            );
        }
    }
}

fn interp<const T: usize, P: Pel>(
    rng: &mut Rng,
    bd: u32,
    w: usize,
    h: usize,
    hf: &[i8; T],
    vf: &[i8; T],
    fx: usize,
    fy: usize,
) {
    let rows: Vec<Vec<P>> = (0..h + T - 1)
        .map(|_| {
            (0..w + T - 1)
                .map(|_| P::from_i32(rng.below(1 << bd) as i32))
                .collect()
        })
        .collect();
    let lines: Vec<&[P]> = rows.iter().map(|r| r.as_slice()).collect();
    let taps = Taps {
        fx,
        fy,
        hf,
        vf,
        shift1: bd - 8,
        shift3: 14 - bd,
    };
    let (a, b) = both(|| {
        let mut tmp = vec![0i16; 71 * 71];
        let mut out = vec![0i16; w * h];
        inter::interpolate::<T, P>(&lines, &mut tmp, &taps, w, h, &mut out);
        out
    });
    assert_eq!(a, b, "interp T={T} {w}x{h} f=({fx},{fy}) bd={bd}");
}

fn put<P: Pel>(rng: &mut Rng, bd: u32) {
    let max = (1i32 << bd) - 1;
    for _ in 0..3000 {
        let w = rng.pick(&[2, 4, 6, 8, 12, 16, 24, 32, 48, 64]);
        let h = rng.pick(&[1, 2, 4, 6, 8, 16, 64]);
        let ds = w + rng.below(20);
        let lim = rng.pick(&[1 << 14, 32767]);
        let p0: Vec<i16> = (0..w * h).map(|_| rng.i16(lim)).collect();
        let p1: Vec<i16> = (0..w * h).map(|_| rng.i16(lim)).collect();
        let base: Vec<P> = (0..ds * h)
            .map(|_| P::from_i32(rng.below(max as usize + 1) as i32))
            .collect();
        let log2wd = 14 - bd + rng.below(8) as u32;
        let (w0, w1) = (rng.i16(128) as i32, rng.i16(128) as i32);
        let (o0, o1) = (
            rng.i16(128) as i32 * (1 << (bd - 8)),
            rng.i16(128) as i32 * (1 << (bd - 8)),
        );
        let run = |k: usize| {
            both(|| {
                let mut d = base.clone();
                match k {
                    0 => inter::put_uni(&p0, w, h, bd, &mut d, ds),
                    1 => inter::put_bi(&p0, &p1, w, h, bd, &mut d, ds),
                    2 => inter::put_weighted_uni(&p0, w, h, bd, log2wd, w0, o0, &mut d, ds),
                    _ => inter::put_weighted_bi(
                        &p0, &p1, w, h, bd, log2wd, w0, w1, o0, o1, &mut d, ds,
                    ),
                }
                d
            })
        };
        for k in 0..4 {
            let (a, b) = run(k);
            assert_eq!(a, b, "put {k} {w}x{h} bd={bd}");
        }
    }
}

fn best(mut f: impl FnMut()) -> f64 {
    let mut b = f64::MAX;
    for _ in 0..40 {
        let t = std::time::Instant::now();
        f();
        b = b.min(t.elapsed().as_secs_f64());
    }
    b
}

fn race(name: &str, mut f: impl FnMut()) {
    super::force_off(true);
    let s = best(&mut f);
    super::force_off(false);
    let v = best(&mut f);
    println!(
        "{name:<28} scalar {:>8.1} us  simd {:>8.1} us  x{:.2}",
        s * 1e6,
        v * 1e6,
        s / v
    );
}

#[test]
#[ignore]
fn kernel_speed() {
    let mut rng = Rng(7);
    for n in [4usize, 8, 16, 32] {
        for (mx, my, tag) in [(1, 1, "sparse"), (n - 1, n - 1, "full")] {
            let (mx, my) = (mx.min(n - 1), my.min(n - 1));
            let blocks: Vec<Vec<i16>> = (0..256)
                .map(|_| {
                    let mut c = vec![0i16; n * n];
                    for y in 0..=my {
                        for x in 0..=mx {
                            c[y * n + x] = rng.i16(300);
                        }
                    }
                    c
                })
                .collect();
            let mut work = blocks.clone();
            race(&format!("idct {n}x{n} {tag}"), || {
                for (w, b) in work.iter_mut().zip(&blocks) {
                    w.copy_from_slice(b);
                    transform::inverse_transform(w, n, false, mx, my, 12);
                }
            });
        }
        let res = vec![3i16; n * n];
        let mut plane = vec![100u8; 64 * 1920];
        race(&format!("add_residual {n} u8"), || {
            for k in 0..256 {
                let o = (k % 32) * 48;
                transform::add_residual(&mut plane[o..], 1920, n, &res, 255);
            }
        });
    }
    let plane: Vec<Vec<u8>> = (0..80)
        .map(|_| (0..128).map(|_| rng.below(256) as u8).collect())
        .collect();
    let mut tmp = vec![0i16; 71 * 71];
    let mut out = vec![0i16; 64 * 64];
    for (w, h) in [(8, 8), (16, 16), (32, 32), (64, 64)] {
        for (fx, fy) in [(0, 0), (2, 0), (0, 2), (1, 3)] {
            let lines: Vec<&[u8]> = plane.iter().take(h + 7).map(|l| &l[..w + 7]).collect();
            let taps = Taps {
                fx,
                fy,
                hf: &LUMA_FILTER[fx],
                vf: &LUMA_FILTER[fy],
                shift1: 0,
                shift3: 6,
            };
            race(&format!("luma {w}x{h} f=({fx},{fy})"), || {
                for _ in 0..64 {
                    luma(&lines, &mut tmp, &taps, w, h, &mut out);
                }
            });
        }
    }
    for (w, h) in [(4, 4), (8, 8), (16, 16)] {
        let lines: Vec<&[u8]> = plane.iter().take(h + 3).map(|l| &l[..w + 3]).collect();
        let taps = Taps {
            fx: 3,
            fy: 5,
            hf: &CHROMA_FILTER[3],
            vf: &CHROMA_FILTER[5],
            shift1: 0,
            shift3: 6,
        };
        race(&format!("chroma {w}x{h} f=(3,5)"), || {
            for _ in 0..64 {
                chroma(&lines, &mut tmp, &taps, w, h, &mut out);
            }
        });
    }
    let p0: Vec<i16> = (0..4096).map(|_| rng.i16(8000) + 8000).collect();
    let p1: Vec<i16> = (0..4096).map(|_| rng.i16(8000) + 8000).collect();
    let mut dst = vec![0u8; 64 * 128];
    for (w, h) in [(4, 4), (8, 8), (16, 16), (64, 64)] {
        race(&format!("put_uni {w}x{h}"), || {
            for _ in 0..64 {
                inter::put_uni(&p0, w, h, 8, &mut dst, 128);
            }
        });
        race(&format!("put_bi {w}x{h}"), || {
            for _ in 0..64 {
                inter::put_bi(&p0, &p1, w, h, 8, &mut dst, 128);
            }
        });
    }
}

crate::hevc::isa::dispatch! {
fn luma(lines: &[&[u8]], tmp: &mut [i16], taps: &Taps<8>, w: usize, h: usize, out: &mut [i16]) {
    inter::interpolate::<8, u8>(lines, tmp, taps, w, h, out);
}
}

crate::hevc::isa::dispatch! {
fn chroma(lines: &[&[u8]], tmp: &mut [i16], taps: &Taps<4>, w: usize, h: usize, out: &mut [i16]) {
    inter::interpolate::<4, u8>(lines, tmp, taps, w, h, out);
}
}
