use std::fs;
use std::path::{Path, PathBuf};

pub const FILES: [(&str, &str); 5] = [
    ("SKILL.md", include_str!("../../../skills/pcod/SKILL.md")),
    (
        "convert.md",
        include_str!("../../../skills/pcod/convert.md"),
    ),
    ("audio.md", include_str!("../../../skills/pcod/audio.md")),
    ("stills.md", include_str!("../../../skills/pcod/stills.md")),
    ("gpu.md", include_str!("../../../skills/pcod/gpu.md")),
];

const HOMES: [(&str, &str, Option<&str>); 4] = [
    ("Claude skill", ".claude/skills/pcod", None),
    ("Agents skill", ".agents/skills/pcod", None),
    ("Codex skill", ".codex/skills/pcod", Some(".codex")),
    ("Pi skill", ".pi/agent/skills/pcod", Some(".pi")),
];

const OLD_COPY: &str = ".local/opt/pcod/skills";

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, PartialEq, Eq)]
pub enum State {
    Matched,
    Stale,
    Missing,
}

pub enum Action {
    Install,
    Status,
    Uninstall,
}

pub struct Row {
    label: &'static str,
    state: State,
}

pub fn home() -> Result<PathBuf, String> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "no home folder: set HOME".to_string())
}

fn matched(dir: &Path) -> bool {
    if dir.is_symlink() {
        return false;
    }
    let mut have: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    have.sort();
    let mut want: Vec<&str> = FILES.iter().map(|(name, _)| *name).collect();
    want.sort();
    have == want
        && FILES
            .iter()
            .all(|(name, text)| fs::read_to_string(dir.join(name)).is_ok_and(|have| have == *text))
}

fn state(dir: &Path) -> State {
    if matched(dir) {
        State::Matched
    } else if dir.is_symlink() || dir.exists() {
        State::Stale
    } else {
        State::Missing
    }
}

fn clear(path: &Path) -> Result<(), String> {
    let gone = if path.is_symlink() || path.is_file() {
        fs::remove_file(path)
    } else if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        Ok(())
    };
    gone.map_err(|e| format!("{}: {e}", path.display()))
}

fn write(dir: &Path) -> Result<(), String> {
    clear(dir)?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, text) in FILES {
        fs::write(dir.join(name), text)
            .map_err(|e| format!("{}: {e}", dir.join(name).display()))?;
    }
    Ok(())
}

fn wanted(home: &Path, only_present: bool) -> impl Iterator<Item = (&'static str, PathBuf)> + '_ {
    HOMES
        .iter()
        .filter(move |(_, rel, tool)| {
            let dir = home.join(rel);
            let present = tool.is_none_or(|t| home.join(t).is_dir());
            present || (only_present && (dir.is_symlink() || dir.exists()))
        })
        .map(move |(label, rel, _)| (*label, home.join(rel)))
}

pub fn run(action: &Action) -> Result<String, String> {
    rows_in(&home()?, action).map(|rows| lines(&rows))
}

pub fn rows_in(home: &Path, action: &Action) -> Result<Vec<Row>, String> {
    match action {
        Action::Install => {
            for (_, dir) in wanted(home, false) {
                write(&dir)?;
            }
            clear(&home.join(OLD_COPY))?;
        }
        Action::Uninstall => {
            for (_, dir) in wanted(home, true) {
                clear(&dir)?;
            }
            clear(&home.join(OLD_COPY))?;
        }
        Action::Status => {}
    }
    Ok(wanted(home, true)
        .map(|(label, dir)| Row {
            label,
            state: state(&dir),
        })
        .collect())
}

pub fn lines(rows: &[Row]) -> String {
    let mut out = String::new();
    for row in rows {
        let label = row.label;
        match row.state {
            State::Matched => {
                out.push_str(&format!("{label}: installed, matches pcod {VERSION}\n"))
            }
            State::Stale => out.push_str(&format!(
                "{label}: installed, does not match pcod {VERSION}; pcod skill install refreshes it\n"
            )),
            State::Missing => out.push_str(&format!("{label}: not installed\n")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pcod-skill-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_skill_has_text_and_front_matter() {
        assert!(FILES[0].1.starts_with("---\nname: pcod\n"));
        assert!(FILES.iter().all(|(_, text)| !text.is_empty()));
    }

    #[test]
    fn status_reports_missing_then_install_matches_in_both_homes() {
        let dir = fake("install");
        let rows = rows_in(&dir, &Action::Status).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.state == State::Missing));
        assert!(lines(&rows).contains("Claude skill: not installed\n"));
        let rows = rows_in(&dir, &Action::Install).unwrap();
        assert!(rows.iter().all(|r| r.state == State::Matched));
        for (name, text) in FILES {
            for home in [".claude/skills/pcod", ".agents/skills/pcod"] {
                assert_eq!(fs::read_to_string(dir.join(home).join(name)).unwrap(), text);
            }
        }
        assert!(lines(&rows).contains(&format!(
            "Agents skill: installed, matches pcod {VERSION}\n"
        )));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn codex_and_pi_get_it_only_when_their_folders_exist() {
        let dir = fake("tools");
        fs::create_dir_all(dir.join(".codex")).unwrap();
        let rows = rows_in(&dir, &Action::Install).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(dir.join(".codex/skills/pcod/SKILL.md").is_file());
        assert!(!dir.join(".pi").exists());
        fs::create_dir_all(dir.join(".pi")).unwrap();
        assert_eq!(rows_in(&dir, &Action::Install).unwrap().len(), 4);
        assert!(dir.join(".pi/agent/skills/pcod/SKILL.md").is_file());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_changed_or_extra_file_reads_stale_and_install_refreshes_it() {
        let dir = fake("stale");
        rows_in(&dir, &Action::Install).unwrap();
        let skill = dir.join(".claude/skills/pcod");
        fs::write(skill.join("SKILL.md"), "old").unwrap();
        fs::write(skill.join("extra.md"), "x").unwrap();
        let rows = rows_in(&dir, &Action::Status).unwrap();
        assert_eq!(rows[0].state, State::Stale);
        assert_eq!(rows[1].state, State::Matched);
        assert!(lines(&rows).contains("does not match pcod"));
        let rows = rows_in(&dir, &Action::Install).unwrap();
        assert!(rows.iter().all(|r| r.state == State::Matched));
        assert!(!skill.join("extra.md").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn an_old_link_is_replaced_by_real_files_and_the_old_copy_goes() {
        let dir = fake("link");
        let copy = dir.join(OLD_COPY).join("pcod");
        fs::create_dir_all(&copy).unwrap();
        fs::write(copy.join("SKILL.md"), "old").unwrap();
        fs::create_dir_all(dir.join(".claude/skills")).unwrap();
        let link = dir.join(".claude/skills/pcod");
        std::os::unix::fs::symlink(&copy, &link).unwrap();
        assert_eq!(
            rows_in(&dir, &Action::Status).unwrap()[0].state,
            State::Stale
        );
        rows_in(&dir, &Action::Install).unwrap();
        assert!(!link.is_symlink());
        assert!(link.is_dir());
        assert_eq!(
            fs::read_to_string(link.join("SKILL.md")).unwrap(),
            FILES[0].1
        );
        assert!(!dir.join(OLD_COPY).exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn uninstall_removes_the_folders_and_status_then_lists_nothing_installed() {
        let dir = fake("uninstall");
        rows_in(&dir, &Action::Install).unwrap();
        let rows = rows_in(&dir, &Action::Uninstall).unwrap();
        assert!(rows.iter().all(|r| r.state == State::Missing));
        assert!(!dir.join(".claude/skills/pcod").exists());
        assert!(dir.join(".claude/skills").is_dir());
        let _ = fs::remove_dir_all(dir);
    }
}
