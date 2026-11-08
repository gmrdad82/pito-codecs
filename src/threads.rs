use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static BELOW_NORMAL: AtomicBool = AtomicBool::new(false);
static ENTERED: AtomicUsize = AtomicUsize::new(0);

pub fn set_below_normal(on: bool) {
    BELOW_NORMAL.store(on, Ordering::Relaxed);
}

pub fn below_normal() -> bool {
    BELOW_NORMAL.load(Ordering::Relaxed)
}

pub fn entered() -> usize {
    ENTERED.load(Ordering::Relaxed)
}

pub fn enter() {
    if below_normal() {
        ENTERED.fetch_add(1, Ordering::Relaxed);
        platform::lower();
    }
}

#[cfg(target_os = "windows")]
mod platform {
    pub const BELOW_NORMAL: i32 = -1;
    unsafe extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(thread: isize, priority: i32) -> i32;
    }
    #[cfg(test)]
    unsafe extern "system" {
        fn GetThreadPriority(thread: isize) -> i32;
    }
    pub fn lower() {
        unsafe {
            SetThreadPriority(GetCurrentThread(), BELOW_NORMAL);
        }
    }
    #[cfg(test)]
    pub fn current() -> i32 {
        unsafe { GetThreadPriority(GetCurrentThread()) }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    pub fn lower() {}
    #[cfg(all(test, target_os = "linux"))]
    unsafe extern "C" {
        fn getpriority(which: i32, who: u32) -> i32;
        fn setpriority(which: i32, who: u32, prio: i32) -> i32;
    }
    #[cfg(all(test, target_os = "linux"))]
    pub fn current() -> i32 {
        unsafe { getpriority(0, 0) }
    }
    #[cfg(all(test, target_os = "linux"))]
    pub fn raise_nice(by: i32) {
        unsafe {
            setpriority(0, 0, current() + by);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn default_is_off_and_enter_counts_nothing() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_below_normal(false);
        let before = entered();
        std::thread::spawn(enter).join().unwrap();
        assert_eq!(entered(), before);
        assert!(!below_normal());
    }

    #[test]
    fn enter_counts_when_on() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_below_normal(true);
        let before = entered();
        std::thread::spawn(enter).join().unwrap();
        set_below_normal(false);
        assert_eq!(entered(), before + 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn child_threads_keep_the_creators_nice() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_below_normal(true);
        let parent = std::thread::spawn(|| {
            platform::raise_nice(5);
            let own = platform::current();
            let child = std::thread::spawn(|| {
                enter();
                platform::current()
            })
            .join()
            .unwrap();
            (own, child)
        })
        .join()
        .unwrap();
        set_below_normal(false);
        assert_eq!(parent.0, parent.1);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_thread_drops_to_below_normal() {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_below_normal(true);
        let level = std::thread::spawn(|| {
            enter();
            platform::current()
        })
        .join()
        .unwrap();
        set_below_normal(false);
        assert_eq!(level, platform::BELOW_NORMAL);
    }
}
