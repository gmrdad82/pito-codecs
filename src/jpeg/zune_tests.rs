use crate::testref;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

fn zune(bytes: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let pixels = d.decode().ok()?;
    let (w, h) = d.dimensions()?;
    Some((w, h, pixels))
}

fn worst(a: &[u8], b: &[u8]) -> (u8, f64) {
    let mut max = 0u8;
    let mut sum = 0u64;
    for (x, y) in a.iter().zip(b) {
        let d = x.abs_diff(*y);
        max = max.max(d);
        sum += u64::from(d);
    }
    (max, sum as f64 / a.len() as f64)
}

#[test]
fn every_reference_file_decodes_like_zune_jpeg_to_within_the_upsampling_filters() {
    let dir = testref::area_dir("jpeg");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
        .collect();
    files.sort();
    let mut compared = 0;
    let mut odd = Vec::new();
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let Some((w, h, theirs)) = zune(&bytes) else {
            continue;
        };
        let ours = crate::jpeg::decode(&bytes).unwrap();
        assert_eq!(
            (ours.width as usize, ours.height as usize),
            (w, h),
            "{path:?}"
        );
        let (max, _) = worst(&ours.data, &theirs);
        if max != 0 {
            odd.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
        compared += 1;
    }
    assert!(compared > 100, "{compared} files");
    assert!(odd.iter().all(|n| n.contains("scans")), "{odd:?}");
}

#[test]
fn a_1080p_webcam_frame_decodes_like_zune_jpeg_in_every_sampling() {
    let (w, h) = (1920u32, 1080u32);
    let mut image = crate::media::Rgba::new(w, h);
    let mut s = 7u32;
    for (i, px) in image.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((i % w as usize) as f32, (i / w as usize) as f32);
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let n = ((s >> 24) as f32 - 128.0) * 0.08;
        px[0] = (128.0 + 100.0 * (x * 0.01).sin() + n).clamp(0.0, 255.0) as u8;
        px[1] = (128.0 + 100.0 * (y * 0.013).cos() + n).clamp(0.0, 255.0) as u8;
        px[2] = (128.0 + 60.0 * ((x + y) * 0.007).sin() + n).clamp(0.0, 255.0) as u8;
        px[3] = 255;
    }
    let file = crate::jpeg::encode(&image, 90).unwrap();
    let (zw, zh, theirs) = zune(&file).unwrap();
    let ours = crate::jpeg::decode(&file).unwrap();
    assert_eq!((ours.width as usize, ours.height as usize), (zw, zh));
    let (max, mean) = worst(&ours.data, &theirs);
    assert_eq!((max, mean), (0, 0.0));
}

#[test]
fn a_size_off_the_block_grid_differs_from_zune_jpeg_only_on_the_last_row_and_column() {
    let (w, h) = (40u32, 30u32);
    let mut image = crate::media::Rgba::new(w, h);
    let mut s = 5u32;
    for (i, px) in image.data.chunks_exact_mut(4).enumerate() {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let x = (i as u32 % w) as f32;
        px.copy_from_slice(&[
            (128.0 + 90.0 * (x * 0.3).sin() + (s >> 26) as f32) as u8,
            (128.0 + 60.0 * (x * 0.1).sin()) as u8,
            (s >> 24) as u8,
            255,
        ]);
    }
    let file = crate::jpeg::encode(&image, 92).unwrap();
    let (_, _, theirs) = zune(&file).unwrap();
    let ours = crate::jpeg::decode(&file).unwrap();
    let mut seen_edge = false;
    for (i, (a, b)) in ours.data.iter().zip(&theirs).enumerate() {
        let x = (i / 4) as u32 % w;
        let y = (i / 4) as u32 / w;
        if a != b {
            assert!(x == w - 1 || y == h - 1, "byte {i}");
            assert!(a.abs_diff(*b) <= 40);
            seen_edge = true;
        }
    }
    assert!(seen_edge);
}

#[test]
fn decoding_into_a_buffer_gives_the_same_pixels_and_keeps_the_allocation() {
    let mut image = crate::media::Rgba::new(64, 48);
    for (i, b) in image.data.iter_mut().enumerate() {
        *b = (i * 7 % 251) as u8;
    }
    let file = crate::jpeg::encode(&image, 85).unwrap();
    let whole = crate::jpeg::decode(&file).unwrap();
    let mut out = crate::media::Rgba::new(128, 96);
    let kept = out.data.as_ptr();
    crate::jpeg::decode_into(&file, &mut out).unwrap();
    assert!(out == whole);
    assert_eq!(out.data.as_ptr(), kept);
    assert!(crate::jpeg::decode_into(&file[..40], &mut out).is_err());
}
