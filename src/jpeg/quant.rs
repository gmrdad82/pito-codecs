use super::Block;
use super::writer::ZIGZAG;

pub(super) static LUMA: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

pub(super) static CHROMA: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

const SHIFT: u32 = 2 * 8 - 1;

pub(super) fn compute_reciprocal(divisor: u32) -> (i32, i32) {
    let divisor = divisor.max(1);
    let mut reciprocals = (1 << SHIFT) / divisor;
    let fractional = (1 << SHIFT) % divisor;

    let mut correction = divisor / 2;

    if fractional != 0 {
        if fractional <= correction {
            correction += 1;
        } else {
            reciprocals += 1;
        }
    }

    (reciprocals as i32, correction as i32)
}

pub(super) struct QuantTable {
    table: [u16; 64],
    reciprocals: [i32; 64],
    corrections: [i32; 64],
}

impl QuantTable {
    pub(super) fn new(base: &[u16; 64], quality: u8) -> QuantTable {
        let table = scaled(base, quality);
        let mut reciprocals = [0i32; 64];
        let mut corrections = [0i32; 64];
        for i in 0..64 {
            (reciprocals[i], corrections[i]) = compute_reciprocal(u32::from(table[i]));
        }
        QuantTable {
            table,
            reciprocals,
            corrections,
        }
    }

    #[inline]
    pub(super) fn get(&self, index: usize) -> u8 {
        (self.table[index] >> 3) as u8
    }

    #[inline]
    pub(super) fn quantize(&self, in_value: i16, index: usize) -> i16 {
        let value = i32::from(in_value);
        let abs_value = value.abs();
        let mut product = (abs_value + self.corrections[index]) * self.reciprocals[index];
        product >>= SHIFT;
        if value != abs_value {
            product = -product;
        }
        product as i16
    }

    pub(super) fn quantize_block(&self, block: &Block) -> [i16; 64] {
        let mut out = [0i16; 64];
        for (o, &z) in out.iter_mut().zip(ZIGZAG.iter()) {
            let z = usize::from(z) & 0x3f;
            *o = self.quantize(block.0[z], z);
        }
        out
    }
}

pub(super) fn scaled(base: &[u16; 64], quality: u8) -> [u16; 64] {
    let quality = u32::from(quality.clamp(1, 100));
    let scale = if quality < 50 {
        5000 / quality
    } else {
        200 - quality * 2
    };
    let mut out = [0u16; 64];
    for (o, &v) in out.iter_mut().zip(base) {
        let v = (u32::from(v) * scale + 50) / 100;
        *o = (v.clamp(1, 255) as u16) << 3;
    }
    out
}
