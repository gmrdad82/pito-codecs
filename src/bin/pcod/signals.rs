use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

pub static STOP: AtomicBool = AtomicBool::new(false);
static CAUGHT: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
mod imp {
    use std::ffi::c_int;
    use std::sync::atomic::Ordering;

    unsafe extern "C" {
        fn signal(signum: c_int, handler: usize) -> usize;
    }

    extern "C" fn on_signal(signum: c_int) {
        super::CAUGHT.store(signum, Ordering::SeqCst);
        super::STOP.store(true, Ordering::SeqCst);
    }

    pub fn catch() {
        let handler = on_signal as extern "C" fn(c_int) as usize;
        for signum in [1, 2, 15] {
            unsafe {
                signal(signum, handler);
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::Ordering;
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows::core::BOOL;

    unsafe extern "system" fn on_ctrl(_kind: u32) -> BOOL {
        super::CAUGHT.store(2, Ordering::SeqCst);
        super::STOP.store(true, Ordering::SeqCst);
        BOOL(1)
    }

    pub fn catch() {
        unsafe {
            let _ = SetConsoleCtrlHandler(Some(on_ctrl), true);
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    pub fn catch() {}
}

pub fn catch() {
    imp::catch();
}

pub fn caught() -> Option<(u8, String)> {
    let signum = CAUGHT.load(Ordering::SeqCst);
    if signum == 0 {
        return None;
    }
    let name = if cfg!(windows) {
        "Ctrl-C".to_string()
    } else {
        match signum {
            1 => "SIGHUP".to_string(),
            2 => "SIGINT".to_string(),
            15 => "SIGTERM".to_string(),
            n => format!("signal {n}"),
        }
    };
    Some((
        (128 + signum).clamp(1, 255) as u8,
        format!("interrupted by {name}"),
    ))
}
