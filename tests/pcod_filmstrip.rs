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
        &run(&["filmstrip", c, s, "--count", "20", "--height", "4320"]),
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
