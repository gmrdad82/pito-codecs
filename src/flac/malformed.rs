use super::tests::{Sub, frame_bytes, info16, wave};
use super::*;
use std::io::Cursor;

const ROUNDS: usize = 3000;

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

fn frames(rng: &mut Rng) -> Vec<Vec<u8>> {
    let n = 64 + rng.below(400);
    let a = wave(n, 30000, rng.below(50) as i32);
    let b = wave(n, 20000, rng.below(50) as i32);
    let small: Vec<i32> = a.iter().map(|v| v >> 4).collect();
    vec![
        frame_bytes(
            0,
            false,
            n,
            1,
            &[(Sub::Verbatim(a.clone()), 0), (Sub::Constant(7), 0)],
        ),
        frame_bytes(
            1,
            true,
            n & !3,
            10,
            &[
                (
                    Sub::Fixed {
                        order: 2,
                        samples: a[..n & !3].iter().map(|v| v >> 1).collect(),
                        param: None,
                        partitions: 2,
                    },
                    0,
                ),
                (
                    Sub::Lpc {
                        qlp: vec![1500, -600, 40, 9],
                        precision: 12,
                        shift: 10,
                        samples: b[..n & !3].to_vec(),
                        param: Some(16),
                    },
                    0,
                ),
            ],
        ),
        frame_bytes(
            2,
            false,
            n,
            9,
            &[
                (
                    Sub::Fixed {
                        order: 0,
                        samples: small,
                        param: Some(6),
                        partitions: 0,
                    },
                    4,
                ),
                (Sub::Verbatim(b), 0),
            ],
        ),
    ]
}

fn mutate(rng: &mut Rng, f: &mut Vec<u8>) {
    if f.is_empty() {
        f.push(rng.next() as u8);
        return;
    }
    match rng.below(5) {
        0 => {
            let at = rng.below(f.len());
            f[at] ^= 1 << rng.below(8);
        }
        1 => {
            let at = rng.below(f.len());
            f[at] = rng.next() as u8;
        }
        2 => f.truncate(rng.below(f.len())),
        3 => {
            let at = 4 + rng.below(f.len().saturating_sub(4));
            for b in f.iter_mut().skip(at).take(1 + rng.below(16)) {
                *b = rng.next() as u8;
            }
        }
        _ => {
            let at = rng.below(f.len());
            let extra: Vec<u8> = (0..1 + rng.below(32)).map(|_| rng.next() as u8).collect();
            f.splice(at..at, extra);
        }
    }
}

fn reseal(f: &mut [u8]) {
    if f.len() > 8 {
        let end = f.len() - 2;
        let crc = crc::crc16(&f[..end]);
        f[end..].copy_from_slice(&crc.to_be_bytes());
    }
}

#[test]
fn mutated_frames_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut pcm = Vec::new();
    for round in 0..ROUNDS {
        let mut dec = if round % 3 == 0 {
            Decoder::new(None)
        } else {
            Decoder::new(Some(info16(2)))
        };
        for mut f in frames(&mut rng) {
            for _ in 0..1 + rng.below(3) {
                mutate(&mut rng, &mut f);
            }
            if rng.below(2) == 0 {
                reseal(&mut f);
            }
            let _ = dec.decode_frame(&f);
            let _ = dec.decode_packet(&f, &mut pcm);
            let mut ints = Vec::new();
            dec.interleave_i32(&mut ints);
            let _ = dec.channel(0);
            let _ = dec.channel(9);
        }
    }
}

#[test]
fn resealed_frames_with_random_bodies_never_panic() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut dec = Decoder::new(Some(info16(2)));
    for _ in 0..ROUNDS {
        let mut f = frames(&mut rng).swap_remove(rng.below(3));
        let head = 6 + rng.below(4);
        for b in f.iter_mut().skip(head) {
            if rng.below(4) == 0 {
                *b = rng.next() as u8;
            }
        }
        reseal(&mut f);
        let _ = dec.decode_frame(&f);
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut dec = Decoder::new(Some(info16(1)));
    for _ in 0..ROUNDS {
        let n = rng.below(600);
        let mut f: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        if n >= 2 {
            f[0] = 0xff;
            f[1] = 0xf8 | rng.below(2) as u8;
        }
        let _ = dec.decode_frame(&f);
        let _ = config(&f);
        let _ = StreamInfo::parse(&f);
        let _ = Decoder::from_config(&f);
    }
}

fn file(rng: &mut Rng) -> Vec<u8> {
    let mut data = b"fLaC".to_vec();
    let mut info = info16(2);
    info.samples = rng.below(4000) as u64;
    let fixed = rng.below(2) == 0;
    if fixed {
        info.min_block = 300;
        info.max_block = 300;
    }
    data.extend_from_slice(&[0x00, 0, 0, 34]);
    data.extend_from_slice(&meta::streaminfo_bytes(&info));
    let mut table = Vec::new();
    for k in 0..3u64 {
        table.extend_from_slice(&(k * 300).to_be_bytes());
        table.extend_from_slice(&(rng.below(3000) as u64).to_be_bytes());
        table.extend_from_slice(&300u16.to_be_bytes());
    }
    data.extend_from_slice(&[0x83, 0, 0, table.len() as u8]);
    data.extend_from_slice(&table);
    for k in 0..4 {
        let a = wave(300, 25000, k);
        let number = if fixed { k as u64 } else { k as u64 * 300 };
        data.extend_from_slice(&frame_bytes(
            number,
            !fixed,
            300,
            1,
            &[(Sub::Verbatim(a.clone()), 0), (Sub::Verbatim(a), 0)],
        ));
    }
    data
}

#[test]
fn mutated_files_never_panic_reading_seeking_or_totalling() {
    let mut rng = Rng(0x94D0_49BB_1331_11EB);
    for _ in 0..ROUNDS / 3 {
        let mut data = file(&mut rng);
        for _ in 0..1 + rng.below(4) {
            mutate(&mut rng, &mut data);
        }
        let _ = decode_file(&data);
        let Ok(mut r) = Reader::new(Cursor::new(&data)) else {
            continue;
        };
        let _ = r.total();
        let mut out = Vec::new();
        for _ in 0..3 {
            let at = rng.below(5000) as u64;
            if r.seek(at).is_err() {
                break;
            }
            for _ in 0..3 {
                match r.read_f32(&mut out) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
        let _ = r.read_i32(&mut Vec::new());
    }
}

#[test]
fn the_reader_refuses_headers_that_lie() {
    let mut rng = Rng(7);
    let good = file(&mut rng);
    assert!(decode_file(&good).is_ok());
    let mut not_flac = good.clone();
    not_flac[0] = b'X';
    assert!(Reader::new(Cursor::new(&not_flac)).is_err());
    let mut invalid_block = good.clone();
    invalid_block[4] = 0x7f;
    assert!(Reader::new(Cursor::new(&invalid_block)).is_err());
    let mut short_info = good.clone();
    short_info[7] = 20;
    assert!(decode_file(&short_info).is_err());
    let mut no_info = b"fLaC".to_vec();
    no_info.extend_from_slice(&[0x81, 0, 0, 2, 0, 0]);
    assert!(Reader::new(Cursor::new(&no_info)).is_err());
    let mut huge = good.clone();
    huge[4] = 0x01;
    huge[5] = 0xff;
    huge[6] = 0xff;
    huge[7] = 0xff;
    assert!(Reader::new(Cursor::new(&huge)).is_err());
}
