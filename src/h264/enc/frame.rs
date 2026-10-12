use std::sync::Arc;

use crate::h264::dec::cabac_tables::CABAC_INIT_MN;
use crate::h264::enc::cabac::CostTable;
use crate::h264::enc::deblock::DeblockParams;
use crate::h264::enc::nal::{SliceHeader, SliceType};
use crate::h264::enc::picture::{
    CHROMA_PAD, DONE, Frame, LUMA_PAD, Pic, copy_row, extend_rows, hpel_rows,
};
use crate::h264::enc::slice::{EncParams, FrameEnc, Lambdas, SliceEnc, SliceOut};
use crate::h264::enc::transform::QuantTables;

const VMAX: usize = 96;

pub struct Ctx {
    pub p: EncParams,
    pub qt: QuantTables,
    pub cost_tab: CostTable,
    pub lam: Lambdas,
}

pub struct Job {
    pub seq: u64,
    pub src: Frame,
    pub pts: i64,
    pub st: SliceType,
    pub idr: bool,
    pub qp: u8,
    pub qpf: f64,
    pub cplx: f64,
    pub aq: Vec<i8>,
    pub hdr: SliceHeader,
    pub nal_ref_idc: u8,
    pub nal_type: u8,
    pub pic: Arc<Pic>,
    pub refs: [Option<Arc<Pic>>; 2],
    pub publish: bool,
    pub dp: DeblockParams,
}

pub struct Scratch {
    hpel: Vec<i16>,
}

impl Scratch {
    pub fn new() -> Self {
        Scratch { hpel: Vec::new() }
    }
}

struct Progress {
    yb: usize,
    cb: usize,
    yext: usize,
    cext: usize,
}

fn extend(se: &mut SliceEnc, pic: &Pic, pr: &mut Progress, yfin: usize, cfin: usize) {
    let b = se.band_mut();
    let (ys, cs) = (pic.ys, pic.cs);
    extend_rows(
        b.y,
        b.ybase,
        ys,
        LUMA_PAD,
        pic.w,
        LUMA_PAD + pr.yext,
        LUMA_PAD + yfin,
    );
    for d in [&mut *b.u, &mut *b.v] {
        extend_rows(
            d,
            b.cbase,
            cs,
            CHROMA_PAD,
            pic.w / 2,
            CHROMA_PAD + pr.cext,
            CHROMA_PAD + cfin,
        );
    }
    if pr.yb == 0 {
        copy_row(b.y, b.ybase, ys, LUMA_PAD, 0..LUMA_PAD);
        for d in [&mut *b.u, &mut *b.v] {
            copy_row(d, b.cbase, cs, CHROMA_PAD, 0..CHROMA_PAD);
        }
    }
    if yfin == pic.h {
        let last = LUMA_PAD + pic.h - 1;
        copy_row(b.y, b.ybase, ys, last, last + 1..pic.luma_rows());
        let clast = CHROMA_PAD + pic.h / 2 - 1;
        for d in [&mut *b.u, &mut *b.v] {
            copy_row(d, b.cbase, cs, clast, clast + 1..pic.chroma_rows());
        }
    }
    pr.yext = yfin;
    pr.cext = cfin;
}

fn hpel(pic: &Pic, pr: &mut Progress, upto: usize, scratch: &mut Scratch) {
    if upto <= pr.yb {
        return;
    }
    let ys = pic.ys;
    unsafe {
        let src = pic.y.head(usize::MAX);
        let out = [
            pic.hp[0].tail(pr.yb * ys),
            pic.hp[1].tail(pr.yb * ys),
            pic.hp[2].tail(pr.yb * ys),
        ];
        hpel_rows(
            src,
            ys,
            pic.luma_rows(),
            pr.yb,
            upto,
            out,
            &mut scratch.hpel,
        );
    }
    pr.yb = upto;
}

pub fn encode_frame(ctx: &Ctx, job: &Job, scratch: &mut Scratch) -> SliceOut {
    let pic = &*job.pic;
    let fe = FrameEnc {
        p: &ctx.p,
        qt: &ctx.qt,
        cost_tab: &ctx.cost_tab,
        st: job.st,
        src: &job.src,
        qp: job.qp,
        aq: &job.aq,
        hdr: job.hdr.clone(),
        nal_ref_idc: job.nal_ref_idc,
        nal_type: job.nal_type,
        cabac_table: &CABAC_INIT_MN[if job.st == SliceType::I { 3 } else { 0 }],
        lam: &ctx.lam,
    };
    let mbh = ctx.p.mbh;
    let (ys, cs) = (pic.ys, pic.cs);
    let mut se = SliceEnc::new(&fe, 0);
    se.start();
    let mut pr = Progress {
        yb: 0,
        cb: 0,
        yext: 0,
        cext: 0,
    };
    let mut mb = 0usize;
    for my in 0..mbh {
        let need = 16 * (my + 1) + VMAX;
        se.set_refs([
            job.refs[0].as_deref().map(|p| p.view(need)),
            job.refs[1].as_deref().map(|p| p.view(need)),
        ]);
        se.set_band(unsafe { pic.band(pr.yb, pr.cb, mb) }, ys, cs);
        se.encode_row(my);
        if job.publish && my >= 1 {
            se.deblock_row(my - 1, &job.dp);
            let (yfin, cfin) = (16 * my - 3, 8 * my - 1);
            extend(&mut se, pic, &mut pr, yfin, cfin);
            se.clear_band();
            hpel(pic, &mut pr, LUMA_PAD + yfin - 3, scratch);
            pr.cb = CHROMA_PAD + cfin - 1;
            mb = my - 1;
            pic.publish(pr.yb, pr.cb, mb);
        }
        se.clear_band();
    }
    if job.publish {
        se.set_band(unsafe { pic.band(pr.yb, pr.cb, mb) }, ys, cs);
        se.deblock_row(mbh - 1, &job.dp);
        extend(&mut se, pic, &mut pr, pic.h, pic.h / 2);
        se.clear_band();
        hpel(pic, &mut pr, pic.luma_rows(), scratch);
        pic.publish(DONE, DONE, DONE);
    }
    se.set_refs([None, None]);
    se.finish()
}
