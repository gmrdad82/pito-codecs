mod color;
mod fps;
mod geometry;
mod limiter;
mod loudness;
mod mix;
mod overlay;
mod packed;
mod plane;
mod resample;
mod scale;

pub use color::{ColorSpec, Matrix, Range, Siting, convert_color};

#[deprecated(note = "use pito_codecs::media::Rgba")]
pub type Rgba = crate::media::Rgba;
pub use fps::FpsConverter;
pub use geometry::{Fill, Rotation, crop, flip_horizontal, flip_vertical, pad, rotate};
pub use limiter::{Limiter, normalize, normalize_gain_db};
pub use loudness::{Loudness, LoudnessMeter};
pub use mix::{downmix, gain};
pub use overlay::Overlay;
pub use packed::{
    Chroma, Order, Packed, PackedMut, packed_to_yuv, rgba_to_yuv, yuv_to_packed, yuv_to_rgba,
};
pub use resample::{Resampler, resample};
pub use scale::{Kernel, Scaler, scale};

#[cfg(test)]
mod tests;
