use super::AudioBuffer;
use super::wav::{self, Format, Reader, Writer};
use std::io::Cursor;

fn tone(channels: u16, frames: usize) -> Vec<f32> {
    (0..frames * usize::from(channels))
        .map(|i| {
            let t = (i / usize::from(channels)) as f32;
            let c = (i % usize::from(channels)) as f32;
            ((t * 0.05 + c).sin() * 0.9 + (t * 0.31).cos() * 0.2).clamp(-1.0, 1.0)
        })
        .collect()
}

fn spec(channels: u16, rate: u32, format: Format) -> hound::WavSpec {
    hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: format.bits(),
        sample_format: if format.float() {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    }
}

const FORMATS: [Format; 5] = [
    Format::Int8,
    Format::Int16,
    Format::Int24,
    Format::Int32,
    Format::Float32,
];

fn ints(format: Format, v: f32) -> i32 {
    let v = v.clamp(-1.0, 1.0);
    match format {
        Format::Int8 => (v * 127.0).round() as i32,
        Format::Int16 => (v * 32767.0).round() as i32,
        Format::Int24 => (v * 8_388_607.0).round() as i32,
        _ => (f64::from(v) * 2_147_483_647.0).round() as i32,
    }
}

#[test]
fn what_is_written_is_read_by_hound_in_every_format_and_channel_count() {
    for format in FORMATS {
        for channels in [1u16, 2, 3, 6] {
            let samples = tone(channels, 1001);
            let buffer = AudioBuffer {
                samples: samples.clone(),
                rate: 44_100,
                channels,
            };
            let bytes = wav::encode(&buffer, format).unwrap();
            let mut theirs = hound::WavReader::new(Cursor::new(&bytes)).unwrap();
            let tag = format!("{format:?} x{channels}");
            assert_eq!(theirs.spec(), spec(channels, 44_100, format), "{tag}");
            assert_eq!(theirs.duration(), 1001, "{tag}");
            if format.float() {
                let got: Vec<f32> = theirs.samples::<f32>().map(Result::unwrap).collect();
                assert_eq!(got, samples, "{tag}");
            } else {
                let got: Vec<i32> = theirs.samples::<i32>().map(Result::unwrap).collect();
                let want: Vec<i32> = samples.iter().map(|&v| ints(format, v)).collect();
                assert_eq!(got, want, "{tag}");
            }
        }
    }
}

#[test]
fn what_hound_writes_is_read_whole_and_in_blocks_with_the_same_samples() {
    for format in FORMATS {
        for channels in [1u16, 2, 5] {
            let samples = tone(channels, 777);
            let mut file = Cursor::new(Vec::new());
            {
                let mut w =
                    hound::WavWriter::new(&mut file, spec(channels, 48_000, format)).unwrap();
                for &v in &samples {
                    if format.float() {
                        w.write_sample(v).unwrap();
                    } else {
                        w.write_sample(ints(format, v)).unwrap();
                    }
                }
                w.finalize().unwrap();
            }
            let bytes = file.into_inner();
            let tag = format!("{format:?} x{channels}");
            let mut theirs = hound::WavReader::new(Cursor::new(&bytes)).unwrap();
            let scale = 1.0 / (1u64 << (format.bits() - 1)) as f32;
            let want: Vec<f32> = if format.float() {
                theirs.samples::<f32>().map(Result::unwrap).collect()
            } else {
                theirs
                    .samples::<i32>()
                    .map(|v| v.unwrap() as f32 * scale)
                    .collect()
            };
            let whole = wav::decode(&bytes).unwrap();
            assert_eq!((whole.rate, whole.channels), (48_000, channels), "{tag}");
            assert_eq!(whole.samples, want, "{tag}: decode");
            let mut reader = Reader::new(Cursor::new(&bytes)).unwrap();
            assert_eq!(reader.layout.frames(), 777, "{tag}");
            let mut blocks = Vec::new();
            while reader.read(100, &mut blocks).unwrap() > 0 {}
            assert_eq!(blocks, want, "{tag}: streamed");
            if !format.float() {
                let mut reader = Reader::new(Cursor::new(&bytes)).unwrap();
                let mut got = Vec::new();
                while reader.read_ints(64, &mut got).unwrap() > 0 {}
                let expect: Vec<i32> = theirs_ints(&bytes);
                assert_eq!(got, expect, "{tag}: integers");
            } else {
                let mut reader = Reader::new(Cursor::new(&bytes)).unwrap();
                assert!(reader.read_ints(8, &mut Vec::new()).is_err());
            }
        }
    }
}

fn theirs_ints(bytes: &[u8]) -> Vec<i32> {
    hound::WavReader::new(Cursor::new(bytes))
        .unwrap()
        .samples::<i32>()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_stream_in_odd_blocks_equals_one_write_and_seeks_back_to_the_top() {
    for format in FORMATS {
        let samples = tone(2, 5000);
        let one = wav::encode(
            &AudioBuffer {
                samples: samples.clone(),
                rate: 22_050,
                channels: 2,
            },
            format,
        )
        .unwrap();
        let mut w = Writer::new(Cursor::new(Vec::new()), 22_050, 2, format).unwrap();
        for block in samples.chunks(2 * 333) {
            w.write(block).unwrap();
        }
        assert_eq!(w.frames(), 5000);
        let many = w.finish().unwrap().into_inner();
        assert_eq!(many, one, "{format:?}");
        let mut r = Reader::new(Cursor::new(&many)).unwrap();
        let mut first = Vec::new();
        r.read(10, &mut first).unwrap();
        r.seek(0);
        let mut again = Vec::new();
        r.read(10, &mut again).unwrap();
        assert_eq!(first, again);
        assert_eq!(r.position(), 10);
    }
}

#[test]
fn integer_samples_go_in_as_the_files_own_range_and_clip_at_its_ends() {
    let mut w = Writer::new(Cursor::new(Vec::new()), 8000, 1, Format::Int24).unwrap();
    w.write_ints(&[0, 1, -1, 8_388_607, -8_388_608, i32::MAX, i32::MIN])
        .unwrap();
    let bytes = w.finish().unwrap().into_inner();
    let got = theirs_ints(&bytes);
    assert_eq!(
        got,
        [0, 1, -1, 8_388_607, -8_388_608, 8_388_607, -8_388_608]
    );
    let mut f = Writer::new(Cursor::new(Vec::new()), 8000, 1, Format::Float32).unwrap();
    assert!(f.write_ints(&[1]).is_err());
    assert!(Writer::new(Cursor::new(Vec::new()), 0, 1, Format::Int16).is_err());
    assert!(Writer::new(Cursor::new(Vec::new()), 8000, 0, Format::Int16).is_err());
    let mut s = Writer::new(Cursor::new(Vec::new()), 8000, 2, Format::Int16).unwrap();
    assert!(s.write(&[0.0; 3]).is_err());
}

#[test]
fn an_odd_data_chunk_is_padded_and_the_file_still_reads() {
    let mut w = Writer::new(Cursor::new(Vec::new()), 8000, 1, Format::Int8).unwrap();
    w.write(&[0.0, 0.5, -0.5]).unwrap();
    let bytes = w.finish().unwrap().into_inner();
    assert_eq!(bytes.len() % 2, 0);
    assert_eq!(
        hound::WavReader::new(Cursor::new(&bytes))
            .unwrap()
            .duration(),
        3
    );
    assert_eq!(wav::decode(&bytes).unwrap().samples.len(), 3);
}

#[test]
fn hostile_wav_files_never_panic_the_streaming_reader() {
    let good = wav::encode(
        &AudioBuffer {
            samples: tone(2, 300),
            rate: 8000,
            channels: 2,
        },
        Format::Int24,
    )
    .unwrap();
    let mut s = 3u32;
    for round in 0..2000 {
        let mut bad = good.clone();
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let at = (s as usize >> 8) % bad.len();
        bad[at] = (s >> 3) as u8;
        if round % 4 == 0 {
            bad.truncate(at);
        }
        if let Ok(mut r) = Reader::new(Cursor::new(&bad)) {
            let mut out = Vec::new();
            let _ = r.read(1 << 20, &mut out);
            let mut ints = Vec::new();
            r.seek(0);
            let _ = r.read_ints(1 << 20, &mut ints);
        }
        let _ = wav::decode(&bad);
    }
}
