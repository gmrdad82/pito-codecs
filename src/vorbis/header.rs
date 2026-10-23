use std::fmt;

use super::bitpacking::{BitpackCursor, EndOfPacket};
use super::header_cached::{CachedBlocksizeDerived, compute_bark_map_cos_omega};
use super::huffman_tree::{HuffmanError, VorbisHuffmanTree};
use super::ilog;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderReadError {
    EndOfPacket,
    NotVorbisHeader,
    UnsupportedVorbisVersion,
    HeaderBadFormat,
    HeaderBadType(u8),
    HeaderIsAudio,
    Utf8DecodeError,
    BufferNotAddressable,
}

impl From<EndOfPacket> for HeaderReadError {
    fn from(_: EndOfPacket) -> HeaderReadError {
        HeaderReadError::EndOfPacket
    }
}

impl From<HuffmanError> for HeaderReadError {
    fn from(_: HuffmanError) -> HeaderReadError {
        HeaderReadError::HeaderBadFormat
    }
}

impl std::error::Error for HeaderReadError {}

impl fmt::Display for HeaderReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeaderReadError::EndOfPacket => write!(f, "Vorbis header cut short"),
            HeaderReadError::NotVorbisHeader => write!(f, "not a Vorbis header"),
            HeaderReadError::UnsupportedVorbisVersion => write!(f, "Vorbis version other than 0"),
            HeaderReadError::HeaderBadFormat => write!(f, "malformed Vorbis header"),
            HeaderReadError::HeaderBadType(t) => write!(f, "unexpected Vorbis header type {t}"),
            HeaderReadError::HeaderIsAudio => {
                write!(f, "Vorbis audio packet where a header was expected")
            }
            HeaderReadError::Utf8DecodeError => write!(f, "Vorbis vendor string is not UTF-8"),
            HeaderReadError::BufferNotAddressable => {
                write!(f, "Vorbis header asks for a buffer too large")
            }
        }
    }
}

impl From<HeaderReadError> for crate::media::Error {
    fn from(e: HeaderReadError) -> crate::media::Error {
        match e {
            HeaderReadError::UnsupportedVorbisVersion => {
                crate::media::Error::Unsupported(e.to_string())
            }
            _ => crate::media::Error::Invalid(e.to_string()),
        }
    }
}

fn read_header_begin(rdr: &mut BitpackCursor) -> Result<u8, HeaderReadError> {
    let res = rdr.read_u8()?;
    if res & 1 == 0 {
        return Err(HeaderReadError::HeaderIsAudio);
    }
    for &want in b"vorbis" {
        if rdr.read_u8()? != want {
            return Err(HeaderReadError::NotVorbisHeader);
        }
    }
    Ok(res)
}

pub type HeaderSet = (IdentHeader, CommentHeader, SetupHeader);

#[derive(Clone, Debug)]
pub struct IdentHeader {
    pub audio_channels: u8,
    pub audio_sample_rate: u32,
    pub bitrate_maximum: i32,
    pub bitrate_nominal: i32,
    pub bitrate_minimum: i32,
    pub blocksize_0: u8,
    pub blocksize_1: u8,
    pub cached_bs_derived: [CachedBlocksizeDerived; 2],
}

pub fn read_header_ident(packet: &[u8]) -> Result<IdentHeader, HeaderReadError> {
    let mut rdr = BitpackCursor::new(packet);
    let hd_id = read_header_begin(&mut rdr)?;
    if hd_id != 1 {
        return Err(HeaderReadError::HeaderBadType(hd_id));
    }
    let vorbis_version = rdr.read_u32()?;
    if vorbis_version != 0 {
        return Err(HeaderReadError::UnsupportedVorbisVersion);
    }
    let audio_channels = rdr.read_u8()?;
    let audio_sample_rate = rdr.read_u32()?;
    let bitrate_maximum = rdr.read_i32()?;
    let bitrate_nominal = rdr.read_i32()?;
    let bitrate_minimum = rdr.read_i32()?;
    let blocksize_0 = rdr.read_u4()?;
    let blocksize_1 = rdr.read_u4()?;
    let framing = rdr.read_u8()?;
    if !(6..=13).contains(&blocksize_0)
        || !(6..=13).contains(&blocksize_1)
        || framing & 1 == 0
        || blocksize_0 > blocksize_1
        || audio_channels == 0
        || audio_sample_rate == 0
    {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    Ok(IdentHeader {
        audio_channels,
        audio_sample_rate,
        bitrate_maximum,
        bitrate_nominal,
        bitrate_minimum,
        blocksize_0,
        blocksize_1,
        cached_bs_derived: [
            CachedBlocksizeDerived::from_blocksize(blocksize_0),
            CachedBlocksizeDerived::from_blocksize(blocksize_1),
        ],
    })
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CommentHeader {
    pub vendor: String,
    pub comment_list: Vec<(String, String)>,
}

struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], HeaderReadError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(HeaderReadError::EndOfPacket)?;
        let s = self
            .data
            .get(self.pos..end)
            .ok_or(HeaderReadError::EndOfPacket)?;
        self.pos = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, HeaderReadError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

pub fn read_header_comment(packet: &[u8]) -> Result<CommentHeader, HeaderReadError> {
    let mut rdr = BitpackCursor::new(packet);
    let hd_id = read_header_begin(&mut rdr)?;
    if hd_id != 3 {
        return Err(HeaderReadError::HeaderBadType(hd_id));
    }
    let mut rdr = ByteReader {
        data: packet,
        pos: 7,
    };
    let vendor_length = rdr.u32()? as usize;
    let vendor = String::from_utf8(rdr.take(vendor_length)?.to_vec())
        .map_err(|_| HeaderReadError::Utf8DecodeError)?;

    let comment_count = rdr.u32()? as usize;
    let mut comment_list = Vec::with_capacity(comment_count.min(packet.len() / 4));
    for _ in 0..comment_count {
        let comment_length = rdr.u32()? as usize;
        let Ok(comment) = std::str::from_utf8(rdr.take(comment_length)?) else {
            continue;
        };
        let Some((key, val)) = comment.split_once('=') else {
            continue;
        };
        comment_list.push((key.to_string(), val.to_string()));
    }
    let framing = rdr.take(1)?[0];
    if framing & 1 == 0 {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    Ok(CommentHeader {
        vendor,
        comment_list,
    })
}

#[derive(Clone, Debug)]
pub struct Codebook {
    pub codebook_dimensions: u16,
    pub codebook_entries: u32,
    pub codebook_vq_lookup_vec: Option<Vec<f32>>,
    pub codebook_huffman_tree: VorbisHuffmanTree,
}

#[derive(Clone, Debug)]
pub struct Residue {
    pub residue_type: u8,
    pub residue_begin: u32,
    pub residue_end: u32,
    pub residue_partition_size: u32,
    pub residue_classifications: u8,
    pub residue_classbook: u8,
    pub residue_books: Vec<ResidueBook>,
}

#[derive(Clone, Debug)]
pub struct Mapping {
    pub mapping_magnitudes: Vec<u8>,
    pub mapping_angles: Vec<u8>,
    pub mapping_mux: Vec<u8>,
    pub mapping_submap_floors: Vec<u8>,
    pub mapping_submap_residues: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ModeInfo {
    pub mode_blockflag: bool,
    pub mode_mapping: u8,
}

#[derive(Clone, Debug)]
pub enum Floor {
    TypeZero(FloorTypeZero),
    TypeOne(FloorTypeOne),
}

#[derive(Clone, Debug)]
pub struct FloorTypeZero {
    pub floor0_order: u8,
    pub floor0_amplitude_bits: u8,
    pub floor0_amplitude_offset: u8,
    pub floor0_number_of_books: u8,
    pub floor0_book_list: Vec<u8>,
    pub cached_bark_cos_omega: [Vec<f32>; 2],
}

#[derive(Clone, Debug)]
pub struct FloorTypeOne {
    pub floor1_multiplier: u8,
    pub floor1_partition_class: Vec<u8>,
    pub floor1_class_dimensions: Vec<u8>,
    pub floor1_class_subclasses: Vec<u8>,
    pub floor1_subclass_books: Vec<Vec<i16>>,
    pub floor1_class_masterbooks: Vec<u8>,
    pub floor1_x_list: Vec<u32>,
    pub floor1_x_list_sorted: Vec<(usize, u32)>,
}

#[derive(Clone, Debug)]
pub struct ResidueBook {
    vals_used: u8,
    val_i: [u8; 8],
}

impl ResidueBook {
    pub fn get_val(&self, i: u8) -> Option<u8> {
        if i < 8 && self.vals_used & (1 << i) > 0 {
            Some(self.val_i[i as usize])
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn first_stage(book: u8) -> ResidueBook {
        let mut val_i = [0u8; 8];
        val_i[0] = book;
        ResidueBook {
            vals_used: 1,
            val_i,
        }
    }

    fn read_book(
        rdr: &mut BitpackCursor,
        vals_used: u8,
        codebooks: &[Codebook],
    ) -> Result<Self, HeaderReadError> {
        let mut val_i = [0u8; 8];
        for (i, slot) in val_i.iter_mut().enumerate() {
            if vals_used & (1 << i) == 0 {
                continue;
            }
            let val_entry = rdr.read_u8()?;
            match codebooks.get(val_entry as usize) {
                Some(v) if v.codebook_vq_lookup_vec.is_some() => {}
                _ => return Err(HeaderReadError::HeaderBadFormat),
            }
            *slot = val_entry;
        }
        Ok(ResidueBook { vals_used, val_i })
    }
}

#[derive(Clone, Debug)]
pub struct SetupHeader {
    pub codebooks: Vec<Codebook>,
    pub floors: Vec<Floor>,
    pub residues: Vec<Residue>,
    pub mappings: Vec<Mapping>,
    pub modes: Vec<ModeInfo>,
}

struct CodebookVqLookup {
    codebook_lookup_type: u8,
    codebook_minimum_value: f32,
    codebook_delta_value: f32,
    codebook_sequence_p: bool,
    codebook_multiplicands: Vec<u32>,
}

fn lookup_vec_val_decode(
    lup: &CodebookVqLookup,
    codebook_entries: u32,
    codebook_dimensions: u16,
) -> Vec<f32> {
    let mut value_vectors =
        Vec::with_capacity(codebook_entries as usize * codebook_dimensions as usize);
    let mults = &lup.codebook_multiplicands;
    if lup.codebook_lookup_type == 1 {
        let codebook_lookup_values = mults.len().max(1);
        for lookup_offset in 0..codebook_entries as usize {
            let mut last = 0.;
            let mut index_divisor = 1usize;
            for _ in 0..codebook_dimensions {
                let multiplicand_offset = (lookup_offset / index_divisor) % codebook_lookup_values;
                let m = mults.get(multiplicand_offset).copied().unwrap_or(0);
                let vec_elem =
                    m as f32 * lup.codebook_delta_value + lup.codebook_minimum_value + last;
                if lup.codebook_sequence_p {
                    last = vec_elem;
                }
                value_vectors.push(vec_elem);
                index_divisor = index_divisor.saturating_mul(codebook_lookup_values);
            }
        }
    } else {
        for chunk in mults.chunks(codebook_dimensions.max(1) as usize) {
            let mut last = 0.;
            for &m in chunk {
                let vec_elem =
                    m as f32 * lup.codebook_delta_value + lup.codebook_minimum_value + last;
                if lup.codebook_sequence_p {
                    last = vec_elem;
                }
                value_vectors.push(vec_elem);
            }
        }
    }
    value_vectors
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HuffmanVqReadErr {
    EndOfPacket,
    NoVqLookupForCodebook,
}

impl BitpackCursor<'_> {
    pub fn read_huffman_vq<'b>(&mut self, b: &'b Codebook) -> Result<&'b [f32], HuffmanVqReadErr> {
        let idx = self
            .read_huffman(&b.codebook_huffman_tree)
            .map_err(|_| HuffmanVqReadErr::EndOfPacket)? as usize;
        let lookup = b
            .codebook_vq_lookup_vec
            .as_deref()
            .ok_or(HuffmanVqReadErr::NoVqLookupForCodebook)?;
        let dim = b.codebook_dimensions as usize;
        lookup
            .get(idx * dim..(idx + 1) * dim)
            .ok_or(HuffmanVqReadErr::NoVqLookupForCodebook)
    }
}

fn lookup1_values(codebook_entries: u32, codebook_dimensions: u16) -> u32 {
    if codebook_dimensions == 0 {
        return if codebook_entries == 0 { 0 } else { u32::MAX };
    }
    if codebook_dimensions >= 32 {
        return if codebook_entries == 0 { 0 } else { 1 };
    }
    let fits = |base: u32| {
        base.checked_pow(codebook_dimensions as u32)
            .is_some_and(|p| p <= codebook_entries)
    };
    let mut base_bits: u32 = 0;
    for bit in (0..32).rev() {
        let cand = base_bits | (1 << bit);
        if fits(cand) {
            base_bits = cand;
        }
    }
    base_bits
}

const SETUP_ENTRY_BUDGET: u64 = 1 << 20;
const SETUP_VQ_FLOAT_BUDGET: u64 = 1 << 22;

struct SetupBudget {
    entries: u64,
    vq_floats: u64,
}

impl SetupBudget {
    fn new() -> SetupBudget {
        SetupBudget {
            entries: SETUP_ENTRY_BUDGET,
            vq_floats: SETUP_VQ_FLOAT_BUDGET,
        }
    }

    fn take(left: &mut u64, n: u64) -> Result<(), HeaderReadError> {
        *left = left
            .checked_sub(n)
            .ok_or(HeaderReadError::BufferNotAddressable)?;
        Ok(())
    }
}

fn read_codebook(
    rdr: &mut BitpackCursor,
    budget: &mut SetupBudget,
) -> Result<Codebook, HeaderReadError> {
    if rdr.read_u24()? != 0x564342 {
        return Err(HeaderReadError::HeaderBadFormat);
    }

    let codebook_dimensions = rdr.read_u16()?;
    let codebook_entries = rdr.read_u24()?;
    let ordered = rdr.read_bit_flag()?;
    if ilog(codebook_dimensions as u64) + ilog(codebook_entries as u64) > 24 {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    SetupBudget::take(&mut budget.entries, codebook_entries as u64)?;

    let mut codebook_codeword_lengths: Vec<u8>;
    if !ordered {
        let sparse = rdr.read_bit_flag()?;
        let min_bits = if sparse { 1 } else { 5 };
        if codebook_entries as usize * min_bits > rdr.bits_left() {
            return Err(HeaderReadError::EndOfPacket);
        }
        codebook_codeword_lengths = Vec::with_capacity(codebook_entries as usize);
        for _ in 0..codebook_entries {
            let length = if !sparse || rdr.read_bit_flag()? {
                rdr.read_u5()? + 1
            } else {
                0
            };
            codebook_codeword_lengths.push(length);
        }
    } else {
        codebook_codeword_lengths = Vec::with_capacity(codebook_entries as usize);
        let mut current_entry: u32 = 0;
        let mut current_length = rdr.read_u5()? + 1;
        while current_entry < codebook_entries {
            let number = rdr.read_dyn_u32(ilog((codebook_entries - current_entry) as u64))?;
            if number > codebook_entries - current_entry || current_length > 32 {
                return Err(HeaderReadError::HeaderBadFormat);
            }
            codebook_codeword_lengths.extend(std::iter::repeat_n(current_length, number as usize));
            current_entry += number;
            current_length += 1;
        }
    }

    let codebook_lookup_type = rdr.read_u4()?;
    if codebook_lookup_type > 2 {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    let codebook_lookup = if codebook_lookup_type == 0 {
        None
    } else {
        if codebook_dimensions == 0 {
            return Err(HeaderReadError::HeaderBadFormat);
        }
        SetupBudget::take(
            &mut budget.vq_floats,
            codebook_entries as u64 * codebook_dimensions as u64,
        )?;
        let codebook_minimum_value = rdr.read_f32()?;
        let codebook_delta_value = rdr.read_f32()?;
        let codebook_value_bits = rdr.read_u4()? + 1;
        let codebook_sequence_p = rdr.read_bit_flag()?;
        let codebook_lookup_values: u64 = if codebook_lookup_type == 1 {
            lookup1_values(codebook_entries, codebook_dimensions) as u64
        } else {
            codebook_entries as u64 * codebook_dimensions as u64
        };
        if codebook_lookup_values.saturating_mul(codebook_value_bits as u64)
            > rdr.bits_left() as u64
        {
            return Err(HeaderReadError::EndOfPacket);
        }
        let mut codebook_multiplicands = Vec::with_capacity(codebook_lookup_values as usize);
        for _ in 0..codebook_lookup_values {
            codebook_multiplicands.push(rdr.read_dyn_u32(codebook_value_bits)?);
        }
        Some(CodebookVqLookup {
            codebook_lookup_type,
            codebook_minimum_value,
            codebook_delta_value,
            codebook_sequence_p,
            codebook_multiplicands,
        })
    };
    let codebook_vq_lookup_vec = codebook_lookup
        .as_ref()
        .map(|lup| lookup_vec_val_decode(lup, codebook_entries, codebook_dimensions));

    Ok(Codebook {
        codebook_dimensions,
        codebook_entries,
        codebook_vq_lookup_vec,
        codebook_huffman_tree: VorbisHuffmanTree::load_from_array(&codebook_codeword_lengths)?,
    })
}

fn read_floor(
    rdr: &mut BitpackCursor,
    codebooks: &[Codebook],
    blocksizes: (u8, u8),
) -> Result<Floor, HeaderReadError> {
    let codebook_cnt = codebooks.len() as u16;
    match rdr.read_u16()? {
        0 => {
            let floor0_order = rdr.read_u8()?;
            let floor0_rate = rdr.read_u16()?;
            let floor0_bark_map_size = rdr.read_u16()?;
            if floor0_order == 0 || floor0_rate == 0 || floor0_bark_map_size == 0 {
                return Err(HeaderReadError::HeaderBadFormat);
            }
            let floor0_amplitude_bits = rdr.read_u6()?;
            let floor0_amplitude_offset = rdr.read_u8()?;
            let floor0_number_of_books = rdr.read_u4()? + 1;
            let mut floor0_book_list = Vec::with_capacity(floor0_number_of_books as usize);
            for _ in 0..floor0_number_of_books {
                let value = rdr.read_u8()?;
                let has_vq = codebooks
                    .get(value as usize)
                    .is_some_and(|b| b.codebook_vq_lookup_vec.is_some());
                if !has_vq {
                    return Err(HeaderReadError::HeaderBadFormat);
                }
                floor0_book_list.push(value);
            }
            Ok(Floor::TypeZero(FloorTypeZero {
                floor0_order,
                floor0_amplitude_bits,
                floor0_amplitude_offset,
                floor0_number_of_books,
                floor0_book_list,
                cached_bark_cos_omega: [
                    compute_bark_map_cos_omega(
                        1 << (blocksizes.0 - 1),
                        floor0_rate,
                        floor0_bark_map_size,
                    ),
                    compute_bark_map_cos_omega(
                        1 << (blocksizes.1 - 1),
                        floor0_rate,
                        floor0_bark_map_size,
                    ),
                ],
            }))
        }
        1 => {
            let floor1_partitions = rdr.read_u5()?;
            let mut floor1_partition_class = Vec::with_capacity(floor1_partitions as usize);
            for _ in 0..floor1_partitions {
                floor1_partition_class.push(rdr.read_u4()?);
            }
            let classes = floor1_partition_class
                .iter()
                .map(|&c| c as usize + 1)
                .max()
                .unwrap_or(0);

            let mut floor1_class_dimensions = Vec::with_capacity(classes);
            let mut floor1_class_subclasses = Vec::with_capacity(classes);
            let mut floor1_subclass_books = Vec::with_capacity(classes);
            let mut floor1_class_masterbooks = Vec::with_capacity(classes);
            for _ in 0..classes {
                floor1_class_dimensions.push(rdr.read_u3()? + 1);
                let cur_subclass = rdr.read_u2()?;
                floor1_class_subclasses.push(cur_subclass);
                if cur_subclass != 0 {
                    let cur_masterbook = rdr.read_u8()?;
                    if cur_masterbook as u16 >= codebook_cnt {
                        return Err(HeaderReadError::HeaderBadFormat);
                    }
                    floor1_class_masterbooks.push(cur_masterbook);
                } else {
                    floor1_class_masterbooks.push(0);
                }
                let cur_books_cnt = 1usize << cur_subclass;
                let mut cur_books = Vec::with_capacity(cur_books_cnt);
                for _ in 0..cur_books_cnt {
                    let cur_book = rdr.read_u8()? as i16 - 1;
                    if cur_book >= codebook_cnt as i16 {
                        return Err(HeaderReadError::HeaderBadFormat);
                    }
                    cur_books.push(cur_book);
                }
                floor1_subclass_books.push(cur_books);
            }
            let floor1_multiplier = rdr.read_u2()? + 1;
            let rangebits = rdr.read_u4()?;
            let floor1_values: usize = 2 + floor1_partition_class
                .iter()
                .map(|&c| floor1_class_dimensions[c as usize] as usize)
                .sum::<usize>();
            if floor1_values > 65 {
                return Err(HeaderReadError::HeaderBadFormat);
            }
            let mut floor1_x_list = Vec::with_capacity(floor1_values);
            floor1_x_list.push(0);
            floor1_x_list.push(1u32 << rangebits);
            for &c in &floor1_partition_class {
                for _ in 0..floor1_class_dimensions[c as usize] {
                    floor1_x_list.push(rdr.read_dyn_u32(rangebits)?);
                }
            }
            let mut floor1_x_list_sorted: Vec<(usize, u32)> =
                floor1_x_list.iter().copied().enumerate().collect();
            floor1_x_list_sorted.sort_by_key(|a| a.1);
            if floor1_x_list_sorted.windows(2).any(|w| w[0].1 == w[1].1) {
                return Err(HeaderReadError::HeaderBadFormat);
            }
            Ok(Floor::TypeOne(FloorTypeOne {
                floor1_multiplier,
                floor1_partition_class,
                floor1_class_dimensions,
                floor1_class_subclasses,
                floor1_subclass_books,
                floor1_class_masterbooks,
                floor1_x_list,
                floor1_x_list_sorted,
            }))
        }
        _ => Err(HeaderReadError::HeaderBadFormat),
    }
}

fn read_residue(
    rdr: &mut BitpackCursor,
    codebooks: &[Codebook],
) -> Result<Residue, HeaderReadError> {
    let residue_type = rdr.read_u16()?;
    if residue_type > 2 {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    let residue_begin = rdr.read_u24()?;
    let residue_end = rdr.read_u24()?;
    if residue_begin > residue_end {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    let residue_partition_size = rdr.read_u24()? + 1;
    let residue_classifications = rdr.read_u6()? + 1;
    let residue_classbook = rdr.read_u8()?;
    let mut residue_cascade = Vec::with_capacity(residue_classifications as usize);
    for _ in 0..residue_classifications {
        let low_bits = rdr.read_u3()?;
        let high_bits = if rdr.read_bit_flag()? {
            rdr.read_u5()?
        } else {
            0
        };
        residue_cascade.push((high_bits << 3) | low_bits);
    }
    let mut residue_books = Vec::with_capacity(residue_classifications as usize);
    for &cascade_entry in &residue_cascade {
        residue_books.push(ResidueBook::read_book(rdr, cascade_entry, codebooks)?);
    }
    let classbook = codebooks
        .get(residue_classbook as usize)
        .ok_or(HeaderReadError::HeaderBadFormat)?;
    if classbook.codebook_dimensions == 0 {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    let mut partvals: u64 = 1;
    for _ in 0..classbook.codebook_dimensions {
        partvals *= residue_classifications as u64;
        if partvals > classbook.codebook_entries as u64 {
            return Err(HeaderReadError::HeaderBadFormat);
        }
    }
    Ok(Residue {
        residue_type: residue_type as u8,
        residue_begin,
        residue_end,
        residue_partition_size,
        residue_classifications,
        residue_classbook,
        residue_books,
    })
}

fn read_mapping(
    rdr: &mut BitpackCursor,
    audio_chan_ilog: u8,
    audio_channels: u8,
    floor_count: u8,
    residue_count: u8,
) -> Result<Mapping, HeaderReadError> {
    if rdr.read_u16()? != 0 {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    let mapping_submaps = if rdr.read_bit_flag()? {
        rdr.read_u4()? + 1
    } else {
        1
    };
    let mapping_coupling_steps = if rdr.read_bit_flag()? {
        rdr.read_u8()? as u16 + 1
    } else {
        0
    };
    let mut mapping_magnitudes = Vec::with_capacity(mapping_coupling_steps as usize);
    let mut mapping_angles = Vec::with_capacity(mapping_coupling_steps as usize);
    for _ in 0..mapping_coupling_steps {
        let cur_mag = rdr.read_dyn_u8(audio_chan_ilog)?;
        let cur_angle = rdr.read_dyn_u8(audio_chan_ilog)?;
        if cur_angle == cur_mag || cur_mag >= audio_channels || cur_angle >= audio_channels {
            return Err(HeaderReadError::HeaderBadFormat);
        }
        mapping_magnitudes.push(cur_mag);
        mapping_angles.push(cur_angle);
    }
    if rdr.read_u2()? != 0 {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    let mapping_mux = if mapping_submaps > 1 {
        let mut m = Vec::with_capacity(audio_channels as usize);
        for _ in 0..audio_channels {
            let val = rdr.read_u4()?;
            if val >= mapping_submaps {
                return Err(HeaderReadError::HeaderBadFormat);
            }
            m.push(val);
        }
        m
    } else {
        vec![0; audio_channels as usize]
    };
    let mut mapping_submap_floors = Vec::with_capacity(mapping_submaps as usize);
    let mut mapping_submap_residues = Vec::with_capacity(mapping_submaps as usize);
    for _ in 0..mapping_submaps {
        rdr.read_u8()?;
        let cur_floor = rdr.read_u8()?;
        let cur_residue = rdr.read_u8()?;
        if cur_floor >= floor_count || cur_residue >= residue_count {
            return Err(HeaderReadError::HeaderBadFormat);
        }
        mapping_submap_floors.push(cur_floor);
        mapping_submap_residues.push(cur_residue);
    }
    Ok(Mapping {
        mapping_magnitudes,
        mapping_angles,
        mapping_mux,
        mapping_submap_floors,
        mapping_submap_residues,
    })
}

fn read_mode_info(rdr: &mut BitpackCursor, mapping_count: u8) -> Result<ModeInfo, HeaderReadError> {
    let mode_blockflag = rdr.read_bit_flag()?;
    let mode_windowtype = rdr.read_u16()?;
    let mode_transformtype = rdr.read_u16()?;
    let mode_mapping = rdr.read_u8()?;
    if mode_windowtype != 0 || mode_transformtype != 0 || mode_mapping >= mapping_count {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    Ok(ModeInfo {
        mode_blockflag,
        mode_mapping,
    })
}

pub fn read_header_setup(
    packet: &[u8],
    audio_channels: u8,
    blocksizes: (u8, u8),
) -> Result<SetupHeader, HeaderReadError> {
    if audio_channels == 0 || !(6..=13).contains(&blocksizes.0) || !(6..=13).contains(&blocksizes.1)
    {
        return Err(HeaderReadError::HeaderBadFormat);
    }
    let mut rdr = BitpackCursor::new(packet);
    let hd_id = read_header_begin(&mut rdr)?;
    if hd_id != 5 {
        return Err(HeaderReadError::HeaderBadType(hd_id));
    }

    let audio_chan_ilog = ilog((audio_channels - 1) as u64);

    let vorbis_codebook_count = rdr.read_u8()? as u16 + 1;
    let mut budget = SetupBudget::new();
    let mut codebooks = Vec::with_capacity(vorbis_codebook_count as usize);
    for _ in 0..vorbis_codebook_count {
        codebooks.push(read_codebook(&mut rdr, &mut budget)?);
    }

    let vorbis_time_count = rdr.read_u6()? + 1;
    for _ in 0..vorbis_time_count {
        if rdr.read_u16()? != 0 {
            return Err(HeaderReadError::HeaderBadFormat);
        }
    }

    let vorbis_floor_count = rdr.read_u6()? + 1;
    let mut floors = Vec::with_capacity(vorbis_floor_count as usize);
    for _ in 0..vorbis_floor_count {
        floors.push(read_floor(&mut rdr, &codebooks, blocksizes)?);
    }

    let vorbis_residue_count = rdr.read_u6()? + 1;
    let mut residues = Vec::with_capacity(vorbis_residue_count as usize);
    for _ in 0..vorbis_residue_count {
        residues.push(read_residue(&mut rdr, &codebooks)?);
    }

    let vorbis_mapping_count = rdr.read_u6()? + 1;
    let mut mappings = Vec::with_capacity(vorbis_mapping_count as usize);
    for _ in 0..vorbis_mapping_count {
        mappings.push(read_mapping(
            &mut rdr,
            audio_chan_ilog,
            audio_channels,
            vorbis_floor_count,
            vorbis_residue_count,
        )?);
    }

    let vorbis_mode_count = rdr.read_u6()? + 1;
    let mut modes = Vec::with_capacity(vorbis_mode_count as usize);
    for _ in 0..vorbis_mode_count {
        modes.push(read_mode_info(&mut rdr, vorbis_mapping_count)?);
    }

    if !rdr.read_bit_flag()? {
        return Err(HeaderReadError::HeaderBadFormat);
    }

    Ok(SetupHeader {
        codebooks,
        floors,
        residues,
        mappings,
        modes,
    })
}

pub fn read_header_set(
    ident: &[u8],
    comment: &[u8],
    setup: &[u8],
) -> Result<HeaderSet, HeaderReadError> {
    let i = read_header_ident(ident)?;
    let c = read_header_comment(comment)?;
    let s = read_header_setup(setup, i.audio_channels, (i.blocksize_0, i.blocksize_1))?;
    Ok((i, c, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENT: [u8; 30] = [
        0x01, 0x76, 0x6f, 0x72, 0x62, 0x69, 0x73, 0x00, 0x00, 0x00, 0x00, 0x02, 0x44, 0xac, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xb5, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0xb8, 0x01,
    ];

    #[test]
    fn lookup1_values_is_the_integer_root() {
        assert_eq!(lookup1_values(1025, 10), 2);
        assert_eq!(lookup1_values(1024, 10), 2);
        assert_eq!(lookup1_values(1023, 10), 1);
        assert_eq!(lookup1_values(3126, 5), 5);
        assert_eq!(lookup1_values(3125, 5), 5);
        assert_eq!(lookup1_values(3124, 5), 4);
        assert_eq!(lookup1_values(1, 1), 1);
        assert_eq!(lookup1_values(0, 15), 0);
        assert_eq!(lookup1_values(0, 0), 0);
        assert_eq!(lookup1_values(1, 0), u32::MAX);
        assert_eq!(lookup1_values(400, 0), u32::MAX);
        assert_eq!(lookup1_values(6561, 8), 3);
        assert_eq!(lookup1_values(6560, 8), 2);
        assert_eq!(lookup1_values(1 << 23, 1), 1 << 23);
        assert_eq!(lookup1_values(5, 40), 1);
        assert_eq!(lookup1_values(0, 40), 0);
    }

    #[test]
    fn ident_header_reads_the_lewton_vector() {
        let hdr = read_header_ident(&IDENT).unwrap();
        assert_eq!(hdr.audio_channels, 2);
        assert_eq!(hdr.audio_sample_rate, 44100);
        assert_eq!(hdr.bitrate_maximum, 0);
        assert_eq!(hdr.bitrate_nominal, 112000);
        assert_eq!(hdr.bitrate_minimum, 0);
        assert_eq!(hdr.blocksize_0, 8);
        assert_eq!(hdr.blocksize_1, 11);
        assert_eq!(hdr.cached_bs_derived[0].window_slope.len(), 128);
        assert_eq!(hdr.cached_bs_derived[1].window_slope.len(), 1024);
    }

    #[test]
    fn ident_header_refuses_each_bad_field() {
        let mut bad_magic = IDENT;
        bad_magic[6] = 0x72;
        assert_eq!(
            read_header_ident(&bad_magic).err(),
            Some(HeaderReadError::NotVorbisHeader)
        );
        let mut audio = IDENT;
        audio[0] = 0x00;
        assert_eq!(
            read_header_ident(&audio).err(),
            Some(HeaderReadError::HeaderIsAudio)
        );
        let mut comment_type = IDENT;
        comment_type[0] = 0x03;
        assert_eq!(
            read_header_ident(&comment_type).err(),
            Some(HeaderReadError::HeaderBadType(3))
        );
        let mut version = IDENT;
        version[7] = 1;
        assert_eq!(
            read_header_ident(&version).err(),
            Some(HeaderReadError::UnsupportedVorbisVersion)
        );
        let edits: [(usize, u8); 7] = [
            (11, 0),
            (12, 0),
            (28, 0x85),
            (28, 0xe8),
            (28, 0x8b),
            (29, 0x00),
            (29, 0x02),
        ];
        for (at, v) in edits {
            let mut h = IDENT;
            h[at] = v;
            if at == 12 {
                h[13] = 0;
            }
            assert_eq!(
                read_header_ident(&h).err(),
                Some(HeaderReadError::HeaderBadFormat),
                "byte {at} = {v:#x}"
            );
        }
        let mut high_bits = IDENT;
        high_bits[29] = 0x03;
        assert!(read_header_ident(&high_bits).is_ok());
        for len in 0..IDENT.len() {
            assert_eq!(
                read_header_ident(&IDENT[..len]).err(),
                Some(HeaderReadError::EndOfPacket),
                "cut at {len}"
            );
        }
    }

    #[test]
    fn errors_read_as_one_line_and_map_to_media_errors() {
        let all = [
            HeaderReadError::EndOfPacket,
            HeaderReadError::NotVorbisHeader,
            HeaderReadError::UnsupportedVorbisVersion,
            HeaderReadError::HeaderBadFormat,
            HeaderReadError::HeaderBadType(9),
            HeaderReadError::HeaderIsAudio,
            HeaderReadError::Utf8DecodeError,
            HeaderReadError::BufferNotAddressable,
        ];
        for e in all {
            let text = e.to_string();
            assert!(!text.is_empty() && !text.contains('\n'), "{e:?}");
            assert!(text.contains("Vorbis"), "{text}");
            let m: crate::media::Error = e.into();
            match (e, m) {
                (
                    HeaderReadError::UnsupportedVorbisVersion,
                    crate::media::Error::Unsupported(_),
                ) => {}
                (HeaderReadError::UnsupportedVorbisVersion, other) => panic!("{other:?}"),
                (_, crate::media::Error::Invalid(s)) => assert_eq!(s, text),
                (_, other) => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn residue_book_values_outside_eight_stages_are_absent() {
        let b = ResidueBook {
            vals_used: 0b1000_0001,
            val_i: [3, 0, 0, 0, 0, 0, 0, 7],
        };
        assert_eq!(b.get_val(0), Some(3));
        assert_eq!(b.get_val(1), None);
        assert_eq!(b.get_val(7), Some(7));
        assert_eq!(b.get_val(8), None);
        assert_eq!(b.get_val(255), None);
    }

    fn lookup(kind: u8, sequence_p: bool, mults: Vec<u32>) -> CodebookVqLookup {
        CodebookVqLookup {
            codebook_lookup_type: kind,
            codebook_minimum_value: 0.5,
            codebook_delta_value: 2.0,
            codebook_sequence_p: sequence_p,
            codebook_multiplicands: mults,
        }
    }

    #[test]
    fn sequence_p_adds_each_value_to_the_next_as_the_spec_says() {
        let one = |p| lookup_vec_val_decode(&lookup(1, p, vec![0, 1, 2]), 4, 2);
        assert_eq!(one(false), [0.5, 0.5, 2.5, 0.5, 4.5, 0.5, 0.5, 2.5]);
        assert_eq!(one(true), [0.5, 1.0, 2.5, 3.0, 4.5, 5.0, 0.5, 3.0]);
        let two = |p| lookup_vec_val_decode(&lookup(2, p, vec![0, 1, 2, 3]), 2, 2);
        assert_eq!(two(false), [0.5, 2.5, 4.5, 6.5]);
        assert_eq!(two(true), [0.5, 3.0, 4.5, 11.0]);
    }
}
