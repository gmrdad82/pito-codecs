#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use pito_codecs::media::Rgba;

const FRAMES: u32 = 48;
const WIDTH: u32 = 384;
const HEIGHT: u32 = 240;
const BLOCK: u32 = WIDTH / 8;
const CROP: &str = "96:48:144:96";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/pcod-crophold")
}

fn dir(name: &str) -> PathBuf {
    let path = root().join(name);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pcod"))
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ok(out: &Output) -> String {
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    text(&out.stdout)
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn refused(out: &Output, words: &str) {
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("pcod: "), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains(words), "{words:?} not in {stderr}");
    assert!(out.stdout.is_empty());
}

fn misused(out: &Output, words: &str) {
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(stderr.contains(words), "{words:?} not in {stderr}");
}

fn numbered(n: u32) -> Rgba {
    let mut img = Rgba::new(WIDTH, HEIGHT);
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % WIDTH, i as u32 / WIDTH);
        let bit = x / BLOCK;
        let v = if n >> bit & 1 == 1 { 255 } else { 0 };
        let shade = ((x * 3 + y * 5) % 64) as u8;
        px.copy_from_slice(&[v.max(shade), shade, v / 2, 255]);
    }
    img
}

fn number_of(img: &Rgba) -> u32 {
    let y = img.height / 2;
    let block = img.width / 8;
    (0..8).fold(0, |n, bit| {
        let x = bit * block + block / 2;
        let at = (y * img.width + x) as usize * 4;
        if img.data[at] > 128 { n | 1 << bit } else { n }
    })
}

fn rect(img: &Rgba, (w, h, x, y): (u32, u32, u32, u32)) -> Vec<u8> {
    let mut out = Vec::new();
    for row in y..y + h {
        let at = (row * img.width + x) as usize * 4;
        out.extend_from_slice(&img.data[at..at + w as usize * 4]);
    }
    out
}

const RECT: (u32, u32, u32, u32) = (96, 48, 144, 96);

fn decoded(path: &Path) -> Rgba {
    pito_codecs::png::decode(&fs::read(path).unwrap()).unwrap()
}

fn png_folder(name: &str, count: u32) -> PathBuf {
    let folder = dir(name);
    for n in 0..count {
        let png = pito_codecs::png::encode(&numbered(n)).unwrap();
        fs::write(folder.join(format!("{n:06}.png")), png).unwrap();
    }
    folder
}

fn source() -> &'static Path {
    static MADE: OnceLock<PathBuf> = OnceLock::new();
    MADE.get_or_init(|| {
        let root = root();
        fs::create_dir_all(&root).unwrap();
        let lock = fs::File::create(root.join("source.lock")).unwrap();
        lock.lock().unwrap();
        let video = root.join("two.mp4");
        if video.exists() {
            return video;
        }
        let thread = format!("{:?}", std::thread::current().id());
        let thread: String = thread.chars().filter(char::is_ascii_digit).collect();
        let pid = format!("{}-{thread}", std::process::id());
        let frames = png_folder(&format!("frames.{pid}"), FRAMES);
        let part = root.join(format!("two.{pid}.mp4"));
        ok(&run(&[
            "frames",
            s(&frames),
            s(&part),
            "--fps",
            "24",
            "--route",
            "cpu",
            "--crf",
            "12",
        ]));
        fs::rename(&part, &video).unwrap();
        let _ = fs::remove_dir_all(&frames);
        video
    })
}

fn listed(dir: &Path) -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    names.sort();
    names
}

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &json[at..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim_matches('"'))
}

#[test]
fn video_crops_are_the_same_rectangle_of_the_full_frames() {
    let work = dir("video-crop");
    let (full, cut) = (work.join("full"), work.join("cut"));
    let window = ["--from", "0.5", "--to", "1", "--route", "cpu"];
    let mut args = vec!["stills", s(source()), "--size", "384", "--out", s(&full)];
    args.extend_from_slice(&window);
    ok(&run(&args));
    let mut args = vec![
        "stills",
        s(source()),
        "--crop",
        CROP,
        "--json",
        "--out",
        s(&cut),
    ];
    args.extend_from_slice(&window);
    let json = ok(&run(&args));
    assert_eq!(field(&json, "width"), Some("96"), "{json}");
    assert_eq!(field(&json, "height"), Some("48"), "{json}");
    assert!(
        json.contains("\"crop\":{\"width\":96,\"height\":48,\"x\":144,\"y\":96}"),
        "{json}"
    );
    let (whole, parts) = (listed(&full), listed(&cut));
    assert_eq!(whole.len(), 12);
    assert_eq!(parts.len(), 12);
    for (i, (w, p)) in whole.iter().zip(&parts).enumerate() {
        assert_eq!(w.file_name(), p.file_name());
        let full_frame = decoded(w);
        assert_eq!(number_of(&full_frame), 12 + i as u32);
        let piece = decoded(p);
        assert_eq!((piece.width, piece.height), (96, 48));
        assert_eq!(piece.data, rect(&full_frame, RECT), "frame {i}");
    }
}

#[test]
fn a_two_second_hold_at_30_fps_adds_60_copies_of_the_last_frame() {
    let work = dir("video-hold");
    let out = work.join("frames");
    let json = ok(&run(&[
        "stills",
        s(source()),
        "--fps",
        "30",
        "--to",
        "1",
        "--hold",
        "2s",
        "--crop",
        CROP,
        "--route",
        "cpu",
        "--json",
        "--out",
        s(&out),
    ]));
    assert_eq!(field(&json, "count"), Some("90"), "{json}");
    assert_eq!(field(&json, "expected"), Some("30"), "{json}");
    assert_eq!(field(&json, "held"), Some("60"), "{json}");
    let files = listed(&out);
    assert_eq!(files.len(), 90);
    for (k, f) in files.iter().enumerate() {
        assert_eq!(
            f.file_name().unwrap().to_str().unwrap(),
            format!("{k:06}.png")
        );
    }
    let last = fs::read(&files[29]).unwrap();
    for f in &files[30..] {
        assert_eq!(fs::read(f).unwrap(), last, "{}", f.display());
    }
}

#[test]
fn a_hold_reads_in_the_plain_report() {
    let work = dir("video-hold-plain");
    let out = work.join("frames");
    let plain = ok(&run(&[
        "stills",
        s(source()),
        "--fps",
        "10",
        "--from",
        "1",
        "--to",
        "1.5",
        "--hold",
        "0.3",
        "--route",
        "cpu",
        "--out",
        s(&out),
    ]));
    assert!(plain.starts_with("8 png frames"), "{plain}");
    assert!(
        plain.contains("the last frame held for 3 more frames"),
        "{plain}"
    );
    assert_eq!(listed(&out).len(), 8);
}

#[test]
fn a_png_folder_from_a_time_with_a_crop_and_a_hold() {
    let folder = png_folder("folder-src", 24);
    let work = dir("folder-out");
    let out = work.join("frames");
    let json = ok(&run(&[
        "stills",
        s(&folder),
        "--fps",
        "12",
        "--from",
        "1",
        "--to",
        "1.5",
        "--crop",
        CROP,
        "--hold",
        "0.5",
        "--json",
        "--out",
        s(&out),
    ]));
    assert_eq!(field(&json, "count"), Some("12"), "{json}");
    assert_eq!(field(&json, "held"), Some("6"), "{json}");
    assert_eq!(field(&json, "rate"), Some("12/1"), "{json}");
    let files = listed(&out);
    assert_eq!(files.len(), 12);
    for (k, f) in files.iter().enumerate() {
        let n = 12 + k.min(5) as u32;
        let got = decoded(f);
        assert_eq!(got.data, rect(&numbered(n), RECT), "{}", f.display());
    }
    assert_eq!(listed(&folder).len(), 24);
}

#[test]
fn a_png_folder_from_a_time_keeps_every_frame_at_full_size() {
    let folder = png_folder("folder-whole-src", 24);
    let out = dir("folder-whole-out").join("frames");
    ok(&run(&[
        "stills",
        s(&folder),
        "--fps",
        "12",
        "--from",
        "1",
        "--out",
        s(&out),
    ]));
    let got: Vec<u32> = listed(&out)
        .iter()
        .map(|f| number_of(&decoded(f)))
        .collect();
    assert_eq!(got, (12..24).collect::<Vec<u32>>());
    let first = decoded(&listed(&out)[0]);
    assert_eq!(first.data, numbered(12).data);
}

#[test]
fn a_png_folder_is_refused_without_a_rate_past_its_end_or_as_its_own_output() {
    let folder = png_folder("folder-bad", 4);
    let out = root().join("folder-bad-out");
    let _ = fs::remove_dir_all(&out);
    refused(
        &run(&["stills", s(&folder), "--out", s(&out)]),
        "give --fps for their rate",
    );
    refused(
        &run(&[
            "stills",
            s(&folder),
            "--fps",
            "12",
            "--from",
            "1",
            "--out",
            s(&out),
        ]),
        "at or past the end of the frames",
    );
    assert!(!out.exists());
    refused(
        &run(&[
            "stills",
            s(&folder),
            "--fps",
            "12",
            "--size",
            "100",
            "--out",
            s(&out),
        ]),
        "written at their own size",
    );
    refused(
        &run(&[
            "stills",
            s(&folder),
            "--fps",
            "12",
            "--force",
            "--out",
            s(&folder),
        ]),
        "is the source folder",
    );
    let windows: [(&[&str], &str); 3] = [
        (&["--from", "1"], "give --fps for their rate"),
        (
            &["--fps", "12", "--from", "2", "--to", "1"],
            "--to 1 must come after --from 2",
        ),
        (
            &["--fps", "10", "--from", "0.01", "--to", "0.05"],
            "no frame starts between 0.010 s and 0.050 s",
        ),
    ];
    for (flags, words) in windows {
        let mut args = vec!["stills", s(&folder)];
        args.extend_from_slice(flags);
        args.extend_from_slice(&["--out", s(&out)]);
        refused(&run(&args), words);
        assert!(!out.exists(), "{flags:?}");
    }
    assert_eq!(listed(&folder).len(), 4);
}

#[test]
fn bad_crops_on_frames_are_refused_and_leave_no_folder() {
    let out = root().join("bad-crop-out");
    for (crop, words) in [
        ("385:10:0:0", "falls outside the 384x240 frame"),
        ("10:10:380:0", "falls outside the 384x240 frame"),
        ("10:10:0:231", "falls outside the 384x240 frame"),
        ("0:10:0:0", "needs a width and height above zero"),
    ] {
        let _ = fs::remove_dir_all(&out);
        refused(
            &run(&[
                "stills",
                s(source()),
                "--from",
                "0",
                "--crop",
                crop,
                "--route",
                "cpu",
                "--out",
                s(&out),
            ]),
            words,
        );
        assert!(!out.exists(), "{crop}");
    }
    let folder = png_folder("bad-crop-folder", 2);
    refused(
        &run(&[
            "stills",
            s(&folder),
            "--fps",
            "12",
            "--crop",
            "10:10:375:0",
            "--out",
            s(&out),
        ]),
        "falls outside the 384x240 frame",
    );
    assert!(!out.exists());
}

#[test]
fn a_crop_or_hold_on_the_wrong_command_line_is_a_usage_error() {
    let out = root().join("usage-out");
    let v = s(source());
    let o = s(&out);
    misused(
        &run(&["stills", v, "--from", "1", "--hold", "2s", "--out", o]),
        "--fps",
    );
    misused(
        &run(&[
            "stills", v, "--from", "1", "--crop", CROP, "--height", "120", "--out", o,
        ]),
        "--height",
    );
    misused(
        &run(&[
            "stills", v, "--from", "1", "--crop", CROP, "--size", "720p", "--out", o,
        ]),
        "--size",
    );
    misused(&run(&["stills", v, "--crop", CROP, "--out", o]), "--from");
    misused(
        &run(&["stills", v, "--from", "1", "--crop", "96x48", "--out", o]),
        "is not a crop such as",
    );
    misused(
        &run(&["stills", v, "--fps", "30", "--hold", "0", "--out", o]),
        "is not an interval above zero",
    );
    assert!(!out.exists());
}

#[test]
fn one_png_crops_to_the_same_rectangle() {
    let work = dir("one-png");
    let input = work.join("in.png");
    fs::write(&input, pito_codecs::png::encode(&numbered(5)).unwrap()).unwrap();
    let output = work.join("out.png");
    let plain = ok(&run(&[s(&input), s(&output), "--crop", CROP]));
    assert!(plain.contains("96x48 at 144,96 of 384x240"), "{plain}");
    assert_eq!(decoded(&output).data, rect(&numbered(5), RECT));
    refused(
        &run(&[s(&input), s(&output), "--crop", CROP]),
        "already exists; use --force",
    );
    let json = ok(&run(&[
        "convert",
        s(&input),
        s(&output),
        "--crop",
        "1:1:383:239",
        "--force",
        "--json",
    ]));
    assert!(
        json.contains("\"crop\":{\"width\":1,\"height\":1,\"x\":383,\"y\":239}"),
        "{json}"
    );
    assert_eq!(decoded(&output).data, rect(&numbered(5), (1, 1, 383, 239)));
}

#[test]
fn one_png_keeps_its_alpha() {
    let work = dir("one-png-alpha");
    let input = work.join("in.png");
    let mut img = numbered(3);
    for px in img.data.chunks_exact_mut(4) {
        px[3] = px[1];
    }
    fs::write(&input, pito_codecs::png::encode_rgba(&img).unwrap()).unwrap();
    let output = work.join("out.png");
    ok(&run(&[s(&input), s(&output), "--crop", CROP]));
    assert_eq!(decoded(&output).data, rect(&img, RECT));
}

#[test]
fn one_png_bad_crops_and_flags_are_refused() {
    let work = dir("one-png-bad");
    let input = work.join("in.png");
    fs::write(&input, pito_codecs::png::encode(&numbered(1)).unwrap()).unwrap();
    let output = work.join("out.png");
    refused(
        &run(&[s(&input), s(&output), "--crop", "100:100:300:0"]),
        "falls outside the 384x240 frame",
    );
    refused(
        &run(&[s(&input), s(&output), "--crop", "0:1:0:0"]),
        "needs a width and height above zero",
    );
    assert!(!output.exists());
    refused(
        &run(&[s(&work.join("missing.png")), s(&output), "--crop", CROP]),
        "missing.png",
    );
    fs::write(work.join("junk.png"), b"not a png").unwrap();
    refused(
        &run(&[s(&work.join("junk.png")), s(&output), "--crop", CROP]),
        "junk.png",
    );
    assert!(!output.exists());
    misused(
        &run(&[s(&input), s(&output), "--crop", CROP, "--size", "100"]),
        "--size doesn't apply to a PNG crop",
    );
    misused(
        &run(&[s(&input), s(&work.join("out.mp4")), "--crop", CROP]),
        "cuts one PNG into a PNG",
    );
    misused(
        &run(&[s(source()), s(&output), "--crop", CROP]),
        "cuts one PNG into a PNG",
    );
    misused(
        &run(&["join", s(&input), s(&input), s(&output), "--crop", CROP]),
        "join takes clips",
    );
    assert!(!output.exists());
    let before = fs::read(&input).unwrap();
    refused(
        &run(&[s(&input), s(&input), "--crop", CROP, "--force"]),
        "input and output are the same file",
    );
    assert_eq!(fs::read(&input).unwrap(), before);
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in bytes {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = if c & 1 == 1 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

fn stored_zlib(raw: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let n = block.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for x in raw {
        a = (a + u32::from(*x)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());
    z
}

const PALETTE: [[u8; 4]; 4] = [
    [255, 0, 0, 255],
    [0, 255, 0, 128],
    [0, 0, 255, 0],
    [255, 255, 255, 255],
];

fn palette_index(x: u32, y: u32) -> u8 {
    ((x / 7 + y / 5) % 4) as u8
}

fn palette_png(w: u32, h: u32) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut head = Vec::new();
    head.extend_from_slice(&w.to_be_bytes());
    head.extend_from_slice(&h.to_be_bytes());
    head.extend_from_slice(&[8, 3, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &head);
    let colours: Vec<u8> = PALETTE.iter().flat_map(|p| p[..3].to_vec()).collect();
    chunk(&mut png, b"PLTE", &colours);
    let alphas: Vec<u8> = PALETTE.iter().map(|p| p[3]).collect();
    chunk(&mut png, b"tRNS", &alphas);
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0);
        raw.extend((0..w).map(|x| palette_index(x, y)));
    }
    chunk(&mut png, b"IDAT", &stored_zlib(&raw));
    chunk(&mut png, b"IEND", &[]);
    png
}

#[test]
fn one_palette_png_keeps_its_trns_transparency() {
    let work = dir("one-png-palette");
    let input = work.join("in.png");
    fs::write(&input, palette_png(WIDTH, HEIGHT)).unwrap();
    let full = decoded(&input);
    assert!(full.data.chunks_exact(4).any(|p| p[3] == 0));
    let output = work.join("out.png");
    ok(&run(&[s(&input), s(&output), "--crop", CROP]));
    let got = decoded(&output);
    assert_eq!((got.width, got.height), (96, 48));
    assert_eq!(got.data, rect(&full, RECT));
    let (w, _, x, y) = RECT;
    for (i, px) in got.data.chunks_exact(4).enumerate() {
        let (px_x, px_y) = (x + i as u32 % w, y + i as u32 / w);
        assert_eq!(px, PALETTE[palette_index(px_x, px_y) as usize], "{i}");
    }
    assert!(!work.join("out.png.part").exists());
}

#[test]
fn a_png_folder_of_mixed_sizes_is_refused_and_leaves_no_frames() {
    let folder = png_folder("mixed-src", 4);
    let small = pito_codecs::png::encode(&Rgba::new(WIDTH / 2, HEIGHT)).unwrap();
    fs::write(folder.join("000002.png"), small).unwrap();
    let out = dir("mixed-out").join("frames");
    for crop in [None, Some(CROP)] {
        let _ = fs::remove_dir_all(&out);
        let mut args = vec!["stills", s(&folder), "--fps", "12", "--out", s(&out)];
        if let Some(c) = crop {
            args.extend_from_slice(&["--crop", c]);
        }
        refused(
            &run(&args),
            "is 192x240 where the folder's first frame is 384x240",
        );
        assert!(
            !out.exists() || listed(&out).is_empty(),
            "{crop:?}: {:?}",
            listed(&out)
        );
    }
}

#[test]
fn a_crop_with_every_cuts_each_picked_frame() {
    let work = dir("video-crop-every");
    let (full, cut) = (work.join("full"), work.join("cut"));
    let window = ["--every", "0.5", "--route", "cpu"];
    let mut args = vec!["stills", s(source()), "--size", "384", "--out", s(&full)];
    args.extend_from_slice(&window);
    ok(&run(&args));
    let mut args = vec!["stills", s(source()), "--crop", CROP, "--out", s(&cut)];
    args.extend_from_slice(&window);
    ok(&run(&args));
    let (whole, parts) = (listed(&full), listed(&cut));
    assert_eq!(whole.len(), 4);
    assert_eq!(parts.len(), 4);
    for (w, p) in whole.iter().zip(&parts) {
        let frame = decoded(w);
        assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
        assert_eq!(decoded(p).data, rect(&frame, RECT), "{}", p.display());
    }
}

#[test]
fn a_whole_frame_crop_is_the_unscaled_frame() {
    let work = dir("video-crop-whole");
    let (full, cut) = (work.join("full"), work.join("cut"));
    let window = ["--from", "1", "--to", "1.25", "--route", "cpu"];
    let mut args = vec!["stills", s(source()), "--size", "384", "--out", s(&full)];
    args.extend_from_slice(&window);
    ok(&run(&args));
    let mut args = vec![
        "stills",
        s(source()),
        "--crop",
        "384:240:0:0",
        "--out",
        s(&cut),
    ];
    args.extend_from_slice(&window);
    ok(&run(&args));
    let (whole, parts) = (listed(&full), listed(&cut));
    assert_eq!(whole.len(), 6);
    assert_eq!(parts.len(), 6);
    for (w, p) in whole.iter().zip(&parts) {
        assert_eq!(
            fs::read(w).unwrap(),
            fs::read(p).unwrap(),
            "{}",
            p.display()
        );
    }
}

#[test]
fn a_hold_in_jpeg_copies_the_last_jpeg() {
    let out = dir("video-hold-jpeg").join("frames");
    let json = ok(&run(&[
        "stills",
        s(source()),
        "--fps",
        "10",
        "--to",
        "0.5",
        "--hold",
        "0.4",
        "--format",
        "jpeg",
        "--route",
        "cpu",
        "--json",
        "--out",
        s(&out),
    ]));
    assert_eq!(field(&json, "count"), Some("9"), "{json}");
    assert_eq!(field(&json, "held"), Some("4"), "{json}");
    let files = listed(&out);
    assert_eq!(files.len(), 9);
    for (k, f) in files.iter().enumerate() {
        assert_eq!(
            f.file_name().unwrap().to_str().unwrap(),
            format!("{k:06}.jpg")
        );
    }
    let last = fs::read(&files[4]).unwrap();
    assert!(last.starts_with(&[0xff, 0xd8]));
    for f in &files[5..] {
        assert_eq!(fs::read(f).unwrap(), last, "{}", f.display());
    }
}

#[test]
fn held_frames_count_toward_the_frame_limit() {
    let out = root().join("hold-limit-out");
    let _ = fs::remove_dir_all(&out);
    refused(
        &run(&[
            "stills",
            s(source()),
            "--fps",
            "30",
            "--to",
            "1",
            "--hold",
            "3333.3",
            "--route",
            "cpu",
            "--out",
            s(&out),
        ]),
        "100029 frames from 0.000 s to 1.000 s is more than 100000; use --force",
    );
    assert!(!out.exists());
    let folder = png_folder("hold-limit-folder", 2);
    refused(
        &run(&[
            "stills",
            s(&folder),
            "--fps",
            "30",
            "--hold",
            "3334",
            "--out",
            s(&out),
        ]),
        "is more than 100000; use --force",
    );
    assert!(!out.exists());
}

#[test]
fn a_png_folder_refuses_every_and_height() {
    let folder = png_folder("folder-every", 3);
    let out = root().join("folder-every-out");
    let _ = fs::remove_dir_all(&out);
    refused(
        &run(&["stills", s(&folder), "--every", "1", "--out", s(&out)]),
        "--every fits a video",
    );
    refused(
        &run(&[
            "stills",
            s(&folder),
            "--fps",
            "12",
            "--height",
            "100",
            "--out",
            s(&out),
        ]),
        "drop --size and --height",
    );
    assert!(!out.exists());
}

#[test]
fn a_png_folder_of_palette_frames_keeps_their_trns_transparency() {
    let folder = dir("folder-palette");
    for n in 0..3 {
        fs::write(
            folder.join(format!("{n:06}.png")),
            palette_png(WIDTH, HEIGHT),
        )
        .unwrap();
    }
    let full = decoded(&folder.join("000000.png"));
    let out = root().join("folder-palette-out");
    let _ = fs::remove_dir_all(&out);
    ok(&run(&[
        "stills",
        s(&folder),
        "--fps",
        "12",
        "--crop",
        CROP,
        "--out",
        s(&out),
    ]));
    let frames = listed(&out);
    assert_eq!(frames.len(), 3);
    for frame in frames {
        let got = decoded(&frame);
        assert_eq!(got.data, rect(&full, RECT), "{}", frame.display());
        assert!(got.data.chunks_exact(4).any(|p| p[3] == 0));
    }
}

#[test]
fn a_gif_refuses_a_crop_and_a_hold_and_leaves_no_folder() {
    let work = dir("gif-refused");
    let frames: Vec<Rgba> = (0..3).map(numbered).collect();
    let gif = pito_codecs::gif::encode(
        &frames,
        &[10, 10, 10],
        pito_codecs::gif::EncodeOptions::default(),
    )
    .unwrap();
    let input = work.join("in.gif");
    fs::write(&input, gif).unwrap();
    let out = work.join("frames");
    for flags in [&["--crop", CROP][..], &["--hold", "0.5"]] {
        let mut args = vec!["stills", s(&input), "--fps", "10", "--out", s(&out)];
        args.extend_from_slice(flags);
        refused(
            &run(&args),
            "is a GIF; --crop and --hold take a video or a folder of PNG frames",
        );
        assert!(!out.exists(), "{flags:?}");
    }
    ok(&run(&[
        "stills",
        s(&input),
        "--fps",
        "10",
        "--out",
        s(&out),
    ]));
    assert_eq!(listed(&out).len(), 3);
}
