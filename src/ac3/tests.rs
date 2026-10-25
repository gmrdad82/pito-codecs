use super::*;
use crate::testref::{self, Diff, Refs, diff};

#[path = "refs.rs"]
pub(super) mod refs;

pub(super) const LIMIT: f64 = 5e-5;

fn frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(n) = frame_len(&data[pos..]).filter(|&n| n > 0 && pos + n <= data.len()) {
        out.push(&data[pos..pos + n]);
        pos += n;
    }
    out
}

pub(super) fn without_dither_flags(data: &[u8]) -> (Vec<u8>, usize) {
    let i = info(data).unwrap();
    let mut d = Decoder::new(i.rate, i.channels);
    let mut patched = data.to_vec();
    let mut off = 0;
    let mut cleared = 0;
    let mut buf = Vec::new();
    for f in frames(data) {
        d.decode_into(f, &mut buf).unwrap();
        for m in d.marks() {
            let bit = off * 8 + m;
            patched[bit / 8] &= !(0x80 >> (bit % 8));
            cleared += 1;
        }
        off += f.len();
    }
    (patched, cleared)
}

pub(super) fn ours(data: &[u8], dither: bool) -> (Vec<f32>, u16) {
    let i = info(data).unwrap();
    let mut d = Decoder::new(i.rate, i.channels);
    if !dither {
        d = d.without_dither();
    }
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for f in frames(data) {
        d.decode_into(f, &mut buf).unwrap();
        out.extend_from_slice(&buf);
    }
    (out, d.channels())
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub(super) fn with_dither_flags(data: &[u8]) -> Vec<u8> {
    let i = info(data).unwrap();
    let mut d = Decoder::new(i.rate, i.channels);
    let nfchans = header::parse(data).unwrap().nfchans;
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for f in frames(data) {
        d.decode_into(f, &mut buf).unwrap();
        let holes = d.holes();
        let (flag, rest) = holes.split_first().unwrap();
        let (end, blocks) = rest.split_last().unwrap();
        let total = f.len() * 8;
        assert!(
            end + blocks.len() * nfchans + 16 <= total,
            "no room for dither flags"
        );
        let bit = |i: usize| f[i / 8] >> (7 - i % 8) & 1;
        let mut bits: Vec<u8> = Vec::with_capacity(total);
        let mut next = blocks.iter().peekable();
        for i in 0..total {
            while next.peek() == Some(&&i) {
                bits.extend(std::iter::repeat_n(0, nfchans));
                next.next();
            }
            bits.push(if i == *flag { 1 } else { bit(i) });
        }
        bits.truncate(total);
        let mut frame: Vec<u8> = bits
            .chunks(8)
            .map(|c| c.iter().fold(0u8, |a, &b| a << 1 | b))
            .collect();
        let n = frame.len();
        let crc = crc16(&frame[2..n - 2]);
        frame[n - 2..].copy_from_slice(&crc.to_be_bytes());
        out.extend(frame);
    }
    out
}

pub(super) fn plain(c: &refs::Case, data: &[u8]) -> (Vec<u8>, usize) {
    if c.codec == "ac3" {
        without_dither_flags(data)
    } else {
        (with_dither_flags(data), 0)
    }
}

pub(super) fn pinned_plain(r: &Refs, c: &refs::Case) -> (Vec<u8>, usize) {
    let (bytes, cleared) = plain(c, &r.bytes(&c.stream()));
    r.assert_digest(&c.plain(), &testref::sha256(&bytes));
    (bytes, cleared)
}

pub(super) fn against_ffmpeg(r: &Refs, c: &refs::Case, got: &[f32], channels: u16) -> Diff {
    diff(&refs::window::window(got, channels as usize), &r.f32s(&c.decoded()))
}

#[test]
#[ignore]
fn write_plain_streams() {
    let dir = refs::work_dir();
    for c in refs::cases() {
        let Ok(data) = std::fs::read(dir.join(c.stream())) else {
            continue;
        };
        testref::write_atomic(&dir.join(c.plain()), &plain(c, &data).0).unwrap();
    }
}

#[test]
fn the_patch_cases_are_named_once() {
    let mut names: Vec<&str> = refs::cases().map(|c| c.name).collect();
    let all = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), all);
    for name in refs::BOXED {
        assert_eq!(refs::named(name).name, name);
    }
}

#[test]
fn the_patches_change_only_what_they_name() {
    let r = Refs::open(refs::AREA);
    let c = refs::named("ac3-51");
    let data = r.bytes(&c.stream());
    let (cleared, n) = without_dither_flags(&data);
    assert!(n > 0);
    assert_eq!(cleared.len(), data.len());
    let flipped: u32 = data
        .iter()
        .zip(&cleared)
        .map(|(a, b)| (a ^ b).count_ones())
        .sum();
    assert_eq!(flipped as usize, n);
    assert_eq!(without_dither_flags(&cleared).1, 0);
    let e = refs::named("eac3-stereo");
    let data = r.bytes(&e.stream());
    let flagged = with_dither_flags(&data);
    assert_eq!(flagged.len(), data.len());
    assert_eq!(frames(&flagged).len(), frames(&data).len());
}

#[test]
fn ac3_matches_ffmpeg() {
    let r = Refs::open(refs::AREA);
    for name in ["ac3-51", "ac3-stereo", "ac3-mono"] {
        let c = refs::named(name);
        let (plain, cleared) = pinned_plain(&r, c);
        assert!(cleared > 0, "{name}: no dither flags to clear");
        let (got, ch) = ours(&plain, false);
        assert_eq!(ch, c.channels);
        testref::assert_within(name, &against_ffmpeg(&r, c, &got, ch), LIMIT, LIMIT);
    }
}

#[test]
fn eac3_matches_ffmpeg() {
    let r = Refs::open(refs::AREA);
    for name in ["eac3-51", "eac3-stereo"] {
        let c = refs::named(name);
        let (plain, _) = pinned_plain(&r, c);
        let (got, ch) = ours(&plain, false);
        assert_eq!(ch, c.channels);
        testref::assert_within(name, &against_ffmpeg(&r, c, &got, ch), LIMIT, LIMIT);
    }
}

#[test]
fn a_dithered_decode_differs_from_the_plain_one() {
    let r = Refs::open(refs::AREA);
    let c = refs::named("ac3-51");
    let data = r.bytes(&c.stream());
    let (dithered, _) = ours(&data, true);
    let (plain, _) = ours(&without_dither_flags(&data).0, false);
    assert_eq!(dithered.len(), plain.len());
    assert!(dithered != plain);
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[test]
fn malformed_frames_fail_cleanly() {
    let r = Refs::open(refs::AREA);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for name in ["ac3-51", "eac3-stereo"] {
        let c = refs::named(name);
        let channels = c.channels;
        let data = r.bytes(&c.stream());
        let good: Vec<Vec<u8>> = frames(&data).into_iter().map(<[u8]>::to_vec).collect();
        assert!(good.len() >= 2, "{name}: {} frames", good.len());
        let mut buf = Vec::new();
        for round in 0..400 {
            let mut d = Decoder::new(48_000, channels);
            for f in good.iter().take(3) {
                let mut p = f.clone();
                match rng.below(4) {
                    0 => p.truncate(rng.below(p.len() + 1)),
                    1 => {
                        for _ in 0..1 + rng.below(8) {
                            let i = rng.below(p.len());
                            p[i] ^= 1 << rng.below(8);
                        }
                    }
                    2 => {
                        for b in p.iter_mut().skip(2 + rng.below(16)) {
                            *b = rng.next() as u8;
                        }
                    }
                    _ => p.extend((0..rng.below(64)).map(|_| rng.next() as u8)),
                }
                if round % 3 == 0 {
                    p.extend_from_slice(f);
                }
                if d.decode_into(&p, &mut buf).is_ok() {
                    assert_eq!(buf.len() % channels as usize, 0);
                }
            }
        }
        assert!(
            Decoder::new(48_000, channels)
                .decode_into(&[], &mut buf)
                .is_err()
        );
    }
}

#[cfg(all(feature = "mkv", feature = "mp4"))]
#[test]
fn containers_decode_through_the_media_front() {
    use crate::media::{Prefs, audio_pcm_range, probe};
    let r = Refs::open(refs::AREA);
    for name in refs::BOXED {
        let c = refs::named(name);
        let (rate, channels) = (c.rate, c.channels);
        let n = channels as usize;
        let reference = r.f32s(&c.decoded());
        for ext in refs::BOX_EXTS {
            let file = refs::boxed(name, ext);
            let path = r.path(&file);
            let p = probe(&path, &Prefs::default()).unwrap();
            assert_eq!(p.audio.len(), 2, "{file}");
            assert!(p.audio.iter().all(|a| a.native()), "{file}: {:?}", p.audio);
            assert_eq!(p.audio[0].channels, channels, "{file}");
            let got = audio_pcm_range(
                &path,
                0,
                0.0,
                f64::INFINITY,
                rate,
                channels,
                &Prefs::default(),
            )
            .unwrap();
            assert_eq!(got.channels, channels);
            let got = refs::window::window(&got.samples, n);
            let k = got.len().min(reference.len());
            let per_frame = (1536 / (refs::window::RUN * refs::window::EVERY) + 1)
                * refs::window::RUN
                * n;
            assert!(
                k + per_frame >= reference.len(),
                "{file}: {k} of {}",
                reference.len()
            );
            let d = diff(&got[..k], &reference[..k]);
            testref::assert_within(&file, &d, LIMIT, LIMIT);
            let stereo =
                audio_pcm_range(&path, 0, 0.01, 0.05, 48_000, 2, &Prefs::default()).unwrap();
            assert_eq!(stereo.samples.len(), 3840, "{file}");
        }
    }
}
