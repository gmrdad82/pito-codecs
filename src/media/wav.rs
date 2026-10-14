use super::{AudioBuffer, Error, Result};

fn invalid(what: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("WAV: {what}"))
}

fn le16(b: &[u8], at: usize) -> Result<u16> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| invalid("the fmt chunk is too short"))
}

fn le32(b: &[u8], at: usize) -> Result<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| invalid("the file ends inside a chunk header"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sample {
    Int,
    Float,
}

pub fn decode(data: &[u8]) -> Result<AudioBuffer> {
    if data.get(..4) != Some(b"RIFF") || data.get(8..12) != Some(b"WAVE") {
        return Err(invalid("not a RIFF WAVE file"));
    }
    let mut format = None;
    let mut samples = None;
    let mut at = 12usize;
    while at + 8 <= data.len() {
        let len = le32(data, at + 4)? as usize;
        let start = at + 8;
        let end = start.saturating_add(len).min(data.len());
        let body = &data[start..end];
        match &data[at..at + 4] {
            b"fmt " => {
                let tag = le16(body, 0)?;
                let channels = le16(body, 2)?;
                let rate = le32(body, 4).map_err(|_| invalid("the fmt chunk is too short"))?;
                let align = le16(body, 12)?;
                let bits = le16(body, 14)?;
                let tag = if tag == 0xfffe { le16(body, 24)? } else { tag };
                let kind = match (tag, bits) {
                    (1, 8 | 16 | 24 | 32) => Sample::Int,
                    (3, 32 | 64) => Sample::Float,
                    _ => {
                        return Err(Error::Unsupported(format!(
                            "WAV format {tag} with {bits}-bit samples; PCM 8, 16, 24 or 32-bit and float 32 or 64-bit are read"
                        )));
                    }
                };
                if channels == 0 || rate == 0 {
                    return Err(invalid(format!("{channels} channels at {rate} Hz")));
                }
                if usize::from(align) != usize::from(channels) * usize::from(bits / 8) {
                    return Err(invalid(format!(
                        "a block of {align} bytes for {channels} channels of {bits} bits"
                    )));
                }
                format = Some((kind, channels, rate, bits));
            }
            b"data" => {
                samples = Some(body);
                if format.is_some() {
                    break;
                }
            }
            _ => {}
        }
        at = start.saturating_add(len).saturating_add(len & 1);
    }
    let (kind, channels, rate, bits) = format.ok_or_else(|| invalid("no fmt chunk"))?;
    let body = samples.ok_or_else(|| invalid("no data chunk"))?;
    let width = usize::from(bits / 8);
    let frame = width * usize::from(channels);
    let body = &body[..body.len() - body.len() % frame];
    let out: Vec<f32> = match (kind, bits) {
        (Sample::Int, 8) => body
            .iter()
            .map(|&b| (f32::from(b) - 128.0) / 128.0)
            .collect(),
        (Sample::Int, 16) => body
            .chunks_exact(2)
            .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0)
            .collect(),
        (Sample::Int, 24) => body
            .chunks_exact(3)
            .map(|s| (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0)
            .collect(),
        (Sample::Int, _) => body
            .chunks_exact(4)
            .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0)
            .collect(),
        (Sample::Float, 32) => body
            .chunks_exact(4)
            .map(|s| clean(f32::from_le_bytes([s[0], s[1], s[2], s[3]])))
            .collect(),
        (Sample::Float, _) => body
            .chunks_exact(8)
            .map(|s| {
                let mut b = [0u8; 8];
                b.copy_from_slice(s);
                clean(f64::from_le_bytes(b) as f32)
            })
            .collect(),
    };
    Ok(AudioBuffer {
        samples: out,
        rate,
        channels,
    })
}

fn clean(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}
