use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sha256::sha256_file;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

pub const FORMAT: i64 = 1;
pub const PATH: &str = "bench/ffmpeg-reference.toml";
const REMAKE: &str = "re-making the inputs is `bench --record-ffmpeg`, run by hand";
pub const SHA_CACHE: &str = ".sha256-cache";

fn stamp(path: &Path) -> Res<(u64, u128)> {
    let m = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let modified = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Ok((m.len(), modified))
}

pub fn cached_sha256(dir: &Path, name: &str, fresh: bool) -> Res<String> {
    let path = dir.join(name);
    let (bytes, modified) = stamp(&path)?;
    let cache = dir.join(SHA_CACHE);
    let text = fs::read_to_string(&cache).unwrap_or_default();
    let key = format!("{name}\t{bytes}\t{modified}\t");
    let hit = text
        .lines()
        .find_map(|l| l.strip_prefix(&key))
        .filter(|sha| modified > 0 && sha.len() == 64);
    if let (Some(sha), false) = (hit, fresh) {
        return Ok(sha.to_string());
    }
    let sha = sha256_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|l| l.split('\t').next() != Some(name))
        .collect();
    let line = format!("{key}{sha}");
    lines.push(&line);
    let _ = fs::write(&cache, lines.join("\n") + "\n");
    Ok(sha)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Case {
    pub unit: String,
    pub value: f64,
    pub runs: Vec<f64>,
    pub details: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Input {
    pub bytes: u64,
    pub sha256: String,
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
    pub inputs: BTreeMap<String, Input>,
    pub cases: BTreeMap<String, Case>,
}

#[derive(Clone, Debug, PartialEq)]
enum Val {
    Text(String),
    Num(f64),
    List(Vec<f64>),
    Table(Vec<(String, Val)>),
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
    if let Some(inner) = s.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
        let mut fields = Vec::new();
        for part in inner.split(',') {
            let (k, v) = split_kv(part).ok()?;
            match value(v)? {
                Val::Table(_) => return None,
                v => fields.push((k, v)),
            }
        }
        return Some(Val::Table(fields));
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

fn split_kv(s: &str) -> Result<(String, &str), &'static str> {
    let s = s.trim();
    if s.starts_with('"') {
        let (k, rest) = unquote(s).ok_or("bad key")?;
        let v = rest
            .trim_start()
            .strip_prefix('=')
            .ok_or("not key = value")?;
        return Ok((k, v));
    }
    let (k, v) = s.split_once('=').ok_or("not key = value")?;
    Ok((key(k).ok_or("bad key")?, v))
}

fn count(v: f64) -> Option<u64> {
    (v >= 0.0 && v.fract() == 0.0).then_some(v as u64)
}

fn input(v: Val) -> Option<Input> {
    match v {
        Val::Num(b) => Some(Input {
            bytes: count(b)?,
            sha256: String::new(),
        }),
        Val::Table(fields) => {
            let mut i = Input::default();
            let mut bytes = false;
            for (k, v) in fields {
                match (k.as_str(), v) {
                    ("bytes", Val::Num(b)) if !bytes => {
                        i.bytes = count(b)?;
                        bytes = true;
                    }
                    ("sha256", Val::Text(h))
                        if i.sha256.is_empty()
                            && h.len() == 64
                            && h.chars().all(|c| c.is_ascii_hexdigit()) =>
                    {
                        i.sha256 = h.to_ascii_lowercase();
                    }
                    _ => return None,
                }
            }
            bytes.then_some(i)
        }
        _ => None,
    }
}

enum Table {
    Top,
    Inputs,
    Case(String),
}

fn positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

impl Reference {
    pub fn parse(text: &str) -> Result<Reference, String> {
        let mut top = BTreeMap::new();
        let mut r = Reference::default();
        let mut table = Table::Top;
        let mut tables = BTreeSet::new();
        let mut seen = BTreeSet::new();
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
                    Table::Case(id)
                } else {
                    return Err(bad("unknown table"));
                };
                let name = match &table {
                    Table::Case(id) => format!("cases.{id}"),
                    _ => head.to_string(),
                };
                if !tables.insert(name) {
                    return Err(bad("duplicate table"));
                }
                if let Table::Case(id) = &table {
                    r.cases.insert(id.clone(), Case::default());
                }
                seen.clear();
                continue;
            }
            let (k, v) = split_kv(line).map_err(bad)?;
            let v = value(v).ok_or_else(|| bad("bad value"))?;
            if !seen.insert(k.clone()) {
                return Err(bad("duplicate key"));
            }
            match (&table, v) {
                (Table::Top, v) => {
                    top.insert(k, v);
                }
                (Table::Inputs, v) => {
                    let i = input(v).ok_or_else(|| bad("bad input"))?;
                    r.inputs.insert(k, i);
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
        let whole = |k: &str| match top.get(k) {
            Some(Val::Num(x)) => count(*x).map(|c| c as usize).ok_or(format!("no {k}")),
            _ => Err(format!("no {k}")),
        };
        r.ffmpeg = text("ffmpeg")?;
        if !r.ffmpeg.starts_with("ffmpeg version ") {
            return Err("ffmpeg is not an ffmpeg -version line".into());
        }
        r.configuration = text("configuration")?;
        r.date = text("date")?;
        r.runs = whole("runs")?;
        r.cpu = text("cpu")?;
        r.threads = whole("threads")?;
        r.gpu = text("gpu")?;
        r.driver = text("driver")?;
        r.os = text("os")?;
        for (id, c) in &r.cases {
            if c.unit.is_empty() {
                return Err(format!("case {id} has no unit"));
            }
            if !positive(c.value) {
                return Err(format!("case {id} needs a value above 0"));
            }
            if c.runs.is_empty() || !c.runs.iter().copied().all(positive) {
                return Err(format!("case {id} needs runs above 0"));
            }
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
            "# ffmpeg's side of the bench, recorded once with `bench --record-ffmpeg`.\n# The bench reads every ffmpeg column from this file and never runs ffmpeg;\n# re-recording is run by hand.\n\n",
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
        for (name, i) in &self.inputs {
            if i.sha256.is_empty() {
                out.push_str(&format!("{} = {{ bytes = {} }}\n", quote(name), i.bytes));
            } else {
                out.push_str(&format!(
                    "{} = {{ bytes = {}, sha256 = {} }}\n",
                    quote(name),
                    i.bytes,
                    quote(&i.sha256)
                ));
            }
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

    fn sized(&self, dir: &Path) -> Res<()> {
        if self.inputs.is_empty() {
            return Err("the reference names no inputs".into());
        }
        for (name, want) in &self.inputs {
            let path = dir.join(name);
            let Ok(meta) = fs::metadata(&path) else {
                return Err(format!("{} is missing; {REMAKE}", path.display()).into());
            };
            if meta.len() != want.bytes {
                return Err(format!(
                    "{} is {} bytes, the reference was measured on {}; {REMAKE}",
                    path.display(),
                    meta.len(),
                    want.bytes
                )
                .into());
            }
        }
        Ok(())
    }

    pub fn check_inputs(&self, dir: &Path) -> Res<()> {
        self.sized(dir)?;
        for (name, want) in &self.inputs {
            if want.sha256.is_empty() {
                return Err(format!(
                    "the reference records no sha256 for {name}; `bench --hash-inputs` records it from {}",
                    dir.display()
                )
                .into());
            }
            let path = dir.join(name);
            let got = cached_sha256(dir, name, false)?;
            if got != want.sha256 {
                return Err(format!(
                    "{} has sha256 {got}, the reference was measured on {}; {REMAKE}",
                    path.display(),
                    want.sha256
                )
                .into());
            }
        }
        Ok(())
    }

    pub fn hash_inputs(&mut self, dir: &Path) -> Res<()> {
        self.sized(dir)?;
        for (name, i) in self.inputs.iter_mut() {
            i.sha256 = cached_sha256(dir, name, true)?;
        }
        Ok(())
    }

    pub fn same_ffmpeg(&self, line: &str, configuration: &str) -> Res<()> {
        if self.ffmpeg != line || self.configuration != configuration {
            return Err(format!(
                "the reference holds `{}`, the ffmpeg on PATH is `{line}`; one file holds one ffmpeg build, so record every case without --only",
                self.ffmpeg
            )
            .into());
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

    pub const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    pub fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/bench-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

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
        r.inputs.insert(
            "h264-1080.mp4".into(),
            Input {
                bytes: 10044607,
                sha256: ABC.into(),
            },
        );
        r.inputs.insert(
            "raw-1080.yuv".into(),
            Input {
                bytes: 3,
                sha256: String::new(),
            },
        );
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

    fn one_input(sha256: &str) -> Reference {
        let mut r = sample();
        r.inputs = BTreeMap::from([(
            "a.mp4".into(),
            Input {
                bytes: 3,
                sha256: sha256.into(),
            },
        )]);
        r
    }

    #[test]
    fn inputs_are_checked_by_presence_size_and_sha256() {
        let dir = scratch("reference-inputs");
        let r = one_input(ABC);
        let missing = r.check_inputs(&dir).unwrap_err().to_string();
        fs::write(dir.join("a.mp4"), b"ab").unwrap();
        let short = r.check_inputs(&dir).unwrap_err().to_string();
        fs::write(dir.join("a.mp4"), b"abd").unwrap();
        let changed = r.check_inputs(&dir).unwrap_err().to_string();
        fs::write(dir.join("a.mp4"), b"abc").unwrap();
        let ok = r.check_inputs(&dir);
        let unhashed = one_input("").check_inputs(&dir).unwrap_err().to_string();
        let _ = fs::remove_dir_all(&dir);
        assert!(
            missing.ends_with(&format!("a.mp4 is missing; {REMAKE}")),
            "{missing}"
        );
        assert!(
            short.ends_with(&format!(
                "is 2 bytes, the reference was measured on 3; {REMAKE}"
            )),
            "{short}"
        );
        assert!(changed.contains("has sha256 "), "{changed}");
        assert!(
            changed.ends_with(&format!("the reference was measured on {ABC}; {REMAKE}")),
            "{changed}"
        );
        assert!(ok.is_ok());
        assert!(
            unhashed.starts_with(
                "the reference records no sha256 for a.mp4; `bench --hash-inputs` records it from "
            ),
            "{unhashed}"
        );
        for line in [missing, short, changed, unhashed] {
            assert!(!line.contains('\n'));
        }
    }

    #[test]
    fn a_reference_with_no_inputs_is_refused() {
        let mut r = sample();
        r.inputs.clear();
        let err = r.check_inputs(Path::new("tmp")).unwrap_err().to_string();
        assert_eq!(err, "the reference names no inputs");
    }

    #[test]
    fn a_check_reuses_the_cached_sha256_until_the_file_changes() {
        let dir = scratch("reference-cache");
        let r = one_input(ABC);
        let file = dir.join("a.mp4");
        fs::write(&file, b"abc").unwrap();
        let first = r.check_inputs(&dir);
        let cache = fs::read_to_string(dir.join(SHA_CACHE)).unwrap_or_default();
        let stale = cache.replace(ABC, &"0".repeat(64));
        fs::write(dir.join(SHA_CACHE), &stale).unwrap();
        let cached = r.check_inputs(&dir).unwrap_err().to_string();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let touched = r.check_inputs(&dir);
        let mut hashed = one_input("");
        fs::write(dir.join(SHA_CACHE), &stale).unwrap();
        hashed.hash_inputs(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert!(first.is_ok());
        assert!(
            cache.starts_with("a.mp4\t3\t") && cache.trim_end().ends_with(ABC),
            "{cache}"
        );
        assert!(
            cached.contains(&format!("has sha256 {}", "0".repeat(64))),
            "{cached}"
        );
        assert!(touched.is_ok());
        assert_eq!(hashed.inputs["a.mp4"].sha256, ABC);
    }

    #[test]
    fn hash_inputs_records_each_inputs_sha256_and_needs_the_recorded_size() {
        let dir = scratch("reference-hash");
        let mut r = one_input("");
        let missing = r.hash_inputs(&dir).unwrap_err().to_string();
        fs::write(dir.join("a.mp4"), b"abcd").unwrap();
        let wrong = r.hash_inputs(&dir).unwrap_err().to_string();
        fs::write(dir.join("a.mp4"), b"abc").unwrap();
        r.hash_inputs(&dir).unwrap();
        let checked = r.check_inputs(&dir);
        let _ = fs::remove_dir_all(&dir);
        assert!(missing.contains("is missing"), "{missing}");
        assert!(wrong.contains("is 4 bytes"), "{wrong}");
        assert_eq!(r.inputs["a.mp4"].sha256, ABC);
        assert!(checked.is_ok());
        assert_eq!(Reference::parse(&r.render()).unwrap(), r);
    }

    #[test]
    fn inputs_read_as_a_size_or_a_size_and_sha256() {
        let text = sample().render();
        assert!(text.contains(&format!(
            "\"h264-1080.mp4\" = {{ bytes = 10044607, sha256 = \"{ABC}\" }}"
        )));
        assert!(text.contains("\"raw-1080.yuv\" = { bytes = 3 }"));
        let plain = text.replace("\"raw-1080.yuv\" = { bytes = 3 }", "\"raw-1080.yuv\" = 3");
        assert_eq!(Reference::parse(&plain).unwrap(), sample());
        let upper = text.replace(ABC, &ABC.to_uppercase());
        assert_eq!(Reference::parse(&upper).unwrap(), sample());
        for bad in [
            "\"raw-1080.yuv\" = { sha256 = \"ab\" }",
            "\"raw-1080.yuv\" = { bytes = 3, sha256 = \"xyz\" }",
            "\"raw-1080.yuv\" = { bytes = 3, bytes = 4 }",
            "\"raw-1080.yuv\" = { bytes = -3 }",
            "\"raw-1080.yuv\" = { bytes = 3, size = 3 }",
            "\"raw-1080.yuv\" = \"three\"",
        ] {
            let text = text.replace("\"raw-1080.yuv\" = { bytes = 3 }", bad);
            let err = Reference::parse(&text).unwrap_err();
            assert!(
                err.ends_with("bad input") || err.ends_with("bad value"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn a_case_without_a_positive_value_is_refused() {
        let base = sample().render();
        for (from, to) in [
            ("value = 85.2\n", ""),
            ("value = 85.2", "value = 0.0"),
            ("value = 85.2", "value = -85.2"),
            ("value = 85.2", "value = \"fast\""),
        ] {
            let text = base.replace(from, to);
            let err = Reference::parse(&text).unwrap_err();
            if to.contains('"') {
                assert!(err.ends_with("bad case field"), "{to}: {err}");
            } else {
                assert_eq!(
                    err, "case first-frame.h264.1440.cpu needs a value above 0",
                    "{to}"
                );
            }
        }
    }

    #[test]
    fn a_case_without_positive_runs_is_refused() {
        let base = sample().render();
        for (from, to) in [
            ("runs = [85.2]\n", ""),
            ("runs = [85.2]", "runs = []"),
            ("runs = [85.2]", "runs = [85.2, 0.0]"),
            ("runs = [85.2]", "runs = [-1.0]"),
        ] {
            let err = Reference::parse(&base.replace(from, to)).unwrap_err();
            assert_eq!(
                err, "case first-frame.h264.1440.cpu needs runs above 0",
                "{to}"
            );
        }
    }

    #[test]
    fn duplicate_tables_and_keys_are_refused() {
        let base = sample().render();
        let twice = format!("{base}\n[cases.\"first-frame.h264.1440.cpu\"]\nunit = \"ms\"\n");
        assert!(
            Reference::parse(&twice)
                .unwrap_err()
                .ends_with("duplicate table")
        );
        let inputs = format!("{base}\n[inputs]\n\"b.mp4\" = 4\n");
        assert!(
            Reference::parse(&inputs)
                .unwrap_err()
                .ends_with("duplicate table")
        );
        for (from, to) in [
            ("value = 85.2\n", "value = 85.2\nvalue = 90.0\n"),
            ("runs = 3\n", "runs = 3\nruns = 4\n"),
            (
                "\"raw-1080.yuv\" = { bytes = 3 }\n",
                "\"raw-1080.yuv\" = { bytes = 3 }\n\"raw-1080.yuv\" = 3\n",
            ),
        ] {
            let err = Reference::parse(&base.replacen(from, to, 1)).unwrap_err();
            assert!(err.starts_with("line "), "{err}");
            assert!(err.ends_with("duplicate key"), "{err}");
        }
    }

    #[test]
    fn key_value_lines_read_with_any_spacing_around_the_sign() {
        let base = sample().render();
        let tight = base
            .replace("format = 1", "format=1")
            .replace("unit = \"ms\"", "unit=\"ms\"")
            .replace("value = 85.2", "value   =   85.2")
            .replace(
                "\"raw-1080.yuv\" = { bytes = 3 }",
                "\"raw-1080.yuv\"={bytes=3}",
            );
        assert_eq!(Reference::parse(&tight).unwrap(), sample());
        let bare = base.replace("os = ", "o s = ");
        assert!(Reference::parse(&bare).unwrap_err().ends_with("bad key"));
    }

    #[test]
    fn same_ffmpeg_needs_the_version_and_configuration_lines() {
        let r = sample();
        assert!(r.same_ffmpeg(&r.ffmpeg, &r.configuration).is_ok());
        let other = "ffmpeg version 9.1 Copyright (c) 2000-2026 the FFmpeg developers";
        let err = r
            .same_ffmpeg(other, &r.configuration)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!(
                "the reference holds `{}`, the ffmpeg on PATH is `{other}`; one file holds one ffmpeg build, so record every case without --only",
                r.ffmpeg
            )
        );
        assert!(r.same_ffmpeg(&r.ffmpeg, "configuration: --other").is_err());
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
