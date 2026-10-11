use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn texture(w: usize, h: usize, seed: u32) -> Vec<u8> {
    let mut s = seed.wrapping_mul(2654435761) | 1;
    let mut noise = vec![0u8; w * h];
    for v in noise.iter_mut() {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        *v = (s >> 24) as u8;
    }
    let mut f: Vec<i32> = noise.iter().map(|&v| v as i32).collect();
    for _ in 0..12 {
        let g = f.clone();
        for y in 0..h {
            for x in 0..w {
                let at = |xx: usize, yy: usize| g[(yy % h) * w + xx % w];
                f[y * w + x] = (at(x + w - 1, y) + 2 * at(x, y) + at(x + 1, y)) / 4;
            }
        }
        let g = f.clone();
        for y in 0..h {
            for x in 0..w {
                let at = |xx: usize, yy: usize| g[(yy % h) * w + xx % w];
                f[y * w + x] = (at(x, y + h - 1) + 2 * at(x, y) + at(x, y + 1)) / 4;
            }
        }
    }
    let out: Vec<u8> = f
        .iter()
        .map(|&v| ((v - 128) * 10 + 128).clamp(0, 255) as u8)
        .collect();
    out
}

fn shifted(src: &[u8], w: usize, h: usize, dx: usize, dy: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = src[((y + dy) % h) * w + (x + dx) % w];
        }
    }
    out
}

#[test]
fn wavefront_keeps_the_row_above_two_superblocks_ahead() {
    let (mi_cols, mi_rows) = (1920usize.div_ceil(8), 1080usize.div_ceil(8));
    let layout = TileLayout::new(mi_cols, mi_rows, 16);
    assert_eq!(layout.cols.len(), 4);
    assert_eq!(layout.cols.last().unwrap().1, mi_cols);
    let (sbc, sbr) = (layout.sb_cols(), layout.sb_rows());
    let wf = Wavefront::new(layout);
    let clock = AtomicUsize::new(1);
    let stamps: Vec<AtomicUsize> = (0..sbc * sbr).map(|_| AtomicUsize::new(0)).collect();
    let bad = AtomicUsize::new(0);
    let mut workers = vec![(); 12];
    for _ in 0..3 {
        for s in &stamps {
            s.store(0, Ordering::Relaxed);
        }
        wf.run(&mut workers, |_, row| {
            for c in row.sbs() {
                if row.sb_row > 0 {
                    for dc in c..(c + LAG).min(row.sb_end) {
                        if stamps[(row.sb_row - 1) * sbc + dc].load(Ordering::Acquire) == 0 {
                            bad.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                let t = clock.fetch_add(1, Ordering::Relaxed);
                assert_eq!(stamps[row.sb_row * sbc + c].swap(t, Ordering::AcqRel), 0);
                std::hint::black_box((0..200).sum::<u64>());
            }
        });
        assert!(stamps.iter().all(|s| s.load(Ordering::Relaxed) > 0));
    }
    assert_eq!(bad.load(Ordering::Relaxed), 0);
}

#[test]
fn analysis_tracks_motion_and_spots_a_cut() {
    let (w, h) = (320, 192);
    let a = texture(w, h, 7);
    let b = shifted(&a, w, h, 6, 4);
    let c = texture(w, h, 99);
    let mut an = Analyser::new(w, h);
    let first = an.run(&a, w);
    assert!(!first.has_prev && first.intra_cost > 0);
    let moved = an.run(&b, w);
    assert!(moved.inter_ratio() < 0.25, "ratio {}", moved.inter_ratio());
    assert!(!moved.scene_cut());
    let cut = an.run(&c, w);
    assert!(cut.scene_cut(), "ratio {}", cut.inter_ratio());
}

fn synthetic(rc: &mut RateControl, frames: usize, keyint: usize, stats: &[FrameAnalysis]) -> u64 {
    let mut total = 0u64;
    for i in 0..frames {
        let kind = if i % keyint == 0 {
            FrameKind::Key
        } else {
            FrameKind::Inter
        };
        let a = stats[i];
        let p = rc.plan(kind, &a);
        let k = if kind == FrameKind::Key { 3.1 } else { 1.7 };
        let wobble = 1.0 + 0.15 * ((i as f64) * 0.7).sin();
        let bits = (k * a.cost(kind) as f64 / qstep(p.base_q_idx).powf(1.08) * wobble) as u64;
        rc.update(kind, bits, p.base_q_idx);
        total += bits;
    }
    total
}

fn fake_stats(frames: usize) -> Vec<FrameAnalysis> {
    (0..frames)
        .map(|i| {
            let intra = 4_000_000 + 1_500_000 * ((i / 90) % 3) as u64;
            let motion = if (i / 45) % 2 == 0 { 0.08 } else { 0.35 };
            FrameAnalysis {
                intra_cost: intra,
                inter_cost: (intra as f64 * motion) as u64,
                has_prev: i > 0,
                blocks: 8160,
            }
        })
        .collect()
}

#[test]
fn bitrate_modes_land_near_the_target() {
    let frames = 600;
    let stats = fake_stats(frames);
    for kbps in [800u32, 3000, 9000] {
        let wanted = kbps as f64 * 1000.0 * frames as f64 / 30.0;
        let mut cfg = RcConfig::new(
            RcMode::Bitrate {
                kbps,
                buffer_ms: 2000,
            },
            1920,
            1080,
        );
        cfg.keyint = 120;
        cfg.aq = Aq::Off;
        let got = synthetic(&mut RateControl::new(cfg.clone()), frames, 120, &stats) as f64;
        assert!(
            (got / wanted - 1.0).abs() < 0.1,
            "1-pass {kbps}: {}",
            got / wanted
        );
        cfg.mode = RcMode::TwoPass {
            kbps,
            stats: stats.clone(),
        };
        let got = synthetic(&mut RateControl::new(cfg), frames, 120, &stats) as f64;
        assert!(
            (got / wanted - 1.0).abs() < 0.05,
            "2-pass {kbps}: {}",
            got / wanted
        );
    }
}

#[test]
fn variance_aq_gives_flat_areas_the_finer_quantiser() {
    let (w, h) = (256, 128);
    let tex = texture(w, h, 3);
    let mut img = vec![128u8; w * h];
    for y in 0..h {
        for x in w / 2..w {
            img[y * w + x] = tex[y * w + x];
        }
    }
    let mut rc = RateControl::new(RcConfig::new(RcMode::Crf { crf: 30 }, w, h));
    let a = rc.analyse(&img, w);
    let p = rc.plan(FrameKind::Key, &a);
    let seg = rc.segmentation();
    assert!(seg.enabled);
    let flat = seg.q_index(p.base_q_idx, seg.segment(4, 2));
    let busy = seg.q_index(p.base_q_idx, seg.segment(4, 28));
    assert!(
        flat < p.base_q_idx && busy > p.base_q_idx,
        "{flat} {} {busy}",
        p.base_q_idx
    );
}
