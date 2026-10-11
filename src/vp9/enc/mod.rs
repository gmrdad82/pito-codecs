pub mod pred;

pub mod rc;

mod bitstream;
mod bool;
mod entropy;
mod frame;
mod mvref;
mod search;
mod tables;
pub mod tx;

#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use bitstream::{CompressedParams, SegParams, Uncompressed, write_compressed, write_uncompressed};
use bool::BoolWriter;
use entropy::{Costs, FrameSyntax, NullSink, TileWriter};
use frame::{MiGrid, MiInfo, Picture, SbCoefs};
use rc::{Aq, FrameKind, RateControl, RcConfig, RcMode, TileLayout, Wavefront};
use search::{FrameEnv, SbEncoder, Shared, Speed, Workspace};
use tables::*;
use tx::Quant;

use crate::media::{Error, Frame, Packet, PixelFormat, Result};
use crate::vp9::header::{LoopFilterParams, Segmentation};
use crate::vp9::loopfilter::{LfFrame, PlaneView, filter_superblock};
use crate::vp9::probs::{Counts, FrameContext, adapt_coef_probs, adapt_noncoef_probs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rate {
    Crf(u8),
    Bitrate(u32),
    Q(u8),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub rate: Rate,
    pub gop: u32,
    pub threads: usize,
    pub aq: bool,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        EncoderConfig {
            width: 1920,
            height: 1080,
            fps: (30, 1),
            rate: Rate::Crf(32),
            gop: 240,
            threads: 0,
            aq: false,
        }
    }
}

pub struct Encoder {
    cfg: EncoderConfig,
    index: u64,
    contexts: [FrameContext; 4],
    slots: [Option<Arc<Picture>>; 8],
    prev_mi: Option<MiGrid>,
    last_key: bool,
    mi_rows: usize,
    mi_cols: usize,
    sb_cols: usize,
    coefs: Vec<Mutex<SbCoefs>>,
    rc: RateControl,
    wavefront: Wavefront,
    workers: Vec<Workspace>,
    recon_out: Option<Arc<Picture>>,
    broken: bool,
}

fn quant(qindex: u8) -> [Quant; 2] {
    let q = qindex as usize;
    let y = Quant {
        dc: DC_QLOOKUP[q],
        ac: AC_QLOOKUP[q],
    };
    [y, y]
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Result<Encoder> {
        if cfg.width == 0 || cfg.height == 0 || cfg.width > 16384 || cfg.height > 16384 {
            return Err(Error::Invalid(format!(
                "vp9 encoder: invalid size {}x{}",
                cfg.width, cfg.height
            )));
        }
        let mi_cols = cfg.width.div_ceil(8) as usize;
        let mi_rows = cfg.height.div_ceil(8) as usize;
        let sb_cols = mi_cols.div_ceil(8);
        let sb_rows = mi_rows.div_ceil(8);
        let threads = match cfg.threads {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
            n => n,
        };
        let mode = match cfg.rate {
            Rate::Crf(c) => RcMode::Crf { crf: c.min(63) },
            Rate::Q(q) => RcMode::ConstantQ { q_index: q },
            Rate::Bitrate(kbps) => RcMode::Bitrate {
                kbps,
                buffer_ms: 1000,
            },
        };
        let mut rcc = RcConfig::new(mode, cfg.width as usize, cfg.height as usize);
        rcc.fps_num = cfg.fps.0;
        rcc.fps_den = cfg.fps.1;
        rcc.keyint = cfg.gop;
        rcc.aq = if cfg.aq { Aq::Variance } else { Aq::Off };
        let layout = TileLayout::new(mi_cols, mi_rows, threads);
        let workers = (0..threads.min(layout.cols.len() * sb_rows).max(1))
            .map(|_| Workspace::default())
            .collect();
        let fc = FrameContext::default();
        Ok(Encoder {
            cfg,
            index: 0,
            contexts: [fc.clone(), fc.clone(), fc.clone(), fc],
            slots: Default::default(),
            prev_mi: None,
            last_key: true,
            mi_rows,
            mi_cols,
            sb_cols,
            coefs: (0..sb_rows * sb_cols)
                .map(|_| Mutex::new(SbCoefs::default()))
                .collect(),
            rc: RateControl::new(rcc),
            wavefront: Wavefront::new(layout),
            workers,
            recon_out: None,
            broken: false,
        })
    }

    pub fn encode(&mut self, frame: &Frame) -> Result<Vec<Packet>> {
        if frame.format != PixelFormat::Yuv420p8 {
            return Err(Error::Unsupported(format!(
                "vp9 encoder: {:?} input, 8-bit 4:2:0 only",
                frame.format
            )));
        }
        if (frame.width, frame.height) != (self.cfg.width, self.cfg.height) {
            return Err(Error::Invalid(format!(
                "vp9 encoder: {}x{} frame for a {}x{} stream",
                frame.width, frame.height, self.cfg.width, self.cfg.height
            )));
        }
        if self.broken {
            return Err(Error::Backend(
                "vp9 encoder: an earlier frame failed, start a new encoder".into(),
            ));
        }
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        for (p, fp) in frame.planes.iter().enumerate() {
            let (pw, ph) = if p == 0 {
                (w, h)
            } else {
                (w.div_ceil(2), h.div_ceil(2))
            };
            if fp.stride < pw || fp.data.len() < fp.stride * (ph - 1) + pw {
                return Err(Error::Invalid(format!(
                    "vp9 encoder: plane {p} holds {} bytes at stride {} for {pw}x{ph}",
                    fp.data.len(),
                    fp.stride
                )));
            }
        }
        let mut src = Picture::new(w, h);
        for p in 0..3 {
            let (pw, ph) = src.visible(p);
            let fp = &frame.planes[p];
            for y in 0..ph {
                let o = src.idx(p, 0, y);
                src.planes[p][o..o + pw]
                    .copy_from_slice(&fp.data[y * fp.stride..y * fp.stride + pw]);
            }
        }
        src.extend_borders();
        let key = self.index.is_multiple_of(self.cfg.gop.max(1) as u64);
        let data = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.encode_picture(&src, key)
        })) {
            Ok(d) => d,
            Err(e) => {
                self.broken = true;
                let why = e
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| e.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                return Err(Error::Backend(format!(
                    "vp9 encoder: frame {} failed: {why}",
                    self.index
                )));
            }
        };
        let pts = frame.pts;
        self.index += 1;
        Ok(vec![Packet {
            data,
            pts,
            dts: pts,
            keyframe: key,
        }])
    }

    pub fn flush(&mut self) -> Result<Vec<Packet>> {
        Ok(Vec::new())
    }

    #[cfg(test)]
    fn recon(&self) -> Option<&Picture> {
        self.recon_out.as_deref()
    }

    fn encode_picture(&mut self, src: &Picture, key: bool) -> Vec<u8> {
        let (mi_rows, mi_cols) = (self.mi_rows, self.mi_cols);
        if key {
            let fc = FrameContext::default();
            self.contexts = [fc.clone(), fc.clone(), fc.clone(), fc];
        }
        let kind = if key {
            FrameKind::Key
        } else {
            FrameKind::Inter
        };
        let analysis = self
            .rc
            .analyse(&src.planes[0][src.origin[0]..], src.stride[0]);
        let plan = self.rc.plan(kind, &analysis);
        let base_q_idx = plan.base_q_idx.max(1);
        let rseg = self.rc.segmentation();
        let seg_on = rseg.enabled && base_q_idx > 0;
        let mut seg_q = [quant(base_q_idx); 8];
        let mut seg_lambda = [plan.lambda; 8];
        if seg_on {
            for s in 0..8 {
                let qi = rseg.q_index(base_q_idx, s as u8).max(1);
                seg_q[s] = quant(qi);
                seg_lambda[s] = rc::lambda(qi);
            }
        }
        let mut seg = Segmentation::default();
        if seg_on {
            seg.enabled = true;
            seg.update_map = true;
            seg.tree_probs = rseg.tree_probs;
            seg.pred_probs = [255; 3];
            seg.abs_or_delta_update = rseg.abs_delta;
            for s in 0..8 {
                seg.feature_enabled[s][0] = rseg.q[s] != 0;
                seg.feature_data[s][0] = rseg.q[s];
                seg.feature_enabled[s][1] = rseg.lf[s] != 0;
                seg.feature_data[s][1] = rseg.lf[s] as i16;
            }
        }
        let seg_map = seg_on.then(|| rseg.map.clone());
        let tx_mode = TX_MODE_SELECT;
        let allow_hp = false;
        let interp_filter = 0u8;
        let fc = self.contexts[0].clone();
        let costs = Costs::new(&fc, key);
        let mv_cost = pred::mvcost::MvCost::new(pred::mvcost::MvProbs {
            joints: fc.mv_joint,
            sign: fc.mv_sign,
            classes: fc.mv_class,
            class0: fc.mv_class0_bit,
            bits: fc.mv_bits,
            class0_fr: fc.mv_class0_fr,
            fr: fc.mv_fr,
            class0_hp: fc.mv_class0_hp,
            hp: fc.mv_hp,
        });
        let speed = Speed {
            intra_modes: if key {
                &pred::mode::ALL_INTRA
            } else {
                &pred::mode::FAST_INTRA
            },
            subpel: pred::motion::Subpel::Quarter,
            range: 32,
            intra_margin: 110,
            tx_depth: 1,
            trellis: true,
        };
        let mut recon = Picture::new(src.width, src.height);
        let mut mi = MiGrid {
            cols: mi_cols,
            rows: mi_rows,
            mi: vec![MiInfo::default(); mi_rows * mi_cols],
        };
        let mut uv = vec![0u8; mi_rows * mi_cols];
        let layout = self.wavefront.layout().clone();
        let refs: [Option<&Picture>; 3] = if key {
            [None, None, None]
        } else {
            [
                self.slots[0].as_deref(),
                self.slots[1].as_deref(),
                self.slots[2].as_deref(),
            ]
        };
        let prev_mi = if key {
            None
        } else {
            self.prev_mi.as_ref().map(|m| &m.mi[..])
        };
        {
            let env = FrameEnv {
                src,
                refs,
                key,
                mi_rows,
                mi_cols,
                q: seg_q,
                lambda: seg_lambda,
                seg_map: seg_map.as_deref(),
                tx_mode,
                allow_hp,
                costs: &costs,
                mv_cost: &mv_cost,
                speed: &speed,
                prev_mi,
                sign_bias: [false; 4],
                filter: interp_filter,
            };
            let shared = Shared::new(&mut recon, &mut mi.mi, &mut uv, mi_cols);
            let coefs = &self.coefs;
            let sb_cols = self.sb_cols;
            self.wavefront.run(&mut self.workers, |ws, row| {
                let tile = layout.cols[row.tile_col];
                for sb_col in row.sbs() {
                    let mut co = coefs[row.sb_row * sb_cols + sb_col]
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    SbEncoder::encode(&env, &shared, ws, &mut co, tile, row.sb_row, sb_col);
                }
            });
        }
        let lf_level = plan.filter_level;
        loop_filter(&mut recon, &mi, lf_level, &seg);
        let coefs: Vec<SbCoefs> = self
            .coefs
            .iter_mut()
            .map(|m| std::mem::take(m.get_mut().unwrap_or_else(|e| e.into_inner())))
            .collect();
        let syntax = FrameSyntax {
            intra: key,
            lossless: false,
            tx_mode,
            interp_filter,
            allow_hp,
            sign_bias: [false; 4],
            seg: &seg,
            mi: &mi,
            uv: &uv,
            prev_mi,
            coefs: &coefs,
            sb_cols: self.sb_cols,
            fc: &fc,
        };
        let counts = {
            let parts: Vec<Counts> = std::thread::scope(|s| {
                let hs: Vec<_> = layout
                    .cols
                    .iter()
                    .map(|&(c0, c1)| {
                        let syntax = &syntax;
                        let rows = &layout.rows;
                        s.spawn(move || {
                            let mut tw = TileWriter::new(syntax, NullSink, c0, c1);
                            for &(r0, r1) in rows {
                                tw.tile(r0, r1);
                            }
                            tw.counts
                        })
                    })
                    .collect();
                hs.into_iter()
                    .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
                    .collect()
            });
            let mut c = Counts::default();
            for p in &parts {
                c.add(p);
            }
            c
        };
        let mut fc_upd = fc.clone();
        let compressed = write_compressed(
            &CompressedParams {
                intra: key,
                lossless: false,
                tx_mode,
                interp_filter,
                allow_hp,
            },
            &mut fc_upd,
            &counts,
        );
        let syntax = FrameSyntax {
            fc: &fc_upd,
            ..syntax
        };
        let tiles: Vec<Vec<Vec<u8>>> = std::thread::scope(|s| {
            let hs: Vec<_> = layout
                .cols
                .iter()
                .map(|&(c0, c1)| {
                    let syntax = &syntax;
                    let rows = &layout.rows;
                    s.spawn(move || {
                        let mut tw = TileWriter::new(syntax, BoolWriter::new(Vec::new()), c0, c1);
                        let mut out = Vec::new();
                        for &(r0, r1) in rows {
                            tw.tile(r0, r1);
                            let w = std::mem::replace(&mut tw.w, BoolWriter::new(Vec::new()));
                            out.push(w.finish());
                        }
                        out
                    })
                })
                .collect();
            hs.into_iter()
                .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
                .collect()
        });
        let hdr = Uncompressed {
            key,
            width: src.width as u32,
            height: src.height as u32,
            color_space: 2,
            full_range: false,
            refresh_frame_flags: if key { 0xff } else { 0x01 },
            ref_frame_idx: [0, 1, 2],
            allow_hp,
            interp_filter,
            refresh_frame_context: true,
            frame_parallel: false,
            frame_context_idx: 0,
            lf_level,
            sharpness: plan.sharpness,
            base_q_idx,
            seg: seg_on.then(|| SegParams {
                tree_probs: seg.tree_probs,
                abs_delta: seg.abs_or_delta_update,
                q: rseg_q(&seg),
                lf: rseg_lf(&seg),
            }),
            tile_cols_log2: layout.log2_cols as u32,
            tile_rows_log2: layout.log2_rows as u32,
        };
        let mut out = write_uncompressed(&hdr, compressed.len());
        out.extend_from_slice(&compressed);
        let (nr, nc) = (layout.rows.len(), layout.cols.len());
        for r in 0..nr {
            for c in 0..nc {
                let t = &tiles[c][r];
                if r + 1 < nr || c + 1 < nc {
                    out.extend_from_slice(&(t.len() as u32).to_be_bytes());
                }
                out.extend_from_slice(t);
            }
        }
        for (m, c) in self.coefs.iter_mut().zip(coefs) {
            *m.get_mut().unwrap_or_else(|e| e.into_inner()) = c;
        }
        let saved = &self.contexts[0];
        let mut a = saved.clone();
        a.tx = fc_upd.tx;
        a.skip = fc_upd.skip;
        adapt_coef_probs(&mut a, &counts, key, self.last_key);
        if !key {
            a.tx = saved.tx;
            a.skip = saved.skip;
            adapt_noncoef_probs(
                &mut a,
                &counts,
                interp_filter == SWITCHABLE,
                tx_mode == TX_MODE_SELECT,
                allow_hp,
            );
        }
        self.contexts[0] = a;
        self.last_key = key;
        self.rc.update(kind, out.len() as u64 * 8, base_q_idx);
        recon.extend_borders();
        let recon = Arc::new(recon);
        let flags = hdr.refresh_frame_flags;
        for i in 0..8 {
            if (flags >> i) & 1 == 1 {
                self.slots[i] = Some(recon.clone());
            }
        }
        self.recon_out = Some(recon);
        self.prev_mi = Some(mi);
        out
    }
}

fn rseg_q(seg: &Segmentation) -> [Option<i16>; 8] {
    std::array::from_fn(|s| seg.feature_enabled[s][0].then_some(seg.feature_data[s][0]))
}

fn rseg_lf(seg: &Segmentation) -> [Option<i16>; 8] {
    std::array::from_fn(|s| seg.feature_enabled[s][1].then_some(seg.feature_data[s][1]))
}

fn loop_filter(recon: &mut Picture, mi: &MiGrid, level: u8, seg: &Segmentation) {
    if level == 0 {
        return;
    }
    let lf = LoopFilterParams {
        level,
        sharpness: 0,
        delta_enabled: true,
        ref_deltas: [1, 0, -1, -1],
        mode_deltas: [0, 0],
    };
    let f = LfFrame::new(&lf, seg, mi.rows, mi.cols, true, true, 8);
    let mut bufs: Vec<Vec<u16>> = (0..3)
        .map(|p| {
            let (aw, ah) = recon.aligned[p];
            let mut v = vec![0u16; aw * ah];
            for y in 0..ah {
                let o = recon.idx(p, 0, y);
                for (d, s) in v[y * aw..(y + 1) * aw]
                    .iter_mut()
                    .zip(&recon.planes[p][o..o + aw])
                {
                    *d = *s as u16;
                }
            }
            v
        })
        .collect();
    {
        let [a, b, c] = &mut bufs[..] else {
            return;
        };
        let mut views = [
            PlaneView {
                data: a,
                stride: recon.aligned[0].0,
                ox: 0,
                oy: 0,
            },
            PlaneView {
                data: b,
                stride: recon.aligned[1].0,
                ox: 0,
                oy: 0,
            },
            PlaneView {
                data: c,
                stride: recon.aligned[2].0,
                ox: 0,
                oy: 0,
            },
        ];
        for r in 0..mi.rows.div_ceil(8) {
            for c in 0..mi.cols.div_ceil(8) {
                filter_superblock(&mut views, mi, &f, r * 8, c * 8);
            }
        }
    }
    for p in 0..3 {
        let (aw, ah) = recon.aligned[p];
        for y in 0..ah {
            let o = recon.idx(p, 0, y);
            for (d, s) in recon.planes[p][o..o + aw]
                .iter_mut()
                .zip(&bufs[p][y * aw..(y + 1) * aw])
            {
                *d = *s as u8;
            }
        }
    }
}
