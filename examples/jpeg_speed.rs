use pito_codecs::jpeg;
use std::time::Instant;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

fn best(rounds: usize, mut f: impl FnMut()) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..rounds {
        let start = Instant::now();
        f();
        best = best.min(start.elapsed().as_secs_f64() * 1e3);
    }
    best
}

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let ours = best(200, || {
            std::hint::black_box(jpeg::decode(std::hint::black_box(&bytes)).unwrap());
        });
        let theirs = best(200, || {
            let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
            let mut d = JpegDecoder::new_with_options(ZCursor::new(&bytes), options);
            std::hint::black_box(d.decode().unwrap());
        });
        println!("{path}: pito-codecs {ours:.2} ms, zune-jpeg {theirs:.2} ms");
    }
}
