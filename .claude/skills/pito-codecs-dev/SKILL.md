---
name: pito-codecs-dev
description: "Develop pito-codecs itself: its Rust codecs, containers, filters and GPU routes, the pcod command line's source, bin/gate and its quick check, the tests, references, fuzz targets, bench, docs and releases. Triggers: pito-codecs, pito_codecs, bin/gate, bin/smoke, bin/cov, bin/docs-bundle, gate --fast, nextest, tests/refs, record_refs, TOOLS.toml, no_ffmpeg, tests/skill.rs, src/gpu, vaapi, nvenc, videotoolbox, media_foundation, src/media, src/bin/pcod, fuzz/, the bench example, NOTICE, CHANGELOG, a codec feature in Cargo.toml, a dispatch/ branch here. Excludes using pcod to convert, cut, probe or take stills (that is the pcod skill, shipped from skills/pcod)."
---

# pito-codecs-dev

pito-codecs is one Cargo package (MIT, edition 2024, Rust 1.97 or newer): pure-Rust video, audio and image codecs, containers, filters and GPU routes, a media front for apps, and `pcod`, its command line. Speed is the goal and bit-exact decode is the floor. This skill is for the sessions that change the repo; using pcod is `skills/pcod`.

AGENTS.md and CLAUDE.md hold the rules and prompts/RESUME.md what runs now and what is next. They are git-ignored, so a fresh worktree lacks them: read them from the repository's main checkout first. This page adds the how-to and does not repeat them.

## Layout

- `src/<codec>/`: one folder per codec or container (h264, hevc, vp9, av1, aac, ac3, opus, vorbis, mp3, flac, alac, mp4, mkv, gif, webp, png, jpeg), each behind its own feature in `Cargo.toml`; `default` turns on all, `cli` builds pcod.
- `src/media/`: shared types; `src/media/front/` is the media front apps call (probe, demux, mux, video, audio, still, export, join, gif).
- `src/gpu/{vaapi,nvenc,media_foundation,videotoolbox}`: one backend per system, behind `cfg(target_os)`. `src/bin/pcod.rs` and `src/bin/pcod/` are the CLI.
- `tests/`: integration tests, one file per area. `tests/refs/` holds reference outputs and their `manifest.toml` (sha256); `examples/record_refs.rs` records them with the tools pinned in `tests/refs/TOOLS.toml`. `tests/fixtures/` holds tracked media.
- `examples/`: `bench` (reads `bench/ffmpeg-reference.toml`), `media_speed`, `png_speed`, `jpeg_speed`, `readme_shots`, `record_refs`.
- `fuzz/`: its own workspace, one target per entry point; `fuzz/seeds` writes a target's corpus.
- `skills/pcod/`: the product skill and its install scripts. `docs/site/`: the guides each release's docs bundle carries; `docs/charts/` and `docs/images/` feed the README. `render/`: the recipes for the README's terminal clips.
- `README.md`, `CHANGELOG.md`, `NOTICE` (every ported source's notice), `INSPIRATION.md` (origins), `FILMCRAFT.from` (the pinned FilmCraft commit).

## How work goes

- `main` is the trunk. Each piece of work gets its own branch (a plan's pieces are `dispatch/<id>-<slug>`) in its own git worktree, never in the main checkout. Commit signed and staged by name, then merge into `main` locally with a merge commit named `Merge <branch>: <what it does>`. Push only when asked.
- Work another repo needs is not written from here: it goes to that repo as a plan.
- One next version: everything planned joins it, and nothing obsolete is built or released. Quick-check and commit as you go.

## Critical rules

- **Quick check only while working.** Format, lint, and run the tests of what changed (`bin/gate --fast`). No full suite, GPU or heavy tests, re-recorded references or bench unless asked.
- **No ffmpeg or ffprobe.** `tests/no_ffmpeg.rs` fails when a test, example or source spawns either, or when `bin/`, `render/`, `fuzz/` or `examples/record_refs*` names one. Only `bench --record-ffmpeg` runs ffmpeg, by hand. References come from the tools in `tests/refs/TOOLS.toml`.
- **Never panic, always one line.** Library code does not panic on bad input or a failing driver, and `Drop` never panics. Examples, benches and binaries return a `Result` through `examples/support/fail.rs` or pcod's own main: one line `<bin>: <what failed and why>`, exit 1.
- **VA-API refuses pictures below 352x240** (`large_enough` in `src/gpu/vaapi/mod.rs`): smaller ones faulted the GPU. GPU tests stay at 352x240 or larger, run headless with no window, and skip cleanly with no device. A route that fails falls back or reports.
- **A CLI change updates the skill.** `skills/pcod/*.md` change in the same commit as any pcod command, flag, default or refusal; `tests/skill.rs` fails on drift and lists the skill's files in `FILES`.
- **Ports credit their source.** Permissive sources only, noted in `NOTICE` and `INSPIRATION.md`; FFmpeg, x264, x265 and libde265 are read, never copied.
- **The repo is public.** No private repo, machine path, person or plan in a tracked file; scratch lives in `tmp/` (git-ignored); test media comes from the tests or tracked files with a clear licence.

## Command discovery

| Command | What it does |
| --- | --- |
| `bin/gate --fast` | the quick check: `cargo fmt`, clippy with warnings denied (all features, feature subsets, Windows, macOS and Linux ARM when their std is installed), then the tests of what changed against `main` |
| `bin/gate` | the same with every test, then `cargo build --release` |
| `cargo nextest run --cargo-profile ci --all-targets -E 'binary(<test file>)'` | one integration test file, e.g. `binary(skill)`; `ci` is release without LTO or strip |
| `cargo build --release --locked --bin pcod` | build pcod into `target/release/pcod` |
| `pcod --help`, `pcod <command> --help` | the CLI's source of truth; bare `pcod` prints one line per command |
| `bin/smoke [pcod]` | end to end on a built pcod: synth, probe, decode on both routes, encode, one-line failures |
| `bin/cov [--json]` | coverage with cargo-llvm-cov and nextest, into `tmp/cov.txt` or `tmp/cov.json` |
| `bin/docs-bundle vX.Y.Z [out dir]` | the release docs: `docs/site`, a command reference from pcod's help, `cargo doc`, the changelog |
| `cargo run --release --example bench -- --help` | the bench: `--quick`, `--only ID`, `--check <results.json>`, `--chart-docs <results.json> docs/charts` |
| `cargo run --release --example record_refs -- <area>` | re-record one area's references; a wrong area names the valid ones |
| `cargo run --release --example media_speed -- <file> [mode]` | speed of one file; a wrong call prints its usage (as does `png_speed`) |
| `cargo fuzz list`, `fuzz/seeds <target>`, `cargo fuzz run <target>` | the fuzz targets, a target's seed corpus, a run |

Each tool's `--help` (or the usage it prints on a wrong call) is the source of truth.

## Releases

A release is `v<Cargo.toml version>`: a `CHANGELOG.md` section (`## vX.Y.Z — date`), the README's `--tag vX.Y.Z` pins matching the version (`tests/readme.rs`), `bin/docs-bundle`, a signed tag, and a GitHub Release carrying pcod for Linux x86-64, Windows x86-64 and macOS arm64 (signed and notarized on a Mac), the docs bundle and `SHA256SUMS`. The release script is not in git. A session cuts a release only when asked.

## Known pitfalls

- `bin/gate` uses the first `rustc` on `PATH`. A toolchain without the Windows, macOS or ARM std skips those clippy runs with a `skipping` line, so put the rustup toolchain first.
- A change under `src/`, `examples/`, `tests/fixtures/`, `tests/refs/`, `Cargo.toml`, `Cargo.lock` or `bin/gate` makes `--fast` run every test: iterate with the single-file nextest line, then run the gate once. Nothing testable changed says `none run`; `GATE_BASE=<rev>` changes the base.
- `bin/smoke` needs a built pcod and `tests/refs/pcod/smoke-hevc.mp4` matching its sha256 in `tests/refs/pcod/manifest.toml`.
- `record_refs` refuses an area when this machine's tool prints another version than `TOOLS.toml` pins. Never hand-edit a reference or a manifest.
- `docs/charts/*.json` come from `bench --chart-docs`; a contended machine reads 10 to 50% slow on CPU rows, so no regression claim or README number comes from one.
- A variant the code cannot read (10-bit H.264, 4:2:2, interlaced H.264) is refused by name, never decoded wrong or silently; the `pcod` tests hold the list.

## Decision framework

1. **A codec, container or filter in `src/`** → edit it with its tests beside it or in `tests/<area>.rs`, compare against `tests/refs`, iterate with the single-file nextest line, then `bin/gate --fast`.
2. **A new format or feature** → its own feature in `Cargo.toml` (in `default`, and in `cli` when pcod uses it), its tests and references, its `NOTICE` and `INSPIRATION.md` entries, a `CHANGELOG.md` line.
3. **A pcod command, flag or message** → `src/bin/pcod/`, then `skills/pcod/`, `docs/site/` and the README where they name it, then `binary(skill)` and the `pcod` tests.
4. **A GPU route** → `src/gpu/<backend>`, headless, the VA-API size guard kept, a fallback or a report on failure; the GPU suite runs only when asked.
5. **README, docs or a `bin/` script** → `bin/gate --fast` runs `readme` for the README and `no_ffmpeg` for `bin/`; `docs/site` has no test of its own.
6. **A speed claim** → time it on a quiet machine against a baseline from the same run, then `bench`; never from a contended run.
7. **A release** → only when asked: the Releases section.
