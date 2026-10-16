use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Hevc,
    Aac,
    Vp9,
    Av1,
    Opus,
    Ac3,
    Eac3,
    Mp3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    Gpu,
    Cpu,
    Ffmpeg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Decode,
    Encode,
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
            Codec::Aac => "AAC",
            Codec::Vp9 => "VP9",
            Codec::Av1 => "AV1",
            Codec::Opus => "Opus",
            Codec::Ac3 => "AC-3",
            Codec::Eac3 => "E-AC-3",
            Codec::Mp3 => "MP3",
        })
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Route::Gpu => "GPU",
            Route::Cpu => "CPU",
            Route::Ffmpeg => "ffmpeg",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    Yuv420p8,
    Yuv420p10,
}

impl PixelFormat {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            PixelFormat::Yuv420p8 => 1,
            PixelFormat::Yuv420p10 => 2,
        }
    }

    pub fn bit_depth(self) -> u8 {
        match self {
            PixelFormat::Yuv420p8 => 8,
            PixelFormat::Yuv420p10 => 10,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeBase {
    pub num: u32,
    pub den: u32,
}

impl TimeBase {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    pub fn seconds(self, ticks: i64) -> f64 {
        ticks as f64 * self.num as f64 / self.den as f64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plane {
    pub data: Vec<u8>,
    pub stride: usize,
    pub width: usize,
    pub height: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    pub pts: i64,
    pub time_base: TimeBase,
    pub planes: [Plane; 3],
}

impl Frame {
    pub fn new(
        format: PixelFormat,
        width: u32,
        height: u32,
        pts: i64,
        time_base: TimeBase,
    ) -> Self {
        let bytes = format.bytes_per_sample();
        let cw = width.div_ceil(2) as usize;
        let ch = height.div_ceil(2) as usize;
        let plane = |w: usize, h: usize| Plane {
            data: vec![0; w * bytes * h],
            stride: w * bytes,
            width: w,
            height: h,
        };
        Self {
            format,
            width,
            height,
            pts,
            time_base,
            planes: [
                plane(width as usize, height as usize),
                plane(cw, ch),
                plane(cw, ch),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub data: Vec<u8>,
    pub pts: i64,
    pub dts: i64,
    pub keyframe: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioBuffer {
    pub samples: Vec<f32>,
    pub rate: u32,
    pub channels: u16,
}

impl AudioBuffer {
    pub fn new(rate: u32, channels: u16) -> Self {
        Self {
            samples: Vec::new(),
            rate,
            channels,
        }
    }

    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; width as usize * height as usize * 4],
        }
    }

    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rate {
    Crf(u8),
    Bitrate(u32),
    Vbr { kbps: u32, max_kbps: u32 },
    Qp(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bitstream {
    LengthPrefixed,
    AnnexB,
}

#[derive(Debug)]
pub enum Error {
    Unsupported(String),
    Invalid(String),
    Backend(String),
    Io(std::io::Error),
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
            Error::Invalid(s) => write!(f, "invalid data: {s}"),
            Error::Backend(s) => write!(f, "backend: {s}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl Error {
    pub fn at(path: &std::path::Path, e: std::io::Error) -> Self {
        let text = e.to_string();
        let cause = match text.rfind(" (os error ") {
            Some(i) if text.ends_with(')') => &text[..i],
            _ => text.as_str(),
        };
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {cause}", path.display()),
        ))
    }

    pub fn in_file(self, path: &std::path::Path) -> Self {
        let shown = path.display().to_string();
        let named = |s: String| {
            if s.contains(&shown) {
                s
            } else {
                format!("{shown}: {s}")
            }
        };
        match self {
            Error::Unsupported(s) => Error::Unsupported(named(s)),
            Error::Invalid(s) => Error::Invalid(named(s)),
            Error::Backend(s) => Error::Backend(named(s)),
            Error::Io(e) if !e.to_string().contains(&shown) => Error::at(path, e),
            e => e,
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_sizes_round_chroma_up_and_ten_bit_doubles_stride() {
        let f = Frame::new(
            PixelFormat::Yuv420p10,
            1281,
            721,
            0,
            TimeBase::new(1, 90000),
        );
        assert_eq!(f.planes[0].stride, 1281 * 2);
        assert_eq!((f.planes[1].width, f.planes[1].height), (641, 361));
        assert_eq!(f.planes[2].data.len(), 641 * 2 * 361);
    }
}
