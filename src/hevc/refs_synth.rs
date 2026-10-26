#[allow(dead_code, clippy::duplicate_mod)]
#[path = "../syntax/bits.rs"]
mod bits;
#[allow(dead_code, clippy::duplicate_mod)]
#[path = "../syntax/cabac.rs"]
mod cabac;
#[allow(dead_code, clippy::duplicate_mod)]
#[path = "spec_tables.rs"]
mod spec_tables;

use bits::{BitWriter, escape_rbsp};
use cabac::{NEXT_STATE, RANGE_TAB_LPS, ctx_state};
use spec_tables::*;

pub const AREA: &str = "hevc";

type Contexts = [u8; NUM_CTX.next_power_of_two()];

fn init_contexts(ctx: &mut Contexts, slice_qp: i32, init_type: usize) {
    let qp = slice_qp.clamp(0, 51);
    for (c, &v) in ctx.iter_mut().zip(CABAC_INIT[init_type].iter()) {
        let slope = (v >> 4) as i32;
        let offset = (v & 15) as i32;
        *c = ctx_state(slope * 5 - 45, (offset << 3) - 16, qp);
    }
}

pub const W: usize = 96;
pub const H: usize = 64;
pub const CTB: usize = 16;
pub const WC: usize = W / CTB;
pub const HC: usize = H / CTB;

struct Enc {
    w: BitWriter,
    low: u32,
    range: u32,
    outstanding: u32,
    first: bool,
    ctx: Contexts,
}

impl Enc {
    fn new(w: BitWriter, qp: i32, init_type: usize) -> Self {
        let mut ctx: Contexts = [0u8; NUM_CTX.next_power_of_two()];
        init_contexts(&mut ctx, qp, init_type);
        Enc {
            w,
            low: 0,
            range: 510,
            outstanding: 0,
            first: true,
            ctx,
        }
    }
    fn reset_engine(&mut self) {
        self.low = 0;
        self.range = 510;
        self.outstanding = 0;
        self.first = true;
    }
    fn put(&mut self, b: u32) {
        if self.first {
            self.first = false;
        } else {
            self.w.write_bits(b, 1);
        }
        while self.outstanding > 0 {
            self.w.write_bits(1 - b, 1);
            self.outstanding -= 1;
        }
    }
    fn renorm(&mut self) {
        while self.range < 256 {
            if self.low < 256 {
                self.put(0);
            } else if self.low >= 512 {
                self.low -= 512;
                self.put(1);
            } else {
                self.low -= 256;
                self.outstanding += 1;
            }
            self.range <<= 1;
            self.low <<= 1;
        }
    }
    fn decision(&mut self, ctx: usize, bin: u32) {
        let s = self.ctx[ctx] as usize;
        let q = ((self.range >> 6) & 3) as usize;
        let lps = RANGE_TAB_LPS[s >> 1][q] as u32;
        self.range -= lps;
        if bin != (s & 1) as u32 {
            self.low += self.range;
            self.range = lps;
            self.ctx[ctx] = NEXT_STATE[s][1];
        } else {
            self.ctx[ctx] = NEXT_STATE[s][0];
        }
        self.renorm();
    }
    fn terminate(&mut self, bin: u32) {
        self.range -= 2;
        if bin != 0 {
            self.low += self.range;
            self.range = 2;
            self.renorm();
            self.put((self.low >> 9) & 1);
            self.w.write_bits(((self.low >> 7) & 3) | 1, 2);
        } else {
            self.renorm();
        }
    }
}

fn pattern(pic: u32, c: usize, x: usize, y: usize) -> u8 {
    let v = (x * (3 + c) + y * (5 + pic as usize) + ((x ^ y) & 7) * 9 + pic as usize * 37 + c * 50)
        % 200;
    (v + 20) as u8
}

pub struct Planes {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

pub fn content(pic: u32) -> Planes {
    let mut p = Planes {
        y: vec![0; W * H],
        u: vec![0; W * H / 4],
        v: vec![0; W * H / 4],
    };
    for y in 0..H {
        for x in 0..W {
            p.y[y * W + x] = pattern(pic, 0, x, y);
        }
    }
    for y in 0..H / 2 {
        for x in 0..W / 2 {
            p.u[y * W / 2 + x] = pattern(pic, 1, x, y);
            p.v[y * W / 2 + x] = pattern(pic, 2, x, y);
        }
    }
    p
}

pub fn nal(t: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1, t << 1, 1];
    out.extend(escape_rbsp(rbsp));
    out
}

fn ptl(w: &mut BitWriter) {
    w.write_bits(0, 2);
    w.write_bits(0, 1);
    w.write_bits(1, 5);
    w.write_bits(0x6000_0000, 32);
    w.write_bits(0b1001, 4);
    w.write_bits(0, 32);
    w.write_bits(0, 11);
    w.write_bits(0, 1);
    w.write_bits(93, 8);
}

pub fn vps() -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(0, 4);
    w.write_bits(3, 2);
    w.write_bits(0, 6);
    w.write_bits(0, 3);
    w.write_bits(1, 1);
    w.write_bits(0xffff, 16);
    ptl(&mut w);
    w.write_bits(1, 1);
    w.write_ue(4);
    w.write_ue(0);
    w.write_ue(0);
    w.write_bits(0, 6);
    w.write_ue(0);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.rbsp_trailing();
    nal(32, &w.finish())
}

pub fn sps(pcm_loop_filter_disabled: bool) -> Vec<u8> {
    sps_with(pcm_loop_filter_disabled, None)
}

pub fn sps_with(pcm_loop_filter_disabled: bool, window: Option<[u32; 4]>) -> Vec<u8> {
    sps_sized(pcm_loop_filter_disabled, window, W as u32, H as u32)
}

pub fn sps_sized(
    pcm_loop_filter_disabled: bool,
    window: Option<[u32; 4]>,
    width: u32,
    height: u32,
) -> Vec<u8> {
    sps_tuned(pcm_loop_filter_disabled, window, width, height, ("", 0))
}

pub fn sps_tuned(
    pcm_loop_filter_disabled: bool,
    window: Option<[u32; 4]>,
    width: u32,
    height: u32,
    (field, value): (&str, u32),
) -> Vec<u8> {
    let tuned = |name: &str, default: u32| if name == field { value } else { default };
    let mut w = BitWriter::new();
    w.write_bits(0, 4);
    w.write_bits(0, 3);
    w.write_bits(1, 1);
    ptl(&mut w);
    w.write_ue(0);
    w.write_ue(tuned("chroma_format_idc", 1));
    w.write_ue(width);
    w.write_ue(height);
    w.write_bit(window.is_some());
    for v in window.into_iter().flatten() {
        w.write_ue(v);
    }
    w.write_ue(tuned("bit_depth_luma_minus8", 0));
    w.write_ue(0);
    w.write_ue(tuned("log2_max_pic_order_cnt_lsb_minus4", 4));
    w.write_bits(1, 1);
    w.write_ue(4);
    w.write_ue(0);
    w.write_ue(0);
    w.write_ue(tuned("log2_min_luma_coding_block_size_minus3", 0));
    w.write_ue(tuned("log2_diff_max_min_luma_coding_block_size", 1));
    w.write_ue(0);
    w.write_ue(2);
    w.write_ue(0);
    w.write_ue(0);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(1, 1);
    w.write_bits(7, 4);
    w.write_bits(7, 4);
    w.write_ue(0);
    w.write_ue(1);
    w.write_bits(pcm_loop_filter_disabled as u32, 1);
    w.write_ue(tuned("num_short_term_ref_pic_sets", 0));
    w.write_bits(1, 1);
    w.write_ue(0);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.rbsp_trailing();
    nal(33, &w.finish())
}

pub type Tiles = Option<(Vec<usize>, Vec<usize>, bool, bool)>;

pub fn pps(id: u32, tiles: &Tiles, lists_mod: bool) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_ue(id);
    w.write_ue(0);
    w.write_bits(1, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 3);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_ue(0);
    w.write_ue(0);
    w.write_se(0);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_se(0);
    w.write_se(0);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(tiles.is_some() as u32, 1);
    w.write_bits(0, 1);
    if let Some((cols, rows, uniform, across)) = tiles {
        w.write_ue(cols.len() as u32 - 1);
        w.write_ue(rows.len() as u32 - 1);
        w.write_bits(*uniform as u32, 1);
        if !uniform {
            for c in &cols[..cols.len() - 1] {
                w.write_ue(*c as u32 - 1);
            }
            for r in &rows[..rows.len() - 1] {
                w.write_ue(*r as u32 - 1);
            }
        }
        w.write_bits(*across as u32, 1);
    }
    w.write_bits(1, 1);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.write_bits(lists_mod as u32, 1);
    w.write_ue(0);
    w.write_bits(0, 1);
    w.write_bits(0, 1);
    w.rbsp_trailing();
    nal(34, &w.finish())
}

fn tile_scan(tiles: &Tiles) -> (Vec<usize>, Vec<usize>) {
    let (cols, rows) = match tiles {
        Some((c, r, _, _)) => (c.clone(), r.clone()),
        None => (vec![WC], vec![HC]),
    };
    let mut order = Vec::new();
    let mut tile = vec![0; WC * HC];
    let mut t = 0;
    let mut y0 = 0;
    for &rh in &rows {
        let mut x0 = 0;
        for &cw in &cols {
            for y in y0..y0 + rh {
                for x in x0..x0 + cw {
                    order.push(y * WC + x);
                    tile[y * WC + x] = t;
                }
            }
            t += 1;
            x0 += cw;
        }
        y0 += rh;
    }
    (order, tile)
}

#[derive(Clone, Copy, PartialEq)]
enum Cu {
    Pcm,
    Skip,
}

pub struct Segment {
    pub start: usize,
    pub len: usize,
    pub dependent: bool,
}

struct PicDesc {
    nal_type: u8,
    poc: u32,
    intra: bool,
    cu: Cu,
    pps_id: u32,
    st: Vec<(i32, bool)>,
    lt: Vec<(u32, bool)>,
    num_ref: u32,
    list_entry: Option<Vec<u32>>,
    segments: Vec<Segment>,
}

fn encode_picture(
    d: &PicDesc,
    tiles: &Tiles,
    pcm_content: &Planes,
    out: &mut Vec<u8>,
    saved: &mut Option<Contexts>,
) {
    let (order, tile_of) = tile_scan(tiles);
    let qp = 26;
    let init_type = if d.intra { 0 } else { 1 };
    for seg in &d.segments {
        let mut e = Enc::new(BitWriter::new(), qp, init_type);
        if seg.dependent {
            e.ctx = saved.expect("contexts of the previous segment");
        }
        let mut starts = vec![0usize];
        for k in seg.start..seg.start + seg.len {
            let rs = order[k];
            let (rx, ry) = (rs % WC, rs / WC);
            if k > 0 && tile_of[rs] != tile_of[order[k - 1]] {
                init_contexts(&mut e.ctx, qp, init_type);
            }
            e.decision(SPLIT_CU, 0);
            match d.cu {
                Cu::Pcm => {
                    if !d.intra {
                        e.decision(CU_SKIP, 0);
                        e.decision(PRED_MODE, 1);
                    }
                    e.terminate(1);
                    e.w.align_zero();
                    for y in 0..CTB {
                        for x in 0..CTB {
                            e.w.write_bits(
                                pcm_content.y[(ry * CTB + y) * W + rx * CTB + x] as u32,
                                8,
                            );
                        }
                    }
                    for plane in [&pcm_content.u, &pcm_content.v] {
                        for y in 0..CTB / 2 {
                            for x in 0..CTB / 2 {
                                e.w.write_bits(
                                    plane[(ry * CTB / 2 + y) * W / 2 + rx * CTB / 2 + x] as u32,
                                    8,
                                );
                            }
                        }
                    }
                    e.reset_engine();
                }
                Cu::Skip => {
                    let inc = (rx > 0) as usize + (ry > 0) as usize;
                    e.decision(CU_SKIP + inc, 1);
                }
            }
            let last = k + 1 == seg.start + seg.len;
            e.terminate(last as u32);
            if last {
                e.w.align_zero();
            } else if tile_of[order[k + 1]] != tile_of[rs] {
                e.terminate(1);
                e.w.align_zero();
                starts.push(e.w.bit_len() / 8);
                e.reset_engine();
            }
        }
        *saved = Some(e.ctx);
        let data = e.w.finish();
        let mut offsets: Vec<u32> = starts.windows(2).map(|s| (s[1] - s[0]) as u32).collect();
        loop {
            let mut w = BitWriter::new();
            let first = seg.start == 0;
            w.write_bit(first);
            if (16..=23).contains(&d.nal_type) {
                w.write_bit(false);
            }
            w.write_ue(d.pps_id);
            if !first {
                w.write_bit(seg.dependent);
                w.write_bits(order[seg.start] as u32, 5);
            }
            if !seg.dependent {
                w.write_ue(if d.intra { 2 } else { 1 });
                if d.nal_type != 19 && d.nal_type != 20 {
                    w.write_bits(d.poc & 255, 8);
                    w.write_bit(false);
                    w.write_ue(d.st.len() as u32);
                    w.write_ue(0);
                    let mut prev = 0;
                    for &(dp, used) in &d.st {
                        w.write_ue((prev - dp - 1) as u32);
                        w.write_bit(used);
                        prev = dp;
                    }
                    w.write_ue(d.lt.len() as u32);
                    for &(lsb, used) in &d.lt {
                        w.write_bits(lsb, 8);
                        w.write_bit(used);
                        w.write_bit(false);
                    }
                }
                if !d.intra {
                    w.write_bit(true);
                    w.write_ue(d.num_ref - 1);
                    let total =
                        d.st.iter().filter(|s| s.1).count() + d.lt.iter().filter(|l| l.1).count();
                    if d.list_entry.is_none() && d.pps_id == 1 && total > 1 {
                        w.write_bit(false);
                    }
                    if let Some(e) = &d.list_entry {
                        w.write_bit(true);
                        let bits = 32 - (total as u32 - 1).leading_zeros();
                        for &v in e {
                            w.write_bits(v, bits);
                        }
                    }
                    w.write_ue(4);
                }
                w.write_se(0);
                w.write_bit(true);
            }
            if tiles.is_some() {
                w.write_ue(offsets.len() as u32);
                if !offsets.is_empty() {
                    w.write_ue(31);
                    for &o in &offsets {
                        w.write_bits(o - 1, 32);
                    }
                }
            }
            w.write_bit(true);
            w.align_zero();
            let mut rbsp = w.finish();
            let hdr_len = rbsp.len();
            rbsp.extend_from_slice(&data);
            let esc_pos = |idx: usize| escape_rbsp(&rbsp[..idx]).len();
            let s: Vec<usize> = starts.iter().map(|&s| esc_pos(hdr_len + s)).collect();
            let new: Vec<u32> = s.windows(2).map(|p| (p[1] - p[0]) as u32).collect();
            if new == offsets {
                out.extend(nal(d.nal_type, &rbsp));
                break;
            }
            offsets = new;
        }
    }
}

pub fn whole(n: usize) -> Vec<Segment> {
    vec![Segment {
        start: 0,
        len: n,
        dependent: false,
    }]
}

pub fn sequence(
    tiles: &Tiles,
    pcm_lf_disabled: bool,
    segments: Vec<Segment>,
) -> (Vec<u8>, Vec<Planes>) {
    let mut s = Vec::new();
    s.extend(vps());
    s.extend(sps(pcm_lf_disabled));
    s.extend(pps(0, tiles, false));
    s.extend(pps(1, &None, true));
    let n = WC * HC;
    let mut saved = None;
    let c0 = content(0);
    let c1 = content(1);
    let p0 = PicDesc {
        nal_type: 19,
        poc: 0,
        intra: true,
        cu: Cu::Pcm,
        pps_id: 0,
        st: vec![],
        lt: vec![],
        num_ref: 0,
        list_entry: None,
        segments,
    };
    encode_picture(&p0, tiles, &c0, &mut s, &mut saved);
    let p1 = PicDesc {
        nal_type: 1,
        poc: 1,
        intra: true,
        cu: Cu::Pcm,
        pps_id: 1,
        st: vec![],
        lt: vec![(0, false)],
        num_ref: 0,
        list_entry: None,
        segments: whole(n),
    };
    encode_picture(&p1, &None, &c1, &mut s, &mut saved);
    let p2 = PicDesc {
        nal_type: 1,
        poc: 2,
        intra: false,
        cu: Cu::Skip,
        pps_id: 1,
        st: vec![(-1, false)],
        lt: vec![(0, true)],
        num_ref: 1,
        list_entry: None,
        segments: whole(n),
    };
    encode_picture(&p2, &None, &c1, &mut s, &mut saved);
    let p3 = PicDesc {
        nal_type: 1,
        poc: 3,
        intra: false,
        cu: Cu::Skip,
        pps_id: 1,
        st: vec![(-1, true), (-2, true)],
        lt: vec![(0, true)],
        num_ref: 3,
        list_entry: Some(vec![2, 0, 1]),
        segments: whole(n),
    };
    encode_picture(&p3, &None, &c1, &mut s, &mut saved);
    let p4 = PicDesc {
        nal_type: 1,
        poc: 4,
        intra: false,
        cu: Cu::Skip,
        pps_id: 1,
        st: vec![(-1, true), (-3, true)],
        lt: vec![(0, true)],
        num_ref: 2,
        list_entry: None,
        segments: whole(n),
    };
    encode_picture(&p4, &None, &c1, &mut s, &mut saved);
    let expect = vec![content(0), content(1), content(0), content(0), content(0)];
    (s, expect)
}

pub fn segment(start: usize, len: usize, dependent: bool) -> Segment {
    Segment {
        start,
        len,
        dependent,
    }
}

pub struct Deblocked {
    pub name: &'static str,
    pub tiles: Tiles,
    pub segments: Vec<Segment>,
}

fn case(name: &'static str, tiles: Tiles, segments: Vec<Segment>) -> Deblocked {
    Deblocked {
        name,
        tiles,
        segments,
    }
}

pub const DEBLOCKED: [&str; 6] = [
    "pcm_dbk",
    "tiles_across",
    "tiles_no_across",
    "tiles_slices",
    "slices",
    "row_slices",
];

pub fn deblocked(name: &str) -> Option<Deblocked> {
    let all = WC * HC;
    Some(match name {
        "pcm_dbk" => case("pcm_dbk", None, whole(all)),
        "tiles_across" => case(
            "tiles_across",
            Some((vec![1, 3, 2], vec![1, 3], false, true)),
            whole(all),
        ),
        "tiles_no_across" => case(
            "tiles_no_across",
            Some((vec![2, 3, 1], vec![3, 1], false, false)),
            whole(all),
        ),
        "tiles_slices" => case(
            "tiles_slices",
            Some((vec![3, 3], vec![2, 2], true, false)),
            vec![
                segment(0, 12, false),
                segment(12, 2, false),
                segment(14, 4, true),
                segment(18, 6, true),
            ],
        ),
        "slices" => case(
            "slices",
            None,
            vec![
                segment(0, 7, false),
                segment(7, 9, true),
                segment(16, 8, false),
            ],
        ),
        "row_slices" => case(
            "row_slices",
            None,
            vec![
                segment(0, 12, false),
                segment(12, 6, false),
                segment(18, 6, false),
            ],
        ),
        _ => return None,
    })
}

pub fn file(name: &str) -> String {
    format!("synth_{name}.hevc")
}

pub fn decoded(name: &str) -> String {
    format!("synth_{name}.yuv")
}

pub fn stream(name: &str) -> Result<Vec<u8>, String> {
    let d = deblocked(name).ok_or(format!("no synthetic HEVC case '{name}'"))?;
    Ok(sequence(&d.tiles, false, d.segments).0)
}
