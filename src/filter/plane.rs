use crate::media::{Error, Frame, PixelFormat, Plane, Result, TimeBase};

pub(crate) trait Sample: Copy + Send + Sync + Default + 'static {
    const BYTES: usize;
    fn get(self) -> i32;
    fn put(v: i32) -> Self;
}

impl Sample for u8 {
    const BYTES: usize = 1;
    #[inline(always)]
    fn get(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn put(v: i32) -> Self {
        v as u8
    }
}

impl Sample for u16 {
    const BYTES: usize = 2;
    #[inline(always)]
    fn get(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn put(v: i32) -> Self {
        v as u16
    }
}

pub(crate) fn samples<S: Sample>(p: &Plane) -> Result<&[S]> {
    let (head, body, tail) = unsafe { p.data.align_to::<S>() };
    if !head.is_empty() || !tail.is_empty() || !p.stride.is_multiple_of(S::BYTES) {
        return Err(Error::Invalid(
            "plane is not aligned to its sample size".into(),
        ));
    }
    Ok(body)
}

pub(crate) fn samples_mut<S: Sample>(p: &mut Plane) -> Result<&mut [S]> {
    if !p.stride.is_multiple_of(S::BYTES) {
        return Err(Error::Invalid(
            "plane is not aligned to its sample size".into(),
        ));
    }
    let (head, body, tail) = unsafe { p.data.align_to_mut::<S>() };
    if !head.is_empty() || !tail.is_empty() {
        return Err(Error::Invalid(
            "plane is not aligned to its sample size".into(),
        ));
    }
    Ok(body)
}

pub(crate) const MAX_SIDE: u32 = 1 << 15;

pub(crate) fn sized(width: u32, height: u32) -> Result<()> {
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::Invalid(format!(
            "{width}x{height} is larger than {MAX_SIDE} on a side"
        )));
    }
    Ok(())
}

pub(crate) fn span(stride: usize, rows: usize, row: usize) -> Option<usize> {
    if rows == 0 {
        return Some(0);
    }
    stride.checked_mul(rows - 1)?.checked_add(row)
}

pub(crate) fn check(frame: &Frame) -> Result<()> {
    sized(frame.width, frame.height)?;
    let bytes = frame.format.bytes_per_sample();
    for (i, p) in frame.planes.iter().enumerate() {
        let (pw, ph) = plane_dims(frame.width, frame.height, i);
        if p.width < pw || p.height < ph || p.stride < pw * bytes {
            return Err(Error::Invalid(format!(
                "plane {i} is smaller than the frame"
            )));
        }
        if span(p.stride, ph, pw * bytes).is_none_or(|n| p.data.len() < n) {
            return Err(Error::Invalid(format!("plane {i} data is too short")));
        }
    }
    Ok(())
}

pub(crate) fn reshape(
    frame: &mut Frame,
    format: PixelFormat,
    width: u32,
    height: u32,
    pts: i64,
    time_base: TimeBase,
) {
    let bytes = format.bytes_per_sample();
    for (i, p) in frame.planes.iter_mut().enumerate() {
        let (w, h) = plane_dims(width, height, i);
        p.width = w;
        p.height = h;
        p.stride = w * bytes;
        p.data.resize(w * bytes * h, 0);
    }
    frame.format = format;
    frame.width = width;
    frame.height = height;
    frame.pts = pts;
    frame.time_base = time_base;
}

pub(crate) fn plane_dims(width: u32, height: u32, index: usize) -> (usize, usize) {
    if index == 0 {
        (width as usize, height as usize)
    } else {
        (width.div_ceil(2) as usize, height.div_ceil(2) as usize)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(target_arch = "aarch64", allow(dead_code))]
pub(crate) enum Simd {
    Plain,
    #[cfg(target_arch = "x86_64")]
    Avx2,
    #[cfg(target_arch = "aarch64")]
    Neon,
}

pub(crate) fn simd() -> Simd {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            Simd::Avx2
        } else {
            Simd::Plain
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        Simd::Neon
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        Simd::Plain
    }
}
