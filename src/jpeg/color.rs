use super::Block;

#[inline]
pub(super) fn rgb_to_ycbcr(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let r = i32::from(r);
    let g = i32::from(g);
    let b = i32::from(b);

    let y = 19595 * r + 38470 * g + 7471 * b;
    let cb = -11059 * r - 21709 * g + 32768 * b + (128 << 16);
    let cr = 32768 * r - 27439 * g - 5329 * b + (128 << 16);

    let y = (y + 0x7FFF) >> 16;
    let cb = (cb + 0x7FFF) >> 16;
    let cr = (cr + 0x7FFF) >> 16;

    (y as u8, cb as u8, cr as u8)
}

pub(super) fn ycbcr_row(rgba: &[u8], y: &mut [u8], cb: &mut [u8], cr: &mut [u8]) {
    for (((px, y), cb), cr) in rgba
        .chunks_exact(4)
        .zip(y.iter_mut())
        .zip(cb.iter_mut())
        .zip(cr.iter_mut())
    {
        (*y, *cb, *cr) = rgb_to_ycbcr(px[0], px[1], px[2]);
    }
}

pub(super) fn block(data: &[u8], x: usize, y: usize, stride: usize) -> Block {
    let mut block = [0i16; 64];
    for (row, out) in block.chunks_exact_mut(8).enumerate() {
        let at = (y + row) * stride + x;
        for (o, &v) in out.iter_mut().zip(&data[at..at + 8]) {
            *o = i16::from(v) - 128;
        }
    }
    Block(block)
}

pub(super) fn block_averaged(data: &[u8], x: usize, y: usize, stride: usize) -> Block {
    let mut block = [0i16; 64];
    for (row, out) in block.chunks_exact_mut(8).enumerate() {
        let top = (y + row * 2) * stride + x;
        let bottom = top + stride;
        for (col, o) in out.iter_mut().enumerate() {
            let ix = col * 2;
            let sum = u16::from(data[top + ix])
                + u16::from(data[top + ix + 1])
                + u16::from(data[bottom + ix])
                + u16::from(data[bottom + ix + 1]);
            let bias = if col & 1 == 0 { 1 } else { 2 };
            *o = ((sum + bias) / 4) as i16 - 128;
        }
    }
    Block(block)
}
