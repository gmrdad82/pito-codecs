#![allow(dead_code)]

#[path = "../../tests/fixtures/alac/pcm.rs"]
pub mod pcm;

pub const AREA: &str = "alac";
pub const FRAME: usize = 4096;
pub const PB: u8 = 40;
pub const MB: u8 = 10;
pub const KB: u8 = 14;
pub const DEN: u32 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Iso,
    QtWave,
    QtV2,
    Matroska,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub order: usize,
    pub mix_res: i8,
    pub shift: bool,
    pub verbatim: bool,
}

pub const fn opts(order: usize, mix_res: i8, shift: bool) -> Options {
    Options {
        order,
        mix_res,
        shift,
        verbatim: false,
    }
}

pub struct Fixture {
    pub name: &'static str,
    pub channels: usize,
    pub depth: u32,
    pub rate: u32,
    pub frames: usize,
    pub salt: u64,
    pub hash: u64,
    pub entry: Entry,
    pub opts: Options,
}

#[allow(clippy::too_many_arguments)]
const fn fixture(
    name: &'static str,
    channels: usize,
    depth: u32,
    rate: u32,
    frames: usize,
    salt: u64,
    hash: u64,
    entry: Entry,
    opts: Options,
) -> Fixture {
    Fixture {
        name,
        channels,
        depth,
        rate,
        frames,
        salt,
        hash,
        entry,
        opts,
    }
}

pub const FIXTURES: [Fixture; 10] = [
    fixture(
        "s16-mono.m4a",
        1,
        16,
        44_100,
        5097,
        1,
        0x38fe_2ad8_37cb_f5bf,
        Entry::Iso,
        opts(30, 0, false),
    ),
    fixture(
        "s16-stereo.m4a",
        2,
        16,
        44_100,
        5097,
        4,
        0xfaf2_b693_2497_8db4,
        Entry::Iso,
        opts(16, 2, false),
    ),
    fixture(
        "s24-stereo.mov",
        2,
        24,
        48_000,
        5097,
        3,
        0xd4e8_8479_0573_c49b,
        Entry::QtWave,
        opts(12, 3, true),
    ),
    fixture(
        "s24-stereo-96k.mov",
        2,
        24,
        96_000,
        5097,
        4,
        0xfc82_224f_f7b5_6653,
        Entry::QtV2,
        opts(8, 1, true),
    ),
    fixture(
        "s24-mono-96k.m4a",
        1,
        24,
        96_000,
        5097,
        5,
        0x819b_20c9_6df9_ec7a,
        Entry::Iso,
        opts(6, 0, true),
    ),
    fixture(
        "s16-stereo.mkv",
        2,
        16,
        48_000,
        5097,
        6,
        0x24d4_c6d2_e649_9652,
        Entry::Matroska,
        opts(4, 0, false),
    ),
    fixture(
        "s24-stereo-order8.mkv",
        2,
        24,
        48_000,
        5097,
        7,
        0xc445_2612_2ddd_f212,
        Entry::Matroska,
        opts(8, 2, true),
    ),
    fixture(
        "s16-stereo-order4.m4a",
        2,
        16,
        44_100,
        5097,
        8,
        0xa7b4_ec3e_9b75_6457,
        Entry::Iso,
        opts(4, 2, false),
    ),
    fixture(
        "s16-5.1.m4a",
        6,
        16,
        48_000,
        1501,
        9,
        0xc488_8270_2336_762b,
        Entry::Iso,
        opts(8, 2, false),
    ),
    fixture(
        "s24-mono-verbatim.mov",
        1,
        24,
        44_100,
        2000,
        10,
        0xe111_114e_9b0b_5ba7,
        Entry::QtWave,
        Options {
            verbatim: true,
            ..opts(0, 0, false)
        },
    ),
];

pub fn named(name: &str) -> &'static Fixture {
    FIXTURES
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no ALAC fixture {name}"))
}

impl Fixture {
    pub fn source(&self) -> Vec<i32> {
        pcm::source(self.channels, self.depth, self.frames, self.salt)
    }
}

pub const ALAC_ORDER: [&[usize]; 8] = [
    &[0],
    &[0, 1],
    &[2, 0, 1],
    &[2, 0, 1, 3],
    &[2, 0, 1, 3, 4],
    &[2, 0, 1, 4, 5, 3],
    &[2, 0, 1, 4, 5, 6, 3],
    &[2, 6, 7, 0, 1, 4, 5, 3],
];

const ELEMENTS: [&[usize]; 8] = [
    &[1],
    &[2],
    &[1, 2],
    &[1, 2, 1],
    &[1, 2, 2],
    &[1, 2, 2, 1],
    &[1, 2, 2, 1, 1],
    &[1, 2, 2, 2, 1],
];

pub fn in_alac_order(samples: &[i32], channels: usize) -> Vec<i32> {
    let order = ALAC_ORDER[channels - 1];
    samples
        .chunks_exact(channels)
        .flat_map(|f| order.iter().map(move |&c| f[c]))
        .collect()
}

pub fn cookie(channels: usize, depth: u32, rate: u32, max_frame: u32) -> Vec<u8> {
    let mut c = (FRAME as u32).to_be_bytes().to_vec();
    c.extend_from_slice(&[0, depth as u8, PB, MB, KB, channels as u8]);
    c.extend_from_slice(&255u16.to_be_bytes());
    c.extend_from_slice(&max_frame.to_be_bytes());
    c.extend_from_slice(&0u32.to_be_bytes());
    c.extend_from_slice(&rate.to_be_bytes());
    c
}

#[derive(Default)]
pub struct Writer {
    bytes: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Writer {
    pub fn put(&mut self, bits: u32, v: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | u64::from((v >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                self.bytes.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    pub fn ones(&mut self, n: u32) {
        for _ in 0..n {
            self.put(1, 1);
        }
    }

    pub fn len(&self) -> usize {
        self.bytes.len() * 8 + self.n as usize
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put(pad, 0);
        }
        self.bytes
    }
}

fn sext(v: i32, bits: u32) -> i32 {
    let s = 32 - bits;
    v.wrapping_shl(s).wrapping_shr(s)
}

fn lpc(x: &[i32], order: usize) -> Vec<i16> {
    let mut r = vec![0f64; order + 1];
    for (lag, rv) in r.iter_mut().enumerate() {
        *rv = x
            .iter()
            .zip(x.iter().skip(lag))
            .map(|(&a, &b)| f64::from(a) * f64::from(b))
            .sum();
    }
    let mut a = vec![0f64; order];
    let mut e = r[0];
    if e <= 0.0 {
        return vec![0; order];
    }
    for i in 0..order {
        let mut k = r[i + 1];
        for j in 0..i {
            k -= a[j] * r[i - j];
        }
        k /= e;
        let prev = a.clone();
        a[i] = k;
        for j in 0..i {
            a[j] = prev[j] - k * prev[i - 1 - j];
        }
        e *= 1.0 - k * k;
        if e <= 0.0 {
            break;
        }
    }
    a.iter()
        .map(|&v| {
            (v * f64::from(1u32 << DEN))
                .round()
                .clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

pub fn pc_block(x: &[i32], coefs: &mut [i16], order: usize, bits: u32) -> Vec<i32> {
    let n = x.len();
    let mut pc = vec![0i32; n];
    if n == 0 {
        return pc;
    }
    pc[0] = x[0];
    if order == 0 {
        pc.copy_from_slice(x);
        return pc;
    }
    let first = if order == 31 { n - 1 } else { order.min(n - 1) };
    for j in 1..=first {
        pc[j] = sext(x[j].wrapping_sub(x[j - 1]), bits);
    }
    if order == 31 {
        return pc;
    }
    let denhalf = 1i32 << (DEN - 1);
    let lim = order + 1;
    for j in lim..n {
        let top = x[j - lim];
        let mut sum1 = 0i32;
        for (k, &c) in coefs[..order].iter().enumerate() {
            sum1 = sum1.wrapping_add((c as i32).wrapping_mul(x[j - 1 - k].wrapping_sub(top)));
        }
        let pred = top.wrapping_add(sum1.wrapping_add(denhalf).wrapping_shr(DEN));
        let del = sext(x[j].wrapping_sub(pred), bits);
        pc[j] = del;
        let mut del0 = del;
        if del > 0 {
            for k in (0..order).rev() {
                let dd = top.wrapping_sub(x[j - 1 - k]);
                let sgn = dd.signum();
                coefs[k] = coefs[k].wrapping_sub(sgn as i16);
                del0 = del0.wrapping_sub(
                    ((order - k) as i32).wrapping_mul(sgn.wrapping_mul(dd).wrapping_shr(DEN)),
                );
                if del0 <= 0 {
                    break;
                }
            }
        } else if del < 0 {
            for k in (0..order).rev() {
                let dd = top.wrapping_sub(x[j - 1 - k]);
                let sgn = dd.signum();
                coefs[k] = coefs[k].wrapping_add(sgn as i16);
                del0 = del0.wrapping_sub(
                    ((order - k) as i32)
                        .wrapping_mul(sgn.wrapping_neg().wrapping_mul(dd).wrapping_shr(DEN)),
                );
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
    pc
}

fn lg3a(x: u32) -> u32 {
    31u32.wrapping_sub(x.wrapping_add(3).leading_zeros())
}

fn code(w: &mut Writer, x: u32, m: u32, k: u32, escape_bits: u32) {
    let (q, r) = match x.checked_div(m) {
        Some(q) => (q, x % m),
        None => (9, 0),
    };
    if q >= 9 {
        w.ones(9);
        w.put(escape_bits, x);
        return;
    }
    w.ones(q);
    w.put(1, 0);
    if k > 1 {
        if r == 0 {
            w.put(k - 1, 0);
        } else {
            w.put(k, r + 1);
        }
    }
}

pub fn ag_encode(w: &mut Writer, pc: &[i32], bits: u32) {
    let pb = u32::from(PB);
    let kb = u32::from(KB);
    let wb = (1u32 << kb) - 1;
    let mut mb = u32::from(MB);
    let mut zmode = 0u32;
    let mut c = 0;
    let num = pc.len();
    while c < num {
        let k = lg3a(mb >> 9).min(kb);
        let m = (1u32 << k) - 1;
        let v = pc[c];
        let ndecode = if v >= 0 {
            (v as u32) << 1
        } else {
            (v.unsigned_abs() << 1) - 1
        };
        let n = ndecode.wrapping_sub(zmode);
        code(w, n, m, k, bits);
        c += 1;
        mb = pb
            .wrapping_mul(n.wrapping_add(zmode))
            .wrapping_add(mb)
            .wrapping_sub(pb.wrapping_mul(mb) >> 9);
        if ndecode > 0xffff {
            mb = 0xffff;
        }
        zmode = 0;
        if (mb << 2) < 512 && c < num {
            zmode = 1;
            let k = mb
                .leading_zeros()
                .wrapping_sub(24)
                .wrapping_add(mb.wrapping_add(16) >> 6);
            let mz = ((1u32 << k) - 1) & wb;
            let run = pc[c..].iter().take_while(|&&s| s == 0).count().min(65_535);
            code(w, run as u32, mz, k, 16);
            c += run;
            if run >= 65_535 {
                zmode = 0;
            }
            mb = 0;
        }
    }
}

fn predictor(w: &mut Writer, coefs: &[i16], order: usize) {
    w.put(4, 0);
    w.put(4, DEN);
    w.put(3, 4);
    w.put(5, order as u32);
    for &c in &coefs[..order.min(coefs.len())] {
        w.put(16, c as u16 as u32);
    }
}

fn element(w: &mut Writer, tag: u32, instance: u32, chans: &[&[i32]], depth: u32, o: &Options) {
    let n = chans[0].len();
    let shift = if o.shift && depth >= 24 && !o.verbatim {
        (depth - 16) / 8
    } else {
        0
    };
    let partial = n != FRAME;
    w.put(3, tag);
    w.put(4, instance);
    w.put(12, 0);
    w.put(1, u32::from(partial));
    w.put(2, shift);
    w.put(1, u32::from(o.verbatim));
    if partial {
        w.put(32, n as u32);
    }
    if o.verbatim {
        for i in 0..n {
            for ch in chans {
                w.put(depth, ch[i] as u32 & mask(depth));
            }
        }
        return;
    }
    let sh = shift * 8;
    let highs: Vec<Vec<i32>> = chans
        .iter()
        .map(|c| c.iter().map(|&s| s >> sh).collect())
        .collect();
    let pair = chans.len() == 2;
    let (mix_bits, mix_res) = if pair && o.mix_res != 0 {
        (2u32, i32::from(o.mix_res))
    } else {
        (0, 0)
    };
    let streams: Vec<Vec<i32>> = if mix_res != 0 {
        let (l, r) = (&highs[0], &highs[1]);
        let v: Vec<i32> = l.iter().zip(r).map(|(&a, &b)| a.wrapping_sub(b)).collect();
        let u = r
            .iter()
            .zip(&v)
            .map(|(&b, &d)| b.wrapping_add(mix_res.wrapping_mul(d).wrapping_shr(mix_bits)))
            .collect();
        vec![u, v]
    } else {
        highs
    };
    let bits = depth - sh + u32::from(pair);
    w.put(8, mix_bits);
    w.put(8, mix_res as u8 as u32);
    let mut coefs = Vec::new();
    for s in &streams {
        let c = lpc(
            &s.iter().map(|&v| v >> 4).collect::<Vec<_>>(),
            o.order.min(30),
        );
        let c = if o.order == 31 { vec![0; 31] } else { c };
        predictor(w, &c, o.order);
        coefs.push(c);
    }
    if sh > 0 {
        for i in 0..n {
            for ch in chans {
                w.put(sh, ch[i] as u32 & mask(sh));
            }
        }
    }
    for (s, c) in streams.iter().zip(coefs.iter_mut()) {
        let pc = pc_block(s, c, o.order, bits);
        ag_encode(w, &pc, bits);
    }
}

fn mask(bits: u32) -> u32 {
    if bits >= 32 {
        u32::MAX
    } else {
        (1u32 << bits) - 1
    }
}

pub fn encode_frame(samples: &[i32], channels: usize, depth: u32, o: &Options) -> Vec<u8> {
    let n = samples.len() / channels;
    let planes: Vec<Vec<i32>> = (0..channels)
        .map(|c| (0..n).map(|i| samples[i * channels + c]).collect())
        .collect();
    let mut w = Writer::default();
    let mut at = 0;
    for (instance, &width) in ELEMENTS[channels - 1].iter().enumerate() {
        let chans: Vec<&[i32]> = planes[at..at + width].iter().map(Vec::as_slice).collect();
        element(
            &mut w,
            (width - 1) as u32,
            instance as u32,
            &chans,
            depth,
            o,
        );
        at += width;
    }
    w.put(3, 7);
    w.finish()
}

pub fn encode(wave: &[i32], channels: usize, depth: u32, o: &Options) -> Vec<Vec<u8>> {
    let alac = in_alac_order(wave, channels);
    alac.chunks(FRAME * channels)
        .map(|f| encode_frame(f, channels, depth, o))
        .collect()
}

fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    b.extend_from_slice(kind);
    b.extend_from_slice(body);
    b
}

pub fn cookie_atom(cookie: &[u8]) -> Vec<u8> {
    let mut body = vec![0u8; 4];
    body.extend_from_slice(cookie);
    atom(b"alac", &body)
}

pub fn sample_entry(
    entry: Entry,
    channels: usize,
    depth: u32,
    rate: u32,
    cookie: &[u8],
) -> Vec<u8> {
    let mut e = vec![0u8; 6];
    e.extend_from_slice(&1u16.to_be_bytes());
    let version: u16 = match entry {
        Entry::QtWave => 1,
        Entry::QtV2 => 2,
        _ => 0,
    };
    e.extend_from_slice(&version.to_be_bytes());
    e.extend_from_slice(&[0; 6]);
    if entry == Entry::QtV2 {
        e.extend_from_slice(&3u16.to_be_bytes());
        e.extend_from_slice(&16u16.to_be_bytes());
        e.extend_from_slice(&(-2i16).to_be_bytes());
        e.extend_from_slice(&0u16.to_be_bytes());
        e.extend_from_slice(&65536u32.to_be_bytes());
        e.extend_from_slice(&72u32.to_be_bytes());
        e.extend_from_slice(&f64::from(rate).to_bits().to_be_bytes());
        e.extend_from_slice(&(channels as u32).to_be_bytes());
        e.extend_from_slice(&0x7f00_0000u32.to_be_bytes());
        e.extend_from_slice(&0u32.to_be_bytes());
        let flag = match depth {
            16 => 1u32,
            20 => 2,
            24 => 3,
            _ => 4,
        };
        e.extend_from_slice(&flag.to_be_bytes());
        e.extend_from_slice(&0u32.to_be_bytes());
        e.extend_from_slice(&(FRAME as u32).to_be_bytes());
        e.extend(cookie_atom(cookie));
        return e;
    }
    e.extend_from_slice(&(channels as u16).to_be_bytes());
    e.extend_from_slice(&(depth as u16).to_be_bytes());
    e.extend_from_slice(&[0; 4]);
    e.extend_from_slice(&rate.wrapping_shl(16).to_be_bytes());
    if entry == Entry::QtWave {
        e.extend_from_slice(&(FRAME as u32).to_be_bytes());
        e.extend_from_slice(&0u32.to_be_bytes());
        e.extend_from_slice(&0u32.to_be_bytes());
        e.extend_from_slice(&2u32.to_be_bytes());
        let mut wave = atom(b"frma", b"alac");
        wave.extend(cookie_atom(cookie));
        wave.extend_from_slice(&[0, 0, 0, 8, 0, 0, 0, 0]);
        e.extend(atom(b"wave", &wave));
        return e;
    }
    e.extend(cookie_atom(cookie));
    e
}

pub fn max_frame(packets: &[Vec<u8>]) -> u32 {
    packets.iter().map(|p| p.len() as u32).max().unwrap_or(0)
}
