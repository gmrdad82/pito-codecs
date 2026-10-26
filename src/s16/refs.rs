use crate::testref::Sound;

pub const AREA: &str = "s16";
pub const WINDOW: f64 = 0.1;
pub const LOW_RATE: u32 = 8_000;
pub const STEREO: [&str; 4] = ["aac", "opus", "mp3", "ac3"];

fn chirp(amp: f64, f0: f64, sweep: f64, i: u64, rate: u32) -> f32 {
    let t = i as f64 / f64::from(rate);
    (amp * (std::f64::consts::TAU * (f0 + sweep * t) * t).sin()) as f32
}

pub fn survey_at(i: u64, ch: u16, rate: u32) -> f32 {
    match ch {
        0 => chirp(0.5, 200.0, 100.0, i, rate),
        _ => chirp(0.4, 300.0, 50.0, i, rate),
    }
}

pub fn first_track(i: u64, ch: u16, rate: u32) -> f32 {
    survey_at(i, ch, rate)
}

pub fn second_track(i: u64, ch: u16, rate: u32) -> f32 {
    match ch {
        0 => chirp(0.5, 500.0, 30.0, i, rate),
        _ => chirp(0.4, 800.0, 60.0, i, rate),
    }
}

pub fn mono_track(i: u64, _: u16, rate: u32) -> f32 {
    chirp(0.5, 500.0, 30.0, i, rate)
}

pub fn tone_a(i: u64, _: u16, rate: u32) -> f32 {
    chirp(0.5, 200.0, 100.0, i, rate)
}

pub fn tone_b(i: u64, _: u16, rate: u32) -> f32 {
    chirp(0.5, 500.0, 300.0, i, rate)
}

pub fn five_one_at(i: u64, ch: u16, rate: u32) -> f32 {
    const HZ: [f64; 6] = [220.0, 330.0, 440.0, 55.0, 660.0, 770.0];
    chirp(0.3, HZ[usize::from(ch).min(5)], 0.0, i, rate)
}

pub type Tone = fn(u64, u16, u32) -> f32;

pub const SURVEY_SOUND: Sound = Sound::Fn(survey_at);

#[derive(Debug, Clone)]
pub struct Track {
    pub tone: Tone,
    pub channels: u16,
    pub rate: u32,
    pub seconds: f64,
    pub delay: f64,
    pub codec: &'static str,
    pub bits: u8,
}

impl Track {
    pub fn frames(&self) -> usize {
        (self.seconds * f64::from(self.rate)).round() as usize
    }
}

const fn track(tone: Tone, channels: u16, rate: u32, seconds: f64, codec: &'static str) -> Track {
    Track {
        tone,
        channels,
        rate,
        seconds,
        delay: 0.0,
        codec,
        bits: 16,
    }
}

#[derive(Debug, Clone)]
pub struct Fixture {
    pub file: String,
    pub tracks: Vec<Track>,
}

pub struct Kind {
    pub ext: &'static str,
    pub codec: &'static str,
    pub bits: u8,
    pub lossless: bool,
    pub may_refuse: bool,
}

const fn k(ext: &'static str, codec: &'static str, lossless: bool) -> Kind {
    Kind {
        ext,
        codec,
        bits: 16,
        lossless,
        may_refuse: false,
    }
}

pub const KINDS: &[Kind] = &[
    k("mkv", "aac", false),
    k("mp4", "aac", false),
    k("mov", "aac", false),
    k("3gp", "aac", false),
    k("mkv", "opus", false),
    k("mp4", "opus", false),
    k("mkv", "mp3", false),
    k("mp4", "mp3", false),
    k("mkv", "vorbis", false),
    k("mkv", "ac3", false),
    k("mp4", "ac3", false),
    Kind {
        bits: 24,
        ..k("mkv", "flac", true)
    },
    Kind {
        bits: 24,
        ..k("mp4", "flac", true)
    },
    Kind {
        bits: 32,
        may_refuse: true,
        ..k("mp4", "flac", true)
    },
    k("mkv", "alac", true),
    k("mp4", "alac", true),
    k("mp4", "pcm_s16le", true),
    k("mp4", "pcm_s16be", true),
    k("mp4", "pcm_s24le", true),
    k("mp4", "pcm_s24be", true),
    k("mp4", "pcm_s32le", true),
    k("mp4", "pcm_s32be", true),
    k("mp4", "pcm_f32le", true),
    k("mp4", "pcm_f32be", true),
    k("mkv", "pcm_s16le", true),
    k("mkv", "pcm_s16be", true),
    k("mkv", "pcm_s24le", true),
    k("mkv", "pcm_s24be", true),
    k("mkv", "pcm_s32le", true),
    k("mkv", "pcm_s32be", true),
    k("mkv", "pcm_f32le", true),
    k("mkv", "pcm_alaw", true),
    k("mkv", "pcm_mulaw", true),
    k("mov", "pcm_s16le", true),
    k("mov", "pcm_s16be", true),
    k("mov", "pcm_s24le", true),
    k("mov", "pcm_s24be", true),
    k("mov", "pcm_s32le", true),
    k("mov", "pcm_s32be", true),
    k("mov", "pcm_f32le", true),
    k("mov", "pcm_f32be", true),
    k("mov", "pcm_alaw", true),
    k("mov", "pcm_mulaw", true),
];

impl Kind {
    pub fn law(&self) -> bool {
        self.codec.contains("law")
    }

    pub fn rate(&self) -> u32 {
        if self.lossless { LOW_RATE } else { 48_000 }
    }

    pub fn seconds(&self) -> f64 {
        if self.lossless { 0.02 } else { 0.25 }
    }

    pub fn frames(&self) -> usize {
        (self.seconds() * f64::from(self.rate())).round() as usize
    }

    fn tag(&self) -> String {
        if self.codec == "flac" {
            format!("flac{}", self.bits)
        } else {
            self.codec.to_string()
        }
    }

    pub fn label(&self) -> String {
        format!("{} in {}", self.tag(), self.ext)
    }

    pub fn file(&self) -> String {
        format!("survey-{}.{}", self.tag(), self.ext)
    }

    pub fn mono(&self) -> bool {
        !self.lossless && !STEREO.contains(&self.codec)
    }

    pub fn channels(&self) -> usize {
        if self.mono() { 1 } else { 2 }
    }

    pub fn fixture(&self) -> Fixture {
        Fixture {
            file: self.file(),
            tracks: vec![Track {
                bits: self.bits,
                ..track(
                    survey_at,
                    self.channels() as u16,
                    self.rate(),
                    self.seconds(),
                    self.codec,
                )
            }],
        }
    }
}

pub const DELAYED: f64 = 0.2;
pub const DELAYED_LONG: f64 = 0.8;
pub const DELAYED_SHORT: f64 = 0.4;
pub const DELAYED_FROM: [f64; 2] = [0.3, 0.45];
pub const DELAYED_PAST: f64 = DELAYED + DELAYED_SHORT + 0.1;
pub const MIXED: f64 = 0.3;
pub const MIXED_FROM: f64 = 0.1;
pub const MIXED_THIRD: [(&str, &str); 2] = [("mkv", "pcm_s24le"), ("mp4", "flac")];
pub const FIVE_ONE_CODECS: [(&str, &str); 2] = [("mkv", "flac"), ("mp4", "aac")];
pub const FIVE_ONE_RATES: [u32; 2] = [LOW_RATE, 16_000];
pub const FIVE_ONE_SECONDS: f64 = 0.15;
pub const REFUSED: &str = "refused-wmav2.mkv";
pub const LAW_MOV: &str = "survey-pcm_alaw.mov";

pub fn delayed(ext: &str) -> String {
    format!("delayed-short.{ext}")
}

pub fn mixed(ext: &str) -> String {
    format!("mixed.{ext}")
}

pub fn five_one(ext: &str, codec: &str) -> String {
    format!("five-one-{codec}.{ext}")
}

pub fn mixed_tracks(third: &'static str) -> Vec<Track> {
    vec![
        track(survey_at, 2, 48_000, MIXED, "aac"),
        track(mono_track, 1, 48_000, MIXED, "opus"),
        Track {
            bits: 24,
            ..track(tone_a, 1, LOW_RATE, MIXED, third)
        },
    ]
}

pub fn fixtures() -> Vec<Fixture> {
    let mut out = Vec::new();
    for ext in ["mkv", "mp4"] {
        out.push(Fixture {
            file: delayed(ext),
            tracks: vec![
                track(tone_a, 1, 48_000, DELAYED_LONG, "aac"),
                Track {
                    delay: DELAYED,
                    ..track(tone_b, 1, 48_000, DELAYED_SHORT, "aac")
                },
            ],
        });
    }
    for (ext, third) in MIXED_THIRD {
        out.push(Fixture {
            file: mixed(ext),
            tracks: mixed_tracks(third),
        });
    }
    for (ext, codec) in FIVE_ONE_CODECS {
        out.push(Fixture {
            file: five_one(ext, codec),
            tracks: vec![track(five_one_at, 6, LOW_RATE, FIVE_ONE_SECONDS, codec)],
        });
    }
    out.push(Fixture {
        file: REFUSED.into(),
        tracks: vec![track(tone_a, 1, 44_100, 0.1, "wmav2")],
    });
    out.extend(KINDS.iter().map(Kind::fixture));
    out
}

pub fn alaw(pcm: i16) -> u8 {
    const END: [i32; 8] = [0x1f, 0x3f, 0x7f, 0xff, 0x1ff, 0x3ff, 0x7ff, 0xfff];
    let v = i32::from(pcm) >> 3;
    let (mask, v) = if v >= 0 { (0xd5u8, v) } else { (0x55, -v - 1) };
    let Some(seg) = END.iter().position(|&e| v <= e) else {
        return 0x7f ^ mask;
    };
    let low = if seg < 2 {
        (v >> 1) & 0xf
    } else {
        (v >> seg) & 0xf
    };
    ((seg as u8) << 4 | low as u8) ^ mask
}

pub fn alaw_value(a: u8) -> i16 {
    let a = a ^ 0x55;
    let mut t = i32::from(a & 0xf) << 4;
    let seg = (a & 0x70) >> 4;
    match seg {
        0 => t += 8,
        _ => {
            t += 0x108;
            if seg > 1 {
                t <<= seg - 1;
            }
        }
    }
    (if a & 0x80 != 0 { t } else { -t }) as i16
}

pub fn mulaw(pcm: i16) -> u8 {
    const BIAS: i32 = 0x84;
    let v = i32::from(pcm);
    let (sign, v) = if v < 0 { (0x80u8, -v) } else { (0, v) };
    let v = v.min(32_635) + BIAS;
    let exp = (7 - (v >> 7).leading_zeros().saturating_sub(24).min(7)) as u8;
    let exp = exp.min(7);
    let mant = ((v >> (exp + 3)) & 0xf) as u8;
    !(sign | exp << 4 | mant)
}

pub fn mulaw_value(u: u8) -> i16 {
    const BIAS: i32 = 0x84;
    let u = !u;
    let t = ((i32::from(u & 0xf) << 3) + BIAS) << ((u & 0x70) >> 4);
    (if u & 0x80 != 0 { BIAS - t } else { t - BIAS }) as i16
}
