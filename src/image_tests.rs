use crate::image::{self, Format, Output};
use crate::media::Rgba;
use crate::png::{self, Color, Compression, Options, Picture, Transform};
use std::io::Cursor;

fn photo(w: u32, h: u32, seed: u32) -> Rgba {
    let mut img = Rgba::new(w, h);
    let mut s = seed;
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
        let n = (s >> 26) as f32;
        px[0] = (128.0 + 90.0 * (x * 0.07).sin() + n) as u8;
        px[1] = (128.0 + 90.0 * (y * 0.05).cos() + n) as u8;
        px[2] = ((x + y) * 1.3 + n) as u8;
        px[3] = if (x as u32 / 5 + y as u32 / 7) % 4 == 0 {
            90
        } else {
            255
        };
    }
    img
}

fn theirs(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let img = ::image::load_from_memory(bytes).unwrap().to_rgba8();
    (img.width(), img.height(), img.into_raw())
}

#[test]
fn png_and_jpeg_open_by_their_bytes_like_load_from_memory() {
    let src = photo(61, 47, 3);
    let png_file = image::encode(&src, Output::PngRgba).unwrap();
    assert_eq!(image::sniff(&png_file), Some(Format::Png));
    let (w, h, raw) = theirs(&png_file);
    let ours = image::decode(&png_file).unwrap();
    assert_eq!((ours.width, ours.height), (w, h));
    assert!(ours.data == raw && ours.data == src.data);
    let jpeg_file = image::encode(&src, Output::Jpeg(88)).unwrap();
    assert_eq!(image::sniff(&jpeg_file), Some(Format::Jpeg));
    let (w, h, raw) = theirs(&jpeg_file);
    let ours = image::decode(&jpeg_file).unwrap();
    assert_eq!((ours.width, ours.height), (w, h));
    assert!(ours.data == raw);
    assert!(image::decode(b"GIF89a").is_err());
    assert!(image::decode(&[]).is_err());
}

#[test]
fn dimensions_match_image_dimensions_without_decoding() {
    let src = photo(33, 21, 4);
    for output in [Output::Png, Output::PngRgba, Output::Jpeg(70)] {
        let file = image::encode(&src, output).unwrap();
        let want = ::image::ImageReader::new(Cursor::new(&file))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(image::dimensions(&file).unwrap(), want, "{output:?}");
    }
}

#[test]
fn files_written_here_open_in_image_and_files_opened_here_were_written_there() {
    let dir = crate::testref::scratch("image");
    std::fs::create_dir_all(&dir).unwrap();
    let src = photo(48, 32, 5);
    for (name, output) in [("a.png", Output::PngRgba), ("b.jpg", Output::Jpeg(92))] {
        let path = dir.join(name);
        image::save(&path, &src, output).unwrap();
        let there = ::image::open(&path).unwrap().to_rgba8();
        let here = image::open(&path).unwrap();
        assert!(here.data == there.into_raw(), "{name}");
    }
    let path = dir.join("c.png");
    ::image::RgbaImage::from_raw(48, 32, src.data.clone())
        .unwrap()
        .save(&path)
        .unwrap();
    assert!(image::open(&path).unwrap().data == src.data);
    assert!(image::open(&dir.join("missing.png")).is_err());
}

#[test]
fn a_sixteen_bit_picture_rounds_to_eight_the_way_to_rgba8_does() {
    let mut samples = Vec::new();
    for v in (0..65536u32).step_by(37) {
        samples.extend_from_slice(&[v as u16, (65535 - v) as u16, (v / 2) as u16, 65535]);
    }
    let w = (samples.len() / 4) as u32;
    let pic = Picture::from_u16(w, 1, Color::Rgba, &samples).unwrap();
    let file = png::encode_picture(
        &pic,
        &Options {
            compression: Compression::Balanced,
            ..Options::default()
        },
    )
    .unwrap();
    let (_, _, raw) = theirs(&file);
    let decoded = png::Decoder::new()
        .decode_picture(&file, Transform::Expand)
        .unwrap();
    assert!(decoded.to_rgba_rounded().unwrap().data == raw);
    assert!(decoded.to_rgba().unwrap().data != raw);
}

#[test]
fn rgba_helpers_crop_replace_and_index_like_the_image_crate() {
    let a = photo(30, 20, 6);
    let b = photo(9, 7, 7);
    let theirs_a = ::image::RgbaImage::from_raw(30, 20, a.data.clone()).unwrap();
    let theirs_b = ::image::RgbaImage::from_raw(9, 7, b.data.clone()).unwrap();
    let cropped = a.crop(4, 5, 12, 9);
    let want = ::image::imageops::crop_imm(&theirs_a, 4, 5, 12, 9).to_image();
    assert_eq!(
        (cropped.width, cropped.height),
        (want.width(), want.height())
    );
    assert!(cropped.data == want.into_raw());
    let edge = a.crop(25, 15, 99, 99);
    let want = ::image::imageops::crop_imm(&theirs_a, 25, 15, 99, 99).to_image();
    assert_eq!((edge.width, edge.height), (want.width(), want.height()));
    assert!(edge.data == want.into_raw());
    for (x, y) in [(0i64, 0i64), (3, 4), (-2, -3), (25, 16), (-20, 5), (40, 40)] {
        let mut ours = a.clone();
        ours.replace(&b, x, y);
        let mut there = theirs_a.clone();
        ::image::imageops::replace(&mut there, &theirs_b, x, y);
        assert!(ours.data == there.into_raw(), "at {x},{y}");
    }
    assert_eq!(a.pixel(7, 3), Some(theirs_a.get_pixel(7, 3).0));
    assert_eq!(a.pixel(30, 3), None);
    let mut c = a.clone();
    assert!(c.put(2, 2, [1, 2, 3, 4]));
    assert!(!c.put(2, 20, [1, 2, 3, 4]));
    assert_eq!(c.pixel(2, 2), Some([1, 2, 3, 4]));
    assert!(Rgba::from_raw(2, 2, vec![0; 15]).is_none());
    assert!(Rgba::from_raw(2, 2, vec![0; 16]).is_some());
}
