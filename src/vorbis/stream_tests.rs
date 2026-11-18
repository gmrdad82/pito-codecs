use super::decode_tests::refs::{self, CASES};
use super::{Stream, decode};
use crate::testref::{self, Refs};
use std::io::Cursor;
use std::sync::Arc;

fn files() -> Vec<(String, Vec<u8>, u32, u16)> {
    let r = Refs::open(refs::AREA);
    CASES
        .iter()
        .map(|c| (c.name.to_string(), r.bytes(&c.ogg()), c.rate, c.channels))
        .collect()
}

fn collect<R: std::io::Read + std::io::Seek>(s: &mut Stream<R>) -> (Vec<f32>, usize) {
    let mut all = Vec::new();
    let mut blocks = 0;
    while let Some(b) = s.next_block().unwrap() {
        assert!(!b.is_empty());
        all.extend_from_slice(b);
        blocks += 1;
    }
    (all, blocks)
}

#[test]
fn blocks_join_into_the_whole_decode_sample_for_sample_on_every_reference_file() {
    for (name, data, rate, channels) in files() {
        let whole = decode(&data).unwrap();
        let mut s = Stream::from_bytes(&data).unwrap();
        assert_eq!((s.rate(), s.channels()), (rate, channels), "{name}");
        assert_eq!(s.total_frames(), Some(whole.frames() as u64), "{name}");
        let (all, blocks) = collect(&mut s);
        assert!(all == whole.samples, "{name}: samples differ");
        assert!(blocks > 4, "{name}: {blocks} blocks");
        assert_eq!(s.position(), whole.frames() as u64, "{name}");
        assert!(s.next_block().unwrap().is_none());
    }
}

#[test]
fn the_stream_lands_on_libvorbisfiles_samples_as_the_whole_decode_does() {
    let r = Refs::open(refs::AREA);
    for c in &CASES {
        let data = r.bytes(&c.ogg());
        let mut s = Stream::from_bytes(&data).unwrap();
        let (all, _) = collect(&mut s);
        let mine = refs::decimate(&all, c.channels, c.stride());
        let theirs = r.f32s(&refs::every(&c.ogg()));
        let d = testref::diff(&mine, &theirs);
        testref::assert_within(c.name, &d, refs::MAX_DIFF, refs::MAX_RMS);
    }
}

#[test]
fn the_stream_agrees_with_lewton_on_rate_channels_length_and_samples() {
    let mut compared = 0;
    for (name, data, rate, channels) in files() {
        let mut theirs = lewton::inside_ogg::OggStreamReader::new(Cursor::new(&data[..])).unwrap();
        assert_eq!(theirs.ident_hdr.audio_sample_rate, rate, "{name}");
        assert_eq!(
            u16::from(theirs.ident_hdr.audio_channels),
            channels,
            "{name}"
        );
        let mut pcm: Vec<i16> = Vec::new();
        while let Some(p) = theirs.read_dec_packet_itl().unwrap() {
            pcm.extend(p);
        }
        let end = theirs.get_last_absgp().unwrap() as usize * usize::from(channels);
        pcm.truncate(end);
        let mut s = Stream::from_bytes(&data).unwrap();
        let (all, _) = collect(&mut s);
        assert_eq!(all.len(), pcm.len(), "{name}: length");
        if channels > 2 {
            continue;
        }
        compared += 1;
        let worst = all
            .iter()
            .zip(&pcm)
            .map(|(&a, &b)| (a * 32768.0 - f32::from(b)).abs())
            .fold(0.0f32, f32::max);
        assert!(worst <= 1.5, "{name}: {worst} steps from lewton");
    }
    assert!(compared >= 3);
}

#[test]
fn a_stream_starts_again_from_the_top_at_any_point() {
    let (name, data, _, _) = files().remove(5);
    let mut s = Stream::from_bytes(&data).unwrap();
    let (first, _) = collect(&mut s);
    s.rewind().unwrap();
    assert_eq!(s.position(), 0);
    let (second, _) = collect(&mut s);
    assert!(first == second, "{name}: after the end");
    s.rewind().unwrap();
    for _ in 0..7 {
        s.next_block().unwrap().unwrap();
    }
    s.rewind().unwrap();
    let (third, _) = collect(&mut s);
    assert!(first == third, "{name}: from the middle");
}

#[test]
fn any_read_and_seek_source_streams_the_same_samples() {
    let (_, data, _, _) = files().remove(2);
    let want = decode(&data).unwrap().samples;
    let dir = testref::scratch(refs::AREA);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stream-source.ogg");
    std::fs::write(&path, &data).unwrap();
    let mut file = Stream::open(&path).unwrap();
    assert!(collect(&mut file).0 == want);
    let mut owned = Stream::new(Cursor::new(data.clone())).unwrap();
    assert!(collect(&mut owned).0 == want);
    let shared: Arc<[u8]> = data.into();
    let mut arc = Stream::new(Cursor::new(shared)).unwrap();
    assert!(collect(&mut arc).0 == want);
}

fn page(flags: u8, granule: i64, seq: u32, packets: &[&[u8]]) -> Vec<u8> {
    let mut lacing = Vec::new();
    for p in packets {
        lacing.extend(std::iter::repeat_n(255u8, p.len() / 255));
        lacing.push((p.len() % 255) as u8);
    }
    assert!(lacing.len() <= 255);
    let mut out = b"OggS".to_vec();
    out.push(0);
    out.push(flags);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&7u32.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.push(lacing.len() as u8);
    out.extend_from_slice(&lacing);
    for p in packets {
        out.extend_from_slice(p);
    }
    let crc = super::ogg::page_crc(&out);
    out[22..26].copy_from_slice(&crc.to_le_bytes());
    out
}

fn repage(data: &[u8], per: usize, shift: i64) -> Vec<u8> {
    let ogg = super::OggVorbis::parse(data).unwrap();
    let dec = ogg.decoder().unwrap();
    let packets: Vec<&[u8]> = (0..ogg.packets.len()).map(|i| ogg.packet(i)).collect();
    let end = ogg.samples().unwrap() as i64;
    let mut out = page(2, 0, 0, &[&ogg.headers[0]]);
    out.extend(page(0, 0, 1, &[&ogg.headers[1], &ogg.headers[2]]));
    let mut cum = 0i64;
    let mut prev: Option<usize> = None;
    let mut seq = 2;
    let groups: Vec<&[&[u8]]> = packets.chunks(per).collect();
    for (k, group) in groups.iter().enumerate() {
        for p in group.iter() {
            let n = dec.blocksize(p).unwrap();
            cum += prev.map_or(0, |q| (q / 4 + n / 4) as i64);
            prev = Some(n);
        }
        let last = k + 1 == groups.len();
        let granule = if last { end - shift } else { cum - shift };
        out.extend(page(if last { 4 } else { 0 }, granule, seq, group));
        seq += 1;
    }
    out
}

#[test]
fn a_multi_page_file_with_a_lead_streams_like_the_whole_decode() {
    let (_, data, _, _) = files().remove(1);
    for per in [1usize, 2, 5, 11] {
        for shift in [0i64, 300] {
            let paged = repage(&data, per, shift);
            let whole = super::OggVorbis::parse(&paged).unwrap().decode().unwrap();
            let mut s = Stream::from_bytes(&paged).unwrap();
            let (all, blocks) = collect(&mut s);
            assert!(all == whole.samples, "per {per} shift {shift}");
            assert_eq!(
                s.total_frames(),
                Some(whole.frames() as u64),
                "per {per} shift {shift}"
            );
            assert!(blocks > 4);
            s.rewind().unwrap();
            assert!(
                collect(&mut s).0 == whole.samples,
                "rewound, per {per} shift {shift}"
            );
        }
    }
}

#[test]
fn a_stream_cut_short_ends_on_its_last_whole_page_and_a_headerless_one_is_refused() {
    let (_, data, _, _) = files().remove(1);
    assert!(Stream::from_bytes(&[]).is_err());
    assert!(Stream::from_bytes(b"RIFF....WAVE").is_err());
    assert!(Stream::from_bytes(&data[..100]).is_err());
    let paged = repage(&data, 2, 0);
    let cut = &paged[..paged.len() * 3 / 4];
    let mut s = Stream::from_bytes(cut).unwrap();
    let mut n = 0;
    loop {
        match s.next_block() {
            Ok(Some(_)) => n += 1,
            Ok(None) => break,
            Err(e) => {
                assert!(e.to_string().contains("Ogg Vorbis"), "{e}");
                break;
            }
        }
    }
    assert!(n > 2);
    assert!(s.position() < decode(&data).unwrap().frames() as u64);
}

#[test]
fn hostile_streams_never_panic() {
    let (_, data, _, _) = files().remove(4);
    let mut s = 11u32;
    for round in 0..600 {
        let mut bad = data.clone();
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let at = (s as usize >> 8) % bad.len();
        bad[at] ^= 1 << (round % 8);
        if round % 5 == 0 {
            bad.truncate(at.max(1));
        }
        if let Ok(mut st) = Stream::from_bytes(&bad) {
            let mut guard = 0;
            while let Ok(Some(_)) = st.next_block() {
                guard += 1;
                assert!(guard < 100_000);
            }
            let _ = st.rewind();
        }
    }
}
