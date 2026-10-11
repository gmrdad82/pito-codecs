use crate::vp9::spec_tables::AC_QLOOKUP;

pub fn qstep(q_index: u8) -> f64 {
    AC_QLOOKUP[q_index as usize] as f64
}

pub fn q_index_for(step: f64) -> u8 {
    let ac = &AC_QLOOKUP[..256];
    let i = ac.partition_point(|&s| (s as f64) < step);
    if i == 0 {
        return 0;
    }
    if i >= 256 {
        return 255;
    }
    let lo = ac[i - 1] as f64;
    let hi = ac[i] as f64;
    if step * step < lo * hi {
        (i - 1) as u8
    } else {
        i as u8
    }
}

pub fn lambda(q_index: u8) -> u32 {
    let s = AC_QLOOKUP[q_index as usize] as u64;
    ((s * s * 137) >> 16).max(1) as u32
}

pub fn crf_to_q(crf: u8) -> u8 {
    if crf >= 63 { 255 } else { crf * 4 }
}
