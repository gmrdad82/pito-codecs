use super::huffman_tree::{PeekedDataLookupResult, VorbisHuffmanTree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndOfPacket;

pub struct BitpackCursor<'a> {
    bit_cursor: u8,
    byte_cursor: usize,
    inner: &'a [u8],
}

fn float32_unpack(val: u32) -> f32 {
    let sgn = val & 0x8000_0000;
    let exp = (val & 0x7fe0_0000) >> 21;
    let mantissa = (val & 0x1f_ffff) as f64;
    let signed_mantissa = if sgn != 0 { -mantissa } else { mantissa };
    signed_mantissa as f32 * (exp as f32 - 788.0).exp2()
}

macro_rules! uk_reader {
    ($fnname:ident, $rettype:ident, $bitnum:expr) => {
        #[inline]
        pub fn $fnname(&mut self) -> Result<$rettype, EndOfPacket> {
            self.read_bits($bitnum).map(|v| v as $rettype)
        }
    };
}

macro_rules! uk_dynamic_reader {
    ($fnname:ident, $rettype:ident, $bit_num_max:expr) => {
        #[inline]
        pub fn $fnname(&mut self, bit_num: u8) -> Result<$rettype, EndOfPacket> {
            if bit_num > $bit_num_max {
                return Err(EndOfPacket);
            }
            self.read_bits(bit_num as u32).map(|v| v as $rettype)
        }
    };
}

impl<'a> BitpackCursor<'a> {
    pub fn new(arr: &'a [u8]) -> BitpackCursor<'a> {
        BitpackCursor {
            bit_cursor: 0,
            byte_cursor: 0,
            inner: arr,
        }
    }

    pub fn bits_left(&self) -> usize {
        (self.inner.len().saturating_sub(self.byte_cursor) * 8)
            .saturating_sub(self.bit_cursor as usize)
    }

    #[inline]
    fn peek_bits(&self, n: u32) -> Result<u64, EndOfPacket> {
        if n == 0 {
            return Ok(0);
        }
        if n > 64 || (n as usize) > self.bits_left() {
            return Err(EndOfPacket);
        }
        let shift = self.bit_cursor as u32;
        let raw = match self.inner.get(self.byte_cursor..self.byte_cursor + 16) {
            Some(b) => {
                let mut w = [0u8; 16];
                w.copy_from_slice(b);
                u128::from_le_bytes(w)
            }
            None => {
                let mut w = [0u8; 16];
                let tail = &self.inner[self.byte_cursor..];
                w[..tail.len()].copy_from_slice(tail);
                u128::from_le_bytes(w)
            }
        };
        let v = (raw >> shift) as u64;
        Ok(if n == 64 { v } else { v & ((1u64 << n) - 1) })
    }

    #[inline]
    fn skip_bits(&mut self, n: u32) {
        let pos = self.bit_cursor as usize + n as usize;
        self.byte_cursor += pos >> 3;
        self.bit_cursor = (pos & 7) as u8;
    }

    #[inline]
    fn read_bits(&mut self, n: u32) -> Result<u64, EndOfPacket> {
        let v = self.peek_bits(n)?;
        self.skip_bits(n);
        Ok(v)
    }

    uk_reader!(read_u32, u32, 32);
    uk_reader!(read_u24, u32, 24);
    uk_reader!(read_u16, u16, 16);
    uk_reader!(read_u13, u16, 13);
    uk_reader!(read_u8, u8, 8);
    uk_reader!(read_u7, u8, 7);
    uk_reader!(read_u6, u8, 6);
    uk_reader!(read_u5, u8, 5);
    uk_reader!(read_u4, u8, 4);
    uk_reader!(read_u3, u8, 3);
    uk_reader!(read_u2, u8, 2);
    uk_reader!(read_u1, u8, 1);

    #[inline]
    pub fn read_bit_flag(&mut self) -> Result<bool, EndOfPacket> {
        Ok(self.read_u1()? == 1)
    }

    uk_dynamic_reader!(read_dyn_u8, u8, 8);
    uk_dynamic_reader!(read_dyn_u16, u16, 16);
    uk_dynamic_reader!(read_dyn_u32, u32, 32);
    uk_dynamic_reader!(read_dyn_u64, u64, 64);

    #[inline]
    pub fn read_i32(&mut self) -> Result<i32, EndOfPacket> {
        Ok(self.read_u32()? as i32)
    }

    pub fn read_f32(&mut self) -> Result<f32, EndOfPacket> {
        Ok(float32_unpack(self.read_u32()?))
    }

    #[inline]
    pub fn peek_u8(&self) -> Result<u8, EndOfPacket> {
        self.peek_bits(8).map(|v| v as u8)
    }

    pub fn advance_dyn_u8(&mut self, bit_num: u8) -> Result<(), EndOfPacket> {
        if bit_num > 8 || bit_num as usize > self.bits_left() {
            return Err(EndOfPacket);
        }
        self.skip_bits(bit_num as u32);
        Ok(())
    }

    pub fn read_huffman(&mut self, tree: &VorbisHuffmanTree) -> Result<u32, EndOfPacket> {
        if tree.is_empty() {
            return Err(EndOfPacket);
        }
        let mut iter = match self.peek_u8() {
            Ok(data) => match tree.lookup_peeked_data(8, data as u32) {
                PeekedDataLookupResult::Iter(advance, iter) => {
                    self.advance_dyn_u8(advance)?;
                    iter
                }
                PeekedDataLookupResult::PayloadFound(advance, payload) => {
                    self.advance_dyn_u8(advance)?;
                    return Ok(payload);
                }
            },
            Err(_) => tree.iter(),
        };
        loop {
            let b = self.read_bit_flag()?;
            if let Some(v) = iter.next(b) {
                return Ok(v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_32_unpack_matches_stb_vorbis() {
        let cases: [(u32, f32); 27] = [
            (1611661312, 1.0),
            (1616117760, 5.0),
            (1618345984, 11.0),
            (1620115456, 17.0),
            (1627381760, 255.0),
            (3759144960, -1.0),
            (3761242112, -2.0),
            (3763339264, -4.0),
            (3763601408, -5.0),
            (3765436416, -8.0),
            (3765829632, -11.0),
            (3768451072, -30.0),
            (3772628992, -119.0),
            (3780634624, -1530.0),
            (1628434432, 255.0),
            (1621655552, 17.0),
            (1619722240, 11.0),
            (1613234176, 1.0),
            (3760717824, -1.0),
            (3762814976, -2.0),
            (3764912128, -4.0),
            (3765043200, -5.0),
            (3767009280, -8.0),
            (3767205888, -11.0),
            (3769565184, -30.0),
            (3773751296, -119.0),
            (3781948416, -1530.0),
        ];
        for (raw, want) in cases {
            assert_eq!(float32_unpack(raw), want, "{raw}");
        }
    }

    #[test]
    fn reads_the_spec_vectors() {
        let test_arr = &[0b11111100, 0b01001000, 0b11001110, 0b00000110];
        let mut cur = BitpackCursor::new(test_arr);
        assert_eq!(cur.read_u4().unwrap(), 12);
        assert_eq!(cur.read_u3().unwrap(), 7);
        assert_eq!(cur.read_u7().unwrap(), 17);
        assert_eq!(cur.read_u13().unwrap(), 6969);
        assert_eq!(cur.bits_left(), 5);
        assert_eq!(cur.read_u8(), Err(EndOfPacket));
        assert_eq!(cur.bits_left(), 5);
    }

    #[test]
    fn dynamic_reads_match_the_static_ones() {
        let test_arr = &[0b11111100, 0b01001000, 0b11001110, 0b00000110];
        let mut cur = BitpackCursor::new(test_arr);
        assert_eq!(cur.read_dyn_u8(4).unwrap(), 12);
        assert_eq!(cur.read_dyn_u8(0).unwrap(), 0);
        assert_eq!(cur.read_dyn_u8(3).unwrap(), 7);
        assert_eq!(cur.read_dyn_u16(7).unwrap(), 17);
        assert_eq!(cur.read_dyn_u16(0).unwrap(), 0);
        assert_eq!(cur.read_dyn_u16(13).unwrap(), 6969);
        let mut cur = BitpackCursor::new(&[93, 92]);
        assert_eq!(cur.read_dyn_u32(10).unwrap(), 93);
    }

    #[test]
    fn byte_aligned_reads_regain_alignment() {
        let mut cur = BitpackCursor::new(&[0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(cur.read_dyn_u32(32).unwrap(), 0);
        assert_eq!(cur.read_dyn_u8(8).unwrap(), 1);
        let mut cur = BitpackCursor::new(&[0x09, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(cur.read_u1().unwrap(), 1);
        assert_eq!(cur.read_u7().unwrap(), 4);
        assert_eq!(cur.read_u8().unwrap(), 2);
        assert_eq!(cur.read_u32().unwrap(), 0);
        assert_eq!(cur.read_u8().unwrap(), 1);
        assert_eq!(cur.bits_left(), 0);
    }

    #[test]
    fn capture_pattern_reads_unaligned() {
        let mut cur = BitpackCursor::new(&[0x42, 0x43, 0x56]);
        assert_eq!(cur.read_u24().unwrap(), 0x564342);
        let mut cur = BitpackCursor::new(&[0x28, 0x81, 0xd0, 0x90, 0x55, 0x00, 0x00]);
        cur.read_u5().unwrap();
        cur.read_u5().unwrap();
        assert_eq!(cur.read_u4().unwrap(), 0);
        assert_eq!(cur.read_u24().unwrap(), 0x564342);
        assert_eq!(cur.read_u16().unwrap(), 1);
    }

    #[test]
    fn sixty_four_bit_reads_at_every_offset() {
        let data: Vec<u8> = (0..24u8).map(|i| i.wrapping_mul(37) ^ 0xa5).collect();
        let mut bits = Vec::new();
        for b in &data {
            for k in 0..8 {
                bits.push((b >> k) & 1);
            }
        }
        for off in 0..8u8 {
            let mut cur = BitpackCursor::new(&data);
            cur.read_dyn_u8(off).unwrap();
            let got = cur.read_dyn_u64(64).unwrap();
            let want = (0..64).fold(0u64, |a, i| a | ((bits[off as usize + i] as u64) << i));
            assert_eq!(got, want, "offset {off}");
        }
    }

    #[test]
    fn reads_near_the_end_use_the_short_path() {
        let data = [0xffu8, 0x0f, 0xf0];
        let mut cur = BitpackCursor::new(&data);
        assert_eq!(cur.read_u4().unwrap(), 0xf);
        assert_eq!(cur.read_dyn_u32(20).unwrap(), 0xf00ff);
        assert_eq!(cur.read_bit_flag(), Err(EndOfPacket));
    }

    #[test]
    fn oversized_dynamic_widths_are_refused() {
        let data = [0u8; 16];
        let mut cur = BitpackCursor::new(&data);
        assert_eq!(cur.read_dyn_u8(9), Err(EndOfPacket));
        assert_eq!(cur.read_dyn_u16(17), Err(EndOfPacket));
        assert_eq!(cur.read_dyn_u32(33), Err(EndOfPacket));
        assert_eq!(cur.read_dyn_u64(65), Err(EndOfPacket));
        assert_eq!(cur.advance_dyn_u8(9), Err(EndOfPacket));
        assert_eq!(cur.bits_left(), 128);
    }

    #[test]
    fn empty_input_refuses_every_read_but_zero_width() {
        let mut cur = BitpackCursor::new(&[]);
        assert_eq!(cur.read_dyn_u32(0), Ok(0));
        assert_eq!(cur.read_u1(), Err(EndOfPacket));
        assert_eq!(cur.peek_u8(), Err(EndOfPacket));
        assert_eq!(cur.advance_dyn_u8(1), Err(EndOfPacket));
        assert_eq!(cur.read_f32(), Err(EndOfPacket));
    }

    #[test]
    fn peek_does_not_advance_and_advance_does() {
        let mut cur = BitpackCursor::new(&[0b1010_0110, 0b0000_0001]);
        cur.read_u3().unwrap();
        assert_eq!(cur.peek_u8().unwrap(), 0b0011_0100);
        assert_eq!(cur.peek_u8().unwrap(), 0b0011_0100);
        cur.advance_dyn_u8(5).unwrap();
        assert_eq!(cur.read_u8().unwrap(), 1);
    }
}
