mod alpha;
mod anim;
mod lossless;
#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
mod oracle;
mod riff;
#[cfg(all(test, target_os = "linux"))]
mod tests;
mod vp8;

#[deprecated(note = "use pito_codecs::media::Rgba")]
pub type Rgba = crate::media::Rgba;
pub use anim::{AnimDecoder, AnimEncoder, Animation, Frame, FrameRef, decode_animation};

use crate::media::{self, Result};
use riff::{Chunks, invalid, u24};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub animated: bool,
    pub lossless: bool,
    pub frames: u32,
    pub loop_count: u16,
    pub background: [u8; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    pub lossless: bool,
    pub quality: f32,
    pub effort: u8,
    pub exact: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            lossless: false,
            quality: 75.0,
            effort: 4,
            exact: false,
        }
    }
}

impl Options {
    pub fn lossless() -> Self {
        Options {
            lossless: true,
            ..Options::default()
        }
    }

    pub fn lossy(quality: f32) -> Self {
        Options {
            quality,
            ..Options::default()
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Coded<'a> {
    Lossy {
        data: &'a [u8],
        alpha: Option<&'a [u8]>,
    },
    Lossless(&'a [u8]),
}

#[derive(Clone, Copy)]
pub(crate) struct Piece<'a> {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub duration: u32,
    pub blend: bool,
    pub dispose: bool,
    pub alpha: bool,
    pub coded: Coded<'a>,
}

pub(crate) struct Parsed<'a> {
    pub info: Info,
    pub pieces: Vec<Piece<'a>>,
}

pub fn info(data: &[u8]) -> Result<Info> {
    Ok(parse(data)?.info)
}

pub fn decode(data: &[u8]) -> Result<media::Rgba> {
    let parsed = parse(data)?;
    if parsed.info.animated {
        let mut dec = AnimDecoder::from_parsed(parsed);
        return match dec.next_frame()? {
            Some(f) => Ok(f.image.clone()),
            None => Err(invalid("animation without frames")),
        };
    }
    let piece = parsed.pieces[0];
    let mut out = media::Rgba::new(piece.width, piece.height);
    decode_piece(&piece, &mut out.data)?;
    Ok(out)
}

pub fn encode(image: &media::Rgba, options: &Options) -> Result<Vec<u8>> {
    check_image(image)?;
    let (body, alpha) = encode_piece(image, options)?;
    let mut w = riff::Writer::new();
    if alpha && !options.lossless {
        let mut x = Vec::with_capacity(10);
        x.extend_from_slice(&[0x10, 0, 0, 0]);
        riff::put_u24(&mut x, image.width - 1);
        riff::put_u24(&mut x, image.height - 1);
        w.chunk(b"VP8X", &x);
    }
    w.out.extend_from_slice(&body);
    w.finish()
}

pub(crate) fn encode_piece(image: &media::Rgba, options: &Options) -> Result<(Vec<u8>, bool)> {
    let alpha = image.data.chunks_exact(4).any(|p| p[3] != 255);
    let mut out = Vec::new();
    if options.lossless {
        riff::put_chunk(&mut out, b"VP8L", &lossless::encode(image, options)?);
    } else {
        let data = vp8::encode(&image.data, image.width, image.height, options)?;
        if alpha {
            riff::put_chunk(&mut out, b"ALPH", &alpha::encode(image, options)?);
        }
        riff::put_chunk(&mut out, b"VP8 ", &data);
    }
    Ok((out, alpha))
}

pub(crate) fn check_image(image: &media::Rgba) -> Result<()> {
    if image.width == 0 || image.height == 0 || image.width > 16383 || image.height > 16383 {
        return Err(crate::media::Error::Unsupported(format!(
            "webp: {}x{} (each side 1 to 16383)",
            image.width, image.height
        )));
    }
    if image.data.len() != image.width as usize * image.height as usize * 4 {
        return Err(invalid("RGBA buffer size"));
    }
    Ok(())
}

pub(crate) fn decode_piece(piece: &Piece, out: &mut [u8]) -> Result<()> {
    match piece.coded {
        Coded::Lossless(data) => {
            let (w, h, _) = lossless::header(data)?;
            if w != piece.width || h != piece.height {
                return Err(invalid("VP8L size differs from its frame"));
            }
            lossless::decode_rgba(data, out)
        }
        Coded::Lossy { data, alpha } => {
            let (w, h) = vp8::info(data)?;
            if w != piece.width || h != piece.height {
                return Err(invalid("VP8 size differs from its frame"));
            }
            vp8::decode_into(data, out)?;
            if let Some(a) = alpha {
                alpha::decode(a, w, h, out)?;
            }
            Ok(())
        }
    }
}

pub(crate) fn parse(file: &[u8]) -> Result<Parsed<'_>> {
    let body = riff::body(file)?;
    let mut chunks = Chunks::new(body);
    let first = chunks.next().ok_or_else(|| invalid("no chunks"))??;
    match &first.id {
        b"VP8 " => {
            let (w, h) = vp8::info(first.data)?;
            Ok(still(
                Coded::Lossy {
                    data: first.data,
                    alpha: None,
                },
                w,
                h,
                false,
                false,
            ))
        }
        b"VP8L" => {
            let (w, h, a) = lossless::header(first.data)?;
            Ok(still(Coded::Lossless(first.data), w, h, a, true))
        }
        b"VP8X" => parse_extended(first.data, chunks),
        _ => Err(invalid("unknown first chunk")),
    }
}

fn still(coded: Coded<'_>, width: u32, height: u32, alpha: bool, lossless: bool) -> Parsed<'_> {
    let piece = Piece {
        x: 0,
        y: 0,
        width,
        height,
        duration: 0,
        blend: false,
        dispose: false,
        alpha,
        coded,
    };
    Parsed {
        info: Info {
            width,
            height,
            alpha,
            animated: false,
            lossless,
            frames: 1,
            loop_count: 0,
            background: [0; 4],
        },
        pieces: vec![piece],
    }
}

fn parse_extended<'a>(x: &[u8], chunks: Chunks<'a>) -> Result<Parsed<'a>> {
    if x.len() < 10 {
        return Err(invalid("VP8X chunk too short"));
    }
    let flags = x[0];
    let width = u24(&x[4..]) + 1;
    let height = u24(&x[7..]) + 1;
    if width as u64 * height as u64 > 1 << 28 {
        return Err(crate::media::Error::Unsupported(format!(
            "webp: {width}x{height} canvas is over 268 megapixels"
        )));
    }
    let animated = flags & 0x02 != 0;
    let mut info = Info {
        width,
        height,
        alpha: flags & 0x10 != 0,
        animated,
        lossless: false,
        frames: 0,
        loop_count: 0,
        background: [0; 4],
    };
    let mut pieces = Vec::new();
    let mut alph = None;
    for chunk in chunks {
        let chunk = chunk?;
        match &chunk.id {
            b"ANIM" if animated => {
                if chunk.data.len() < 6 {
                    return Err(invalid("ANIM chunk too short"));
                }
                let b = chunk.data;
                info.background = [b[2], b[1], b[0], b[3]];
                info.loop_count = u16::from_le_bytes([b[4], b[5]]);
            }
            b"ANMF" if animated => pieces.push(parse_anmf(chunk.data)?),
            b"ALPH" if !animated => alph = Some(chunk.data),
            b"VP8 " if !animated && pieces.is_empty() => {
                let (w, h) = vp8::info(chunk.data)?;
                pieces.push(Piece {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                    duration: 0,
                    blend: false,
                    dispose: false,
                    alpha: alph.is_some(),
                    coded: Coded::Lossy {
                        data: chunk.data,
                        alpha: alph,
                    },
                });
            }
            b"VP8L" if !animated && pieces.is_empty() => {
                let (w, h, a) = lossless::header(chunk.data)?;
                pieces.push(Piece {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                    duration: 0,
                    blend: false,
                    dispose: false,
                    alpha: a,
                    coded: Coded::Lossless(chunk.data),
                });
            }
            _ => {}
        }
    }
    if pieces.is_empty() {
        return Err(invalid("no image data"));
    }
    for p in &pieces {
        if p.x + p.width > width || p.y + p.height > height {
            return Err(invalid("frame outside the canvas"));
        }
    }
    if !animated && (pieces[0].width != width || pieces[0].height != height) {
        return Err(invalid("image size differs from the canvas"));
    }
    info.lossless = pieces.iter().all(|p| matches!(p.coded, Coded::Lossless(_)));
    info.frames = pieces.len() as u32;
    Ok(Parsed { info, pieces })
}

fn parse_anmf(d: &[u8]) -> Result<Piece<'_>> {
    if d.len() < 16 {
        return Err(invalid("ANMF chunk too short"));
    }
    let x = u24(&d[0..]) * 2;
    let y = u24(&d[3..]) * 2;
    let width = u24(&d[6..]) + 1;
    let height = u24(&d[9..]) + 1;
    let duration = u24(&d[12..]);
    let flags = d[15];
    let mut alph = None;
    let mut coded = None;
    for chunk in Chunks::new(&d[16..]) {
        let chunk = chunk?;
        match &chunk.id {
            b"ALPH" => alph = Some(chunk.data),
            b"VP8 " => {
                coded = Some((
                    Coded::Lossy {
                        data: chunk.data,
                        alpha: alph,
                    },
                    alph.is_some(),
                ));
                break;
            }
            b"VP8L" => {
                let (_, _, a) = lossless::header(chunk.data)?;
                coded = Some((Coded::Lossless(chunk.data), a));
                break;
            }
            _ => {}
        }
    }
    let (coded, alpha) = coded.ok_or_else(|| invalid("ANMF without image data"))?;
    Ok(Piece {
        x,
        y,
        width,
        height,
        duration,
        blend: flags & 0x02 == 0,
        dispose: flags & 0x01 != 0,
        alpha,
        coded,
    })
}
