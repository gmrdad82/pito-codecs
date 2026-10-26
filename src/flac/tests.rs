use super::synth::{self, FIXTURES, Fixture};
use super::*;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "refs.rs"]
mod refs;

type HandCase = (Vec<(Sub, u32)>, Vec<Vec<i32>>);

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-flac");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tracked() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/flac")
}

fn has(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn need(tools: &[&str], test: &str) -> bool {
    let missing: Vec<&str> = tools.iter().copied().filter(|t| !has(t)).collect();
    if missing.is_empty() {
        return true;
    }
    eprintln!("skipped {test}: {} not on PATH", missing.join(" and "));
    false
}

fn run(tool: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(tool).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{tool} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn load(f: &Fixture, ext: &str) -> Vec<u8> {
    let path = tracked().join(format!("{}.{ext}", f.name));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn expected_hash(name: &str) -> u64 {
    let text = std::fs::read_to_string(tracked().join("expected.txt")).unwrap();
    let line = text
        .lines()
        .find(|l| l.split_whitespace().next() == Some(name))
        .unwrap_or_else(|| panic!("no expected hash for {name}"));
    u64::from_str_radix(line.split_whitespace().nth(1).unwrap(), 16).unwrap()
}

fn scaled(ints: &[i32], bits: u32) -> Vec<f32> {
    let k = (1u32 << (bits - 1)) as f32;
    ints.iter().map(|&s| s as f32 / k).collect()
}

struct Case {
    name: &'static str,
    rate: u32,
    channels: usize,
    bits: u32,
    frames: usize,
    flac: &'static [&'static str],
}

const CASES: [Case; 10] = [
    Case {
        name: "s16_stereo_44k",
        rate: 44100,
        channels: 2,
        bits: 16,
        frames: 145_530,
        flac: &["-5"],
    },
    Case {
        name: "s24_stereo_48k",
        rate: 48000,
        channels: 2,
        bits: 24,
        frames: 129_600,
        flac: &["-8", "-m"],
    },
    Case {
        name: "s16_mono_32k_fixed",
        rate: 32000,
        channels: 1,
        bits: 16,
        frames: 67_200,
        flac: &["-0"],
    },
    Case {
        name: "s24_mono_96k_exhaustive",
        rate: 96000,
        channels: 1,
        bits: 24,
        frames: 124_800,
        flac: &["-8", "-e", "-p"],
    },
    Case {
        name: "s16_odd_block_lpc32",
        rate: 44100,
        channels: 2,
        bits: 16,
        frames: 90_405,
        flac: &["--lax", "-b", "1001", "-l", "32"],
    },
    Case {
        name: "s16_11025_block577_independent",
        rate: 11025,
        channels: 2,
        bits: 16,
        frames: 18_742,
        flac: &["--lax", "-b", "577", "--no-mid-side"],
    },
    Case {
        name: "u8_mono_8k",
        rate: 8000,
        channels: 1,
        bits: 8,
        frames: 15_200,
        flac: &["-5"],
    },
    Case {
        name: "s24_six_channels",
        rate: 48000,
        channels: 6,
        bits: 24,
        frames: 52_800,
        flac: &["-5"],
    },
    Case {
        name: "s24_88k_rice_order_8",
        rate: 88200,
        channels: 2,
        bits: 24,
        frames: 105_840,
        flac: &["--lax", "-r", "8", "-l", "16", "-q", "15"],
    },
    Case {
        name: "s16_22k_16384_block",
        rate: 22050,
        channels: 2,
        bits: 16,
        frames: 52_920,
        flac: &["--lax", "-b", "16384"],
    },
];

fn wav_file(name: &str, pcm: &[i32], rate: u32, channels: usize, bits: u32) -> PathBuf {
    let path = dir().join(format!("{name}.wav"));
    std::fs::write(&path, synth::wav(pcm, rate, channels, bits)).unwrap();
    path
}

fn encode(wav: &Path, name: &str, opts: &[&str]) -> PathBuf {
    let out = dir().join(format!("{name}.flac"));
    let mut args = vec!["--silent", "-f"];
    args.extend_from_slice(opts);
    args.extend(["-o", out.to_str().unwrap(), wav.to_str().unwrap()]);
    run("flac", &args);
    out
}

fn reference(flac: &Path, bits: u32) -> Vec<i32> {
    let raw = run(
        "flac",
        &[
            "-d",
            "-c",
            "--silent",
            "--force-raw-format",
            "--endian=little",
            "--sign=signed",
            flac.to_str().unwrap(),
        ],
    );
    match bits.div_ceil(8) {
        1 => raw.iter().map(|&b| b as i8 as i32).collect(),
        2 => raw
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as i32)
            .collect(),
        3 => raw
            .chunks_exact(3)
            .map(|c| i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8)
            .collect(),
        _ => raw
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    }
}

fn ours(data: &[u8]) -> Result<Vec<i32>> {
    let mut r = Reader::new(Cursor::new(data))?;
    let mut out = Vec::new();
    while r.read_i32(&mut out)? > 0 {}
    Ok(out)
}

fn first_difference(a: &[i32], b: &[i32]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
}

fn source(c: &Case, seed: u32) -> Vec<i32> {
    synth::pcm(seed, c.frames, c.channels, c.bits)
}

fn fixture(c: &Case, tag: &str) -> (PathBuf, Vec<i32>) {
    let pcm = source(c, 11);
    let name = format!("{tag}_{}", c.name);
    let w = wav_file(&name, &pcm, c.rate, c.channels, c.bits);
    let f = encode(&w, &name, c.flac);
    let want = reference(&f, c.bits);
    assert_eq!(first_difference(&want, &pcm), None, "{name}: flac -d");
    assert_eq!(want.len(), pcm.len(), "{name}: flac -d");
    (f, want)
}

#[test]
fn the_expected_hashes_cover_every_tracked_fixture() {
    let text = std::fs::read_to_string(tracked().join("expected.txt")).unwrap();
    assert_eq!(text.lines().count(), FIXTURES.len());
    for f in &FIXTURES {
        assert_eq!(synth::fnv(&f.pcm()), expected_hash(f.name), "{}", f.name);
    }
}

#[test]
fn tracked_fixtures_decode_bit_exact() {
    for f in &FIXTURES {
        let data = load(f, "flac");
        let r = Reader::new(Cursor::new(&data)).unwrap();
        assert_eq!(r.info().rate, f.rate, "{}", f.name);
        assert_eq!(r.info().channels as usize, f.channels, "{}", f.name);
        assert_eq!(r.info().bits as u32, f.bits, "{}", f.name);
        assert_eq!(r.info().samples, f.frames as u64, "{}", f.name);
        let got = ours(&data).unwrap();
        let want = f.pcm();
        assert_eq!(got.len(), want.len(), "{}", f.name);
        assert_eq!(first_difference(&got, &want), None, "{}", f.name);
        assert_eq!(synth::fnv(&got), expected_hash(f.name), "{}", f.name);
    }
}

#[test]
fn the_wasted_bits_fixture_really_has_wasted_bits() {
    let f = &FIXTURES[3];
    assert_eq!(f.shift, 8);
    let got = ours(&load(f, "flac")).unwrap();
    assert!(got.iter().all(|s| s & 0xff == 0));
    assert!(got.iter().any(|&s| s != 0));
}

#[test]
fn float_output_is_the_integer_output_scaled() {
    for f in &FIXTURES {
        let b = decode_file(&load(f, "flac")).unwrap();
        assert_eq!(b.rate, f.rate, "{}", f.name);
        assert_eq!(b.channels as usize, f.channels, "{}", f.name);
        let want = scaled(&f.pcm(), f.bits);
        assert_eq!(b.samples.len(), want.len(), "{}", f.name);
        assert!(b.samples == want, "{}", f.name);
        assert!(b.samples.iter().all(|s| (-1.0..1.0).contains(s)));
    }
}

fn read_from(data: &[u8], at: u64, n: usize) -> Vec<i32> {
    let mut r = Reader::new(Cursor::new(data)).unwrap();
    r.seek(at).unwrap();
    let mut out = Vec::new();
    while out.len() < n && r.read_i32(&mut out).unwrap() > 0 {}
    out.truncate(n);
    out
}

fn seek_targets(total: u64, block: u64) -> Vec<u64> {
    let mut t = vec![
        0,
        1,
        block - 1,
        block,
        block + 1,
        3 * block + 17,
        total / 3,
        total / 2 + 5,
        total.saturating_sub(block),
        total - 1,
    ];
    t.retain(|&x| x < total);
    t
}

fn check_seeks(data: &[u8], want: &[i32], ch: usize, block: u64, name: &str) {
    let total = (want.len() / ch) as u64;
    for at in seek_targets(total, block) {
        let n = 3000.min((total - at) as usize) * ch;
        let got = read_from(data, at, n);
        let from = at as usize * ch;
        assert_eq!(got.len(), n, "{name}: seek to {at}");
        assert_eq!(got, want[from..from + n], "{name}: seek to {at}");
    }
    let mut r = Reader::new(Cursor::new(data)).unwrap();
    r.seek(total).unwrap();
    let mut out = Vec::new();
    assert_eq!(r.read_i32(&mut out).unwrap(), 0, "{name}: seek to the end");
    r.seek(total + 100_000).unwrap();
    assert_eq!(
        r.read_i32(&mut out).unwrap(),
        0,
        "{name}: seek past the end"
    );
    r.seek(0).unwrap();
    assert!(
        r.read_i32(&mut out).unwrap() > 0,
        "{name}: seek back to the start"
    );
    assert_eq!(out[..], want[..out.len()], "{name}: seek back to the start");
}

#[test]
fn tracked_fixtures_seek_to_the_exact_sample() {
    for f in [&FIXTURES[0], &FIXTURES[2], &FIXTURES[5], &FIXTURES[6]] {
        check_seeks(&load(f, "flac"), &f.pcm(), f.channels, f.block, f.name);
    }
}

#[test]
fn an_unknown_length_is_found_by_scanning_the_tail() {
    let f = &FIXTURES[0];
    let mut data = load(f, "flac");
    assert_eq!(&data[..4], b"fLaC");
    data[21] &= 0xf0;
    data[22..26].fill(0);
    let want = f.pcm();
    let mut r = Reader::new(Cursor::new(&data)).unwrap();
    assert_eq!(r.info().samples, 0);
    assert_eq!(r.total().unwrap(), f.frames as u64);
    assert_eq!(ours(&data).unwrap(), want);
}

#[test]
fn an_id3_tag_before_the_stream_is_skipped() {
    let f = &FIXTURES[4];
    let mut data = b"ID3\x03\x00\x00\x00\x00\x01\x05".to_vec();
    data.extend(std::iter::repeat_n(0u8, 133));
    data.extend_from_slice(&load(f, "flac"));
    assert_eq!(ours(&data).unwrap(), f.pcm());
}

#[test]
fn a_cut_stream_returns_every_whole_frame_before_the_cut() {
    let f = &FIXTURES[2];
    let data = load(f, "flac");
    let want = f.pcm();
    for cut in [0, 3, 4, 7, 20, 41, 42] {
        assert!(
            Reader::new(Cursor::new(&data[..cut])).is_err(),
            "cut at {cut}"
        );
    }
    let first = Reader::new(Cursor::new(&data)).unwrap().first as usize;
    let frame = f.block as usize * f.channels;
    for cut in [
        first + 1,
        first + 5,
        data.len() / 2,
        data.len() - 3,
        data.len() - 1,
    ] {
        let part = &data[..cut];
        let got = ours(part).unwrap_or_else(|e| panic!("cut at {cut}: {e}"));
        assert!(got.len() < want.len(), "cut at {cut}");
        assert_eq!(got.len() % frame, 0, "cut at {cut}");
        assert_eq!(got[..], want[..got.len()], "cut at {cut}");
        let b = decode_file(part).unwrap_or_else(|e| panic!("cut at {cut}: {e}"));
        assert_eq!(b.samples, scaled(&got, f.bits), "cut at {cut}");
    }
    let whole = (f.frames / f.block as usize) * frame;
    assert_eq!(ours(&data[..data.len() - 1]).unwrap().len(), whole);
    assert!(ours(&data[..first + 5]).unwrap().is_empty());
}

#[test]
fn damaged_frames_are_refused_not_decoded() {
    let f = &FIXTURES[2];
    let data = load(f, "flac");
    let start = Reader::new(Cursor::new(&data)).unwrap().first as usize;
    for at in [
        start + 2,
        start + 9,
        start + 400,
        data.len() / 2,
        data.len() - 1,
    ] {
        let mut bad = data.clone();
        bad[at] ^= 0x10;
        assert!(decode_file(&bad).is_err(), "flip at {at}");
    }
}

#[test]
fn container_packets_decode_through_the_codec_config() {
    let f = &FIXTURES[1];
    let data = load(f, "flac");
    let want = f.pcm();
    let r = Reader::new(Cursor::new(&data)).unwrap();
    let si = meta::streaminfo_bytes(r.info());
    let mut private = b"fLaC\x80\x00\x00\x22".to_vec();
    private.extend_from_slice(&si);
    let mut dec = Decoder::from_config(&private).unwrap();
    assert_eq!(dec.rate(), f.rate);
    assert_eq!(dec.channels(), f.channels as u16);
    let mut at = r.first as usize;
    let mut got = Vec::new();
    let mut pcm = Vec::new();
    while at < data.len() {
        let fr = dec.decode_frame(&data[at..]).unwrap();
        dec.interleave_i32(&mut got);
        dec.decode_packet(&data[at..at + fr.bytes], &mut pcm)
            .unwrap();
        assert_eq!(pcm.len(), fr.block_size * f.channels);
        at += fr.bytes;
    }
    assert_eq!(got, want);
    assert!(
        dec.decode_packet(&data[r.first as usize..r.first as usize + 9], &mut pcm)
            .is_err()
    );
    assert!(pcm.is_empty());
}

#[test]
fn decodes_bit_exact_against_the_reference_decoder() {
    if !need(&["flac"], "decodes_bit_exact_against_the_reference_decoder") {
        return;
    }
    for c in &CASES {
        let (f, want) = fixture(c, "exact");
        let data = std::fs::read(&f).unwrap();
        let r = Reader::new(Cursor::new(&data)).unwrap();
        assert_eq!(r.info().rate, c.rate, "{}", c.name);
        assert_eq!(r.info().channels as usize, c.channels, "{}", c.name);
        assert_eq!(r.info().bits as u32, c.bits, "{}", c.name);
        let got = ours(&data).unwrap();
        assert_eq!(got.len(), want.len(), "{}", c.name);
        assert_eq!(first_difference(&got, &want), None, "{}", c.name);
        let b = decode_file(&data).unwrap();
        assert!(b.samples == scaled(&want, c.bits), "{}", c.name);
    }
}

#[test]
fn tracked_fixtures_remade_from_their_source_still_decode_bit_exact() {
    if !need(
        &["flac"],
        "tracked_fixtures_remade_from_their_source_still_decode_bit_exact",
    ) {
        return;
    }
    for f in &FIXTURES {
        let pcm = f.pcm();
        let name = format!("remade_{}", f.name);
        let w = wav_file(&name, &pcm, f.rate, f.channels, f.bits);
        let mut opts = vec!["--no-padding"];
        opts.extend_from_slice(f.flac);
        let made = encode(&w, &name, &opts);
        assert_eq!(reference(&made, f.bits), pcm, "{}", f.name);
        assert_eq!(
            ours(&std::fs::read(&made).unwrap()).unwrap(),
            pcm,
            "{}",
            f.name
        );
    }
}

#[test]
fn wasted_bits_decode_bit_exact() {
    if !need(&["flac"], "wasted_bits_decode_bit_exact") {
        return;
    }
    let c = &CASES[0];
    let pcm: Vec<i32> = source(c, 12).iter().map(|v| v << 8).collect();
    let w = wav_file("wasted24", &pcm, c.rate, c.channels, 24);
    let f = encode(&w, "wasted24", &["-5"]);
    let want = reference(&f, 24);
    assert_eq!(want, pcm);
    let got = ours(&std::fs::read(&f).unwrap()).unwrap();
    assert_eq!(got.len(), want.len());
    assert_eq!(first_difference(&got, &want), None);
}

#[test]
fn thirty_two_bit_streams_are_refused_cleanly() {
    if !need(&["flac"], "thirty_two_bit_streams_are_refused_cleanly") {
        return;
    }
    let pcm = synth::pcm(13, 24_000, 2, 32);
    let w = wav_file("s32", &pcm, 48000, 2, 32);
    let f = encode(&w, "s32", &[]);
    let data = std::fs::read(&f).unwrap();
    assert_eq!(read_info(&f).unwrap().bits, 32);
    assert!(matches!(
        Reader::new(Cursor::new(&data)),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(decode_file(&data), Err(Error::Unsupported(_))));
}

#[test]
fn seeks_land_on_the_exact_sample() {
    if !need(&["flac"], "seeks_land_on_the_exact_sample") {
        return;
    }
    let c = &CASES[0];
    let pcm = source(c, 14);
    let w = wav_file("seek", &pcm, c.rate, c.channels, c.bits);
    for (name, opts, block) in [
        ("seek_table", &["-5", "-S", "0.5s"][..], 4096u64),
        ("no_table", &["-5", "--no-seektable"][..], 4096),
        (
            "odd_block",
            &["--lax", "-b", "1001", "--no-seektable"][..],
            1001,
        ),
    ] {
        let f = encode(&w, name, opts);
        let want = reference(&f, 16);
        assert_eq!(want, pcm, "{name}");
        let data = std::fs::read(&f).unwrap();
        check_seeks(&data, &want, 2, block, name);
    }
}

#[test]
fn bisection_seek_without_a_table_on_a_long_stream() {
    if !need(&["flac"], "bisection_seek_without_a_table_on_a_long_stream") {
        return;
    }
    let mut state = 0x9e37_79b9u32;
    let pcm: Vec<i32> = synth::pcm(15, 882_000, 2, 16)
        .into_iter()
        .map(|v| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (v + (state >> 21) as i32 - 1024).clamp(-32768, 32767)
        })
        .collect();
    let w = wav_file("long", &pcm, 44100, 2, 16);
    let f = encode(&w, "long", &["-5", "--no-seektable"]);
    let want = reference(&f, 16);
    assert_eq!(want, pcm);
    let data = std::fs::read(&f).unwrap();
    assert!(data.len() > 1 << 20);
    check_seeks(&data, &want, 2, 4096, "long");
}

#[derive(Default)]
pub(super) struct Writer {
    bytes: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Writer {
    pub fn put(&mut self, v: u64, bits: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                self.bytes.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    pub fn signed(&mut self, v: i64, bits: u32) {
        self.put(v as u64 & ((1u64 << bits) - 1), bits);
    }

    pub fn rice(&mut self, v: i32, k: u32) {
        let u = ((v << 1) ^ (v >> 31)) as u32;
        for _ in 0..u >> k {
            self.put(0, 1);
        }
        self.put(1, 1);
        self.put((u & ((1u64 << k) - 1) as u32) as u64, k);
    }

    pub fn align(&mut self) {
        while self.n != 0 {
            self.put(0, 1);
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone)]
pub(super) enum Sub {
    Constant(i32),
    Verbatim(Vec<i32>),
    Fixed {
        order: usize,
        samples: Vec<i32>,
        param: Option<u32>,
        partitions: u32,
    },
    Lpc {
        qlp: Vec<i32>,
        precision: u32,
        shift: u32,
        samples: Vec<i32>,
        param: Option<u32>,
    },
}

fn residual_bits(w: &mut Writer, order: usize, res: &[i32], param: Option<u32>, partitions: u32) {
    let rice2 = param.is_some_and(|k| k > 14);
    w.put(rice2 as u64, 2);
    w.put(partitions as u64, 4);
    let block = res.len() + order;
    let per = block >> partitions;
    let mut at = 0;
    for p in 0..1usize << partitions {
        let n = if p == 0 { per - order } else { per };
        let part = &res[at..at + n];
        match param {
            Some(k) => {
                w.put(k as u64, if rice2 { 5 } else { 4 });
                for &r in part {
                    w.rice(r, k);
                }
            }
            None => {
                w.put(if rice2 { 31 } else { 15 }, if rice2 { 5 } else { 4 });
                let raw = part
                    .iter()
                    .map(|&r| 33 - (r ^ (r >> 31)).leading_zeros())
                    .max()
                    .unwrap_or(0);
                w.put(raw as u64, 5);
                for &r in part {
                    if raw > 0 {
                        w.signed(r as i64, raw);
                    }
                }
            }
        }
        at += n;
    }
}

fn fixed_residual(order: usize, s: &[i32]) -> Vec<i32> {
    let mut r = s.to_vec();
    for _ in 0..order {
        for i in (1..r.len()).rev() {
            r[i] -= r[i - 1];
        }
    }
    r[order..].to_vec()
}

pub(super) fn sub_bits(w: &mut Writer, sub: &Sub, bps: u32, wasted: u32) {
    let kind = match sub {
        Sub::Constant(_) => 0,
        Sub::Verbatim(_) => 1,
        Sub::Fixed { order, .. } => 8 + *order as u64,
        Sub::Lpc { qlp, .. } => 32 + qlp.len() as u64 - 1,
    };
    w.put(0, 1);
    w.put(kind, 6);
    if wasted > 0 {
        w.put(1, 1);
        for _ in 0..wasted - 1 {
            w.put(0, 1);
        }
        w.put(1, 1);
    } else {
        w.put(0, 1);
    }
    let bps = bps - wasted;
    match sub {
        Sub::Constant(v) => w.signed(*v as i64, bps),
        Sub::Verbatim(s) => s.iter().for_each(|&v| w.signed(v as i64, bps)),
        Sub::Fixed {
            order,
            samples,
            param,
            partitions,
        } => {
            samples[..*order]
                .iter()
                .for_each(|&v| w.signed(v as i64, bps));
            residual_bits(
                w,
                *order,
                &fixed_residual(*order, samples),
                *param,
                *partitions,
            );
        }
        Sub::Lpc {
            qlp,
            precision,
            shift,
            samples,
            param,
        } => {
            let order = qlp.len();
            samples[..order]
                .iter()
                .for_each(|&v| w.signed(v as i64, bps));
            w.put(*precision as u64 - 1, 4);
            w.signed(*shift as i64, 5);
            qlp.iter().for_each(|&c| w.signed(c as i64, *precision));
            let res: Vec<i32> = (order..samples.len())
                .map(|i| {
                    let p: i64 = qlp
                        .iter()
                        .enumerate()
                        .map(|(j, &c)| c as i64 * samples[i - 1 - j] as i64)
                        .sum();
                    (samples[i] as i64 - (p >> shift)) as i32
                })
                .collect();
            residual_bits(w, order, &res, *param, 0);
        }
    }
}

pub(super) fn frame_bytes(
    number: u64,
    variable: bool,
    block: usize,
    assignment: u8,
    subs: &[(Sub, u32)],
) -> Vec<u8> {
    let bps = 16;
    let mut h = vec![0xff, 0xf8 | variable as u8, 0x79, assignment << 4 | 4 << 1];
    let n = number;
    if n < 0x80 {
        h.push(n as u8);
    } else if n < 0x800 {
        h.extend_from_slice(&[0xc0 | (n >> 6) as u8, 0x80 | (n & 0x3f) as u8]);
    } else if n < 0x10000 {
        h.extend_from_slice(&[
            0xe0 | (n >> 12) as u8,
            0x80 | ((n >> 6) & 0x3f) as u8,
            0x80 | (n & 0x3f) as u8,
        ]);
    } else {
        h.extend_from_slice(&[
            0xf0 | (n >> 18) as u8,
            0x80 | ((n >> 12) & 0x3f) as u8,
            0x80 | ((n >> 6) & 0x3f) as u8,
            0x80 | (n & 0x3f) as u8,
        ]);
    }
    h.extend_from_slice(&((block - 1) as u16).to_be_bytes());
    h.push(super::crc::crc8(&h));
    let mut w = Writer::default();
    for (i, (s, wasted)) in subs.iter().enumerate() {
        let side = matches!((assignment, i), (8, 1) | (9, 0) | (10, 1));
        sub_bits(&mut w, s, bps + side as u32, *wasted);
    }
    w.align();
    h.extend_from_slice(w.bytes());
    let crc = super::crc::crc16(&h);
    h.extend_from_slice(&crc.to_be_bytes());
    h
}

pub(super) fn wave(n: usize, amp: i32, seed: i32) -> Vec<i32> {
    (0..n as i32)
        .map(|i| {
            let x = ((i * 37 + seed * 101) % 211) - 105;
            let y = (i * (7 + seed)) % 63 - 31;
            (x * amp / 105 + y).clamp(-amp, amp)
        })
        .collect()
}

pub(super) fn info16(channels: u8) -> StreamInfo {
    StreamInfo {
        min_block: 16,
        max_block: 4096,
        min_frame: 0,
        max_frame: 0,
        rate: 44_100,
        channels,
        bits: 16,
        samples: 0,
        md5: [0; 16],
    }
}

fn planar(d: &Decoder, ch: usize) -> Vec<Vec<i32>> {
    (0..ch).map(|c| d.channel(c).to_vec()).collect()
}

#[test]
fn hand_built_frames_cover_every_subframe_and_escape() {
    let n = 300;
    let a = wave(n, 30000, 1);
    let b = wave(n, 20000, 2);
    let shifted: Vec<i32> = wave(n, 2000, 3).iter().map(|v| v << 3).collect();
    let cases: Vec<HandCase> = vec![
        (
            vec![(Sub::Constant(-1234), 0), (Sub::Verbatim(b.clone()), 0)],
            vec![vec![-1234; n], b.clone()],
        ),
        (
            vec![
                (
                    Sub::Fixed {
                        order: 2,
                        samples: a.clone(),
                        param: None,
                        partitions: 2,
                    },
                    0,
                ),
                (
                    Sub::Fixed {
                        order: 4,
                        samples: b.clone(),
                        param: Some(12),
                        partitions: 0,
                    },
                    0,
                ),
            ],
            vec![a.clone(), b.clone()],
        ),
        (
            vec![
                (
                    Sub::Lpc {
                        qlp: vec![1900, -900, 31],
                        precision: 12,
                        shift: 10,
                        samples: a.clone(),
                        param: Some(17),
                    },
                    0,
                ),
                (
                    Sub::Fixed {
                        order: 0,
                        samples: shifted.iter().map(|v| v >> 3).collect(),
                        param: Some(9),
                        partitions: 0,
                    },
                    3,
                ),
            ],
            vec![a.clone(), shifted.clone()],
        ),
    ];
    let mut d = Decoder::new(Some(info16(2)));
    for (i, (subs, want)) in cases.into_iter().enumerate() {
        let f = frame_bytes(i as u64, false, n, 1, &subs);
        let got = d.decode_frame(&f).unwrap();
        assert_eq!(got.bytes, f.len(), "case {i}");
        assert_eq!(got.block_size, n);
        assert_eq!(planar(&d, 2), want, "case {i}");
    }
}

#[test]
fn hand_built_stereo_modes_and_variable_numbers() {
    let n = 257;
    let l = wave(n, 30000, 4);
    let r = wave(n, 25000, 5);
    let side: Vec<i32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
    let mid: Vec<i32> = l.iter().zip(&r).map(|(a, b)| (a + b) >> 1).collect();
    let verb = |v: &Vec<i32>| Sub::Verbatim(v.clone());
    let mut d = Decoder::new(None);
    for (number, assignment, subs) in [
        (0u64, 8u8, [verb(&l), verb(&side)]),
        (257, 9, [verb(&side), verb(&r)]),
        (514, 10, [verb(&mid), verb(&side)]),
        (100_000, 1, [verb(&l), verb(&r)]),
    ] {
        let subs: Vec<(Sub, u32)> = subs.into_iter().map(|s| (s, 0)).collect();
        let f = frame_bytes(number, true, n, assignment, &subs);
        let got = d.decode_frame(&f).unwrap();
        assert_eq!(got.sample, number, "assignment {assignment}");
        assert_eq!(got.rate, 44_100);
        assert_eq!(
            planar(&d, 2),
            vec![l.clone(), r.clone()],
            "assignment {assignment}"
        );
        let mut inter = Vec::new();
        d.interleave_i32(&mut inter);
        assert_eq!(inter[..4], [l[0], r[0], l[1], r[1]]);
    }
}

#[test]
fn fixed_frame_numbers_count_in_the_streaminfo_block_size() {
    let n = 4096;
    let s = wave(n, 1000, 6);
    let mut info = info16(1);
    info.min_block = 4096;
    info.max_block = 4096;
    let mut d = Decoder::new(Some(info));
    let f = frame_bytes(5, false, n, 0, &[(Sub::Verbatim(s.clone()), 0)]);
    assert_eq!(d.decode_frame(&f).unwrap().sample, 5 * 4096);
    let short = frame_bytes(6, false, 100, 0, &[(Sub::Verbatim(s[..100].to_vec()), 0)]);
    assert_eq!(d.decode_frame(&short).unwrap().sample, 6 * 4096);
}

#[test]
fn frames_that_break_the_rules_are_refused() {
    let n = 64;
    let s = wave(n, 1000, 7);
    let info = info16(2);
    let good = frame_bytes(
        0,
        false,
        n,
        1,
        &[(Sub::Verbatim(s.clone()), 0), (Sub::Verbatim(s.clone()), 0)],
    );
    let mut d = Decoder::new(Some(info));
    assert!(d.decode_frame(&good).is_ok());
    let mono = frame_bytes(0, false, n, 0, &[(Sub::Verbatim(s.clone()), 0)]);
    assert!(d.decode_frame(&mono).is_err());
    let mut crc = good.clone();
    let last = crc.len() - 1;
    crc[last] ^= 1;
    assert!(d.decode_frame(&crc).is_err());
    let out_of_range = frame_bytes(
        0,
        false,
        n,
        8,
        &[(Sub::Constant(32767), 0), (Sub::Constant(-32768), 0)],
    );
    assert!(matches!(
        d.decode_frame(&out_of_range),
        Err(Error::Invalid(_))
    ));
    let big_order = frame_bytes(
        0,
        false,
        4,
        0,
        &[(
            Sub::Fixed {
                order: 4,
                samples: vec![1, 2, 3, 4],
                param: Some(1),
                partitions: 0,
            },
            0,
        )],
    );
    assert!(Decoder::new(None).decode_frame(&big_order).is_err());
    for cut in 0..good.len() {
        assert!(d.decode_frame(&good[..cut]).is_err(), "cut at {cut}");
    }
}

#[test]
fn a_hand_built_file_reads_seeks_and_totals() {
    let n = 1000;
    let frames = 9;
    let tracks: Vec<Vec<i32>> = (0..2).map(|c| wave(n * frames, 20000, 8 + c)).collect();
    let mut info = info16(2);
    info.min_block = n as u16;
    info.max_block = n as u16;
    info.samples = (n * frames - 333) as u64;
    let mut data = b"fLaC".to_vec();
    data.extend_from_slice(&[0x00, 0, 0, 34]);
    data.extend_from_slice(&meta::streaminfo_bytes(&info));
    data.extend_from_slice(&[0x81, 0, 0, 5, 1, 2, 3, 4, 5]);
    for k in 0..frames {
        let subs: Vec<(Sub, u32)> = tracks
            .iter()
            .map(|t| (Sub::Verbatim(t[k * n..(k + 1) * n].to_vec()), 0))
            .collect();
        data.extend_from_slice(&frame_bytes(k as u64, false, n, 1, &subs));
    }
    data.extend_from_slice(b"TAG trailing junk that is not a frame");
    let want: Vec<i32> = (0..info.samples as usize)
        .flat_map(|i| [tracks[0][i], tracks[1][i]])
        .collect();
    assert_eq!(ours(&data).unwrap(), want);
    let mut r = Reader::new(Cursor::new(&data)).unwrap();
    assert_eq!(r.total().unwrap(), info.samples);
    for at in [0u64, 1, 999, 1000, 4321, info.samples - 1] {
        let got = read_from(&data, at, 50);
        let from = at as usize * 2;
        let end = (from + 50).min(want.len());
        assert_eq!(got, want[from..end], "seek to {at}");
    }
}

fn hand_file(
    sizes: &[usize],
    variable: bool,
    known: bool,
    junk: &[u8],
) -> (Vec<u8>, Vec<i32>, Vec<u64>) {
    let total: usize = sizes.iter().sum();
    let tracks: Vec<Vec<i32>> = (0..2).map(|c| wave(total, 20000, 20 + c)).collect();
    let mut info = info16(2);
    info.min_block = sizes.iter().copied().min().unwrap_or(16) as u16;
    info.max_block = sizes.iter().copied().max().unwrap_or(16) as u16;
    info.samples = if known { total as u64 } else { 0 };
    let mut data = b"fLaC".to_vec();
    data.extend_from_slice(&[0x80, 0, 0, 34]);
    data.extend_from_slice(&meta::streaminfo_bytes(&info));
    let mut starts = Vec::new();
    let mut at = 0;
    for (k, &n) in sizes.iter().enumerate() {
        starts.push(at as u64);
        let subs: Vec<(Sub, u32)> = tracks
            .iter()
            .map(|t| (Sub::Verbatim(t[at..at + n].to_vec()), 0))
            .collect();
        let number = if variable { at as u64 } else { k as u64 };
        data.extend_from_slice(&frame_bytes(number, variable, n, 1, &subs));
        at += n;
    }
    data.extend_from_slice(junk);
    let want = (0..total)
        .flat_map(|i| [tracks[0][i], tracks[1][i]])
        .collect();
    (data, want, starts)
}

#[test]
fn a_variable_block_size_stream_seeks_by_bisection() {
    let sizes: Vec<usize> = [1000, 1777, 513, 4096, 2500, 3001].repeat(7);
    for known in [true, false] {
        let (data, want, starts) = hand_file(&sizes, true, known, b"");
        assert!(data.len() > 300_000);
        let mut r = Reader::new(Cursor::new(&data)).unwrap();
        assert!(r.seek_table().is_empty());
        assert_ne!(r.info().min_block, r.info().max_block);
        let total = (want.len() / 2) as u64;
        assert_eq!(r.total().unwrap(), total, "known {known}");
        let mut targets = vec![0, 1, total - 1];
        for w in starts.windows(2).step_by(5) {
            targets.extend([w[0], w[0] + 1, w[1] - 1, (w[0] + w[1]) / 2]);
        }
        for at in targets {
            let got = read_from(&data, at, 1400);
            let from = at as usize * 2;
            let end = (from + 1400).min(want.len());
            assert_eq!(got, want[from..end], "known {known}: seek to {at}");
        }
        assert_eq!(ours(&data).unwrap(), want, "known {known}");
    }
}

#[test]
fn a_wrong_seek_table_still_lands_on_the_exact_sample() {
    let n = 1000;
    let frames = 40;
    let tracks: Vec<Vec<i32>> = (0..2).map(|c| wave(n * frames, 20000, 30 + c)).collect();
    let mut info = info16(2);
    info.min_block = n as u16;
    info.max_block = n as u16;
    info.samples = (n * frames) as u64;
    let mut body = Vec::new();
    let mut offsets = Vec::new();
    for k in 0..frames {
        offsets.push(body.len() as u64);
        let subs: Vec<(Sub, u32)> = tracks
            .iter()
            .map(|t| (Sub::Verbatim(t[k * n..(k + 1) * n].to_vec()), 0))
            .collect();
        body.extend_from_slice(&frame_bytes(k as u64, false, n, 1, &subs));
    }
    let want: Vec<i32> = (0..n * frames)
        .flat_map(|i| [tracks[0][i], tracks[1][i]])
        .collect();
    let file = |points: &[(u64, u64)]| {
        let mut data = b"fLaC".to_vec();
        data.extend_from_slice(&[0x00, 0, 0, 34]);
        data.extend_from_slice(&meta::streaminfo_bytes(&info));
        let len = 18 * points.len();
        data.extend_from_slice(&[0x83, (len >> 16) as u8, (len >> 8) as u8, len as u8]);
        for &(sample, offset) in points {
            data.extend_from_slice(&sample.to_be_bytes());
            data.extend_from_slice(&offset.to_be_bytes());
            data.extend_from_slice(&(n as u16).to_be_bytes());
        }
        data.extend_from_slice(&body);
        data
    };
    let cases: [(&str, Vec<(u64, u64)>); 6] = [
        (
            "a good table",
            vec![(0, 0), (10_000, offsets[10]), (30_000, offsets[30])],
        ),
        (
            "an offset inside a frame",
            vec![(0, 0), (20_000, offsets[20] + 7)],
        ),
        (
            "a sample number too low",
            vec![(0, 0), (15_000, offsets[25])],
        ),
        (
            "a sample number too high",
            vec![(0, 0), (35_000, offsets[5])],
        ),
        (
            "an offset past the end",
            vec![(20_000, 2 * body.len() as u64)],
        ),
        (
            "every point wrong",
            vec![(1_000, offsets[39] + 3), (2_000, offsets[38]), (3_000, 1)],
        ),
    ];
    for (name, points) in cases {
        let data = file(&points);
        let r = Reader::new(Cursor::new(&data)).unwrap();
        assert_eq!(r.seek_table().len(), points.len(), "{name}");
        for at in [0u64, 1, 999, 1000, 15_000, 20_003, 25_500, 34_999, 39_999] {
            let got = read_from(&data, at, 1400);
            let from = at as usize * 2;
            let end = (from + 1400).min(want.len());
            assert_eq!(got, want[from..end], "{name}: seek to {at}");
        }
        let mut r = Reader::new(Cursor::new(&data)).unwrap();
        r.seek(25_500).unwrap();
        r.seek(0).unwrap();
        let mut out = Vec::new();
        while r.read_i32(&mut out).unwrap() > 0 {}
        assert_eq!(out, want, "{name}: seek back to 0 and read to the end");
        assert_eq!(ours(&data).unwrap(), want, "{name}");
    }
}

#[test]
fn total_keeps_the_read_position_and_a_pending_seek() {
    let (data, want, _) = hand_file(&[1000; 9], false, false, b"");
    let mut r = Reader::new(Cursor::new(&data)).unwrap();
    let mut out = Vec::new();
    assert_eq!(r.read_i32(&mut out).unwrap(), 1000);
    assert_eq!(r.total().unwrap(), 9000);
    while r.read_i32(&mut out).unwrap() > 0 {}
    assert_eq!(out, want);

    let mut r = Reader::new(Cursor::new(&data)).unwrap();
    r.seek(4321).unwrap();
    assert_eq!(r.total().unwrap(), 9000);
    let mut out = Vec::new();
    while r.read_i32(&mut out).unwrap() > 0 {}
    assert_eq!(out, want[4321 * 2..]);

    let mut r = Reader::new(Cursor::new(&data)).unwrap();
    let mut out = Vec::new();
    while r.read_i32(&mut out).unwrap() > 0 {}
    assert_eq!(r.total().unwrap(), 9000);
    assert_eq!(r.read_i32(&mut out).unwrap(), 0);
    assert_eq!(out, want);
}

#[test]
fn trailing_junk_ends_the_stream_but_a_damaged_last_frame_does_not() {
    let mut junk = b"TAG".to_vec();
    junk.resize(128, b' ');
    for known in [false, true] {
        let (data, want, _) = hand_file(&[1000; 5], false, known, &junk);
        assert_eq!(ours(&data).unwrap(), want, "known {known}");
        assert_eq!(
            decode_file(&data).unwrap().samples.len(),
            want.len(),
            "known {known}"
        );
        let mut r = Reader::new(Cursor::new(&data)).unwrap();
        assert_eq!(r.total().unwrap(), 5000, "known {known}");
    }
    let (mut data, _, _) = hand_file(&[1000; 5], false, false, b"");
    data.extend_from_slice(&[0xff, 0xf8, 0x79, 0x18, 0x05, 0x03, 0xe7, 0x00]);
    assert!(ours(&data).is_err());
    let (mut data, _, _) = hand_file(&[1000; 5], false, false, b"");
    let last = data.len() - 1;
    data[last] ^= 1;
    assert!(ours(&data).is_err());
}

#[test]
fn a_frame_that_overruns_into_the_next_is_called_damaged() {
    let grow = |data: &mut Vec<u8>| {
        let first = Reader::new(Cursor::new(&data[..])).unwrap().first as usize;
        data[first + 5..first + 7].copy_from_slice(&3999u16.to_be_bytes());
        data[first + 7] = super::crc::crc8(&data[first..first + 7]);
    };
    let (mut data, _, _) = hand_file(&[1000; 3], false, true, b"");
    grow(&mut data);
    let e = ours(&data).unwrap_err().to_string();
    assert!(e.contains("damaged"), "{e}");
    assert!(!e.contains('\n'), "{e}");
    let (mut alone, _, _) = hand_file(&[1000], false, true, b"");
    grow(&mut alone);
    assert!(ours(&alone).unwrap().is_empty());
}

#[test]
fn float_interleave_matches_the_integers_for_one_and_three_channels() {
    let n = 333;
    for ch in [1usize, 3] {
        let tracks: Vec<Vec<i32>> = (0..ch).map(|c| wave(n, 30000, 30 + c as i32)).collect();
        let subs: Vec<(Sub, u32)> = tracks
            .iter()
            .map(|t| (Sub::Verbatim(t.clone()), 0))
            .collect();
        let mut d = Decoder::new(None);
        d.decode_frame(&frame_bytes(0, false, n, ch as u8 - 1, &subs))
            .unwrap();
        let want: Vec<i32> = (0..n)
            .flat_map(|i| tracks.iter().map(move |t| t[i]))
            .collect();
        let mut ints = Vec::new();
        d.interleave_i32(&mut ints);
        assert_eq!(ints, want, "{ch} channels");
        let mut floats = Vec::new();
        d.interleave_f32(&mut floats);
        assert_eq!(floats, scaled(&want, 16), "{ch} channels");
        let mut part = Vec::new();
        d.put_f32(10, 20, &mut part);
        assert_eq!(part, scaled(&want[10 * ch..20 * ch], 16), "{ch} channels");
    }
}

fn thirty_two_bit_file() -> Vec<u8> {
    let mut info = info16(2);
    info.bits = 32;
    info.rate = 48000;
    info.samples = 48000;
    let mut data = b"fLaC".to_vec();
    data.extend_from_slice(&[0x80, 0, 0, 34]);
    data.extend_from_slice(&meta::streaminfo_bytes(&info));
    data
}

#[test]
fn a_32_bit_header_is_refused_by_the_reader_and_named_by_read_info() {
    let data = thirty_two_bit_file();
    assert!(matches!(
        Reader::new(Cursor::new(&data)),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(decode_file(&data), Err(Error::Unsupported(_))));
    let path = dir().join("hand_s32.flac");
    std::fs::write(&path, &data).unwrap();
    let i = read_info(&path).unwrap();
    assert_eq!(
        (i.bits, i.channels, i.rate, i.samples),
        (32, 2, 48000, 48000)
    );
    let mut private = b"fLaC\x80\x00\x00\x22".to_vec();
    private.extend_from_slice(&data[8..42]);
    assert!(matches!(
        Decoder::from_config(&private),
        Err(Error::Unsupported(_))
    ));
}

#[cfg(any(
    feature = "audio",
    all(
        feature = "h264",
        feature = "hevc",
        feature = "aac",
        feature = "mp4",
        feature = "gpu",
        feature = "filter",
        feature = "mkv",
        feature = "vp9"
    )
))]
mod front {
    use super::*;
    use crate::media::{AudioTrack, Prefs, audio_pcm_range, probe};
    use crate::testref::Refs;

    fn max_diff(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max)
    }

    fn containers() -> Vec<(&'static Fixture, PathBuf)> {
        let mut out = Vec::new();
        let r = Refs::open(refs::AREA);
        for (name, exts) in refs::REMUXED {
            let f = FIXTURES.iter().find(|f| f.name == name).unwrap();
            out.push((f, tracked().join(format!("{name}.flac"))));
            for ext in exts {
                out.push((f, r.path(&refs::remuxed(name, ext))));
            }
        }
        out
    }

    #[test]
    fn probe_names_flac_in_every_tracked_container() {
        let prefs = Prefs::default();
        for (f, path) in containers() {
            let p = probe(&path, &prefs).unwrap();
            let at = path.display();
            assert_eq!(p.audio.len(), 1, "{at}");
            let a = &p.audio[0];
            assert_eq!(a.codec, "flac", "{at}");
            assert!(a.native(), "{at}");
            assert_eq!(a.bits as u32, f.bits, "{at}");
            assert_eq!(a.variant(), format!("{}-bit FLAC", f.bits), "{at}");
            assert_eq!(a.channels as usize, f.channels, "{at}");
            assert_eq!(a.rate, f.rate, "{at}");
            assert!(!p.unsupported().iter().any(|n| n.contains("FLAC")), "{at}");
            let seconds = f.frames as f64 / f.rate as f64;
            assert!(
                (p.duration - seconds).abs() < 0.05,
                "{at}: {} against {seconds}",
                p.duration
            );
        }
    }

    #[test]
    fn ranges_decode_sample_exact_from_every_tracked_container() {
        let prefs = Prefs::default();
        for (f, path) in containers() {
            let want = scaled(&f.pcm(), f.bits);
            let ch = f.channels;
            let rate = f.rate as f64;
            let seconds = f.frames as f64 / rate;
            for (from, to) in [
                (0.0, seconds / 2.0),
                (0.013, seconds * 0.7),
                (seconds / 3.0, seconds - 0.001),
            ] {
                let got = audio_pcm_range(&path, 0, from, to, f.rate, ch as u16, &prefs).unwrap();
                let start = (from * rate).round() as usize * ch;
                let end = (to * rate).round() as usize * ch;
                let at = path.display();
                assert_eq!(got.samples.len(), end - start, "{at} {from}..{to}");
                let d = max_diff(&got.samples, &want[start..end]);
                assert!(d < 1e-6, "{at} {from}..{to}: max difference {d}");
            }
        }
    }

    #[test]
    fn a_cut_flac_file_plays_up_to_the_cut() {
        let f = &FIXTURES[2];
        let data = load(f, "flac");
        let path = dir().join("front_cut.flac");
        std::fs::write(&path, &data[..data.len() - 3]).unwrap();
        let prefs = Prefs::default();
        let ch = f.channels;
        let held = f.frames / f.block as usize * f.block as usize;
        let want = scaled(&f.pcm(), f.bits);
        let all = audio_pcm_range(&path, 0, 0.0, f64::INFINITY, f.rate, ch as u16, &prefs).unwrap();
        assert_eq!(all.samples.len(), held * ch);
        assert!(max_diff(&all.samples, &want[..held * ch]) < 1e-6);
        let from = 0.1;
        let first = (from * f.rate as f64).round() as usize;
        let tail = audio_pcm_range(&path, 0, from, 0.2, f.rate, ch as u16, &prefs).unwrap();
        assert_eq!(tail.samples.len(), (held - first) * ch);
        assert!(max_diff(&tail.samples, &want[first * ch..held * ch]) < 1e-6);
    }

    #[test]
    fn a_damaged_flac_file_fails_with_one_clean_error() {
        let f = &FIXTURES[2];
        let mut data = load(f, "flac");
        let at = data.len() / 2;
        data[at] ^= 0x40;
        let bad = dir().join("front_damaged.flac");
        std::fs::write(&bad, &data).unwrap();
        let prefs = Prefs::default();
        let seconds = f.frames as f64 / f.rate as f64;
        let got = audio_pcm_range(&bad, 0, 0.0, seconds, f.rate, f.channels as u16, &prefs);
        let e = got.unwrap_err().to_string();
        assert!(e.contains("FLAC"), "{e}");
        assert!(!e.contains('\n'), "{e}");
    }

    #[test]
    fn a_32_bit_flac_probes_as_not_native_and_names_the_variant() {
        let path = dir().join("probe_s32.flac");
        std::fs::write(&path, thirty_two_bit_file()).unwrap();
        let prefs = Prefs::default();
        let p = probe(&path, &prefs).unwrap();
        let a = &p.audio[0];
        assert_eq!(a.codec, "flac");
        assert_eq!(a.bits, 32);
        assert!(!a.native());
        assert_eq!(a.variant(), "32-bit FLAC");
        assert!(p.unsupported().iter().any(|n| n == "32-bit FLAC"));
        assert!((p.duration - 1.0).abs() < 1e-9);
        let e = audio_pcm_range(&path, 0, 0.0, 0.5, 48000, 2, &prefs)
            .unwrap_err()
            .to_string();
        assert!(e.contains("32-bit FLAC"), "{e}");
    }

    #[test]
    fn native_follows_the_flac_bit_depth() {
        let mut a = AudioTrack {
            index: 0,
            stream: 0,
            title: None,
            language: None,
            codec: "flac".into(),
            channels: 2,
            rate: 48000,
            bitrate: None,
            default: true,
            start: None,
            bits: 24,
            refused: None,
        };
        assert!(a.native());
        assert_eq!(a.variant(), "24-bit FLAC");
        a.bits = 32;
        assert!(!a.native());
        assert_eq!(a.variant(), "32-bit FLAC");
        a.bits = 0;
        assert!(a.native());
        assert_eq!(a.variant(), "flac");
        a.codec = "aac".into();
        a.bits = 32;
        assert!(a.native());
        assert_eq!(a.variant(), "aac");
        a.codec = "alac".into();
        a.bits = 0;
        assert_eq!(a.native(), cfg!(feature = "alac"));
        assert_eq!(a.variant(), "alac");
        a.refused = Some("ALAC version 1".into());
        assert!(!a.native());
        assert_eq!(a.variant(), "ALAC version 1");
    }

    #[test]
    fn every_remuxed_fixture_holds_the_tracked_frames() {
        let r = Refs::open(refs::AREA);
        for (name, exts) in refs::REMUXED {
            let f = FIXTURES.iter().find(|f| f.name == name).unwrap();
            let want = scaled(&f.pcm(), f.bits);
            for ext in exts {
                let path = r.path(&refs::remuxed(name, ext));
                let got = audio_pcm_range(
                    &path,
                    0,
                    0.0,
                    f64::INFINITY,
                    f.rate,
                    f.channels as u16,
                    &Prefs::default(),
                )
                .unwrap();
                assert_eq!(got.samples.len(), want.len(), "{}", path.display());
                assert!(max_diff(&got.samples, &want) < 1e-6, "{}", path.display());
            }
        }
    }

    fn matroska_of(flac: &[u8], name: &str) -> PathBuf {
        use crate::media::TimeBase;
        use crate::mkv::{TrackConfig, Writer};
        use crate::mp4::TrackKind;
        let path = dir().join(name);
        let mut w = Writer::create(&path).unwrap();
        let track = w
            .add_track(TrackConfig {
                codec_private: flac[..42].to_vec(),
                sample_rate: 48000,
                channels: 2,
                ..TrackConfig::new(TrackKind::Audio, "A_FLAC", TimeBase::new(1, 48000))
            })
            .unwrap();
        w.push_sample(track, &[0xff, 0xf8, 0, 0], 0, Some(4096), true)
            .unwrap();
        w.finish().unwrap();
        path
    }

    #[test]
    fn a_32_bit_flac_in_matroska_probes_as_not_native() {
        let out = matroska_of(&thirty_two_bit_file(), "s32_mkv.mka");
        let p = probe(&out, &Prefs::default()).unwrap();
        let a = &p.audio[0];
        assert_eq!(a.codec, "flac");
        assert_eq!(a.bits, 32);
        assert!(!a.native());
        assert!(p.unsupported().iter().any(|n| n == "32-bit FLAC"));
        let e = audio_pcm_range(&out, 0, 0.0, 0.5, 48000, 2, &Prefs::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("32-bit FLAC"), "{e}");
    }

    #[test]
    fn a_16_bit_flac_in_our_matroska_probes_as_native() {
        let mut info = info16(2);
        info.rate = 48000;
        info.samples = 48000;
        let mut data = b"fLaC".to_vec();
        data.extend_from_slice(&[0x80, 0, 0, 34]);
        data.extend_from_slice(&meta::streaminfo_bytes(&info));
        let out = matroska_of(&data, "s16_mkv.mka");
        let p = probe(&out, &Prefs::default()).unwrap();
        let a = &p.audio[0];
        assert_eq!((a.codec.as_str(), a.bits), ("flac", 16));
        assert!(a.native());
        assert!(!p.unsupported().iter().any(|n| n.contains("FLAC")));
    }
}
