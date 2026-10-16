use std::ffi::{c_char, c_uint, c_void};
use std::fmt;
use std::fmt::Write as _;
use std::io::{Write as _, stderr, stdout};
use std::ptr::fn_addr_eq;

use crate::av1::cpu::send_sync_non_null::SendSyncNonNull;

pub type Dav1dLoggerCallback =
    unsafe extern "C" fn(cookie: Option<SendSyncNonNull<c_void>>, fmt: *const c_char, args: ...);

#[derive(Clone)]
#[repr(C)]
pub struct Dav1dLogger {
    cookie: Option<SendSyncNonNull<c_void>>,

    callback: Option<Dav1dLoggerCallback>,
}

impl Dav1dLogger {
    pub const unsafe fn new(
        cookie: Option<SendSyncNonNull<c_void>>,
        callback: Option<Dav1dLoggerCallback>,
    ) -> Self {
        Self { cookie, callback }
    }
}

impl fmt::Write for Dav1dLogger {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let callback = self.callback.unwrap();

        let fmt = c"%c";
        for &byte in s.as_bytes() {
            unsafe {
                callback(self.cookie, fmt.as_ptr(), byte as c_uint);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) enum Rav1dLogger {
    Dav1d(Dav1dLogger),
    Stdout,
    #[default]
    Stderr,
}

pub trait Rav1dLog {
    fn write_fmt(&self, args: fmt::Arguments);
}

impl Rav1dLog for Rav1dLogger {
    #[inline(never)]
    fn write_fmt(&self, args: fmt::Arguments) {
        match self {
            Self::Dav1d(dav1d) => dav1d.clone().write_fmt(args).unwrap(),
            Self::Stdout => stdout().write_fmt(args).unwrap(),
            Self::Stderr => stderr().write_fmt(args).unwrap(),
        }
    }
}

impl Rav1dLog for Option<Rav1dLogger> {
    #[inline(always)]
    fn write_fmt(&self, args: fmt::Arguments) {
        if let Some(logger) = self {
            logger.write_fmt(args);
        }
    }
}

mod marker {
    use super::*;

    type Callback = extern "C" fn(cookie: *mut c_void, fmt: *const c_char);

    const fn cast(callback: Callback) -> Dav1dLoggerCallback {
        unsafe { std::mem::transmute(callback) }
    }

    pub const STDOUT: Dav1dLoggerCallback = {
        extern "C" fn stdout(_cookie: *mut c_void, _fmt: *const c_char) {
            unimplemented!(stringify!(stdout));
        }
        cast(stdout)
    };

    pub const STDERR: Dav1dLoggerCallback = {
        extern "C" fn stderr(_cookie: *mut c_void, _fmt: *const c_char) {
            unimplemented!(stringify!(stderr));
        }
        cast(stderr)
    };
}

impl From<Dav1dLogger> for Option<Rav1dLogger> {
    fn from(logger: Dav1dLogger) -> Self {
        let Dav1dLogger { cookie, callback } = logger;
        Some(match callback {
            None => return None,
            Some(cb) if fn_addr_eq(cb, marker::STDOUT) => Rav1dLogger::Stdout,
            Some(cb) if fn_addr_eq(cb, marker::STDERR) => Rav1dLogger::Stderr,
            _ => Rav1dLogger::Dav1d(Dav1dLogger { cookie, callback }),
        })
    }
}

impl From<Option<Rav1dLogger>> for Dav1dLogger {
    fn from(logger: Option<Rav1dLogger>) -> Self {
        let cookie = match &logger {
            Some(Rav1dLogger::Dav1d(dav1d)) => dav1d.cookie,
            _ => None,
        };
        let callback = logger.and_then(|logger| match logger {
            Rav1dLogger::Dav1d(dav1d) => dav1d.callback,
            Rav1dLogger::Stdout => Some(marker::STDOUT),
            Rav1dLogger::Stderr => Some(marker::STDERR),
        });
        Self { cookie, callback }
    }
}
