use pito_codecs::filter::{BLUE_PURPLE_PINK, BarMode, Corner, Edge, ProgressBar, Rgb, Thickness};

pub const HELP: &str = "Draw a progress bar over the video: --progress bar or --progress contour, then optional ,key=value settings. bar is a line along one edge that fills left to right (top and bottom) or bottom to top (left and right) over the video's length; contour runs around the frame's edge, all four sides, and fills the whole loop. The fill is linear with the frame's time, empty at the first frame and full at the last. Keys: position=top|bottom|left|right (bar, top by default); start=tl|tr|br|bl and direction=cw|ccw (contour, tl and cw by default); thickness=6 or 6px in pixels, or 0.8% of the short side [default: 0.6% of the height]; color=white, a #rrggbb, or 2 to 4 of them joined with + for a gradient along the filled part (blue-purple-pink is a preset) [default: white]; track=#rrggbb for the unfilled part [default: #101018] and opacity=60% or 0.6 [default: 60%]. Example: --progress bar,position=top,color=blue-purple-pink. It re-encodes the video (mp4, mov, mkv or webm); the audio stays as it is where nothing else changes it.";

const NAMES: [(&str, Rgb); 12] = [
    ("white", Rgb(255, 255, 255)),
    ("black", Rgb(0, 0, 0)),
    ("red", Rgb(239, 68, 68)),
    ("green", Rgb(34, 197, 94)),
    ("blue", Rgb(59, 130, 246)),
    ("yellow", Rgb(250, 204, 21)),
    ("cyan", Rgb(34, 211, 238)),
    ("magenta", Rgb(217, 70, 239)),
    ("orange", Rgb(249, 115, 22)),
    ("purple", Rgb(168, 85, 247)),
    ("pink", Rgb(236, 72, 153)),
    ("gray", Rgb(128, 128, 128)),
];

fn color(text: &str) -> Result<Rgb, String> {
    let t = text.trim().to_ascii_lowercase();
    if t == "grey" {
        return color("gray");
    }
    if let Some((_, c)) = NAMES.iter().find(|(n, _)| *n == t) {
        return Ok(*c);
    }
    let hex = t.strip_prefix('#').unwrap_or(&t);
    let digit = |i: usize| u8::from_str_radix(hex.get(i..=i)?, 16).ok();
    let pair = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    let parsed = match hex.len() {
        3 => (|| Some(Rgb(digit(0)? * 17, digit(1)? * 17, digit(2)? * 17)))(),
        6 => (|| Some(Rgb(pair(0)?, pair(2)?, pair(4)?)))(),
        _ => None,
    };
    parsed.ok_or_else(|| {
        format!("{text:?} is not a colour: use a name such as white, or #rgb or #rrggbb")
    })
}

fn colors(text: &str) -> Result<Vec<Rgb>, String> {
    if text.eq_ignore_ascii_case("blue-purple-pink") {
        return Ok(BLUE_PURPLE_PINK.to_vec());
    }
    let list: Vec<Rgb> = text.split('+').map(color).collect::<Result<_, _>>()?;
    if list.len() > 4 {
        return Err(format!("color takes 1 to 4 colours, not {}", list.len()));
    }
    Ok(list)
}

fn number(text: &str, what: &str) -> Result<f64, String> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{what} {text:?} is not a number"))
}

fn thickness(text: &str) -> Result<Thickness, String> {
    if let Some(p) = text.strip_suffix('%') {
        let share = number(p, "thickness")? / 100.0;
        if !(share > 0.0 && share <= 0.5) {
            return Err("thickness as a share of the short side is above 0 and up to 50%".into());
        }
        return Ok(Thickness::ShortSide(share));
    }
    let digits = text.strip_suffix("px").unwrap_or(text);
    match digits.trim().parse::<u32>() {
        Ok(p) if p >= 1 => Ok(Thickness::Pixels(p)),
        _ => Err(format!(
            "thickness {text:?} is whole pixels (6 or 6px) or a share of the short side (0.8%)"
        )),
    }
}

fn opacity(text: &str) -> Result<f64, String> {
    let v = match text.strip_suffix('%') {
        Some(p) => number(p, "opacity")? / 100.0,
        None => number(text, "opacity")?,
    };
    if (0.0..=1.0).contains(&v) {
        Ok(v)
    } else {
        Err("opacity is 0 to 1, or 0% to 100%".into())
    }
}

fn edge(text: &str) -> Result<Edge, String> {
    match text {
        "top" => Ok(Edge::Top),
        "bottom" => Ok(Edge::Bottom),
        "left" => Ok(Edge::Left),
        "right" => Ok(Edge::Right),
        _ => Err(format!("position {text:?} is top, bottom, left or right")),
    }
}

fn corner(text: &str) -> Result<Corner, String> {
    match text {
        "tl" | "top-left" => Ok(Corner::TopLeft),
        "tr" | "top-right" => Ok(Corner::TopRight),
        "br" | "bottom-right" => Ok(Corner::BottomRight),
        "bl" | "bottom-left" => Ok(Corner::BottomLeft),
        _ => Err(format!("start {text:?} is tl, tr, br or bl")),
    }
}

fn clockwise(text: &str) -> Result<bool, String> {
    match text {
        "cw" | "clockwise" => Ok(true),
        "ccw" | "counterclockwise" | "anticlockwise" => Ok(false),
        _ => Err(format!("direction {text:?} is cw or ccw")),
    }
}

pub fn parse(spec: &str) -> Result<ProgressBar, String> {
    let mut parts = spec.split(',');
    let mode = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let contour = match mode.as_str() {
        "bar" => false,
        "contour" => true,
        _ => {
            return Err(format!(
                "{spec:?}: the mode is bar or contour, then optional ,key=value settings (pcod --help lists them)"
            ));
        }
    };
    let mut bar = ProgressBar::default();
    let mut position = Edge::Top;
    let mut start = Corner::TopLeft;
    let mut cw = true;
    for part in parts {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| format!("{part:?} is not key=value"))?;
        let (key, value) = (
            key.trim().to_ascii_lowercase(),
            value.trim().to_ascii_lowercase(),
        );
        let only = |wanted: bool, mode: &str| {
            if wanted {
                Ok(())
            } else {
                Err(format!("{key} fits {mode} only"))
            }
        };
        match key.as_str() {
            "position" => {
                only(!contour, "bar")?;
                position = edge(&value)?;
            }
            "start" => {
                only(contour, "contour")?;
                start = corner(&value)?;
            }
            "direction" => {
                only(contour, "contour")?;
                cw = clockwise(&value)?;
            }
            "thickness" => bar.thickness = Some(thickness(&value)?),
            "color" | "colour" => bar.colors = colors(&value)?,
            "track" => bar.track = color(&value)?,
            "opacity" => bar.track_opacity = opacity(&value)?,
            _ => {
                return Err(format!(
                    "unknown setting {key:?}: position, start, direction, thickness, color, track or opacity"
                ));
            }
        }
    }
    bar.mode = if contour {
        BarMode::Contour {
            start,
            clockwise: cw,
        }
    } else {
        BarMode::Bar(position)
    };
    bar.validate().map_err(|e| e.to_string())?;
    Ok(bar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_mode_takes_the_neutral_defaults() {
        let b = parse("bar").unwrap();
        assert_eq!(b, ProgressBar::default());
        assert_eq!(b.colors, vec![Rgb(255, 255, 255)]);
        let c = parse("contour").unwrap();
        assert_eq!(
            c.mode,
            BarMode::Contour {
                start: Corner::TopLeft,
                clockwise: true
            }
        );
    }

    #[test]
    fn every_setting_is_read() {
        let b = parse("bar,position=left,thickness=0.8%,color=#f00+00f,track=black,opacity=25%")
            .unwrap();
        assert_eq!(b.mode, BarMode::Bar(Edge::Left));
        assert_eq!(b.thickness, Some(Thickness::ShortSide(0.008)));
        assert_eq!(b.colors, vec![Rgb(255, 0, 0), Rgb(0, 0, 255)]);
        assert_eq!(b.track, Rgb(0, 0, 0));
        assert_eq!(b.track_opacity, 0.25);
        let c =
            parse("contour,start=br,direction=ccw,thickness=6px,color=blue-purple-pink").unwrap();
        assert_eq!(
            c.mode,
            BarMode::Contour {
                start: Corner::BottomRight,
                clockwise: false
            }
        );
        assert_eq!(c.thickness, Some(Thickness::Pixels(6)));
        assert_eq!(c.colors, BLUE_PURPLE_PINK.to_vec());
        assert_eq!(
            parse("bar,thickness=9").unwrap().thickness,
            Some(Thickness::Pixels(9))
        );
        assert_eq!(parse("bar,opacity=0.5").unwrap().track_opacity, 0.5);
    }

    #[test]
    fn a_wrong_spec_is_refused_with_the_reason() {
        for (spec, part) in [
            ("ring", "bar or contour"),
            ("bar,position=middle", "top, bottom, left or right"),
            ("bar,start=tl", "contour only"),
            ("contour,position=top", "bar only"),
            ("contour,direction=up", "cw or ccw"),
            ("bar,thickness=0", "whole pixels"),
            ("bar,thickness=80%", "up to 50%"),
            ("bar,color=nope", "not a colour"),
            ("bar,color=red+red+red+red+red", "1 to 4"),
            ("bar,opacity=2", "0 to 1"),
            ("bar,speed=2", "unknown setting"),
            ("bar,top", "key=value"),
        ] {
            let e = parse(spec).unwrap_err();
            assert!(e.contains(part), "{spec}: {e}");
        }
    }
}
