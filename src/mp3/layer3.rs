use super::bits::Bits;
use super::hdr::Head;
use super::simd::F4;
use super::tables::{
    AA, LINBITS, MDCT_WINDOW, PAN, POW43, SCF_LONG, SCF_MIXED, SCF_PARTITIONS, SCF_SHORT,
    TAB_INDEX, TAB32, TAB33, TABS, TWID9,
};

const SHORT_BLOCK_TYPE: u8 = 2;
const STOP_BLOCK_TYPE: u8 = 3;
const MAX_SCFI: i32 = 44;

const EXPFRAC: [f32; 4] = [
    9.31322575e-10,
    7.83145814e-10,
    6.58544508e-10,
    5.53767716e-10,
];
const SCFC_DECODE: [u8; 16] = [0, 1, 2, 3, 12, 5, 6, 7, 9, 10, 11, 13, 14, 15, 18, 19];
const MOD: [u8; 24] = [
    5, 5, 4, 4, 5, 5, 4, 1, 4, 3, 1, 1, 5, 6, 6, 1, 4, 4, 4, 1, 4, 3, 1, 1,
];
const PREAMP: [u8; 10] = [1, 1, 1, 1, 2, 2, 3, 3, 3, 2];
const TWID3: [f32; 6] = [
    0.79335334, 0.92387953, 0.99144486, 0.60876143, 0.38268343, 0.13052619,
];

#[derive(Clone, Copy, Default)]
pub struct GrInfo {
    sfbtab: &'static [u8],
    part_23_length: u16,
    big_values: u16,
    scalefac_compress: u16,
    global_gain: u8,
    block_type: u8,
    mixed_block_flag: u8,
    n_long_sfb: u8,
    n_short_sfb: u8,
    table_select: [u8; 3],
    region_count: [u8; 3],
    subblock_gain: [u8; 3],
    preflag: u8,
    scalefac_scale: u8,
    count1_table: u8,
    scfsi: u8,
}

pub fn read_side_info(bs: &mut Bits, gr: &mut [GrInfo; 4], h: Head) -> i32 {
    let mpeg1 = h.mpeg1();
    let mono = h.mono();
    let mut sr_idx = h.my_sample_rate();
    sr_idx -= (sr_idx != 0) as usize;
    let mut gr_count = if mono { 1 } else { 2 };
    let mut scfsi: u32 = 0;
    let main_data_begin = if mpeg1 {
        gr_count *= 2;
        let m = bs.get(9) as i32;
        scfsi = bs.get(7 + gr_count) & if mono { 0x0F } else { 0xFF };
        m
    } else {
        (bs.get(8 + gr_count) >> gr_count) as i32
    };
    let mut part_23_sum = 0i64;
    for g in gr.iter_mut().take(gr_count as usize) {
        if mono {
            scfsi <<= 4;
        }
        g.part_23_length = bs.get(12) as u16;
        part_23_sum += g.part_23_length as i64;
        g.big_values = bs.get(9) as u16;
        if g.big_values > 288 {
            return -1;
        }
        g.global_gain = bs.get(8) as u8;
        g.scalefac_compress = bs.get(if mpeg1 { 4 } else { 9 }) as u16;
        g.sfbtab = &SCF_LONG[sr_idx];
        g.n_long_sfb = 22;
        g.n_short_sfb = 0;
        let tables;
        if bs.get(1) != 0 {
            g.block_type = bs.get(2) as u8;
            if g.block_type == 0 {
                return -1;
            }
            g.mixed_block_flag = bs.get(1) as u8;
            g.region_count[0] = 7;
            g.region_count[1] = 255;
            if g.block_type == SHORT_BLOCK_TYPE {
                scfsi &= 0x0F0F;
                if g.mixed_block_flag == 0 {
                    g.region_count[0] = 8;
                    g.sfbtab = &SCF_SHORT[sr_idx];
                    g.n_long_sfb = 0;
                    g.n_short_sfb = 39;
                } else {
                    g.sfbtab = &SCF_MIXED[sr_idx];
                    g.n_long_sfb = if mpeg1 { 8 } else { 6 };
                    g.n_short_sfb = 30;
                }
            }
            tables = bs.get(10) << 5;
            g.subblock_gain[0] = bs.get(3) as u8;
            g.subblock_gain[1] = bs.get(3) as u8;
            g.subblock_gain[2] = bs.get(3) as u8;
        } else {
            g.block_type = 0;
            g.mixed_block_flag = 0;
            tables = bs.get(15);
            g.region_count[0] = bs.get(4) as u8;
            g.region_count[1] = bs.get(3) as u8;
            g.region_count[2] = 255;
        }
        g.table_select = [
            (tables >> 10) as u8,
            ((tables >> 5) & 31) as u8,
            (tables & 31) as u8,
        ];
        g.preflag = if mpeg1 {
            bs.get(1) as u8
        } else {
            (g.scalefac_compress >= 500) as u8
        };
        g.scalefac_scale = bs.get(1) as u8;
        g.count1_table = bs.get(1) as u8;
        g.scfsi = ((scfsi >> 12) & 15) as u8;
        scfsi <<= 4;
    }
    if part_23_sum + bs.pos as i64 > bs.limit as i64 + main_data_begin as i64 * 8 {
        return -1;
    }
    main_data_begin
}

fn read_scalefactors(
    scf: &mut [u8; 40],
    ist_pos: &mut [u8; 39],
    scf_size: &[u8; 4],
    scf_count: &[u8],
    bs: &mut Bits,
    mut scfsi: i32,
) {
    let mut o = 0;
    for i in 0..4 {
        let cnt = scf_count.get(i).copied().unwrap_or(0) as usize;
        if cnt == 0 || o + cnt > 39 {
            break;
        }
        if scfsi & 8 != 0 {
            scf[o..o + cnt].copy_from_slice(&ist_pos[o..o + cnt]);
        } else {
            let bits = scf_size[i] as u32;
            if bits == 0 {
                scf[o..o + cnt].fill(0);
                ist_pos[o..o + cnt].fill(0);
            } else {
                let max_scf: i32 = if scfsi < 0 { (1 << bits) - 1 } else { -1 };
                for k in 0..cnt {
                    let s = bs.get(bits) as i32;
                    ist_pos[o + k] = if s == max_scf { 0xFF } else { s as u8 };
                    scf[o + k] = s as u8;
                }
            }
        }
        o += cnt;
        scfsi = scfsi.wrapping_mul(2);
    }
    let end = (o + 3).min(40);
    scf[o..end].fill(0);
}

#[inline]
fn ldexp_q2(mut y: f32, mut exp_q2: i32) -> f32 {
    loop {
        let e = exp_q2.clamp(0, 30 * 4);
        y *= EXPFRAC[(e & 3) as usize] * ((1i32 << 30) >> (e >> 2)) as f32;
        exp_q2 -= e;
        if exp_q2 <= 0 {
            return y;
        }
    }
}

fn decode_scalefactors(
    h: Head,
    ist_pos: &mut [u8; 39],
    bs: &mut Bits,
    gr: &GrInfo,
    scf: &mut [f32; 40],
    ch: usize,
) {
    let row = (gr.n_short_sfb != 0) as usize + (gr.n_long_sfb == 0) as usize;
    let mut part_off = 0;
    let mut scf_size = [0u8; 4];
    let mut iscf = [0u8; 40];
    let scf_shift = gr.scalefac_scale as u32 + 1;
    let mut scfsi = gr.scfsi as i32;
    if h.mpeg1() {
        let part = SCFC_DECODE[(gr.scalefac_compress & 15) as usize];
        scf_size[0] = part >> 2;
        scf_size[1] = part >> 2;
        scf_size[2] = part & 3;
        scf_size[3] = part & 3;
    } else {
        let ist = (h.i_stereo() && ch != 0) as usize;
        let mut sfc = (gr.scalefac_compress >> ist) as i32;
        let mut k = ist * 12;
        while sfc >= 0 && k + 4 <= MOD.len() {
            let mut modprod = 1i32;
            for i in (0..4).rev() {
                let m = MOD[k + i] as i32;
                scf_size[i] = (sfc / modprod % m) as u8;
                modprod *= m;
            }
            sfc -= modprod;
            k += 4;
        }
        part_off = k;
        scfsi = -16;
    }
    let partition = SCF_PARTITIONS[row].get(part_off..).unwrap_or(&[]);
    read_scalefactors(&mut iscf, ist_pos, &scf_size, partition, bs, scfsi);
    let nl = gr.n_long_sfb as usize;
    let ns = gr.n_short_sfb as usize;
    if ns != 0 {
        let sh = 3 - scf_shift;
        for i in (0..ns).step_by(3) {
            for j in 0..3 {
                iscf[nl + i + j] = iscf[nl + i + j].wrapping_add(gr.subblock_gain[j] << sh);
            }
        }
    } else if gr.preflag != 0 {
        for (i, p) in PREAMP.iter().enumerate() {
            iscf[11 + i] = iscf[11 + i].wrapping_add(*p);
        }
    }
    let gain_exp = gr.global_gain as i32 - 4 - 210 - if h.is_ms_stereo() { 2 } else { 0 };
    let gain = ldexp_q2((1 << (MAX_SCFI / 4)) as f32, MAX_SCFI - gain_exp);
    for i in 0..(nl + ns).min(40) {
        scf[i] = ldexp_q2(gain, (iscf[i] as i32) << scf_shift);
    }
}

#[inline]
fn pow_43(mut x: i32) -> f32 {
    if x < 129 {
        return POW43[(16 + x) as usize];
    }
    let mut mult = 256.0;
    if x < 1024 {
        mult = 16.0;
        x <<= 3;
    }
    let sign = (2 * x) & 64;
    let frac = ((x & 63) - sign) as f32 / ((x & !63) + sign) as f32;
    POW43
        .get((16 + ((x + sign) >> 6)) as usize)
        .copied()
        .unwrap_or(0.0)
        * (1.0 + frac * ((4.0 / 3.0) + frac * (2.0 / 9.0)))
        * mult
}

struct Reader<'a> {
    buf: &'a [u8],
    next: usize,
    cache: u32,
    sh: i32,
}

impl Reader<'_> {
    #[inline(always)]
    fn byte(&self, i: usize) -> u32 {
        self.buf.get(i).copied().unwrap_or(0) as u32
    }

    #[inline(always)]
    fn peek(&self, n: u32) -> u32 {
        ((self.cache as u64) >> (32 - n)) as u32
    }

    #[inline(always)]
    fn flush(&mut self, n: u32) {
        self.cache = ((self.cache as u64) << n) as u32;
        self.sh += n as i32;
    }

    #[inline(always)]
    fn check(&mut self) {
        while self.sh >= 0 {
            self.cache |= self.byte(self.next) << self.sh;
            self.next += 1;
            self.sh -= 8;
        }
    }

    #[inline(always)]
    fn pos(&self) -> i64 {
        self.next as i64 * 8 - 24 + self.sh as i64
    }

    #[inline(always)]
    fn sign(&self) -> bool {
        (self.cache as i32) < 0
    }
}

#[inline(always)]
fn leaf(code: usize, idx: i32) -> i32 {
    TABS.get((code as i32 + idx) as usize).copied().unwrap_or(0) as i32
}

fn huffman(dst: &mut [f32], bs: &mut Bits, gr: &GrInfo, scf: &[f32; 40], limit: usize) {
    let start = bs.pos / 8;
    let mut r = Reader {
        buf: bs.buf,
        next: start + 4,
        cache: 0,
        sh: (bs.pos & 7) as i32 - 8,
    };
    r.cache = (((r.byte(start) * 256 + r.byte(start + 1)) * 256 + r.byte(start + 2)) * 256
        + r.byte(start + 3))
        << (bs.pos & 7);
    let sfbtab = gr.sfbtab;
    let mut sfb = 0usize;
    let mut sc = 0usize;
    let sfb_at = |i: usize| sfbtab.get(i).copied().unwrap_or(0) as i32;
    let scf_at = |i: usize| scf.get(i).copied().unwrap_or(0.0);
    let mut one = 0.0f32;
    let mut ireg = 0usize;
    let mut big_val_cnt = gr.big_values as i32;
    let mut d = 0usize;
    let n = dst.len();
    'big: while big_val_cnt > 0 {
        let Some(&tab_num) = gr.table_select.get(ireg) else {
            break;
        };
        let mut sfb_cnt = gr.region_count[ireg] as i32;
        ireg += 1;
        let code = TAB_INDEX[tab_num as usize & 31] as usize;
        let linbits = LINBITS[tab_num as usize & 31] as u32;
        loop {
            let np = sfb_at(sfb) / 2;
            sfb += 1;
            if np == 0 || d + 2 * np.min(big_val_cnt) as usize > n {
                break 'big;
            }
            let mut pairs = big_val_cnt.min(np);
            one = scf_at(sc);
            sc += 1;
            while pairs > 0 {
                let mut w = 5;
                let mut lf = leaf(code, r.peek(w) as i32);
                while lf < 0 {
                    r.flush(w);
                    w = (lf & 7) as u32;
                    lf = leaf(code, r.peek(w) as i32 - (lf >> 3));
                }
                r.flush((lf >> 8) as u32);
                for _ in 0..2 {
                    let mut lsb = lf & 0x0F;
                    if linbits != 0 && lsb == 15 {
                        lsb += r.peek(linbits) as i32;
                        r.flush(linbits);
                        r.check();
                        let s = if r.sign() { -1.0 } else { 1.0 };
                        dst[d] = one * pow_43(lsb) * s;
                    } else {
                        dst[d] = POW43[(16 + lsb - 16 * (r.cache >> 31) as i32) as usize] * one;
                    }
                    r.flush((lsb != 0) as u32);
                    d += 1;
                    lf >>= 4;
                }
                r.check();
                pairs -= 1;
            }
            big_val_cnt -= np;
            sfb_cnt -= 1;
            if big_val_cnt <= 0 || sfb_cnt < 0 {
                break;
            }
        }
    }
    let codebook: &[u8] = if gr.count1_table != 0 { &TAB33 } else { &TAB32 };
    let mut np = 1 - big_val_cnt;
    let limit = limit as i64;
    loop {
        let mut lf = codebook[(r.peek(4) & 15) as usize] as i32;
        if lf & 8 == 0 {
            let idx = (lf >> 3) as u32 + ((((r.cache << 4) as u64) >> (32 - (lf & 3))) as u32);
            lf = codebook.get(idx as usize).copied().unwrap_or(0) as i32;
        }
        r.flush((lf & 7) as u32);
        if r.pos() > limit {
            break;
        }
        np -= 1;
        if np == 0 {
            np = sfb_at(sfb) / 2;
            sfb += 1;
            if np == 0 {
                break;
            }
            one = scf_at(sc);
            sc += 1;
        }
        if d + 2 > n {
            break;
        }
        for s in 0..2 {
            if lf & (128 >> s) != 0 {
                dst[d + s] = if r.sign() { -one } else { one };
                r.flush(1);
            }
        }
        np -= 1;
        if np == 0 {
            np = sfb_at(sfb) / 2;
            sfb += 1;
            if np == 0 {
                break;
            }
            one = scf_at(sc);
            sc += 1;
        }
        if d + 4 > n {
            break;
        }
        for s in 2..4 {
            if lf & (128 >> s) != 0 {
                dst[d + s] = if r.sign() { -one } else { one };
                r.flush(1);
            }
        }
        r.check();
        d += 4;
    }
    bs.pos = limit as usize;
}

fn midside(g: &mut [f32], l: usize, n: usize) {
    let r = l + 576;
    let mut i = 0;
    while i + 3 < n {
        let vl = F4::ld(g, l + i);
        let vr = F4::ld(g, r + i);
        vl.add(vr).st(g, l + i);
        vl.sub(vr).st(g, r + i);
        i += 4;
    }
    while i < n {
        let a = g[l + i];
        let b = g[r + i];
        g[l + i] = a + b;
        g[r + i] = a - b;
        i += 1;
    }
}

fn intensity_band(g: &mut [f32], l: usize, n: usize, kl: f32, kr: f32) {
    for i in l..l + n {
        g[i + 576] = g[i] * kr;
        g[i] *= kl;
    }
}

fn stereo_top_band(g: &[f32], sfb: &[u8], nbands: usize, max_band: &mut [i32; 3]) {
    *max_band = [-1; 3];
    let mut r = 576;
    for (i, &n) in sfb.iter().take(nbands).enumerate() {
        let n = n as usize;
        let mut k = 0;
        while k < n {
            if g[r + k] != 0.0 || g[r + k + 1] != 0.0 {
                max_band[i % 3] = i as i32;
                break;
            }
            k += 2;
        }
        r += n;
    }
}

fn stereo_process(
    g: &mut [f32],
    ist_pos: &[u8; 39],
    sfb: &[u8],
    h: Head,
    max_band: &[i32; 3],
    mpeg2_sh: i32,
) {
    let mpeg1 = h.mpeg1();
    let ms = h.ms_stereo();
    let max_pos = if mpeg1 { 7 } else { 64 };
    let mut l = 0;
    for (i, &n) in sfb.iter().enumerate().take(39) {
        if n == 0 {
            break;
        }
        let n = n as usize;
        let ipos = ist_pos[i] as u32;
        if i as i32 > max_band[i % 3] && ipos < max_pos {
            let s = if ms { std::f32::consts::SQRT_2 } else { 1.0 };
            let (kl, kr) = if mpeg1 {
                (PAN[2 * ipos as usize], PAN[2 * ipos as usize + 1])
            } else {
                let kr = ldexp_q2(1.0, (((ipos + 1) >> 1) << mpeg2_sh) as i32);
                if ipos & 1 != 0 { (kr, 1.0) } else { (1.0, kr) }
            };
            intensity_band(g, l, n, kl * s, kr * s);
        } else if ms {
            midside(g, l, n);
        }
        l += n;
    }
}

fn intensity_stereo(g: &mut [f32], ist_pos: &mut [u8; 39], gr: &[GrInfo], h: Head) {
    let g0 = &gr[0];
    let n_sfb = g0.n_long_sfb as usize + g0.n_short_sfb as usize;
    let max_blocks = if g0.n_short_sfb != 0 { 3 } else { 1 };
    let mut max_band = [-1; 3];
    stereo_top_band(g, g0.sfbtab, n_sfb, &mut max_band);
    if g0.n_long_sfb != 0 {
        let m = max_band[0].max(max_band[1]).max(max_band[2]);
        max_band = [m; 3];
    }
    for i in 0..max_blocks {
        let default_pos = if h.mpeg1() { 3 } else { 0 };
        let itop = n_sfb - max_blocks + i;
        let prev = itop - max_blocks;
        ist_pos[itop] = if max_band[i] >= prev as i32 {
            default_pos
        } else {
            ist_pos[prev]
        };
    }
    let sh = (gr[1].scalefac_compress & 1) as i32;
    stereo_process(g, ist_pos, g0.sfbtab, h, &max_band, sh);
}

fn reorder(g: &mut [f32], base: usize, scratch: &mut [f32], sfb: &[u8]) {
    let mut src = base;
    let mut dst = 0;
    for &len in sfb.iter().step_by(3) {
        if len == 0 {
            break;
        }
        let len = len as usize;
        for _ in 0..len {
            scratch[dst] = g[src];
            scratch[dst + 1] = g[src + len];
            scratch[dst + 2] = g[src + 2 * len];
            dst += 3;
            src += 1;
        }
        src += 2 * len;
    }
    g[base..base + dst].copy_from_slice(&scratch[..dst]);
}

fn antialias(g: &mut [f32], base: usize, nbands: i32) {
    let c0 = [F4::ld(&AA, 0), F4::ld(&AA, 4)];
    let c1 = [F4::ld(&AA, 8), F4::ld(&AA, 12)];
    for b in 0..nbands.max(0) as usize {
        let o = base + 18 * b;
        for (j, i) in [0, 4].into_iter().enumerate() {
            let vu = F4::ld(g, o + 18 + i);
            let vd = F4::ld(g, o + 14 - i).rev();
            vu.mul(c0[j]).sub(vd.mul(c1[j])).st(g, o + 18 + i);
            vu.mul(c1[j]).add(vd.mul(c0[j])).rev().st(g, o + 14 - i);
        }
    }
}

fn dct3_9(y: &mut [f32; 9]) {
    let mut s0 = y[0];
    let mut s2 = y[2];
    let mut s4 = y[4];
    let mut s6 = y[6];
    let mut s8 = y[8];
    let mut t0 = s0 + s6 * 0.5;
    s0 -= s6;
    let mut t4 = (s4 + s2) * 0.93969262;
    let mut t2 = (s8 + s2) * 0.76604444;
    s6 = (s4 - s8) * 0.17364818;
    s4 += s8 - s2;
    s2 = s0 - s4 * 0.5;
    y[4] = s4 + s0;
    s8 = t0 - t2 + s6;
    s0 = t0 - t4 + t2;
    s4 = t0 + t4 - s6;
    let mut s1 = y[1];
    let mut s3 = y[3];
    let mut s5 = y[5];
    let mut s7 = y[7];
    s3 *= 0.86602540;
    t0 = (s5 + s1) * 0.98480775;
    t4 = (s5 - s7) * 0.34202014;
    t2 = (s1 + s7) * 0.64278761;
    s1 = (s1 - s5 - s7) * 0.86602540;
    s5 = t0 - s3 - t2;
    s7 = t4 - s3 - t0;
    s3 = t4 + s3 - t2;
    y[0] = s4 - s7;
    y[1] = s2 + s1;
    y[2] = s0 - s3;
    y[3] = s8 + s5;
    y[5] = s8 - s5;
    y[6] = s0 + s3;
    y[7] = s2 - s1;
    y[8] = s4 + s7;
}

fn imdct36(
    g: &mut [f32],
    base: usize,
    ov: &mut [f32],
    obase: usize,
    window: &[f32],
    nbands: usize,
) {
    let r0 = [F4::ld(&TWID9, 0), F4::ld(&TWID9, 4)];
    let r1 = [F4::ld(&TWID9, 9), F4::ld(&TWID9, 13)];
    let w0 = [F4::ld(window, 0), F4::ld(window, 4)];
    let w1 = [F4::ld(window, 9), F4::ld(window, 13)];
    for j in 0..nbands {
        let o = base + 18 * j;
        let oo = obase + 9 * j;
        let mut co = [0.0f32; 9];
        let mut si = [0.0f32; 9];
        co[0] = -g[o];
        si[0] = g[o + 17];
        for i in 0..4 {
            si[8 - 2 * i] = g[o + 4 * i + 1] - g[o + 4 * i + 2];
            co[1 + 2 * i] = g[o + 4 * i + 1] + g[o + 4 * i + 2];
            si[7 - 2 * i] = g[o + 4 * i + 4] - g[o + 4 * i + 3];
            co[2 + 2 * i] = -(g[o + 4 * i + 3] + g[o + 4 * i + 4]);
        }
        dct3_9(&mut co);
        dct3_9(&mut si);
        si[1] = -si[1];
        si[3] = -si[3];
        si[5] = -si[5];
        si[7] = -si[7];
        for (k, i) in [0, 4].into_iter().enumerate() {
            let vovl = F4::ld(ov, oo + i);
            let vc = F4::ld(&co, i);
            let vs = F4::ld(&si, i);
            let vsum = vc.mul(r1[k]).add(vs.mul(r0[k]));
            vc.mul(r0[k]).sub(vs.mul(r1[k])).st(ov, oo + i);
            vovl.mul(w0[k]).sub(vsum.mul(w1[k])).st(g, o + i);
            vovl.mul(w1[k]).add(vsum.mul(w0[k])).rev().st(g, o + 14 - i);
        }
        let ovl = ov[oo + 8];
        let sum = co[8] * TWID9[17] + si[8] * TWID9[8];
        ov[oo + 8] = co[8] * TWID9[8] - si[8] * TWID9[17];
        g[o + 8] = ovl * window[8] - sum * window[17];
        g[o + 9] = ovl * window[17] + sum * window[8];
    }
}

fn idct3(x0: f32, x1: f32, x2: f32) -> [f32; 3] {
    let m1 = x1 * 0.86602540;
    let a1 = x0 - x2 * 0.5;
    [a1 + m1, x0 + x2, a1 - m1]
}

fn imdct12(x: &[f32], dst: &mut [f32], ov: &mut [f32]) {
    let co = idct3(-x[0], x[6] + x[3], x[12] + x[9]);
    let mut si = idct3(x[15], x[12] - x[9], x[6] - x[3]);
    si[1] = -si[1];
    for i in 0..3 {
        let ovl = ov[i];
        let sum = co[i] * TWID3[3 + i] + si[i] * TWID3[i];
        ov[i] = co[i] * TWID3[i] - si[i] * TWID3[3 + i];
        dst[i] = ovl * TWID3[2 - i] - sum * TWID3[5 - i];
        dst[5 - i] = ovl * TWID3[5 - i] + sum * TWID3[2 - i];
    }
}

fn imdct_short(g: &mut [f32], base: usize, ov: &mut [f32], obase: usize, nbands: usize) {
    for b in 0..nbands {
        let o = base + 18 * b;
        let oo = obase + 9 * b;
        let mut tmp = [0.0f32; 18];
        tmp.copy_from_slice(&g[o..o + 18]);
        g[o..o + 6].copy_from_slice(&ov[oo..oo + 6]);
        let (lo, hi) = ov[oo..oo + 9].split_at_mut(6);
        imdct12(&tmp, &mut g[o + 6..o + 12], hi);
        imdct12(&tmp[1..], &mut g[o + 12..o + 18], hi);
        imdct12(&tmp[2..], lo, hi);
    }
}

fn change_sign(g: &mut [f32], base: usize) {
    for b in 0..16 {
        let o = base + 18 + 36 * b;
        for i in (1..18).step_by(2) {
            g[o + i] = -g[o + i];
        }
    }
}

fn imdct_gr(g: &mut [f32], base: usize, ov: &mut [f32; 288], block_type: u8, nlb: usize) {
    let mut gb = base;
    let mut ob = 0;
    if nlb > 0 {
        imdct36(g, gb, ov, ob, &MDCT_WINDOW[..18], nlb);
        gb += 18 * nlb;
        ob += 9 * nlb;
    }
    if block_type == SHORT_BLOCK_TYPE {
        imdct_short(g, gb, ov, ob, 32 - nlb);
    } else {
        let w = if block_type == STOP_BLOCK_TYPE { 18 } else { 0 };
        imdct36(g, gb, ov, ob, &MDCT_WINDOW[w..w + 18], 32 - nlb);
    }
}

pub struct Work<'a> {
    pub bs: Bits<'a>,
    pub grbuf: &'a mut [f32; 1156],
    pub scf: &'a mut [f32; 40],
    pub syn: &'a mut [f32; 2112],
    pub ist_pos: &'a mut [[u8; 39]; 2],
}

pub fn decode(w: &mut Work, h: Head, overlap: &mut [[f32; 288]; 2], gr: &[GrInfo], nch: usize) {
    for ch in 0..nch {
        let limit = w.bs.pos + gr[ch].part_23_length as usize;
        decode_scalefactors(h, &mut w.ist_pos[ch], &mut w.bs, &gr[ch], w.scf, ch);
        huffman(
            &mut w.grbuf[576 * ch..576 * ch + 576],
            &mut w.bs,
            &gr[ch],
            w.scf,
            limit,
        );
    }
    if h.i_stereo() {
        intensity_stereo(&mut w.grbuf[..], &mut w.ist_pos[1], gr, h);
    } else if h.is_ms_stereo() {
        midside(&mut w.grbuf[..], 0, 576);
    }
    for ch in 0..nch {
        let g = &gr[ch];
        let mut aa_bands = 31;
        let nlb = (if g.mixed_block_flag != 0 { 2 } else { 0 }) << (h.my_sample_rate() == 2) as u32;
        let base = 576 * ch;
        if g.n_short_sfb != 0 {
            aa_bands = nlb as i32 - 1;
            let sfb = g.sfbtab.get(g.n_long_sfb as usize..).unwrap_or(&[]);
            reorder(&mut w.grbuf[..], base + nlb * 18, &mut w.syn[..], sfb);
        }
        antialias(&mut w.grbuf[..], base, aa_bands);
        imdct_gr(&mut w.grbuf[..], base, &mut overlap[ch], g.block_type, nlb);
        change_sign(&mut w.grbuf[..], base);
    }
}
