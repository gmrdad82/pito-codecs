use super::*;
use crate::media::Packet;
use std::io::Cursor;
use std::sync::Arc;

const TREES: [&[u8; 4]; 7] = [
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"dinf",
];

#[derive(Clone, Debug)]
enum Node {
    Leaf([u8; 4], Vec<u8>),
    Tree([u8; 4], Vec<Node>),
}

fn kind(n: &Node) -> [u8; 4] {
    match n {
        Node::Leaf(k, _) | Node::Tree(k, _) => *k,
    }
}

fn parse(d: &[u8]) -> Vec<Node> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= d.len() {
        let size32 = u32::from_be_bytes(d[p..p + 4].try_into().unwrap()) as usize;
        let k: [u8; 4] = d[p + 4..p + 8].try_into().unwrap();
        let (hl, size) = match size32 {
            0 => (8, d.len() - p),
            1 => (
                16,
                u64::from_be_bytes(d[p + 8..p + 16].try_into().unwrap()) as usize,
            ),
            s => (8, s),
        };
        let body = &d[p + hl..p + size];
        out.push(if TREES.contains(&&k) {
            Node::Tree(k, parse(body))
        } else {
            Node::Leaf(k, body.to_vec())
        });
        p += size;
    }
    out
}

fn write(nodes: &[Node], large: &[&[u8; 4]], open: Option<&[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::new();
    for n in nodes {
        let k = kind(n);
        let body = match n {
            Node::Leaf(_, b) => b.clone(),
            Node::Tree(_, c) => write(c, large, None),
        };
        if open == Some(&k) {
            out.extend(0u32.to_be_bytes());
            out.extend(k);
        } else if large.contains(&&k) {
            out.extend(1u32.to_be_bytes());
            out.extend(k);
            out.extend((16 + body.len() as u64).to_be_bytes());
        } else {
            out.extend((8 + body.len() as u32).to_be_bytes());
            out.extend(k);
        }
        out.extend(body);
    }
    out
}

fn visit(nodes: &mut [Node], f: &mut impl FnMut(&mut Node)) {
    for n in nodes {
        f(n);
        if let Node::Tree(_, c) = n {
            visit(c, f);
        }
    }
}

fn chunk_offsets(stco: &[u8]) -> Vec<u64> {
    let n = u32::from_be_bytes(stco[4..8].try_into().unwrap()) as usize;
    (0..n)
        .map(|i| u32::from_be_bytes(stco[8 + 4 * i..12 + 4 * i].try_into().unwrap()) as u64)
        .collect()
}

fn co64(offsets: &[u64]) -> Vec<u8> {
    let mut b = vec![0; 4];
    b.extend((offsets.len() as u32).to_be_bytes());
    for o in offsets {
        b.extend(o.to_be_bytes());
    }
    b
}

fn mdat_payload(d: &[u8]) -> usize {
    let at = d.windows(4).position(|w| w == b"mdat").unwrap();
    let size32 = u32::from_be_bytes(d[at - 4..at].try_into().unwrap());
    at + if size32 == 1 { 12 } else { 4 }
}

fn avc() -> AvcConfig {
    AvcConfig::new(
        vec![vec![0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50]],
        vec![vec![0x68, 0xEB, 0xE3, 0xCB]],
    )
}

fn packet(i: i64) -> Packet {
    Packet {
        data: vec![0, 0, 0, 3, 0x65, i as u8, 7, i as u8],
        pts: i * 3000,
        dts: i * 3000,
        keyframe: i % 3 == 0,
    }
}

fn plain() -> Vec<u8> {
    let mut w = Writer::new(
        Cursor::new(Vec::new()),
        Options {
            brand: Brand::Mp4,
            reserve: 0,
        },
    )
    .unwrap();
    let v = w
        .add_track(TrackConfig::new(SampleEntry::h264(avc(), 64, 48), 90_000))
        .unwrap();
    for i in 0..6 {
        w.push(v, &packet(i)).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn fragmented() -> Vec<u8> {
    let mut w = FragmentedWriter::new(Vec::new(), 0.02);
    let v = w
        .add_track(TrackConfig::new(SampleEntry::h264(avc(), 64, 48), 90_000))
        .unwrap();
    for i in 0..6 {
        w.push(v, &packet(i)).unwrap();
    }
    w.finish().unwrap()
}

fn frames<S: ByteSource>(r: &Reader<S>) -> Vec<(i64, i64, bool, Vec<u8>)> {
    (0..r.tracks[0].samples.len())
        .map(|i| {
            let p = r.packet(0, i).unwrap();
            (p.pts, p.dts, p.keyframe, p.data)
        })
        .collect()
}

fn rebuild(
    base: &[u8],
    order: &[&[u8; 4]],
    large: &[&[u8; 4]],
    open: Option<&[u8; 4]>,
    to_co64: bool,
) -> Vec<u8> {
    let tops = parse(base);
    let nodes: Vec<Node> = order
        .iter()
        .filter_map(|k| tops.iter().find(|n| &kind(n) == *k).cloned())
        .collect();
    let old = mdat_payload(base) as i64;
    let mut shift = 0i64;
    loop {
        let mut copy = nodes.clone();
        visit(&mut copy, &mut |n| {
            let moved: Option<Vec<u64>> = match n {
                Node::Leaf(k, b) if k == b"stco" => Some(
                    chunk_offsets(b)
                        .iter()
                        .map(|&o| (o as i64 + shift) as u64)
                        .collect(),
                ),
                _ => None,
            };
            if let Some(moved) = moved {
                *n = if to_co64 {
                    Node::Leaf(*b"co64", co64(&moved))
                } else {
                    let mut s = vec![0; 4];
                    s.extend((moved.len() as u32).to_be_bytes());
                    for o in moved {
                        s.extend((o as u32).to_be_bytes());
                    }
                    Node::Leaf(*b"stco", s)
                };
            }
        });
        let out = write(&copy, large, open);
        let now = mdat_payload(&out) as i64 - old;
        if now == shift {
            return out;
        }
        shift = now;
    }
}

#[test]
fn large_sizes_co64_and_an_open_mdat_read_the_same_frames() {
    let base = plain();
    let want = frames(&Reader::new(base.as_slice()).unwrap());
    assert_eq!(want.len(), 6);
    let variants = [
        rebuild(
            &base,
            &[b"ftyp", b"mdat", b"moov"],
            &[b"mdat", b"moov"],
            None,
            true,
        ),
        rebuild(
            &base,
            &[b"ftyp", b"moov", b"mdat"],
            &[b"trak", b"stbl"],
            Some(b"mdat"),
            true,
        ),
        rebuild(
            &base,
            &[b"ftyp", b"moov", b"mdat"],
            &[],
            Some(b"mdat"),
            false,
        ),
    ];
    for (i, d) in variants.iter().enumerate() {
        let r = Reader::new(d.as_slice()).unwrap_or_else(|e| panic!("variant {i}: {e}"));
        assert_eq!(frames(&r), want, "variant {i}");
    }
    assert!(variants[0].windows(4).any(|w| w == b"co64"));
}

#[test]
fn an_empty_stts_still_reads_every_frame() {
    let mut nodes = parse(&plain());
    visit(&mut nodes, &mut |n| {
        if let Node::Leaf(k, b) = n
            && k == b"stts"
        {
            *b = vec![0; 8];
        }
    });
    let d = write(&nodes, &[], None);
    let r = Reader::new(d.as_slice()).unwrap();
    let t = &r.tracks[0];
    assert_eq!(t.samples.len(), 6);
    assert_eq!(r.read(0, 5).unwrap(), packet(5).data);
}

#[test]
fn a_fragmented_file_reads_every_frame_once() {
    let d = fragmented();
    let r = Reader::new(d.as_slice()).unwrap();
    assert!(r.fragmented);
    let p = plain();
    let want = frames(&Reader::new(p.as_slice()).unwrap());
    assert_eq!(frames(&r), want);
    let data: Vec<Vec<u8>> = want.into_iter().map(|f| f.3).collect();
    assert_eq!(data, (0..6).map(|i| packet(i).data).collect::<Vec<_>>());
}

#[test]
fn box_sizes_below_their_header_are_refused() {
    let base = plain();
    for k in [b"moov", b"ftyp"] {
        let at = base.windows(4).position(|w| w == k).unwrap() - 4;
        for bad in [2u32, 7] {
            let mut d = base.clone();
            d[at..at + 4].copy_from_slice(&bad.to_be_bytes());
            assert!(Reader::new(d.as_slice()).is_err(), "{k:?} size {bad}");
        }
    }
    let mut d = base.clone();
    d[0..4].copy_from_slice(&1u32.to_be_bytes());
    assert!(Reader::new(d.as_slice()).is_err(), "ftyp largesize");
    let mut stsc = parse(&base);
    visit(&mut stsc, &mut |n| {
        if let Node::Leaf(k, b) = n
            && k == b"stsc"
        {
            b[8..12].copy_from_slice(&0u32.to_be_bytes());
        }
    });
    assert!(Reader::new(write(&stsc, &[], None).as_slice()).is_err());
    let mut stz2 = parse(&base);
    visit(&mut stz2, &mut |n| {
        if let Node::Leaf(k, _) = n
            && k == b"stsz"
        {
            *n = Node::Leaf(*b"stz2", vec![0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 1]);
        }
    });
    assert!(Reader::new(write(&stz2, &[], None).as_slice()).is_err());
    assert!(Reader::new(&b"\0\0\0\x08free"[..]).is_err());
}

#[test]
fn stz2_sizes_read_at_every_field_width() {
    for (field, packed) in [
        (16u32, vec![0u8, 8, 0, 8, 0, 8, 0, 8, 0, 8, 0, 8]),
        (8, vec![8; 6]),
        (4, vec![0x88; 3]),
    ] {
        let mut nodes = parse(&plain());
        visit(&mut nodes, &mut |n| {
            if let Node::Leaf(k, _) = n
                && k == b"stsz"
            {
                let mut b = vec![0; 4];
                b.extend(field.to_be_bytes());
                b.extend(6u32.to_be_bytes());
                b.extend(&packed);
                *n = Node::Leaf(*b"stz2", b);
            }
        });
        let d = write(&nodes, &[], None);
        let r = Reader::new(d.as_slice()).unwrap();
        assert_eq!(r.tracks[0].samples.len(), 6, "field {field}");
        assert_eq!(r.read(0, 3).unwrap(), packet(3).data, "field {field}");
    }
}

#[test]
fn truncated_and_corrupted_copies_never_panic() {
    for base in [plain(), fragmented()] {
        for cut in 0..base.len() {
            if let Ok(r) = Reader::new(&base[..cut]) {
                for ti in 0..r.tracks.len() {
                    for (i, s) in r.tracks[ti].samples.iter().enumerate() {
                        let got = r.packet(ti, i);
                        if s.offset + s.size as u64 > cut as u64 {
                            assert!(
                                got.is_err(),
                                "cut {cut}: track {ti} sample {i} past the end"
                            );
                        } else {
                            let want =
                                &base[s.offset as usize..(s.offset + s.size as u64) as usize];
                            assert_eq!(got.unwrap().data, want, "cut {cut}: track {ti} sample {i}");
                        }
                    }
                }
            }
        }
        for at in 0..base.len() {
            for v in [0x00, 0xFF, 0x01, base[at] ^ 0x80] {
                let mut d = base.clone();
                d[at] = v;
                if let Ok(r) = Reader::new(d.as_slice()) {
                    let _ = (r.duration_seconds(), r.video(), r.audio());
                    for ti in 0..r.tracks.len() {
                        for i in 0..r.tracks[ti].samples.len() {
                            let _ = r.packet(ti, i);
                        }
                    }
                }
            }
        }
    }
    assert!(Reader::new(&[][..]).is_err());
}

#[test]
fn byte_sources_read_exactly_or_fail_at_the_end() {
    let data: Vec<u8> = (0..32u8).collect();
    let check = |s: &dyn ByteSource| {
        assert_eq!(s.len(), 32);
        assert!(!s.is_empty());
        let mut b = [0u8; 4];
        s.read_at(28, &mut b).unwrap();
        assert_eq!(b, [28, 29, 30, 31]);
        let e = s.read_at(29, &mut b).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
        assert!(s.read_at(u64::MAX, &mut b).is_err());
        assert!(s.read_at(u64::MAX - 1, &mut b).is_err());
        s.read_at(32, &mut []).unwrap();
    };
    check(&data.as_slice());
    check(&data);
    check(&&data);
    check(&Arc::new(data.clone()));
    check(&Box::new(data.clone()));
    let empty: &[u8] = &[];
    assert!(empty.is_empty());
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tmp/test-mp4/source-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bytes");
    std::fs::write(&path, &data).unwrap();
    let f = std::fs::File::open(&path).unwrap();
    check(&f);
    drop(f);
    std::fs::remove_dir_all(&dir).unwrap();
}
