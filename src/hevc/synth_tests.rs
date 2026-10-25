use super::Decoder;
use crate::hevc::cabac::{Contexts, NEXT_STATE, RANGE_TAB_LPS, init_contexts};
use crate::hevc::spec_tables::*;
use crate::syntax::bits::{BitWriter, escape_rbsp};

const W: usize = 96;
const H: usize = 64;
const CTB: usize = 16;
const WC: usize = W / CTB;
const HC: usize = H / CTB;

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

struct Planes {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

fn content(pic: u32) -> Planes {
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

fn nal(t: u8, rbsp: &[u8]) -> Vec<u8> {
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

fn vps() -> Vec<u8> {
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

fn sps(pcm_loop_filter_disabled: bool) -> Vec<u8> {
    sps_with(pcm_loop_filter_disabled, None)
}

fn sps_with(pcm_loop_filter_disabled: bool, window: Option<[u32; 4]>) -> Vec<u8> {
    sps_sized(pcm_loop_filter_disabled, window, W as u32, H as u32)
}

fn sps_sized(
    pcm_loop_filter_disabled: bool,
    window: Option<[u32; 4]>,
    width: u32,
    height: u32,
) -> Vec<u8> {
    sps_tuned(pcm_loop_filter_disabled, window, width, height, ("", 0))
}

fn sps_tuned(
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

type Tiles = Option<(Vec<usize>, Vec<usize>, bool, bool)>;

fn pps(id: u32, tiles: &Tiles, lists_mod: bool) -> Vec<u8> {
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

struct Segment {
    start: usize,
    len: usize,
    dependent: bool,
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

fn whole(n: usize) -> Vec<Segment> {
    vec![Segment {
        start: 0,
        len: n,
        dependent: false,
    }]
}

fn sequence(
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

fn decode_all(stream: &[u8], threads: usize) -> Vec<crate::media::Frame> {
    let mut dec = Decoder::with_threads(threads);
    let packet = crate::media::Packet {
        data: stream.to_vec(),
        pts: 0,
        dts: 0,
        keyframe: true,
    };
    let mut pics = dec.decode(&packet).expect("decode");
    pics.extend(dec.flush().expect("flush"));
    pics
}

fn tile_edges(sizes: &[usize]) -> Vec<usize> {
    sizes[..sizes.len() - 1]
        .iter()
        .scan(0, |at, s| {
            *at += s * CTB;
            Some(*at)
        })
        .collect()
}

fn near(v: usize, edges: &[usize], step: usize) -> bool {
    let reach = 4 / step;
    edges
        .iter()
        .any(|&e| (v as isize - (e / step) as isize).unsigned_abs() <= reach)
}

fn agrees_with_one_tile(
    name: &str,
    tiles: &Tiles,
    pics: &[crate::media::Frame],
    one: &[crate::media::Frame],
) {
    assert_eq!(pics.len(), one.len(), "{name}");
    let (cols, rows) = match tiles {
        Some((c, r, _, false)) => (tile_edges(c), tile_edges(r)),
        _ => (Vec::new(), Vec::new()),
    };
    for (i, (p, o)) in pics.iter().zip(one).enumerate() {
        for (k, (a, b)) in p.planes.iter().zip(&o.planes).enumerate() {
            let step = if k == 0 { 1 } else { 2 };
            for y in 0..a.height {
                for x in 0..a.width {
                    if near(x, &cols, step) || near(y, &rows, step) {
                        continue;
                    }
                    let (u, v) = (a.data[y * a.stride + x], b.data[y * b.stride + x]);
                    assert!(
                        u == v,
                        "{name}: picture {i} plane {k} at {x},{y} is {u}, one tile gives {v}"
                    );
                }
            }
        }
    }
}

fn run(name: &str, tiles: Tiles, pcm_lf_disabled: bool, segments: Vec<Segment>) {
    let (stream, expect) = sequence(&tiles, pcm_lf_disabled, segments);
    let one = (!pcm_lf_disabled).then(|| decode_all(&sequence(&None, false, whole(WC * HC)).0, 1));
    for threads in [1, 4] {
        let pics = decode_all(&stream, threads);
        assert_eq!(pics.len(), expect.len(), "{name}");
        for (i, (p, e)) in pics.iter().zip(&expect).enumerate() {
            if pcm_lf_disabled {
                assert!(p.planes[0].data == e.y, "{name}: picture {i} luma");
                assert!(
                    p.planes[1].data == e.u && p.planes[2].data == e.v,
                    "{name}: picture {i} chroma"
                );
            }
        }
        if let Some(one) = &one {
            agrees_with_one_tile(name, &tiles, &pics, one);
        }
    }
}

#[test]
fn pcm_long_term_refs_list_modification() {
    run("pcm_lt", None, true, whole(WC * HC));
}

#[test]
fn pcm_deblocked_single_tile() {
    run("pcm_dbk", None, false, whole(WC * HC));
}

#[test]
fn tiles_uniform_exact() {
    run(
        "tiles_uniform",
        Some((vec![2, 2, 2], vec![2, 2], true, true)),
        true,
        whole(WC * HC),
    );
}

#[test]
fn tiles_explicit_filter_across() {
    run(
        "tiles_across",
        Some((vec![1, 3, 2], vec![1, 3], false, true)),
        false,
        whole(WC * HC),
    );
}

#[test]
fn tiles_explicit_no_filter_across() {
    run(
        "tiles_no_across",
        Some((vec![2, 3, 1], vec![3, 1], false, false)),
        false,
        whole(WC * HC),
    );
}

#[test]
fn tiles_slices_and_dependent_segments() {
    let segs = vec![
        Segment {
            start: 0,
            len: 12,
            dependent: false,
        },
        Segment {
            start: 12,
            len: 2,
            dependent: false,
        },
        Segment {
            start: 14,
            len: 4,
            dependent: true,
        },
        Segment {
            start: 18,
            len: 6,
            dependent: true,
        },
    ];
    run(
        "tiles_slices",
        Some((vec![3, 3], vec![2, 2], true, false)),
        false,
        segs,
    );
}

#[test]
fn slices_without_tiles() {
    let segs = vec![
        Segment {
            start: 0,
            len: 7,
            dependent: false,
        },
        Segment {
            start: 7,
            len: 9,
            dependent: true,
        },
        Segment {
            start: 16,
            len: 8,
            dependent: false,
        },
    ];
    run("slices", None, false, segs);
}

#[test]
fn row_aligned_slices_decode_in_parallel() {
    let segs = vec![
        Segment {
            start: 0,
            len: 12,
            dependent: false,
        },
        Segment {
            start: 12,
            len: 6,
            dependent: false,
        },
        Segment {
            start: 18,
            len: 6,
            dependent: false,
        },
    ];
    run("row_slices", None, false, segs);
}

#[test]
fn conformance_window_past_the_picture_stays_inside_it() {
    let (stream, expect) = sequence(&None, true, whole(WC * HC));
    let plain = sps(true);
    let at = stream
        .windows(plain.len())
        .position(|w| w == plain.as_slice())
        .unwrap();
    let far = (W as u32 - 2) / 2;
    let window = [far, (1 << 31) - far, 0, 0];
    let mut s = stream[..at].to_vec();
    s.extend(sps_with(true, Some(window)));
    s.extend_from_slice(&stream[at + plain.len()..]);
    for threads in [1, 4] {
        let pics = decode_all(&s, threads);
        assert_eq!(pics.len(), expect.len());
        for p in &pics {
            assert!(p.width >= 1 && p.width <= W as u32 && p.height == H as u32);
            assert!(p.planes[0].data.len() >= p.width as usize);
        }
    }
}

#[test]
fn pictures_above_the_largest_level_are_refused() {
    let parse = |width, height| {
        let n = sps_sized(false, None, width, height);
        crate::syntax::hevc::params::Sps::parse(&crate::syntax::bits::unescape_rbsp(&n[6..]))
    };
    assert!(parse(W as u32, H as u32).is_ok());
    assert!(parse(8192, 4352).is_ok());
    assert!(parse(8192, 4368).is_err());
    assert!(parse(16384, 16384).is_err());
}

fn packet(data: Vec<u8>) -> crate::media::Packet {
    crate::media::Packet {
        data,
        pts: 0,
        dts: 0,
        keyframe: true,
    }
}

fn first_error(stream: &[u8], threads: usize) -> Option<String> {
    let mut dec = Decoder::with_threads(threads);
    let a = dec.decode(&packet(stream.to_vec())).err();
    let b = dec.flush().err();
    a.or(b).map(|e| e.to_string())
}

fn replaced(stream: &[u8], part: &[u8], with: &[u8]) -> Vec<u8> {
    let at = stream
        .windows(part.len())
        .position(|w| w == part)
        .expect("part not in the stream");
    [&stream[..at], with, &stream[at + part.len()..]].concat()
}

fn params() -> Vec<u8> {
    [vps(), sps(true), pps(0, &None, false), pps(1, &None, true)].concat()
}

fn expect_error(name: &str, stream: &[u8], wording: &str) {
    for threads in [1, 4] {
        let e = first_error(stream, threads)
            .unwrap_or_else(|| panic!("{name} ({threads} threads): decoded without an error"));
        assert!(
            e.contains(wording) && !e.contains("internal error"),
            "{name} ({threads} threads): {e}"
        );
    }
}

#[test]
fn a_pps_naming_a_missing_sps_is_an_error() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    expect_error(
        "no SPS",
        &replaced(&stream, &sps(true), &[]),
        "missing parameter set: SPS 0",
    );
}

#[test]
fn a_slice_before_any_parameter_set_is_an_error_and_the_decoder_recovers() {
    let (stream, expect) = sequence(&None, true, whole(WC * HC));
    let head = params();
    assert!(stream.starts_with(&head));
    let slices = stream[head.len()..].to_vec();
    expect_error("slices first", &slices, "missing parameter set: PPS 0");
    for threads in [1, 4] {
        let mut dec = Decoder::with_threads(threads);
        assert!(dec.decode(&packet(slices.clone())).is_err());
        let mut pics = dec
            .decode(&packet(stream.clone()))
            .expect("decode after the error");
        pics.extend(dec.flush().expect("flush after the error"));
        assert_eq!(pics.len(), expect.len(), "{threads} threads");
        for (i, (p, e)) in pics.iter().zip(&expect).enumerate() {
            for (plane, want) in [&e.y, &e.u, &e.v].into_iter().enumerate() {
                assert!(
                    &p.planes[plane].data == want,
                    "{threads} threads: picture {i}, plane {plane}"
                );
            }
        }
    }
}

fn slice_naming(pps_id: u32) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bit(true);
    w.write_bit(false);
    w.write_ue(pps_id);
    w.write_bits(0, 7);
    w.rbsp_trailing();
    nal(19, &w.finish())
}

#[test]
fn out_of_range_syntax_values_are_errors() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    let good = sps(true);
    for (name, width, wording) in [
        ("zero width", 0, "out of range"),
        ("width off the CB grid", W as u32 + 4, "MinCbSizeY"),
    ] {
        let bad = sps_sized(true, None, width, H as u32);
        expect_error(name, &replaced(&stream, &good, &bad), wording);
    }
    expect_error(
        "PPS id 64",
        &replaced(&stream, &pps(0, &None, false), &pps(64, &None, false)),
        "pps_pic_parameter_set_id out of range",
    );
    let head = params();
    expect_error(
        "slice PPS id 64",
        &[head.clone(), slice_naming(64)].concat(),
        "slice_pic_parameter_set_id out of range",
    );
    expect_error(
        "slice naming an absent PPS",
        &[head.clone(), slice_naming(7)].concat(),
        "missing parameter set: PPS 7",
    );
    let mut forbidden = slice_naming(0);
    forbidden[4] |= 0x80;
    expect_error(
        "forbidden_zero_bit",
        &[head.clone(), forbidden].concat(),
        "forbidden_zero_bit",
    );
    let mut tid0 = slice_naming(0);
    tid0[5] = 0;
    expect_error(
        "temporal id 0",
        &[head, tid0].concat(),
        "nuh_temporal_id_plus1",
    );
}

#[test]
fn truncated_synthetic_nal_units_fail_without_panics() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    let nals: Vec<Vec<u8>> = crate::syntax::bits::annexb_nals(&stream)
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect();
    for (k, n) in nals.iter().enumerate() {
        for cut in [2, n.len() / 2, n.len() - 1] {
            if cut < 2 || cut >= n.len() {
                continue;
            }
            let mut s = Vec::new();
            for (j, m) in nals.iter().enumerate() {
                s.extend([0, 0, 0, 1]);
                s.extend_from_slice(if j == k { &m[..cut] } else { m });
            }
            for threads in [1, 4] {
                if let Some(e) = first_error(&s, threads) {
                    assert!(
                        !e.contains("internal error"),
                        "NAL {k} cut to {cut} ({threads} threads): {e}"
                    );
                } else {
                    assert!(
                        cut > n.len() / 2 || k > 3,
                        "NAL {k} cut to {cut} ({threads} threads) decoded without an error"
                    );
                }
            }
        }
    }
}

#[test]
fn out_of_range_sps_values_are_errors() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    let good = sps(true);
    for (field, value, wording) in [
        ("chroma_format_idc", 4, "chroma_format_idc out of range"),
        ("chroma_format_idc", 2, "only 4:2:0"),
        ("bit_depth_luma_minus8", 4, "bit depth 12/8"),
        ("bit_depth_luma_minus8", 9, "bit depth out of range"),
        (
            "log2_max_pic_order_cnt_lsb_minus4",
            13,
            "log2_max_pic_order_cnt_lsb_minus4 out of range",
        ),
        (
            "log2_min_luma_coding_block_size_minus3",
            4,
            "CTB size out of range",
        ),
        (
            "log2_diff_max_min_luma_coding_block_size",
            4,
            "CTB size out of range",
        ),
        (
            "num_short_term_ref_pic_sets",
            65,
            "num_short_term_ref_pic_sets out of range",
        ),
    ] {
        let bad = sps_tuned(true, None, W as u32, H as u32, (field, value));
        expect_error(
            &format!("{field} {value}"),
            &replaced(&stream, &good, &bad),
            wording,
        );
    }
}

#[test]
fn a_slice_segment_address_past_the_picture_is_an_error() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    assert_eq!(WC * HC, 24);
    let mut w = BitWriter::new();
    w.write_bit(false);
    w.write_ue(0);
    w.write_bit(false);
    w.write_bits(31, 5);
    w.write_ue(2);
    w.rbsp_trailing();
    expect_error(
        "slice_segment_address 31 of 24",
        &[stream, nal(1, &w.finish())].concat(),
        "slice_segment_address out of range",
    );
}

#[test]
fn sixteen_active_references_are_an_error() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    let mut w = BitWriter::new();
    w.write_bit(true);
    w.write_ue(0);
    w.write_ue(1);
    w.write_bits(1, 8);
    w.write_bit(false);
    w.write_ue(1);
    w.write_ue(0);
    w.write_ue(0);
    w.write_bit(true);
    w.write_ue(0);
    w.write_bit(true);
    w.write_ue(15);
    w.write_ue(0);
    w.write_se(0);
    w.rbsp_trailing();
    expect_error(
        "num_ref_idx_l0_active_minus1 15",
        &[stream, nal(1, &w.finish())].concat(),
        "num_ref_idx_active out of range",
    );
}

#[test]
fn more_entry_points_than_ctbs_are_an_error() {
    let tiles: Tiles = Some((vec![3, 3], vec![2, 2], false, true));
    let mut w = BitWriter::new();
    w.write_bit(true);
    w.write_bit(false);
    w.write_ue(0);
    w.write_ue(2);
    w.write_se(0);
    w.write_bit(true);
    w.write_ue(WC as u32 * HC as u32 + 1);
    w.write_ue(0);
    for _ in 0..=WC * HC {
        w.write_bit(false);
    }
    w.rbsp_trailing();
    let stream = [
        vps(),
        sps(true),
        pps(0, &tiles, false),
        nal(19, &w.finish()),
    ]
    .concat();
    expect_error(
        "num_entry_point_offsets 25 of 24",
        &stream,
        "num_entry_point_offsets out of range",
    );
}
