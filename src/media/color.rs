#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

impl Color {
    pub const BT709: Color = Color {
        primaries: 1,
        transfer: 1,
        matrix: 1,
        full_range: false,
    };

    pub const UNKNOWN: Color = Color {
        primaries: 2,
        transfer: 2,
        matrix: 2,
        full_range: false,
    };

    pub fn described(&self) -> bool {
        self.primaries != 2 || self.transfer != 2 || self.matrix != 2
    }
}
