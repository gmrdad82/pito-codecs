use super::header::{Floor, HeaderReadError};
use super::*;
use crate::testref::Refs;

use super::decode_tests::refs;

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn new() -> BitWriter {
        BitWriter {
            out: Vec::new(),
            acc: 0,
            n: 0,
        }
    }

    fn put(&mut self, v: u64, bits: u32) -> &mut Self {
        for i in 0..bits {
            self.acc |= ((v >> i) & 1) << self.n;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
        self
    }

    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        for &x in b {
            self.put(x as u64, 8);
        }
        self
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

#[derive(Clone)]
struct Knobs {
    channels: u8,
    book0_dim: u64,
    book0_sparse: bool,
    sync: u32,
    vq_dim: u64,
    vq_entries: u32,
    vq_ordered_first_len: u64,
    vq_split: Option<u64>,
    lookup_type: u64,
    sequence_p: bool,
    value_bits_m1: u64,
    time_value: u64,
    floor_type: u64,
    floor0_order: u64,
    floor0_rate: u64,
    floor0_bark: u64,
    floor0_book: u64,
    floor1_partitions: u64,
    floor1_dim_m1: u64,
    floor1_subclass: u64,
    floor1_masterbook: u64,
    floor1_rangebits: u64,
    floor1_x: u64,
    floor1_book: u64,
    residue_type: u64,
    residue_begin: u64,
    residue_end: u64,
    residue_classifications: u64,
    residue_classbook: u64,
    residue_book: u64,
    residue_high_stage: bool,
    mapping_type: u64,
    couple: Option<(u64, u64)>,
    submaps: Option<(u64, u64)>,
    reserved: u64,
    submap_floor: u64,
    submap_residue: u64,
    windowtype: u64,
    mode_mapping: u64,
    framing: u64,
}

fn int_root(entries: u64, dim: u64) -> u64 {
    if dim == 0 {
        return 0;
    }
    let mut r = 0u64;
    while (r + 1)
        .checked_pow(dim as u32)
        .is_some_and(|p| p <= entries)
    {
        r += 1;
    }
    r
}

impl Knobs {
    fn ok(channels: u8) -> Knobs {
        Knobs {
            channels,
            book0_dim: 1,
            book0_sparse: false,
            sync: 0x564342,
            vq_dim: 2,
            vq_entries: 4,
            vq_ordered_first_len: 1,
            vq_split: None,
            lookup_type: 1,
            sequence_p: false,
            value_bits_m1: 0,
            time_value: 0,
            floor_type: 1,
            floor0_order: 8,
            floor0_rate: 44100,
            floor0_bark: 256,
            floor0_book: 1,
            floor1_partitions: 1,
            floor1_dim_m1: 0,
            floor1_subclass: 0,
            floor1_masterbook: 0,
            floor1_rangebits: 4,
            floor1_x: 5,
            floor1_book: 1,
            residue_type: 2,
            residue_begin: 0,
            residue_end: 64,
            residue_classifications: 1,
            residue_classbook: 0,
            residue_book: 1,
            residue_high_stage: false,
            mapping_type: 0,
            couple: if channels == 2 { Some((0, 1)) } else { None },
            submaps: None,
            reserved: 0,
            submap_floor: 0,
            submap_residue: 0,
            windowtype: 0,
            mode_mapping: 0,
            framing: 1,
        }
    }

    fn setup(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.put(5, 8).bytes(b"vorbis").put(1, 8);
        if self.book0_sparse {
            w.put(0x564342, 24)
                .put(self.book0_dim, 16)
                .put(3, 24)
                .put(0, 1)
                .put(1, 1);
            w.put(1, 1).put(0, 5).put(0, 1).put(1, 1).put(0, 5);
        } else {
            w.put(0x564342, 24)
                .put(self.book0_dim, 16)
                .put(2, 24)
                .put(0, 1)
                .put(0, 1);
            w.put(0, 5).put(0, 5);
        }
        w.put(0, 4);
        w.put(self.sync as u64, 24)
            .put(self.vq_dim, 16)
            .put(self.vq_entries as u64, 24)
            .put(1, 1);
        let entries = self.vq_entries as u64;
        w.put(self.vq_ordered_first_len, 5);
        match self.vq_split {
            Some(first) => {
                w.put(first, super::ilog(entries) as u32)
                    .put(entries - first, super::ilog(entries - first) as u32);
            }
            None => {
                w.put(entries, super::ilog(entries) as u32);
            }
        }
        w.put(self.lookup_type, 4);
        if self.lookup_type == 1 || self.lookup_type == 2 {
            w.put(3759144960, 32).put(1611661312, 32);
            w.put(self.value_bits_m1, 4).put(self.sequence_p as u64, 1);
            let values = if self.lookup_type == 1 {
                int_root(self.vq_entries as u64, self.vq_dim)
            } else {
                self.vq_entries as u64 * self.vq_dim
            };
            for i in 0..values {
                w.put(i & 1, self.value_bits_m1 as u32 + 1);
            }
        }
        w.put(0, 6).put(self.time_value, 16);
        w.put(0, 6).put(self.floor_type, 16);
        if self.floor_type == 0 {
            w.put(self.floor0_order, 8)
                .put(self.floor0_rate, 16)
                .put(self.floor0_bark, 16)
                .put(6, 6)
                .put(100, 8)
                .put(0, 4)
                .put(self.floor0_book, 8);
        } else {
            w.put(self.floor1_partitions, 5);
            for _ in 0..self.floor1_partitions {
                w.put(0, 4);
            }
            w.put(self.floor1_dim_m1, 3).put(self.floor1_subclass, 2);
            if self.floor1_subclass != 0 {
                w.put(self.floor1_masterbook, 8);
            }
            for _ in 0..1u64 << self.floor1_subclass {
                w.put(self.floor1_book, 8);
            }
            w.put(1, 2).put(self.floor1_rangebits, 4);
            let xs = self.floor1_partitions * (self.floor1_dim_m1 + 1);
            for i in 0..xs {
                w.put(
                    if i == 0 { self.floor1_x } else { 5 + i },
                    self.floor1_rangebits as u32,
                );
            }
        }
        w.put(0, 6).put(self.residue_type, 16);
        w.put(self.residue_begin, 24)
            .put(self.residue_end, 24)
            .put(7, 24);
        w.put(self.residue_classifications.wrapping_sub(1), 6)
            .put(self.residue_classbook, 8);
        for _ in 0..self.residue_classifications {
            if self.residue_high_stage {
                w.put(1, 3).put(1, 1).put(0b10000, 5);
            } else {
                w.put(1, 3).put(0, 1);
            }
        }
        for _ in 0..self.residue_classifications {
            w.put(self.residue_book, 8);
            if self.residue_high_stage {
                w.put(self.residue_book, 8);
            }
        }
        w.put(0, 6).put(self.mapping_type, 16);
        match self.submaps {
            Some((n, _)) => w.put(1, 1).put(n - 1, 4),
            None => w.put(0, 1),
        };
        match self.couple {
            Some((m, a)) => {
                let bits = super::ilog(self.channels.saturating_sub(1) as u64) as u32;
                w.put(1, 1).put(0, 8).put(m, bits).put(a, bits);
            }
            None => {
                w.put(0, 1);
            }
        }
        w.put(self.reserved, 2);
        let n = match self.submaps {
            Some((n, mux)) => {
                for _ in 0..self.channels {
                    w.put(mux, 4);
                }
                n
            }
            None => 1,
        };
        for _ in 0..n {
            w.put(0, 8)
                .put(self.submap_floor, 8)
                .put(self.submap_residue, 8);
        }
        w.put(0, 6)
            .put(0, 1)
            .put(self.windowtype, 16)
            .put(0, 16)
            .put(self.mode_mapping, 8);
        w.put(self.framing, 1);
        w.finish()
    }

    fn read(&self) -> Result<SetupHeader, HeaderReadError> {
        read_header_setup(&self.setup(), self.channels, (8, 11))
    }
}

#[test]
fn a_hand_built_setup_header_parses_to_its_fields() {
    let s = Knobs::ok(2).read().unwrap();
    assert_eq!(s.codebooks.len(), 2);
    assert_eq!(s.codebooks[0].codebook_dimensions, 1);
    assert_eq!(s.codebooks[0].codebook_entries, 2);
    assert!(s.codebooks[0].codebook_vq_lookup_vec.is_none());
    let vq = s.codebooks[1].codebook_vq_lookup_vec.as_ref().unwrap();
    assert_eq!(vq, &[-1.0, -1.0, 0.0, -1.0, -1.0, 0.0, 0.0, 0.0]);
    let Floor::TypeOne(f) = &s.floors[0] else {
        panic!("floor 0 is not type 1");
    };
    assert_eq!(f.floor1_multiplier, 2);
    assert_eq!(f.floor1_partition_class, [0]);
    assert_eq!(f.floor1_class_dimensions, [1]);
    assert_eq!(f.floor1_subclass_books, [vec![0i16]]);
    assert_eq!(f.floor1_x_list, [0, 16, 5]);
    assert_eq!(f.floor1_x_list_sorted, [(0, 0), (2, 5), (1, 16)]);
    let r = &s.residues[0];
    assert_eq!(
        (
            r.residue_type,
            r.residue_begin,
            r.residue_end,
            r.residue_partition_size
        ),
        (2, 0, 64, 8)
    );
    assert_eq!(r.residue_books[0].get_val(0), Some(1));
    assert_eq!(r.residue_books[0].get_val(1), None);
    let m = &s.mappings[0];
    assert_eq!(m.mapping_magnitudes, [0]);
    assert_eq!(m.mapping_angles, [1]);
    assert_eq!(m.mapping_mux, [0, 0]);
    assert_eq!(s.modes.len(), 1);
    assert!(!s.modes[0].mode_blockflag);
}

#[test]
fn hand_built_codebooks_decode_through_read_huffman_vq() {
    let s = Knobs::ok(1).read().unwrap();
    let book = &s.codebooks[1];
    let mut w = BitWriter::new();
    for code in [0b11u64, 0b00, 0b10, 0b01] {
        w.put(code >> 1, 1).put(code & 1, 1);
    }
    let data = w.finish();
    let mut cur = bitpacking::BitpackCursor::new(&data);
    assert_eq!(cur.read_huffman_vq(book), Ok(&[0.0f32, 0.0][..]));
    assert_eq!(cur.read_huffman_vq(book), Ok(&[-1.0f32, -1.0][..]));
    assert_eq!(cur.read_huffman_vq(book), Ok(&[-1.0f32, 0.0][..]));
    assert_eq!(cur.read_huffman_vq(book), Ok(&[0.0f32, -1.0][..]));
    assert_eq!(
        cur.read_huffman_vq(book),
        Err(header::HuffmanVqReadErr::EndOfPacket)
    );
    let mut cur = bitpacking::BitpackCursor::new(&[0u8]);
    assert_eq!(
        cur.read_huffman_vq(&s.codebooks[0]),
        Err(header::HuffmanVqReadErr::NoVqLookupForCodebook)
    );
}

#[test]
fn lookup_type_two_and_eight_stage_residues_parse() {
    let mut k = Knobs::ok(1);
    k.lookup_type = 2;
    k.value_bits_m1 = 3;
    k.residue_high_stage = true;
    let s = k.read().unwrap();
    let vq = s.codebooks[1].codebook_vq_lookup_vec.as_ref().unwrap();
    assert_eq!(vq, &[-1.0, 0.0, -1.0, 0.0, -1.0, 0.0, -1.0, 0.0]);
    let b = &s.residues[0].residue_books[0];
    assert_eq!(b.get_val(0), Some(1));
    assert_eq!(b.get_val(7), Some(1));
    assert_eq!(b.get_val(3), None);
}

#[test]
fn each_broken_setup_field_is_refused_by_name() {
    type Edit = fn(&mut Knobs);
    let bad: [(&str, Edit); 21] = [
        ("codebook sync", |k| k.sync = 0x564343),
        ("lookup type 3", |k| k.lookup_type = 3),
        ("time value", |k| k.time_value = 1),
        ("floor type 2", |k| k.floor_type = 2),
        ("floor1 x equal to 0", |k| k.floor1_x = 0),
        ("floor1 book past the count", |k| k.floor1_book = 3),
        ("residue type 3", |k| k.residue_type = 3),
        ("residue begin past end", |k| k.residue_begin = 65),
        ("residue classbook past the count", |k| {
            k.residue_classbook = 2
        }),
        ("residue book without lookup", |k| k.residue_book = 0),
        ("residue book past the count", |k| k.residue_book = 9),
        ("high residue stage without lookup", |k| {
            k.residue_high_stage = true;
            k.residue_book = 0
        }),
        ("mapping type 1", |k| k.mapping_type = 1),
        ("coupling onto itself", |k| k.couple = Some((1, 1))),
        ("coupling past the channels", |k| k.couple = Some((0, 3))),
        ("mapping reserved bits", |k| k.reserved = 2),
        ("submap floor past the count", |k| k.submap_floor = 1),
        ("submap residue past the count", |k| k.submap_residue = 1),
        ("mode window type", |k| k.windowtype = 1),
        ("mode mapping past the count", |k| k.mode_mapping = 1),
        ("framing bit", |k| k.framing = 0),
    ];
    for (what, edit) in bad {
        let mut k = Knobs::ok(3);
        k.couple = Some((0, 1));
        assert!(k.read().is_ok(), "{what}: the base header must parse");
        edit(&mut k);
        assert_eq!(
            k.read().err(),
            Some(HeaderReadError::HeaderBadFormat),
            "{what}"
        );
    }
}

#[test]
fn codebook_sizes_past_what_the_packet_can_hold_are_refused_before_allocating() {
    let mut k = Knobs::ok(1);
    k.vq_entries = (1 << 23) + 1;
    assert_eq!(k.read().err(), Some(HeaderReadError::HeaderBadFormat));

    let mut k = Knobs::ok(1);
    k.vq_entries = 1 << 19;
    k.vq_ordered_first_len = 19;
    k.lookup_type = 2;
    k.value_bits_m1 = 15;
    let mut packet = k.setup();
    packet.truncate(80);
    assert_eq!(
        read_header_setup(&packet, 1, (8, 11)).err(),
        Some(HeaderReadError::EndOfPacket)
    );

    let mut k = Knobs::ok(1);
    k.vq_entries = 3;
    assert_eq!(k.read().err(), Some(HeaderReadError::HeaderBadFormat));

    let mut k = Knobs::ok(1);
    k.vq_ordered_first_len = 31;
    assert_eq!(k.read().err(), Some(HeaderReadError::HeaderBadFormat));
}

#[test]
fn an_ordered_codebook_is_refused_the_moment_a_run_reaches_length_33() {
    let mut k = Knobs::ok(1);
    k.vq_split = Some(2);
    k.vq_ordered_first_len = 31;
    let past = k.setup();
    assert_eq!(
        read_header_setup(&past[..28], 1, (8, 11)).err(),
        Some(HeaderReadError::HeaderBadFormat)
    );
    assert_eq!(k.read().err(), Some(HeaderReadError::HeaderBadFormat));
    k.vq_ordered_first_len = 30;
    let fits = k.setup();
    assert_eq!(
        read_header_setup(&fits[..28], 1, (8, 11)).err(),
        Some(HeaderReadError::EndOfPacket)
    );
}

#[test]
fn a_mapping_mux_names_one_of_its_submaps() {
    let mut k = Knobs::ok(2);
    k.submaps = Some((2, 1));
    let s = k.read().unwrap();
    assert_eq!(s.mappings[0].mapping_mux, [1, 1]);
    assert_eq!(s.mappings[0].mapping_submap_floors, [0, 0]);
    assert_eq!(s.mappings[0].mapping_submap_residues, [0, 0]);
    k.submaps = Some((2, 2));
    assert_eq!(k.read().err(), Some(HeaderReadError::HeaderBadFormat));
    k.submaps = Some((16, 15));
    let sixteen = k.read().unwrap();
    assert_eq!(sixteen.mappings.len(), 1);
    assert_eq!(sixteen.mappings[0].mapping_mux, [15, 15]);
    assert_eq!(sixteen.mappings[0].mapping_submap_floors.len(), 16);
}

#[test]
fn sequence_p_accumulates_through_a_parsed_codebook() {
    let vq = |k: &Knobs| {
        k.read().unwrap().codebooks[1]
            .codebook_vq_lookup_vec
            .clone()
            .unwrap()
    };
    let mut k = Knobs::ok(1);
    k.sequence_p = true;
    assert_eq!(vq(&k), [-1.0, -2.0, 0.0, -1.0, -1.0, -1.0, 0.0, 0.0]);
    k.lookup_type = 2;
    assert_eq!(vq(&k), [-1.0; 8]);
    k.sequence_p = false;
    assert_eq!(vq(&k), [-1.0, 0.0, -1.0, 0.0, -1.0, 0.0, -1.0, 0.0]);
}

#[test]
fn setup_arguments_out_of_range_are_refused() {
    let packet = Knobs::ok(1).setup();
    assert_eq!(
        read_header_setup(&packet, 0, (8, 11)).err(),
        Some(HeaderReadError::HeaderBadFormat)
    );
    assert_eq!(
        read_header_setup(&packet, 1, (0, 11)).err(),
        Some(HeaderReadError::HeaderBadFormat)
    );
    assert_eq!(
        read_header_setup(&packet, 1, (8, 14)).err(),
        Some(HeaderReadError::HeaderBadFormat)
    );
    let mut wrong = packet.clone();
    wrong[0] = 3;
    assert_eq!(
        read_header_setup(&wrong, 1, (8, 11)).err(),
        Some(HeaderReadError::HeaderBadType(3))
    );
}

#[test]
fn a_cut_hand_built_setup_is_refused_at_every_length() {
    let packet = Knobs::ok(2).setup();
    for len in 0..packet.len() {
        assert!(
            read_header_setup(&packet[..len], 2, (8, 11)).is_err(),
            "cut at {len}"
        );
    }
}

#[test]
fn a_twelve_byte_codebook_asking_for_millions_of_entries_is_refused_before_allocating() {
    let mut k = Knobs::ok(1);
    k.vq_dim = 1;
    k.vq_entries = (1 << 23) - 1;
    k.vq_ordered_first_len = 22;
    k.lookup_type = 0;
    assert_eq!(k.read().err(), Some(HeaderReadError::BufferNotAddressable));

    let mut k = Knobs::ok(1);
    k.vq_dim = 15;
    k.vq_entries = (1 << 20) - 2;
    k.vq_ordered_first_len = 19;
    k.lookup_type = 1;
    assert_eq!(k.read().err(), Some(HeaderReadError::BufferNotAddressable));
}

#[test]
fn many_large_codebooks_together_exhaust_the_setup_budget() {
    let mut w = BitWriter::new();
    w.put(5, 8).bytes(b"vorbis").put(255, 8);
    for _ in 0..256 {
        w.put(0x564342, 24).put(1, 16).put(1 << 16, 24).put(1, 1);
        w.put(15, 5).put(1 << 16, 17);
        w.put(0, 4);
    }
    let packet = w.finish();
    assert!(packet.len() < 4096);
    assert_eq!(
        read_header_setup(&packet, 1, (8, 11)).err(),
        Some(HeaderReadError::BufferNotAddressable)
    );
}

#[test]
fn a_vq_codebook_with_no_dimensions_is_refused() {
    for lookup_type in [1, 2] {
        let mut k = Knobs::ok(1);
        k.vq_dim = 0;
        k.lookup_type = lookup_type;
        assert_eq!(
            k.read().err(),
            Some(HeaderReadError::HeaderBadFormat),
            "lookup type {lookup_type}"
        );
    }
}

#[test]
fn residue_classbooks_follow_libvorbis_partitioning_checks() {
    let mut k = Knobs::ok(1);
    k.residue_classifications = 2;
    let s = k.read().unwrap();
    assert_eq!(s.residues[0].residue_classifications, 2);
    assert_eq!(s.residues[0].residue_books.len(), 2);

    let mut k = Knobs::ok(1);
    k.residue_classifications = 3;
    assert_eq!(k.read().err(), Some(HeaderReadError::HeaderBadFormat));

    let mut k = Knobs::ok(1);
    k.residue_classbook = 1;
    k.residue_classifications = 2;
    assert!(k.read().is_ok());
    k.residue_classifications = 3;
    assert_eq!(k.read().err(), Some(HeaderReadError::HeaderBadFormat));

    let mut k = Knobs::ok(1);
    k.book0_dim = 0;
    assert_eq!(k.read().err(), Some(HeaderReadError::HeaderBadFormat));
}

#[test]
fn a_floor_zero_setup_parses_to_its_fields() {
    let mut k = Knobs::ok(2);
    k.floor_type = 0;
    let s = k.read().unwrap();
    let Floor::TypeZero(f) = &s.floors[0] else {
        panic!("floor 0 is not type 0");
    };
    assert_eq!(f.floor0_order, 8);
    assert_eq!(f.floor0_amplitude_bits, 6);
    assert_eq!(f.floor0_amplitude_offset, 100);
    assert_eq!(f.floor0_number_of_books, 1);
    assert_eq!(f.floor0_book_list, [1]);
    assert_eq!(f.cached_bark_cos_omega[0].len(), 128);
    assert_eq!(f.cached_bark_cos_omega[1].len(), 1024);
    for table in &f.cached_bark_cos_omega {
        assert_eq!(table[0], 1.0);
        assert!(
            table
                .iter()
                .all(|v| v.is_finite() && (-1.0..=1.0).contains(v))
        );
    }
    assert_eq!(s.residues.len(), 1);
}

#[test]
fn floor_zero_refuses_what_libvorbis_refuses() {
    type Edit = fn(&mut Knobs);
    let bad: [(&str, Edit); 6] = [
        ("order 0", |k| k.floor0_order = 0),
        ("rate 0", |k| k.floor0_rate = 0),
        ("bark map size 0", |k| k.floor0_bark = 0),
        ("book equal to the count", |k| k.floor0_book = 2),
        ("book past the count", |k| k.floor0_book = 200),
        ("book without a VQ lookup", |k| k.floor0_book = 0),
    ];
    for (what, edit) in bad {
        let mut k = Knobs::ok(1);
        k.floor_type = 0;
        assert!(k.read().is_ok(), "{what}: the base header must parse");
        edit(&mut k);
        assert_eq!(
            k.read().err(),
            Some(HeaderReadError::HeaderBadFormat),
            "{what}"
        );
    }
}

#[test]
fn a_floor_zero_setup_cut_anywhere_is_refused() {
    let mut k = Knobs::ok(2);
    k.floor_type = 0;
    let packet = k.setup();
    for len in 0..packet.len() {
        assert!(
            read_header_setup(&packet[..len], 2, (8, 11)).is_err(),
            "cut at {len}"
        );
    }
}

#[test]
fn floor_one_subclasses_read_their_masterbook_and_books() {
    let mut k = Knobs::ok(1);
    k.floor1_subclass = 2;
    k.floor1_masterbook = 1;
    k.floor1_dim_m1 = 2;
    let s = k.read().unwrap();
    let Floor::TypeOne(f) = &s.floors[0] else {
        panic!("floor 0 is not type 1");
    };
    assert_eq!(f.floor1_class_subclasses, [2]);
    assert_eq!(f.floor1_class_masterbooks, [1]);
    assert_eq!(f.floor1_class_dimensions, [3]);
    assert_eq!(f.floor1_subclass_books, [vec![0i16; 4]]);
    assert_eq!(f.floor1_x_list, [0, 16, 5, 6, 7]);

    let mut k = Knobs::ok(1);
    k.floor1_subclass = 1;
    k.floor1_book = 0;
    let s = k.read().unwrap();
    let Floor::TypeOne(f) = &s.floors[0] else {
        panic!("floor 0 is not type 1");
    };
    assert_eq!(f.floor1_subclass_books, [vec![-1i16, -1]]);
}

#[test]
fn floor_one_refusals_past_the_single_class_path() {
    type Edit = fn(&mut Knobs);
    let bad: [(&str, Edit); 4] = [
        ("masterbook equal to the count", |k| {
            k.floor1_subclass = 1;
            k.floor1_masterbook = 2
        }),
        ("masterbook past the count", |k| {
            k.floor1_subclass = 1;
            k.floor1_masterbook = 255
        }),
        ("subclass book past the count", |k| {
            k.floor1_subclass = 3;
            k.floor1_book = 3
        }),
        ("more than 65 values", |k| {
            k.floor1_rangebits = 8;
            k.floor1_partitions = 9;
            k.floor1_dim_m1 = 7
        }),
    ];
    for (what, edit) in bad {
        let mut k = Knobs::ok(1);
        edit(&mut k);
        assert_eq!(
            k.read().err(),
            Some(HeaderReadError::HeaderBadFormat),
            "{what}"
        );
    }
    let mut k = Knobs::ok(1);
    k.floor1_rangebits = 8;
    k.floor1_partitions = 9;
    k.floor1_dim_m1 = 6;
    let s = k.read().unwrap();
    let Floor::TypeOne(f) = &s.floors[0] else {
        panic!("floor 0 is not type 1");
    };
    assert_eq!(f.floor1_x_list.len(), 65);
}

#[test]
fn a_sparse_codebook_skips_its_unused_entries() {
    let mut k = Knobs::ok(1);
    k.book0_sparse = true;
    let s = k.read().unwrap();
    assert_eq!(s.codebooks[0].codebook_entries, 3);
    let mut w = BitWriter::new();
    w.put(0, 1).put(1, 1).put(1, 1).put(0, 1);
    let data = w.finish();
    let mut cur = bitpacking::BitpackCursor::new(&data);
    let tree = &s.codebooks[0].codebook_huffman_tree;
    assert_eq!(cur.read_huffman(tree), Ok(0));
    assert_eq!(cur.read_huffman(tree), Ok(2));
    assert_eq!(cur.read_huffman(tree), Ok(2));
    assert_eq!(cur.read_huffman(tree), Ok(0));
}

fn comment(vendor: &[u8], comments: &[&[u8]], framing: u8) -> Vec<u8> {
    let mut p = vec![3];
    p.extend_from_slice(b"vorbis");
    p.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    p.extend_from_slice(vendor);
    p.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in comments {
        p.extend_from_slice(&(c.len() as u32).to_le_bytes());
        p.extend_from_slice(c);
    }
    p.push(framing);
    p
}

#[test]
fn comment_header_keeps_good_pairs_and_skips_odd_ones() {
    let p = comment(
        b"Xiph.Org libVorbis I 20200704",
        &[
            b"TITLE=Tone",
            b"no equals sign",
            b"\xff\xfe=bad utf8",
            b"ARTIST=a=b",
            b"EMPTY=",
        ],
        1,
    );
    let c = read_header_comment(&p).unwrap();
    assert_eq!(c.vendor, "Xiph.Org libVorbis I 20200704");
    assert_eq!(
        c.comment_list,
        [
            ("TITLE".to_string(), "Tone".to_string()),
            ("ARTIST".to_string(), "a=b".to_string()),
            ("EMPTY".to_string(), String::new()),
        ]
    );
}

#[test]
fn comment_header_refusals() {
    assert_eq!(
        read_header_comment(&comment(b"v", &[], 0)).err(),
        Some(HeaderReadError::HeaderBadFormat)
    );
    assert_eq!(
        read_header_comment(&comment(b"\xc3\x28", &[], 1)).err(),
        Some(HeaderReadError::Utf8DecodeError)
    );
    let mut ident_type = comment(b"v", &[], 1);
    ident_type[0] = 1;
    assert_eq!(
        read_header_comment(&ident_type).err(),
        Some(HeaderReadError::HeaderBadType(1))
    );
    let mut huge_vendor = comment(b"v", &[], 1);
    huge_vendor[7..11].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        read_header_comment(&huge_vendor).err(),
        Some(HeaderReadError::EndOfPacket)
    );
    let mut huge_count = comment(b"v", &[], 1);
    huge_count[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        read_header_comment(&huge_count).err(),
        Some(HeaderReadError::EndOfPacket)
    );
    let full = comment(b"vendor", &[b"A=1", b"B=2"], 1);
    for len in 0..full.len() {
        assert!(read_header_comment(&full[..len]).is_err(), "cut at {len}");
    }
}

fn ogg_packets(data: &[u8], want: usize) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut cur = Vec::new();
    let mut pos = 0;
    while pos + 27 <= data.len() && packets.len() < want {
        assert_eq!(&data[pos..pos + 4], b"OggS", "page at {pos}");
        let nseg = data[pos + 26] as usize;
        let lacing = &data[pos + 27..pos + 27 + nseg];
        let mut body = pos + 27 + nseg;
        for &lace in lacing {
            cur.extend_from_slice(&data[body..body + lace as usize]);
            body += lace as usize;
            if lace < 255 {
                packets.push(std::mem::take(&mut cur));
            }
        }
        pos = body;
    }
    packets.truncate(want);
    packets
}

fn probed(r: &Refs, c: &refs::Case) -> Vec<(String, String)> {
    String::from_utf8(r.bytes(&c.probe()))
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn consistent(s: &SetupHeader, channels: u8, name: &str) {
    let below = |v: u8, len: usize| (v as usize) < len;
    for m in &s.modes {
        assert!(
            below(m.mode_mapping, s.mappings.len()),
            "{name}: mode mapping"
        );
    }
    for m in &s.mappings {
        let submaps = m.mapping_submap_floors.len();
        assert_eq!(submaps, m.mapping_submap_residues.len(), "{name}: submaps");
        assert!(
            m.mapping_mux.iter().all(|&x| below(x, submaps)),
            "{name}: mux"
        );
        assert!(
            m.mapping_submap_floors
                .iter()
                .all(|&f| below(f, s.floors.len())),
            "{name}: submap floor"
        );
        assert!(
            m.mapping_submap_residues
                .iter()
                .all(|&r| below(r, s.residues.len())),
            "{name}: submap residue"
        );
        assert!(
            m.mapping_magnitudes
                .iter()
                .chain(&m.mapping_angles)
                .all(|&c| c < channels),
            "{name}: coupling channel"
        );
    }
    for r in &s.residues {
        assert!(
            below(r.residue_classbook, s.codebooks.len()),
            "{name}: classbook"
        );
        for b in r
            .residue_books
            .iter()
            .flat_map(|b| (0..8).filter_map(|i| b.get_val(i)))
        {
            let book = s.codebooks.get(b as usize);
            assert!(
                book.is_some_and(|k| k.codebook_vq_lookup_vec.is_some()),
                "{name}: residue book {b} without values"
            );
        }
    }
}

type Encoded = (&'static refs::Case, Vec<(String, String)>, Vec<Vec<u8>>);

fn encoded() -> Vec<Encoded> {
    let r = Refs::open(refs::AREA);
    refs::CASES
        .iter()
        .map(|c| {
            let packets = ogg_packets(&r.bytes(&c.ogg()), 3);
            assert_eq!(packets.len(), 3, "{}: three header packets", c.name);
            (c, probed(&r, c), packets)
        })
        .collect()
}

#[test]
fn ffmpeg_vorbis_headers_parse_to_what_ffprobe_reports() {
    for (c, probed, packets) in encoded() {
        let channels = c.channels as u8;
        let (ident, comments, setup) = read_header_set(&packets[0], &packets[1], &packets[2])
            .unwrap_or_else(|e| {
                panic!("{}: {e}", c.name);
            });
        let get = |k: &str| {
            probed
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("{}: ffprobe gave no {k}", c.name))
        };
        assert_eq!(
            get("sample_rate"),
            ident.audio_sample_rate.to_string(),
            "{}",
            c.name
        );
        assert_eq!(
            get("channels"),
            ident.audio_channels.to_string(),
            "{}",
            c.name
        );
        assert_eq!(ident.audio_sample_rate, c.rate, "{}", c.name);
        assert_eq!(ident.audio_channels, channels, "{}", c.name);
        match get("bit_rate").parse::<i32>() {
            Ok(b) => assert_eq!(b, ident.bitrate_nominal, "{}", c.name),
            Err(_) => assert!(ident.bitrate_nominal <= 0, "{}", c.name),
        }
        assert!(ident.blocksize_0 <= ident.blocksize_1, "{}", c.name);

        assert!(!comments.vendor.is_empty(), "{}", c.name);
        for (k, v) in probed
            .iter()
            .filter_map(|(k, v)| Some((k.strip_prefix("TAG:")?, v)))
        {
            assert!(
                comments
                    .comment_list
                    .iter()
                    .any(|(ck, cv)| ck.eq_ignore_ascii_case(k) && cv == v),
                "{}: tag {k}={v} missing from {:?}",
                c.name,
                comments.comment_list
            );
        }

        assert!(!setup.codebooks.is_empty(), "{}", c.name);
        assert!(!setup.modes.is_empty(), "{}", c.name);
        for m in &setup.mappings {
            assert_eq!(m.mapping_mux.len(), usize::from(channels), "{}", c.name);
            assert!(m.mapping_magnitudes.iter().all(|&x| x < channels));
            assert!(m.mapping_angles.iter().all(|&x| x < channels));
        }
        if channels == 2 {
            assert!(
                setup
                    .mappings
                    .iter()
                    .any(|m| !m.mapping_magnitudes.is_empty()),
                "{}: coupled stereo expected",
                c.name
            );
        }
        for mode in &setup.modes {
            assert!((mode.mode_mapping as usize) < setup.mappings.len());
        }
        for b in &setup.codebooks {
            if let Some(v) = &b.codebook_vq_lookup_vec {
                assert_eq!(
                    v.len(),
                    b.codebook_entries as usize * b.codebook_dimensions as usize,
                    "{}",
                    c.name
                );
            }
        }
        if !c.native {
            assert!(
                setup.floors.iter().all(|f| matches!(f, Floor::TypeOne(_))),
                "{}: libvorbis writes floor 1 only",
                c.name
            );
            let flags: Vec<bool> = setup.modes.iter().map(|m| m.mode_blockflag).collect();
            assert_eq!(flags, [false, true], "{}: short and long modes", c.name);
        }
        if c.name == "stereo_44k_q3" {
            assert_eq!((ident.blocksize_0, ident.blocksize_1), (8, 11));
            assert!(setup.residues.iter().all(|r| r.residue_type == 2));
            assert_eq!(setup.mappings[0].mapping_magnitudes, [0]);
            assert_eq!(setup.mappings[0].mapping_angles, [1]);
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn cut_and_corrupted_ffmpeg_headers_are_refused_without_a_panic() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for (c, _, packets) in encoded() {
        let ident = read_header_ident(&packets[0]).unwrap();
        let bs = (ident.blocksize_0, ident.blocksize_1);
        for len in 0..packets[0].len() {
            assert!(
                read_header_ident(&packets[0][..len]).is_err(),
                "{}: ident cut {len}",
                c.name
            );
        }
        for len in 0..packets[1].len() {
            assert!(
                read_header_comment(&packets[1][..len]).is_err(),
                "{}: comment cut {len}",
                c.name
            );
        }
        let setup = &packets[2];
        let cuts = (0..64.min(setup.len())).chain((64..setup.len()).step_by(41));
        for len in cuts {
            let r = read_header_setup(&setup[..len], ident.audio_channels, bs);
            assert!(r.is_err(), "{}: setup cut {len}", c.name);
        }
        for _ in 0..150 {
            let mut p = setup.clone();
            for _ in 0..1 + rng.next() % 4 {
                let at = (rng.next() as usize) % p.len();
                p[at] ^= 1 << (rng.next() % 8);
            }
            if let Ok(s) = read_header_setup(&p, ident.audio_channels, bs) {
                consistent(&s, ident.audio_channels, c.name);
            }
        }
        for _ in 0..50 {
            let mut p = packets[1].clone();
            let at = 7 + (rng.next() as usize) % (p.len() - 7);
            p[at] = rng.next() as u8;
            let _ = read_header_comment(&p);
        }
    }
}
