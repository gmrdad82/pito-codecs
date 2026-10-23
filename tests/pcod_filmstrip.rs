#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::media::Rgba;

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/pcod-filmstrip-test")
        .join(name);
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

fn ok(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

fn refused(out: &Output, says: &str) {
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.starts_with("pcod: ") && err.contains(says), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
}

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &json[at..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim_matches('"'))
}

fn list(json: &str, key: &str) -> Vec<f64> {
    let at = json.find(&format!("\"{key}\":[")).unwrap() + key.len() + 4;
    let rest = &json[at..];
    rest[..rest.find(']').unwrap()]
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap())
        .collect()
}

fn video(folder: &Path) -> PathBuf {
    video_of(folder, 20)
}

fn video_of(folder: &Path, count: u32) -> PathBuf {
    let frames = folder.join("frames");
    fs::create_dir_all(&frames).unwrap();
    for n in 0..count {
        let mut img = Rgba::new(352, 240);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % 352, i as u32 / 352);
            px.copy_from_slice(&[
                (x + n * 12) as u8,
                (y * 2) as u8,
                ((x ^ y) + n * 4) as u8,
                255,
            ]);
        }
        fs::write(
            frames.join(format!("{n:04}.png")),
            pito_codecs::png::encode(&img).unwrap(),
        )
        .unwrap();
    }
    let mp4 = folder.join("clip.mp4");
    ok(&run(&[
        "frames",
        frames.to_str().unwrap(),
        mp4.to_str().unwrap(),
        "--fps",
        "10",
        "--route",
        "cpu",
    ]));
    mp4
}

fn png(path: &Path) -> Rgba {
    pito_codecs::png::decode(&fs::read(path).unwrap()).unwrap()
}

fn pixel(img: &Rgba, x: u32, y: u32) -> [u8; 3] {
    let i = (y as usize * img.width as usize + x as usize) * 4;
    [img.data[i], img.data[i + 1], img.data[i + 2]]
}

#[test]
fn every_takes_a_tile_per_interval_and_reports_its_times() {
    let folder = dir("every");
    let clip = video(&folder);
    let strip = folder.join("strip.png");
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--every",
        "500ms",
        "--seek",
        "exact",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&out);
    let json = text(&out.stdout);
    assert_eq!(json.lines().count(), 1, "{json}");
    assert_eq!(field(&json, "tiles"), Some("4"), "{json}");
    assert_eq!(field(&json, "tile_height"), Some("90"), "{json}");
    let times = list(&json, "times");
    assert_eq!(times.len(), 4);
    for (t, want) in times.iter().zip([0.0, 0.5, 1.0, 1.5]) {
        assert!((t - want).abs() < 1e-6, "{json}");
    }
    let tw: u32 = field(&json, "tile_width").unwrap().parse().unwrap();
    assert!((130..=134).contains(&tw), "{json}");
    let img = png(&strip);
    assert_eq!(img.width, 4 * tw + 5 * 4);
    assert_eq!(img.height, 90 + 8);
    assert_eq!(field(&json, "width"), Some(img.width.to_string().as_str()));
}

#[test]
fn count_spaces_tiles_and_the_text_says_it_in_one_line() {
    let folder = dir("count");
    let clip = video(&folder);
    let strip = folder.join("strip.webp");
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--count",
        "3",
        "--route",
        "cpu",
    ]);
    ok(&out);
    let line = text(&out.stdout);
    assert_eq!(line.lines().count(), 1, "{line}");
    assert!(
        line.contains("3 tiles of") && line.contains(" at 0:00"),
        "{line}"
    );
    let info = pito_codecs::webp::info(&fs::read(&strip).unwrap()).unwrap();
    assert_eq!(info.height, 98);
    let json = text(
        &run(&[
            "filmstrip",
            clip.to_str().unwrap(),
            folder.join("exact.png").to_str().unwrap(),
            "--count",
            "4",
            "--seek",
            "exact",
            "--from",
            "0.4",
            "--to",
            "1.2",
            "--route",
            "cpu",
            "--json",
        ])
        .stdout,
    );
    let times = list(&json, "times");
    for (t, want) in times.iter().zip([0.5, 0.7, 0.9, 1.1]) {
        assert!((t - want).abs() < 1e-6, "{json}");
    }
    assert_eq!(times.len(), 4);
}

#[test]
fn wrap_timecodes_and_the_gap_colour_shape_the_grid() {
    let folder = dir("wrap");
    let clip = video(&folder);
    let strip = folder.join("grid.png");
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--count",
        "5",
        "--wrap",
        "2",
        "--gap",
        "6",
        "--background",
        "#ff0000",
        "--timecodes",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&out);
    let json = text(&out.stdout);
    assert_eq!(field(&json, "cols"), Some("2"));
    assert_eq!(field(&json, "rows"), Some("3"));
    let tw: u32 = field(&json, "tile_width").unwrap().parse().unwrap();
    let img = png(&strip);
    assert_eq!((img.width - 3 * 6) % 2, 0);
    let cell = (img.width - 3 * 6) / 2;
    assert!(cell >= tw, "{json}");
    let label = (img.height - 4 * 6) / 3 - 90;
    assert!(label > 0, "{json}");
    assert_eq!(pixel(&img, 0, 0), [255, 0, 0]);
    assert_eq!(pixel(&img, 6 + cell + 2, 50), [255, 0, 0]);
    assert_eq!(pixel(&img, img.width - 1, img.height - 1), [255, 0, 0]);
    let row = 6 + 90;
    let inked = (row..row + label)
        .flat_map(|y| (6..6 + tw).map(move |x| (x, y)))
        .filter(|&(x, y)| pixel(&img, x, y) != [255, 0, 0])
        .count();
    assert!(inked > 0, "no timecode drawn under the first tile");
    let narrow = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        folder.join("narrow.png").to_str().unwrap(),
        "--count",
        "4",
        "--max-width",
        "300",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&narrow);
    let json = text(&narrow.stdout);
    assert_eq!(field(&json, "cols"), Some("2"), "{json}");
    assert!(field(&json, "width").unwrap().parse::<u32>().unwrap() <= 300);
}

#[test]
fn a_video_shorter_than_one_interval_gives_one_tile() {
    let folder = dir("short");
    let clip = video(&folder);
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        folder.join("one.png").to_str().unwrap(),
        "--every",
        "10",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&out);
    assert_eq!(field(&text(&out.stdout), "tiles"), Some("1"));
}

#[test]
fn bad_cadences_sizes_and_outputs_are_refused_in_one_line() {
    let folder = dir("refused");
    let clip = video(&folder);
    let c = clip.to_str().unwrap();
    let strip = folder.join("strip.png");
    let s = strip.to_str().unwrap();
    for bad in ["0", "0ms", "-2s", "soon"] {
        let out = run(&["filmstrip", c, s, "--every", bad]);
        assert_eq!(out.status.code(), Some(2), "{bad}: {}", text(&out.stderr));
        let err = text(&out.stderr);
        assert!(
            err.starts_with(&format!("error: invalid value '{bad}' for '--every <EVERY>': '{bad}' is not an interval above zero")),
            "{err}"
        );
        assert!(err.contains("For more information, try '--help'."), "{err}");
    }
    refused(&run(&["filmstrip", c, s, "--count", "0"]), "--count");
    refused(
        &run(&["filmstrip", c, s, "--count", "4000", "--height", "240"]),
        "268 megapixels",
    );
    refused(
        &run(&["filmstrip", c, folder.join("strip.gif").to_str().unwrap()]),
        ".png",
    );
    refused(&run(&["filmstrip", c, s, "--from", "5"]), "past the end");
    assert!(!strip.exists());
    let both = run(&["filmstrip", c, s, "--every", "1", "--count", "2"]);
    assert_eq!(both.status.code(), Some(2));
    let both = run(&["filmstrip", c, s, "--wrap", "2", "--max-width", "300"]);
    assert_eq!(both.status.code(), Some(2));
    let bad = run(&["filmstrip", c, s, "--background", "red"]);
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn an_existing_output_needs_force() {
    let folder = dir("force");
    let clip = video(&folder);
    let strip = folder.join("strip.png");
    fs::write(&strip, b"keep me").unwrap();
    let args = [
        "filmstrip",
        clip.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--count",
        "2",
        "--route",
        "cpu",
    ];
    refused(&run(&args), "--force");
    assert_eq!(fs::read(&strip).unwrap(), b"keep me");
    let mut forced = args.to_vec();
    forced.push("--force");
    ok(&run(&forced));
    assert_eq!(png(&strip).height, 98);
    let leftovers: Vec<_> = fs::read_dir(&folder)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".partial"))
        .collect();
    assert!(leftovers.is_empty());
}

fn crop(img: &Rgba, x: u32, w: u32, y: u32, h: u32) -> Vec<[u8; 3]> {
    (y..y + h)
        .flat_map(|yy| (x..x + w).map(move |xx| (xx, yy)))
        .map(|(xx, yy)| pixel(img, xx, yy))
        .collect()
}

#[test]
fn the_default_seek_labels_each_tile_with_the_keyframe_it_shows() {
    let folder = dir("keyframe");
    let clip = video_of(&folder, 50);
    let strip = folder.join("strip.png");
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--every",
        "1",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&out);
    let json = text(&out.stdout);
    assert_eq!(field(&json, "seek"), Some("keyframe"), "{json}");
    let asked = list(&json, "asked");
    let times = list(&json, "times");
    assert_eq!(asked.len(), 5, "{json}");
    for (t, want) in times.iter().zip([0.0, 0.0, 2.0, 2.0, 4.0]) {
        assert!((t - want).abs() < 1e-3, "{json}");
    }
    assert_eq!(times.len(), 5, "{json}");
    let tw: u32 = field(&json, "tile_width").unwrap().parse().unwrap();
    let img = png(&strip);
    let tile = |i: u32| crop(&img, 4 + i * (tw + 4), tw, 4, 90);
    assert_eq!(
        tile(0),
        tile(1),
        "0:00 and 0:01 should show the same keyframe"
    );
    assert_eq!(
        tile(2),
        tile(3),
        "0:02 and 0:03 should show the same keyframe"
    );
    assert_ne!(
        tile(1),
        tile(2),
        "the 0:02 keyframe should differ from 0:00"
    );
    assert_ne!(tile(3), tile(4));
    let line = text(
        &run(&[
            "filmstrip",
            clip.to_str().unwrap(),
            folder.join("line.png").to_str().unwrap(),
            "--every",
            "1",
            "--route",
            "cpu",
        ])
        .stdout,
    );
    assert!(
        line.trim_end().ends_with("at 0:00 0:00 0:02 0:02 0:04"),
        "{line}"
    );
}

#[test]
fn exact_seek_past_the_end_stops_at_the_end() {
    let folder = dir("to-past");
    let clip = video(&folder);
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        folder.join("strip.png").to_str().unwrap(),
        "--every",
        "0.5",
        "--to",
        "30",
        "--seek",
        "exact",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&out);
    let times = list(&text(&out.stdout), "times");
    assert_eq!(times.len(), 4);
    assert!(times.iter().all(|t| *t < 2.0));
}

#[test]
fn a_jpeg_strip_is_written_or_refused_in_one_line_before_decoding() {
    let folder = dir("jpeg");
    let clip = video(&folder);
    let strip = folder.join("strip.jpg");
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--count",
        "2",
        "--route",
        "cpu",
    ]);
    match out.status.code() {
        Some(0) => assert!(fs::read(&strip).unwrap().starts_with(&[0xff, 0xd8])),
        _ => {
            refused(&out, "JPEG");
            assert!(!strip.exists());
        }
    }
    let leftovers = fs::read_dir(&folder)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".partial"))
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn bad_heights_wraps_and_widths_are_refused_in_one_line() {
    let folder = dir("refused-shape");
    let clip = video(&folder);
    let c = clip.to_str().unwrap();
    let strip = folder.join("strip.png");
    let s = strip.to_str().unwrap();
    refused(&run(&["filmstrip", c, s, "--height", "0"]), "height 0");
    refused(
        &run(&["filmstrip", c, s, "--height", "4321"]),
        "height 4321",
    );
    refused(&run(&["filmstrip", c, s, "--wrap", "0"]), "--wrap");
    refused(
        &run(&["filmstrip", c, s, "--max-width", "0"]),
        "narrower than one tile",
    );
    refused(
        &run(&["filmstrip", c, s, "--max-width", "100"]),
        "narrower than one tile",
    );
    assert!(!strip.exists());
}

#[test]
fn timecodes_never_spill_into_the_gaps_on_a_narrow_portrait_tile() {
    let folder = dir("portrait");
    let frames = folder.join("frames");
    fs::create_dir_all(&frames).unwrap();
    for n in 0..20u32 {
        let mut img = Rgba::new(240, 432);
        for px in img.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[(n * 10) as u8, 90, 160, 255]);
        }
        fs::write(
            frames.join(format!("{n:04}.png")),
            pito_codecs::png::encode(&img).unwrap(),
        )
        .unwrap();
    }
    let clip = folder.join("clip.mp4");
    ok(&run(&[
        frames.to_str().unwrap(),
        clip.to_str().unwrap(),
        "--fps",
        "10",
    ]));
    let strip = folder.join("strip.png");
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--count",
        "3",
        "--height",
        "40",
        "--timecodes",
        "--background",
        "#ff0000",
        "--seek",
        "exact",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&out);
    let json = text(&out.stdout);
    let tw: u32 = field(&json, "tile_width").unwrap().parse().unwrap();
    let img = png(&strip);
    let cell = (img.width - 4 * 4) / 3;
    assert!(cell > tw, "{json}");
    for c in 0..=3u32 {
        let x0 = c * (cell + 4);
        for y in 0..img.height {
            for x in x0..x0 + 4 {
                assert_eq!(pixel(&img, x, y), [255, 0, 0], "ink in the gap at {x},{y}");
            }
        }
    }
}

#[test]
fn a_webm_labels_each_tile_with_the_keyframe_it_shows() {
    let folder = dir("keyframe-webm");
    let clip = video_of(&folder, 50);
    let webm = folder.join("clip.webm");
    ok(&run(&[clip.to_str().unwrap(), webm.to_str().unwrap()]));
    let strip = folder.join("strip.png");
    let out = run(&[
        "filmstrip",
        webm.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--every",
        "1",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&out);
    let json = text(&out.stdout);
    assert_eq!(field(&json, "seek"), Some("keyframe"), "{json}");
    let asked = list(&json, "asked");
    let times = list(&json, "times");
    assert_eq!(asked.len(), 5, "{json}");
    assert_eq!(times.len(), 5, "{json}");
    for (t, a) in times.iter().zip(&asked) {
        assert!(*t <= *a + 1e-3, "{json}");
    }
    let tw: u32 = field(&json, "tile_width").unwrap().parse().unwrap();
    let img = png(&strip);
    let tile = |i: u32| crop(&img, 4 + i * (tw + 4), tw, 4, 90);
    for i in 1..5u32 {
        let same = (times[i as usize] - times[i as usize - 1]).abs() < 1e-3;
        assert_eq!(tile(i) == tile(i - 1), same, "tile {i}: {json}");
    }
}

#[test]
fn an_alpha_webm_strip_is_flattened_and_labelled_with_the_frames_it_shows() {
    let folder = dir("alpha-webm");
    let frames = folder.join("frames");
    fs::create_dir_all(&frames).unwrap();
    for n in 0..30u32 {
        let mut img = Rgba::new(352, 240);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let x = i as u32 % 352;
            px.copy_from_slice(&[(x + n * 8) as u8, 200, 40, if x < 176 { 0 } else { 255 }]);
        }
        fs::write(
            frames.join(format!("{n:04}.png")),
            pito_codecs::png::encode_rgba(&img).unwrap(),
        )
        .unwrap();
    }
    let webm = folder.join("clip.webm");
    ok(&run(&[
        frames.to_str().unwrap(),
        webm.to_str().unwrap(),
        "--fps",
        "10",
    ]));
    let strip = folder.join("strip.png");
    let out = run(&[
        "filmstrip",
        webm.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--every",
        "1",
        "--background",
        "#0000ff",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&out);
    let json = text(&out.stdout);
    let asked = list(&json, "asked");
    let times = list(&json, "times");
    assert_eq!(asked.len(), 3, "{json}");
    assert_eq!(times.len(), 3, "{json}");
    for (t, a) in times.iter().zip(&asked) {
        assert!(*t <= *a + 1e-3, "{json}");
    }
    let img = png(&strip);
    assert!(img.data.chunks_exact(4).all(|p| p[3] == 255));
    let tw: u32 = field(&json, "tile_width").unwrap().parse().unwrap();
    assert_eq!(
        pixel(&img, 4 + 10, 40),
        [0, 0, 255],
        "the see-through half shows the background"
    );
    assert_ne!(pixel(&img, 4 + tw - 10, 40), [0, 0, 255]);
}

#[test]
fn a_window_ending_before_it_starts_or_an_output_that_is_the_input_is_refused() {
    let folder = dir("window-and-self");
    let clip = video(&folder);
    let c = clip.to_str().unwrap();
    let strip = folder.join("strip.png");
    let s = strip.to_str().unwrap();
    refused(
        &run(&["filmstrip", c, s, "--from", "1", "--to", "0.5"]),
        "must come after --from 1",
    );
    let same = folder.join("same.png");
    fs::copy(&clip, &same).unwrap();
    let same = same.to_str().unwrap();
    refused(
        &run(&["filmstrip", same, same, "--force"]),
        "the output is the input",
    );
    assert!(!strip.exists());
}

#[test]
fn a_sheet_alone_lists_its_stills_with_no_paths_and_a_tall_height_is_refused() {
    let folder = dir("sheet-json");
    let clip = video(&folder);
    let c = clip.to_str().unwrap();
    let sheet = folder.join("sheet.png");
    let out = run(&[
        "stills",
        c,
        "--count",
        "3",
        "--sheet",
        sheet.to_str().unwrap(),
        "--seek",
        "exact",
        "--height",
        "60",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&out);
    let json = text(&out.stdout);
    assert_eq!(json.lines().count(), 1, "{json}");
    assert!(json.contains("\"seek\":\"exact\""), "{json}");
    assert_eq!(json.matches("\"path\":null").count(), 3, "{json}");
    assert_eq!(png(&sheet).data.len() % 4, 0);
    refused(
        &run(&[
            "stills",
            c,
            "--count",
            "3",
            "--height",
            "5000",
            "--sheet",
            folder.join("tall.png").to_str().unwrap(),
        ]),
        "height 5000 is outside 1 to 4320",
    );
}

fn wav(path: &Path, frames: u32) {
    let data = frames * 4;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&48_000u32.to_le_bytes());
    b.extend_from_slice(&192_000u32.to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    b.resize(b.len() + data as usize, 0);
    fs::write(path, b).unwrap();
}

fn partials(folder: &Path) -> usize {
    fs::read_dir(folder)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".partial"))
        .count()
}

#[test]
fn audio_alone_or_an_empty_file_is_refused_before_decoding() {
    let folder = dir("no-video");
    let strip = folder.join("strip.png");
    let s = strip.to_str().unwrap();
    let tone = folder.join("tone.wav");
    wav(&tone, 48_000);
    refused(
        &run(&["filmstrip", tone.to_str().unwrap(), s]),
        "tone.wav: has no video to take tiles from",
    );
    let empty = folder.join("empty.wav");
    wav(&empty, 0);
    refused(
        &run(&["filmstrip", empty.to_str().unwrap(), s]),
        "empty.wav: has no duration to take tiles over",
    );
    assert!(!strip.exists());
}

#[test]
fn a_tile_that_fails_to_decode_is_refused_with_its_time() {
    let folder = dir("broken-tile");
    let clip = folder.join("bars.mp4");
    ok(&run(&[
        "synth",
        clip.to_str().unwrap(),
        "--size",
        "352x240",
        "--fps",
        "10",
        "--duration",
        "6",
    ]));
    let bytes = fs::read(&clip).unwrap();
    let cut = folder.join("cut.mp4");
    fs::write(&cut, &bytes[..bytes.len() - 1]).unwrap();
    let c = cut.to_str().unwrap();
    let strip = folder.join("strip.png");
    let s = strip.to_str().unwrap();
    let out = run(&[
        "filmstrip",
        c,
        s,
        "--from",
        "5.8",
        "--count",
        "1",
        "--seek",
        "exact",
        "--route",
        "cpu",
    ]);
    refused(&out, "cut.mp4: tile at 5.900 s: ");
    assert!(!strip.exists());
    assert_eq!(partials(&folder), 0);
    refused(
        &run(&[
            "filmstrip",
            c,
            s,
            "--from",
            "5.8",
            "--count",
            "1",
            "--seek",
            "exact",
            "--route",
            "cpu",
            "--height",
            "1",
            "--gap",
            "0",
            "--max-width",
            "1",
        ]),
        "--max-width 1 is narrower than one tile (2 px with its gaps)",
    );
}

#[test]
fn a_taller_height_keeps_the_videos_own_size() {
    let folder = dir("no-upscale");
    let clip = video(&folder);
    let strip = folder.join("strip.png");
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--count",
        "2",
        "--height",
        "4320",
        "--route",
        "cpu",
        "--json",
    ]);
    ok(&out);
    let json = text(&out.stdout);
    assert_eq!(field(&json, "tile_width"), Some("352"), "{json}");
    assert_eq!(field(&json, "tile_height"), Some("240"), "{json}");
    let img = png(&strip);
    assert_eq!((img.width, img.height), (2 * 352 + 3 * 4, 240 + 2 * 4));
}

#[test]
fn a_failed_landing_leaves_no_partial_file() {
    let folder = dir("landing");
    let clip = video(&folder);
    let strip = folder.join("strip.png");
    fs::create_dir_all(strip.join("inside")).unwrap();
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--count",
        "2",
        "--route",
        "cpu",
        "--force",
    ]);
    refused(&out, "strip.png");
    assert!(strip.join("inside").is_dir());
    assert_eq!(partials(&folder), 0);
}

#[test]
fn a_stdout_nobody_reads_is_a_one_line_refusal_after_the_strip_lands() {
    let folder = dir("closed-stdout");
    let clip = video(&folder);
    let strip = folder.join("strip.png");
    let mut child = Command::new(env!("CARGO_BIN_EXE_pcod"))
        .env("NO_COLOR", "1")
        .args([
            "filmstrip",
            clip.to_str().unwrap(),
            strip.to_str().unwrap(),
            "--count",
            "2",
            "--route",
            "cpu",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    refused(&out, "Broken pipe");
    assert_eq!(png(&strip).height, 90 + 2 * 4);
}

#[test]
fn a_jpeg_strip_is_the_same_bytes_with_and_without_simd() {
    let folder = dir("jpeg-simd");
    let clip = video(&folder);
    let strip = |name: &str, scalar: bool| -> Vec<u8> {
        let path = folder.join(name);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pcod"));
        cmd.env("NO_COLOR", "1").args([
            "filmstrip",
            clip.to_str().unwrap(),
            path.to_str().unwrap(),
            "--count",
            "3",
            "--timecodes",
            "--route",
            "cpu",
        ]);
        if scalar {
            cmd.env("PITO_CODECS_NO_SIMD", "1");
        }
        let out = cmd.output().unwrap();
        ok(&out);
        fs::read(path).unwrap()
    };
    let fast = strip("fast.jpg", false);
    let plain = strip("plain.jpg", true);
    assert!(fast.starts_with(&[0xff, 0xd8]) && fast.ends_with(&[0xff, 0xd9]));
    assert!(fast == plain, "{} vs {} bytes", fast.len(), plain.len());
}

fn bars_clip(folder: &Path) -> PathBuf {
    let clip = folder.join("bars.mp4");
    ok(&run(&[
        "synth",
        clip.to_str().unwrap(),
        "--size",
        "352x240",
        "--fps",
        "10",
        "--duration",
        "2",
    ]));
    clip
}

fn sized(clip: &Path, out: &Path, width: Option<u16>, height: Option<u16>) {
    let mut b = fs::read(clip).unwrap();
    let stsd = b.windows(4).position(|w| w == b"stsd").unwrap();
    let at = stsd + b[stsd..].windows(4).position(|w| w == b"avc1").unwrap() + 28;
    assert_eq!(b[at..at + 4], [1, 96, 0, 240]);
    if let Some(w) = width {
        b[at..at + 2].copy_from_slice(&w.to_be_bytes());
    }
    if let Some(h) = height {
        b[at + 2..at + 4].copy_from_slice(&h.to_be_bytes());
    }
    fs::write(out, b).unwrap();
}

fn one_tile(clip: &Path, height: u32) -> (Rgba, String) {
    let strip = clip.with_extension(format!("{height}.png"));
    let out = run(&[
        "filmstrip",
        clip.to_str().unwrap(),
        strip.to_str().unwrap(),
        "--count",
        "1",
        "--gap",
        "0",
        "--height",
        &height.to_string(),
        "--route",
        "cpu",
        "--force",
        "--json",
    ]);
    ok(&out);
    (png(&strip), text(&out.stdout))
}

fn apart(ours: &Rgba, whole: &Rgba) -> f64 {
    let mut sum = 0u64;
    for y in 0..ours.height {
        for x in 0..ours.width {
            let a = pixel(ours, x, y);
            let b = pixel(
                whole,
                x * whole.width / ours.width,
                y * whole.height / ours.height,
            );
            sum += a
                .iter()
                .zip(b)
                .map(|(p, q)| u64::from(p.abs_diff(q)))
                .sum::<u64>();
        }
    }
    sum as f64 / f64::from(ours.width * ours.height * 3)
}

#[test]
fn a_sample_entry_narrower_than_the_stream_keeps_the_whole_picture() {
    let folder = dir("narrow-entry");
    let clip = bars_clip(&folder);
    let narrow = folder.join("narrow.mp4");
    sized(&clip, &narrow, Some(320), None);
    let (strip, json) = one_tile(&narrow, 90);
    assert_eq!(field(&json, "tile_width"), Some("120"), "{json}");
    assert_eq!(field(&json, "tile_height"), Some("81"), "{json}");
    assert_eq!((strip.width, strip.height), (120, 81));
    let (whole, _) = one_tile(&clip, 81);
    assert_eq!((whole.width, whole.height), (118, 81));
    let tile = strip;
    let off = apart(&tile, &whole);
    assert!(
        off < 6.0,
        "the tile is {off:.2} apart from the whole picture"
    );
    for bar in 0..7 {
        let (x, rx) = ((2 * bar + 1) * 120 / 14, (2 * bar + 1) * 118 / 14);
        let (a, b) = (pixel(&tile, x, 4), pixel(&whole, rx, 4));
        assert!(
            a.iter().zip(b).all(|(p, q)| p.abs_diff(q) < 24),
            "bar {bar}: {a:?} against {b:?}"
        );
    }
}

#[test]
fn a_sample_entry_shorter_than_the_stream_keeps_the_whole_picture() {
    let folder = dir("short-entry");
    let clip = bars_clip(&folder);
    let short = folder.join("short.mp4");
    sized(&clip, &short, None, Some(60));
    let (strip, json) = one_tile(&short, 90);
    assert_eq!(field(&json, "tile_width"), Some("88"), "{json}");
    assert_eq!(field(&json, "tile_height"), Some("60"), "{json}");
    assert_eq!((strip.width, strip.height), (88, 60));
    let (whole, _) = one_tile(&clip, 60);
    assert_eq!((whole.width, whole.height), (88, 60));
    let off = apart(&strip, &whole);
    assert!(
        off < 6.0,
        "the tile is {off:.2} apart from the whole picture"
    );
    let bottom = (0..88).map(|x| pixel(&strip, x, 59)).collect::<Vec<_>>();
    let truth = (0..88).map(|x| pixel(&whole, x, 59)).collect::<Vec<_>>();
    let rows = bottom
        .iter()
        .zip(&truth)
        .all(|(a, b)| a.iter().zip(b).all(|(p, q)| p.abs_diff(*q) < 24));
    assert!(rows, "{bottom:?} against {truth:?}");
}
