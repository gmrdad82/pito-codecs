use super::*;

#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
    bits: usize,
}

impl Writer {
    fn put(&mut self, n: u32, v: u32) {
        for i in (0..n).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (v >> i) & 1 != 0 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }

    fn ones(&mut self, n: u32) {
        for _ in 0..n {
            self.put(1, 1);
        }
    }

    fn head(&mut self, tag: u32, partial: Option<u32>, shifted: u32, escape: bool) {
        self.put(3, tag);
        self.put(4, 0);
        self.put(12, 0);
        self.put(1, partial.is_some() as u32);
        self.put(2, shifted);
        self.put(1, escape as u32);
        if let Some(n) = partial {
            self.put(16, n >> 16);
            self.put(16, n & 0xffff);
        }
    }

    fn end(&mut self) {
        self.put(3, 7);
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn cookie(frame_length: u32, depth: u8, channels: u8, rate: u32) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&frame_length.to_be_bytes());
    c.extend_from_slice(&[0, depth, 40, 10, 14, channels]);
    c.extend_from_slice(&255u16.to_be_bytes());
    c.extend_from_slice(&0u32.to_be_bytes());
    c.extend_from_slice(&0u32.to_be_bytes());
    c.extend_from_slice(&rate.to_be_bytes());
    c
}

fn atom(kind: &[u8; 4], body: &[u8], full: bool) -> Vec<u8> {
    let extra = if full { 4 } else { 0 };
    let mut a = ((8 + extra + body.len()) as u32).to_be_bytes().to_vec();
    a.extend_from_slice(kind);
    if full {
        a.extend_from_slice(&[0; 4]);
    }
    a.extend_from_slice(body);
    a
}

fn decoder(frame_length: u32, depth: u8, channels: u8) -> Decoder {
    Decoder::new(&cookie(frame_length, depth, channels, 44_100)).unwrap()
}

fn seq(n: usize, depth: u8, salt: u32) -> Vec<i32> {
    let max = (1i64 << (depth - 1)) - 1;
    let min = -(1i64 << (depth - 1));
    let mut s = 0x9e37_79b9u32 ^ salt;
    (0..n)
        .map(|i| match i % 97 {
            0 => max as i32,
            1 => min as i32,
            2 => 0,
            _ => {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let span = (max - min + 1) as u64;
                (min + ((s as u64 * 0x1_0000_0001) % span) as i64) as i32
            }
        })
        .collect()
}

fn put_signed(w: &mut Writer, bits: u32, v: i32) {
    if bits <= 16 {
        w.put(bits, v as u32 & ((1u64 << bits) - 1) as u32);
    } else {
        let u = v as u32;
        w.put(16, (u >> (bits - 16)) & 0xffff);
        w.put(bits - 16, u & ((1u32 << (bits - 16)) - 1));
    }
}

#[test]
fn a_cookie_parses_bare_inside_its_atom_and_after_frma() {
    let bare = cookie(4096, 24, 2, 96_000);
    let want = Config {
        frame_length: 4096,
        compatible_version: 0,
        bit_depth: 24,
        pb: 40,
        mb: 10,
        kb: 14,
        channels: 2,
        max_run: 255,
        max_frame_bytes: 0,
        avg_bit_rate: 0,
        sample_rate: 96_000,
    };
    assert_eq!(Config::parse(&bare).unwrap(), want);
    let wrapped = atom(b"alac", &bare, true);
    assert_eq!(Config::parse(&wrapped).unwrap(), want);
    let mut frma = atom(b"frma", b"alac", false);
    frma.extend_from_slice(&wrapped);
    assert_eq!(Config::parse(&frma).unwrap(), want);
    let mut layout = bare.clone();
    layout.extend_from_slice(&[0; 24]);
    assert_eq!(Config::parse(&layout).unwrap(), want);
}

#[test]
fn short_or_unsupported_cookies_are_refused() {
    assert!(Config::parse(&[]).is_err());
    assert!(Config::parse(&cookie(4096, 16, 2, 44_100)[..23]).is_err());
    assert!(Config::parse(&atom(b"alac", &[0; 10], true)).is_err());
    for (frame_length, depth, channels) in [
        (4096, 8, 2),
        (4096, 18, 2),
        (4096, 33, 2),
        (4096, 16, 0),
        (4096, 16, 9),
        (0, 16, 2),
        (MAX_FRAME_LENGTH + 1, 16, 2),
        (u32::MAX, 16, 2),
    ] {
        assert!(
            Decoder::new(&cookie(frame_length, depth, channels, 44_100)).is_err(),
            "{frame_length} {depth} {channels}"
        );
    }
    assert!(Decoder::new(&cookie(4096, 16, 2, 0)).is_err());
    let mut v1 = cookie(4096, 16, 2, 44_100);
    v1[4] = 1;
    assert!(matches!(Decoder::new(&v1), Err(Error::Unsupported(_))));
}

#[test]
fn the_cookie_is_found_in_an_entry_and_inside_wave() {
    let bare = cookie(4096, 16, 2, 48_000);
    let alac = atom(b"alac", &bare, true);
    let mut boxes = atom(b"btrt", &[0; 12], false);
    boxes.extend_from_slice(&alac);
    assert_eq!(find_cookie(&boxes), Some(alac.as_slice()));
    let mut wave_body = atom(b"frma", b"alac", false);
    wave_body.extend_from_slice(&alac);
    wave_body.extend_from_slice(&[0, 0, 0, 8, 0, 0, 0, 0]);
    let wave = atom(b"wave", &wave_body, false);
    assert_eq!(find_cookie(&wave), Some(alac.as_slice()));
    assert_eq!(find_cookie(&atom(b"btrt", &[0; 12], false)), None);
    assert_eq!(find_cookie(&[0, 0, 0, 200, b'a', b'l', b'a', b'c']), None);
    assert_eq!(find_cookie(&[0, 0, 0, 3, b'a', b'l', b'a', b'c']), None);
}

#[test]
fn escape_frames_decode_exactly_at_every_depth() {
    for (depth, channels) in [
        (16u8, 1u8),
        (16, 2),
        (20, 2),
        (24, 1),
        (24, 2),
        (32, 1),
        (32, 2),
    ] {
        let n = 257usize;
        let mut d = decoder(n as u32, depth, channels);
        let want: Vec<Vec<i32>> = (0..channels as u32).map(|c| seq(n, depth, c)).collect();
        let mut w = Writer::default();
        let bits = depth as u32;
        if channels == 1 {
            w.head(ID_SCE, None, 0, true);
            for &v in &want[0] {
                put_signed(&mut w, bits, v);
            }
        } else {
            w.head(ID_CPE, None, 0, true);
            for (&l, &r) in want[0].iter().zip(&want[1]) {
                put_signed(&mut w, bits, l);
                put_signed(&mut w, bits, r);
            }
        }
        w.end();
        let got = d.decode(&w.finish()).unwrap().to_vec();
        let interleaved: Vec<i32> = (0..n)
            .flat_map(|i| want.iter().map(move |c| c[i]))
            .collect();
        assert_eq!(got, interleaved, "{depth}-bit, {channels} channels");
    }
}

#[test]
fn a_partial_frame_of_odd_length_sets_the_sample_count() {
    let mut d = decoder(4096, 16, 2);
    let n = 1001usize;
    let l = seq(n, 16, 7);
    let r = seq(n, 16, 8);
    let mut w = Writer::default();
    w.head(ID_CPE, Some(n as u32), 0, true);
    for (&a, &b) in l.iter().zip(&r) {
        put_signed(&mut w, 16, a);
        put_signed(&mut w, 16, b);
    }
    let packet = w.finish();
    let got = d.decode(&packet).unwrap();
    assert_eq!(got.len(), n * 2);
    assert!((0..n).all(|i| got[2 * i] == l[i] && got[2 * i + 1] == r[i]));
    let mut f = Vec::new();
    assert_eq!(d.decode_into(&packet, &mut f).unwrap(), n);
    assert_eq!(f[0], l[0] as f32 / 32768.0);
    assert_eq!(f[1], r[0] as f32 / 32768.0);
}

#[test]
fn a_partial_frame_longer_than_the_cookie_allows_is_refused() {
    let mut d = decoder(1024, 16, 1);
    let mut w = Writer::default();
    w.head(ID_SCE, Some(1025), 0, true);
    w.put(16, 0);
    assert!(d.decode(&w.finish()).is_err());
}

#[test]
fn fill_and_data_elements_are_skipped() {
    let mut d = decoder(4, 16, 1);
    let mut w = Writer::default();
    w.put(3, ID_FIL);
    w.put(4, 15);
    w.put(8, 3);
    for _ in 0..17 {
        w.put(8, 0x5a);
    }
    w.put(3, ID_DSE);
    w.put(4, 0);
    w.put(1, 1);
    w.put(8, 2);
    while !w.bits.is_multiple_of(8) {
        w.put(1, 1);
    }
    w.put(16, 0xffff);
    w.head(ID_SCE, None, 0, true);
    for v in [1, -1, 32767, -32768] {
        put_signed(&mut w, 16, v);
    }
    assert_eq!(d.decode(&w.finish()).unwrap(), &[1, -1, 32767, -32768]);
}

#[test]
fn coupling_and_program_elements_are_refused() {
    for tag in [ID_CCE, ID_PCE] {
        let mut d = decoder(16, 16, 1);
        let mut w = Writer::default();
        w.put(3, tag);
        w.put(29, 0);
        assert!(matches!(d.decode(&w.finish()), Err(Error::Unsupported(_))));
    }
}

#[test]
fn an_end_tag_before_any_channel_gives_silence() {
    let mut d = decoder(8, 24, 2);
    let mut w = Writer::default();
    w.end();
    assert_eq!(d.decode(&w.finish()).unwrap(), &[0; 16]);
}

#[test]
fn three_channels_come_out_in_element_order() {
    let mut d = decoder(3, 16, 3);
    let mut w = Writer::default();
    w.head(ID_SCE, None, 0, true);
    for v in [10, 11, 12] {
        put_signed(&mut w, 16, v);
    }
    w.head(ID_CPE, None, 0, true);
    for (l, r) in [(20, 30), (21, 31), (22, 32)] {
        put_signed(&mut w, 16, l);
        put_signed(&mut w, 16, r);
    }
    assert_eq!(
        d.decode(&w.finish()).unwrap(),
        &[10, 20, 30, 11, 21, 31, 12, 22, 32]
    );
}

#[test]
fn a_pair_that_would_overflow_the_channels_is_left_silent() {
    let mut d = decoder(2, 16, 1);
    let mut w = Writer::default();
    w.head(ID_CPE, None, 0, true);
    for v in [5, 6, 7, 8] {
        put_signed(&mut w, 16, v);
    }
    assert_eq!(d.decode(&w.finish()).unwrap(), &[0, 0]);
}

fn compressed_mono(w: &mut Writer, n: u32, mode: u32, coefs: &[i16], den: u32) {
    w.head(ID_SCE, Some(n), 0, false);
    w.put(8, 0);
    w.put(8, 0);
    w.put(8, (mode << 4) | den);
    w.put(8, (4 << 5) | coefs.len() as u32);
    for &c in coefs {
        w.put(16, c as u16 as u32);
    }
}

#[test]
fn adaptive_golomb_codes_signs_and_zero_runs() {
    let mut d = decoder(64, 16, 1);
    let mut w = Writer::default();
    compressed_mono(&mut w, 1, 0, &[], 0);
    w.ones(3);
    w.put(1, 0);
    assert_eq!(d.decode(&w.finish()).unwrap(), &[-2]);

    let mut w = Writer::default();
    compressed_mono(&mut w, 21, 0, &[], 0);
    w.put(1, 0);
    w.ones(1);
    w.put(1, 0);
    w.put(4, 5);
    w.ones(4);
    w.put(1, 0);
    let mut want = vec![0; 20];
    want.push(-3);
    assert_eq!(d.decode(&w.finish()).unwrap(), want.as_slice());

    let mut w = Writer::default();
    compressed_mono(&mut w, 16, 0, &[], 0);
    w.put(1, 0);
    w.ones(1);
    w.put(1, 0);
    w.put(3, 0);
    assert_eq!(d.decode(&w.finish()).unwrap(), &[0; 16]);
}

#[test]
fn an_escaped_golomb_value_reads_the_full_width() {
    let mut d = decoder(4, 16, 1);
    let mut w = Writer::default();
    compressed_mono(&mut w, 1, 0, &[], 0);
    w.ones(9);
    w.put(16, 65_535);
    assert_eq!(d.decode(&w.finish()).unwrap(), &[-32768]);
}

#[test]
fn mode_one_integrates_the_residual_first() {
    let mut d = decoder(4, 16, 1);
    let mut w = Writer::default();
    compressed_mono(&mut w, 2, 1, &[], 0);
    w.ones(4);
    w.put(1, 0);
    w.ones(4);
    w.put(1, 0);
    assert_eq!(d.decode(&w.finish()).unwrap(), &[2, 4]);
}

#[test]
fn the_unrolled_predictors_match_the_general_one() {
    let mut s = 0x2545_f491u32;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    for round in 0..64 {
        let num = 40 + (next() % 600) as usize;
        let chanbits = [16u32, 17, 20, 24, 25, 32][round % 6];
        let denshift = 1 + next() % 15;
        let span = 1u32 << (chanbits.min(31) - 2);
        let pc: Vec<i32> = (0..num)
            .map(|i| match i % 37 {
                0 => 0,
                _ => (next() % span) as i32 - (span / 2) as i32,
            })
            .collect();
        for n in [4usize, 8] {
            let seed: Vec<i16> = (0..n).map(|_| (next() % 2048) as i16 - 1024).collect();
            let chanshift = 32 - chanbits;
            let denhalf = 1i32.wrapping_shl(denshift - 1);
            let mut a = vec![0; num];
            let mut b = vec![0; num];
            let mut ca = seed.clone();
            let mut cb = seed.clone();
            a[0] = pc[0];
            b[0] = pc[0];
            for j in 1..=n {
                let v = pc[j].wrapping_add(a[j - 1]);
                a[j] = v.wrapping_shl(chanshift).wrapping_shr(chanshift);
                b[j] = a[j];
            }
            if n == 4 {
                dp::fixed::<4>(&pc, &mut a, num, &mut ca, chanshift, denshift, denhalf);
            } else {
                dp::fixed::<8>(&pc, &mut a, num, &mut ca, chanshift, denshift, denhalf);
            }
            dp::general(&pc, &mut b, num, &mut cb, n, chanshift, denshift, denhalf);
            assert_eq!(a, b, "order {n}, {chanbits} bits, shift {denshift}");
            assert_eq!(ca, cb, "order {n} coefficients");
        }
    }
}

#[test]
fn predictors_shorter_than_their_order_stay_in_bounds() {
    for numactive in [0usize, 1, 4, 8, 12, 30, 31] {
        for num in 0..12usize {
            let pc: Vec<i32> = (0..num as i32).collect();
            let mut out = vec![0; num];
            let mut coefs = [3i16; 32];
            dp::unpc_block(&pc, &mut out, num, &mut coefs, numactive, 16, 9);
            if num > 0 {
                assert_eq!(out[0], 0);
            }
        }
    }
}

fn lcg(s: &mut u64) -> u64 {
    *s = s
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *s >> 33
}

#[test]
fn truncated_packets_end_cleanly() {
    let mut d = decoder(300, 24, 2);
    let mut w = Writer::default();
    w.head(ID_CPE, None, 0, true);
    for i in 0..300 {
        put_signed(&mut w, 24, i * 1000 - 150_000);
        put_signed(&mut w, 24, -i);
    }
    let packet = w.finish();
    assert!(d.decode(&packet).is_ok());
    for cut in 0..packet.len() {
        assert!(d.decode(&packet[..cut]).is_err(), "cut at {cut}");
    }
    assert!(d.decode(&packet).is_ok());
}

#[test]
fn garbage_and_flipped_bits_never_panic() {
    let mut s = 0x1234_5678_9abc_def0u64;
    let mut valid = Writer::default();
    compressed_mono(&mut valid, 21, 0, &[], 0);
    valid.put(1, 0);
    valid.ones(1);
    valid.put(1, 0);
    valid.put(4, 5);
    valid.ones(4);
    valid.put(1, 0);
    let valid = valid.finish();
    for config in [
        (4096u32, 16u8, 1u8),
        (4096, 16, 2),
        (1152, 20, 2),
        (4096, 24, 2),
        (64, 24, 6),
        (4096, 32, 2),
        (17, 32, 8),
    ] {
        let mut d = decoder(config.0, config.1, config.2);
        for _ in 0..400 {
            let len = (lcg(&mut s) % 600) as usize;
            let junk: Vec<u8> = (0..len).map(|_| lcg(&mut s) as u8).collect();
            let _ = d.decode(&junk);
            let mut flipped = valid.clone();
            for _ in 0..1 + lcg(&mut s) % 4 {
                let at = (lcg(&mut s) as usize) % flipped.len();
                flipped[at] ^= 1 << (lcg(&mut s) % 8);
            }
            let _ = d.decode(&flipped);
            let mut out = Vec::new();
            let _ = d.decode_into(&flipped, &mut out);
        }
    }
}

#[test]
fn a_decoder_recovers_after_a_bad_packet() {
    let mut d = decoder(64, 16, 1);
    let mut w = Writer::default();
    compressed_mono(&mut w, 1, 0, &[], 0);
    w.ones(3);
    w.put(1, 0);
    let good = w.finish();
    assert!(matches!(d.decode(&[0x40]), Err(Error::Unsupported(_))));
    assert_eq!(d.decode(&good).unwrap(), &[-2]);
}

fn golomb(w: &mut Writer, bits: u32, values: &[i32]) {
    for &x in values {
        assert!(x.unsigned_abs() >= 2);
        let n = if x > 0 {
            2 * x as u32
        } else {
            (-2 * x as i64 - 1) as u32
        };
        w.ones(9);
        w.put(bits, n);
    }
}

fn plain_predictor(w: &mut Writer) {
    w.put(8, 0);
    w.put(8, 4 << 5);
}

fn matrixed_pair(w: &mut Writer, n: u32, shifted: u32, mix: (u32, u32), lows: &[u32]) {
    w.head(ID_CPE, Some(n), shifted, false);
    w.put(8, mix.0);
    w.put(8, mix.1);
    plain_predictor(w);
    plain_predictor(w);
    for &low in lows {
        w.put(shifted * 8, low);
    }
}

#[test]
fn a_matrixed_pair_unmixes_like_the_reference() {
    let mut d = decoder(16, 16, 2);
    let mut w = Writer::default();
    matrixed_pair(&mut w, 3, 0, (2, 3), &[]);
    golomb(&mut w, 17, &[5, -7, 300]);
    golomb(&mut w, 17, &[-3, 2, -1000]);
    w.end();
    assert_eq!(d.decode(&w.finish()).unwrap(), &[5, 8, -6, -8, 50, 1050]);

    let mut w = Writer::default();
    matrixed_pair(&mut w, 2, 0, (31, 1), &[]);
    golomb(&mut w, 17, &[5, 9]);
    golomb(&mut w, 17, &[-3, 4]);
    w.end();
    assert_eq!(d.decode(&w.finish()).unwrap(), &[3, 6, 13, 9]);
}

#[test]
fn a_32_bit_pair_shifted_by_two_bytes_keeps_its_low_words() {
    let mut d = decoder(16, 32, 2);
    let mut w = Writer::default();
    matrixed_pair(&mut w, 2, 2, (2, 3), &[0x1234, 0xfedc, 0x0001, 0x8000]);
    golomb(&mut w, 17, &[5, -7]);
    golomb(&mut w, 17, &[-3, 2]);
    w.end();
    assert_eq!(
        d.decode(&w.finish()).unwrap(),
        &[0x0005_1234, 0x0008_fedc, -393_215, -491_520]
    );
}

fn shifted_mono(w: &mut Writer, n: u32, lows: &[u32]) {
    w.head(ID_SCE, Some(n), 1, false);
    w.put(8, 0);
    w.put(8, 0);
    plain_predictor(w);
    for &low in lows {
        w.put(8, low);
    }
}

#[test]
fn a_24_bit_element_shifted_by_a_byte_puts_its_low_bytes_back() {
    let mut d = decoder(16, 24, 1);
    let mut w = Writer::default();
    shifted_mono(&mut w, 3, &[0xab, 0x00, 0xff]);
    golomb(&mut w, 16, &[5, -7, 300]);
    w.end();
    assert_eq!(d.decode(&w.finish()).unwrap(), &[1451, -1792, 77_055]);
}

#[test]
fn a_16_bit_element_shifted_by_a_byte_reads_and_drops_its_low_bytes() {
    let mut d = decoder(16, 16, 1);
    let mut w = Writer::default();
    shifted_mono(&mut w, 3, &[0xab, 0x00, 0xff]);
    golomb(&mut w, 8, &[5, -7, 100]);
    w.end();
    assert_eq!(d.decode(&w.finish()).unwrap(), &[5, -7, 100]);
}

#[test]
fn nothing_stale_reaches_a_shorter_frame_or_a_frame_that_ends_early() {
    let mut d = decoder(4, 24, 2);
    let mut w = Writer::default();
    w.head(ID_CPE, None, 1, false);
    w.put(8, 0);
    w.put(8, 0);
    plain_predictor(&mut w);
    plain_predictor(&mut w);
    for low in [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88] {
        w.put(8, low);
    }
    golomb(&mut w, 17, &[2, 3, 4, 5]);
    golomb(&mut w, 17, &[-2, -3, -4, -5]);
    w.end();
    let full = w.finish();
    let want = [529, -478, 819, -700, 1109, -922, 1399, -1144];
    assert_eq!(d.decode(&full).unwrap(), &want);

    let mut w = Writer::default();
    w.head(ID_CPE, Some(2), 0, true);
    for v in [1, 2, 3, 4] {
        put_signed(&mut w, 24, v);
    }
    w.end();
    assert_eq!(d.decode(&w.finish()).unwrap(), &[1, 2, 3, 4]);

    let mut w = Writer::default();
    w.end();
    assert_eq!(d.decode(&w.finish()).unwrap(), &[0; 8]);
    assert_eq!(d.decode(&full).unwrap(), &want);
}

fn escaped(w: &mut Writer, tag: u32, values: &[i32]) {
    w.head(tag, None, 0, true);
    for &v in values {
        put_signed(w, 16, v);
    }
}

fn wave(d: &mut Decoder, packet: &[u8]) -> Vec<i32> {
    let mut out = Vec::new();
    d.decode_into(packet, &mut out).unwrap();
    out.iter().map(|&s| (s * 32768.0) as i32).collect()
}

#[test]
fn surround_comes_out_of_decode_into_in_wave_order() {
    let mut d = decoder(2, 16, 3);
    let mut w = Writer::default();
    escaped(&mut w, ID_SCE, &[10, 11]);
    escaped(&mut w, ID_CPE, &[20, 30, 21, 31]);
    let three = w.finish();
    assert_eq!(d.decode(&three).unwrap(), &[10, 20, 30, 11, 21, 31]);
    assert_eq!(wave(&mut d, &three), [20, 30, 10, 21, 31, 11]);

    let mut d = decoder(1, 16, 6);
    let mut w = Writer::default();
    escaped(&mut w, ID_SCE, &[1]);
    escaped(&mut w, ID_CPE, &[2, 3]);
    escaped(&mut w, ID_CPE, &[4, 5]);
    escaped(&mut w, ID_LFE, &[6]);
    let six = w.finish();
    assert_eq!(d.decode(&six).unwrap(), &[1, 2, 3, 4, 5, 6]);
    assert_eq!(wave(&mut d, &six), [2, 3, 1, 6, 4, 5]);

    let mut d = decoder(1, 16, 8);
    let mut w = Writer::default();
    escaped(&mut w, ID_SCE, &[1]);
    escaped(&mut w, ID_CPE, &[2, 3]);
    escaped(&mut w, ID_CPE, &[4, 5]);
    escaped(&mut w, ID_CPE, &[6, 7]);
    escaped(&mut w, ID_LFE, &[8]);
    let eight = w.finish();
    assert_eq!(wave(&mut d, &eight), [4, 5, 1, 8, 6, 7, 2, 3]);

    let mut d = decoder(1, 16, 2);
    let mut w = Writer::default();
    escaped(&mut w, ID_CPE, &[2, 3]);
    assert_eq!(wave(&mut d, &w.finish()), [2, 3]);
}

#[test]
fn refused_names_what_the_decoder_will_not_take() {
    assert_eq!(
        refused(None).as_deref(),
        Some("ALAC without its magic cookie")
    );
    assert_eq!(
        refused(Some(&[])).as_deref(),
        Some("ALAC without its magic cookie")
    );
    assert_eq!(
        refused(Some(&[0; 10])).as_deref(),
        Some("ALAC with a damaged magic cookie")
    );
    let mut v1 = cookie(4096, 16, 2, 44_100);
    v1[4] = 1;
    assert_eq!(refused(Some(&v1)).as_deref(), Some("ALAC version 1"));
    assert_eq!(
        refused(Some(&cookie(4096, 8, 2, 44_100))).as_deref(),
        Some("8-bit ALAC")
    );
    assert_eq!(
        refused(Some(&cookie(4096, 16, 9, 44_100))).as_deref(),
        Some("ALAC with 9 channels")
    );
    assert_eq!(refused(Some(&cookie(4096, 24, 6, 192_000))), None);
    assert_eq!(
        refused(Some(&atom(b"alac", &cookie(4096, 16, 2, 48_000), true))),
        None
    );
}

#[cfg(feature = "mp4")]
fn sample_entry(qt_version: u16, extra: usize, tail: &[u8]) -> crate::mp4::SampleEntry {
    let mut raw = vec![0u8; 28 + extra];
    raw[8..10].copy_from_slice(&qt_version.to_be_bytes());
    raw.extend_from_slice(tail);
    crate::mp4::SampleEntry {
        format: crate::mp4::FourCc(*b"alac"),
        codec: crate::mp4::CodecConfig::Other(crate::mp4::FourCc(*b"alac")),
        video: None,
        audio: Some(crate::mp4::AudioParams {
            qt_version,
            ..Default::default()
        }),
        bitrate: None,
        raw,
    }
}

#[cfg(feature = "mp4")]
#[test]
fn the_cookie_is_found_in_every_quicktime_sample_entry() {
    let alac = atom(b"alac", &cookie(4096, 24, 2, 96_000), true);
    let mut wave_body = atom(b"frma", b"alac", false);
    wave_body.extend_from_slice(&alac);
    wave_body.extend_from_slice(&[0, 0, 0, 8, 0, 0, 0, 0]);
    let wave = atom(b"wave", &wave_body, false);
    let v0 = sample_entry(0, 0, &alac);
    assert_eq!(entry_cookie(&v0), Some(alac.as_slice()));
    let v1 = sample_entry(1, 16, &wave);
    assert_eq!(entry_cookie(&v1), Some(alac.as_slice()));
    let v2 = sample_entry(2, 36, &alac);
    assert_eq!(entry_cookie(&v2), Some(alac.as_slice()));
    let v1_called_v0 = sample_entry(0, 16, &wave);
    assert_eq!(entry_cookie(&v1_called_v0), Some(alac.as_slice()));
    let v2_called_v1 = sample_entry(1, 36, &alac);
    assert_eq!(entry_cookie(&v2_called_v1), Some(alac.as_slice()));
    for e in [
        sample_entry(0, 0, &[]),
        sample_entry(1, 16, &atom(b"btrt", &[0; 12], false)),
        sample_entry(2, 0, &[]),
    ] {
        assert_eq!(entry_cookie(&e), None);
    }
    let found = entry_cookie(&v2).unwrap();
    assert_eq!(Config::parse(found).unwrap().sample_rate, 96_000);
}

#[test]
fn a_packet_coded_with_apples_mean_clamp_decodes_on_the_retry() {
    let mut w = Writer::default();
    compressed_mono(&mut w, 4, 0, &[], 0);
    w.put(1, 0);
    w.put(4, 0);
    w.ones(9);
    w.put(24, 0xffff);
    w.put(13, 0b0_1000_0001_1111);
    w.ones(4);
    w.put(1, 0);
    w.put(12, 2);
    w.end();
    let packet = w.finish();
    let want = [0, 0, 32768, 0, 1039, 0, -8191, 0];
    let mut d = decoder(8, 24, 2);
    assert!(d.run(&packet).is_err());
    assert_eq!(d.decode(&packet).unwrap(), &want);
    d.apple = true;
    assert_eq!(d.run(&packet).unwrap(), 4);
    assert_eq!(&d.samples[..8], &want);

    let mut w = Writer::default();
    compressed_mono(&mut w, 2, 0, &[], 0);
    w.put(1, 0);
    w.put(4, 0);
    w.ones(9);
    w.put(24, 0xffff);
    w.end();
    let plain = w.finish();
    assert_eq!(d.decode(&plain).unwrap(), &[0, 0, 32768, 0]);
    assert!(!d.apple);

    let mut cut = packet.clone();
    cut.truncate(cut.len() - 3);
    assert!(d.decode(&cut).is_err());
}
