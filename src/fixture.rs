use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

pub(crate) fn unique(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{}-{n}.part.{name}", std::process::id()))
}

fn lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>> = Mutex::new(BTreeMap::new());
    LOCKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(path.to_path_buf())
        .or_default()
        .clone()
}

fn remove(path: &Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

pub(crate) fn once(path: &Path, make: impl FnOnce(&Path)) -> PathBuf {
    once_all(&[path], |parts| make(&parts[0]));
    path.to_path_buf()
}

pub(crate) fn once_all(paths: &[&Path], make: impl FnOnce(&[PathBuf])) {
    build(paths, false, make);
}

pub(crate) fn fresh(path: &Path, make: impl FnOnce(&Path)) -> PathBuf {
    build(&[path], true, |parts| make(&parts[0]));
    path.to_path_buf()
}

fn build(paths: &[&Path], fresh: bool, make: impl FnOnce(&[PathBuf])) {
    static BUILT: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
    let mut order = paths.to_vec();
    order.sort();
    order.dedup();
    let locks: Vec<_> = order.iter().map(|p| lock(p)).collect();
    let _held: Vec<_> = locks
        .iter()
        .map(|l| l.lock().unwrap_or_else(PoisonError::into_inner))
        .collect();
    let built = |p: &&Path| {
        BUILT
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(*p)
    };
    if paths.iter().all(|p| p.exists() && (!fresh || built(p))) {
        return;
    }
    let parts: Vec<PathBuf> = paths.iter().map(|p| unique(p)).collect();
    make(&parts);
    if !parts.iter().all(|p| p.exists()) {
        parts.iter().for_each(|p| remove(p));
        return;
    }
    for (part, path) in parts.iter().zip(paths) {
        if let Err(e) = std::fs::rename(part, path) {
            remove(part);
            assert!(path.exists(), "{}: {e}", path.display());
        }
    }
    if fresh {
        let mut done = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
        done.extend(paths.iter().map(|p| p.to_path_buf()));
    }
}
