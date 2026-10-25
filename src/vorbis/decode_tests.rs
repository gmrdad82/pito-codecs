use super::ogg::{page_crc, span};
use super::*;
use crate::testref::{self, Refs};
use std::path::PathBuf;

#[allow(dead_code)]
#[path = "refs.rs"]
pub(super) mod refs;

use refs::{Case, End, Made};

fn open() -> Refs {
    Refs::open(refs::AREA)
}

fn scratch(name: &str) -> PathBuf {
    testref::scratch(refs::AREA).join(name)
}

fn made(file: &str) -> Made {
    *refs::MADE
        .iter()
        .find(|m| m.file == file)
        .unwrap_or_else(|| panic!("no Vorbis file named {file}"))
}

fn stereo() -> Case {
    refs::named("stereo_44k_q3")
}

fn close(name: &str, ours: &[f32], theirs: &[f32]) {
    assert!(!theirs.is_empty(), "{name}: nothing to compare");
    let d = testref::diff(ours, theirs);
    testref::assert_within(name, &d, refs::MAX_DIFF, refs::MAX_RMS);
}

fn like_ffmpeg(r: &Refs, file: &str, c: &Case, ours: &[f32], whole: bool) {
    let mine = refs::decimate(ours, c.channels, c.stride());
    let theirs = r.f32s(&refs::every(file));
    if whole {
        close(
            &format!("{file}: every {}th frame", c.stride()),
            &mine,
            &theirs,
        );
    } else {
        let n = mine.len().min(theirs.len());
        close(
            &format!("{file}: every {}th frame", c.stride()),
            &mine[..n],
            &theirs[..n],
        );
    }
    match refs::end(file) {
        End::Head => {
            let head = r.f32s(&refs::head(file));
            assert!(ours.len() >= head.len(), "{file}: {} samples", ours.len());
            close(&format!("{file}: head"), &ours[..head.len()], &head);
        }
        End::Tail => {
            let tail = r.f32s(&refs::tail(file));
            assert!(ours.len() >= tail.len(), "{file}: {} samples", ours.len());
            close(
                &format!("{file}: tail"),
                &ours[ours.len() - tail.len()..],
                &tail,
            );
        }
    }
}

fn expected_frames(c: &Case, ogg: &[u8]) -> usize {
    if c.native {
        OggVorbis::parse(ogg).unwrap().samples().unwrap() as usize
    } else {
        c.frames()
    }
}

#[test]
fn ogg_vorbis_decodes_to_ffmpegs_samples_with_exact_lengths() {
    let r = open();
    for c in &refs::CASES {
        let file = c.ogg();
        let data = r.bytes(&file);
        let ogg = OggVorbis::parse(&data).unwrap();
        assert!(ogg.warnings.is_empty(), "{}: {:?}", c.name, ogg.warnings);
        let ours = ogg.decode().unwrap();
        assert_eq!(ours.rate, c.rate, "{}", c.name);
        assert_eq!(ours.channels, c.channels, "{}", c.name);
        assert_eq!(
            ours.frames(),
            expected_frames(c, &data),
            "{}: frames",
            c.name
        );
        assert_eq!(ogg.samples().unwrap() as usize, ours.frames(), "{}", c.name);
        let picked = r.f32s(&refs::every(&file)).len() / c.channels as usize;
        let theirs = picked * c.stride();
        let long = 1usize << ogg.decoder().unwrap().ident.blocksize_1;
        assert!(
            theirs.abs_diff(ours.frames()) <= long / 2 + c.stride(),
            "{}: ffmpeg's Ogg demuxer gives about {theirs} frames, ours {}",
            c.name,
            ours.frames()
        );
        like_ffmpeg(&r, &file, c, &ours.samples, false);
    }
}

#[test]
fn every_channel_lands_in_ffmpegs_order() {
    let r = open();
    let mut seen = Vec::new();
    for c in refs::CASES.iter().filter(|c| c.channels >= 3) {
        seen.push(c.channels);
        let ours = decode(&r.bytes(&c.ogg())).unwrap();
        let mine = refs::decimate(&ours.samples, c.channels, c.stride());
        let theirs = r.f32s(&refs::every(&c.ogg()));
        assert!(
            mine.len() >= theirs.len(),
            "{}: {} picked",
            c.name,
            mine.len()
        );
        let stride = c.channels as usize;
        for ch in 0..stride {
            let energy = |s: &[f32]| -> f64 {
                s.iter()
                    .skip(ch)
                    .step_by(stride)
                    .map(|&v| (v as f64) * (v as f64))
                    .sum()
            };
            let a = energy(&mine[..theirs.len()]);
            let b = energy(&theirs);
            assert!(
                (a - b).abs() <= 1e-3 * b.max(1e-9),
                "{} channel {ch}: {a} vs {b}",
                c.name
            );
        }
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, [3, 4, 5, 6, 7, 8]);
}

#[test]
fn the_first_packet_primes_and_later_packets_follow_the_blocksizes() {
    let ogg = OggVorbis::parse(&open().bytes(&stereo().ogg())).unwrap();
    let mut dec = ogg.decoder().unwrap();
    assert!(!dec.primed());
    let first = ogg.packet(0);
    let n0 = dec.blocksize(first).unwrap();
    let (_, flags0) = dec.shape(first).unwrap();
    assert!(flags0.is_none(), "this stream starts on a short block");
    assert_eq!(dec.lead_in(first).unwrap(), n0 / 2);
    let short = 1usize << dec.ident.blocksize_0;
    let tail = |p: &[u8]| match dec.shape(p).unwrap() {
        (n, Some((_, false))) => n / 4 - short / 4,
        _ => 0,
    };
    let last_tail = tail(ogg.packet(ogg.packets.len() - 1));
    let mut out = Vec::new();
    assert_eq!(dec.decode_into(first, &mut out).unwrap(), 0);
    assert!(out.is_empty());
    assert!(dec.primed());
    assert_eq!(dec.decode_into(&[], &mut out).unwrap(), 0);
    let mut total = 0;
    let mut sum_formula = 0;
    let mut prev = n0;
    for i in 1..ogg.packets.len() {
        let p = ogg.packet(i);
        let n = dec.blocksize(p).unwrap();
        sum_formula += prev / 4 + n / 4;
        prev = n;
        total += dec.decode_into(p, &mut out).unwrap();
        assert_eq!(out.len(), total * 2);
    }
    assert_eq!(total, sum_formula + last_tail);
    dec.reset();
    assert!(!dec.primed());
    let mut aligned = Vec::new();
    let got = dec.decode_aligned(ogg.packet(0), &mut aligned).unwrap();
    assert_eq!(got, n0 / 2);
    assert!(aligned.iter().all(|&v| v == 0.0));
}

#[test]
fn the_table_picks_every_short_block_and_keeps_its_ends_off_the_millisecond() {
    for c in &refs::CASES {
        assert!(
            c.stride() <= refs::FINE,
            "{}: every {}th frame",
            c.name,
            c.stride()
        );
        assert_eq!(c.stride() % 2, 1, "{}", c.name);
    }
    assert!(refs::LONG.stride() > refs::FINE);
    for file in refs::OFF_THE_MILLISECOND {
        let c = made(file).case();
        assert!(
            refs::off_the_millisecond(c.frames(), c.rate),
            "{file}: {}",
            c.frames()
        );
    }
    assert!(made(refs::COPIED_MKV).copied);
    assert!(!refs::off_the_millisecond(22050, 44100));
    assert!(!refs::off_the_millisecond(3 * 96000, 96000));
    assert!(refs::off_the_millisecond(22592, 44100));
    assert!(refs::off_the_millisecond(97, 96000));
    let (a, b) = refs::seam_span(&refs::LONG);
    assert!(a < b && b < refs::LONG.frames(), "{a}..{b}");
    const { assert!(refs::LONG.seconds > refs::SEAM + 0.2) };
}

#[cfg(feature = "audio")]
mod front {
    use super::*;
    use crate::media::{Prefs, audio_pcm, audio_pcm_range, probe};
    use std::path::Path;

    fn whole(path: &Path, rate: u32, ch: u16) -> crate::media::AudioBuffer {
        audio_pcm_range(path, 0, 0.0, f64::INFINITY, rate, ch, &Prefs::default()).unwrap()
    }

    fn range(path: &Path, c: &Case, from: f64, to: f64) -> crate::media::AudioBuffer {
        audio_pcm_range(path, 0, from, to, c.rate, c.channels, &Prefs::default()).unwrap()
    }

    fn at(c: &Case, t: f64) -> usize {
        (t * c.rate as f64).round() as usize * c.channels as usize
    }

    fn streamed(path: &Path, c: &Case, from: f64) -> Vec<f32> {
        crate::media::audio_stream(path, 0, from, c.rate, c.channels, &Prefs::default())
            .unwrap()
            .flat_map(|b| b.unwrap().samples)
            .collect()
    }

    #[test]
    fn a_bare_ogg_reads_through_probe_and_audio_pcm_range() {
        let r = open();
        for c in &refs::CASES {
            let path = r.path(&c.ogg());
            let data = std::fs::read(&path).unwrap();
            let oga = scratch(&format!("{}.oga", c.name));
            testref::write_atomic(&oga, &data).unwrap();
            let frames = expected_frames(c, &data);
            for p in [&path, &oga] {
                let pr = probe(p, &Prefs::default()).unwrap();
                let a = &pr.audio[0];
                assert_eq!(a.codec, "vorbis", "{}", c.name);
                assert!(a.native(), "{}", c.name);
                assert_eq!((a.channels, a.rate), (c.channels, c.rate), "{}", c.name);
                assert!(
                    (pr.duration - frames as f64 / c.rate as f64).abs() < 1e-6,
                    "{}: {}",
                    c.name,
                    pr.duration
                );
                let b = whole(p, c.rate, c.channels);
                assert_eq!(b.frames(), frames, "{}", c.name);
                let direct = decode(&data).unwrap();
                assert_eq!(b.samples, direct.samples, "{}", c.name);
                if c.channels <= 2 {
                    let pcm = audio_pcm(p, 0, c.rate, c.channels == 1, &Prefs::default()).unwrap();
                    assert_eq!(pcm.samples, b.samples, "{}", c.name);
                }
            }
            let (from, to) = (c.seconds * 0.35, c.seconds * 0.7);
            let part = range(&path, c, from, to);
            let direct = decode(&data).unwrap();
            assert_eq!(
                part.samples,
                direct.samples[at(c, from)..at(c, to)],
                "{}",
                c.name
            );
        }
    }

    #[test]
    fn webm_vorbis_matches_ffmpeg_sample_for_sample_and_in_length() {
        let r = open();
        for m in refs::MADE.iter().filter(|m| !m.mp4()) {
            let c = m.case();
            let path = r.path(m.file);
            let pr = probe(&path, &Prefs::default()).unwrap();
            assert!(pr.audio[0].native(), "{}", m.file);
            assert!(pr.unsupported().is_empty(), "{}", m.file);
            let ours = whole(&path, c.rate, c.channels);
            if !m.copied {
                assert_eq!(ours.frames(), c.frames(), "{}", m.file);
            }
            like_ffmpeg(&r, m.file, &c, &ours.samples, true);
            if c.channels <= 2 {
                let pcm = audio_pcm(&path, 0, c.rate, c.channels == 1, &Prefs::default()).unwrap();
                assert_eq!(pcm.samples, ours.samples, "{}", m.file);
            }
        }
    }

    #[test]
    fn a_webm_window_lines_up_with_the_whole_decode() {
        let c = refs::LONG;
        let path = open().path(refs::LONG_WEBM);
        let all = whole(&path, c.rate, c.channels);
        let (from, to) = (0.3, 0.6);
        let part = range(&path, &c, from, to);
        assert_eq!(part.samples.len(), at(&c, to) - at(&c, from));
        close(
            "webm window",
            &part.samples,
            &all.samples[at(&c, from)..at(&c, to)],
        );
    }

    #[test]
    fn a_long_webm_decodes_in_pieces_that_join_without_a_seam() {
        let r = open();
        let c = refs::LONG;
        let path = r.path(refs::LONG_WEBM);
        let ours = whole(&path, c.rate, c.channels);
        assert_eq!(ours.frames(), c.frames());
        like_ffmpeg(&r, refs::LONG_WEBM, &c, &ours.samples, true);
        let (a, b) = refs::seam_span(&c);
        let ch = c.channels as usize;
        close(
            "long webm across the seam",
            &ours.samples[a * ch..b * ch],
            &r.f32s(&refs::seam(refs::LONG_WEBM)),
        );
        let (from, to) = (refs::SEAM - 0.1, refs::SEAM + 0.1);
        let part = range(&path, &c, from, to);
        assert_eq!(part.samples.len(), at(&c, to) - at(&c, from));
        close(
            "long webm window",
            &part.samples,
            &ours.samples[at(&c, from)..at(&c, to)],
        );
    }

    #[test]
    fn high_rate_matroska_pieces_and_windows_land_on_ffmpegs_samples() {
        let r = open();
        let windows: [(&str, &[(f64, f64)]); 2] = [
            (
                refs::LONG_WEBM,
                &[(3.9, 4.1), (1.2345, 1.5), (0.613, 0.9), (4.0, 4.2)],
            ),
            (refs::HIGH_MKV, &[(1.2345, 1.5), (0.613, 0.9), (0.5, 1.5)]),
        ];
        for (file, spans) in windows {
            let c = made(file).case();
            let path = r.path(file);
            let ours = whole(&path, c.rate, c.channels);
            assert_eq!(ours.frames(), c.frames(), "{file}");
            like_ffmpeg(&r, file, &c, &ours.samples, true);
            for &(from, to) in spans {
                let part = range(&path, &c, from, to);
                assert_eq!(
                    part.samples.len(),
                    at(&c, to) - at(&c, from),
                    "{file} {from}"
                );
                close(
                    &format!("{file} window {from}"),
                    &part.samples,
                    &ours.samples[at(&c, from)..at(&c, to)],
                );
            }
        }
    }

    #[test]
    fn matroska_vorbis_off_the_millisecond_ends_on_ffmpegs_last_sample() {
        let r = open();
        for file in refs::OFF_THE_MILLISECOND {
            let m = made(file);
            let c = m.case();
            let name = format!("{file}{}", if m.copied { " copied" } else { "" });
            let path = r.path(file);
            let ours = whole(&path, c.rate, c.channels);
            like_ffmpeg(&r, file, &c, &ours.samples, true);
            let frames = ours.frames();
            assert!(
                refs::off_the_millisecond(frames, c.rate),
                "{name}: {frames}"
            );
            assert_eq!(
                streamed(&path, &c, 0.0).len(),
                ours.samples.len(),
                "{name} stream"
            );
            let tail = r.f32s(&refs::tail(file));
            let end = frames as f64 / c.rate as f64;
            for (from, to) in [
                (end * 0.94, f64::INFINITY),
                (end * 0.99, end + 0.5),
                (end * 0.22, f64::INFINITY),
            ] {
                let part = range(&path, &c, from, to);
                assert_eq!(
                    part.samples.len(),
                    ours.samples.len() - at(&c, from),
                    "{name} {from}"
                );
                close(
                    &format!("{name} to the end from {from}"),
                    &part.samples,
                    &ours.samples[at(&c, from)..],
                );
                let n = tail.len().min(part.samples.len());
                close(
                    &format!("{name} last samples from {from}"),
                    &part.samples[part.samples.len() - n..],
                    &tail[tail.len() - n..],
                );
            }
            let from = end * 0.94;
            assert_eq!(
                streamed(&path, &c, from).len(),
                ours.samples.len() - at(&c, from),
                "{name} stream from {from}"
            );
        }
    }

    #[test]
    fn a_webm_cut_short_ends_on_ffmpegs_last_sample_though_its_header_says_longer() {
        let r = open();
        let c = refs::LONG;
        let cut = refs::cut_short(&r.path(refs::LONG_WEBM)).unwrap();
        r.assert_digest(refs::CUT_SHORT, &testref::sha256(&cut));
        let path = scratch(refs::CUT_SHORT);
        testref::write_atomic(&path, &cut).unwrap();
        let ours = whole(&path, c.rate, c.channels);
        like_ffmpeg(&r, refs::CUT_SHORT, &c, &ours.samples, true);
        let frames = ours.frames();
        let rate = c.rate as f64;
        assert!(
            (1.0 * rate..3.5 * rate).contains(&(frames as f64)),
            "{frames}"
        );
        assert!(refs::off_the_millisecond(frames, c.rate), "{frames}");
        let rd = crate::mkv::Reader::open(&path).unwrap();
        assert!(
            rd.duration_seconds() > frames as f64 / rate + 1.0,
            "{}",
            rd.duration_seconds()
        );
        let from = frames as f64 / rate - 0.7;
        let part = range(&path, &c, from, f64::INFINITY);
        assert_eq!(part.samples.len(), ours.samples.len() - at(&c, from));
        close(
            "webm cut short to the end",
            &part.samples,
            &ours.samples[at(&c, from)..],
        );
    }

    #[test]
    fn a_damaged_block_where_a_window_lands_is_refused_and_a_window_at_the_end_still_fits() {
        let c = refs::LONG;
        let path = open().path(refs::LONG_WEBM);
        let all = whole(&path, c.rate, c.channels);
        let (from, to) = (4.15, 4.2);
        let part = range(&path, &c, from, to);
        assert_eq!(part.samples.len(), at(&c, to) - at(&c, from));
        close(
            "96k webm end",
            &part.samples,
            &all.samples[at(&c, from)..at(&c, to)],
        );
        let r = crate::mkv::Reader::open(&path).unwrap();
        let mut s = r.stream();
        let mut data = Vec::new();
        let mut file = std::fs::read(&path).unwrap();
        let mut hit = 0;
        while let Some(b) = s.next_into(&mut data).unwrap() {
            if !(1940..=2000).contains(&b.pts) {
                continue;
            }
            let i = file
                .windows(data.len())
                .position(|w| w == &data[..])
                .expect("a block's bytes in the file");
            file[i] |= 1;
            hit += 1;
        }
        assert!(hit > 2, "{hit}");
        let bad = scratch("damaged_96k.webm");
        testref::write_atomic(&bad, &file).unwrap();
        let e = audio_pcm_range(&bad, 0, 2.0, 2.2, c.rate, 2, &Prefs::default())
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "invalid data: Vorbis header where an audio packet was expected"
        );
        let fine = range(&bad, &c, 3.0, 3.2);
        close(
            "96k webm past the damage",
            &fine.samples,
            &all.samples[at(&c, 3.0)..at(&c, 3.2)],
        );
    }

    #[test]
    fn mp4_vorbis_reads_through_probe_audio_pcm_and_matches_ffmpeg() {
        let r = open();
        for m in refs::MADE.iter().filter(|m| m.mp4()) {
            let c = m.case();
            let path = r.path(m.file);
            let pr = probe(&path, &Prefs::default()).unwrap();
            assert_eq!(pr.audio[0].codec, "vorbis", "{}", m.file);
            assert!(pr.audio[0].native(), "{}", m.file);
            assert!(pr.unsupported().is_empty(), "{}", m.file);
            assert_eq!(
                (pr.audio[0].channels, pr.audio[0].rate),
                (c.channels, c.rate),
                "{}",
                m.file
            );
            let ours = whole(&path, c.rate, c.channels);
            let ogg = r.bytes(&c.ogg());
            let src = OggVorbis::parse(&ogg).unwrap();
            let primed = src.decoder().unwrap().lead_in(src.packet(0)).unwrap();
            let mp4 = crate::mp4::Reader::open(&path).unwrap();
            let t = &mp4.tracks[0];
            assert_eq!(t.edit_offset, -(primed as i64), "{}", m.file);
            let media_end = t.samples.last().map_or(0, |s| s.pts + s.duration as i64) as usize;
            assert_eq!(
                ours.frames(),
                expected_frames(&c, &ogg).min(media_end) - primed,
                "{}: the first packet's span is priming",
                m.file
            );
            let theirs = r.f32s(&refs::every(m.file)).len();
            let mine = refs::decimate(&ours.samples, c.channels, c.stride()).len();
            assert!(
                theirs >= mine,
                "{}: every {}th frame, ours {mine} samples, ffmpeg {theirs}",
                m.file,
                c.stride()
            );
            like_ffmpeg(&r, m.file, &c, &ours.samples, false);
            if c.channels <= 2 {
                let pcm = audio_pcm(&path, 0, c.rate, c.channels == 1, &Prefs::default()).unwrap();
                assert_eq!(pcm.samples, ours.samples, "{}", m.file);
            }
            let mut data = std::fs::read(&path).unwrap();
            let at = data
                .windows(4)
                .position(|w| w == b"mp4a")
                .expect("an mp4a sample entry");
            data[at + 20..at + 22].copy_from_slice(&3u16.to_be_bytes());
            data[at + 28..at + 32].copy_from_slice(&(8000u32 << 16).to_be_bytes());
            let lying = scratch(&format!("{}-entry.mp4", c.name));
            testref::write_atomic(&lying, &data).unwrap();
            let pr = probe(&lying, &Prefs::default()).unwrap();
            assert_eq!(
                (pr.audio[0].channels, pr.audio[0].rate),
                (c.channels, c.rate),
                "{}: the ident header wins over the sample entry",
                m.file
            );
        }
    }

    #[test]
    fn mp4_vorbis_extradata_and_packets_rebuild_the_source_decode() {
        let r = open();
        for m in refs::MADE.iter().filter(|m| m.mp4()) {
            let c = m.case();
            let rd = crate::mp4::Reader::open(r.path(m.file)).unwrap();
            let t = &rd.tracks[0];
            let private = t.extradata().unwrap();
            let [ident, comment, setup] = xiph_headers(&private).unwrap();
            let src = OggVorbis::parse(&r.bytes(&c.ogg())).unwrap();
            assert_eq!(ident, &src.headers[0][..], "{}", m.file);
            assert_eq!(setup, &src.headers[2][..], "{}", m.file);
            assert_eq!(t.samples.len(), src.packets.len(), "{}", m.file);
            let mut dec = Decoder::from_headers(ident, comment, setup).unwrap();
            let mut ours = Vec::new();
            for k in 0..t.samples.len() {
                dec.decode_into(&rd.read(0, k).unwrap(), &mut ours).unwrap();
            }
            let mut theirs_dec = src.decoder().unwrap();
            let mut theirs = Vec::new();
            for k in 0..src.packets.len() {
                theirs_dec.decode_into(src.packet(k), &mut theirs).unwrap();
            }
            assert!(!ours.is_empty(), "{}", m.file);
            assert_eq!(ours, theirs, "{}", m.file);
        }
    }

    #[test]
    fn a_broken_vorbis_file_fails_in_one_line() {
        let (_, mut packets) = real_packets();
        packets[5][0] |= 1;
        let c = stereo();
        let bad = scratch("header_bit_audio.ogg");
        testref::write_atomic(&bad, &ogg_file(&packets, c.frames() as i64)).unwrap();
        let e = audio_pcm_range(&bad, 0, 0.0, f64::INFINITY, c.rate, 2, &Prefs::default())
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "invalid data: Vorbis header where an audio packet was expected"
        );
        let mut misfit = packets[..3].to_vec();
        misfit.extend([vec![0x0E], vec![0x00]]);
        let bad = scratch("misfit_window.ogg");
        testref::write_atomic(&bad, &ogg_file(&misfit, 1000)).unwrap();
        let e = audio_pcm_range(&bad, 0, 0.0, f64::INFINITY, c.rate, 2, &Prefs::default())
            .unwrap_err()
            .to_string();
        assert_eq!(e, "invalid data: malformed Vorbis audio packet");
    }
}

fn page(serial: u32, seq: u32, flags: u8, granule: i64, lacing: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"OggS");
    out.push(0);
    out.push(flags);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.push(lacing.len() as u8);
    out.extend_from_slice(lacing);
    out.extend_from_slice(body);
    let crc = page_crc(&out);
    out[22..26].copy_from_slice(&crc.to_le_bytes());
    out
}

fn ogg_file(packets: &[Vec<u8>], last_granule: i64) -> Vec<u8> {
    let count = packets.len();
    let mut out = Vec::new();
    for (seq, p) in packets.iter().enumerate() {
        let mut lacing = vec![255u8; p.len() / 255];
        lacing.push((p.len() % 255) as u8);
        let flags = match seq {
            0 => 0x02,
            s if s + 1 == count => 0x04,
            _ => 0,
        };
        let granule: i64 = if seq < 3 {
            0
        } else if seq + 1 == count {
            last_granule
        } else {
            -1
        };
        out.extend(page(0x5eed, seq as u32, flags, granule, &lacing, p));
    }
    out
}

fn fake_headers() -> Vec<Vec<u8>> {
    [&b"\x01vorbis"[..], b"\x03vorbis", b"\x05vorbis"]
        .iter()
        .map(|h| {
            let mut v = h.to_vec();
            v.resize(30, 0);
            v
        })
        .collect()
}

fn split_packet() -> Vec<u8> {
    (0..600u32).map(|i| (i % 251) as u8).collect()
}

fn spanning_pages() -> Vec<Vec<u8>> {
    let h = fake_headers();
    let a = split_packet();
    let b = [7u8; 20];
    vec![
        page(1, 0, 0x02, 0, &[30], &h[0]),
        page(1, 1, 0, 0, &[30, 30], &[&h[1][..], &h[2]].concat()),
        page(1, 2, 0, -1, &[255], &a[..255]),
        page(1, 3, 0x01, -1, &[255], &a[255..510]),
        page(1, 4, 0x01, 100, &[90, 20], &[&a[510..], &b[..]].concat()),
        page(1, 5, 0x04, 200, &[10], &[9u8; 10]),
    ]
}

fn packet_lengths(ogg: &OggVorbis) -> Vec<usize> {
    (0..ogg.packets.len())
        .map(|i| ogg.packet(i).len())
        .collect()
}

#[test]
fn a_packet_split_over_pages_joins_back() {
    let ogg = OggVorbis::parse(&spanning_pages().concat()).unwrap();
    assert!(ogg.warnings.is_empty(), "{:?}", ogg.warnings);
    assert_eq!(packet_lengths(&ogg), [600, 20, 10]);
    assert_eq!(ogg.packet(0), &split_packet()[..]);
    assert_eq!(ogg.granules, [None, Some(100), Some(200)]);
}

#[test]
fn a_lost_page_drops_the_packet_it_split_instead_of_splicing_it() {
    for damage in ["missing", "bad crc"] {
        let mut pages = spanning_pages();
        if damage == "missing" {
            pages.remove(3);
        } else {
            let last = pages[3].len() - 1;
            pages[3][last] ^= 0xff;
        }
        let ogg = OggVorbis::parse(&pages.concat()).unwrap();
        assert_eq!(packet_lengths(&ogg), [20, 10], "{damage}");
        assert_eq!(ogg.packet(0), &[7u8; 20][..], "{damage}");
        assert!(
            ogg.warnings.iter().any(|w| w.starts_with("missing page")),
            "{damage}: {:?}",
            ogg.warnings
        );
    }
}

#[test]
fn a_page_cut_inside_its_lacing_table_ends_the_stream_with_a_warning() {
    let whole = spanning_pages().concat();
    let mut tail = page(1, 6, 0, 300, &[10; 5], &[3u8; 50]);
    tail.truncate(29);
    let ogg = OggVorbis::parse(&[&whole[..], &tail].concat()).unwrap();
    assert_eq!(packet_lengths(&ogg), [600, 20, 10]);
    assert_eq!(
        ogg.warnings,
        [format!("truncated page at byte {}", whole.len())]
    );
    assert_eq!(ogg.granules, [None, Some(100), Some(200)]);
}

#[test]
fn a_packet_left_open_when_the_next_page_starts_fresh_is_dropped() {
    let h = fake_headers();
    let a = split_packet();
    let pages = [
        page(1, 0, 0x02, 0, &[30], &h[0]),
        page(1, 1, 0, 0, &[30, 30], &[&h[1][..], &h[2]].concat()),
        page(1, 2, 0, -1, &[255], &a[..255]),
        page(1, 3, 0, 150, &[20], &[7u8; 20]),
        page(1, 4, 0x04, 200, &[10], &[9u8; 10]),
    ];
    let at: usize = pages[..3].iter().map(Vec::len).sum();
    let ogg = OggVorbis::parse(&pages.concat()).unwrap();
    assert_eq!(packet_lengths(&ogg), [20, 10]);
    assert_eq!(ogg.packet(0), &[7u8; 20][..]);
    assert_eq!(ogg.packet(1), &[9u8; 10][..]);
    assert_eq!(ogg.granules, [Some(150), Some(200)]);
    assert_eq!(
        ogg.warnings,
        [format!("incomplete packet before page at byte {at}")]
    );
}

#[test]
fn a_chained_ogg_file_is_refused_by_name_and_a_multiplexed_one_is_read() {
    let mut pages = spanning_pages();
    pages.push(page(2, 0, 0x02, 0, &[30], &fake_headers()[0]));
    let e = OggVorbis::parse(&pages.concat()).err().unwrap();
    assert_eq!(
        one_line(e),
        "unsupported: chained Ogg Vorbis files are not supported"
    );
    let mut theora = b"\x80theora".to_vec();
    theora.resize(30, 0);
    let mut pages = spanning_pages();
    pages.insert(1, page(9, 0, 0x02, 0, &[30], &theora));
    pages.insert(3, page(9, 1, 0, 0, &[5], &[1; 5]));
    let ogg = OggVorbis::parse(&pages.concat()).unwrap();
    assert!(ogg.warnings.is_empty(), "{:?}", ogg.warnings);
    assert_eq!(packet_lengths(&ogg), [600, 20, 10]);
}

#[test]
fn a_reservation_past_memory_is_refused_in_one_line() {
    let mut v = Vec::new();
    assert_eq!(
        one_line(ogg::reserve(&mut v, u64::MAX, 2).unwrap_err()),
        "invalid data: Ogg Vorbis stream too long to decode in memory"
    );
    assert!(ogg::reserve(&mut v, 1 << 60, 8).is_err());
    assert!(ogg::reserve(&mut v, 1 << 62, 255).is_err());
    ogg::reserve(&mut v, 100, 2).unwrap();
    assert!(v.capacity() >= 200);
}

#[test]
fn a_flood_of_one_byte_packets_stops_at_the_last_granule() {
    let (ogg, packets) = real_packets();
    let long = 1usize << ogg.decoder().unwrap().ident.blocksize_1;
    let mut flood: Vec<Vec<u8>> = packets[..3].to_vec();
    flood.extend(std::iter::repeat_n(vec![0u8], 20_000));
    let ogg = OggVorbis::parse(&ogg_file(&flood, 1000)).unwrap();
    assert_eq!(ogg.packets.len(), 20_000);
    let out = ogg.decode().unwrap();
    assert_eq!(out.frames(), 1000);
    assert!(out.samples.iter().all(|&v| v == 0.0));
    assert!(
        out.samples.capacity() <= (1000 + long) * 2,
        "{}",
        out.samples.capacity()
    );
}

#[test]
fn a_misfit_window_through_decode_fails_in_one_line() {
    let (ogg, packets) = real_packets();
    let dec = ogg.decoder().unwrap();
    assert_eq!(dec.setup.modes.len(), 2);
    assert!(!dec.setup.modes[0].mode_blockflag);
    assert!(dec.setup.modes[1].mode_blockflag);
    let mut misfit = packets[..3].to_vec();
    misfit.extend([vec![0x0E], vec![0x00]]);
    let e = decode(&ogg_file(&misfit, 1000)).err().unwrap();
    assert_eq!(one_line(e), "invalid data: malformed Vorbis audio packet");
}

fn real_packets() -> (OggVorbis, Vec<Vec<u8>>) {
    let ogg = OggVorbis::parse(&open().bytes(&stereo().ogg())).unwrap();
    let mut packets: Vec<Vec<u8>> = ogg.headers.to_vec();
    packets.extend((0..ogg.packets.len()).map(|i| ogg.packet(i).to_vec()));
    (ogg, packets)
}

fn last_granule() -> i64 {
    stereo().frames() as i64
}

fn one_line(e: crate::media::Error) -> String {
    let s = e.to_string();
    assert!(!s.contains('\n'), "{s}");
    s
}

#[test]
fn a_packet_before_the_headers_is_refused_in_one_line() {
    let (ogg, packets) = real_packets();
    let audio = ogg.packet(1);
    let e = Decoder::from_headers(audio, &ogg.headers[1], &ogg.headers[2])
        .err()
        .unwrap();
    assert_eq!(
        one_line(e),
        "invalid data: Vorbis audio packet where a header was expected"
    );
    let mut moved = packets.clone();
    let a = moved.remove(4);
    moved.insert(1, a);
    let file = ogg_file(&moved, last_granule());
    let e = decode(&file).err().unwrap();
    assert_eq!(
        one_line(e),
        "invalid data: Vorbis audio packet where a header was expected"
    );
    let audio_first = ogg_file(&packets[3..], last_granule());
    let e = decode(&audio_first).err().unwrap();
    assert_eq!(
        one_line(e),
        "invalid data: no Vorbis stream in the Ogg file"
    );
}

#[test]
fn a_corrupted_audio_packet_is_refused_in_one_line() {
    let (_, packets) = real_packets();
    let mut bad = packets.clone();
    bad[6][0] |= 1;
    let e = decode(&ogg_file(&bad, last_granule())).err().unwrap();
    assert_eq!(
        one_line(e),
        "invalid data: Vorbis header where an audio packet was expected"
    );
    let good = decode(&ogg_file(&packets, last_granule())).unwrap();
    assert_eq!(good.frames() as i64, last_granule());
}

#[test]
fn random_damage_to_audio_packets_never_panics() {
    let (ogg, _) = real_packets();
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..40 {
        let mut dec = ogg.decoder().unwrap();
        let mut out = Vec::new();
        for i in 0..ogg.packets.len() {
            let mut p = ogg.packet(i).to_vec();
            if p.is_empty() {
                continue;
            }
            for _ in 0..3 {
                let at = (next() as usize) % p.len();
                p[at] ^= 1 << (next() % 8);
            }
            if next() % 4 == 0 {
                let keep = (next() as usize) % p.len();
                p.truncate(keep);
            }
            let _ = dec.decode_into(&p, &mut out);
        }
        assert!(out.len().is_multiple_of(2));
    }
}

#[test]
fn ogg_damage_is_survived_or_refused_by_name() {
    let (_, packets) = real_packets();
    let file = ogg_file(&packets, last_granule());
    let e = decode(b"not an ogg file at all, only words here")
        .err()
        .unwrap();
    assert_eq!(one_line(e), "invalid data: not an Ogg file");
    let e = decode(&ogg_file(&packets[..2], 0)).err().unwrap();
    assert_eq!(
        one_line(e),
        "invalid data: Ogg Vorbis stream ends before its three headers"
    );
    let headers_only = decode(&ogg_file(&packets[..3], 0)).unwrap();
    assert_eq!(headers_only.frames(), 0);
    let cut = &file[..file.len() * 2 / 3];
    let ogg = OggVorbis::parse(cut).unwrap();
    assert!(!ogg.warnings.is_empty());
    let part = ogg.decode().unwrap();
    assert!((part.frames() as i64) < last_granule() && part.frames() > 0);
    let mut flipped = file.clone();
    let last = flipped.len() - 10;
    flipped[last] ^= 0xff;
    let ogg = OggVorbis::parse(&flipped).unwrap();
    assert!(
        ogg.warnings.iter().any(|w| w.starts_with("bad CRC")),
        "{:?}",
        ogg.warnings
    );
    assert!(ogg.decode().unwrap().frames() > 0);
    for cut in (0..file.len()).step_by(97) {
        let _ = decode(&file[..cut]);
    }
}

#[test]
fn granules_set_the_lead_and_the_end() {
    assert_eq!(span(&[], &[]), (0, None));
    assert_eq!(span(&[0, 576, 1024], &[None, None, None]), (0, None));
    assert_eq!(
        span(&[0, 576, 1024, 1024], &[None, None, Some(1600), Some(2000)]),
        (0, Some(2000))
    );
    assert_eq!(
        span(&[0, 576, 1024, 1024], &[None, Some(476), None, Some(2000)]),
        (100, Some(2100))
    );
    assert_eq!(
        span(&[0, 576, 1024, 1024], &[None, Some(1576), None, Some(3000)]),
        (0, Some(2000))
    );
    assert_eq!(
        span(&[0, 576, 1024, 1024], &[None, None, Some(1600), None]),
        (0, None)
    );
    assert_eq!(
        span(&[0, 576, 1024], &[None, None, Some(1500)]),
        (0, Some(1500))
    );
    assert_eq!(span(&[0, 576], &[None, Some(9_999_999)]), (0, Some(576)));
    assert_eq!(span(&[0, 576], &[Some(0), Some(-50)]), (0, Some(0)));
    assert_eq!(
        span(&[0, 576], &[Some(i64::MIN), Some(i64::MAX)]),
        (576, Some(576))
    );
}

#[test]
fn xiph_laced_codec_private_splits_into_three_headers() {
    let ident = vec![1u8; 30];
    let comment = vec![3u8; 300];
    let setup = vec![5u8; 40];
    let mut private = vec![2u8, 30, 255, 45];
    private.extend_from_slice(&ident);
    private.extend_from_slice(&comment);
    private.extend_from_slice(&setup);
    let [i, c, s] = xiph_headers(&private).unwrap();
    assert_eq!((i, c, s), (&ident[..], &comment[..], &setup[..]));
    let refused = "invalid data: Vorbis track without its three headers in CodecPrivate";
    for bad in [
        &[][..],
        &[1, 30][..],
        &[2][..],
        &[2, 255][..],
        &[2, 30, 255][..],
        &private[..private.len() - 41],
    ] {
        assert_eq!(
            xiph_headers(bad).err().unwrap().to_string(),
            refused,
            "{bad:?}"
        );
    }
    let empty_setup = &private[..private.len() - 40];
    assert!(xiph_headers(empty_setup).unwrap()[2].is_empty());
}

#[test]
fn sniff_wants_a_vorbis_ident_on_the_first_page() {
    let (_, packets) = real_packets();
    assert!(ogg::sniff(&ogg_file(&packets, last_granule())));
    assert!(!ogg::sniff(&ogg_file(&packets[3..], last_granule())));
    assert!(!ogg::sniff(b"OggS"));
    assert!(!ogg::sniff(&[0; 64]));
}
