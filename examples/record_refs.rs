use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::OnceLock;

#[path = "support/fail.rs"]
mod fail;
#[path = "../src/testref.rs"]
mod testref;

#[path = "record_refs/audio.rs"]
mod audio;
#[path = "record_refs/containers.rs"]
mod containers;
#[path = "record_refs/filter.rs"]
mod filter;
#[path = "record_refs/gif.rs"]
mod gif;
#[path = "record_refs/gpu.rs"]
mod gpu;
#[path = "record_refs/images.rs"]
mod images;
#[path = "record_refs/media.rs"]
mod media;
#[path = "record_refs/video.rs"]
mod video;
#[path = "record_refs/vorbis.rs"]
mod vorbis;

use testref::{Entry, Kind, Manifest, Pin, Probe, Tools};

pub struct Def {
    pub area: &'static str,
    pub name: String,
    pub file: String,
    pub kind: Kind,
    pub make: Option<Make>,
    pub tool: &'static str,
    pub args: Vec<String>,
    pub take: Take,
    pub lib: Option<&'static str>,
    pub settle: Option<Settle>,
}

pub type Make = Box<dyn Fn(&Path) -> Result<(), String>>;

pub type Settle = (&'static str, fn(&mut [u8]) -> Result<(), String>);

pub enum Take {
    Keep,
    Digest(fn(&[u8]) -> Result<String, String>),
}

impl Def {
    fn kept(
        area: &'static str,
        file: String,
        kind: Kind,
        tool: &'static str,
        args: Vec<String>,
    ) -> Def {
        Def {
            area,
            name: file.clone(),
            file,
            kind,
            make: None,
            tool,
            args,
            take: Take::Keep,
            lib: None,
            settle: None,
        }
    }

    pub fn fixture(area: &'static str, file: String, tool: &'static str, args: Vec<String>) -> Def {
        Def::kept(area, file, Kind::Fixture, tool, args)
    }

    pub fn tolerance(
        area: &'static str,
        file: String,
        tool: &'static str,
        args: Vec<String>,
    ) -> Def {
        Def::kept(area, file, Kind::Tolerance, tool, args)
    }

    pub fn exact(area: &'static str, file: String, tool: &'static str, args: Vec<String>) -> Def {
        Def::kept(area, file, Kind::BitExact, tool, args)
    }

    pub fn digest(
        area: &'static str,
        name: String,
        tool: &'static str,
        args: Vec<String>,
        f: fn(&[u8]) -> Result<String, String>,
    ) -> Def {
        Def {
            area,
            name,
            file: String::new(),
            kind: Kind::BitExact,
            make: None,
            tool,
            args,
            take: Take::Digest(f),
            lib: None,
            settle: None,
        }
    }

    pub fn ours(
        area: &'static str,
        name: String,
        what: &str,
        make: impl Fn() -> Result<Vec<u8>, String> + 'static,
    ) -> Def {
        let mut d = Def::digest(area, name, OURS, words(what), sha);
        d.make = Some(Box::new(move |at| testref::write_atomic(at, &make()?)));
        d
    }

    pub fn library(mut self, name: &'static str) -> Def {
        self.lib = Some(name);
        self
    }

    pub fn settled(mut self, what: &'static str, f: fn(&mut [u8]) -> Result<(), String>) -> Def {
        self.settle = Some((what, f));
        self
    }

    pub fn from(mut self, make: impl Fn() -> Result<Vec<u8>, String> + 'static) -> Def {
        self.make = Some(Box::new(move |at| testref::write_atomic(at, &make()?)));
        self
    }
}

pub const MP4_CLOCK: &str = "mp4_clock_zeroed (mvhd, tkhd and mdhd times set to 0)";

pub fn mp4_clock_zeroed(bytes: &mut [u8]) -> Result<(), String> {
    let mut found = 0;
    zero_clocks(bytes, &mut found)?;
    if found == 0 {
        return Err("no mvhd, tkhd or mdhd box to settle".into());
    }
    Ok(())
}

fn zero_clocks(data: &mut [u8], found: &mut usize) -> Result<(), String> {
    let mut at = 0;
    while at + 8 <= data.len() {
        let small = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        let (head, size) = match small {
            0 => (8, data.len() - at),
            1 => {
                let wide = data
                    .get(at + 8..at + 16)
                    .ok_or("a box cut inside its size")?;
                let mut b = [0; 8];
                b.copy_from_slice(wide);
                let size = usize::try_from(u64::from_be_bytes(b)).map_err(|e| e.to_string())?;
                (16, size)
            }
            n => (8, n as usize),
        };
        if size < head || at + size > data.len() {
            return Err(format!(
                "box {} at {at} runs past its parent",
                String::from_utf8_lossy(&kind)
            ));
        }
        let body = &mut data[at + head..at + size];
        match &kind {
            b"moov" | b"trak" | b"mdia" => zero_clocks(body, found)?,
            b"mvhd" | b"tkhd" | b"mdhd" => {
                let wide = match body.first() {
                    Some(0) => 4,
                    Some(1) => 8,
                    _ => {
                        return Err(format!(
                            "{} with no version",
                            String::from_utf8_lossy(&kind)
                        ));
                    }
                };
                let times = body
                    .get_mut(4..4 + 2 * wide)
                    .ok_or_else(|| format!("{} cut short", String::from_utf8_lossy(&kind)))?;
                times.fill(0);
                *found += 1;
            }
            _ => {}
        }
        at += size;
    }
    Ok(())
}

pub fn sha(bytes: &[u8]) -> Result<String, String> {
    Ok(testref::sha256(bytes))
}

pub const OURS: &str = "";

pub fn words(text: &str) -> Vec<String> {
    vec![text.to_string()]
}

pub fn tool(name: &str, args: &[&str]) -> Vec<String> {
    if let Err(e) = pins().and_then(|t| t.pin(name).map(|_| ())) {
        panic!("{e}");
    }
    args.iter().map(|s| s.to_string()).collect()
}

pub fn lib_version(name: &str) -> Result<String, String> {
    let pin = pins()?.pin(name)?;
    let Probe::Library { path, call, int } = &pin.probe else {
        return Err(format!("{name} is pinned as a command, not a library"));
    };
    let file = std::fs::canonicalize(path)
        .map_err(|e| format!("{name}: {path}: {e}"))?
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let version = call_version(path, call, *int).map_err(|e| format!("{name}: {e}"))?;
    pin.check(&testref::library_line(&file, &version))
}

fn pins() -> Result<&'static Tools, String> {
    static TOOLS: OnceLock<Result<Tools, String>> = OnceLock::new();
    TOOLS
        .get_or_init(Tools::load)
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(target_os = "linux")]
fn call_version(path: &str, call: &str, int: bool) -> Result<String, String> {
    use std::ffi::{CStr, c_char, c_int};
    let lib = unsafe { libloading::Library::new(path) }.map_err(|e| format!("{path}: {e}"))?;
    let missing = |e: libloading::Error| format!("{path} has no {call}: {e}");
    if int {
        let f = unsafe { lib.get::<unsafe extern "C" fn() -> c_int>(call.as_bytes()) }
            .map_err(missing)?;
        return Ok(testref::int_version(unsafe { f() }));
    }
    let f = unsafe { lib.get::<unsafe extern "C" fn() -> *const c_char>(call.as_bytes()) }
        .map_err(missing)?;
    let text = unsafe { f() };
    if text.is_null() {
        return Err(format!("{call} in {path} returned no string"));
    }
    Ok(unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned())
}

#[cfg(not(target_os = "linux"))]
fn call_version(path: &str, _call: &str, _int: bool) -> Result<String, String> {
    Err(format!("{path}: reference libraries load only on Linux"))
}

fn command_version(pin: &Pin) -> Result<String, String> {
    let Probe::Command { run, find, upto } = &pin.probe else {
        return Err(format!(
            "{} is pinned as a library, not a command",
            pin.name
        ));
    };
    let o = Command::new(&run[0])
        .args(&run[1..])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{} did not start: {e}", run[0]))?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let found = testref::version_line(&text, find, upto)
        .ok_or_else(|| format!("`{}` printed no line with '{find}'", run.join(" ")))?;
    pin.check(&found)
}

fn checked(area: &str, defs: &[&Def]) -> Result<BTreeMap<&'static str, String>, String> {
    let mut lines = BTreeMap::new();
    let wanted = defs.iter().flat_map(|d| {
        let run = (d.tool != OURS).then_some((d.tool, false));
        run.into_iter().chain(d.lib.map(|l| (l, true)))
    });
    for (name, lib) in wanted {
        if lines.contains_key(name) {
            continue;
        }
        let line = if lib {
            lib_version(name)
        } else {
            pins().and_then(|t| t.pin(name)).and_then(command_version)
        };
        lines.insert(name, line.map_err(|e| format!("{area} refused: {e}"))?);
    }
    Ok(lines)
}

fn table() -> Vec<Def> {
    [
        audio::defs(),
        vorbis::defs(),
        filter::defs(),
        images::defs(),
        gif::defs(),
        video::defs(),
        gpu::defs(),
        media::defs(),
        containers::defs(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn main() -> ExitCode {
    fail::run("record_refs", || {
        let wanted: Vec<String> = std::env::args().skip(1).collect();
        let defs = table();
        let areas: BTreeSet<&str> = defs.iter().map(|d| d.area).collect();
        if let Some(bad) = wanted.iter().find(|a| !areas.contains(a.as_str())) {
            let known: Vec<&str> = areas.iter().copied().collect();
            return Err(format!("no area '{bad}'; the areas are {}", known.join(", ")).into());
        }
        for area in areas {
            if wanted.is_empty() || wanted.iter().any(|a| a == area) {
                let mine: Vec<&Def> = defs.iter().filter(|d| d.area == area).collect();
                record(area, &mine)?;
            }
        }
        Ok(())
    })
}

fn record(area: &str, defs: &[&Def]) -> Result<(), Box<dyn Error>> {
    let pinned = checked(area, defs)?;
    let work = testref::root().join("tmp/record_refs").join(area);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let mut entries = Vec::new();
    let mut kept: Vec<(String, PathBuf)> = Vec::new();
    let mut tools = BTreeSet::new();
    for d in defs {
        let label = format!("{area}/{}", d.name);
        let out = work.join(if d.file.is_empty() { &d.name } else { &d.file });
        let src = work.join(format!("{}.src", d.name));
        let args: Vec<String> = d
            .args
            .iter()
            .map(|a| expand(a, &src, &out, &work, defs))
            .collect();
        let tool = if d.tool.is_empty() {
            let make = d
                .make
                .as_ref()
                .ok_or(format!("{label}: our own output needs make"))?;
            make(&out).map_err(|e| format!("{label}: {e}"))?;
            match d.lib {
                Some(lib) => pinned[lib].clone(),
                None => format!("pito-codecs {}", env!("CARGO_PKG_VERSION")),
            }
        } else {
            if let Some(make) = &d.make {
                make(&src).map_err(|e| format!("{label}: {e}"))?;
            }
            let run = Command::new(d.tool)
                .args(&args)
                .current_dir(&work)
                .output()
                .map_err(|e| format!("{label}: {} did not start: {e}", d.tool))?;
            if !run.status.success() {
                let why = String::from_utf8_lossy(&run.stderr);
                return Err(format!("{label}: {} failed: {}", d.tool, why.trim()).into());
            }
            pinned[d.tool].clone()
        };
        if let Some((_, settle)) = &d.settle {
            let mut bytes = std::fs::read(&out)
                .map_err(|e| format!("{label}: no output at {}: {e}", out.display()))?;
            settle(&mut bytes).map_err(|e| format!("{label}: {e}"))?;
            testref::write_atomic(&out, &bytes).map_err(|e| format!("{label}: {e}"))?;
        }
        tools.insert(tool.clone());
        let bytes = std::fs::read(&out)
            .map_err(|e| format!("{label}: no output at {}: {e}", out.display()))?;
        let sha256 = match (&d.take, d.file.is_empty()) {
            (Take::Keep, false) => {
                kept.push((d.file.clone(), out.clone()));
                testref::sha256(&bytes)
            }
            (Take::Digest(f), true) if d.kind == Kind::BitExact => {
                f(&bytes).map_err(|e| format!("{label}: {e}"))?
            }
            _ => {
                return Err(format!(
                    "{label}: keep a named file, or digest a bit-exact output with no file"
                )
                .into());
            }
        };
        let mut command = if d.tool.is_empty() {
            format!("examples/record_refs: {}", d.args.join(" "))
        } else {
            format!("{} {}", d.tool, d.args.join(" "))
        };
        if let Some((what, _)) = &d.settle {
            command.push_str(&format!("; examples/record_refs: {what}"));
        }
        entries.push(Entry {
            name: d.name.clone(),
            file: d.file.clone(),
            sha256,
            kind: d.kind,
            tool,
            command,
        });
    }
    let mut total = 0;
    for (file, path) in &kept {
        let size = std::fs::metadata(path)?.len();
        if size > testref::FILE_BUDGET {
            return Err(format!(
                "{area}/{file} is {size} bytes, over the {} byte budget: fewer frames, smaller sizes or shorter audio",
                testref::FILE_BUDGET
            )
            .into());
        }
        total += size;
    }
    let others = tracked_bytes(area)?;
    if total + others > testref::TOTAL_BUDGET {
        return Err(format!(
            "tests/refs would hold {} bytes, over the {} byte budget",
            total + others,
            testref::TOTAL_BUDGET
        )
        .into());
    }
    let dir = testref::area_dir(area);
    std::fs::create_dir_all(&dir)?;
    for old in std::fs::read_dir(&dir)? {
        let old = old?.path();
        let name = old.file_name().map(|n| n.to_string_lossy().into_owned());
        let keep = name
            .as_deref()
            .is_some_and(|n| kept.iter().any(|(f, _)| f == n));
        if old.is_file() && !keep {
            std::fs::remove_file(&old)?;
        }
    }
    for (file, path) in &kept {
        std::fs::copy(path, dir.join(file))?;
    }
    let manifest = Manifest { entries };
    std::fs::write(dir.join(testref::MANIFEST), manifest.render())?;
    std::fs::write(dir.join(testref::SOURCE), source(area, &tools))?;
    let mut say = std::io::stdout();
    use std::io::Write as _;
    writeln!(
        say,
        "{area}: {} references, {} files, {total} bytes",
        manifest.entries.len(),
        kept.len()
    )?;
    Ok(())
}

fn expand(arg: &str, src: &Path, out: &Path, work: &Path, defs: &[&Def]) -> String {
    let rel = |p: &Path| {
        p.strip_prefix(work)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_string_lossy().into_owned())
    };
    let mut s = arg
        .replace("{src}", &rel(src))
        .replace("{out}", &rel(out))
        .replace("{dir}", ".");
    while let Some(at) = s.find("{ref:") {
        let Some(end) = s[at..].find('}') else {
            break;
        };
        let name = &s[at + 5..at + end];
        let file = defs
            .iter()
            .find(|d| d.name == name)
            .map(|d| {
                if d.file.is_empty() {
                    d.name.clone()
                } else {
                    d.file.clone()
                }
            })
            .unwrap_or_else(|| name.to_string());
        s.replace_range(at..at + end + 1, &file);
    }
    s
}

fn tracked_bytes(skip: &str) -> Result<u64, Box<dyn Error>> {
    let mut total = 0;
    let Ok(areas) = std::fs::read_dir(testref::refs_dir()) else {
        return Ok(0);
    };
    for area in areas {
        let area = area?.path();
        if !area.is_dir() || area.file_name().is_some_and(|n| n == skip) {
            continue;
        }
        for f in std::fs::read_dir(&area)? {
            let f = f?.path();
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned());
            if f.is_file() && !matches!(name.as_deref(), Some(testref::MANIFEST | testref::SOURCE))
            {
                total += std::fs::metadata(&f)?.len();
            }
        }
    }
    Ok(total)
}

fn source(area: &str, tools: &BTreeSet<String>) -> String {
    let mut text = String::from(
        "These files are part of this repository and under its MIT licence (see LICENSE at the root).\n\n\
         Their inputs are synthetic: src/testref.rs and examples/record_refs draw every picture and generate every sound \
         with plain arithmetic, so no recorded picture or sound is in them. ",
    );
    text.push_str(&format!(
        "examples/record_refs made them; run `{}` to make them again. manifest.toml gives, for every file and \
         reference, its sha256, the tool and version that made it and the exact command.\n\nTools:\n",
        testref::rerecord(area)
    ));
    for t in tools {
        text.push_str(&format!("- {t}\n"));
    }
    text
}
