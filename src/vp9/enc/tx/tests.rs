use super::lane::{Isa, Scalar};
use super::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }

    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1) as u32) as i32
    }
}

#[allow(clippy::type_complexity, clippy::needless_range_loop)]
fn costs() -> ([[[u32; 2]; 6]; 6], [[[u32; 11]; 6]; 6]) {
    let mut more = [[[0u32; 2]; 6]; 6];
    let mut token = [[[0u32; 11]; 6]; 6];
    for b in 0..6 {
        for c in 0..6 {
            more[b][c] = [600 + 60 * b as u32, 80 + 20 * c as u32];
            for t in 0..11 {
                token[b][c][t] = 120 + 260 * t as u32 + 30 * b as u32;
            }
        }
    }
    (more, token)
}

fn block(rng: &mut Rng, n: usize, amp: i32, smooth: bool) -> (Vec<u8>, Vec<u8>) {
    let mut src = vec![0u8; n * n];
    let mut pred = vec![0u8; n * n];
    let base = rng.range(amp, 255 - amp);
    for i in 0..n {
        for j in 0..n {
            let s = if smooth {
                base + ((i * 7 + j * 3) as i32 % (2 * amp + 1)) - amp
            } else {
                base + rng.range(-amp, amp)
            };
            src[i * n + j] = s.clamp(0, 255) as u8;
            pred[i * n + j] = (base + rng.range(-amp / 2, amp / 2)).clamp(0, 255) as u8;
        }
    }
    (src, pred)
}

#[allow(clippy::too_many_arguments)]
fn run<I: Isa>(
    src: &[u8],
    pred: &[u8],
    n: usize,
    tx_size: u8,
    tx_type: u8,
    q: Quant,
    lambda: u32,
    lossless: bool,
) -> (Vec<u8>, Out, [i16; 1024]) {
    let (more, token) = costs();
    let tc = TokenCosts {
        more: &more,
        token: &token,
    };
    let mut dst = pred.to_vec();
    let mut levels = [0i16; 1024];
    let b = Block {
        src,
        src_stride: n,
        dst: &mut dst,
        dst_stride: n,
        tx_size,
        tx_type,
        lossless,
        ctx: 1,
        is_inter: false,
        chroma: false,
    };
    let out = encode_with::<I>(b, q, &tc, lambda, &mut levels);
    (dst, out, levels)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let sse: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (x as f64 - y as f64).powi(2))
        .sum();
    if sse == 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 * a.len() as f64 / sse).log10()
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn avx2_matches_scalar() {
    if !avx2() {
        return;
    }
    let mut rng = Rng(0x9e3779b97f4a7c15);
    for tx_size in 0..4u8 {
        let n = 4usize << tx_size;
        for tx_type in 0..4u8 {
            for (k, &(qv, amp, lambda)) in [
                (8, 255, 0),
                (40, 60, 0),
                (300, 120, 0),
                (60, 90, 400),
                (1500, 255, 2000),
            ]
            .iter()
            .enumerate()
            {
                let (src, pred) = block(&mut rng, n, amp, k % 2 == 0);
                let q = Quant {
                    dc: qv,
                    ac: qv + qv / 4,
                };
                let s = run::<Scalar>(&src, &pred, n, tx_size, tx_type, q, lambda, false);
                let v = unsafe { run_avx2(&src, &pred, n, tx_size, tx_type, q, lambda) };
                assert_eq!(s.1, v.1, "out tx {tx_size} type {tx_type} q {qv}");
                assert_eq!(
                    s.2[..s.1.eob],
                    v.2[..v.1.eob],
                    "levels tx {tx_size} type {tx_type} q {qv}"
                );
                assert_eq!(s.0, v.0, "recon tx {tx_size} type {tx_type} q {qv}");
                let mut dst = pred.clone();
                reconstruct(&s.2, s.1.eob, tx_size, tx_type, false, q, &mut dst, n);
                assert_eq!(dst, s.0, "reconstruct tx {tx_size} type {tx_type} q {qv}");
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn run_avx2(
    src: &[u8],
    pred: &[u8],
    n: usize,
    tx_size: u8,
    tx_type: u8,
    q: Quant,
    lambda: u32,
) -> (Vec<u8>, Out, [i16; 1024]) {
    run::<avx2::Avx2>(src, pred, n, tx_size, tx_type, q, lambda, false)
}

#[test]
fn dc_shortcut_matches_full_inverse() {
    for tx_size in 0..4u8 {
        let n = 4usize << tx_size;
        for dc in [-4000i16, -517, -9, 7, 333, 2900] {
            let mut a = vec![128u8; n * n];
            let mut b = a.clone();
            xform::dc_add(dc as i32, tx_size, &mut a, n);
            let mut buf = [0i16; 1024];
            buf[0] = dc;
            xform::inverse_add::<Scalar>(&mut buf, tx_size, 0, 1, &mut b, n);
            assert_eq!(a, b, "tx {tx_size} dc {dc}");
        }
    }
}

#[test]
fn lossless_is_exact() {
    let mut rng = Rng(7);
    for _ in 0..50 {
        let (src, pred) = block(&mut rng, 4, 255, false);
        let (rec, out, levels) =
            run::<Scalar>(&src, &pred, 4, 0, 0, Quant { dc: 4, ac: 4 }, 0, true);
        assert_eq!(rec, src);
        let mut again = pred.clone();
        reconstruct(
            &levels,
            out.eob,
            0,
            0,
            true,
            Quant { dc: 4, ac: 4 },
            &mut again,
            4,
        );
        assert_eq!(again, src);
    }
}

#[test]
fn fine_quantiser_round_trips() {
    let mut rng = Rng(11);
    for tx_size in 0..4u8 {
        let n = 4usize << tx_size;
        for tx_type in 0..4u8 {
            let mut worst = 99.0f64;
            for k in 0..8 {
                let (src, pred) = block(&mut rng, n, 120, k % 2 == 0);
                let (rec, out, _) = run::<Scalar>(
                    &src,
                    &pred,
                    n,
                    tx_size,
                    tx_type,
                    Quant { dc: 4, ac: 4 },
                    0,
                    false,
                );
                assert!(out.eob > 0);
                worst = worst.min(psnr(&src, &rec));
            }
            assert!(worst > 50.0, "tx {tx_size} type {tx_type}: {worst:.1} dB");
        }
    }
}

#[test]
fn trellis_reports_its_rate_and_beats_plain() {
    let (more, token) = costs();
    let tc = TokenCosts {
        more: &more,
        token: &token,
    };
    let mut rng = Rng(23);
    let mut jt = 0u64;
    let mut jp = 0u64;
    let lambda = 300u32;
    for tx_size in 0..4u8 {
        let n = 4usize << tx_size;
        for k in 0..12 {
            let (src, pred) = block(&mut rng, n, 40, k % 3 == 0);
            let q = Quant { dc: 48, ac: 60 };
            let (_, t, lt) = run::<Scalar>(&src, &pred, n, tx_size, 0, q, lambda, false);
            let (_, p, _) = run::<Scalar>(&src, &pred, n, tx_size, 0, q, 0, false);
            let s = scan_for(tx_size, 0);
            assert_eq!(t.rate, quant::rate::<1024>(&lt, t.eob, &s, 1, &tc));
            jt += (t.dist << 8) + lambda as u64 * t.rate as u64;
            jp += (p.dist << 8) + lambda as u64 * p.rate as u64;
        }
    }
    assert!(jt < jp, "trellis {jt} plain {jp}");
}

#[test]
#[ignore]
fn speed() {
    let (more, token) = costs();
    let tc = TokenCosts {
        more: &more,
        token: &token,
    };
    let mut rng = Rng(5);
    let w = 256usize;
    let noise: i32 = std::env::var("NOISE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    let src: Vec<u8> = (0..w * w)
        .map(|i| {
            ((i % w) as i32 / 3 + (i / w) as i32 / 2 + rng.range(-noise, noise)).clamp(0, 255) as u8
        })
        .collect();
    let pred: Vec<u8> = (0..w * w)
        .map(|i| ((i % w) as i32 / 3 + (i / w) as i32 / 2 + rng.range(-3, 3)).clamp(0, 255) as u8)
        .collect();
    for simd in [true, false] {
        for tx_size in 0..4u8 {
            let n = 4usize << tx_size;
            for (lambda, tx_type) in [(0u32, 0u8), (0, 3), (400, 0)] {
                if tx_size == 3 && tx_type != 0 {
                    continue;
                }
                let mut dst = pred.clone();
                let mut levels = [0i16; 1024];
                let q = Quant { dc: 40, ac: 48 };
                let mut px = 0usize;
                let mut nz = 0usize;
                let t = std::time::Instant::now();
                while t.elapsed().as_millis() < 300 {
                    for by in (0..w).step_by(n) {
                        for bx in (0..w).step_by(n) {
                            let o = by * w + bx;
                            dst[o..]
                                .iter_mut()
                                .zip(&pred[o..])
                                .take(n)
                                .for_each(|(d, &p)| *d = p);
                            let b = Block {
                                src: &src[o..],
                                src_stride: w,
                                dst: &mut dst[o..],
                                dst_stride: w,
                                tx_size,
                                tx_type,
                                lossless: false,
                                ctx: 0,
                                is_inter: true,
                                chroma: false,
                            };
                            let out = if simd {
                                encode(b, q, &tc, lambda, &mut levels)
                            } else {
                                encode_with::<Scalar>(b, q, &tc, lambda, &mut levels)
                            };
                            nz += out.eob;
                            px += n * n;
                        }
                    }
                }
                let s = t.elapsed().as_secs_f64();
                println!(
                    "{} {n:2}x{n:<2} type {tx_type} lambda {lambda:3}: {:6.0} ns/call {:7.1} Mpx/s  {:6.0} fps at 1080p  (eob avg {:.1})",
                    if simd { "avx2  " } else { "scalar" },
                    s * 1e9 / (px / (n * n)) as f64,
                    px as f64 / s / 1e6,
                    px as f64 / s / (1920.0 * 1080.0 * 1.5),
                    nz as f64 * (n * n) as f64 / px as f64
                );
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
#[ignore]
fn stages() {
    unsafe { stages_avx2() }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn stages_avx2() {
    use super::avx2::Avx2;
    let (more, token) = costs();
    let tc = TokenCosts {
        more: &more,
        token: &token,
    };
    let mut rng = Rng(3);
    for tx_size in [1u8, 2, 3] {
        let n = 4usize << tx_size;
        let n2 = n * n;
        let res: Vec<i16> = (0..n2).map(|_| rng.range(-20, 20) as i16).collect();
        let s = scan_for(tx_size, 0);
        let q = Quant { dc: 40, ac: 48 };
        let iters = 200_000 / n2 * 64;
        let mut buf = [0i16; 1024];
        let t = std::time::Instant::now();
        for _ in 0..iters {
            buf[..n2].copy_from_slice(&res);
            xform::forward::<Avx2>(std::hint::black_box(&mut buf), tx_size, 0);
        }
        let fwd = t.elapsed().as_secs_f64() / iters as f64;
        let t = std::time::Instant::now();
        for _ in 0..iters {
            buf[..n2].copy_from_slice(&res);
            xform::forward::<Scalar>(std::hint::black_box(&mut buf), tx_size, 0);
        }
        println!(
            "scalar fwd {:.0} ns",
            t.elapsed().as_secs_f64() / iters as f64 * 1e9
        );
        let coefs = buf;
        let mut dq = [0i16; 1024];
        let mut levels = [0i16; 1024];
        let t = std::time::Instant::now();
        let mut eob = 0;
        for _ in 0..iters {
            eob = if tx_size == 1 {
                quant::quantise::<64>(
                    std::hint::black_box(&coefs),
                    &mut dq,
                    &s,
                    q,
                    22,
                    &mut levels,
                )
                .0
            } else if tx_size == 2 {
                quant::quantise::<256>(
                    std::hint::black_box(&coefs),
                    &mut dq,
                    &s,
                    q,
                    22,
                    &mut levels,
                )
                .0
            } else {
                quant::quantise::<1024>(
                    std::hint::black_box(&coefs),
                    &mut dq,
                    &s,
                    q,
                    22,
                    &mut levels,
                )
                .0
            };
        }
        let qt = t.elapsed().as_secs_f64() / iters as f64;
        let t = std::time::Instant::now();
        for _ in 0..iters {
            std::hint::black_box(quant::rate::<1024>(
                std::hint::black_box(&levels),
                eob,
                &s,
                0,
                &tc,
            ));
        }
        let rt = t.elapsed().as_secs_f64() / iters as f64;
        let t = std::time::Instant::now();
        for _ in 0..iters {
            std::hint::black_box(if tx_size == 1 {
                quant::trellis::<64>(
                    std::hint::black_box(&coefs),
                    &s,
                    q,
                    0,
                    &tc,
                    400,
                    &mut levels,
                )
            } else if tx_size == 2 {
                quant::trellis::<256>(
                    std::hint::black_box(&coefs),
                    &s,
                    q,
                    0,
                    &tc,
                    400,
                    &mut levels,
                )
            } else {
                quant::trellis::<1024>(
                    std::hint::black_box(&coefs),
                    &s,
                    q,
                    0,
                    &tc,
                    400,
                    &mut levels,
                )
            });
        }
        let tt = t.elapsed().as_secs_f64() / iters as f64;
        let mut dst = vec![128u8; n2];
        let t = std::time::Instant::now();
        for _ in 0..iters {
            let mut b = dq;
            xform::inverse_add::<Avx2>(std::hint::black_box(&mut b), tx_size, 0, n, &mut dst, n);
        }
        let it = t.elapsed().as_secs_f64() / iters as f64;
        println!(
            "{n}x{n}: fwd {:.0} ns, quant {:.0} ns, rate {:.0} ns (eob {eob}), trellis {:.0} ns, inv {:.0} ns",
            fwd * 1e9,
            qt * 1e9,
            rt * 1e9,
            tt * 1e9,
            it * 1e9
        );
    }
}
