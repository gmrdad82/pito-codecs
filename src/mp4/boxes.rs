use crate::media::{Error, Result};
use crate::mp4::truncated;
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct FourCc(pub [u8; 4]);

impl FourCc {
    pub const fn new(b: &[u8; 4]) -> Self {
        FourCc(*b)
    }
    pub fn as_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }
    pub fn to_string_lossy(self) -> String {
        self.0
            .iter()
            .map(|&b| match b {
                0xA9 => '©',
                0x20..=0x7E => b as char,
                _ => '?',
            })
            .collect()
    }
}

impl fmt::Display for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_lossy())
    }
}
impl fmt::Debug for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FourCc({:?})", self.to_string_lossy())
    }
}
impl From<&[u8; 4]> for FourCc {
    fn from(b: &[u8; 4]) -> Self {
        FourCc(*b)
    }
}
impl PartialEq<[u8; 4]> for FourCc {
    fn eq(&self, other: &[u8; 4]) -> bool {
        &self.0 == other
    }
}
impl PartialEq<&[u8; 4]> for FourCc {
    fn eq(&self, other: &&[u8; 4]) -> bool {
        &self.0 == *other
    }
}
impl PartialEq<&&[u8; 4]> for FourCc {
    fn eq(&self, other: &&&[u8; 4]) -> bool {
        &self.0 == **other
    }
}

#[derive(Clone)]
pub(crate) struct Cur<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    what: &'static str,
}

impl<'a> Cur<'a> {
    pub fn new(data: &'a [u8], what: &'static str) -> Self {
        Cur { data, pos: 0, what }
    }
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    pub fn rest(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or(&[])
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| truncated(self.what))?;
        let s = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| truncated(self.what))?;
        self.pos = end;
        Ok(s)
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.bytes(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }
    pub fn u24(&mut self) -> Result<u32> {
        let a = self.array::<3>()?;
        Ok(((a[0] as u32) << 16) | ((a[1] as u32) << 8) | a[2] as u32)
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }
    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.array()?))
    }
    pub fn fourcc(&mut self) -> Result<FourCc> {
        Ok(FourCc(self.array()?))
    }
    pub fn full_header(&mut self) -> Result<(u8, u32)> {
        let v = self.u32()?;
        Ok(((v >> 24) as u8, v & 0x00FF_FFFF))
    }
    pub fn count(&mut self, entry_size: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        self.check_count(n, entry_size)?;
        Ok(n)
    }
    pub fn check_count(&self, n: usize, entry_size: usize) -> Result<()> {
        match n.checked_mul(entry_size) {
            Some(b) if b <= self.remaining() => Ok(()),
            _ => Err(truncated(self.what)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BoxHeader {
    pub kind: FourCc,
    pub header_len: u64,
    pub size: Option<u64>,
}

pub(crate) fn parse_box_header(buf: &[u8]) -> Result<BoxHeader> {
    let mut c = Cur::new(buf, "box header");
    let size32 = c.u32()?;
    let kind = c.fourcc()?;
    let mut header_len = 8u64;
    let size = match size32 {
        0 => None,
        1 => {
            header_len += 8;
            let s = c.u64()?;
            Some(s)
        }
        s => Some(s as u64),
    };
    if kind == b"uuid" {
        header_len += 16;
        c.array::<16>()?;
    }
    if let Some(s) = size
        && s < header_len
    {
        return Err(Error::Invalid(format!(
            "box '{kind}' size {s} smaller than its header"
        )));
    }
    Ok(BoxHeader {
        kind,
        header_len,
        size,
    })
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RawBox<'a> {
    pub kind: FourCc,
    pub payload: &'a [u8],
}

pub(crate) struct Boxes<'a> {
    data: &'a [u8],
    pos: usize,
    done: bool,
}

pub(crate) fn boxes(data: &[u8]) -> Boxes<'_> {
    Boxes {
        data,
        pos: 0,
        done: false,
    }
}

impl<'a> Iterator for Boxes<'a> {
    type Item = Result<RawBox<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let rest = &self.data[self.pos..];
        if rest.is_empty() {
            self.done = true;
            return None;
        }
        if rest.len() < 8 {
            self.done = true;
            if rest.iter().all(|&b| b == 0) {
                return None;
            }
            return Some(Err(truncated("box header")));
        }
        let h = match parse_box_header(rest) {
            Ok(h) => h,
            Err(e) => {
                self.done = true;
                return Some(Err(e));
            }
        };
        let size = h.size.unwrap_or(rest.len() as u64);
        if size > rest.len() as u64 {
            self.done = true;
            return Some(Err(Error::Invalid(format!(
                "box '{}' size {} exceeds its container ({} bytes left)",
                h.kind,
                size,
                rest.len()
            ))));
        }
        let size = size as usize;
        let payload = &rest[h.header_len as usize..size];
        self.pos += size;
        Some(Ok(RawBox {
            kind: h.kind,
            payload,
        }))
    }
}

pub(crate) fn find<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data)
        .map_while(|b| b.ok())
        .find(|b| b.kind == kind)
        .map(|b| b.payload)
}

#[derive(Default, Clone, Debug)]
pub(crate) struct BoxBuf {
    pub buf: Vec<u8>,
}

impl BoxBuf {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn start(&mut self, kind: &[u8; 4]) -> usize {
        let at = self.buf.len();
        self.buf.extend_from_slice(&[0, 0, 0, 0]);
        self.buf.extend_from_slice(kind);
        at
    }
    pub fn start_full(&mut self, kind: &[u8; 4], version: u8, flags: u32) -> usize {
        let at = self.start(kind);
        self.u32(((version as u32) << 24) | (flags & 0xFF_FFFF));
        at
    }
    pub fn end(&mut self, at: usize) {
        let size = (self.buf.len() - at) as u32;
        self.buf[at..at + 4].copy_from_slice(&size.to_be_bytes());
    }
    pub fn leaf(&mut self, kind: &[u8; 4], payload: &[u8]) {
        let at = self.start(kind);
        self.bytes(payload);
        self.end(at);
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
    pub fn zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0);
    }
    pub fn len(&self) -> usize {
        self.buf.len()
    }
}
