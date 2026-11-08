use pito_codecs::h264::{Encoder, EncoderConfig};
use pito_codecs::media::{Frame, PixelFormat, TimeBase};
use pito_codecs::threads;

fn encode_clip(threads: usize) {
    let (w, h) = (128u32, 128u32);
    let mut enc = Encoder::new(EncoderConfig {
        width: w,
        height: h,
        threads,
        ..Default::default()
    })
    .unwrap();
    for n in 0..6 {
        let f = Frame::new(PixelFormat::Yuv420p8, w, h, n, TimeBase::new(1, 30));
        enc.encode(&f).unwrap();
    }
    enc.flush().unwrap();
}

#[test]
fn encoder_threads_enter_only_when_opted_in() {
    assert!(!threads::below_normal());
    encode_clip(4);
    assert_eq!(threads::entered(), 0);

    threads::set_below_normal(true);
    encode_clip(4);
    threads::set_below_normal(false);
    assert!(threads::entered() >= 4);
}
