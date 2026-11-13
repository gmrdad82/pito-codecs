use std::io;
use std::sync::Arc;

pub trait ByteSource {
    fn len(&self) -> u64;
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "read past end of byte source")
}

impl ByteSource for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| eof())?;
        let end = start.checked_add(buf.len()).ok_or_else(eof)?;
        let src = self.get(start..end).ok_or_else(eof)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

impl ByteSource for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.as_slice().read_at(offset, buf)
    }
}

impl<T: ByteSource + ?Sized> ByteSource for &T {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

impl<T: ByteSource + ?Sized> ByteSource for Arc<T> {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

impl<T: ByteSource + ?Sized> ByteSource for Box<T> {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ByteSource for std::fs::File {
    fn len(&self) -> u64 {
        self.metadata().map(|m| m.len()).unwrap_or(0)
    }
    #[cfg(unix)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        #[cfg(test)]
        trace::record(self, offset, buf.len() as u64);
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
    }
    #[cfg(windows)]
    fn read_at(&self, mut offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
        use std::os::windows::fs::FileExt;
        while !buf.is_empty() {
            let n = self.seek_read(buf, offset)?;
            if n == 0 {
                return Err(eof());
            }
            offset += n as u64;
            buf = &mut buf[n..];
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = self;
        f.seek(SeekFrom::Start(offset))?;
        f.read_exact(buf)
    }
}

#[cfg(all(test, unix))]
pub(crate) mod trace {
    use std::os::unix::fs::MetadataExt;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static WATCHING: AtomicUsize = AtomicUsize::new(0);
    static READS: Mutex<Vec<(u64, u64, u64)>> = Mutex::new(Vec::new());

    pub(crate) fn record(f: &std::fs::File, offset: u64, len: u64) {
        if WATCHING.load(Ordering::Relaxed) == 0 {
            return;
        }
        if let Ok(m) = f.metadata() {
            let mut reads = READS.lock().unwrap_or_else(|e| e.into_inner());
            reads.push((m.ino(), offset, len));
        }
    }

    pub(crate) struct Watch(u64);

    impl Watch {
        pub(crate) fn new(path: &std::path::Path) -> Watch {
            WATCHING.fetch_add(1, Ordering::Relaxed);
            Watch(std::fs::metadata(path).map(|m| m.ino()).unwrap_or(0))
        }

        pub(crate) fn reads(&self) -> Vec<(u64, u64)> {
            READS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|r| r.0 == self.0)
                .map(|r| (r.1, r.2))
                .collect()
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            WATCHING.fetch_sub(1, Ordering::Relaxed);
        }
    }
}
