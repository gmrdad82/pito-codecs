use super::crc::crc;
use super::inflate::adler32;
use super::tests::expected;
use super::{XORIG, XSPC, YORIG, YSPC, decode, info};
use crate::media::Error;
use std::io::Write;
use std::process::{Command, Stdio};

struct Lcg(u32);

impl Lcg {
    fn byte(&mut self) -> u8 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 24) as u8
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
}

fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut c = (body.len() as u32).to_be_bytes().to_vec();
    c.extend_from_slice(kind);
    c.extend_from_slice(body);
    c.extend_from_slice(&crc(&[kind, body]).to_be_bytes());
    c
}

fn stored(data: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01];
    let mut blocks = data.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(b) = blocks.next() {
        let n = b.len() as u16;
        z.push(blocks.peek().is_none() as u8);
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(b);
    }
    z.extend_from_slice(&adler32(data).to_be_bytes());
    z
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = (
        (p - a as i16).abs(),
        (p - b as i16).abs(),
        (p - c as i16).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn filtered(kind: u8, line: &[u8], prev: &[u8], bpp: usize) -> Vec<u8> {
    let mut out = vec![kind];
    for i in 0..line.len() {
        let a = if i >= bpp { line[i - bpp] } else { 0 };
        let b = prev[i];
        let c = if i >= bpp { prev[i - bpp] } else { 0 };
        let pred = match kind {
            0 => 0,
            1 => a,
            2 => b,
            3 => ((a as u16 + b as u16) / 2) as u8,
            _ => paeth(a, b, c),
        };
        out.push(line[i].wrapping_sub(pred));
    }
    out
}

fn samples(color: u8) -> usize {
    match color {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        _ => 4,
    }
}

struct Image {
    w: u32,
    h: u32,
    color: u8,
    depth: u8,
    interlaced: bool,
    key: Vec<u8>,
}

impl Image {
    fn new(w: u32, h: u32, color: u8, depth: u8, interlaced: bool) -> Self {
        Image {
            w,
            h,
            color,
            depth,
            interlaced,
            key: Vec::new(),
        }
    }

    fn ihdr(&self) -> Vec<u8> {
        let mut b = self.w.to_be_bytes().to_vec();
        b.extend_from_slice(&self.h.to_be_bytes());
        b.extend_from_slice(&[self.depth, self.color, 0, 0, self.interlaced as u8]);
        b
    }

    fn rows(&self, rng: &mut Lcg) -> Vec<u8> {
        let bits = samples(self.color) * self.depth as usize;
        let bpp = bits.div_ceil(8);
        let (w, h) = (self.w as usize, self.h as usize);
        let passes: Vec<(usize, usize)> = if self.interlaced {
            (0..7)
                .map(|p| {
                    (
                        w.saturating_sub(XORIG[p]).div_ceil(XSPC[p]),
                        h.saturating_sub(YORIG[p]).div_ceil(YSPC[p]),
                    )
                })
                .collect()
        } else {
            vec![(w, h)]
        };
        let mut out = Vec::new();
        for (pw, ph) in passes {
            if pw == 0 || ph == 0 {
                continue;
            }
            let stride = (pw * bits).div_ceil(8);
            let mut prev = vec![0u8; stride];
            for y in 0..ph {
                let mut line = rng.bytes(stride);
                if self.color == 3 && self.depth == 8 {
                    line.iter_mut().for_each(|v| *v %= 200);
                }
                if out.is_empty() && y == 0 {
                    line[..self.key.len()].copy_from_slice(&self.key);
                }
                out.extend(filtered((y % 5) as u8, &line, &prev, bpp));
                prev = line;
            }
        }
        out
    }

    fn file(&self, rng: &mut Lcg, before_idat: &[Vec<u8>]) -> Vec<u8> {
        let rows = self.rows(rng);
        self.wrap(&rows, before_idat)
    }

    fn wrap(&self, rows: &[u8], before_idat: &[Vec<u8>]) -> Vec<u8> {
        let z = stored(rows);
        let mut f = super::SIGNATURE.to_vec();
        f.extend(chunk(b"IHDR", &self.ihdr()));
        for c in before_idat {
            f.extend_from_slice(c);
        }
        let third = z.len() / 3;
        f.extend(chunk(b"IDAT", &z[..third]));
        f.extend(chunk(b"IDAT", &[]));
        f.extend(chunk(b"IDAT", &z[third..]));
        f.extend(chunk(b"IEND", &[]));
        f
    }
}

fn palette(rng: &mut Lcg) -> Vec<u8> {
    chunk(b"PLTE", &rng.bytes(200 * 3))
}

const CLAIMED: [(u8, &[u8]); 5] = [
    (0, &[1, 2, 4, 8, 16]),
    (2, &[8, 16]),
    (3, &[1, 2, 4, 8]),
    (4, &[8, 16]),
    (6, &[8, 16]),
];

const SIZES: [(u32, u32); 5] = [(1, 1), (2, 3), (5, 1), (9, 7), (17, 11)];

#[test]
fn every_colour_type_depth_and_interlace_matches_the_png_crate() {
    let mut rng = Lcg(7);
    for (color, depths) in CLAIMED {
        for &depth in depths {
            for interlaced in [false, true] {
                for (w, h) in SIZES {
                    let img = Image::new(w, h, color, depth, interlaced);
                    let extra = if color == 3 {
                        vec![palette(&mut rng)]
                    } else {
                        vec![]
                    };
                    let file = img.file(&mut rng, &extra);
                    let name =
                        format!("colour {color} depth {depth} {w}x{h} interlaced {interlaced}");
                    let i = info(&file).unwrap();
                    assert_eq!(
                        (i.width, i.height, i.depth, i.interlaced),
                        (w, h, depth, interlaced)
                    );
                    let ours = decode(&file).unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!((ours.width, ours.height), (w, h), "{name}");
                    assert!(ours.data == expected(&file), "{name}: pixels differ");
                }
            }
        }
    }
}

#[test]
fn transparency_keys_match_the_png_crate() {
    let mut rng = Lcg(21);
    let cases: [(u8, u8, Vec<u8>); 7] = [
        (0, 1, vec![0, 1]),
        (0, 2, vec![0, 2]),
        (0, 4, vec![0, 9]),
        (0, 16, vec![0x12, 0x34]),
        (2, 8, vec![0, 7, 0, 7, 0, 7]),
        (2, 16, vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60]),
        (3, 8, vec![0, 128, 255, 3]),
    ];
    for (color, depth, key) in cases {
        for interlaced in [false, true] {
            let mut extra = vec![];
            if color == 3 {
                extra.push(palette(&mut rng));
            }
            extra.push(chunk(b"tRNS", &key));
            let mut img = Image::new(13, 9, color, depth, interlaced);
            if depth >= 8 && color != 3 {
                img.key = if depth == 16 {
                    key.clone()
                } else {
                    key.iter().skip(1).step_by(2).copied().collect()
                };
            }
            let file = img.file(&mut rng, &extra);
            let name = format!("colour {color} depth {depth} interlaced {interlaced}");
            let ours = decode(&file).unwrap_or_else(|e| panic!("{name}: {e}"));
            let want = expected(&file);
            assert!(ours.data == want, "{name}: pixels differ");
            if color != 3 {
                assert!(
                    want.chunks_exact(4).any(|p| p[3] == 0),
                    "{name}: key never hit"
                );
            }
        }
    }
}

#[test]
fn bad_crcs_refuse_critical_chunks_and_drop_ancillary_ones() {
    let mut rng = Lcg(3);
    let img = Image::new(6, 4, 3, 8, false);
    let extra = vec![
        palette(&mut rng),
        chunk(b"tRNS", &[0; 200]),
        chunk(b"tEXt", b"Comment\0x"),
    ];
    let good = img.file(&mut rng, &extra);
    assert!(
        decode(&good)
            .unwrap()
            .data
            .chunks_exact(4)
            .all(|p| p[3] == 0)
    );
    let crc_at = |kind: &[u8; 4]| {
        let at = good.windows(4).position(|w| w == kind).unwrap() - 4;
        let len = u32::from_be_bytes(good[at..at + 4].try_into().unwrap()) as usize;
        at + 8 + len
    };
    for kind in [b"IHDR", b"PLTE", b"IDAT", b"IEND"] {
        let mut bad = good.clone();
        bad[crc_at(kind)] ^= 0x40;
        match decode(&bad) {
            Err(Error::Invalid(m)) => assert!(m.contains("CRC"), "{m}"),
            other => panic!("{kind:?}: {other:?}"),
        }
    }
    let mut bad = good.clone();
    bad[crc_at(b"IHDR")] ^= 1;
    match info(&bad) {
        Err(Error::Invalid(m)) => assert!(m.contains("CRC on IHDR"), "{m}"),
        other => panic!("info on a bad IHDR CRC: {other:?}"),
    }
    assert!(info(&good[..29]).is_err());
    assert!(info(&good[..33]).is_ok());
    let mut bad = good.clone();
    bad[crc_at(b"tRNS")] ^= 1;
    assert!(
        decode(&bad)
            .unwrap()
            .data
            .chunks_exact(4)
            .all(|p| p[3] == 255)
    );
    let mut bad = good.clone();
    bad[crc_at(b"tEXt")] ^= 1;
    assert_eq!(decode(&bad).unwrap().data, decode(&good).unwrap().data);
    let at = good.windows(4).position(|w| w == b"IDAT").unwrap();
    let len = u32::from_be_bytes(good[at - 4..at].try_into().unwrap()) as usize;
    let mut z = good[at + 4..at + 4 + len].to_vec();
    assert!(z.len() > 4);
    z[3] ^= 0xFF;
    let mut body = good[..at - 4].to_vec();
    body.extend(chunk(b"IDAT", &z));
    body.extend_from_slice(&good[at + 8 + len..]);
    let e = decode(&body).unwrap_err().to_string();
    assert!(e.contains("length check"), "{e}");
    assert!(decode(&good[..good.len() - 2]).is_err());
}

#[test]
fn header_and_layout_faults_are_refused_by_name() {
    let mut rng = Lcg(5);
    let rgb = Image::new(4, 4, 2, 8, false);
    let with = |ihdr: &[u8], rest: &[Vec<u8>]| {
        let mut f = super::SIGNATURE.to_vec();
        f.extend(chunk(b"IHDR", ihdr));
        for c in rest {
            f.extend_from_slice(c);
        }
        f
    };
    let ihdr = |w: u32, h: u32, color: u8, depth: u8, comp: u8, filt: u8, il: u8| {
        let mut b = w.to_be_bytes().to_vec();
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[depth, color, comp, filt, il]);
        b
    };
    let idat = chunk(b"IDAT", &stored(&rgb.rows(&mut rng)));
    let end = chunk(b"IEND", &[]);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "unknown colour type 5",
            with(&ihdr(4, 4, 5, 8, 0, 0, 0), &[]),
        ),
        (
            "bit depth 4 with colour type 2",
            with(&ihdr(4, 4, 2, 4, 0, 0, 0), &[]),
        ),
        (
            "bit depth 16 with colour type 3",
            with(&ihdr(4, 4, 3, 16, 0, 0, 0), &[]),
        ),
        (
            "bit depth 3 with colour type 0",
            with(&ihdr(4, 4, 0, 3, 0, 0, 0), &[]),
        ),
        (
            "unknown interlace method 2",
            with(&ihdr(4, 4, 2, 8, 0, 0, 2), &[]),
        ),
        ("unknown compression", with(&ihdr(4, 4, 2, 8, 1, 0, 0), &[])),
        ("unknown compression", with(&ihdr(4, 4, 2, 8, 0, 1, 0), &[])),
        ("zero width", with(&ihdr(0, 4, 2, 8, 0, 0, 0), &[])),
        (
            "too large",
            with(&ihdr(1 << 15, 1 << 14, 2, 8, 0, 0, 0), &[]),
        ),
        (
            "no image data",
            with(&ihdr(4, 4, 2, 8, 0, 0, 0), std::slice::from_ref(&end)),
        ),
        (
            "before any image data",
            with(&ihdr(4, 4, 2, 8, 0, 0, 0), &[]),
        ),
        (
            "PLTE chunk of the wrong length",
            with(&ihdr(4, 4, 3, 8, 0, 0, 0), &[chunk(b"PLTE", &[0; 4])]),
        ),
        (
            "PLTE chunk of the wrong length",
            with(&ihdr(4, 4, 3, 8, 0, 0, 0), &[chunk(b"PLTE", &[0; 771])]),
        ),
        (
            "without PLTE",
            with(&ihdr(4, 4, 3, 8, 0, 0, 0), &[idat.clone(), end.clone()]),
        ),
        (
            "runs past the end",
            with(
                &ihdr(4, 4, 2, 8, 0, 0, 0),
                &[idat[..idat.len() - 6].to_vec()],
            ),
        ),
        (
            "ends early",
            with(&ihdr(4, 5, 2, 8, 0, 0, 0), &[idat.clone(), end.clone()]),
        ),
        ("first chunk is not", {
            let mut f = super::SIGNATURE.to_vec();
            f.extend(chunk(b"IHDX", &ihdr(4, 4, 2, 8, 0, 0, 0)));
            f
        }),
        ("not a PNG", b"GIF89a\0\0\0\0\0\0".to_vec()),
    ];
    for (want, file) in cases {
        match decode(&file) {
            Err(Error::Invalid(m)) => assert!(m.contains(want), "{want}: got {m}"),
            other => panic!("{want}: {other:?}"),
        }
    }
    let mut rows = rgb.rows(&mut rng);
    rows[13] = 5;
    let f = with(&rgb.ihdr(), &[chunk(b"IDAT", &stored(&rows)), end]);
    match decode(&f) {
        Err(Error::Invalid(m)) => assert!(m.contains("unknown filter type 5"), "{m}"),
        other => panic!("filter 5: {other:?}"),
    }
}

#[test]
fn palette_indices_past_the_palette_are_opaque_black() {
    let img = Image::new(4, 1, 3, 8, false);
    let rows = vec![0u8, 0, 1, 2, 3];
    let mut f = super::SIGNATURE.to_vec();
    f.extend(chunk(b"IHDR", &img.ihdr()));
    f.extend(chunk(b"PLTE", &[10, 20, 30, 40, 50, 60]));
    f.extend(chunk(b"tRNS", &[7]));
    f.extend(chunk(b"IDAT", &stored(&rows)));
    f.extend(chunk(b"IEND", &[]));
    let out = decode(&f).unwrap();
    assert_eq!(
        out.data,
        [10, 20, 30, 7, 40, 50, 60, 255, 0, 0, 0, 255, 0, 0, 0, 255]
    );
}

#[test]
fn corrupted_and_truncated_files_never_panic() {
    let mut rng = Lcg(77);
    for (color, depth, interlaced) in [(3, 2, true), (6, 16, false), (0, 1, true)] {
        let img = Image::new(11, 7, color, depth, interlaced);
        let extra = if color == 3 {
            vec![palette(&mut rng)]
        } else {
            vec![]
        };
        let good = img.file(&mut rng, &extra);
        for cut in 0..good.len() {
            assert!(
                decode(&good[..cut]).is_err() || cut >= good.len() - 12,
                "cut {cut}"
            );
        }
        let rows = img.rows(&mut rng);
        for at in 0..rows.len() {
            for v in [0, 1, 4, 5, 0xFF, rows[at] ^ 0x08] {
                let mut bad = rows.clone();
                bad[at] = v;
                let _ = decode(&img.wrap(&bad, &extra));
                let _ = decode(&img.wrap(&bad[..at], &extra));
            }
        }
        for at in 8..good.len() {
            for v in [0x00, 0xFF, good[at] ^ 0x10] {
                let mut bad = good.clone();
                bad[at] = v;
                let _ = decode(&bad);
            }
        }
    }
}

fn ffmpeg_raw(file: &[u8], pix_fmt: &str) -> Option<Vec<u8>> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-f", "png_pipe", "-i", "-", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(file).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn like_ffmpeg(file: &[u8], color: u8, depth: u8, name: &str) {
    let ours = decode(file).unwrap_or_else(|e| panic!("{name}: {e}"));
    if depth < 16 {
        let want = ffmpeg_raw(file, "rgba").unwrap_or_else(|| panic!("{name}: ffmpeg failed"));
        assert!(ours.data == want, "{name}: differs from ffmpeg");
        return;
    }
    let wide = ffmpeg_raw(file, "rgba64be").unwrap_or_else(|| panic!("{name}: ffmpeg failed"));
    assert_eq!(wide.len(), ours.data.len() * 2, "{name}: sizes");
    let slack = if matches!(color, 0 | 4) { 1 } else { 0 };
    for (i, (&o, w)) in ours.data.iter().zip(wide.chunks_exact(2)).enumerate() {
        assert!(
            o.abs_diff(w[0]) <= slack,
            "{name}: byte {i} is {o}, ffmpeg's high byte {}",
            w[0]
        );
    }
}

#[test]
fn every_claimed_type_and_depth_decodes_like_ffmpeg() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return;
    }
    let mut rng = Lcg(99);
    for (color, depths) in CLAIMED {
        for &depth in depths {
            for interlaced in [false, true] {
                let img = Image::new(17, 11, color, depth, interlaced);
                let extra = if color == 3 {
                    vec![palette(&mut rng), chunk(b"tRNS", &rng.bytes(64))]
                } else {
                    vec![]
                };
                let file = img.file(&mut rng, &extra);
                let name = format!("colour {color} depth {depth} interlaced {interlaced}");
                like_ffmpeg(&file, color, depth, &name);
            }
        }
    }
}

#[test]
fn transparency_keys_decode_like_ffmpeg() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return;
    }
    let mut rng = Lcg(31);
    let cases: [(u8, u8, Vec<u8>); 7] = [
        (0, 1, vec![0, 1]),
        (0, 2, vec![0, 2]),
        (0, 4, vec![0, 9]),
        (0, 8, vec![0, 77]),
        (0, 16, vec![0x12, 0x34]),
        (2, 8, vec![0, 7, 0, 7, 0, 7]),
        (2, 16, vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60]),
    ];
    for (color, depth, key) in cases {
        for interlaced in [false, true] {
            let mut img = Image::new(13, 9, color, depth, interlaced);
            if depth >= 8 {
                img.key = if depth == 16 {
                    key.clone()
                } else {
                    key.iter().skip(1).step_by(2).copied().collect()
                };
            }
            let file = img.file(&mut rng, &[chunk(b"tRNS", &key)]);
            let name = format!("tRNS colour {color} depth {depth} interlaced {interlaced}");
            assert!(
                decode(&file)
                    .unwrap()
                    .data
                    .chunks_exact(4)
                    .any(|p| p[3] == 0),
                "{name}: key never hit"
            );
            if color == 0 && depth < 8 {
                let want =
                    ffmpeg_raw(&file, "rgba").unwrap_or_else(|| panic!("{name}: ffmpeg failed"));
                let ours = decode(&file).unwrap();
                assert_eq!(ours.data.len(), want.len(), "{name}");
                for (a, b) in ours.data.chunks_exact(4).zip(want.chunks_exact(4)) {
                    assert_eq!(
                        (&a[..3], b[3]),
                        (&b[..3], 255),
                        "{name}: ffmpeg keeps sub-byte grey opaque"
                    );
                }
                continue;
            }
            like_ffmpeg(&file, color, depth, &name);
        }
    }
}
