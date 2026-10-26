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
        assert!(
            d.holes().is_empty(),
            "an AC-3 frame left E-AC-3 dither holes"
        );
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

pub(super) fn pinned_plain(r: &Refs, c: &refs::Case) -> (Vec<u8>, usize) {
    let (bytes, cleared) = without_dither_flags(&r.bytes(&c.stream()));
    r.assert_digest(&c.plain(), &testref::sha256(&bytes));
    (bytes, cleared)
}

pub(super) fn against_a52dec(r: &Refs, c: &refs::Case, got: &[f32], channels: u16) -> Diff {
    diff(
        &refs::window::window(got, channels as usize),
        &r.f32s(&c.decoded()),
    )
}

pub(super) fn every_frame(plain: &[u8], channels: u16) -> usize {
    frames(plain).len() * 1536 * channels as usize
}

#[test]
fn the_cases_are_named_once() {
    let mut names: Vec<&str> = refs::cases().map(|c| c.name).collect();
    let all = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), all);
    for name in refs::BOXED {
        assert_eq!(refs::named(name).name, name);
    }
    for acmod in 1..8u8 {
        assert!(
            refs::cases().any(|c| c.acmod == acmod),
            "no case codes acmod {acmod}"
        );
    }
    for rate in [32_000, 44_100, 48_000] {
        assert!(
            refs::cases().any(|c| c.rate == rate),
            "no case at {rate} Hz"
        );
    }
    assert!(refs::cases().any(|c| c.coupling));
    assert!(refs::cases().any(|c| c.short));
    assert!(refs::cases().any(|c| c.remat));
    assert!(refs::cases().any(|c| c.lfe));
}

#[test]
fn the_tracked_streams_are_the_writers_output() {
    let r = Refs::open(refs::AREA);
    for c in refs::cases() {
        let (made, marks) = refs::stream(c, true).unwrap();
        assert!(
            made == r.bytes(&c.stream()),
            "{}: not the writer's stream",
            c.name
        );
        let plain = refs::plain(c).unwrap();
        r.assert_digest(&c.plain(), &testref::sha256(&plain));
        let (patched, cleared) = without_dither_flags(&made);
        assert_eq!(cleared, marks.len(), "{}", c.name);
        assert!(
            patched == plain,
            "{}: the patch and the writer disagree",
            c.name
        );
    }
}

#[test]
fn the_writer_frames_carry_both_crcs_and_their_size() {
    for c in refs::cases() {
        let (made, _) = refs::stream(c, true).unwrap();
        let (_, size) = refs::frame_bytes(c.rate, c.kbps).unwrap();
        let five = (size / 2 / 2 + size / 2 / 8) * 2;
        assert_eq!(made.len() % size, 0, "{}", c.name);
        for f in frames(&made) {
            assert_eq!(f.len(), size, "{}", c.name);
            assert_eq!(refs::crc16(&f[2..five]), 0, "{}: crc1", c.name);
            assert_eq!(refs::crc16(&f[five..]), 0, "{}: crc2", c.name);
        }
        let i = info(&made).unwrap();
        assert_eq!((i.rate, i.channels, i.eac3), (c.rate, c.channels(), false));
    }
}

#[test]
fn the_writer_round_trips_the_source_through_our_decoder() {
    for name in ["ac3-mono", "ac3-stereo", "ac3-51", "edge-ac3-22"] {
        let c = refs::named(name);
        let (made, _) = refs::stream(c, false).unwrap();
        let (got, ch) = ours(&made, false);
        let ch = ch as usize;
        let src = refs::signal(c, refs::SAMPLES);
        let n = c.channels() as usize;
        let slots: &[usize] = match c.acmod {
            7 => &[0, 2, 1, 4, 5],
            6 => &[0, 1, 4, 5],
            _ => &[0, 1],
        };
        let slot = |k: usize| slots[k];
        for k in 0..c.nfchans() {
            let (mut s, mut e) = (0f64, 0f64);
            for i in 0..refs::SAMPLES {
                let x = f64::from(src[i * n + k]);
                let y = f64::from(got[(i + refs::DELAY) * ch + slot(k)]);
                s += x * x;
                e += (x - y).powi(2);
            }
            let snr = 10.0 * (s / e).log10();
            assert!(snr > 30.0, "{name} channel {k}: {snr:.1} dB");
        }
    }
}

#[test]
fn the_mdct_reconstructs_its_input_across_blocks() {
    let m = refs::Mdct::new();
    let x: Vec<f64> = (0..1024)
        .map(|i| ((i as f64 * 0.05).sin() + (i as f64 * 0.31).cos()) * 0.3)
        .collect();
    let a = m.inverse(&m.forward(&x[0..512]));
    let b = m.inverse(&m.forward(&x[256..768]));
    for n in 0..256 {
        assert!((a[256 + n] + b[n] - x[256 + n]).abs() < 1e-9, "sample {n}");
    }
    for (bap, codes) in [(1u8, 3u32), (2, 5), (3, 7), (4, 11), (5, 15)] {
        for code in 0..codes {
            assert_eq!(
                refs::quantize(bap, refs::level(bap, code)),
                code,
                "bap {bap}"
            );
        }
    }
    for bap in 6..16u8 {
        for v in [-1.0, -0.5, 0.0, 0.25, 0.99] {
            let code = refs::quantize(bap, v);
            assert!(
                (refs::level(bap, code) - v).abs() < 1.0 / 16.0,
                "bap {bap} {v}"
            );
        }
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
}

#[test]
fn ac3_matches_a52dec() {
    let r = Refs::open(refs::AREA);
    for c in &refs::MAIN {
        let (plain, cleared) = pinned_plain(&r, c);
        assert!(cleared > 0, "{}: no dither flags to clear", c.name);
        let (got, ch) = ours(&plain, false);
        assert_eq!(ch, layout(c.channels()));
        assert_eq!(got.len(), every_frame(&plain, ch), "{}: length", c.name);
        testref::assert_within(c.name, &against_a52dec(&r, c, &got, ch), LIMIT, LIMIT);
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
    for name in [
        "ac3-51",
        "ac3-stereo",
        "edge-ac3-51-cpl",
        "edge-ac3-50-short",
    ] {
        let c = refs::named(name);
        let channels = c.channels();
        let data = r.bytes(&c.stream());
        let good: Vec<Vec<u8>> = frames(&data).into_iter().map(<[u8]>::to_vec).collect();
        assert!(good.len() >= 2, "{name}: {} frames", good.len());
        let mut buf = Vec::new();
        for round in 0..400 {
            let mut d = Decoder::new(48_000, channels);
            for f in good.iter().cycle().take(3) {
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
        let (rate, channels) = (c.rate, layout(c.channels()));
        let n = channels as usize;
        let (plain, _) = pinned_plain(&r, c);
        let reference = r.f32s(&c.decoded());
        for ext in refs::BOX_EXTS {
            let file = refs::boxed(name, ext);
            let path = r.path(&file);
            let p = probe(&path, &Prefs::default()).unwrap();
            assert_eq!(p.audio.len(), 2, "{file}");
            assert!(p.audio.iter().all(|a| a.native()), "{file}: {:?}", p.audio);
            assert_eq!(p.audio[0].channels, c.channels(), "{file}");
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
            assert_eq!(got.samples.len(), every_frame(&plain, channels), "{file}");
            let d = diff(&refs::window::window(&got.samples, n), &reference);
            testref::assert_within(&file, &d, LIMIT, LIMIT);
            let stereo =
                audio_pcm_range(&path, 0, 0.01, 0.05, 48_000, 2, &Prefs::default()).unwrap();
            assert_eq!(stereo.samples.len(), 3840, "{file}");
        }
    }
}

#[test]
fn the_window_samples_every_block_of_every_frame() {
    let total = refs::SAMPLES + 256;
    assert_eq!(total % 1536, 0);
    let index: Vec<f32> = (0..total).map(|i| i as f32).collect();
    let seen = refs::window::window(&index, 1);
    for block in 0..total / 256 {
        let span = (block * 256) as f32..((block + 1) * 256) as f32;
        assert!(
            seen.iter().any(|i| span.contains(i)),
            "block {block} unseen"
        );
    }
    assert_eq!(seen.last(), index.last());
}

#[test]
fn the_window_tells_a_lost_or_extra_frame() {
    let c = refs::named("ac3-stereo");
    let n = c.channels() as usize;
    let full = refs::signal(c, refs::SAMPLES + 256);
    let want = refs::window::window(&full, n);
    let lost = refs::window::window(&full[..full.len() - 1536 * n], n);
    let mut padded = full.clone();
    padded.extend(std::iter::repeat_n(0.0, 1536 * n));
    let extra = refs::window::window(&padded, n);
    let short = refs::window::window(&full[..full.len() - 32 * n], n);
    for (what, got) in [("lost", lost), ("extra", extra), ("short", short)] {
        assert!(!diff(&got, &want).within(LIMIT, LIMIT), "{what}");
    }
    assert!(diff(&refs::window::window(&full, n), &want).within(0.0, 0.0));
}
