mod types;

pub use types::*;

macro_rules! front {
    ($($item:item)*) => {
        $(
            #[cfg(all(
                feature = "h264",
                feature = "hevc",
                feature = "aac",
                feature = "mp4",
                feature = "gpu",
                feature = "filter"
            ))]
            $item
        )*
    };
}

front! {
    mod audio;
    mod export;
    mod ffmpeg;
    mod mux;
    mod native;
    mod picture;
    mod probe;
    mod route;
    mod still;
    mod video;

    pub use audio::{audio_mix, audio_pcm, audio_pcm_range};
    pub use export::{ExportOptions, Exported, Overlay, Part, Piece, Progress, Stage, export, parts};
    pub use ffmpeg::{Ffmpeg, Listed};
    pub use picture::{
        Color, blend, rotate, scale, scale_to_height, to_rgba, to_rgba_color, to_rgba_height,
    };
    pub use probe::{AudioTrack, Probe, keyframes, probe};
    pub use route::{Caps, Choice, Fallback, Picked, Prefs, UserFfmpeg, pick_decode, pick_encode};
    pub use still::{first_frame, frames_at};
    pub use video::{GpuVideo, IN_FLIGHT, Video, VideoFrame, decode_video, decode_video_gpu};
}
