use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;

use crate::media::{Codec, Direction, Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    pub codec: String,
    pub video: bool,
    pub hardware: bool,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ffmpeg {
    pub path: PathBuf,
    pub ffprobe: Option<PathBuf>,
    pub version: String,
    pub encoders: Vec<Listed>,
    pub decoders: Vec<Listed>,
    pub hwaccels: Vec<String>,
}

const HARDWARE: [&str; 11] = [
    "_vaapi",
    "_nvenc",
    "_cuvid",
    "_qsv",
    "_videotoolbox",
    "_mf",
    "_amf",
    "_v4l2m2m",
    "_vulkan",
    "_d3d12va",
    "_mediacodec",
];

fn cache() -> &'static Mutex<HashMap<PathBuf, Ffmpeg>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Ffmpeg>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

impl Ffmpeg {
    pub fn find(user: Option<&Path>) -> Result<Ffmpeg> {
        let path = match user {
            Some(p) if p.is_file() => p.to_path_buf(),
            Some(p) => {
                return Err(Error::Unsupported(format!("no ffmpeg at {}", p.display())));
            }
            None => on_path(exe("ffmpeg"))
                .ok_or_else(|| Error::Unsupported("no ffmpeg on PATH".to_string()))?,
        };
        if let Some(f) = cache()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&path)
        {
            return Ok(f.clone());
        }
        let f = Ffmpeg::load(path.clone())?;
        cache()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path, f.clone());
        Ok(f)
    }

    fn load(path: PathBuf) -> Result<Ffmpeg> {
        let ffprobe = path
            .parent()
            .map(|d| d.join(exe("ffprobe")))
            .filter(|p| p.is_file())
            .or_else(|| on_path(exe("ffprobe")));
        let mut f = Ffmpeg {
            path,
            ffprobe,
            version: String::new(),
            encoders: Vec::new(),
            decoders: Vec::new(),
            hwaccels: Vec::new(),
        };
        let (version, encoders, decoders, hwaccels) = std::thread::scope(|s| {
            let fr = &f;
            let e = s.spawn(move || fr.run(&["-encoders"]));
            let d = s.spawn(move || fr.run(&["-decoders"]));
            let h = s.spawn(move || fr.run(&["-hwaccels"]));
            let v = fr.run(&["-version"]);
            let join = |r: std::thread::Result<Result<Vec<u8>>>| {
                r.unwrap_or_else(|_| Err(Error::Backend("ffmpeg listing failed".to_string())))
            };
            (v, join(e.join()), join(d.join()), join(h.join()))
        });
        f.version = String::from_utf8_lossy(&version?)
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        f.encoders = listing(&String::from_utf8_lossy(&encoders?));
        f.decoders = listing(&String::from_utf8_lossy(&decoders?));
        f.hwaccels = String::from_utf8_lossy(&hwaccels?)
            .lines()
            .skip_while(|l| !l.starts_with("Hardware acceleration methods"))
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        Ok(f)
    }

    pub fn can(&self, codec: Codec, dir: Direction) -> bool {
        let list = match dir {
            Direction::Decode => &self.decoders,
            Direction::Encode => &self.encoders,
        };
        list.iter().any(|l| l.codec == codec_name(codec))
    }

    pub fn encoder_for(&self, codec: Codec, picked: Option<&str>) -> Option<String> {
        let wanted = codec_name(codec);
        let ours = |n: &str| {
            self.encoders
                .iter()
                .any(|l| l.name == n && l.codec == wanted)
        };
        if let Some(p) = picked.filter(|p| ours(p)) {
            return Some(p.to_string());
        }
        let order: &[&str] = match codec {
            Codec::H264 => &["libx264", "h264_vaapi", "h264_nvenc", "h264_videotoolbox"],
            Codec::Hevc => &["libx265", "hevc_vaapi", "hevc_nvenc", "hevc_videotoolbox"],
            Codec::Aac => &["aac", "libfdk_aac"],
            Codec::Vp9 => &["libvpx-vp9", "vp9_vaapi"],
            Codec::Opus => &["libopus", "opus"],
        };
        order
            .iter()
            .find(|n| ours(n))
            .map(|n| n.to_string())
            .or_else(|| {
                self.encoders
                    .iter()
                    .find(|l| l.codec == wanted)
                    .map(|l| l.name.clone())
            })
    }

    pub fn decoder_args(&self, picked: Option<&str>) -> Vec<String> {
        match picked {
            Some(p) if self.hwaccels.iter().any(|h| h == p) => {
                vec!["-hwaccel".into(), p.into()]
            }
            Some(p) if self.decoders.iter().any(|l| l.name == p) => {
                vec!["-c:v".into(), p.into()]
            }
            _ => Vec::new(),
        }
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.path);
        c.args(["-nostdin", "-hide_banner", "-v", "error"]);
        background(&mut c);
        c
    }

    pub fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let mut c = Command::new(&self.path);
        c.args(["-hide_banner"]).args(args);
        background(&mut c);
        output(c, "ffmpeg")
    }

    pub fn run_args(&self, args: &[String]) -> Result<Vec<u8>> {
        let mut c = self.command();
        c.arg("-y").args(args);
        output(c, "ffmpeg")
    }

    pub fn probe_flat(&self, args: &[&str], path: &Path) -> Result<BTreeMap<String, String>> {
        let probe = self.ffprobe.as_ref().ok_or_else(|| {
            Error::Unsupported(format!("no ffprobe next to {}", self.path.display()))
        })?;
        let mut c = Command::new(probe);
        c.args(["-v", "error", "-of", "flat"]).args(args).arg(path);
        background(&mut c);
        Ok(flat(&String::from_utf8_lossy(&output(c, "ffprobe")?)))
    }

    pub fn probe_csv(&self, args: &[&str], path: &Path) -> Result<String> {
        let probe = self.ffprobe.as_ref().ok_or_else(|| {
            Error::Unsupported(format!("no ffprobe next to {}", self.path.display()))
        })?;
        let mut c = Command::new(probe);
        c.args(["-v", "error", "-of", "csv=p=0"])
            .args(args)
            .arg(path);
        background(&mut c);
        Ok(String::from_utf8_lossy(&output(c, "ffprobe")?).into_owned())
    }

    pub fn spawn(&self, args: &[String], stdin: bool, stdout: bool) -> Result<Child> {
        let mut c = self.command();
        c.arg("-y")
            .args(args)
            .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(if stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped());
        let mut child = c
            .spawn()
            .map_err(|e| Error::Backend(format!("ffmpeg: {e}")))?;
        let err = child.stderr.take().map(|mut s| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                buf
            })
        });
        Ok(Child {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            child,
            err,
        })
    }
}

pub struct Child {
    child: std::process::Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    err: Option<JoinHandle<Vec<u8>>>,
}

impl Child {
    pub fn finish(mut self) -> Result<()> {
        self.stdin.take();
        self.stdout.take();
        let status = self.child.wait()?;
        let err = self
            .err
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        if status.success() {
            Ok(())
        } else {
            Err(Error::Backend(format!("ffmpeg: {}", last_line(&err))))
        }
    }

    pub fn stderr_now(&mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.err
            .take()
            .and_then(|h| h.join().ok())
            .and_then(|e| final_line(&e))
            .unwrap_or_default()
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Aac => "aac",
        Codec::Vp9 => "vp9",
        Codec::Opus => "opus",
    }
}

pub fn codec_of(name: &str) -> Option<Codec> {
    match name {
        "h264" => Some(Codec::H264),
        "hevc" => Some(Codec::Hevc),
        "aac" => Some(Codec::Aac),
        "vp9" => Some(Codec::Vp9),
        "opus" => Some(Codec::Opus),
        _ => None,
    }
}

fn output(mut c: Command, what: &str) -> Result<Vec<u8>> {
    let out = c
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::Backend(format!("{what}: {e}")))?;
    if !out.status.success() {
        return Err(Error::Backend(format!(
            "{what}: {}",
            last_line(&out.stderr)
        )));
    }
    Ok(out.stdout)
}

fn final_line(err: &[u8]) -> Option<String> {
    String::from_utf8_lossy(err)
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
}

fn last_line(err: &[u8]) -> String {
    final_line(err).unwrap_or_else(|| "failed with no message".into())
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn on_path(name: String) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
}

#[cfg(windows)]
fn background(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    c.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn background(_c: &mut Command) {}

pub fn listing(text: &str) -> Vec<Listed> {
    text.lines()
        .skip_while(|l| !l.trim_start().starts_with("------"))
        .skip(1)
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let flags = it.next()?;
            let name = it.next()?.to_string();
            let description = it.collect::<Vec<_>>().join(" ");
            let codec = description
                .rsplit_once("(codec ")
                .and_then(|(_, c)| c.strip_suffix(')'))
                .unwrap_or(&name)
                .to_string();
            Some(Listed {
                hardware: HARDWARE.iter().any(|s| name.ends_with(s)),
                video: flags.starts_with('V'),
                name,
                codec,
                description,
            })
        })
        .collect()
}

pub fn flat(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let v = v.trim();
            let v = match v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                Some(q) => unescape(q),
                None => v.to_string(),
            };
            Some((k.trim().to_string(), v))
        })
        .collect()
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings_name_the_codec_and_spot_hardware() {
        let text = " V....D = Video\n ------\n V....D libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)\n V....D h264_vaapi           H.264/AVC (VAAPI) (codec h264)\n A....D aac                  AAC (Advanced Audio Coding)\n";
        let l = listing(text);
        assert_eq!(l.len(), 3);
        assert_eq!((l[0].codec.as_str(), l[0].hardware), ("h264", false));
        assert_eq!((l[1].codec.as_str(), l[1].hardware), ("h264", true));
        assert_eq!((l[2].codec.as_str(), l[2].video), ("aac", false));
    }
}
