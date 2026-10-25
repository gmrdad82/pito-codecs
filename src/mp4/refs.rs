pub const AREA: &str = "mp4";
pub const SIZE: (u32, u32) = (160, 120);
pub const FPS: u32 = 30;
pub const SECONDS: u32 = 1;
pub const AUDIO_KBPS: u32 = 64;

pub const FOREIGN: [(&str, &str); 4] = [
    ("bframes.mp4", "-c:v|libx264|-bf|3|-g|30"),
    (
        "hevc.mov",
        "-c:v|libx265|-x265-params|log-level=none:bframes=4|-tag:v|hvc1",
    ),
    (
        "fragmented.mp4",
        "-c:v|libx264|-bf|3|-g|30|-movflags|frag_keyframe+empty_moov",
    ),
    (
        "fragmented-base.mp4",
        "-c:v|libx264|-bf|2|-g|15|-movflags|frag_keyframe+default_base_moof",
    ),
];

pub const PACKETS: &str = "packet=stream_index,pts,dts,size,flags";

pub fn packets_of(file: &str) -> String {
    format!("{file}.csv")
}

pub fn sources() -> Vec<String> {
    let (w, h) = SIZE;
    vec![
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("testsrc2=size={w}x{h}:rate={FPS}:duration={SECONDS}"),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("sine=frequency=440:sample_rate=48000:duration={SECONDS}"),
    ]
}

pub fn encoding(video: &str) -> Vec<String> {
    let mut args: Vec<String> = video.split('|').map(str::to_string).collect();
    args.extend(
        [
            "-crf",
            "40",
            "-c:a",
            "aac",
            "-b:a",
            &format!("{AUDIO_KBPS}k"),
            "-ac",
            "2",
            "-pix_fmt",
            "yuv420p",
        ]
        .map(str::to_string),
    );
    args
}

pub fn parse_packets(csv: &str) -> Vec<Vec<String>> {
    let mut streams: Vec<Vec<String>> = Vec::new();
    for line in csv.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split(',').collect();
        let Some(s) = f.first().and_then(|s| s.parse::<usize>().ok()) else {
            continue;
        };
        if streams.len() <= s {
            streams.resize(s + 1, Vec::new());
        }
        let key = if f.get(4).is_some_and(|k| k.contains('K')) {
            "K"
        } else {
            "_"
        };
        let at = |i: usize| f.get(i).copied().unwrap_or_default();
        streams[s].push(format!("{},{},{},{key}", at(1), at(2), at(3)));
    }
    streams
}
