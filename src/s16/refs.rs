use crate::testref::Sound;

pub const AREA: &str = "s16";
pub const WINDOW: f64 = 0.1;
pub const LOW_RATE: u32 = 8_000;

pub const SURVEY: &str = "0.5*sin(2*PI*(200+100*t)*t)|0.4*sin(2*PI*(300+50*t)*t)";
pub const TONE_A: &str = "0.5*sin(2*PI*(200+100*t)*t)";
pub const TONE_B: &str = "0.5*sin(2*PI*(500+300*t)*t)";
pub const TONE_C: &str = "0.5*sin(2*PI*(500+30*t)*t)";
pub const FIVE_ONE: &str = "0.3*sin(2*PI*220*t)|0.3*sin(2*PI*330*t)|0.3*sin(2*PI*440*t)|0.3*sin(2*PI*55*t)|0.3*sin(2*PI*660*t)|0.3*sin(2*PI*770*t)";

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

pub const SURVEY_SOUND: Sound = Sound::Fn(survey_at);

#[derive(Debug, Clone)]
pub struct Src {
    pub expr: &'static str,
    pub rate: u32,
    pub layout: &'static str,
    pub seconds: f64,
    pub offset: f64,
}

impl Src {
    pub fn lavfi(&self) -> String {
        format!(
            "aevalsrc={}:s={}:c={}:d={}",
            self.expr, self.rate, self.layout, self.seconds
        )
    }
}

const fn src(expr: &'static str, rate: u32, layout: &'static str, seconds: f64) -> Src {
    Src {
        expr,
        rate,
        layout,
        seconds,
        offset: 0.0,
    }
}

#[derive(Debug, Clone)]
pub struct Fixture {
    pub file: String,
    pub sources: Vec<Src>,
    pub codecs: Vec<&'static str>,
}

pub struct Kind {
    pub ext: &'static str,
    pub codec: &'static str,
    pub extra: &'static [&'static str],
    pub lossless: bool,
    pub may_refuse: bool,
}

const fn k(ext: &'static str, codec: &'static str, lossless: bool) -> Kind {
    Kind {
        ext,
        codec,
        extra: &[],
        lossless,
        may_refuse: false,
    }
}

const fn refusable(ext: &'static str, codec: &'static str) -> Kind {
    Kind {
        may_refuse: true,
        ..k(ext, codec, false)
    }
}

pub const KINDS: &[Kind] = &[
    k("mkv", "aac", false),
    k("mp4", "aac", false),
    k("mkv", "libopus", false),
    k("mp4", "libopus", false),
    k("mkv", "libmp3lame", false),
    k("mp4", "libmp3lame", false),
    Kind {
        extra: &["-bits_per_raw_sample", "24"],
        ..k("mkv", "flac", true)
    },
    Kind {
        extra: &["-bits_per_raw_sample", "24"],
        ..k("mp4", "flac", true)
    },
    Kind {
        extra: &["-sample_fmt", "s32"],
        may_refuse: true,
        ..k("mp4", "flac", true)
    },
    k("mkv", "alac", true),
    k("mp4", "alac", true),
    Kind {
        may_refuse: true,
        ..k("mkv", "libvorbis", false)
    },
    k("mov", "aac", false),
    k("3gp", "aac", false),
    refusable("m2ts", "aac"),
    refusable("m2ts", "ac3"),
    Kind {
        may_refuse: true,
        ..k("m2ts", "pcm_bluray", true)
    },
    k("mkv", "ac3", false),
    k("mp4", "ac3", false),
    k("mkv", "eac3", false),
    k("mp4", "eac3", false),
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
        match (self.codec, self.lossless) {
            ("pcm_bluray", _) | (_, false) => 48_000,
            _ => LOW_RATE,
        }
    }

    pub fn seconds(&self) -> f64 {
        if self.lossless { 0.02 } else { 0.12 }
    }

    pub fn frames(&self) -> usize {
        (self.seconds() * f64::from(self.rate())).round() as usize
    }

    pub fn label(&self) -> String {
        let profile = self.extra.last().map_or(String::new(), |p| format!("-{p}"));
        format!("{}{profile} in {}", self.codec, self.ext)
    }

    pub fn file(&self) -> String {
        let profile = self.extra.last().map_or(String::new(), |p| format!("-{p}"));
        format!("survey-{}{profile}.{}", self.codec, self.ext)
    }

    pub fn checked_by_ref(&self) -> bool {
        !self.lossless || self.law()
    }

    pub fn reference(&self) -> Ref {
        Ref {
            input: self.file(),
            stream: 0,
            from: 0.0,
            seconds: 0.0,
            mono: !self.lossless,
            rate: if self.lossless { self.rate() } else { LOW_RATE },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ref {
    pub input: String,
    pub stream: usize,
    pub from: f64,
    pub seconds: f64,
    pub mono: bool,
    pub rate: u32,
}

impl Ref {
    pub fn name(&self) -> String {
        format!(
            "{}.s{}.at{}.{}.{}.s16",
            self.input,
            self.stream,
            (self.from * 1000.0).round() as u64,
            if self.mono { "mono" } else { "all" },
            self.rate
        )
    }
}

pub const DELAYED: f64 = 0.3;
pub const DELAYED_LONG: f64 = 1.2;
pub const DELAYED_SHORT: f64 = 0.6;
pub const DELAYED_FROM: [f64; 2] = [0.4, 0.6];
pub const MIXED: f64 = 0.3;
pub const MIXED_FROM: f64 = 0.1;
pub const MIXED_THIRD: [(&str, &str); 2] = [("mkv", "pcm_s24le"), ("mp4", "flac")];
pub const FIVE_ONE_CODECS: [(&str, &str); 2] = [("mkv", "flac"), ("mp4", "aac")];
pub const FIVE_ONE_RATES: [u32; 2] = [LOW_RATE, 16_000];
pub const FIVE_ONE_SECONDS: f64 = 0.3;
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

pub fn fixtures() -> Vec<Fixture> {
    let mut out = Vec::new();
    for ext in ["mkv", "mp4"] {
        out.push(Fixture {
            file: delayed(ext),
            sources: vec![
                src(TONE_A, 48_000, "mono", DELAYED_LONG),
                Src {
                    offset: DELAYED,
                    ..src(TONE_B, 48_000, "mono", DELAYED_SHORT)
                },
            ],
            codecs: vec!["-c:a", "aac", "-b:a", "48k"],
        });
    }
    for (ext, third) in MIXED_THIRD {
        let mut codecs = vec![
            "-c:a:0", "aac", "-b:a:0", "64k", "-c:a:1", "libopus", "-b:a:1", "32k", "-c:a:2", third,
        ];
        if third == "flac" {
            codecs.extend(["-bits_per_raw_sample:a:2", "24"]);
        }
        out.push(Fixture {
            file: mixed(ext),
            sources: vec![
                src(SURVEY, 48_000, "stereo", MIXED),
                src(TONE_C, 48_000, "mono", MIXED),
                src(TONE_A, LOW_RATE, "mono", MIXED),
            ],
            codecs,
        });
    }
    for (ext, codec) in FIVE_ONE_CODECS {
        out.push(Fixture {
            file: five_one(ext, codec),
            sources: vec![src(FIVE_ONE, LOW_RATE, "5.1", FIVE_ONE_SECONDS)],
            codecs: vec!["-c:a", codec, "-b:a", "96k"],
        });
    }
    out.push(Fixture {
        file: REFUSED.into(),
        sources: vec![src(TONE_A, 44_100, "mono", 0.1)],
        codecs: vec!["-c:a", "wmav2"],
    });
    for kind in KINDS {
        let mut codecs = vec!["-c:a", kind.codec];
        codecs.extend(kind.extra);
        out.push(Fixture {
            file: kind.file(),
            sources: vec![src(SURVEY, kind.rate(), "stereo", kind.seconds())],
            codecs,
        });
    }
    out
}

pub fn window(input: String, stream: usize, from: f64, rate: u32) -> Ref {
    Ref {
        input,
        stream,
        from,
        seconds: WINDOW,
        mono: true,
        rate,
    }
}

pub fn refs() -> Vec<Ref> {
    let mut out = Vec::new();
    for ext in ["mkv", "mp4"] {
        for from in DELAYED_FROM {
            out.push(window(delayed(ext), 1, from, LOW_RATE));
        }
    }
    for (ext, _) in MIXED_THIRD {
        for stream in 0..3 {
            out.push(window(mixed(ext), stream, MIXED_FROM, LOW_RATE));
        }
    }
    for (ext, codec) in FIVE_ONE_CODECS {
        for rate in FIVE_ONE_RATES {
            out.push(window(five_one(ext, codec), 0, 0.0, rate));
        }
    }
    out.extend(
        KINDS
            .iter()
            .filter(|k| k.checked_by_ref())
            .map(Kind::reference),
    );
    out
}
