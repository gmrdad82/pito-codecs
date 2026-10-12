use crate::media::{AudioBuffer, Error, Result};

const H: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn stereo_weights(channels: usize) -> Vec<[f32; 2]> {
    let side = |left: bool, g: f32| if left { [g, 0.0] } else { [0.0, g] };
    match channels {
        1 => vec![[1.0, 1.0]],
        2 => vec![[1.0, 0.0], [0.0, 1.0]],
        3 => vec![[1.0, 0.0], [0.0, 1.0], [H, H]],
        4 => vec![[1.0, 0.0], [0.0, 1.0], [H, 0.0], [0.0, H]],
        5 => vec![[1.0, 0.0], [0.0, 1.0], [H, H], [H, 0.0], [0.0, H]],
        6 => vec![
            [1.0, 0.0],
            [0.0, 1.0],
            [H, H],
            [0.0, 0.0],
            [H, 0.0],
            [0.0, H],
        ],
        7 => vec![
            [1.0, 0.0],
            [0.0, 1.0],
            [H, H],
            [0.0, 0.0],
            [0.5, 0.5],
            [H, 0.0],
            [0.0, H],
        ],
        8 => vec![
            [1.0, 0.0],
            [0.0, 1.0],
            [H, H],
            [0.0, 0.0],
            [H, 0.0],
            [0.0, H],
            [H, 0.0],
            [0.0, H],
        ],
        n => (0..n)
            .map(|i| match i {
                0 => [1.0, 0.0],
                1 => [0.0, 1.0],
                _ => side(i % 2 == 0, H),
            })
            .collect(),
    }
}

pub fn downmix(input: &AudioBuffer, channels: u16) -> Result<AudioBuffer> {
    let from = input.channels as usize;
    if from == 0 {
        return Err(Error::Invalid("audio with no channels".into()));
    }
    let mut out = AudioBuffer::new(input.rate, channels);
    if from == channels as usize {
        out.samples = input.samples.clone();
        return Ok(out);
    }
    let mut w = stereo_weights(from);
    let norm = |w: &[[f32; 2]], k: usize| w.iter().map(|c| c[k].abs()).sum::<f32>().max(1.0);
    let (nl, nr) = (norm(&w, 0), norm(&w, 1));
    for c in w.iter_mut() {
        c[0] /= nl;
        c[1] /= nr;
    }
    let frames = input.frames();
    match channels {
        2 => {
            out.samples.resize(frames * 2, 0.0);
            for (o, f) in out
                .samples
                .chunks_exact_mut(2)
                .zip(input.samples.chunks_exact(from))
            {
                let (mut l, mut r) = (0.0, 0.0);
                for (s, c) in f.iter().zip(&w) {
                    l += s * c[0];
                    r += s * c[1];
                }
                o[0] = l;
                o[1] = r;
            }
        }
        1 => {
            let m: Vec<f32> = w.iter().map(|c| 0.5 * (c[0] + c[1])).collect();
            out.samples.resize(frames, 0.0);
            for (o, f) in out.samples.iter_mut().zip(input.samples.chunks_exact(from)) {
                *o = f.iter().zip(&m).map(|(s, c)| s * c).sum();
            }
        }
        n => {
            return Err(Error::Unsupported(format!(
                "downmix to {n} channels; only mono and stereo"
            )));
        }
    }
    Ok(out)
}

pub fn gain(buffer: &mut AudioBuffer, db: f32) {
    let g = 10f32.powf(db / 20.0);
    buffer.samples.iter_mut().for_each(|s| *s *= g);
}
