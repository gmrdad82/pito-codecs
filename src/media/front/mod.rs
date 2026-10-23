#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod alpha;
mod audio;
mod bare;
mod base;
mod demux;
#[cfg(all(
    test,
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod edges;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod export;
mod feed;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod frames;
#[cfg(feature = "gif")]
mod gif;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod join;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod level;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod look;
#[cfg(all(
    test,
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod malformed;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod mux;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod native;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod picture;
mod probe;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod resident;
mod route;
mod s16;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod shape;
mod sound;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod still;
mod stream;
mod variant;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod video;

#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
pub use alpha::{AlphaVideo, decode_alpha, export_rgba, to_rgba_alpha};
pub use audio::{audio_mix, audio_pcm, audio_pcm_range};
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
pub use export::{
    ExportOptions, Exported, Overlay, Part, Piece, Progress, Sessions, Stage, export,
    export_with_audio, parts,
};
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
pub use frames::{Canvas, export_frames};
#[cfg(feature = "gif")]
pub use gif::{
    GifFrame, GifInfo, decode_gif, decode_scanned, flatten_alpha, gif_frames, gif_info, gif_rate,
    is_gif, is_gif_file, scan_gif,
};
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
pub use join::{
    Cut, JoinAudio, JoinMethod, JoinOptions, Joined, cut, first_frame_at, join, track_length,
};
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
pub use level::{
    AAC_KBPS, CALIBRATION_PASSES, Calibration, DEFAULT_TRUE_PEAK, HIGHEST_TRUE_PEAK, LEVEL_RATE,
    LOUDEST_LUFS, LOWEST_TRUE_PEAK, LevelMode, LevelOptions, LevelPlan, LevelProgress, LevelStage,
    LevelStyle, LevelTarget, Leveler, Levelled, LevelledPcm, LoudnessStats, OPUS_KBPS,
    QUIETEST_LUFS, YOUTUBE_LUFS, YOUTUBE_TRUE_PEAK, level, level_pcm, level_with, measure_loudness,
    meter_loudness, plan_level, plan_level_span, plan_level_styled,
};
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
pub use look::{VideoLook, video_look};
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
pub use picture::{
    Blender, Color, blend, rotate, scale, scale_to_height, to_rgba, to_rgba_color, to_rgba_height,
};
pub use probe::{AudioTrack, Probe, frame_count, keyframes, probe};
pub use route::{Caps, Choice, Fallback, Picked, Prefs, pick_decode, pick_encode};
pub use s16::{HIGHEST_RATE, LOWEST_RATE, S16Span, decode_s16, to_s16};
pub use sound::Pcm;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
pub use still::{Seek, Stills, first_frame, frames_at, frames_at_seek};
pub use stream::{AudioSpan, AudioStream, audio_mix_stream, audio_stream, decode_audio_span};
pub use variant::Chroma;
#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
pub use video::{GpuVideo, IN_FLIGHT, Video, VideoFrame, decode_video, decode_video_gpu};
