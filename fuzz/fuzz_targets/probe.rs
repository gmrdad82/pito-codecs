#![no_main]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use libfuzzer_sys::fuzz_target;
use pito_codecs::media::{Prefs, audio_pcm_range, first_frame, frame_count, keyframes, probe};

static COUNT: AtomicU64 = AtomicU64::new(0);

fn scratch() -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("pito-codecs-fuzz-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fuzz_target!(|data: &[u8]| {
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let mut path = scratch();
    path.push(format!("{n}.bin"));
    if std::fs::write(&path, data).is_err() {
        return;
    }
    let prefs = Prefs::default();
    let _ = probe(&path, &prefs);
    let _ = keyframes(&path, &prefs);
    let _ = frame_count(&path, &prefs);
    let _ = first_frame(&path, 32, &prefs);
    let _ = audio_pcm_range(&path, 0, 0.0, 0.5, 48000, 2, &prefs);
    let _ = std::fs::remove_file(&path);
});
