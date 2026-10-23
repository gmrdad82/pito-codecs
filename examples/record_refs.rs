use std::collections::BTreeSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

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

use testref::{Entry, Kind, Manifest};

pub struct Def {
    pub area: &'static str,
    pub name: String,
    pub file: String,
    pub kind: Kind,
    pub make: Option<Make>,
    pub tool: &'static str,
    pub args: Vec<String>,
    pub take: Take,
}

pub type Make = Box<dyn Fn(&Path) -> Result<(), String>>;

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

    pub fn from(mut self, make: impl Fn() -> Result<Vec<u8>, String> + 'static) -> Def {
        self.make = Some(Box::new(move |at| testref::write_atomic(at, &make()?)));
        self
    }
}

pub fn sha(bytes: &[u8]) -> Result<String, String> {
    Ok(testref::sha256(bytes))
}

pub const FFMPEG: &str = "ffmpeg";
pub const OURS: &str = "";

pub fn ff(args: &[&str]) -> Vec<String> {
    ["-hide_banner", "-nostats", "-loglevel", "error", "-y"]
        .iter()
        .chain(args)
        .map(|s| s.to_string())
        .collect()
}

pub fn words(text: &str) -> Vec<String> {
    vec![text.to_string()]
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
            format!("pito-codecs {}", env!("CARGO_PKG_VERSION"))
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
            version(d.tool)?
        };
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
        let command = if d.tool.is_empty() {
            format!("examples/record_refs: {}", d.args.join(" "))
        } else {
            format!("{} {}", d.tool, d.args.join(" "))
        };
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

fn version(tool: &str) -> Result<String, Box<dyn Error>> {
    for flag in ["-version", "--version"] {
        if let Ok(o) = Command::new(tool).arg(flag).output()
            && o.status.success()
        {
            let text = String::from_utf8_lossy(&o.stdout);
            if let Some(line) = text.lines().find(|l| !l.trim().is_empty()) {
                return Ok(line.trim().to_string());
            }
        }
    }
    Err(format!("{tool} gives no version line").into())
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
