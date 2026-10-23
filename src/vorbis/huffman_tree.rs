struct HuffTree {
    even_childs: bool,
    payload: Option<u32>,
    l: Option<Box<HuffTree>>,
    r: Option<Box<HuffTree>>,
}

impl HuffTree {
    fn empty() -> HuffTree {
        HuffTree {
            even_childs: true,
            payload: None,
            l: None,
            r: None,
        }
    }

    fn insert_rec(&mut self, payload: u32, depth: u8) -> bool {
        if self.payload.is_some() {
            return false;
        }
        if depth == 0 {
            if !(self.l.is_none() && self.r.is_none()) {
                return false;
            }
            self.payload = Some(payload);
            return true;
        }
        if self.l.is_none() {
            let mut new_node = HuffTree::empty();
            new_node.insert_rec(payload, depth - 1);
            self.l = Some(Box::new(new_node));
            self.even_childs = false;
            return true;
        }
        let (Some(left), false) = (self.l.as_mut(), self.even_childs) else {
            return false;
        };
        if !left.even_childs && left.insert_rec(payload, depth - 1) {
            self.even_childs =
                left.even_childs && self.r.as_ref().map(|r| r.even_childs).unwrap_or(false);
            return true;
        }
        match self.r {
            Some(ref mut right) => {
                let success = right.insert_rec(payload, depth - 1);
                self.even_childs = left.even_childs && right.even_childs;
                success
            }
            None => {
                let mut new_node = HuffTree::empty();
                let success = new_node.insert_rec(payload, depth - 1);
                self.even_childs = left.even_childs && new_node.even_childs;
                self.r = Some(Box::new(new_node));
                success
            }
        }
    }

    fn children(&self) -> Option<(&HuffTree, &HuffTree)> {
        match (&self.l, &self.r) {
            (Some(l), Some(r)) => Some((l, r)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HuffmanError {
    Overspecified,
    Underpopulated,
    InvalidSingleEntry,
}

#[derive(Clone, Copy, Debug)]
enum UnrolledLookupEntry {
    HasEntry(u8, u32),
    InconclusiveWithHint(u32),
    Inconclusive,
}

pub enum PeekedDataLookupResult<'l> {
    Iter(u8, VorbisHuffmanIter<'l>),
    PayloadFound(u8, u32),
}

#[derive(Clone, Debug)]
pub struct VorbisHuffmanTree {
    desc_prog: Vec<u32>,
    unrolled_entries: [UnrolledLookupEntry; 256],
}

fn traverse(tree: &HuffTree, desc_prog: &mut Vec<u32>) -> u32 {
    let cur_pos = desc_prog.len() as u32;
    let children = tree.children();
    desc_prog.push(((children.is_some() as u32) << 31) | tree.payload.unwrap_or(0));
    if let Some((l, r)) = children {
        desc_prog.push(0);
        desc_prog.push(0);
        desc_prog[cur_pos as usize + 1] = traverse(l, desc_prog);
        desc_prog[cur_pos as usize + 2] = traverse(r, desc_prog);
    }
    cur_pos
}

fn uroll_traverse(
    tree: &HuffTree,
    unrolled_entries: &mut [UnrolledLookupEntry; 256],
    prefix: u32,
    prefix_idx: u8,
    desc_prog: &[u32],
    desc_prog_idx: u32,
) {
    if let Some((l, r)) = tree.children() {
        if prefix_idx == 8 {
            unrolled_entries[prefix as usize] =
                UnrolledLookupEntry::InconclusiveWithHint(desc_prog_idx);
        } else {
            let at = desc_prog_idx as usize;
            uroll_traverse(
                l,
                unrolled_entries,
                prefix,
                prefix_idx + 1,
                desc_prog,
                desc_prog[at + 1],
            );
            uroll_traverse(
                r,
                unrolled_entries,
                prefix + (1 << prefix_idx),
                prefix_idx + 1,
                desc_prog,
                desc_prog[at + 2],
            );
        }
    } else {
        let payload = tree.payload.unwrap_or(0);
        let step = 1usize << prefix_idx;
        for i in (prefix as usize..256).step_by(step) {
            unrolled_entries[i] = UnrolledLookupEntry::HasEntry(prefix_idx, payload);
        }
    }
}

impl VorbisHuffmanTree {
    pub fn load_from_array(
        codebook_codeword_lengths: &[u8],
    ) -> Result<VorbisHuffmanTree, HuffmanError> {
        let mut simple_tree = HuffTree::empty();
        let mut cnt: usize = 0;
        let mut last_valid_idx = 0;
        for (i, &codeword_length) in codebook_codeword_lengths.iter().enumerate() {
            if codeword_length == 0 {
                continue;
            }
            cnt += 1;
            last_valid_idx = i;
            if codeword_length > 32 || !simple_tree.insert_rec(i as u32, codeword_length) {
                return Err(HuffmanError::Overspecified);
            }
        }

        if cnt == 1 {
            if codebook_codeword_lengths[last_valid_idx] != 1 {
                return Err(HuffmanError::InvalidSingleEntry);
            }
            let decoded = last_valid_idx as u32;
            return Ok(VorbisHuffmanTree {
                desc_prog: vec![1u32 << 31, 3, 3, decoded],
                unrolled_entries: [UnrolledLookupEntry::HasEntry(1, decoded); 256],
            });
        }

        if !simple_tree.even_childs {
            return Err(HuffmanError::Underpopulated);
        }

        let mut desc_prog = Vec::with_capacity(cnt * 2);
        traverse(&simple_tree, &mut desc_prog);

        let mut unrolled_entries = [UnrolledLookupEntry::Inconclusive; 256];
        if cnt > 0 {
            uroll_traverse(&simple_tree, &mut unrolled_entries, 0, 0, &desc_prog, 0);
        }

        Ok(VorbisHuffmanTree {
            desc_prog,
            unrolled_entries,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.desc_prog.len() < 3
    }

    pub fn iter(&self) -> VorbisHuffmanIter<'_> {
        VorbisHuffmanIter {
            desc_prog: &self.desc_prog,
            pos: 0,
        }
    }

    pub fn lookup_peeked_data(
        &self,
        bit_count: u8,
        peeked_data: u32,
    ) -> PeekedDataLookupResult<'_> {
        use self::PeekedDataLookupResult::*;
        use self::UnrolledLookupEntry::*;
        let entry = self
            .unrolled_entries
            .get(peeked_data as usize)
            .copied()
            .unwrap_or(Inconclusive);
        match entry {
            HasEntry(cnt_to_remove, payload) if cnt_to_remove <= bit_count => {
                PayloadFound(cnt_to_remove, payload)
            }
            InconclusiveWithHint(hint) if bit_count >= 8 => Iter(
                8,
                VorbisHuffmanIter {
                    desc_prog: &self.desc_prog,
                    pos: hint,
                },
            ),
            _ => Iter(0, self.iter()),
        }
    }
}

pub struct VorbisHuffmanIter<'a> {
    desc_prog: &'a [u32],
    pos: u32,
}

impl VorbisHuffmanIter<'_> {
    pub fn next(&mut self, bit: bool) -> Option<u32> {
        let Some(&next) = self.desc_prog.get(self.pos as usize + 1 + bit as usize) else {
            self.pos = 0;
            return None;
        };
        self.pos = next;
        let child = self.desc_prog.get(next as usize).copied().unwrap_or(0);
        if (child & (1u32 << 31)) != 0 {
            None
        } else {
            self.pos = 0;
            Some(child)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vorbis::bitpacking::BitpackCursor;

    fn iter_test(tree: &VorbisHuffmanTree, path: u32, path_len: u8, expected_val: u32) {
        let mut itr = tree.iter();
        for i in 1..path_len {
            assert_eq!(itr.next((path & (1 << (path_len - i))) != 0), None);
        }
        assert_eq!(itr.next((path & 1) != 0), Some(expected_val));
    }

    fn pack(codes: &[(u32, u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut acc = 0u64;
        let mut n = 0u32;
        for &(code, len) in codes {
            for i in (0..len).rev() {
                acc |= (((code >> i) & 1) as u64) << n;
                n += 1;
                if n == 8 {
                    out.push(acc as u8);
                    acc = 0;
                    n = 0;
                }
            }
        }
        if n > 0 {
            out.push(acc as u8);
        }
        out
    }

    #[test]
    fn spec_example_walks_bit_by_bit() {
        let tree = VorbisHuffmanTree::load_from_array(&[2, 4, 4, 4, 4, 2, 3, 3]).unwrap();
        iter_test(&tree, 0b00, 2, 0);
        iter_test(&tree, 0b0100, 4, 1);
        iter_test(&tree, 0b0101, 4, 2);
        iter_test(&tree, 0b0110, 4, 3);
        iter_test(&tree, 0b0111, 4, 4);
        iter_test(&tree, 0b10, 2, 5);
        iter_test(&tree, 0b110, 3, 6);
        iter_test(&tree, 0b111, 3, 7);
        let lengths: Vec<u8> = (1..=32).chain([32]).collect();
        VorbisHuffmanTree::load_from_array(&lengths).unwrap();
    }

    #[test]
    fn unordered_tree_walks_bit_by_bit() {
        let tree = VorbisHuffmanTree::load_from_array(&[2, 4, 4, 2, 4, 4, 3, 3]).unwrap();
        iter_test(&tree, 0b00, 2, 0);
        iter_test(&tree, 0b0100, 4, 1);
        iter_test(&tree, 0b0101, 4, 2);
        iter_test(&tree, 0b10, 2, 3);
        iter_test(&tree, 0b0110, 4, 4);
        iter_test(&tree, 0b0111, 4, 5);
        iter_test(&tree, 0b110, 3, 6);
        iter_test(&tree, 0b111, 3, 7);
    }

    #[test]
    fn all_zero_lengths_make_an_empty_tree_that_reads_nothing() {
        let tree = VorbisHuffmanTree::load_from_array(&[0; 625]).unwrap();
        assert!(tree.is_empty());
        let mut cur = BitpackCursor::new(&[0xff; 4]);
        assert!(cur.read_huffman(&tree).is_err());
        let mut it = tree.iter();
        assert_eq!(it.next(true), None);
    }

    #[test]
    fn under_and_over_specified_trees_are_refused() {
        assert_eq!(
            VorbisHuffmanTree::load_from_array(&[2, 4, 4, 4, 4, 2, 3]).err(),
            Some(HuffmanError::Underpopulated)
        );
        assert_eq!(
            VorbisHuffmanTree::load_from_array(&[2, 4, 4, 4, 2, 3, 3]).err(),
            Some(HuffmanError::Underpopulated)
        );
        assert_eq!(
            VorbisHuffmanTree::load_from_array(&[2, 4, 4, 4, 4, 2, 3, 3, 3]).err(),
            Some(HuffmanError::Overspecified)
        );
        assert_eq!(
            VorbisHuffmanTree::load_from_array(&[1, 33]).err(),
            Some(HuffmanError::Overspecified)
        );
    }

    #[test]
    fn single_entry_trees_decode_either_bit() {
        let tree = VorbisHuffmanTree::load_from_array(&[1]).unwrap();
        iter_test(&tree, 0b0, 1, 0);
        iter_test(&tree, 0b1, 1, 0);
        let tree = VorbisHuffmanTree::load_from_array(&[0, 0, 1, 0]).unwrap();
        iter_test(&tree, 0b0, 1, 2);
        iter_test(&tree, 0b1, 1, 2);
        let mut cur = BitpackCursor::new(&[0b10]);
        assert_eq!(cur.read_huffman(&tree), Ok(2));
        assert_eq!(cur.read_huffman(&tree), Ok(2));
        assert_eq!(cur.bits_left(), 6);
        assert_eq!(
            VorbisHuffmanTree::load_from_array(&[2]).err(),
            Some(HuffmanError::InvalidSingleEntry)
        );
    }

    #[test]
    fn extracted_real_tree_loads() {
        VorbisHuffmanTree::load_from_array(&[
            5, 6, 11, 11, 11, 11, 10, 10, 12, 11, 5, 2, 11, 5, 6, 6, 7, 9, 11, 13, 13, 10, 7, 11,
            6, 7, 8, 9, 10, 12, 11, 5, 11, 6, 8, 7, 9, 11, 14, 15, 11, 6, 6, 8, 4, 5, 7, 8, 10, 13,
            10, 5, 7, 7, 5, 5, 6, 8, 10, 11, 10, 7, 7, 8, 6, 5, 5, 7, 9, 9, 11, 8, 8, 11, 8, 7, 6,
            6, 7, 9, 12, 11, 10, 13, 9, 9, 7, 7, 7, 9, 11, 13, 12, 15, 12, 11, 9, 8, 8, 8,
        ])
        .unwrap();
    }

    #[test]
    fn read_huffman_decodes_short_and_deep_codes_through_the_table() {
        let lengths = [2u8, 4, 4, 4, 4, 2, 3, 3];
        let tree = VorbisHuffmanTree::load_from_array(&lengths).unwrap();
        let codes = [
            (0b00, 2),
            (0b0100, 4),
            (0b0101, 4),
            (0b0110, 4),
            (0b0111, 4),
            (0b10, 2),
            (0b110, 3),
            (0b111, 3),
        ];
        let order = [7usize, 0, 3, 5, 1, 6, 2, 4, 4, 0, 7];
        let seq: Vec<(u32, u8)> = order.iter().map(|&i| codes[i]).collect();
        let data = pack(&seq);
        let mut cur = BitpackCursor::new(&data);
        for &want in &order {
            assert_eq!(cur.read_huffman(&tree), Ok(want as u32));
        }

        let deep: Vec<u8> = (1..=12).chain([12]).collect();
        let tree = VorbisHuffmanTree::load_from_array(&deep).unwrap();
        let code_of = |i: usize| -> (u32, u8) {
            if i < 12 {
                (((1u32 << i) - 1) << 1, i as u8 + 1)
            } else {
                ((1u32 << 12) - 1, 12)
            }
        };
        let order = [11usize, 12, 0, 9, 3, 10, 12, 1];
        let seq: Vec<(u32, u8)> = order.iter().map(|&i| code_of(i)).collect();
        let data = pack(&seq);
        let mut cur = BitpackCursor::new(&data);
        for &want in &order {
            assert_eq!(cur.read_huffman(&tree), Ok(want as u32), "entry {want}");
        }
    }

    #[test]
    fn every_table_slot_agrees_with_the_bitwise_walk() {
        let lengths = [1u8, 0, 3, 3, 3, 4, 5, 6, 7, 8, 9, 9];
        let tree = VorbisHuffmanTree::load_from_array(&lengths).unwrap();
        for byte in 0..=255u8 {
            let data = [byte, 0x00, 0x00, 0x00];
            let mut fast = BitpackCursor::new(&data);
            let a = fast.read_huffman(&tree);
            let mut slow = BitpackCursor::new(&data);
            let mut it = tree.iter();
            let b = loop {
                if let Some(v) = it.next(slow.read_bit_flag().unwrap()) {
                    break v;
                }
            };
            assert_eq!(a, Ok(b), "byte {byte:#010b}");
            assert_eq!(fast.bits_left(), slow.bits_left(), "byte {byte:#010b}");
        }
    }

    #[test]
    fn a_code_cut_off_by_the_packet_end_is_an_error() {
        let tree =
            VorbisHuffmanTree::load_from_array(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 10]).unwrap();
        let mut cur = BitpackCursor::new(&[0xff]);
        assert!(cur.read_huffman(&tree).is_err());
        let mut cur = BitpackCursor::new(&[]);
        assert!(cur.read_huffman(&tree).is_err());
    }

    #[test]
    fn too_few_peeked_bits_fall_back_to_a_walk_from_the_root() {
        let tree = VorbisHuffmanTree::load_from_array(&[2, 2, 2, 2]).unwrap();
        assert!(matches!(
            tree.lookup_peeked_data(2, 0b10),
            PeekedDataLookupResult::PayloadFound(2, 1)
        ));
        let PeekedDataLookupResult::Iter(0, mut walk) = tree.lookup_peeked_data(1, 0b1) else {
            panic!("one bit of a two-bit code is not a payload");
        };
        assert_eq!(walk.next(true), None);
        assert_eq!(walk.next(true), Some(3));
        let deep =
            VorbisHuffmanTree::load_from_array(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 10]).unwrap();
        assert!(matches!(
            deep.lookup_peeked_data(7, 0xff),
            PeekedDataLookupResult::Iter(0, _)
        ));
        assert!(matches!(
            deep.lookup_peeked_data(8, 0xff),
            PeekedDataLookupResult::Iter(8, _)
        ));
        assert!(matches!(
            tree.lookup_peeked_data(8, 999),
            PeekedDataLookupResult::Iter(0, _)
        ));
    }
}
