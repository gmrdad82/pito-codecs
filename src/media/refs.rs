#![allow(dead_code)]

use crate::testref::{Sound, pcm};

pub const AREA: &str = "media";

pub const PCM_RATE: u32 = 44_100;
pub const PCM_FRAMES: usize = 256;
pub const PCM_CASES: [(&str, &str); 14] = [
    ("mkv", "pcm_s24le"),
    ("mkv", "pcm_s32be"),
    ("mkv", "pcm_f32le"),
    ("mkv", "pcm_f64le"),
    ("mkv", "pcm_u8"),
    ("mov", "pcm_s16le"),
    ("mov", "pcm_s16be"),
    ("mov", "pcm_s24le"),
    ("mov", "pcm_s32le"),
    ("mov", "pcm_f32be"),
    ("mov", "pcm_f64le"),
    ("mp4", "pcm_s16le"),
    ("mp4", "pcm_s24be"),
    ("mp4", "pcm_f32le"),
];

pub fn pcm_file(ext: &str, codec: &str) -> String {
    format!("pcm-{codec}.{ext}")
}

fn pcm_tone(i: u64, ch: u16, rate: u32) -> f32 {
    let t = i as f64 / f64::from(rate);
    if ch == 0 {
        (0.5 * (std::f64::consts::TAU * 440.0 * t).sin()) as f32
    } else {
        Sound::Noise { amp: 0.5, seed: 7 }.at(i, 0, rate)
    }
}

pub fn pcm_source() -> Vec<f32> {
    pcm(Sound::Fn(pcm_tone), PCM_RATE, 2, 0, PCM_FRAMES)
}

pub fn f32le(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

pub const AC3_AAC: &str = "h264-ac3-aac.mkv";
pub const AC3_AAC_ARGS: &str = "-f|lavfi|-i|testsrc2=s=64x64:r=10:d=1|-f|lavfi|-i|sine=f=440:r=48000:d=1\
     |-f|lavfi|-i|sine=f=880:r=48000:d=1|-map|0:v|-map|1:a|-map|2:a|-c:v|libx264|-preset|ultrafast\
     |-c:a:0|ac3|-b:a:0|64k|-c:a:1|aac|-b:a:1|32k";

pub const WINDOW: usize = 256;

pub fn window(file: &str, at: &str) -> String {
    format!("{file}.{at}.f32")
}

pub fn decoded(file: &str) -> String {
    format!("{file}.f32")
}

pub const LINEUP_RATE: u32 = 48_000;
pub const LINEUP_FRAMES: usize = 48_000;
pub const LINEUP_AT: [usize; 2] = [10_000, 25_440];
pub const LINEUPS: [(&str, &str); 2] = [("tones-aac.mkv", "aac"), ("tones-opus.webm", "libopus")];

fn lineup_tone(i: u64, ch: u16, rate: u32) -> f32 {
    let hz = if ch == 0 { 440.0 } else { 1250.0 };
    let t = i as f64 / f64::from(rate);
    (0.125 * (std::f64::consts::TAU * hz * t).sin()) as f32
}

pub fn lineup_source() -> Vec<f32> {
    pcm(Sound::Fn(lineup_tone), LINEUP_RATE, 2, 0, LINEUP_FRAMES)
}

pub const OFF_MS_SECONDS: f64 = 1.0123;
pub const OFF_MS_HEAD: usize = 2_000;
pub const OFF_MS: [(&str, &str, u32, &str, f32); 4] = [
    ("off-ms-opus.webm", "libopus", 48_000, "32k", 2e-3),
    ("off-ms-opus.mkv", "libopus", 48_000, "32k", 2e-3),
    ("off-ms-ac3.mkv", "ac3", 48_000, "64k", 1e-3),
    ("off-ms-mp3.mkv", "libmp3lame", 44_100, "32k", 1e-3),
];
pub const OFF_MS_AAC: &str = "off-ms-aac.mkv";

pub fn off_ms_frames(rate: u32) -> usize {
    (OFF_MS_SECONDS * f64::from(rate)).round() as usize
}

pub const VP9_ALTREF: &str = "vp9-g10.webm";
pub const VP9_ALTREF_ARGS: &str = "-f|lavfi|-i|testsrc2=s=352x240:r=30:d=2|-c:v|libvpx-vp9\
     |-deadline|good|-cpu-used|4|-auto-alt-ref|1|-lag-in-frames|16|-b:v|60k|-g|10|-keyint_min|10\
     |-pix_fmt|yuv420p";

pub const PYRAMID: &str = "pyramid-hevc.mp4";
pub const PYRAMID_FRAMES: usize = 45;
pub const PYRAMID_ARGS: &str = "-f|lavfi|-i|testsrc2=s=352x240:r=30:d=1.5|-c:v|libx265|-preset|ultrafast\
     |-x265-params|log-level=error:keyint=60:min-keyint=60:scenecut=0:bframes=4:b-pyramid=1:crf=35\
     |-pix_fmt|yuv420p|-tag:v|hvc1";

const BT709: &str = "-colorspace|bt709|-color_primaries|bt709|-color_trc|bt709";

pub const AV1_709: &str = "av1-709.mp4";
pub const VP9_709: &str = "vp9-709.mp4";

pub fn tagged_709() -> [(&'static str, String); 2] {
    [
        (
            AV1_709,
            format!(
                "-f|lavfi|-i|testsrc2=s=352x240:r=30:d=1|-c:v|libsvtav1|-preset|12|-crf|55|-g|30\
                 |-pix_fmt|yuv420p|{BT709}"
            ),
        ),
        (
            VP9_709,
            format!(
                "-f|lavfi|-i|testsrc2=s=352x240:r=30:d=1|-c:v|libvpx-vp9|-deadline|realtime\
                 |-cpu-used|8|-b:v|100k|-g|30|-pix_fmt|yuv420p|{BT709}"
            ),
        ),
    ]
}

pub type Key = ((u32, u32), &'static str, &'static str, &'static str, &'static str);

pub const VP9_KEYS: [Key; 4] = [
    ((848, 480), "bt709", "bt709", "tv", "bt601"),
    ((848, 480), "bt709", "bt709", "pc", "bt601"),
    ((1920, 1080), "bt470bg", "bt601", "tv", "bt709"),
    ((1920, 1080), "bt470bg", "bt601", "pc", "bt709"),
];

pub fn vp9_key(h: u32, space: &str, range: &str) -> String {
    format!("vp9-key-{h}-{space}-{range}.mkv")
}

pub fn vp9_key_args(size: (u32, u32), space: &str, range: &str) -> String {
    format!(
        "-f|lavfi|-i|smptebars=s={}x{}:r=10|-frames:v|1|-c:v|libvpx-vp9|-deadline|realtime\
         |-cpu-used|8|-g|10|-pix_fmt|yuv420p|-colorspace|{space}|-color_range|{range}",
        size.0, size.1
    )
}

pub const AV1S: [(&str, &str, u8); 4] = [
    ("av1-8.mp4", "yuv420p", 8),
    ("av1-8.mkv", "yuv420p", 8),
    ("av1-8.webm", "yuv420p", 8),
    ("av1-10.mp4", "yuv420p10le", 10),
];
pub const AV1_FRAMES: usize = 10;

pub fn av1_args(pix: &str) -> String {
    format!(
        "-f|lavfi|-i|testsrc2=s=352x240:r=10:d=1|-c:v|libsvtav1|-preset|12|-crf|55|-g|10\
         |-pix_fmt|{pix}"
    )
}

pub const VARIANT_SOURCE: &str = "-f|lavfi|-i|testsrc2=s=64x64:r=30:d=0.1";
const X264_422: &str = "-c:v|libx264|-pix_fmt|yuv422p10le|-profile:v|high422";
const X264_10: &str = "-c:v|libx264|-pix_fmt|yuv420p10le|-profile:v|high10";
const X265_422: &str = "-c:v|libx265|-pix_fmt|yuv422p10le|-x265-params|log-level=error";
const X265_10: &str = "-c:v|libx265|-pix_fmt|yuv420p10le|-x265-params|log-level=error";
const VPX_444: &str = "-c:v|libvpx-vp9|-deadline|realtime|-cpu-used|8|-pix_fmt|yuv444p";
const VPX_10: &str = "-c:v|libvpx-vp9|-deadline|realtime|-cpu-used|8|-pix_fmt|yuv420p10le";

pub const VARIANTS: [(&str, &str); 25] = [
    ("h264-yuv422p10le.mp4", X264_422),
    ("h264-yuv422p10le.mkv", X264_422),
    ("h264-yuv420p10le.mp4", X264_10),
    ("h264-yuv420p10le.mkv", X264_10),
    (
        "h264-interlaced.mp4",
        "-c:v|libx264|-pix_fmt|yuv420p|-flags|+ildct+ilme|-x264opts|tff=1",
    ),
    ("hevc-10bit-422.mp4", X265_422),
    ("hevc-10bit-422.mkv", X265_422),
    ("hevc-main10.mp4", X265_10),
    ("hevc-main10.mkv", X265_10),
    ("vp9-444.webm", VPX_444),
    ("vp9-444.mp4", VPX_444),
    ("vp9-10bit.webm", VPX_10),
    ("vp9-10bit.mp4", VPX_10),
    ("mpeg4-part2.mp4", "-c:v|mpeg4|-f|mp4"),
    ("mpeg4-part2.mkv", "-c:v|mpeg4|-f|matroska"),
    ("mpeg2.mp4", "-c:v|mpeg2video|-f|mp4"),
    ("mpeg2.mkv", "-c:v|mpeg2video|-f|matroska"),
    ("mjpeg.mov", "-c:v|mjpeg|-pix_fmt|yuvj420p|-f|mov"),
    ("mjpeg.mkv", "-c:v|mjpeg|-pix_fmt|yuvj420p|-f|matroska"),
    ("mpeg4.avi", "-c:v|mpeg4|-f|avi"),
    ("mpeg2.ts", "-c:v|mpeg2video|-f|mpegts"),
    (
        "mpeg2.m2ts",
        "-c:v|mpeg2video|-mpegts_m2ts_mode|1|-f|mpegts",
    ),
    ("flv1.flv", "-c:v|flv1|-f|flv"),
    ("mpeg2.mpg", "-c:v|mpeg2video|-f|vob"),
    ("wmv2.wmv", "-c:v|wmv2|-f|asf"),
];

pub const VP9_PROFILE2: &str = "vp9-profile2-g10.webm";
pub const VP9_PROFILE2_ARGS: &str = "-f|lavfi|-i|testsrc2=s=352x240:r=30:d=1|-c:v|libvpx-vp9\
     |-pix_fmt|yuv420p10le|-g|10|-keyint_min|10|-auto-alt-ref|0|-lag-in-frames|0|-crf|55|-b:v|0";

pub const MP3: &str = "bare-sine.mp3";
pub const MP3_ARGS: &str =
    "-f|lavfi|-i|sine=f=440:r=44100:d=1|-ac|2|-c:a|libmp3lame|-b:a|64k";

pub fn split(args: &str) -> Vec<String> {
    args.split('|').map(str::to_string).collect()
}
