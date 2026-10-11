use crate::opus::celt::rate::{Allocation, MAX_FINE_BITS, Mode};
use crate::opus::celt::tables::*;
use crate::opus::range::{BITRES, RangeEncoder};

const FINE_OFFSET: i32 = 21;
const ALLOC_STEPS: i32 = 6;

pub struct AllocChoice {
    pub prev_coded: usize,
    pub signal_bandwidth: usize,
    pub intensity: usize,
    pub dual_stereo: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn compute_allocation_enc(
    m: &Mode,
    start: usize,
    end: usize,
    offsets: &[i32; NB_EBANDS],
    cap: &[i32; NB_EBANDS],
    alloc_trim: i32,
    total: i32,
    c: usize,
    lm: usize,
    choice: &AllocChoice,
    enc: &mut RangeEncoder,
) -> Allocation {
    let ci = c as i32;
    let lmi = lm as i32;
    let mut total = total.max(0);
    let mut skip_start = start;
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    let mut intensity_rsv = 0;
    let mut dual_stereo_rsv = 0;
    if c == 2 {
        intensity_rsv = LOG2_FRAC_TABLE[end - start];
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_stereo_rsv;
        }
    }
    let mut bits1 = [0i32; NB_EBANDS];
    let mut bits2 = [0i32; NB_EBANDS];
    let mut thresh = [0i32; NB_EBANDS];
    let mut trim_offset = [0i32; NB_EBANDS];
    for j in start..end {
        let w = EBANDS[j + 1] - EBANDS[j];
        thresh[j] = (ci << BITRES).max((3 * w << lmi << BITRES) >> 4);
        trim_offset[j] = ci
            * w
            * (alloc_trim - 5 - lmi)
            * (end as i32 - j as i32 - 1)
            * (1 << (lmi + BITRES as i32))
            >> 6;
        if w << lmi == 1 {
            trim_offset[j] -= ci << BITRES;
        }
    }
    let nb_alloc = ALLOC.len() as i32;
    let mut lo = 1;
    let mut hi = nb_alloc - 1;
    loop {
        let mut done = false;
        let mut psum = 0;
        let mid = (lo + hi) >> 1;
        for j in (start..end).rev() {
            let n = EBANDS[j + 1] - EBANDS[j];
            let mut bitsj = ci * n * (ALLOC[mid as usize][j] as i32) << lmi >> 2;
            if bitsj > 0 {
                bitsj = (bitsj + trim_offset[j]).max(0);
            }
            bitsj += offsets[j];
            if bitsj >= thresh[j] || done {
                done = true;
                psum += bitsj.min(cap[j]);
            } else if bitsj >= ci << BITRES {
                psum += ci << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
        if lo > hi {
            break;
        }
    }
    hi = lo;
    lo -= 1;
    for j in start..end {
        let n = EBANDS[j + 1] - EBANDS[j];
        let mut bits1j = ci * n * (ALLOC[lo as usize][j] as i32) << lmi >> 2;
        let mut bits2j = if hi >= nb_alloc {
            cap[j]
        } else {
            ci * n * (ALLOC[hi as usize][j] as i32) << lmi >> 2
        };
        if bits1j > 0 {
            bits1j = (bits1j + trim_offset[j]).max(0);
        }
        if bits2j > 0 {
            bits2j = (bits2j + trim_offset[j]).max(0);
        }
        if lo > 0 {
            bits1j += offsets[j];
        }
        bits2j += offsets[j];
        if offsets[j] > 0 {
            skip_start = j;
        }
        bits2j = (bits2j - bits1j).max(0);
        bits1[j] = bits1j;
        bits2[j] = bits2j;
    }
    interp_bits2pulses(
        m,
        start,
        end,
        skip_start,
        &bits1,
        &bits2,
        &thresh,
        cap,
        total,
        skip_rsv,
        intensity_rsv,
        dual_stereo_rsv,
        c,
        lm,
        choice,
        enc,
    )
}

#[allow(clippy::too_many_arguments)]
fn interp_bits2pulses(
    m: &Mode,
    start: usize,
    end: usize,
    skip_start: usize,
    bits1: &[i32; NB_EBANDS],
    bits2: &[i32; NB_EBANDS],
    thresh: &[i32; NB_EBANDS],
    cap: &[i32; NB_EBANDS],
    mut total: i32,
    skip_rsv: i32,
    mut intensity_rsv: i32,
    mut dual_stereo_rsv: i32,
    c: usize,
    lm: usize,
    choice: &AllocChoice,
    enc: &mut RangeEncoder,
) -> Allocation {
    let ci = c as i32;
    let alloc_floor = ci << BITRES;
    let stereo = (c > 1) as i32;
    let log_m = (lm as i32) << BITRES;
    let mut lo = 0;
    let mut hi = 1 << ALLOC_STEPS;
    for _ in 0..ALLOC_STEPS {
        let mid = (lo + hi) >> 1;
        let mut psum = 0;
        let mut done = false;
        for j in (start..end).rev() {
            let tmp = bits1[j] + (mid * bits2[j] >> ALLOC_STEPS);
            if tmp >= thresh[j] || done {
                done = true;
                psum += tmp.min(cap[j]);
            } else if tmp >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let mut bits = [0i32; NB_EBANDS];
    let mut psum = 0;
    let mut done = false;
    for j in (start..end).rev() {
        let mut tmp = bits1[j] + (lo * bits2[j] >> ALLOC_STEPS);
        if tmp < thresh[j] && !done {
            tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        tmp = tmp.min(cap[j]);
        bits[j] = tmp;
        psum += tmp;
    }
    let s = start;
    let mut coded_bands = end;
    loop {
        let j = coded_bands - 1;
        if j <= skip_start {
            total += skip_rsv;
            break;
        }
        let mut left = total - psum;
        let span = EBANDS[coded_bands] - EBANDS[s];
        let percoeff = left / span;
        left -= span * percoeff;
        let rem = (left - (EBANDS[j] - EBANDS[s])).max(0);
        let band_width = EBANDS[coded_bands] - EBANDS[j];
        let mut band_bits = bits[j] + percoeff * band_width + rem;
        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            let depth = if j < choice.prev_coded { 7 } else { 9 };
            let keep = coded_bands <= s + 2
                || (band_bits > (depth * band_width << lm << BITRES) >> 4
                    && j <= choice.signal_bandwidth);
            enc.bit_logp(keep, 1);
            if keep {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= bits[j] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = LOG2_FRAC_TABLE[j - s];
        }
        psum += intensity_rsv;
        if band_bits >= alloc_floor {
            psum += alloc_floor;
            bits[j] = alloc_floor;
        } else {
            bits[j] = 0;
        }
        coded_bands -= 1;
    }
    let intensity = if intensity_rsv > 0 {
        let v = choice.intensity.clamp(s, coded_bands);
        enc.uint((v - s) as u32, (coded_bands + 1 - s) as u32);
        v
    } else {
        0
    };
    if intensity <= s {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    let dual_stereo = if dual_stereo_rsv > 0 {
        enc.bit_logp(choice.dual_stereo, 1);
        choice.dual_stereo
    } else {
        false
    };
    let mut left = total - psum;
    let span = EBANDS[coded_bands] - EBANDS[s];
    let percoeff = left / span;
    left -= span * percoeff;
    for j in s..coded_bands {
        bits[j] += percoeff * (EBANDS[j + 1] - EBANDS[j]);
    }
    for j in s..coded_bands {
        let tmp = left.min(EBANDS[j + 1] - EBANDS[j]);
        bits[j] += tmp;
        left -= tmp;
    }
    let mut ebits = [0i32; NB_EBANDS];
    let mut fine_priority = [0i32; NB_EBANDS];
    let mut balance = 0;
    for j in s..coded_bands {
        let n0 = EBANDS[j + 1] - EBANDS[j];
        let n = n0 << lm;
        let bit = bits[j] + balance;
        let mut excess;
        if n > 1 {
            excess = (bit - cap[j]).max(0);
            bits[j] = bit - excess;
            let den = ci * n
                + if c == 2 && n > 2 && !dual_stereo && j < intensity {
                    1
                } else {
                    0
                };
            let nclogn = den * (m.log_n[j] + log_m);
            let mut offset = (nclogn >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += (den << BITRES) >> 2;
            }
            if bits[j] + offset < den * 2 << BITRES {
                offset += nclogn >> 2;
            } else if bits[j] + offset < den * 3 << BITRES {
                offset += nclogn >> 3;
            }
            let mut eb = (bits[j] + offset + (den << (BITRES - 1))).max(0);
            eb = (eb / den) >> BITRES;
            if ci * eb > (bits[j] >> BITRES) {
                eb = bits[j] >> stereo >> BITRES;
            }
            eb = eb.min(MAX_FINE_BITS);
            ebits[j] = eb;
            fine_priority[j] = (eb * (den << BITRES) >= bits[j] + offset) as i32;
            bits[j] -= ci * eb << BITRES;
        } else {
            excess = (bit - (ci << BITRES)).max(0);
            bits[j] = bit - excess;
            ebits[j] = 0;
            fine_priority[j] = 1;
        }
        if excess > 0 {
            let extra_fine = (excess >> (stereo + BITRES as i32)).min(MAX_FINE_BITS - ebits[j]);
            ebits[j] += extra_fine;
            let extra_bits = extra_fine * ci << BITRES;
            fine_priority[j] = (extra_bits >= excess - balance) as i32;
            excess -= extra_bits;
        }
        balance = excess;
    }
    for j in coded_bands..end {
        ebits[j] = bits[j] >> stereo >> BITRES;
        bits[j] = 0;
        fine_priority[j] = (ebits[j] < 1) as i32;
    }
    Allocation {
        coded_bands,
        intensity,
        dual_stereo,
        balance,
        pulses: bits,
        fine_quant: ebits,
        fine_priority,
    }
}
