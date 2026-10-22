use super::base as native;
use crate::media::{Codec, Direction, Error, Result, Route};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Choice {
    #[default]
    Auto,
    Gpu,
    Cpu,
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

const CPU_PICKED: &str = "the settings pick the CPU";

fn reaches_gpu(choice: &Choice) -> bool {
    !matches!(choice, Choice::Cpu)
}

#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub gpu: Vec<(Codec, Direction)>,
    pub gpu_missing: Option<String>,
    pub cpu: Vec<(Codec, Direction)>,
}

impl Caps {
    pub fn detect(prefs: &Prefs) -> Self {
        Caps::probing(reaches_gpu(&prefs.decode) || reaches_gpu(&prefs.encode))
    }

    pub fn for_decode(prefs: &Prefs) -> Self {
        Caps::probing(reaches_gpu(&prefs.decode))
    }

    pub fn for_audio(_: &Prefs) -> Self {
        Caps::probing(false)
    }

    fn probing(gpu: bool) -> Self {
        let (gpu, gpu_missing) = if gpu {
            native::gpu()
        } else {
            (Vec::new(), Some(CPU_PICKED.to_string()))
        };
        Caps {
            gpu,
            gpu_missing,
            cpu: native::cpu(),
        }
    }

    fn has(&self, route: Route, codec: Codec, dir: Direction) -> bool {
        match route {
            Route::Gpu => self.gpu.contains(&(codec, dir)),
            Route::Cpu => self.cpu.contains(&(codec, dir)),
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    pub route: Route,
    pub codec: Codec,
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
    let order: &[Route] = match &prefs.decode {
        Choice::Auto | Choice::Gpu => &[Route::Gpu, Route::Cpu],
        Choice::Cpu => &[Route::Cpu],
    };
    let tries = order.iter().map(|&r| (r, codec)).collect();
    pick(codec, Direction::Decode, tries, caps)
}

pub fn pick_encode(codec: Codec, prefs: &Prefs, caps: &Caps) -> Result<Picked> {
    let tries = match &prefs.encode {
        Choice::Auto | Choice::Gpu => native_order(codec, prefs, &[Route::Gpu, Route::Cpu]),
        Choice::Cpu => native_order(codec, prefs, &[Route::Cpu]),
    };
    pick(codec, Direction::Encode, tries, caps)
}

fn pick(codec: Codec, dir: Direction, tries: Vec<(Route, Codec)>, caps: &Caps) -> Result<Picked> {
    let mut reasons = Vec::new();
    for (route, c) in tries {
        if c == Codec::Av1 && dir == Direction::Encode {
            let why = format!("AV1 encode isn't offered on the {route}");
            if !reasons.contains(&why) {
                reasons.push(why);
            }
            continue;
        }
        if route == Route::Gpu && (c == Codec::Aac || (c == Codec::Vp9 && dir == Direction::Encode))
        {
            continue;
        }
        if caps.has(route, c, dir) {
            if c != codec {
                reasons.push(format!("encoding {c} on the {route} instead"));
            }
            return Ok(Picked {
                route,
                codec: c,
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
    fn the_default_prefs_refuse_in_one_line_with_no_ffmpeg_hint() {
        let prefs = Prefs::default();
        assert_eq!(
            (&prefs.decode, &prefs.encode),
            (&Choice::Auto, &Choice::Auto)
        );
        let all = [Codec::H264, Codec::Hevc, Codec::Vp9, Codec::Av1, Codec::Aac];
        let both: Vec<(Codec, Direction)> = all.iter().flat_map(|&c| [(c, D), (c, E)]).collect();
        let rich = caps(&both, &both);
        let bare = caps(&[], &[]);
        for codec in all {
            for refused in [
                pick_decode(codec, &prefs, &bare),
                pick_encode(codec, &prefs, &bare),
            ] {
                let e = refused.unwrap_err().to_string();
                assert!(!e.to_lowercase().contains("ffmpeg"), "{e}");
                assert_eq!(e.lines().count(), 1, "{e}");
                assert!(e.contains(&codec.to_string()), "{e}");
            }
        }
        let p = pick_decode(Codec::H264, &prefs, &rich).unwrap();
        assert_eq!((p.route, p.reasons.len()), (Route::Gpu, 0));
    }
}
