use std::path::PathBuf;

use super::ffmpeg::Ffmpeg;
use super::native;
use crate::media::{Codec, Direction, Error, Result, Route};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Choice {
    #[default]
    Auto,
    Gpu,
    Cpu,
    Ffmpeg(UserFfmpeg),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserFfmpeg {
    pub path: Option<PathBuf>,
    pub codec: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    H264Gpu,
    H264Cpu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefs {
    pub decode: Choice,
    pub encode: Choice,
    pub hevc_encode_fallback: Vec<Fallback>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            decode: Choice::Auto,
            encode: Choice::Auto,
            hevc_encode_fallback: vec![Fallback::H264Gpu, Fallback::H264Cpu],
        }
    }
}

impl Prefs {
    pub fn ffmpeg() -> Self {
        Self {
            decode: Choice::Ffmpeg(UserFfmpeg::default()),
            encode: Choice::Ffmpeg(UserFfmpeg::default()),
            ..Self::default()
        }
    }

    pub fn user_ffmpeg(&self) -> Option<&UserFfmpeg> {
        [&self.decode, &self.encode]
            .into_iter()
            .find_map(|c| match c {
                Choice::Ffmpeg(u) => Some(u),
                _ => None,
            })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub gpu: Vec<(Codec, Direction)>,
    pub gpu_missing: Option<String>,
    pub cpu: Vec<(Codec, Direction)>,
    pub ffmpeg: Option<Ffmpeg>,
    pub ffmpeg_missing: Option<String>,
}

impl Caps {
    pub fn detect(prefs: &Prefs) -> Self {
        let (gpu, gpu_missing) = native::gpu();
        let mut caps = Caps {
            gpu,
            gpu_missing,
            cpu: native::cpu(),
            ..Caps::default()
        };
        if let Some(user) = prefs.user_ffmpeg() {
            match Ffmpeg::find(user.path.as_deref()) {
                Ok(f) => caps.ffmpeg = Some(f),
                Err(e) => caps.ffmpeg_missing = Some(e.to_string()),
            }
        }
        caps
    }

    fn has(&self, route: Route, codec: Codec, dir: Direction) -> bool {
        match route {
            Route::Gpu => self.gpu.contains(&(codec, dir)),
            Route::Cpu => self.cpu.contains(&(codec, dir)),
            Route::Ffmpeg => self.ffmpeg.as_ref().is_some_and(|f| f.can(codec, dir)),
        }
    }

    fn why_not(&self, route: Route, codec: Codec, dir: Direction) -> String {
        let what = match dir {
            Direction::Decode => "decoder",
            Direction::Encode => "encoder",
        };
        match route {
            Route::Gpu => match &self.gpu_missing {
                Some(why) => format!("no GPU {codec} {what}: {why}"),
                None => format!("no GPU {codec} {what} on this machine"),
            },
            Route::Cpu if codec == Codec::Hevc && dir == Direction::Encode => {
                "HEVC encodes on the GPU only".to_string()
            }
            Route::Cpu => format!("no CPU {codec} {what} in this build"),
            Route::Ffmpeg => match (&self.ffmpeg, &self.ffmpeg_missing) {
                (Some(f), _) => format!("{} has no {codec} {what}", f.path.display()),
                (None, Some(why)) => why.clone(),
                (None, None) => "ffmpeg is not picked in the settings".to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    pub route: Route,
    pub codec: Codec,
    pub ffmpeg_codec: Option<String>,
    pub reasons: Vec<String>,
}

impl Picked {
    pub fn said(&self) -> String {
        let head = format!("{} {}", self.codec, self.route);
        if self.reasons.is_empty() {
            head
        } else {
            format!("{head} ({})", self.reasons.join("; "))
        }
    }
}

pub fn pick_decode(codec: Codec, prefs: &Prefs, caps: &Caps) -> Result<Picked> {
    let (order, ffmpeg_codec): (&[Route], _) = match &prefs.decode {
        Choice::Auto | Choice::Gpu => (&[Route::Gpu, Route::Cpu], None),
        Choice::Cpu => (&[Route::Cpu], None),
        Choice::Ffmpeg(user) => (&[Route::Ffmpeg, Route::Gpu, Route::Cpu], user.codec.clone()),
    };
    let tries = order.iter().map(|&r| (r, codec)).collect();
    pick(codec, Direction::Decode, tries, ffmpeg_codec, caps)
}

pub fn pick_encode(codec: Codec, prefs: &Prefs, caps: &Caps) -> Result<Picked> {
    let (tries, ffmpeg_codec) = match &prefs.encode {
        Choice::Ffmpeg(user) => {
            let mut tries = vec![(Route::Ffmpeg, codec)];
            tries.extend(native_order(codec, prefs, &[Route::Gpu, Route::Cpu]));
            (tries, user.codec.clone())
        }
        Choice::Auto | Choice::Gpu => (native_order(codec, prefs, &[Route::Gpu, Route::Cpu]), None),
        Choice::Cpu => (native_order(codec, prefs, &[Route::Cpu]), None),
    };
    pick(codec, Direction::Encode, tries, ffmpeg_codec, caps)
}

fn pick(
    codec: Codec,
    dir: Direction,
    tries: Vec<(Route, Codec)>,
    ffmpeg_codec: Option<String>,
    caps: &Caps,
) -> Result<Picked> {
    let mut reasons = Vec::new();
    for (route, c) in tries {
        if c == Codec::Aac && route == Route::Gpu {
            continue;
        }
        if caps.has(route, c, dir) {
            if c != codec {
                reasons.push(format!("encoding {c} on the {route} instead"));
            }
            return Ok(Picked {
                route,
                codec: c,
                ffmpeg_codec: if route == Route::Ffmpeg {
                    ffmpeg_codec
                } else {
                    None
                },
                reasons,
            });
        }
        let why = caps.why_not(route, c, dir);
        if !reasons.contains(&why) {
            reasons.push(why);
        }
    }
    let verb = match dir {
        Direction::Decode => "decode",
        Direction::Encode => "encode",
    };
    Err(Error::Unsupported(format!(
        "can't {verb} {codec}: {}",
        reasons.join("; ")
    )))
}

fn native_order(codec: Codec, prefs: &Prefs, routes: &[Route]) -> Vec<(Route, Codec)> {
    if codec != Codec::Hevc {
        return routes.iter().map(|&r| (r, codec)).collect();
    }
    let mut out = Vec::new();
    if routes.contains(&Route::Gpu) {
        out.push((Route::Gpu, Codec::Hevc));
    } else {
        out.push((Route::Cpu, Codec::Hevc));
    }
    for f in &prefs.hevc_encode_fallback {
        let r = match f {
            Fallback::H264Gpu => Route::Gpu,
            Fallback::H264Cpu => Route::Cpu,
        };
        if routes.contains(&r) {
            out.push((r, Codec::H264));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(gpu: &[(Codec, Direction)], cpu: &[(Codec, Direction)]) -> Caps {
        Caps {
            gpu: gpu.to_vec(),
            gpu_missing: None,
            cpu: cpu.to_vec(),
            ..Caps::default()
        }
    }

    const D: Direction = Direction::Decode;
    const E: Direction = Direction::Encode;

    #[test]
    fn auto_prefers_the_gpu_then_the_cpu_and_never_ffmpeg() {
        let both = caps(&[(Codec::H264, D)], &[(Codec::H264, D)]);
        let p = pick_decode(Codec::H264, &Prefs::default(), &both).unwrap();
        assert_eq!((p.route, p.reasons.len()), (Route::Gpu, 0));

        let cpu = caps(&[], &[(Codec::H264, D)]);
        let p = pick_decode(Codec::H264, &Prefs::default(), &cpu).unwrap();
        assert_eq!(p.route, Route::Cpu);
        assert_eq!(p.reasons, ["no GPU H.264 decoder on this machine"]);

        let none = caps(&[], &[]);
        let e = pick_decode(Codec::Hevc, &Prefs::default(), &none).unwrap_err();
        assert!(e.to_string().contains("no CPU HEVC decoder"), "{e}");
    }

    #[test]
    fn hevc_without_a_gpu_encoder_falls_back_to_h264_gpu_then_cpu_saying_why() {
        let gpu264 = caps(&[(Codec::H264, E)], &[(Codec::H264, E)]);
        let p = pick_encode(Codec::Hevc, &Prefs::default(), &gpu264).unwrap();
        assert_eq!((p.route, p.codec), (Route::Gpu, Codec::H264));
        assert_eq!(
            p.reasons,
            [
                "no GPU HEVC encoder on this machine",
                "encoding H.264 on the GPU instead"
            ]
        );

        let cpu264 = caps(&[], &[(Codec::H264, E)]);
        let p = pick_encode(Codec::Hevc, &Prefs::default(), &cpu264).unwrap();
        assert_eq!((p.route, p.codec), (Route::Cpu, Codec::H264));
        assert!(
            p.reasons
                .contains(&"no GPU H.264 encoder on this machine".into())
        );

        let only_cpu = Prefs {
            encode: Choice::Cpu,
            ..Prefs::default()
        };
        let p = pick_encode(Codec::Hevc, &only_cpu, &cpu264).unwrap();
        assert_eq!((p.route, p.codec), (Route::Cpu, Codec::H264));
        assert_eq!(p.reasons[0], "HEVC encodes on the GPU only");

        let no_fallback = Prefs {
            hevc_encode_fallback: vec![],
            ..Prefs::default()
        };
        assert!(pick_encode(Codec::Hevc, &no_fallback, &cpu264).is_err());
    }

    #[test]
    fn a_missing_ffmpeg_falls_back_to_our_routes_saying_why() {
        let mut c = caps(&[], &[(Codec::H264, D)]);
        c.ffmpeg_missing = Some("no ffmpeg at /nowhere/ffmpeg".into());
        let p = pick_decode(Codec::H264, &Prefs::ffmpeg(), &c).unwrap();
        assert_eq!(p.route, Route::Cpu);
        assert_eq!(p.reasons[0], "no ffmpeg at /nowhere/ffmpeg");
    }
}
