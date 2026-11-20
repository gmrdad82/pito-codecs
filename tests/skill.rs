#![cfg(feature = "cli")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FILES: [&str; 5] = ["SKILL.md", "convert.md", "audio.md", "stills.md", "gpu.md"];

fn skill_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/pcod")
}

fn read(name: &str) -> String {
    fs::read_to_string(skill_dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn pcod(dir: Option<&Path>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pcod"));
    command.env("NO_COLOR", "1").args(args);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn help(args: &[&str]) -> String {
    let out = pcod(None, args);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    text(&out.stdout)
}

fn commands(top: &str) -> BTreeSet<String> {
    let after = top.split("Commands:\n").nth(1).expect("a Commands: list");
    after
        .lines()
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .filter(|name| *name != "help")
        .map(str::to_string)
        .collect()
}

fn fenced(file: &str, body: &str) -> (Vec<String>, String) {
    let mut lines = Vec::new();
    let mut prose = String::new();
    let mut inside = false;
    for line in body.lines() {
        if let Some(info) = line.trim_start().strip_prefix("```") {
            if !inside {
                assert_eq!(info, "sh", "{file}: every fence is a sh block");
            }
            inside = !inside;
        } else if inside {
            if !line.trim().is_empty() {
                lines.push(line.trim().to_string());
            }
        } else {
            prose.push_str(line);
            prose.push('\n');
        }
    }
    assert!(!inside, "{file}: a fence is left open");
    (lines, prose)
}

fn spans(prose: &str) -> Vec<String> {
    prose
        .lines()
        .flat_map(|line| {
            line.split('`')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn flags_in(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'-';
    let mut found = BTreeSet::new();
    let mut at = 0;
    while at + 2 < bytes.len() {
        let starts = bytes[at] == b'-'
            && bytes[at + 1] == b'-'
            && bytes[at + 2].is_ascii_lowercase()
            && (at == 0 || !word(bytes[at - 1]));
        if starts {
            let end = bytes[at + 2..]
                .iter()
                .position(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-'))
                .map_or(bytes.len(), |n| at + 2 + n);
            found.insert(text[at..end].trim_end_matches('-').to_string());
            at = end;
        } else {
            at += 1;
        }
    }
    found
}

fn names(help: &str, flag: &str) -> bool {
    help.match_indices(flag).any(|(at, _)| {
        help.as_bytes()
            .get(at + flag.len())
            .is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'-'))
    })
}

struct Use {
    command: Option<String>,
    flags: BTreeSet<String>,
}

fn parse_use(words: &str, known: &BTreeSet<String>) -> Result<Option<Use>, String> {
    let mut tokens = words.split_whitespace();
    if tokens.next() != Some("pcod") {
        return Ok(None);
    }
    let command = match tokens.next() {
        None => None,
        Some(word) if word.starts_with(['-', '<']) => None,
        Some(word) if known.contains(word) => Some(word.to_string()),
        Some(word) if word == "help" || word.contains(['.', '/']) => None,
        Some(word) => return Err(format!("`{words}` names the unknown command {word}")),
    };
    Ok(Some(Use {
        command,
        flags: flags_in(words),
    }))
}

#[test]
fn the_skill_folder_holds_the_files_these_tests_read() {
    let mut held: Vec<String> = fs::read_dir(skill_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    held.sort();
    let mut wanted: Vec<String> = FILES.iter().map(|f| f.to_string()).collect();
    wanted.sort();
    assert_eq!(held, wanted, "a file in skills/pcod needs a place in FILES");

    let skill = read("SKILL.md");
    let front = skill
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .expect("front matter")
        .0;
    assert!(front.starts_with("name: pcod\n"), "{front}");
    assert!(front.contains("description:"), "{front}");

    for file in FILES {
        let body = read(file);
        for (at, _) in body.match_indices("](") {
            let target = body[at + 2..].split(')').next().unwrap();
            if !target.starts_with("http") {
                assert!(
                    skill_dir().join(target).is_file(),
                    "{file} links to {target}, which is not in the folder"
                );
            }
        }
    }
}

#[test]
fn every_command_and_flag_the_skill_names_is_in_pcods_help() {
    let top = help(&["--help"]);
    let known = commands(&top);
    let mut helps: BTreeMap<String, String> = BTreeMap::new();
    for command in &known {
        helps.insert(command.clone(), help(&[command, "--help"]));
    }
    let all: String = helps.values().chain([&top]).cloned().collect();

    let mut problems = Vec::new();
    let mut spoken = BTreeSet::new();
    for file in FILES {
        let body = read(file);
        let (lines, prose) = fenced(file, &body);
        for flag in flags_in(&body) {
            if !names(&all, &flag) {
                problems.push(format!("{file}: {flag} is in no command's help"));
            }
        }
        for words in lines.iter().chain(spans(&prose).iter()) {
            match parse_use(words, &known) {
                Err(why) => problems.push(format!("{file}: {why}")),
                Ok(None) => {}
                Ok(Some(used)) => {
                    let own = match &used.command {
                        Some(command) => &helps[command],
                        None => &top,
                    };
                    if let Some(command) = &used.command {
                        spoken.insert(command.clone());
                    }
                    for flag in &used.flags {
                        if !names(own, flag) {
                            problems.push(format!(
                                "{file}: `{words}` gives {} {flag}, which its help does not list",
                                used.command.as_deref().unwrap_or("pcod")
                            ));
                        }
                    }
                }
            }
        }
    }
    for command in &known {
        if !spoken.contains(command) {
            problems.push(format!("no `pcod {command}` in the skill"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[cfg(unix)]
mod run {
    use super::*;

    fn work() -> PathBuf {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/skill-examples");
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn sh(dir: &Path, line: &str) -> Output {
        let bin = Path::new(env!("CARGO_BIN_EXE_pcod")).parent().unwrap();
        let mut path = vec![bin.to_path_buf()];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        Command::new("sh")
            .arg("-c")
            .arg(line)
            .current_dir(dir)
            .env("PATH", std::env::join_paths(path).unwrap())
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    }

    const REFUSED: [(&[&str], i32, &str); 15] = [
        (
            &["clip.mp4", "site.mp4"],
            1,
            "already exists; use --force to replace it",
        ),
        (
            &["clip.mp4", "clip.mp4", "--force"],
            1,
            "input and output are the same file",
        ),
        (
            &["clip.mp4", "bad.mp4", "--profile", "fast", "--crf", "20"],
            2,
            "use --qp with fast",
        ),
        (
            &["clip.mp4", "bad.mp4", "--size", "360p"],
            2,
            "720p, 1080p, 1440p, 2160p",
        ),
        (
            &["clip.mp4", "bad.mp4", "--from", "3", "--to", "2"],
            1,
            "must come after --from",
        ),
        (
            &["clip.mp4", "bad.mp4", "--from", "9"],
            1,
            "at or past the end",
        ),
        (
            &[
                "clip.mp4", "bad.mp4", "--from", "1", "--to", "3", "--tune", "film",
            ],
            1,
            "only --tune animation does",
        ),
        (
            &["clip.mp4", "bad.webm", "--add-audio", "bed.wav"],
            1,
            "--add-audio writes .mp4 or .mov",
        ),
        (
            &["clip.mp4", "bad.wav", "--size", "320"],
            2,
            "doesn't apply to .wav",
        ),
        (
            &["clip.mp4", "bad.s16", "--track", "0"],
            1,
            "no audio stream 0",
        ),
        (
            &["loudness", "clip.mp4", "--track", "3"],
            1,
            "no audio track",
        ),
        (
            &["stills", "clip.mp4", "--count", "3"],
            2,
            "required arguments",
        ),
        (
            &["stills", "clip.mp4", "--at", "70", "--out", "late"],
            1,
            "past the end of the video",
        ),
        (
            &[
                "stills", "clip.mp4", "--from", "1", "--to", "1.2", "--out", "frames",
            ],
            1,
            "already holds numbered frames",
        ),
        (
            &["frames", "gap", "gap.mp4", "--fps", "30"],
            1,
            "the frames jump from",
        ),
    ];

    #[test]
    fn every_example_runs_as_written_and_every_refusal_it_names_holds() {
        let dir = work();
        for file in FILES {
            let (lines, _) = fenced(file, &read(file));
            for line in lines {
                assert!(
                    line.starts_with("pcod "),
                    "{file}: {line} is not a pcod line"
                );
                let out = sh(&dir, &line);
                assert_eq!(
                    out.status.code(),
                    Some(0),
                    "{file}: {line}\n{}",
                    text(&out.stderr)
                );
            }
        }

        fs::create_dir_all(dir.join("gap")).unwrap();
        for (from, to) in [
            ("000000", "000000"),
            ("000001", "000001"),
            ("000002", "000004"),
        ] {
            fs::copy(
                dir.join(format!("frames/{from}.png")),
                dir.join(format!("gap/{to}.png")),
            )
            .unwrap();
        }
        for (args, code, words) in REFUSED {
            let out = pcod(Some(&dir), args);
            let stderr = text(&out.stderr);
            let shown = args.join(" ");
            assert_eq!(out.status.code(), Some(code), "{shown}: {stderr}");
            let start = if code == 2 { "error:" } else { "pcod: " };
            assert!(stderr.starts_with(start), "{shown}: {stderr}");
            assert!(stderr.contains(words), "{shown}: {words:?} not in {stderr}");
            if code == 1 {
                assert_eq!(stderr.lines().count(), 1, "{shown}: {stderr}");
            }
            assert!(out.stdout.is_empty(), "{shown}");
        }
    }
}
