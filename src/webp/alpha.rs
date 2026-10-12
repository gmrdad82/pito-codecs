use super::riff::invalid;
use super::{Options, lossless};
use crate::media::Result;
use crate::media::Rgba;

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
        1 => {
            let mut plane = vec![0u8; w * h];
            lossless::decode_alpha(body, width, height, &mut plane)?;
            plane
        }
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

pub(super) fn encode(image: &Rgba, options: &Options) -> Result<Vec<u8>> {
    let (w, h) = (image.width as usize, image.height as usize);
    let plane: Vec<u8> = image.data.chunks_exact(4).map(|p| p[3]).collect();
    let mut seen = [false; 256];
    for &a in &plane {
        seen[a as usize] = true;
    }
    let levels = seen.iter().filter(|&&s| s).count();
    let (filter, values) = if levels <= 16 {
        (0, plane)
    } else {
        let best = (1..4u8)
            .map(|f| {
                let r = filter(&plane, w, h, f);
                let c: u64 = r.iter().map(|&v| (v as i8).unsigned_abs() as u64).sum();
                (c, f, r)
            })
            .min_by_key(|x| x.0)
            .unwrap();
        (best.1, best.2)
    };
    let px = values
        .into_iter()
        .map(|a| 0xff000000 | (a as u32) << 8)
        .collect();
    let policy = lossless::Policy {
        effort: options.effort.min(9),
        palette: levels <= 16,
        predict: false,
    };
    let mut out = vec![1 | filter << 2];
    out.extend_from_slice(&lossless::encode_stream(
        px,
        image.width,
        image.height,
        &policy,
    ));
    Ok(out)
}

fn filter(p: &[u8], w: usize, h: usize, filter: u8) -> Vec<u8> {
    let mut r = vec![0u8; p.len()];
    r[0] = p[0];
    for x in 1..w {
        r[x] = p[x].wrapping_sub(p[x - 1]);
    }
    for y in 1..h {
        let prev = &p[(y - 1) * w..y * w];
        let cur = &p[y * w..(y + 1) * w];
        let out = &mut r[y * w..(y + 1) * w];
        out[0] = cur[0].wrapping_sub(prev[0]);
        for x in 1..w {
            let pred = match filter {
                1 => cur[x - 1],
                2 => prev[x],
                _ => (cur[x - 1] as i32 + prev[x] as i32 - prev[x - 1] as i32).clamp(0, 255) as u8,
            };
            out[x] = cur[x].wrapping_sub(pred);
        }
    }
    r
}
