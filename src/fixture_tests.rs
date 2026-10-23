use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crate::fixture::{fresh, once, once_all, unique};

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/fixture");
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn threads_asking_for_one_fixture_build_it_once_and_all_read_it_whole() {
    let path = dir().join(format!("shared-{}.bin", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let built = std::sync::atomic::AtomicUsize::new(0);
    let start = std::sync::Barrier::new(8);
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                start.wait();
                once(&path, |part| {
                    built.fetch_add(1, Ordering::SeqCst);
                    std::fs::write(part, vec![7u8; 1 << 20]).unwrap();
                });
                assert_eq!(std::fs::read(&path).unwrap(), vec![7u8; 1 << 20]);
            });
        }
    });
    assert_eq!(built.load(Ordering::SeqCst), 1);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn parts_are_unique_per_call_and_keep_the_extension() {
    let path = dir().join("clip.mp4");
    let a = unique(&path);
    let b = unique(&path);
    assert_ne!(a, b);
    assert_eq!(a.extension().unwrap(), "mp4");
    assert_eq!(a.parent(), path.parent());
}

#[test]
fn a_builder_that_panics_leaves_nothing_and_the_next_caller_builds() {
    let path = dir().join(format!("retry-{}.bin", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let failed = std::panic::catch_unwind(|| once(&path, |_| panic!("ffmpeg failed")));
    assert!(failed.is_err());
    assert!(!path.exists());
    once(&path, |part| std::fs::write(part, b"ok").unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"ok");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_builder_that_writes_nothing_leaves_the_path_missing_and_the_next_caller_builds() {
    let path = dir().join(format!("skip-{}.bin", std::process::id()));
    let _ = std::fs::remove_file(&path);
    once(&path, |_| {});
    assert!(!path.exists());
    once(&path, |part| std::fs::write(part, b"late").unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"late");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_fresh_fixture_is_rebuilt_once_per_process_over_a_stale_file() {
    let path = dir().join(format!("fresh-{}.bin", std::process::id()));
    std::fs::write(&path, b"stale").unwrap();
    fresh(&path, |part| std::fs::write(part, b"new").unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    fresh(&path, |_| panic!("rebuilt twice in one process"));
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_cached_fixture_keeps_an_existing_file() {
    let path = dir().join(format!("kept-{}.bin", std::process::id()));
    std::fs::write(&path, b"kept").unwrap();
    once(&path, |_| panic!("rebuilt an existing fixture"));
    assert_eq!(std::fs::read(&path).unwrap(), b"kept");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn several_outputs_land_together_and_an_existing_set_is_kept() {
    let a = dir().join(format!("pair-{}.hevc", std::process::id()));
    let b = dir().join(format!("pair-{}.yuv", std::process::id()));
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    once_all(&[&a, &b], |parts| {
        std::fs::write(&parts[0], b"stream").unwrap();
        std::fs::write(&parts[1], b"frames").unwrap();
    });
    assert_eq!(std::fs::read(&a).unwrap(), b"stream");
    assert_eq!(std::fs::read(&b).unwrap(), b"frames");
    once_all(&[&a, &b], |_| panic!("rebuilt an existing set"));
    std::fs::remove_file(&a).unwrap();
    std::fs::remove_file(&b).unwrap();
}

#[test]
fn a_part_that_cannot_rename_onto_a_built_path_is_removed_and_the_path_kept() {
    let path = dir().join(format!("busy-{}.d", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("kept"), b"kept").unwrap();
    let mut part = PathBuf::new();
    fresh(&path, |p| {
        std::fs::create_dir_all(p).unwrap();
        std::fs::write(p.join("new"), b"new").unwrap();
        part = p.to_path_buf();
    });
    assert!(!part.exists());
    assert_eq!(std::fs::read(path.join("kept")).unwrap(), b"kept");
    assert!(!path.join("new").exists());
    std::fs::remove_dir_all(&path).unwrap();
}

#[test]
fn a_directory_part_lands_whole() {
    let path = dir().join(format!("tree-{}.d", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    once(&path, |p| {
        std::fs::create_dir_all(p).unwrap();
        std::fs::write(p.join("a"), b"a").unwrap();
    });
    assert_eq!(std::fs::read(path.join("a")).unwrap(), b"a");
    std::fs::remove_dir_all(&path).unwrap();
}
