use super::huffman::HuffmanTable;
use super::quant::QuantTable;

pub(super) const SOI: u8 = 0xD8;
pub(super) const EOI: u8 = 0xD9;
const APP0: u8 = 0xE0;
const DQT: u8 = 0xDB;
const SOF0: u8 = 0xC0;
const DHT: u8 = 0xC4;
const SOS: u8 = 0xDA;

pub(super) static ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const BUFFER_SIZE: i8 = 64;

pub(super) struct Writer {
    w: Vec<u8>,
    bit_buffer: u64,
    free_bits: i8,
}

impl Writer {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Writer {
            w: Vec::with_capacity(capacity),
            bit_buffer: 0,
            free_bits: BUFFER_SIZE,
        }
    }

    pub(super) fn into_inner(self) -> Vec<u8> {
        self.w
    }

    fn write_u16(&mut self, value: u16) {
        self.w.extend_from_slice(&value.to_be_bytes());
    }

    pub(super) fn marker(&mut self, marker: u8) {
        self.w.extend_from_slice(&[0xFF, marker]);
    }

    pub(super) fn app0(&mut self) {
        self.marker(APP0);
        self.write_u16(16);
        self.w.extend_from_slice(b"JFIF\0");
        self.w.extend_from_slice(&[0x01, 0x02, 0x00]);
        self.write_u16(1);
        self.write_u16(1);
        self.w.extend_from_slice(&[0x00, 0x00]);
    }

    pub(super) fn quant_segment(&mut self, destination: u8, table: &QuantTable) {
        self.marker(DQT);
        self.write_u16(2 + 1 + 64);
        self.w.push(destination);
        for &v in ZIGZAG.iter() {
            self.w.push(table.get(usize::from(v)));
        }
    }

    pub(super) fn frame_header(&mut self, width: u16, height: u16) {
        self.marker(SOF0);
        self.write_u16(2 + 1 + 2 + 2 + 1 + 3 * 3);
        self.w.push(8);
        self.write_u16(height);
        self.write_u16(width);
        self.w.push(3);
        self.w
            .extend_from_slice(&[1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    }

    pub(super) fn huffman_segment(&mut self, class: u8, destination: u8, table: &HuffmanTable) {
        self.marker(DHT);
        self.write_u16(2 + 1 + 16 + table.values().len() as u16);
        self.w.push((class << 4) | destination);
        self.w.extend_from_slice(table.length());
        self.w.extend_from_slice(table.values());
    }

    pub(super) fn scan_header(&mut self) {
        self.marker(SOS);
        self.write_u16(2 + 1 + 3 * 2 + 3);
        self.w.push(3);
        self.w.extend_from_slice(&[1, 0x00, 2, 0x11, 3, 0x11]);
        self.w.extend_from_slice(&[0, 63, 0]);
    }

    pub(super) fn finalize_bits(&mut self) {
        self.write_bits(0x7F, 7);
        self.flush_bit_buffer();
        self.bit_buffer = 0;
        self.free_bits = BUFFER_SIZE;
    }

    fn flush_bit_buffer(&mut self) {
        while self.free_bits <= BUFFER_SIZE - 8 {
            self.flush_byte_from_bit_buffer(self.free_bits);
            self.free_bits += 8;
        }
    }

    #[inline(always)]
    fn flush_byte_from_bit_buffer(&mut self, free_bits: i8) {
        let value = (self.bit_buffer >> (BUFFER_SIZE - 8 - free_bits)) as u8;
        self.w.push(value);
        if value == 0xFF {
            self.w.push(0x00);
        }
    }

    #[inline(always)]
    fn write_bit_buffer(&mut self) {
        if (self.bit_buffer
            & 0x8080_8080_8080_8080
            & !(self.bit_buffer.wrapping_add(0x0101_0101_0101_0101)))
            != 0
        {
            for i in 0..(BUFFER_SIZE / 8) {
                self.flush_byte_from_bit_buffer(i * 8);
            }
        } else {
            self.w.extend_from_slice(&self.bit_buffer.to_be_bytes());
        }
    }

    #[inline]
    pub(super) fn write_bits(&mut self, value: u32, size: u8) {
        let size = size as i8;
        let value = u64::from(value);
        let free_bits = self.free_bits - size;
        if free_bits < 0 {
            self.bit_buffer = (self.bit_buffer << (size + free_bits)) | (value >> -free_bits);
            self.write_bit_buffer();
            self.bit_buffer = value;
            self.free_bits = free_bits + BUFFER_SIZE;
        } else {
            self.free_bits = free_bits;
            self.bit_buffer = (self.bit_buffer << size) | value;
        }
    }

    #[inline]
    fn huffman_encode(&mut self, val: u8, table: &HuffmanTable) {
        let (size, code) = table.get_for_value(val);
        self.write_bits(u32::from(code), size);
    }

    #[inline]
    fn huffman_encode_value(&mut self, size: u8, symbol: u8, value: u16, table: &HuffmanTable) {
        let (num_bits, code) = table.get_for_value(symbol);
        let temp = u32::from(value) | (u32::from(code) << size);
        self.write_bits(temp, size + num_bits);
    }

    pub(super) fn block(
        &mut self,
        block: &[i16; 64],
        prev_dc: i16,
        dc_table: &HuffmanTable,
        ac_table: &HuffmanTable,
    ) {
        let (size, value) = get_code(block[0].wrapping_sub(prev_dc));
        self.huffman_encode_value(size, size, value, dc_table);

        let mut zero_run = 0u8;
        for &value in &block[1..] {
            if value == 0 {
                zero_run += 1;
            } else {
                while zero_run > 15 {
                    self.huffman_encode(0xF0, ac_table);
                    zero_run -= 16;
                }
                let (size, value) = get_code(value);
                self.huffman_encode_value(size, (zero_run << 4) | size, value, ac_table);
                zero_run = 0;
            }
        }
        if zero_run > 0 {
            self.huffman_encode(0x00, ac_table);
        }
    }
}

#[inline]
pub(super) fn get_code(value: i16) -> (u8, u16) {
    let temp = value.wrapping_sub(i16::from(value.is_negative()));
    let temp2 = value.unsigned_abs();
    let num_bits = 16 - temp2.leading_zeros() as u16;
    let coefficient = (temp as u16) & ((1u32 << num_bits) - 1) as u16;
    (num_bits as u8, coefficient)
}
