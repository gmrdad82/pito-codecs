use crate::media::Rgba;
use libloading::{Library, Symbol};
use std::ffi::{c_float, c_int, c_void};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

pub struct Libwebp {
    webp: Library,
    demux: Option<Library>,
}

static LIB: LazyLock<Option<Libwebp>> = LazyLock::new(|| {
    let webp = unsafe { Library::new("libwebp.so.7") }.ok()?;
    let demux = unsafe { Library::new("libwebpdemux.so.2") }.ok();
    Some(Libwebp { webp, demux })
});

pub fn lib() -> Option<&'static Libwebp> {
    let l = LIB.as_ref();
    if l.is_none() {
        eprintln!("libwebp.so.7 not found; oracle checks skipped");
    }
    l
}

type GetInfo = unsafe extern "C" fn(*const u8, usize, *mut c_int, *mut c_int) -> c_int;
type DecodeRgba = unsafe extern "C" fn(*const u8, usize, *mut c_int, *mut c_int) -> *mut u8;
type DecodeYuvInto = unsafe extern "C" fn(
    *const u8,
    usize,
    *mut u8,
    usize,
    c_int,
    *mut u8,
    usize,
    c_int,
    *mut u8,
    usize,
    c_int,
) -> *mut u8;
type EncodeRgba =
    unsafe extern "C" fn(*const u8, c_int, c_int, c_int, c_float, *mut *mut u8) -> usize;
type EncodeLossless = unsafe extern "C" fn(*const u8, c_int, c_int, c_int, *mut *mut u8) -> usize;
type Free = unsafe extern "C" fn(*mut c_void);

#[repr(C)]
struct WebPData {
    bytes: *const u8,
    size: usize,
}

#[repr(C)]
struct AnimOptions {
    color_mode: c_int,
    use_threads: c_int,
    padding: [u32; 7],
}

#[repr(C)]
#[derive(Default)]
struct AnimInfo {
    canvas_width: u32,
    canvas_height: u32,
    loop_count: u32,
    bgcolor: u32,
    frame_count: u32,
    pad: [u32; 4],
}

impl Libwebp {
    fn sym<T>(&self, name: &[u8]) -> Symbol<'_, T> {
        unsafe { self.webp.get(name) }.expect("libwebp symbol")
    }

    fn free(&self, p: *mut u8) {
        let f: Symbol<Free> = self.sym(b"WebPFree");
        unsafe { f(p.cast()) }
    }

    pub fn size(&self, data: &[u8]) -> Option<(u32, u32)> {
        let f: Symbol<GetInfo> = self.sym(b"WebPGetInfo");
        let (mut w, mut h) = (0, 0);
        (unsafe { f(data.as_ptr(), data.len(), &mut w, &mut h) } != 0)
            .then_some((w as u32, h as u32))
    }

    pub fn decode_rgba(&self, data: &[u8]) -> Option<Rgba> {
        let f: Symbol<DecodeRgba> = self.sym(b"WebPDecodeRGBA");
        let (mut w, mut h) = (0, 0);
        let p = unsafe { f(data.as_ptr(), data.len(), &mut w, &mut h) };
        if p.is_null() {
            return None;
        }
        let n = w as usize * h as usize * 4;
        let out = unsafe { std::slice::from_raw_parts(p, n) }.to_vec();
        self.free(p);
        Some(Rgba {
            width: w as u32,
            height: h as u32,
            data: out,
        })
    }

    pub fn decode_yuv(&self, data: &[u8]) -> Option<[Vec<u8>; 3]> {
        let (w, h) = self.size(data)?;
        let (w, h) = (w as usize, h as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut y = vec![0u8; w * h];
        let mut u = vec![0u8; cw * ch];
        let mut v = vec![0u8; cw * ch];
        let f: Symbol<DecodeYuvInto> = self.sym(b"WebPDecodeYUVInto");
        let p = unsafe {
            f(
                data.as_ptr(),
                data.len(),
                y.as_mut_ptr(),
                y.len(),
                w as c_int,
                u.as_mut_ptr(),
                u.len(),
                cw as c_int,
                v.as_mut_ptr(),
                v.len(),
                cw as c_int,
            )
        };
        (!p.is_null()).then_some([y, u, v])
    }

    pub fn encode(&self, image: &Rgba, quality: f32) -> Vec<u8> {
        let f: Symbol<EncodeRgba> = self.sym(b"WebPEncodeRGBA");
        let mut p = std::ptr::null_mut();
        let n = unsafe {
            f(
                image.data.as_ptr(),
                image.width as c_int,
                image.height as c_int,
                image.stride() as c_int,
                quality,
                &mut p,
            )
        };
        let out = unsafe { std::slice::from_raw_parts(p, n) }.to_vec();
        self.free(p);
        out
    }

    pub fn encode_lossless(&self, image: &Rgba) -> Vec<u8> {
        let f: Symbol<EncodeLossless> = self.sym(b"WebPEncodeLosslessRGBA");
        let mut p = std::ptr::null_mut();
        let n = unsafe {
            f(
                image.data.as_ptr(),
                image.width as c_int,
                image.height as c_int,
                image.stride() as c_int,
                &mut p,
            )
        };
        let out = unsafe { std::slice::from_raw_parts(p, n) }.to_vec();
        self.free(p);
        out
    }

    pub fn decode_animation(&self, data: &[u8]) -> Option<Vec<(Rgba, i32)>> {
        type Init = unsafe extern "C" fn(*mut AnimOptions, c_int) -> c_int;
        type New = unsafe extern "C" fn(*const WebPData, *const AnimOptions, c_int) -> *mut c_void;
        type GetInfo = unsafe extern "C" fn(*const c_void, *mut AnimInfo) -> c_int;
        type More = unsafe extern "C" fn(*const c_void) -> c_int;
        type Next = unsafe extern "C" fn(*mut c_void, *mut *mut u8, *mut c_int) -> c_int;
        type Delete = unsafe extern "C" fn(*mut c_void);
        let d = self.demux.as_ref()?;
        unsafe {
            let init: Symbol<Init> = d.get(b"WebPAnimDecoderOptionsInitInternal").ok()?;
            let new: Symbol<New> = d.get(b"WebPAnimDecoderNewInternal").ok()?;
            let get_info: Symbol<GetInfo> = d.get(b"WebPAnimDecoderGetInfo").ok()?;
            let more: Symbol<More> = d.get(b"WebPAnimDecoderHasMoreFrames").ok()?;
            let next: Symbol<Next> = d.get(b"WebPAnimDecoderGetNext").ok()?;
            let delete: Symbol<Delete> = d.get(b"WebPAnimDecoderDelete").ok()?;
            let mut opts = AnimOptions {
                color_mode: 0,
                use_threads: 0,
                padding: [0; 7],
            };
            if init(&mut opts, 0x0107) == 0 {
                return None;
            }
            opts.color_mode = 1;
            let wd = WebPData {
                bytes: data.as_ptr(),
                size: data.len(),
            };
            let dec = new(&wd, &opts, 0x0107);
            if dec.is_null() {
                return None;
            }
            let mut info = AnimInfo::default();
            get_info(dec, &mut info);
            let n = info.canvas_width as usize * info.canvas_height as usize * 4;
            let mut frames = Vec::new();
            while more(dec) != 0 {
                let mut buf = std::ptr::null_mut();
                let mut ts = 0;
                if next(dec, &mut buf, &mut ts) == 0 {
                    delete(dec);
                    return None;
                }
                frames.push((
                    Rgba {
                        width: info.canvas_width,
                        height: info.canvas_height,
                        data: std::slice::from_raw_parts(buf, n).to_vec(),
                    },
                    ts,
                ));
            }
            delete(dec);
            Some(frames)
        }
    }
}

pub fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/fixtures/webp");
    std::fs::create_dir_all(&d).expect("fixture dir");
    d
}

pub fn ffmpeg(args: &[&str]) -> bool {
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("ffmpeg {args:?} failed or is missing; check skipped");
    }
    ok
}

pub fn source(lavfi: &str, width: u32, height: u32, frames: usize) -> Option<Vec<Rgba>> {
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!(
            "{lavfi}{}size={width}x{height}",
            if lavfi.contains('=') { ':' } else { '=' }
        ))
        .args([
            "-frames:v",
            &frames.to_string(),
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!("ffmpeg source {lavfi} failed; check skipped");
        return None;
    }
    let n = width as usize * height as usize * 4;
    Some(
        out.stdout
            .chunks_exact(n)
            .map(|c| Rgba {
                width,
                height,
                data: c.to_vec(),
            })
            .collect(),
    )
}

pub fn ffmpeg_decode(path: &Path) -> Option<Vec<Rgba>> {
    let (w, h) = {
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(path)
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        let mut it = s.trim().split(',').map(|v| v.parse::<u32>().ok());
        (it.next()??, it.next()??)
    };
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-pix_fmt", "rgba", "-f", "rawvideo", "-"])
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!("ffmpeg could not decode {}", path.display());
        return None;
    }
    let n = w as usize * h as usize * 4;
    Some(
        out.stdout
            .chunks_exact(n)
            .map(|c| Rgba {
                width: w,
                height: h,
                data: c.to_vec(),
            })
            .collect(),
    )
}

pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let se: u64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = x as i64 - y as i64;
            (d * d) as u64
        })
        .sum();
    if se == 0 {
        return 99.0;
    }
    let mse = se as f64 / a.len() as f64;
    10.0 * (255.0 * 255.0 / mse).log10()
}
