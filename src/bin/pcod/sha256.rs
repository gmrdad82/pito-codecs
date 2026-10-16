use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::path::Path;

const READ: usize = 1 << 20;

const K32: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const K32X4: [[u32; 4]; 16] = {
    let mut out = [[0u32; 4]; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = [K32[4 * i + 3], K32[4 * i + 2], K32[4 * i + 1], K32[4 * i]];
        i += 1;
    }
    out
};

const H256: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; READ];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(hex(&hasher.finish()))
}

pub fn hex(digest: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}

pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    held: usize,
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: H256,
            block: [0; 64],
            held: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.held > 0 {
            let take = (64 - self.held).min(data.len());
            self.block[self.held..self.held + take].copy_from_slice(&data[..take]);
            self.held += take;
            data = &data[take..];
            if self.held < 64 {
                return;
            }
            compress(&mut self.state, std::slice::from_ref(&self.block));
            self.held = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        if !blocks.is_empty() {
            compress(&mut self.state, blocks);
        }
        self.block[..rest.len()].copy_from_slice(rest);
        self.held = rest.len();
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        let mut tail = [0u8; 128];
        tail[..self.held].copy_from_slice(&self.block[..self.held]);
        tail[self.held] = 0x80;
        let used = if self.held < 56 { 64 } else { 128 };
        tail[used - 8..used].copy_from_slice(&bits.to_be_bytes());
        let (blocks, _) = tail[..used].as_chunks::<64>();
        compress(&mut self.state, blocks);
        let mut out = [0u8; 32];
        for (o, s) in out.chunks_exact_mut(4).zip(self.state) {
            o.copy_from_slice(&s.to_be_bytes());
        }
        out
    }
}

fn compress(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("sha")
        && std::arch::is_x86_feature_detected!("sse2")
        && std::arch::is_x86_feature_detected!("ssse3")
        && std::arch::is_x86_feature_detected!("sse4.1")
    {
        unsafe { x86::compress(state, blocks) };
        return;
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("sha2") {
        unsafe { arm::compress(state, blocks) };
        return;
    }
    soft::compress(state, blocks);
}

mod soft {
    use super::K32X4;

    #[inline(always)]
    fn right(v: [u32; 4], o: u32) -> [u32; 4] {
        [v[0] >> o, v[1] >> o, v[2] >> o, v[3] >> o]
    }

    #[inline(always)]
    fn left(v: [u32; 4], o: u32) -> [u32; 4] {
        [v[0] << o, v[1] << o, v[2] << o, v[3] << o]
    }

    #[inline(always)]
    fn or(a: [u32; 4], b: [u32; 4]) -> [u32; 4] {
        [a[0] | b[0], a[1] | b[1], a[2] | b[2], a[3] | b[3]]
    }

    #[inline(always)]
    fn xor(a: [u32; 4], b: [u32; 4]) -> [u32; 4] {
        [a[0] ^ b[0], a[1] ^ b[1], a[2] ^ b[2], a[3] ^ b[3]]
    }

    #[inline(always)]
    fn add(a: [u32; 4], b: [u32; 4]) -> [u32; 4] {
        [
            a[0].wrapping_add(b[0]),
            a[1].wrapping_add(b[1]),
            a[2].wrapping_add(b[2]),
            a[3].wrapping_add(b[3]),
        ]
    }

    fn load(v2: [u32; 4], v3: [u32; 4]) -> [u32; 4] {
        [v3[3], v2[0], v2[1], v2[2]]
    }

    fn swap(v0: [u32; 4]) -> [u32; 4] {
        [v0[2], v0[3], v0[0], v0[1]]
    }

    fn msg1(v0: [u32; 4], v1: [u32; 4]) -> [u32; 4] {
        let x = load(v0, v1);
        let t1 = or(right(x, 7), left(x, 25));
        let t2 = or(right(x, 18), left(x, 14));
        let t3 = right(x, 3);
        add(v0, xor(xor(t1, t2), t3))
    }

    fn sigma1(a: u32) -> u32 {
        a.rotate_right(17) ^ a.rotate_right(19) ^ (a >> 10)
    }

    fn msg2(v4: [u32; 4], v3: [u32; 4]) -> [u32; 4] {
        let [x3, x2, x1, x0] = v4;
        let [w15, w14, _, _] = v3;
        let w16 = x0.wrapping_add(sigma1(w14));
        let w17 = x1.wrapping_add(sigma1(w15));
        let w18 = x2.wrapping_add(sigma1(w16));
        let w19 = x3.wrapping_add(sigma1(w17));
        [w19, w18, w17, w16]
    }

    fn big_sigma0(a: u32) -> u32 {
        a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)
    }

    fn big_sigma1(a: u32) -> u32 {
        a.rotate_right(6) ^ a.rotate_right(11) ^ a.rotate_right(25)
    }

    fn choose(a: u32, b: u32, c: u32) -> u32 {
        c ^ (a & (b ^ c))
    }

    fn majority(a: u32, b: u32, c: u32) -> u32 {
        (a & b) ^ (a & c) ^ (b & c)
    }

    fn round_x2(cdgh: [u32; 4], abef: [u32; 4], wk: [u32; 4]) -> [u32; 4] {
        let [_, _, wk1, wk0] = wk;
        let [a0, b0, e0, f0] = abef;
        let [c0, d0, g0, h0] = cdgh;
        let x0 = big_sigma1(e0)
            .wrapping_add(choose(e0, f0, g0))
            .wrapping_add(wk0)
            .wrapping_add(h0);
        let y0 = big_sigma0(a0).wrapping_add(majority(a0, b0, c0));
        let (a1, b1, c1, d1, e1, f1, g1, h1) = (
            x0.wrapping_add(y0),
            a0,
            b0,
            c0,
            x0.wrapping_add(d0),
            e0,
            f0,
            g0,
        );
        let x1 = big_sigma1(e1)
            .wrapping_add(choose(e1, f1, g1))
            .wrapping_add(wk1)
            .wrapping_add(h1);
        let y1 = big_sigma0(a1).wrapping_add(majority(a1, b1, c1));
        [x1.wrapping_add(y1), a1, x1.wrapping_add(d1), e1]
    }

    fn schedule(v0: [u32; 4], v1: [u32; 4], v2: [u32; 4], v3: [u32; 4]) -> [u32; 4] {
        msg2(add(msg1(v0, v1), load(v2, v3)), v3)
    }

    macro_rules! rounds4 {
        ($abef:ident, $cdgh:ident, $rest:expr, $i:expr) => {{
            let t1 = add($rest, K32X4[$i]);
            $cdgh = round_x2($cdgh, $abef, t1);
            let t2 = swap(t1);
            $abef = round_x2($abef, $cdgh, t2);
        }};
    }

    macro_rules! schedule_rounds4 {
        ($abef:ident, $cdgh:ident, $w0:expr, $w1:expr, $w2:expr, $w3:expr, $w4:expr, $i:expr) => {{
            $w4 = schedule($w0, $w1, $w2, $w3);
            rounds4!($abef, $cdgh, $w4, $i);
        }};
    }

    fn block_u32(state: &mut [u32; 8], block: &[u32; 16]) {
        let mut abef = [state[0], state[1], state[4], state[5]];
        let mut cdgh = [state[2], state[3], state[6], state[7]];
        let mut w0 = [block[3], block[2], block[1], block[0]];
        let mut w1 = [block[7], block[6], block[5], block[4]];
        let mut w2 = [block[11], block[10], block[9], block[8]];
        let mut w3 = [block[15], block[14], block[13], block[12]];
        let mut w4;
        rounds4!(abef, cdgh, w0, 0);
        rounds4!(abef, cdgh, w1, 1);
        rounds4!(abef, cdgh, w2, 2);
        rounds4!(abef, cdgh, w3, 3);
        schedule_rounds4!(abef, cdgh, w0, w1, w2, w3, w4, 4);
        schedule_rounds4!(abef, cdgh, w1, w2, w3, w4, w0, 5);
        schedule_rounds4!(abef, cdgh, w2, w3, w4, w0, w1, 6);
        schedule_rounds4!(abef, cdgh, w3, w4, w0, w1, w2, 7);
        schedule_rounds4!(abef, cdgh, w4, w0, w1, w2, w3, 8);
        schedule_rounds4!(abef, cdgh, w0, w1, w2, w3, w4, 9);
        schedule_rounds4!(abef, cdgh, w1, w2, w3, w4, w0, 10);
        schedule_rounds4!(abef, cdgh, w2, w3, w4, w0, w1, 11);
        schedule_rounds4!(abef, cdgh, w3, w4, w0, w1, w2, 12);
        schedule_rounds4!(abef, cdgh, w4, w0, w1, w2, w3, 13);
        schedule_rounds4!(abef, cdgh, w0, w1, w2, w3, w4, 14);
        schedule_rounds4!(abef, cdgh, w1, w2, w3, w4, w0, 15);
        let [a, b, e, f] = abef;
        let [c, d, g, h] = cdgh;
        for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }

    pub fn compress(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
        let mut words = [0u32; 16];
        let mut copy = *state;
        for block in blocks {
            for (o, chunk) in words.iter_mut().zip(block.as_chunks::<4>().0) {
                *o = u32::from_be_bytes(*chunk);
            }
            block_u32(&mut copy, &words);
        }
        *state = copy;
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::K32X4;
    use std::arch::x86_64::*;

    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    fn schedule(v0: __m128i, v1: __m128i, v2: __m128i, v3: __m128i) -> __m128i {
        let t1 = _mm_sha256msg1_epu32(v0, v1);
        let t2 = _mm_alignr_epi8(v3, v2, 4);
        let t3 = _mm_add_epi32(t1, t2);
        _mm_sha256msg2_epu32(t3, v3)
    }

    macro_rules! rounds4 {
        ($abef:ident, $cdgh:ident, $rest:expr, $i:expr) => {{
            let k = K32X4[$i];
            let kv = _mm_set_epi32(k[0] as i32, k[1] as i32, k[2] as i32, k[3] as i32);
            let t1 = _mm_add_epi32($rest, kv);
            $cdgh = _mm_sha256rnds2_epu32($cdgh, $abef, t1);
            let t2 = _mm_shuffle_epi32(t1, 0x0E);
            $abef = _mm_sha256rnds2_epu32($abef, $cdgh, t2);
        }};
    }

    macro_rules! schedule_rounds4 {
        ($abef:ident, $cdgh:ident, $w0:expr, $w1:expr, $w2:expr, $w3:expr, $w4:expr, $i:expr) => {{
            $w4 = schedule($w0, $w1, $w2, $w3);
            rounds4!($abef, $cdgh, $w4, $i);
        }};
    }

    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    pub fn compress(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
        let mask = _mm_set_epi64x(
            0x0C0D_0E0F_0809_0A0Bu64 as i64,
            0x0405_0607_0001_0203u64 as i64,
        );
        let state_ptr = state.as_ptr() as *const __m128i;
        let (dcba, efgh) = unsafe {
            (
                _mm_loadu_si128(state_ptr),
                _mm_loadu_si128(state_ptr.add(1)),
            )
        };
        let cdab = _mm_shuffle_epi32(dcba, 0xB1);
        let efgh = _mm_shuffle_epi32(efgh, 0x1B);
        let mut abef = _mm_alignr_epi8(cdab, efgh, 8);
        let mut cdgh = _mm_blend_epi16(efgh, cdab, 0xF0);
        for block in blocks {
            let abef_save = abef;
            let cdgh_save = cdgh;
            let data_ptr = block.as_ptr() as *const __m128i;
            let (mut w0, mut w1, mut w2, mut w3) = unsafe {
                (
                    _mm_shuffle_epi8(_mm_loadu_si128(data_ptr), mask),
                    _mm_shuffle_epi8(_mm_loadu_si128(data_ptr.add(1)), mask),
                    _mm_shuffle_epi8(_mm_loadu_si128(data_ptr.add(2)), mask),
                    _mm_shuffle_epi8(_mm_loadu_si128(data_ptr.add(3)), mask),
                )
            };
            let mut w4;
            rounds4!(abef, cdgh, w0, 0);
            rounds4!(abef, cdgh, w1, 1);
            rounds4!(abef, cdgh, w2, 2);
            rounds4!(abef, cdgh, w3, 3);
            schedule_rounds4!(abef, cdgh, w0, w1, w2, w3, w4, 4);
            schedule_rounds4!(abef, cdgh, w1, w2, w3, w4, w0, 5);
            schedule_rounds4!(abef, cdgh, w2, w3, w4, w0, w1, 6);
            schedule_rounds4!(abef, cdgh, w3, w4, w0, w1, w2, 7);
            schedule_rounds4!(abef, cdgh, w4, w0, w1, w2, w3, 8);
            schedule_rounds4!(abef, cdgh, w0, w1, w2, w3, w4, 9);
            schedule_rounds4!(abef, cdgh, w1, w2, w3, w4, w0, 10);
            schedule_rounds4!(abef, cdgh, w2, w3, w4, w0, w1, 11);
            schedule_rounds4!(abef, cdgh, w3, w4, w0, w1, w2, 12);
            schedule_rounds4!(abef, cdgh, w4, w0, w1, w2, w3, 13);
            schedule_rounds4!(abef, cdgh, w0, w1, w2, w3, w4, 14);
            schedule_rounds4!(abef, cdgh, w1, w2, w3, w4, w0, 15);
            abef = _mm_add_epi32(abef, abef_save);
            cdgh = _mm_add_epi32(cdgh, cdgh_save);
        }
        let feba = _mm_shuffle_epi32(abef, 0x1B);
        let dchg = _mm_shuffle_epi32(cdgh, 0xB1);
        let dcba = _mm_blend_epi16(feba, dchg, 0xF0);
        let hgef = _mm_alignr_epi8(dchg, feba, 8);
        let out = state.as_mut_ptr() as *mut __m128i;
        unsafe {
            _mm_storeu_si128(out, dcba);
            _mm_storeu_si128(out.add(1), hgef);
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::K32;
    use std::arch::aarch64::*;

    #[target_feature(enable = "sha2")]
    pub fn compress(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
        let k = |t: usize| unsafe { vld1q_u32(K32[t..t + 4].as_ptr()) };
        let (mut abcd, mut efgh) = unsafe {
            (
                vld1q_u32(state[0..4].as_ptr()),
                vld1q_u32(state[4..8].as_ptr()),
            )
        };
        for block in blocks {
            let abcd_orig = abcd;
            let efgh_orig = efgh;
            let word = |at: usize| unsafe {
                vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(block[at..at + 16].as_ptr())))
            };
            let mut s = [word(0), word(16), word(32), word(48)];
            for t in 0..16 {
                let i = t % 4;
                if t >= 4 {
                    s[i] = vsha256su1q_u32(
                        vsha256su0q_u32(s[i], s[(i + 1) % 4]),
                        s[(i + 2) % 4],
                        s[(i + 3) % 4],
                    );
                }
                let tmp = vaddq_u32(s[i], k(4 * t));
                let abcd_prev = abcd;
                abcd = vsha256hq_u32(abcd_prev, efgh, tmp);
                efgh = vsha256h2q_u32(efgh, abcd_prev, tmp);
            }
            abcd = vaddq_u32(abcd, abcd_orig);
            efgh = vaddq_u32(efgh, efgh_orig);
        }
        unsafe {
            vst1q_u32(state[0..4].as_mut_ptr(), abcd);
            vst1q_u32(state[4..8].as_mut_ptr(), efgh);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Sha256, hex};

    fn digest(parts: &[&[u8]]) -> String {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p);
        }
        hex(&h.finish())
    }

    #[test]
    fn known_vectors() {
        assert_eq!(
            digest(&[]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            digest(&[b"abc"]),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            digest(&[
                b"abcdbcdecdefdefgefghfghighijhi",
                b"jkijkljklmklmnlmnomnopnopq"
            ]),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let a = vec![b'a'; 1_000_000];
        assert_eq!(
            digest(&[&a[..333], &a[333..999_937], &a[999_937..]]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }
}
