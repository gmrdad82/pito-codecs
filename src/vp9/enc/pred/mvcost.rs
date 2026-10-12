use super::Mv;

pub use crate::vp9::tables::{MV_CLASS_TREE, MV_FR_TREE, MV_JOINT_TREE};

const TABLE: i32 = 1024;

pub use super::super::bool::cost as bit_cost;

pub fn tree_cost(tree: &[i8], probs: &[u8], value: u8) -> u32 {
    fn walk(tree: &[i8], probs: &[u8], node: usize, value: u8, acc: u32) -> Option<u32> {
        for b in 0..2usize {
            let t = tree[node + b];
            let c = acc + bit_cost(b == 1, probs[node >> 1]);
            if t <= 0 {
                if (-t) as u8 == value {
                    return Some(c);
                }
            } else if let Some(r) = walk(tree, probs, t as usize, value, c) {
                return Some(r);
            }
        }
        None
    }
    walk(tree, probs, 0, value, 0).unwrap_or(0)
}

#[derive(Clone, Copy, Debug)]
pub struct MvProbs {
    pub joints: [u8; 3],
    pub sign: [u8; 2],
    pub classes: [[u8; 10]; 2],
    pub class0: [u8; 2],
    pub bits: [[u8; 10]; 2],
    pub class0_fr: [[[u8; 3]; 2]; 2],
    pub fr: [[u8; 3]; 2],
    pub class0_hp: [u8; 2],
    pub hp: [u8; 2],
}

impl Default for MvProbs {
    fn default() -> Self {
        MvProbs {
            joints: [32, 64, 96],
            sign: [128, 128],
            classes: [
                [224, 144, 192, 168, 192, 176, 192, 198, 198, 245],
                [216, 128, 176, 160, 176, 176, 192, 198, 198, 208],
            ],
            class0: [216, 208],
            bits: [
                [136, 140, 148, 160, 176, 192, 224, 234, 234, 240],
                [136, 140, 148, 160, 176, 192, 224, 234, 234, 240],
            ],
            class0_fr: [
                [[128, 128, 64], [96, 112, 64]],
                [[128, 128, 64], [96, 112, 64]],
            ],
            fr: [[64, 96, 64], [64, 96, 64]],
            class0_hp: [160, 160],
            hp: [128, 128],
        }
    }
}

pub struct MvCost {
    joint: [u32; 4],
    comp: [[Vec<u32>; 2]; 2],
    probs: MvProbs,
}

fn mv_class(z: u32) -> u32 {
    if z < 16 {
        0
    } else {
        (31 - (z >> 3).leading_zeros()).min(10)
    }
}

fn component_cost(p: &MvProbs, comp: usize, v: i32, usehp: bool) -> u32 {
    if v == 0 {
        return 0;
    }
    let mut c = bit_cost(v < 0, p.sign[comp]);
    let z = v.unsigned_abs() - 1;
    let class = mv_class(z);
    c += tree_cost(&MV_CLASS_TREE, &p.classes[comp], class as u8);
    if class == 0 {
        let d = (z >> 3) as usize;
        c += bit_cost(d == 1, p.class0[comp]);
        c += tree_cost(&MV_FR_TREE, &p.class0_fr[comp][d], ((z >> 1) & 3) as u8);
        if usehp {
            c += bit_cost(z & 1 == 1, p.class0_hp[comp]);
        }
    } else {
        let offset = z - (2 << (class + 2));
        let d = offset >> 3;
        for i in 0..class as usize {
            c += bit_cost((d >> i) & 1 == 1, p.bits[comp][i]);
        }
        c += tree_cost(&MV_FR_TREE, &p.fr[comp], ((offset >> 1) & 3) as u8);
        if usehp {
            c += bit_cost(offset & 1 == 1, p.hp[comp]);
        }
    }
    c
}

impl MvCost {
    pub fn new(probs: MvProbs) -> Self {
        let joint = std::array::from_fn(|j| tree_cost(&MV_JOINT_TREE, &probs.joints, j as u8));
        let comp = std::array::from_fn(|comp| {
            std::array::from_fn(|hp| {
                (-TABLE..=TABLE)
                    .map(|v| component_cost(&probs, comp, v, hp == 1))
                    .collect()
            })
        });
        MvCost { joint, comp, probs }
    }

    #[inline(always)]
    fn comp(&self, comp: usize, v: i32, usehp: bool) -> u32 {
        if v.abs() <= TABLE {
            self.comp[comp][usehp as usize][(v + TABLE) as usize]
        } else {
            component_cost(&self.probs, comp, v, usehp)
        }
    }

    pub fn rate(&self, mv: Mv, reference: Mv, usehp: bool) -> u32 {
        let (dr, dc) = (
            mv.row as i32 - reference.row as i32,
            mv.col as i32 - reference.col as i32,
        );
        let j = ((dr != 0) as usize) << 1 | (dc != 0) as usize;
        self.joint[j] + self.comp(0, dr, usehp) + self.comp(1, dc, usehp)
    }
}
