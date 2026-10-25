pub const AREA: &str = "mkv";
pub const SIZE: (u32, u32) = (160, 120);
pub const FPS: u32 = 15;
pub const SECONDS: u32 = 2;

pub const H264: &str = "-c:v|libx264|-bf|2|-g|7|-crf|40|-pix_fmt|yuv420p|-c:a|aac|-b:a|48k";
pub const VP9: &str =
    "-c:v|libvpx-vp9|-g|7|-deadline|realtime|-cpu-used|8|-b:v|40k|-c:a|libopus|-b:a|24k";

pub const FOREIGN: [(&str, &str, &str); 3] = [
    ("h264.mkv", H264, ""),
    ("vp9.webm", VP9, ""),
    ("piped.mkv", H264, "-f|matroska|-seekable|0"),
];

pub const PACKETS: &str = "packet=stream_index,pts,size,flags";

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

pub fn split(args: &str) -> Vec<String> {
    args.split('|')
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .collect()
}

pub type Packets = Vec<Vec<(i64, u32, bool)>>;

pub fn parse_packets(csv: &str) -> Packets {
    let mut streams: Packets = Vec::new();
    for line in csv.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split(',').collect();
        let (Some(s), Some(pts), Some(size)) = (
            f.first().and_then(|s| s.parse::<usize>().ok()),
            f.get(1).and_then(|p| p.parse::<i64>().ok()),
            f.get(2).and_then(|p| p.parse::<u32>().ok()),
        ) else {
            continue;
        };
        if streams.len() <= s {
            streams.resize(s + 1, Vec::new());
        }
        streams[s].push((pts, size, f.get(3).is_some_and(|k| k.contains('K'))));
    }
    streams
}
