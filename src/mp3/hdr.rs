use super::tables::HALFRATE;

pub const HDR_SIZE: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Head(pub [u8; 4]);

impl Head {
    pub fn at(b: &[u8], i: usize) -> Option<Head> {
        let s = b.get(i..i.checked_add(HDR_SIZE)?)?;
        Some(Head([s[0], s[1], s[2], s[3]]))
    }

    pub fn mono(self) -> bool {
        self.0[3] & 0xC0 == 0xC0
    }

    pub fn is_ms_stereo(self) -> bool {
        self.0[3] & 0xE0 == 0x60
    }

    pub fn free_format(self) -> bool {
        self.0[2] & 0xF0 == 0
    }

    pub fn crc(self) -> bool {
        self.0[1] & 1 == 0
    }

    pub fn padded(self) -> bool {
        self.0[2] & 2 != 0
    }

    pub fn mpeg1(self) -> bool {
        self.0[1] & 8 != 0
    }

    pub fn not_mpeg25(self) -> bool {
        self.0[1] & 0x10 != 0
    }

    pub fn i_stereo(self) -> bool {
        self.0[3] & 0x10 != 0
    }

    pub fn ms_stereo(self) -> bool {
        self.0[3] & 0x20 != 0
    }

    pub fn stereo_mode(self) -> u8 {
        (self.0[3] >> 6) & 3
    }

    pub fn stereo_mode_ext(self) -> u8 {
        (self.0[3] >> 4) & 3
    }

    pub fn layer_bits(self) -> u8 {
        (self.0[1] >> 1) & 3
    }

    pub fn bitrate_index(self) -> u8 {
        self.0[2] >> 4
    }

    pub fn sample_rate_index(self) -> u8 {
        (self.0[2] >> 2) & 3
    }

    pub fn my_sample_rate(self) -> usize {
        self.sample_rate_index() as usize
            + (((self.0[1] >> 3) & 1) + ((self.0[1] >> 4) & 1)) as usize * 3
    }

    pub fn frame_576(self) -> bool {
        self.0[1] & 14 == 2
    }

    pub fn layer_1(self) -> bool {
        self.0[1] & 6 == 6
    }

    pub fn valid(self) -> bool {
        let h = self.0;
        h[0] == 0xFF
            && ((h[1] & 0xF0) == 0xF0 || (h[1] & 0xFE) == 0xE2)
            && self.layer_bits() != 0
            && self.bitrate_index() != 15
            && self.sample_rate_index() != 3
    }

    pub fn compare(self, h2: Head) -> bool {
        h2.valid()
            && (self.0[1] ^ h2.0[1]) & 0xFE == 0
            && (self.0[2] ^ h2.0[2]) & 0x0C == 0
            && self.free_format() == h2.free_format()
    }

    pub fn bitrate_kbps(self) -> u32 {
        let i = self.mpeg1() as usize * 45
            + (self.layer_bits() as usize).saturating_sub(1) * 15
            + self.bitrate_index() as usize;
        2 * HALFRATE.get(i).copied().unwrap_or(0) as u32
    }

    pub fn sample_rate(self) -> u32 {
        const HZ: [u32; 3] = [44100, 48000, 32000];
        let hz = HZ
            .get(self.sample_rate_index() as usize)
            .copied()
            .unwrap_or(0);
        hz >> (!self.mpeg1()) as u32 >> (!self.not_mpeg25()) as u32
    }

    pub fn frame_samples(self) -> usize {
        if self.layer_1() {
            384
        } else {
            1152 >> self.frame_576() as u32
        }
    }

    pub fn frame_bytes(self, free_format_size: usize) -> usize {
        let hz = self.sample_rate().max(1) as usize;
        let mut n = self.frame_samples() * self.bitrate_kbps() as usize * 125 / hz;
        if self.layer_1() {
            n &= !3;
        }
        if n != 0 { n } else { free_format_size }
    }

    pub fn padding(self) -> usize {
        if self.padded() {
            if self.layer_1() { 4 } else { 1 }
        } else {
            0
        }
    }

    pub fn channels(self) -> usize {
        if self.mono() { 1 } else { 2 }
    }

    pub fn layer(self) -> u8 {
        4 - self.layer_bits()
    }
}
