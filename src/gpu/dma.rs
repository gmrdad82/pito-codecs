use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::mem::ManuallyDrop;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub const DRM_FORMAT_NV12: u32 = u32::from_le_bytes(*b"NV12");
pub const DRM_FORMAT_ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
pub const DRM_FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
pub const DRM_FORMAT_ABGR8888: u32 = u32::from_le_bytes(*b"AB24");
pub const DRM_FORMAT_XBGR8888: u32 = u32::from_le_bytes(*b"XB24");

pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

const READY_TIMEOUT_MS: i32 = 10_000;

pub enum DmaReady {
    Finished,
    SyncFile(OwnedFd),
}

pub struct DmaImagePlane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub pitch: u32,
}

pub struct DmaImage {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub modifier: u64,
    pub planes: Vec<DmaImagePlane>,
    pub ready: DmaReady,
    pub device: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct Object {
    pub fd: RawFd,
    pub size: u32,
}

#[derive(Debug)]
pub(crate) struct Layout {
    pub objects: Vec<Object>,
    pub plane_object: Vec<usize>,
}

impl DmaImage {
    pub fn from_export(buf: &super::DmaBuf, ready: DmaReady) -> Result<Self, String> {
        let [layer] = buf.layers.as_slice() else {
            return Err(format!(
                "the dmabuf has {} layers; export it with composed layers",
                buf.layers.len()
            ));
        };
        let mut planes = Vec::with_capacity(layer.planes.len());
        let mut modifier = DRM_FORMAT_MOD_INVALID;
        for (i, p) in layer.planes.iter().enumerate() {
            let object = buf
                .objects
                .get(p.object)
                .ok_or_else(|| format!("plane {i} names object {}, which is missing", p.object))?;
            if i == 0 {
                modifier = object.modifier;
            }
            planes.push(DmaImagePlane {
                fd: object.fd.try_clone().map_err(|e| e.to_string())?,
                offset: p.offset,
                pitch: p.pitch,
            });
        }
        Ok(Self {
            fourcc: layer.drm_format,
            width: buf.crop.2,
            height: buf.crop.3,
            modifier,
            planes,
            ready,
            device: None,
        })
    }

    pub(crate) fn is_yuv(&self) -> bool {
        self.fourcc == DRM_FORMAT_NV12
    }

    pub(crate) fn layout(&self) -> Result<Layout, String> {
        let (planes, bytes) = match self.fourcc {
            DRM_FORMAT_NV12 => (2, 1),
            DRM_FORMAT_ARGB8888 | DRM_FORMAT_XRGB8888 | DRM_FORMAT_ABGR8888
            | DRM_FORMAT_XBGR8888 => (1, 4),
            other => {
                return Err(format!(
                    "the dmabuf format {} is not taken; NV12, ARGB8888, XRGB8888, ABGR8888 and XBGR8888 are",
                    fourcc_name(other)
                ));
            }
        };
        if self.width == 0 || self.height == 0 {
            return Err("a dmabuf of no size".into());
        }
        if self.is_yuv() && (!self.width.is_multiple_of(2) || !self.height.is_multiple_of(2)) {
            return Err(format!(
                "an NV12 dmabuf of {}x{}; the width and height must be even",
                self.width, self.height
            ));
        }
        if self.planes.len() != planes {
            return Err(format!(
                "{} dmabuf needs {planes} plane(s), got {}",
                fourcc_name(self.fourcc),
                self.planes.len()
            ));
        }
        let need = u64::from(self.width) * bytes;
        for (i, p) in self.planes.iter().enumerate() {
            if u64::from(p.pitch) < need {
                return Err(format!(
                    "plane {i} has a pitch of {} bytes, a row needs {need}",
                    p.pitch
                ));
            }
        }
        let mut objects: Vec<Object> = Vec::new();
        let mut identities: Vec<(u64, u64)> = Vec::new();
        let mut plane_object = Vec::with_capacity(self.planes.len());
        for (i, p) in self.planes.iter().enumerate() {
            let (identity, length) = identify(p.fd.as_raw_fd())
                .map_err(|e| format!("plane {i}'s file descriptor is not usable: {e}"))?;
            let at = match identities.iter().position(|k| *k == identity) {
                Some(at) => at,
                None => {
                    identities.push(identity);
                    objects.push(Object {
                        fd: p.fd.as_raw_fd(),
                        size: 0,
                    });
                    objects.len() - 1
                }
            };
            let rows = if i == 0 {
                u64::from(self.height)
            } else {
                u64::from(self.height / 2)
            };
            let end = u64::from(p.offset) + u64::from(p.pitch) * rows;
            let known = length.max(end);
            let size = u32::try_from(known)
                .map_err(|_| format!("plane {i}'s buffer is larger than 4 GiB"))?;
            objects[at].size = objects[at].size.max(size);
            plane_object.push(at);
        }
        Ok(Layout {
            objects,
            plane_object,
        })
    }

    pub(crate) fn wait_ready(&self) -> Result<(), String> {
        match &self.ready {
            DmaReady::Finished => Ok(()),
            DmaReady::SyncFile(fd) => wait_readable(fd.as_raw_fd(), READY_TIMEOUT_MS),
        }
    }
}

pub fn fourcc_name(fourcc: u32) -> String {
    let b = fourcc.to_le_bytes();
    if b.iter().all(|c| c.is_ascii_graphic()) {
        String::from_utf8_lossy(&b).into_owned()
    } else {
        format!("{fourcc:#010x}")
    }
}

fn identify(fd: RawFd) -> Result<((u64, u64), u64), String> {
    use std::os::unix::fs::MetadataExt;
    if fd < 0 {
        return Err("negative".into());
    }
    let file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    let meta = file.metadata().map_err(|e| e.to_string())?;
    let length = (&*file).seek(SeekFrom::End(0)).unwrap_or(0);
    Ok(((meta.dev(), meta.ino()), length))
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: u64, timeout: i32) -> i32;
}

pub(crate) fn wait_readable(fd: RawFd, timeout_ms: i32) -> Result<(), String> {
    const POLLIN: i16 = 1;
    loop {
        let mut p = PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        };
        let n = unsafe { poll(&mut p, 1, timeout_ms) };
        if n > 0 {
            return Ok(());
        }
        if n == 0 {
            return Err(format!(
                "the caller's rendering was not signalled within {} ms",
                timeout_ms
            ));
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!("waiting on the sync file: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    fn memory() -> OwnedFd {
        OwnedFd::from(File::open("/dev/null").unwrap())
    }

    fn image(fourcc: u32, planes: Vec<DmaImagePlane>) -> DmaImage {
        DmaImage {
            fourcc,
            width: 64,
            height: 32,
            modifier: DRM_FORMAT_MOD_LINEAR,
            planes,
            ready: DmaReady::Finished,
            device: None,
        }
    }

    fn plane(fd: OwnedFd, offset: u32, pitch: u32) -> DmaImagePlane {
        DmaImagePlane { fd, offset, pitch }
    }

    #[test]
    fn a_sync_file_that_is_readable_is_ready_and_one_that_is_not_times_out() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let fd = a.as_raw_fd();
        assert!(wait_readable(fd, 0).unwrap_err().contains("not signalled"));
        b.write_all(&[1]).unwrap();
        wait_readable(fd, 0).unwrap();
        let mut done = image(DRM_FORMAT_XRGB8888, vec![plane(memory(), 0, 256)]);
        done.ready = DmaReady::SyncFile(OwnedFd::from(a));
        done.wait_ready().unwrap();
    }

    #[test]
    fn an_unusable_description_is_refused_by_name() {
        let tiled = image(u32::from_le_bytes(*b"P010"), vec![plane(memory(), 0, 256)]);
        assert!(tiled.layout().unwrap_err().contains("P010"));
        let one = image(DRM_FORMAT_NV12, vec![plane(memory(), 0, 64)]);
        assert!(one.layout().unwrap_err().contains("2 plane"));
        let narrow = image(DRM_FORMAT_XBGR8888, vec![plane(memory(), 0, 100)]);
        assert!(narrow.layout().unwrap_err().contains("pitch"));
        let mut odd = image(
            DRM_FORMAT_NV12,
            vec![plane(memory(), 0, 64), plane(memory(), 2048, 64)],
        );
        odd.width = 63;
        assert!(odd.layout().unwrap_err().contains("even"));
        let mut empty = image(DRM_FORMAT_XBGR8888, vec![plane(memory(), 0, 256)]);
        empty.height = 0;
        assert!(empty.layout().unwrap_err().contains("no size"));
    }

    #[test]
    fn planes_on_one_buffer_share_an_object_and_sizes_cover_every_plane() {
        let first = memory();
        let second = first.try_clone().unwrap();
        let nv12 = image(
            DRM_FORMAT_NV12,
            vec![plane(first, 0, 64), plane(second, 2048, 64)],
        );
        let layout = nv12.layout().unwrap();
        assert_eq!(layout.objects.len(), 1);
        assert_eq!(layout.plane_object, vec![0, 0]);
        assert_eq!(layout.objects[0].size, 2048 + 64 * 16);
        let split = image(
            DRM_FORMAT_NV12,
            vec![
                plane(File::open("/dev/null").unwrap().into(), 0, 64),
                plane(File::open("/dev/zero").unwrap().into(), 0, 64),
            ],
        );
        let layout = split.layout().unwrap();
        assert_eq!(layout.objects.len(), 2);
        assert_eq!(layout.plane_object, vec![0, 1]);
        assert_eq!(layout.objects[0].size, 64 * 32);
        assert_eq!(layout.objects[1].size, 64 * 16);
    }

    #[test]
    fn four_character_codes_read_back_by_name() {
        assert_eq!(fourcc_name(DRM_FORMAT_NV12), "NV12");
        assert_eq!(fourcc_name(DRM_FORMAT_ABGR8888), "AB24");
        assert_eq!(fourcc_name(1), "0x00000001");
    }
}
