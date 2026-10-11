pub use crate::vp9::tables::*;

pub fn bands(tx_size: u8) -> &'static [u8] {
    if tx_size == 0 {
        &COEFBAND_4X4
    } else {
        &COEFBAND_8X8PLUS
    }
}
