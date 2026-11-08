---
title: "Install"
description: "Install pcod on Linux, macOS or Windows, build it from source, or add the library to a Rust project."
sidebar:
  order: 2
---

pcod, the command line over the library, comes with every [GitHub Release](https://github.com/gmrdad82/pito-codecs/releases) for Linux x86-64 and Windows x86-64. macOS installs from source. `SHA256SUMS` in each Release lists every file's checksum.

## Linux x86-64

```sh
v=@VERSION@; curl -fsSLO "https://github.com/gmrdad82/pito-codecs/releases/download/$v/pcod-$v-linux-x86_64.tar.gz"
tar -xzf "pcod-$v-linux-x86_64.tar.gz" pcod && install -Dm755 pcod ~/.local/bin/pcod
```

## macOS on Apple Silicon

Build from source with Rust stable. The macOS build is untested.

```sh
cargo install --locked --git https://github.com/gmrdad82/pito-codecs --tag @VERSION@ pito-codecs --bin pcod
```

## Windows x86-64

In PowerShell:

```powershell
$v="@VERSION@"; Invoke-WebRequest "https://github.com/gmrdad82/pito-codecs/releases/download/$v/pcod-$v-windows-x86_64.zip" -OutFile pcod.zip
Expand-Archive pcod.zip "$env:LOCALAPPDATA\pcod" -Force; [Environment]::SetEnvironmentVariable("Path", [Environment]::GetEnvironmentVariable("Path", "User") + ";$env:LOCALAPPDATA\pcod", "User")
```

On Windows, H.265 decode through Media Foundation needs Microsoft's HEVC Video Extensions, and N editions need the Media Feature Pack.

## From source

Anywhere Rust 1.97 or newer runs, Linux arm64 included:

```sh
cargo install --locked --git https://github.com/gmrdad82/pito-codecs --tag @VERSION@ pito-codecs --bin pcod
```

## The agent skill

Each archive carries `skills/pcod/`, a skill that teaches coding agents to use pcod, and a script that installs it. It goes into these folders:

| Agent | Folder |
| --- | --- |
| Claude Code | `~/.claude/skills/pcod` |
| Codex | `~/.codex/skills/pcod`, only when `~/.codex` exists |
| Pi | `~/.pi/agent/skills/pcod`, only when `~/.pi` exists |
| Other agents | `~/.agents/skills/pcod` |

On Linux, from the folder you unpacked into, `skills/install-skill.sh` copies the skill to `~/.local/opt/pcod/skills/pcod` and links it into each folder. `skills/install-skill.sh uninstall` removes the links and the copy.

On Windows, in PowerShell from the unpacked folder, `skills\install-skill.ps1` copies the skill into the same folders under `%USERPROFILE%`; run it again after each update to refresh the copies, and `skills\install-skill.ps1 uninstall` removes them.

A `pcod` folder there that this install did not place is left alone, with one line saying so. From a source checkout, `bin/install` does the same on every run, and `bin/install --uninstall` removes it.

## The library

Add it to a Rust project's `Cargo.toml`. Every part is on by default:

```toml
[dependencies]
pito-codecs = { git = "https://github.com/gmrdad82/pito-codecs", tag = "@VERSION@" }
```

Each part is a feature: `media`, `h264`, `hevc`, `vp9`, `aac`, `ac3`, `mp3`, `flac`, `opus`, `mp4`, `mkv`, `gpu`, `filter`, `gif`, `webp`, and `cli` for pcod alone. The `media` front (`probe`, `first_frame`, `export` and the rest) needs `h264`, `hevc`, `vp9`, `aac`, `mp4`, `mkv`, `gpu` and `filter`; a library user who wants nothing else turns `cli` off so clap is never built:

```toml
[dependencies]
pito-codecs = { git = "https://github.com/gmrdad82/pito-codecs", tag = "@VERSION@", default-features = false, features = ["h264", "hevc", "vp9", "aac", "mp4", "mkv", "gpu", "filter", "opus", "ac3", "mp3", "flac"] }
```

The GPU route needs nothing extra: libva is loaded at run time on Linux (Mesa's or Intel's driver), VideoToolbox is part of macOS and Media Foundation part of Windows. Without them everything decodes and encodes on the CPU.

## Quick start

On any MP4:

```sh
pcod probe clip.mp4
pcod decode clip.mp4 --route gpu
pcod encode clip.mp4 out.mp4 --codec hevc --route gpu
```

From Rust, let the front pick the fastest route:

```rust
use pito_codecs::media::{Prefs, first_frame, probe};

let prefs = Prefs::default();
let info = probe("clip.mp4".as_ref(), &prefs)?;
let still = first_frame("clip.mp4".as_ref(), 240, &prefs)?;
println!("{}x{} at {}/{} fps, first frame {}x{}", info.width, info.height, info.fps.0, info.fps.1, still.width, still.height);
```

Next: [probe, decode and encode with pcod](../guides/pcod/), or [use the library](../guides/library/).
