#[path = "../src/testref.rs"]
mod testref;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const ALLOWED: [&str; 4] = [
    "examples/record_refs.rs",
    "examples/record_refs/",
    "examples/bench.rs",
    "examples/bench/",
];

fn tools() -> [String; 2] {
    [["ff", "mpeg"].concat(), ["ff", "probe"].concat()]
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !matches!(name.as_str(), "target" | "tmp" | ".git" | ".dev") {
                walk(&p, out);
            }
        } else {
            out.push(p);
        }
    }
}

fn rel(p: &Path) -> String {
    p.strip_prefix(testref::root())
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn allowed(rel: &str) -> bool {
    ALLOWED.iter().any(|a| {
        if a.ends_with('/') {
            rel.starts_with(a)
        } else {
            rel == *a
        }
    })
}

pub fn spawns_in_rust(text: &str) -> Vec<usize> {
    let mut lines = BTreeSet::new();
    for tool in tools() {
        let quoted = format!("\"{tool}\"");
        for (at, _) in text.match_indices(&quoted) {
            let before = text[..at].trim_end();
            let asked = ["contains(", "starts_with(", "ends_with(", "find(", "eq("]
                .iter()
                .any(|f| before.ends_with(f));
            if !asked {
                lines.insert(text[..at].matches('\n').count() + 1);
            }
        }
    }
    lines.into_iter().collect()
}

pub fn spawns_in_shell(text: &str) -> Vec<usize> {
    let tools = tools();
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with('#'))
        .filter(|(_, l)| {
            l.split(|c: char| c.is_whitespace() || "|;&()$`\"'".contains(c))
                .any(|w| tools.iter().any(|t| t == w))
        })
        .map(|(i, _)| i + 1)
        .collect()
}

fn offenders() -> Vec<String> {
    let root = testref::root();
    let mut files = Vec::new();
    for d in ["src", "tests", "examples", "benches", "fuzz"] {
        walk(&root.join(d), &mut files);
    }
    let mut scripts = Vec::new();
    walk(&root.join("bin"), &mut scripts);
    let mut found = Vec::new();
    for f in files {
        let r = rel(&f);
        if allowed(&r) || f.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        found.extend(
            spawns_in_rust(&text)
                .into_iter()
                .map(|l| format!("{r}:{l}")),
        );
    }
    for f in scripts {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        found.extend(
            spawns_in_shell(&text)
                .into_iter()
                .map(|l| format!("{}:{l}", rel(&f))),
        );
    }
    found
}

#[test]
fn no_test_gate_or_pcod_path_runs_ffmpeg_or_ffprobe() {
    let found = offenders();
    assert!(
        found.is_empty(),
        "{} places still name ffmpeg or ffprobe as a program; record a reference with examples/record_refs instead:\n{}",
        found.len(),
        found.join("\n")
    );
}

#[test]
fn the_scan_catches_every_way_of_naming_the_tool() {
    let [ff, probe] = tools();
    let spawn = format!("let o = Command::new(\"{ff}\")\n    .arg(\"-version\");\n");
    assert_eq!(spawns_in_rust(&spawn), vec![1]);
    let named = format!("const BIN: &str = \"{probe}\";\nfn x() {{}}\nlet t = \"{ff}\";");
    assert_eq!(spawns_in_rust(&named), vec![1, 3]);
    let message = format!("assert!(!e.to_lowercase().contains(\"{ff}\"), \"{{e}}\");");
    assert!(spawns_in_rust(&message).is_empty());
    let prose = format!("let s = \"{ff} decodes it\";");
    assert!(spawns_in_rust(&prose).is_empty());
    let script =
        format!("#!/bin/sh\n# {ff} is not used\n{ff} -i a b\nx=$({probe} -v 0 a)\necho done\n");
    assert_eq!(spawns_in_shell(&script), vec![3, 4]);
    assert!(spawns_in_shell(&format!("echo {ff}mpeg-free")).is_empty());
}

#[test]
fn only_the_recorder_and_the_bench_may_run_it() {
    assert!(allowed("examples/record_refs.rs"));
    assert!(allowed("examples/record_refs/gif.rs"));
    assert!(allowed("examples/bench/docs.rs"));
    assert!(!allowed("examples/bench_extra.rs"));
    assert!(!allowed("tests/media.rs"));
    assert!(!allowed("src/gif/tests.rs"));
}

fn tracked() -> Vec<(String, PathBuf, u64)> {
    let mut out = Vec::new();
    let Ok(areas) = std::fs::read_dir(testref::refs_dir()) else {
        return out;
    };
    for area in areas.flatten() {
        let path = area.path();
        let name = area.file_name().to_string_lossy().into_owned();
        assert!(
            path.is_dir(),
            "tests/refs/{name}: only area folders sit in tests/refs"
        );
        let mut files = Vec::new();
        walk(&path, &mut files);
        for f in files {
            let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
            out.push((name.clone(), f, size));
        }
    }
    out
}

#[test]
fn tracked_references_stay_inside_the_size_budget() {
    let files = tracked();
    let total: u64 = files.iter().map(|(_, _, s)| s).sum();
    for (_, f, size) in &files {
        assert!(
            *size <= testref::FILE_BUDGET,
            "{} is {size} bytes, over the {} byte budget for one file",
            rel(f),
            testref::FILE_BUDGET
        );
    }
    assert!(
        total <= testref::TOTAL_BUDGET,
        "tests/refs holds {total} bytes, over its {} byte budget",
        testref::TOTAL_BUDGET
    );
}

#[test]
fn every_tracked_file_is_in_its_manifest_with_its_sha256() {
    let files = tracked();
    let areas: BTreeSet<&str> = files.iter().map(|(a, _, _)| a.as_str()).collect();
    for area in areas {
        let dir = testref::area_dir(area);
        assert!(
            dir.join(testref::SOURCE).is_file(),
            "tests/refs/{area} has no SOURCE"
        );
        let m = testref::Manifest::load(area).unwrap();
        let listed: BTreeSet<&str> = m
            .entries
            .iter()
            .filter(|e| !e.file.is_empty())
            .map(|e| e.file.as_str())
            .collect();
        for (_, f, _) in files.iter().filter(|(a, _, _)| a == area) {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            if name == testref::MANIFEST || name == testref::SOURCE {
                continue;
            }
            assert!(
                listed.contains(name.as_str()),
                "{} is not in its manifest",
                rel(f)
            );
        }
        for e in m.entries.iter().filter(|e| !e.file.is_empty()) {
            let bytes = std::fs::read(dir.join(&e.file))
                .unwrap_or_else(|err| panic!("tests/refs/{area}/{}: {err}", e.file));
            assert_eq!(
                testref::sha256(&bytes),
                e.sha256,
                "tests/refs/{area}/{}",
                e.file
            );
            assert!(
                !e.tool.is_empty() && !e.command.is_empty(),
                "{area}/{}",
                e.name
            );
        }
    }
}
