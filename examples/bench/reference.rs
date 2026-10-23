use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

pub const FORMAT: i64 = 1;
pub const PATH: &str = "bench/ffmpeg-reference.toml";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Case {
    pub unit: String,
    pub value: f64,
    pub runs: Vec<f64>,
    pub details: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reference {
    pub ffmpeg: String,
    pub configuration: String,
    pub date: String,
    pub runs: usize,
    pub cpu: String,
    pub threads: usize,
    pub gpu: String,
    pub driver: String,
    pub os: String,
    pub inputs: BTreeMap<String, u64>,
    pub cases: BTreeMap<String, Case>,
}

#[derive(Clone, Debug, PartialEq)]
enum Val {
    Text(String),
    Num(f64),
    List(Vec<f64>),
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn unquote(s: &str) -> Option<(String, &str)> {
    let mut chars = s.strip_prefix('"')?.char_indices();
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &s[i + 2..])),
            '\\' => match chars.next()?.1 {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn value(s: &str) -> Option<Val> {
    let s = s.trim();
    if s.starts_with('"') {
        let (text, rest) = unquote(s)?;
        return rest.trim().is_empty().then_some(Val::Text(text));
    }
    if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        if inner.trim().is_empty() {
            return Some(Val::List(Vec::new()));
        }
        return inner
            .split(',')
            .map(num)
            .collect::<Option<_>>()
            .map(Val::List);
    }
    num(s).map(Val::Num)
}

fn key(s: &str) -> Option<String> {
    let s = s.trim();
    if s.starts_with('"') {
        let (k, rest) = unquote(s)?;
        return rest.trim().is_empty().then_some(k);
    }
    (!s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    .then(|| s.to_string())
}

enum Table {
    Top,
    Inputs,
    Case(String),
}

impl Reference {
    pub fn parse(text: &str) -> Result<Reference, String> {
        let mut top = BTreeMap::new();
        let mut r = Reference::default();
        let mut table = Table::Top;
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            let bad = |what: &str| format!("line {}: {what}", n + 1);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(head) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                table = if head == "inputs" {
                    Table::Inputs
                } else if let Some(id) = head.strip_prefix("cases.").and_then(key) {
                    r.cases.insert(id.clone(), Case::default());
                    Table::Case(id)
                } else {
                    return Err(bad("unknown table"));
                };
                continue;
            }
            let (k, v) = line
                .split_once(" = ")
                .ok_or_else(|| bad("not key = value"))?;
            let k = key(k).ok_or_else(|| bad("bad key"))?;
            let v = value(v).ok_or_else(|| bad("bad value"))?;
            match (&table, v) {
                (Table::Top, v) => {
                    top.insert(k, v);
                }
                (Table::Inputs, Val::Num(b)) if b >= 0.0 && b.fract() == 0.0 => {
                    r.inputs.insert(k, b as u64);
                }
                (Table::Case(id), v) => {
                    let case = r.cases.get_mut(id).ok_or_else(|| bad("no case"))?;
                    match (k.as_str(), v) {
                        ("unit", Val::Text(t)) => case.unit = t,
                        ("value", Val::Num(x)) => case.value = x,
                        ("runs", Val::List(l)) => case.runs = l,
                        (_, Val::Num(x)) => {
                            case.details.insert(k, x);
                        }
                        _ => return Err(bad("bad case field")),
                    }
                }
                _ => return Err(bad("bad input size")),
            }
        }
        match top.get("format") {
            Some(Val::Num(f)) if *f == FORMAT as f64 => {}
            Some(Val::Num(f)) => return Err(format!("format {f}, this bench reads {FORMAT}")),
            _ => return Err("no format".into()),
        }
        let text = |k: &str| match top.get(k) {
            Some(Val::Text(t)) => Ok(t.clone()),
            _ => Err(format!("no {k}")),
        };
        let count = |k: &str| match top.get(k) {
            Some(Val::Num(x)) if *x >= 0.0 && x.fract() == 0.0 => Ok(*x as usize),
            _ => Err(format!("no {k}")),
        };
        r.ffmpeg = text("ffmpeg")?;
        if !r.ffmpeg.starts_with("ffmpeg version ") {
            return Err("ffmpeg is not an ffmpeg -version line".into());
        }
        r.configuration = text("configuration")?;
        r.date = text("date")?;
        r.runs = count("runs")?;
        r.cpu = text("cpu")?;
        r.threads = count("threads")?;
        r.gpu = text("gpu")?;
        r.driver = text("driver")?;
        r.os = text("os")?;
        if let Some((id, _)) = r.cases.iter().find(|(_, c)| c.unit.is_empty()) {
            return Err(format!("case {id} has no unit"));
        }
        Ok(r)
    }

    pub fn read(path: &Path) -> Res<Reference> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Reference::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?)
    }

    pub fn version(&self) -> &str {
        self.ffmpeg
            .strip_prefix("ffmpeg version ")
            .and_then(|v| v.split_whitespace().next())
            .map(|v| v.trim_start_matches('n'))
            .unwrap_or("")
    }

    pub fn render(&self) -> String {
        let mut out = String::from(
            "# ffmpeg's side of the bench, recorded once with `bench --record-ffmpeg`.\n# The bench reads every ffmpeg column from this file and never runs ffmpeg;\n# re-recording is the owner's call, made only on his word.\n\n",
        );
        out.push_str(&format!("format = {FORMAT}\n"));
        for (k, v) in [
            ("ffmpeg", &self.ffmpeg),
            ("configuration", &self.configuration),
            ("date", &self.date),
        ] {
            out.push_str(&format!("{k} = {}\n", quote(v)));
        }
        out.push_str(&format!("runs = {}\n", self.runs));
        out.push_str(&format!("cpu = {}\n", quote(&self.cpu)));
        out.push_str(&format!("threads = {}\n", self.threads));
        for (k, v) in [
            ("gpu", &self.gpu),
            ("driver", &self.driver),
            ("os", &self.os),
        ] {
            out.push_str(&format!("{k} = {}\n", quote(v)));
        }
        out.push_str("\n[inputs]\n");
        for (name, bytes) in &self.inputs {
            out.push_str(&format!("{} = {bytes}\n", quote(name)));
        }
        for (id, c) in &self.cases {
            out.push_str(&format!("\n[cases.{}]\n", quote(id)));
            out.push_str(&format!("unit = {}\n", quote(&c.unit)));
            out.push_str(&format!("value = {:?}\n", c.value));
            let runs: Vec<String> = c.runs.iter().map(|v| format!("{v:?}")).collect();
            out.push_str(&format!("runs = [{}]\n", runs.join(", ")));
            for (k, v) in &c.details {
                out.push_str(&format!("{} = {v:?}\n", quote(k)));
            }
        }
        out
    }

    pub fn check_inputs(&self, dir: &Path) -> Res<()> {
        for (name, want) in &self.inputs {
            let path = dir.join(name);
            let Ok(meta) = fs::metadata(&path) else {
                return Err(format!(
                    "{} is missing; the inputs are made with ffmpeg by `bench --record-ffmpeg`, run only on the owner's word",
                    path.display()
                )
                .into());
            };
            if meta.len() != *want {
                return Err(format!(
                    "{} is {} bytes, the reference was measured on {want}",
                    path.display(),
                    meta.len()
                )
                .into());
            }
        }
        Ok(())
    }
}

pub fn find_on_path(path: Option<&OsStr>, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn sample() -> Reference {
        let mut r = Reference {
            ffmpeg: "ffmpeg version 9.0.1 Copyright (c) 2000-2026 the FFmpeg developers".into(),
            configuration: "configuration: --prefix=/usr --enable-gpl \"quoted\"".into(),
            date: "2026-10-23".into(),
            runs: 3,
            cpu: "AMD Ryzen 9 9950X 16-Core Processor".into(),
            threads: 32,
            gpu: "AMD Radeon RX 9060 XT".into(),
            driver: "Mesa Gallium driver".into(),
            os: "Linux 7.2.8".into(),
            ..Reference::default()
        };
        r.inputs.insert("h264-1080.mp4".into(), 10044607);
        r.cases.insert(
            "decode.h264.1080.cpu-1t".into(),
            Case {
                unit: "fps".into(),
                value: 412.5,
                runs: vec![410.0, 412.5, 415.25],
                details: BTreeMap::from([("ffmpeg_psnr_db".into(), 41.2)]),
            },
        );
        r.cases.insert(
            "first-frame.h264.1440.cpu".into(),
            Case {
                unit: "ms".into(),
                value: 85.2,
                runs: vec![85.2],
                details: BTreeMap::new(),
            },
        );
        r
    }

    #[test]
    fn render_then_parse_gives_the_same_reference() {
        let r = sample();
        assert_eq!(Reference::parse(&r.render()).unwrap(), r);
    }

    #[test]
    fn version_is_the_number_after_ffmpeg_version() {
        assert_eq!(sample().version(), "9.0.1");
        let mut r = sample();
        r.ffmpeg = "ffmpeg version n9.0.2 Copyright (c) 2000-2026".into();
        assert_eq!(r.version(), "9.0.2");
    }

    #[test]
    fn wrong_format_is_refused_in_one_line() {
        let text = sample().render().replace("format = 1", "format = 2");
        let err = Reference::parse(&text).unwrap_err();
        assert_eq!(err, "format 2, this bench reads 1");
    }

    #[test]
    fn missing_format_is_refused() {
        let text = sample().render().replace("format = 1\n", "");
        assert_eq!(Reference::parse(&text).unwrap_err(), "no format");
    }

    #[test]
    fn a_line_that_is_not_ffmpeg_version_is_refused() {
        let text = sample()
            .render()
            .replace("ffmpeg version 9.0.1", "avconv version 9");
        assert_eq!(
            Reference::parse(&text).unwrap_err(),
            "ffmpeg is not an ffmpeg -version line"
        );
    }

    #[test]
    fn malformed_lines_are_refused_with_their_number() {
        for (good, bad, want) in [
            ("value = 412.5", "value = fast", "bad value"),
            ("unit = \"fps\"", "unit: fps", "not key = value"),
            ("[inputs]", "[outputs]", "unknown table"),
        ] {
            let text = sample().render().replacen(good, bad, 1);
            let err = Reference::parse(&text).unwrap_err();
            assert!(err.starts_with("line "), "{err}");
            assert!(err.ends_with(want), "{err}");
            assert!(!err.contains('\n'));
        }
    }

    #[test]
    fn a_case_without_a_unit_is_refused() {
        let text = sample().render().replace("unit = \"ms\"\n", "");
        assert_eq!(
            Reference::parse(&text).unwrap_err(),
            "case first-frame.h264.1440.cpu has no unit"
        );
    }

    #[test]
    fn non_finite_numbers_are_refused() {
        let text = sample().render().replace("value = 85.2", "value = inf");
        assert!(Reference::parse(&text).unwrap_err().ends_with("bad value"));
    }

    #[test]
    fn read_names_the_file_when_refusing() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/bench-tests")
            .join(format!("reference-read-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ref.toml");
        fs::write(&file, "format = 7\n").unwrap();
        let err = Reference::read(&file).unwrap_err().to_string();
        let absent = Reference::read(&dir.join("absent.toml"))
            .unwrap_err()
            .to_string();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            err,
            format!("{}: format 7, this bench reads 1", file.display())
        );
        assert!(absent.starts_with(&dir.join("absent.toml").display().to_string()));
    }

    #[test]
    fn inputs_are_checked_by_presence_and_size() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/bench-tests")
            .join(format!("reference-inputs-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut r = sample();
        r.inputs = BTreeMap::from([("a.mp4".into(), 3)]);
        let missing = r.check_inputs(&dir).unwrap_err().to_string();
        fs::write(dir.join("a.mp4"), b"ab").unwrap();
        let short = r.check_inputs(&dir).unwrap_err().to_string();
        fs::write(dir.join("a.mp4"), b"abc").unwrap();
        let ok = r.check_inputs(&dir);
        let _ = fs::remove_dir_all(&dir);
        assert!(missing.contains("--record-ffmpeg"), "{missing}");
        assert!(short.ends_with("is 2 bytes, the reference was measured on 3"));
        assert!(ok.is_ok());
    }

    #[test]
    fn find_on_path_misses_an_empty_or_absent_path() {
        assert_eq!(find_on_path(None, "ffmpeg"), None);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/bench-tests")
            .join(format!("reference-path-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let missed = find_on_path(Some(dir.as_os_str()), "ffmpeg");
        fs::write(dir.join("ffmpeg"), b"").unwrap();
        let found = find_on_path(Some(dir.as_os_str()), "ffmpeg");
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(missed, None);
        assert_eq!(found, Some(dir.join("ffmpeg")));
    }
}
