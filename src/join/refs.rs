pub const JOIN: &str = "join";

pub const SMALL: (u32, u32) = (160, 120);
pub const GPU_FLOOR: (u32, u32) = (352, 240);
pub const FPS: u32 = 30;
pub const FRAMES: u64 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Made {
    X264 { chroma: u8, bits: u8 },
    X265 { chroma: u8 },
    Mpeg4,
}

pub const JOIN_CLIPS: [(&str, (u32, u32), Made); 5] = [
    (
        "h264-422-10.mp4",
        SMALL,
        Made::X264 {
            chroma: 2,
            bits: 10,
        },
    ),
    ("h264-444.mp4", SMALL, Made::X264 { chroma: 3, bits: 8 }),
    ("hevc-422.mp4", SMALL, Made::X265 { chroma: 2 }),
    ("hevc.mp4", GPU_FLOOR, Made::X265 { chroma: 1 }),
    ("mpeg4.mp4", SMALL, Made::Mpeg4),
];
