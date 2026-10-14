use super::bits::Bits;
use super::tables::FRAME_WORDS;
use crate::media::{Error, Result};

pub const NFCHANS: [usize; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Independent,
    Dependent,
    Convert,
}

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub eac3: bool,
    pub kind: Kind,
    pub substream: u8,
    pub bytes: usize,
    pub fscod: u8,
    pub rate: u32,
    pub blocks: usize,
    pub acmod: u8,
    pub nfchans: usize,
    pub lfeon: bool,
    pub chanmap: Option<u16>,
    pub bits: usize,
}

impl Header {
    pub fn nchans(&self) -> usize {
        self.nfchans + self.lfeon as usize
    }
}

fn invalid(what: &str) -> Error {
    Error::Invalid(format!("AC-3 {what}"))
}

pub fn frame_len(data: &[u8]) -> Option<usize> {
    if data.len() < 6 || data[0] != 0x0B || data[1] != 0x77 {
        return None;
    }
    let bsid = data[5] >> 3;
    if bsid <= 10 {
        let fscod = (data[4] >> 6) as usize;
        let code = (data[4] & 0x3F) as usize;
        let words = FRAME_WORDS.get(code)?.get(fscod)?;
        Some(*words as usize * 2)
    } else {
        let frmsiz = (((data[2] & 7) as usize) << 8) | data[3] as usize;
        Some((frmsiz + 1) * 2)
    }
}

pub fn parse(data: &[u8]) -> Result<Header> {
    if data.len() < 6 {
        return Err(invalid("frame shorter than its header"));
    }
    if data[0] != 0x0B || data[1] != 0x77 {
        return Err(invalid("frame without a sync word"));
    }
    if data[5] >> 3 <= 10 {
        ac3(data)
    } else {
        eac3(data)
    }
}

fn ac3(data: &[u8]) -> Result<Header> {
    let fscod = data[4] >> 6;
    let code = (data[4] & 0x3F) as usize;
    let rate = match fscod {
        0 => 48_000,
        1 => 44_100,
        2 => 32_000,
        _ => return Err(invalid("frame with a reserved sample rate")),
    };
    let words = FRAME_WORDS
        .get(code)
        .ok_or_else(|| invalid("frame with a reserved frame size"))?[fscod as usize];
    let mut b = Bits::new(&data[5..]);
    b.skip(8)?;
    let acmod = b.read(3)? as u8;
    if acmod & 1 != 0 && acmod != 1 {
        b.skip(2)?;
    }
    if acmod & 4 != 0 {
        b.skip(2)?;
    }
    if acmod == 2 {
        b.skip(2)?;
    }
    let lfeon = b.flag()?;
    for _ in 0..if acmod == 0 { 2 } else { 1 } {
        b.skip(5)?;
        if b.flag()? {
            b.skip(8)?;
        }
        if b.flag()? {
            b.skip(8)?;
        }
        if b.flag()? {
            b.skip(7)?;
        }
    }
    b.skip(2)?;
    if b.flag()? {
        b.skip(14)?;
    }
    if b.flag()? {
        b.skip(14)?;
    }
    if b.flag()? {
        let n = b.read(6)? as usize + 1;
        b.skip(n * 8)?;
    }
    Ok(Header {
        eac3: false,
        kind: Kind::Independent,
        substream: 0,
        bytes: words as usize * 2,
        fscod,
        rate,
        blocks: 6,
        acmod,
        nfchans: NFCHANS[acmod as usize],
        lfeon,
        chanmap: None,
        bits: 40 + b.pos(),
    })
}

fn eac3(data: &[u8]) -> Result<Header> {
    let mut b = Bits::new(&data[2..]);
    let kind = match b.read(2)? {
        0 => Kind::Independent,
        1 => Kind::Dependent,
        2 => Kind::Convert,
        _ => return Err(invalid("E-AC-3 frame with a reserved stream type")),
    };
    let substream = b.read(3)? as u8;
    let bytes = (b.read(11)? as usize + 1) * 2;
    if bytes < 8 {
        return Err(invalid("E-AC-3 frame too small"));
    }
    let fscod = b.read(2)? as u8;
    let numblkscod = if fscod == 3 {
        return Err(Error::Unsupported(
            "E-AC-3 at a reduced sample rate (16 to 24 kHz)".into(),
        ));
    } else {
        b.read(2)? as u8
    };
    let rate = [48_000, 44_100, 32_000][fscod as usize];
    let blocks = [1, 2, 3, 6][numblkscod as usize];
    let acmod = b.read(3)? as u8;
    let lfeon = b.flag()?;
    let bsid = b.read(5)? as u8;
    if !(11..=16).contains(&bsid) {
        return Err(Error::Unsupported(format!("E-AC-3 bitstream id {bsid}")));
    }
    for _ in 0..if acmod == 0 { 2 } else { 1 } {
        b.skip(5)?;
        if b.flag()? {
            b.skip(8)?;
        }
    }
    let mut chanmap = None;
    if kind == Kind::Dependent && b.flag()? {
        chanmap = Some(b.read(16)? as u16);
    }
    if b.flag()? {
        mixing(&mut b, acmod, lfeon, kind, numblkscod)?;
    }
    if b.flag()? {
        b.skip(5)?;
        if acmod == 2 {
            b.skip(4)?;
        }
        if acmod >= 6 {
            b.skip(2)?;
        }
        for _ in 0..if acmod == 0 { 2 } else { 1 } {
            if b.flag()? {
                b.skip(8)?;
            }
        }
        if fscod < 3 {
            b.skip(1)?;
        }
    }
    if kind == Kind::Independent && numblkscod != 3 {
        b.skip(1)?;
    }
    if kind == Kind::Convert && (numblkscod == 3 || b.flag()?) {
        b.skip(6)?;
    }
    if b.flag()? {
        let n = b.read(6)? as usize + 1;
        b.skip(n * 8)?;
    }
    Ok(Header {
        eac3: true,
        kind,
        substream,
        bytes,
        fscod,
        rate,
        blocks,
        acmod,
        nfchans: NFCHANS[acmod as usize],
        lfeon,
        chanmap,
        bits: 16 + b.pos(),
    })
}

fn mixing(b: &mut Bits, acmod: u8, lfeon: bool, kind: Kind, numblkscod: u8) -> Result<()> {
    if acmod > 2 {
        b.skip(2)?;
    }
    if acmod & 1 != 0 && acmod > 2 {
        b.skip(6)?;
    }
    if acmod & 4 != 0 {
        b.skip(6)?;
    }
    if lfeon && b.flag()? {
        b.skip(5)?;
    }
    if kind != Kind::Independent {
        return Ok(());
    }
    if b.flag()? {
        b.skip(6)?;
    }
    if acmod == 0 && b.flag()? {
        b.skip(6)?;
    }
    if b.flag()? {
        b.skip(6)?;
    }
    match b.read(2)? {
        1 => b.skip(5)?,
        2 => b.skip(12)?,
        3 => {
            let len = b.read(5)? as usize;
            b.skip(8 * (len + 2) - 5)?;
        }
        _ => {}
    }
    if acmod < 2 {
        if b.flag()? {
            b.skip(14)?;
        }
        if acmod == 0 && b.flag()? {
            b.skip(14)?;
        }
    }
    if b.flag()? {
        if numblkscod == 0 {
            b.skip(5)?;
        } else {
            for _ in 0..[1, 2, 3, 6][numblkscod as usize] {
                if b.flag()? {
                    b.skip(5)?;
                }
            }
        }
    }
    Ok(())
}
