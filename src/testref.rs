#![allow(dead_code)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[allow(dead_code)]
#[path = "bin/pcod/sha256.rs"]
mod sha256;

pub const FILE_BUDGET: u64 = 64 * 1024;
pub const TOTAL_BUDGET: u64 = 2 * 1024 * 1024;
pub const MANIFEST: &str = "manifest.toml";
pub const SOURCE: &str = "SOURCE";

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn refs_dir() -> PathBuf {
    root().join("tests/refs")
}

pub fn area_dir(area: &str) -> PathBuf {
    refs_dir().join(area)
}

pub fn scratch(area: &str) -> PathBuf {
    let d = root().join("tmp/testref").join(area);
    std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{}: {e}", d.display()));
    d
}

pub fn rerecord(area: &str) -> String {
    format!("cargo run --release --example record_refs -- {area}")
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let part = part_of(path);
    std::fs::write(&part, bytes).map_err(|e| format!("{}: {e}", part.display()))?;
    std::fs::rename(&part, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn part_of(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{}-{n}.part.{name}", std::process::id()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Fixture,
    BitExact,
    Tolerance,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Fixture => "fixture",
            Kind::BitExact => "bit-exact",
            Kind::Tolerance => "tolerance",
        }
    }

    pub fn parse(text: &str) -> Result<Kind, String> {
        match text {
            "fixture" => Ok(Kind::Fixture),
            "bit-exact" => Ok(Kind::BitExact),
            "tolerance" => Ok(Kind::Tolerance),
            _ => Err(format!(
                "kind '{text}': use fixture, bit-exact or tolerance"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub file: String,
    pub sha256: String,
    pub kind: Kind,
    pub tool: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    pub entries: Vec<Entry>,
}

const KEYS: [&str; 6] = ["name", "file", "sha256", "kind", "tool", "command"];

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let mut entries = Vec::new();
        let mut open: Option<[Option<String>; 6]> = None;
        let close = |fields: [Option<String>; 6], entries: &mut Vec<Entry>| {
            let [name, file, sha256, kind, tool, command] = fields;
            let need =
                |v: Option<String>, k: &str| v.ok_or_else(|| format!("a [[ref]] has no {k}"));
            let name = need(name, "name")?;
            let sha256 = need(sha256, "sha256")?;
            if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("{name}: sha256 '{sha256}' is not 64 hex digits"));
            }
            if entries.iter().any(|e: &Entry| e.name == name) {
                return Err(format!("{name}: named twice"));
            }
            entries.push(Entry {
                file: need(file, "file")?,
                sha256,
                kind: Kind::parse(&need(kind, "kind")?)?,
                tool: need(tool, "tool")?,
                command: need(command, "command")?,
                name,
            });
            Ok::<(), String>(())
        };
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            let at = |m: String| format!("line {}: {m}", i + 1);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[[ref]]" {
                if let Some(fields) = open.take() {
                    close(fields, &mut entries).map_err(at)?;
                }
                open = Some(Default::default());
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| at(format!("'{line}' is not key = \"value\"")))?;
            let key = key.trim();
            let slot = KEYS
                .iter()
                .position(|k| *k == key)
                .ok_or_else(|| at(format!("unknown key '{key}'")))?;
            let fields = open
                .as_mut()
                .ok_or_else(|| at(format!("'{key}' comes before any [[ref]]")))?;
            if fields[slot].is_some() {
                return Err(at(format!("'{key}' given twice")));
            }
            fields[slot] = Some(unquote(value.trim()).map_err(at)?);
        }
        if let Some(fields) = open {
            close(fields, &mut entries)?;
        }
        Ok(Manifest { entries })
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, e) in self.entries.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str("[[ref]]\n");
            let values = [
                &e.name,
                &e.file,
                &e.sha256,
                e.kind.as_str(),
                &e.tool,
                &e.command,
            ];
            for (k, v) in KEYS.iter().zip(values) {
                let _ = writeln!(out, "{k} = {}", quote(v));
            }
        }
        out
    }

    pub fn load(area: &str) -> Result<Manifest, String> {
        let path = area_dir(area).join(MANIFEST);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Manifest::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

pub const TOOLS: &str = "TOOLS.toml";

pub fn tools_path() -> PathBuf {
    refs_dir().join(TOOLS)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Command {
        run: Vec<String>,
        find: String,
        upto: String,
    },
    Library { path: String, call: String, int: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub probe: Probe,
    pub line: String,
}

impl Pin {
    pub fn check(&self, found: &str) -> Result<String, String> {
        if found == self.line {
            Ok(self.line.clone())
        } else {
            Err(format!(
                "{} is pinned at '{}' in tests/refs/{TOOLS} but this machine has '{found}'",
                self.name, self.line
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tools {
    pub pins: Vec<Pin>,
}

const TOOL_KEYS: [&str; 7] = [
    "version", "find", "upto", "library", "call", "returns", "line",
];

type ToolFields = [Option<String>; 7];

fn close_pin(name: String, fields: ToolFields, pins: &mut Vec<Pin>) -> Result<(), String> {
    let [version, find, upto, library, call, returns, line] = fields;
    let line = line.ok_or_else(|| format!("[{name}] has no line"))?;
    let probe = match (version, library) {
        (Some(v), None) => {
            if call.is_some() || returns.is_some() {
                return Err(format!("[{name}]: call and returns go with library"));
            }
            let run: Vec<String> = v.split_whitespace().map(str::to_string).collect();
            if run.is_empty() {
                return Err(format!("[{name}]: version is empty"));
            }
            Probe::Command {
                run,
                find: find.unwrap_or_default(),
                upto: upto.unwrap_or_default(),
            }
        }
        (None, Some(path)) => {
            if find.is_some() || upto.is_some() {
                return Err(format!("[{name}]: find and upto go with version"));
            }
            let call = call.ok_or_else(|| format!("[{name}]: a library needs call"))?;
            let int = match returns.as_deref() {
                None | Some("string") => false,
                Some("int") => true,
                Some(other) => {
                    return Err(format!(
                        "[{name}]: returns is \"string\" or \"int\", not \"{other}\""
                    ));
                }
            };
            Probe::Library { path, call, int }
        }
        _ => return Err(format!("[{name}] needs one of version or library")),
    };
    if pins.iter().any(|p| p.name == name) {
        return Err(format!("[{name}] given twice"));
    }
    pins.push(Pin { name, probe, line });
    Ok(())
}

impl Tools {
    pub fn parse(text: &str) -> Result<Tools, String> {
        let mut pins = Vec::new();
        let mut open: Option<(String, ToolFields)> = None;
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            let at = |m: String| format!("line {}: {m}", i + 1);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let name = name.trim();
                let bad = name.is_empty()
                    || !name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
                if bad {
                    return Err(at(format!("'{line}' is not a [tool] header")));
                }
                if let Some((n, fields)) = open.take() {
                    close_pin(n, fields, &mut pins).map_err(at)?;
                }
                open = Some((name.to_string(), Default::default()));
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| at(format!("'{line}' is not key = \"value\"")))?;
            let key = key.trim();
            let slot = TOOL_KEYS
                .iter()
                .position(|k| *k == key)
                .ok_or_else(|| at(format!("unknown key '{key}'")))?;
            let (_, fields) = open
                .as_mut()
                .ok_or_else(|| at(format!("'{key}' comes before any [tool]")))?;
            if fields[slot].is_some() {
                return Err(at(format!("'{key}' given twice")));
            }
            fields[slot] = Some(unquote(value.trim()).map_err(at)?);
        }
        if let Some((n, fields)) = open {
            close_pin(n, fields, &mut pins)?;
        }
        Ok(Tools { pins })
    }

    pub fn load() -> Result<Tools, String> {
        let path = tools_path();
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Tools::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn pin(&self, name: &str) -> Result<&Pin, String> {
        self.pins
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("{name} is not pinned in tests/refs/{TOOLS}; add its table first"))
    }
}

pub fn version_line(output: &str, find: &str, upto: &str) -> Option<String> {
    let line = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .find(|l| l.contains(find))?;
    let line = match line.find(upto) {
        Some(at) if !upto.is_empty() => line[..at].trim_end(),
        _ => line,
    };
    Some(line.to_string())
}

pub fn library_line(file: &str, version: &str) -> String {
    format!("{file} {version}")
}

pub fn int_version(v: i32) -> String {
    format!("{}.{}.{}", (v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff)
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn unquote(text: &str) -> Result<String, String> {
    let body = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .filter(|_| text.len() >= 2)
        .ok_or_else(|| format!("{text} is not a \"quoted\" string"))?;
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Err(format!("{text}: a bare quote inside the string")),
            '\\' => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let c = u32::from_str_radix(&hex, 16)
                        .ok()
                        .filter(|_| hex.len() == 4)
                        .and_then(char::from_u32)
                        .ok_or_else(|| format!("{text}: bad \\u{hex}"))?;
                    out.push(c);
                }
                other => return Err(format!("{text}: unknown escape \\{other:?}")),
            },
            c => out.push(c),
        }
    }
    Ok(out)
}

pub fn sha256(bytes: &[u8]) -> String {
    let mut h = sha256::Sha256::new();
    h.update(bytes);
    sha256::hex(&h.finish())
}

pub struct Refs {
    pub area: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Refs {
    pub fn open(area: &str) -> Refs {
        Refs::at(area_dir(area), area)
    }

    pub fn at(dir: PathBuf, area: &str) -> Refs {
        let path = dir.join(MANIFEST);
        let manifest = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| Manifest::parse(&t))
            .unwrap_or_else(|e| {
                panic!(
                    "{}: {e}; record it with `{}` where ffmpeg exists",
                    path.display(),
                    rerecord(area)
                )
            });
        Refs {
            area: area.to_string(),
            dir,
            manifest,
        }
    }

    pub fn entry(&self, name: &str) -> &Entry {
        self.manifest.entry(name).unwrap_or_else(|| {
            panic!(
                "tests/refs/{}/{MANIFEST} has no '{name}'; add it to examples/record_refs/{}.rs and run `{}`",
                self.area,
                self.area,
                rerecord(&self.area)
            )
        })
    }

    pub fn bytes(&self, name: &str) -> Vec<u8> {
        let e = self.entry(name);
        assert!(
            !e.file.is_empty(),
            "{}/{name} is a digest with no file; use Refs::assert_digest",
            self.area
        );
        let path = self.dir.join(&e.file);
        let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let got = sha256(&bytes);
        assert!(
            got == e.sha256,
            "{}: sha256 {got}, the manifest records {}; rerun `{}` where ffmpeg exists",
            path.display(),
            e.sha256,
            rerecord(&self.area)
        );
        bytes
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.bytes(name);
        self.dir.join(&self.entry(name).file)
    }

    pub fn digest(&self, name: &str) -> &str {
        &self.entry(name).sha256
    }

    pub fn assert_digest(&self, name: &str, got: &str) {
        let e = self.entry(name);
        assert!(
            got == e.sha256,
            "{}/{name}: ours hashes to {got}, {} recorded {} with `{}`",
            self.area,
            e.tool,
            e.sha256,
            e.command
        );
    }

    pub fn f32s(&self, name: &str) -> Vec<f32> {
        le_f32(&self.bytes(name))
    }

    pub fn i16s(&self, name: &str) -> Vec<i16> {
        le_i16(&self.bytes(name))
    }
}

pub fn rgba_digest(bytes: &[u8]) -> String {
    let mut d = Digest::new();
    d.rgba_bytes(bytes);
    d.hex()
}

pub fn le_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

pub fn le_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

#[derive(Default)]
pub struct Digest(sha256::Sha256);

impl Digest {
    pub fn new() -> Digest {
        Digest::default()
    }

    pub fn bytes(&mut self, data: &[u8]) -> &mut Digest {
        self.0.update(data);
        self
    }

    pub fn f32s(&mut self, data: &[f32]) -> &mut Digest {
        for v in data {
            self.0.update(&v.to_le_bytes());
        }
        self
    }

    pub fn i16s(&mut self, data: &[i16]) -> &mut Digest {
        for v in data {
            self.0.update(&v.to_le_bytes());
        }
        self
    }

    #[cfg(feature = "media")]
    pub fn frame(&mut self, f: &pito_codecs::media::Frame) -> &mut Digest {
        let bytes = f.format.bytes_per_sample();
        for p in &f.planes {
            for row in p.data.chunks(p.stride.max(1)).take(p.height) {
                self.0.update(&row[..(p.width * bytes).min(row.len())]);
            }
        }
        self
    }

    pub fn rgba_bytes(&mut self, data: &[u8]) -> &mut Digest {
        let mut row = Vec::with_capacity(4096);
        for chunk in data.chunks(4096) {
            row.clear();
            for px in chunk.chunks(4) {
                if px.len() == 4 && px[3] == 0 {
                    row.extend_from_slice(&[0; 4]);
                } else {
                    row.extend_from_slice(px);
                }
            }
            self.0.update(&row);
        }
        self
    }

    #[cfg(feature = "media")]
    pub fn rgba(&mut self, f: &pito_codecs::media::Rgba) -> &mut Digest {
        self.rgba_bytes(&f.data)
    }

    pub fn hex(self) -> String {
        sha256::hex(&self.0.finish())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Diff {
    pub max: f64,
    pub rms: f64,
    pub ours: usize,
    pub theirs: usize,
}

impl Diff {
    pub fn within(&self, max: f64, rms: f64) -> bool {
        self.ours == self.theirs && self.max <= max && self.rms <= rms
    }
}

pub fn diff<T: Copy + Into<f64>>(ours: &[T], theirs: &[T]) -> Diff {
    let mut max = 0.0f64;
    let mut sum = 0.0f64;
    for (a, b) in ours.iter().zip(theirs) {
        let d = (*a).into() - (*b).into();
        max = max.max(d.abs());
        sum += d * d;
    }
    let n = ours.len().min(theirs.len()).max(1) as f64;
    Diff {
        max,
        rms: (sum / n).sqrt(),
        ours: ours.len(),
        theirs: theirs.len(),
    }
}

pub fn assert_within(what: &str, d: &Diff, max: f64, rms: f64) {
    assert!(
        d.within(max, rms),
        "{what}: {} samples against {}, max {:.6} (limit {max}), rms {:.6} (limit {rms})",
        d.ours,
        d.theirs,
        d.max,
        d.rms
    );
}

pub fn psnr(ours: &[u8], theirs: &[u8]) -> f64 {
    let d = diff(ours, theirs);
    10.0 * (255.0f64 * 255.0 / (d.rms * d.rms).max(1e-12)).log10()
}

pub fn snr_db(ours: &[f32], theirs: &[f32]) -> f64 {
    let signal: f64 = theirs.iter().map(|v| f64::from(*v).powi(2)).sum();
    let noise: f64 = ours
        .iter()
        .zip(theirs)
        .map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2))
        .sum();
    10.0 * (signal.max(1e-30) / noise.max(1e-30)).log10()
}

fn mix(x: u32, y: u32, n: u64) -> u32 {
    let mut v = u64::from(x) | (u64::from(y) << 20) | (n << 40);
    v ^= v >> 33;
    v = v.wrapping_mul(0xff51_afd7_ed55_8ccd);
    v ^= v >> 33;
    v = v.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    (v ^ (v >> 33)) as u32
}

pub fn noise(x: u32, y: u32, n: u64, amp: u8) -> i32 {
    if amp == 0 {
        return 0;
    }
    let span = 2 * u32::from(amp) + 1;
    (mix(x, y, n) % span) as i32 - i32::from(amp)
}

const BARS: [[u8; 3]; 8] = [
    [191, 191, 191],
    [191, 191, 0],
    [0, 191, 191],
    [0, 191, 0],
    [191, 0, 191],
    [191, 0, 0],
    [0, 0, 191],
    [16, 16, 16],
];

pub fn rgb_at(w: u32, h: u32, n: u64, x: u32, y: u32) -> [u8; 3] {
    let (w, h) = (w.max(1), h.max(3));
    let band = h / 3;
    let side = (band / 2).max(2);
    let room = u64::from(w.saturating_sub(side).max(1));
    let sx = ((n * 7) % room) as u32;
    let sy = band + (band - side.min(band)) / 2;
    if (sx..sx + side).contains(&x) && (sy..sy + side).contains(&y) {
        let edge = x < sx + 2 || y < sy + 2 || x + 2 >= sx + side || y + 2 >= sy + side;
        return if edge { [0, 0, 0] } else { [255, 255, 255] };
    }
    if y < band {
        return BARS[(x * 8 / w).min(7) as usize];
    }
    if y < 2 * band {
        let t = (n as u32).wrapping_mul(3);
        return [
            (x * 255 / w) as u8,
            ((y - band) * 255 / band.max(1)) as u8,
            ((x + y).wrapping_add(t) % 256) as u8,
        ];
    }
    let g = ((x * 255 / w) as u64 + n * 2) % 256;
    let stripe = (u64::from(x) + n * 5) % 64 < 4;
    if stripe { [255, 64, 32] } else { [g as u8; 3] }
}

pub fn rgb_noisy(w: u32, h: u32, n: u64, x: u32, y: u32, amp: u8) -> [u8; 3] {
    let p = rgb_at(w, h, n, x, y);
    let d = noise(x, y, n, amp);
    p.map(|c| (i32::from(c) + d).clamp(0, 255) as u8)
}

pub fn yuv_of(p: [u8; 3]) -> [u8; 3] {
    let [r, g, b] = p.map(i32::from);
    let y = ((47 * r + 157 * g + 16 * b + 128) >> 8) + 16;
    let u = ((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128;
    let v = ((112 * r - 102 * g - 10 * b + 128) >> 8) + 128;
    [y, u, v].map(|c| c.clamp(0, 255) as u8)
}

#[cfg(feature = "media")]
pub fn rgba_frame(w: u32, h: u32, n: u64, amp: u8) -> pito_codecs::media::Rgba {
    let mut f = pito_codecs::media::Rgba::new(w, h);
    for (i, px) in f.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        let [r, g, b] = rgb_noisy(w, h, n, x, y, amp);
        px.copy_from_slice(&[r, g, b, 255]);
    }
    f
}

#[cfg(feature = "media")]
pub fn rgba_frames(w: u32, h: u32, count: u64, amp: u8) -> Vec<pito_codecs::media::Rgba> {
    (0..count).map(|n| rgba_frame(w, h, n, amp)).collect()
}

#[cfg(feature = "media")]
pub fn draw_yuv(n: u64, amp: u8, f: &mut pito_codecs::media::Frame) {
    let (w, h) = (f.width, f.height);
    let [yp, up, vp] = &mut f.planes;
    for y in 0..h {
        for x in 0..w {
            yp.data[y as usize * yp.stride + x as usize] = yuv_of(rgb_noisy(w, h, n, x, y, amp))[0];
        }
    }
    for cy in 0..up.height as u32 {
        for cx in 0..up.width as u32 {
            let mut sum = [0i32; 2];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, y) = ((2 * cx + dx).min(w - 1), (2 * cy + dy).min(h - 1));
                let c = yuv_of(rgb_noisy(w, h, n, x, y, amp));
                sum[0] += i32::from(c[1]);
                sum[1] += i32::from(c[2]);
            }
            up.data[cy as usize * up.stride + cx as usize] = ((sum[0] + 2) >> 2) as u8;
            vp.data[cy as usize * vp.stride + cx as usize] = ((sum[1] + 2) >> 2) as u8;
        }
    }
}

#[cfg(feature = "media")]
pub fn yuv_frame(w: u32, h: u32, n: u64, amp: u8) -> pito_codecs::media::Frame {
    use pito_codecs::media::{Frame, PixelFormat, TimeBase};
    let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, n as i64, TimeBase::new(1, 1));
    draw_yuv(n, amp, &mut f);
    f
}

#[derive(Debug, Clone, Copy)]
pub enum Sound {
    Silence,
    Sine { hz: f64, amp: f64 },
    Noise { amp: f64, seed: u32 },
    Fn(fn(u64, u16, u32) -> f32),
}

impl Sound {
    pub fn at(self, i: u64, ch: u16, rate: u32) -> f32 {
        match self {
            Sound::Silence => 0.0,
            Sound::Sine { hz, amp } => {
                let t = i as f64 / f64::from(rate);
                let phase = f64::from(ch) * 0.25;
                (amp * (std::f64::consts::TAU * (hz * t + phase)).sin()) as f32
            }
            Sound::Noise { amp, seed } => {
                let r = mix(seed, u32::from(ch), i);
                (amp * (f64::from(r) / f64::from(u32::MAX) * 2.0 - 1.0)) as f32
            }
            Sound::Fn(f) => f(i, ch, rate),
        }
    }
}

pub fn pcm(sound: Sound, rate: u32, channels: u16, start: u64, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * usize::from(channels));
    for i in start..start + frames as u64 {
        for ch in 0..channels {
            out.push(sound.at(i, ch, rate));
        }
    }
    out
}

pub fn s16_of(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|v| (f64::from(*v) * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

#[cfg(all(feature = "h264", feature = "aac", feature = "mp4", feature = "mkv"))]
#[path = "testref_clip.rs"]
mod clip;

#[cfg(all(feature = "h264", feature = "aac", feature = "mp4", feature = "mkv"))]
#[allow(unused_imports)]
pub use clip::{AudioCodec, Clip, Picture, VideoCodec};
