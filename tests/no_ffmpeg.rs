#[path = "../src/testref.rs"]
mod testref;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const ALLOWED: [&str; 2] = ["examples/bench.rs", "examples/bench/"];

const NEVER: [&str; 5] = [
    "examples/record_refs.rs",
    "examples/record_refs/",
    "bin/",
    "render/",
    "fuzz/",
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

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn git(root: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    c.arg("-C")
        .arg(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    c
}

fn git_files(root: &Path) -> Option<Vec<String>> {
    if !root.join(".git").exists() {
        return None;
    }
    let out = git(root).args(["ls-files", "-z"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        out.stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).replace('\\', "/"))
            .collect(),
    )
}

fn files_in(root: &Path, dirs: &[&str]) -> Vec<String> {
    let inside = |r: &str| {
        dirs.iter()
            .any(|d| r.strip_prefix(d).is_some_and(|t| t.starts_with('/')))
    };
    match git_files(root) {
        Some(all) => all.into_iter().filter(|r| inside(r)).collect(),
        None => {
            let mut files = Vec::new();
            for d in dirs {
                walk(&root.join(d), &mut files);
            }
            files.iter().map(|f| rel(root, f)).collect()
        }
    }
}

fn under(rel: &str, list: &[&str]) -> bool {
    list.iter().any(|a| {
        if a.ends_with('/') {
            rel.starts_with(a)
        } else {
            rel == *a
        }
    })
}

fn allowed(rel: &str) -> bool {
    under(rel, &ALLOWED)
}

fn never(rel: &str) -> bool {
    under(rel, &NEVER)
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

fn names_tool(line: &str) -> bool {
    let tools = tools();
    line.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| tools.iter().any(|t| t.eq_ignore_ascii_case(w)))
}

pub fn names_in_text(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| names_tool(l))
        .map(|(i, _)| i + 1)
        .collect()
}

pub fn names_in_manifest(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            l.split_once('=')
                .is_some_and(|(k, v)| matches!(k.trim(), "tool" | "command") && names_tool(v))
        })
        .map(|(i, _)| i + 1)
        .collect()
}

fn offenders_in(root: &Path) -> Vec<String> {
    let files = files_in(
        root,
        &[
            "src", "tests", "examples", "benches", "fuzz", "bin", "render",
        ],
    );
    let mut found = Vec::new();
    for r in files {
        if allowed(&r) || r.starts_with("tests/refs/") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&r)) else {
            continue;
        };
        let lines = if never(&r) {
            names_in_text(&text)
        } else if r.ends_with(".rs") {
            spawns_in_rust(&text)
        } else {
            continue;
        };
        found.extend(lines.into_iter().map(|l| format!("{r}:{l}")));
    }
    let mut manifests: Vec<String> = files_in(root, &["tests/refs"])
        .into_iter()
        .filter(|r| {
            r.strip_prefix("tests/refs/")
                .and_then(|t| t.split_once('/'))
                .is_some_and(|(_, n)| n == testref::MANIFEST)
        })
        .collect();
    manifests.sort();
    for r in manifests {
        let Ok(text) = std::fs::read_to_string(root.join(&r)) else {
            continue;
        };
        found.extend(
            names_in_manifest(&text)
                .into_iter()
                .map(|l| format!("{r}:{l}")),
        );
    }
    found
}

#[test]
fn no_test_gate_recorder_or_manifest_runs_ffmpeg_or_ffprobe() {
    let found = offenders_in(&testref::root());
    assert!(
        found.is_empty(),
        "{} places still name ffmpeg or ffprobe; record a reference with a tool pinned in tests/refs/TOOLS.toml instead:\n{}",
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
fn the_recorder_bin_render_and_fuzz_may_not_even_name_it() {
    let [ff, probe] = tools();
    let upper = ff.to_uppercase();
    let text = format!(
        "pub const {upper}: &str = \"x\";\nfn ok() {{}}\nlet a = {ff}_args();\n# {probe} once\nlet b = x264(\"--crf\");\n"
    );
    assert_eq!(names_in_text(&text), vec![1, 3, 4]);
    assert!(names_in_text("x264 --crf 0\nff_owned(args)\nffmpegx\n").is_empty());
}

#[test]
fn a_manifest_may_not_name_it_as_tool_or_command() {
    let [ff, probe] = tools();
    let text = format!(
        "[[ref]]\nname = \"{ff}.yuv\"\nfile = \"\"\ntool = \"{ff} version n9\"\ncommand = \"x264 {{src}}\"\n\n[[ref]]\nname = \"b\"\ntool = \"x265 4.3\"\ncommand = \"{probe} -i {{src}}\"\n"
    );
    assert_eq!(names_in_manifest(&text), vec![4, 10]);
    let clean = "[[ref]]\nname = \"a\"\ntool = \"x264 0.165.3222 b35605a\"\ncommand = \"x264 --crf 0 -o {out} {src}\"\n";
    assert!(names_in_manifest(clean).is_empty());
}

#[test]
fn only_the_bench_may_run_it_and_the_recorder_never() {
    assert!(allowed("examples/bench.rs"));
    assert!(allowed("examples/bench/docs.rs"));
    assert!(!allowed("examples/bench_extra.rs"));
    assert!(!allowed("examples/record_refs.rs"));
    assert!(!allowed("examples/record_refs/gif.rs"));
    assert!(!allowed("tests/media.rs"));
    assert!(!allowed("src/gif/tests.rs"));
    assert!(never("examples/record_refs.rs"));
    assert!(never("examples/record_refs/containers/pcod.rs"));
    assert!(never("bin/gate"));
    assert!(never("render/terminal/clip.sh"));
    assert!(never("fuzz/seeds"));
    assert!(!never("examples/bench.rs"));
    assert!(!never("src/gif/tests.rs"));
}

fn tree(name: &str, files: &[(&str, String)]) -> PathBuf {
    let root = testref::scratch("no_ffmpeg-self").join(name);
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in files {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    root
}

#[test]
fn each_rule_fires_on_a_tree_with_file_and_line() {
    let [ff, probe] = tools();
    let root = tree(
        "dirty",
        &[
            ("src/a.rs", format!("fn a() {{}}\nlet x = \"{ff}\";\n")),
            ("examples/record_refs.rs", format!("pub fn {ff}() {{}}\n")),
            (
                "examples/record_refs/video.rs",
                format!("fn v() {{}}\nconst P: &str = \"{probe}\";\n"),
            ),
            ("bin/gate", format!("#!/bin/sh\n# uses {ff}\n")),
            ("render/terminal/clip.sh", format!("{ff} -i a b\n")),
            ("fuzz/seeds", format!("set -e\n\n{probe} x\n")),
            (
                "tests/refs/h264/manifest.toml",
                format!("[[ref]]\nname = \"a\"\ntool = \"{ff} version\"\n"),
            ),
            ("examples/bench.rs", format!("let f = \"{ff}\";\n")),
            ("examples/bench/docs.rs", format!("let f = \"{probe}\";\n")),
        ],
    );
    let found: BTreeSet<String> = offenders_in(&root).into_iter().collect();
    let want: BTreeSet<String> = [
        "src/a.rs:2",
        "examples/record_refs.rs:1",
        "examples/record_refs/video.rs:2",
        "bin/gate:2",
        "render/terminal/clip.sh:1",
        "fuzz/seeds:3",
        "tests/refs/h264/manifest.toml:3",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(found, want);
}

#[test]
fn a_clean_tree_passes() {
    let root = tree(
        "clean",
        &[
            ("src/a.rs", "let x = \"x264\";\n".to_string()),
            (
                "examples/record_refs.rs",
                "pub fn tool(name: &str) {}\n".to_string(),
            ),
            ("bin/gate", "#!/bin/sh\ncargo fmt\n".to_string()),
            ("render/terminal/clip.sh", "pcod clip a b\n".to_string()),
            ("fuzz/seeds", "pcod synth x\n".to_string()),
            (
                "tests/refs/h264/manifest.toml",
                "[[ref]]\nname = \"a\"\ntool = \"x264 0.165\"\ncommand = \"x264 {src}\"\n".into(),
            ),
            (
                "tests/refs/TOOLS.toml",
                "[x264]\nversion = \"x264 --version\"\nline = \"x264 0.165\"\n".into(),
            ),
        ],
    );
    assert_eq!(offenders_in(&root), Vec::<String>::new());
}

fn tracked() -> Vec<(String, PathBuf, u64)> {
    let root = testref::root();
    let mut out = Vec::new();
    for r in files_in(&root, &["tests/refs"]) {
        let t = r.strip_prefix("tests/refs/").unwrap_or(&r);
        let Some((area, _)) = t.split_once('/') else {
            assert!(
                t == testref::TOOLS,
                "tests/refs/{t}: only area folders and {} sit in tests/refs",
                testref::TOOLS
            );
            continue;
        };
        let f = root.join(&r);
        let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
        out.push((area.to_string(), f, size));
    }
    out
}

#[test]
fn only_git_tracked_files_are_scanned() {
    let [ff, probe] = tools();
    let root = tree(
        "git",
        &[
            ("fuzz/seeds", format!("set -e\n{ff} x\n")),
            ("fuzz/corpus/local", format!("{probe} y\n")),
            ("src/a.rs", "fn a() {}\n".to_string()),
        ],
    );
    assert!(git_files(&root).is_none());
    let disk: BTreeSet<String> = offenders_in(&root).into_iter().collect();
    let both: BTreeSet<String> = ["fuzz/seeds:2", "fuzz/corpus/local:1"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(disk, both);
    assert!(git(&root).args(["init", "-q"]).status().unwrap().success());
    assert!(
        git(&root)
            .args(["add", "--", "fuzz/seeds", "src/a.rs"])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        git_files(&root),
        Some(vec!["fuzz/seeds".to_string(), "src/a.rs".to_string()])
    );
    assert_eq!(offenders_in(&root), vec!["fuzz/seeds:2".to_string()]);
}

#[test]
fn tracked_references_stay_inside_the_size_budget() {
    let root = testref::root();
    let files = tracked();
    let total: u64 = files.iter().map(|(_, _, s)| s).sum();
    for (_, f, size) in &files {
        assert!(
            *size <= testref::FILE_BUDGET,
            "{} is {size} bytes, over the {} byte budget for one file",
            rel(&root, f),
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
    let root = testref::root();
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
                rel(&root, f)
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

#[test]
fn every_manifest_tool_line_is_pinned_or_ours() {
    let tools = testref::Tools::load().unwrap();
    for area in tracked()
        .into_iter()
        .map(|(a, _, _)| a)
        .collect::<BTreeSet<_>>()
    {
        let m = testref::Manifest::load(&area).unwrap();
        for e in &m.entries {
            let pinned = tools.pins.iter().any(|p| p.line == e.tool);
            assert!(
                pinned || e.tool.starts_with("pito-codecs "),
                "tests/refs/{area}: {} was made by '{}', which tests/refs/{} does not pin",
                e.name,
                e.tool,
                testref::TOOLS
            );
        }
    }
}
