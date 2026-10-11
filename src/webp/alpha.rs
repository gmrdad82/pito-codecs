use super::riff::invalid;
use super::{Options, Rgba, lossless};
use crate::media::Result;

pub(super) fn decode(data: &[u8], width: u32, height: u32, out: &mut [u8]) -> Result<()> {
    let (w, h) = (width as usize, height as usize);
    let head = *data.first().ok_or_else(|| invalid("empty ALPH chunk"))?;
    let filter = (head >> 2) & 3;
    let body = &data[1..];
    let mut plane = match head & 3 {
        0 => {
            if body.len() < w * h {
                return Err(invalid("ALPH data too short"));
            }
            body[..w * h].to_vec()
        }
        1 => lossless::decode_stream(body, width, height)?
            .into_iter()
            .map(|p| (p >> 8) as u8)
            .collect(),
        _ => return Err(invalid("ALPH compression method")),
    };
    unfilter(&mut plane, w, h, filter);
    for (o, &a) in out.chunks_exact_mut(4).zip(&plane) {
        o[3] = a;
    }
    Ok(())
}

fn unfilter(p: &mut [u8], w: usize, h: usize, filter: u8) {
    if filter == 0 {
        return;
    }
    for x in 1..w {
        p[x] = p[x].wrapping_add(p[x - 1]);
    }
    for y in 1..h {
        let (prev, cur) = p[(y - 1) * w..(y + 1) * w].split_at_mut(w);
        cur[0] = cur[0].wrapping_add(prev[0]);
        match filter {
            1 => {
                for x in 1..w {
                    cur[x] = cur[x].wrapping_add(cur[x - 1]);
                }
            }
            2 => {
                for x in 1..w {
                    cur[x] = cur[x].wrapping_add(prev[x]);
                }
            }
            _ => {
                for x in 1..w {
                    let g = cur[x - 1] as i32 + prev[x] as i32 - prev[x - 1] as i32;
                    cur[x] = cur[x].wrapping_add(g.clamp(0, 255) as u8);
                }
            }
        }
    }
}

pub(super) fn encode(image: &Rgba, _options: &Options) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(1 + image.data.len() / 4);
    out.push(0);
    out.extend(image.data.chunks_exact(4).map(|p| p[3]));
    Ok(out)
}
