use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

unsafe extern "C" {
    fn memfd_create(name: *const c_char, flags: c_uint) -> c_int;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
}

const MFD_CLOEXEC: c_uint = 1;
const MFD_ALLOW_SEALING: c_uint = 2;
const F_ADD_SEALS: c_int = 1033;
const F_SEAL_SHRINK: c_int = 2;
const UDMABUF_FLAGS_CLOEXEC: u32 = 1;
const UDMABUF_CREATE: c_ulong = 0x4018_7542;
const PROT_READ: c_int = 1;
const MAP_SHARED: c_int = 1;
const PAGE: usize = 4096;

#[repr(C)]
struct UdmabufCreate {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

pub struct HostBuffer {
    _memory: File,
    dmabuf: OwnedFd,
    ptr: *mut u8,
    len: usize,
}

unsafe impl Send for HostBuffer {}
unsafe impl Sync for HostBuffer {}

fn os(what: &str) -> String {
    format!("{what}: {}", io::Error::last_os_error())
}

impl HostBuffer {
    pub fn new(len: usize) -> Result<Self, String> {
        let len = len
            .checked_next_multiple_of(PAGE)
            .filter(|&l| l > 0)
            .ok_or("host buffer size overflows")?;
        let fd = unsafe { memfd_create(c"pito-codecs".as_ptr(), MFD_CLOEXEC | MFD_ALLOW_SEALING) };
        if fd < 0 {
            return Err(os("memfd_create"));
        }
        let memory = unsafe { File::from_raw_fd(fd) };
        memory
            .set_len(len as u64)
            .map_err(|e| format!("memfd size: {e}"))?;
        if unsafe { fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK) } < 0 {
            return Err(os("memfd seal"));
        }
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/udmabuf")
            .map_err(|e| format!("/dev/udmabuf: {e}"))?;
        let create = UdmabufCreate {
            memfd: u32::try_from(fd).map_err(|_| "memfd out of range")?,
            flags: UDMABUF_FLAGS_CLOEXEC,
            offset: 0,
            size: len as u64,
        };
        let buf = unsafe { ioctl(device.as_raw_fd(), UDMABUF_CREATE, &create) };
        if buf < 0 {
            return Err(os("UDMABUF_CREATE"));
        }
        let dmabuf = unsafe { OwnedFd::from_raw_fd(buf) };
        let ptr = unsafe { mmap(std::ptr::null_mut(), len, PROT_READ, MAP_SHARED, fd, 0) };
        if ptr.is_null() || ptr as usize == usize::MAX {
            return Err(os("mmap"));
        }
        Ok(Self {
            _memory: memory,
            dmabuf,
            ptr: ptr.cast(),
            len,
        })
    }

    pub fn fd(&self) -> c_int {
        self.dmabuf.as_raw_fd()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn data(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for HostBuffer {
    fn drop(&mut self) {
        unsafe { munmap(self.ptr.cast(), self.len) };
    }
}
