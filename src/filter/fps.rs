use crate::media::{Error, Frame, Result, TimeBase};

#[derive(Debug, Clone)]
pub struct FpsConverter {
    num: u32,
    den: u32,
    held: Option<(Frame, i64)>,
    next: Option<i64>,
}

impl FpsConverter {
    pub fn new(num: u32, den: u32) -> Result<Self> {
        if num == 0 || den == 0 {
            return Err(Error::Invalid(format!("frame rate {num}/{den}")));
        }
        Ok(FpsConverter {
            num,
            den,
            held: None,
            next: None,
        })
    }

    pub fn time_base(&self) -> TimeBase {
        TimeBase::new(self.den, self.num)
    }

    fn slot(&self, frame: &Frame) -> i64 {
        let a = frame.pts as i128 * frame.time_base.num as i128 * self.num as i128;
        let b = frame.time_base.den as i128 * self.den as i128;
        if b == 0 {
            return 0;
        }
        (2 * a + b).div_euclid(2 * b) as i64
    }

    pub fn push(&mut self, frame: Frame) -> Vec<Frame> {
        let slot = self.slot(&frame);
        let mut out = Vec::new();
        if let Some((held, at)) = self.held.take() {
            self.emit(held, at, slot, &mut out);
        }
        self.held = Some((frame, slot));
        out
    }

    pub fn flush(&mut self) -> Vec<Frame> {
        let mut out = Vec::new();
        if let Some((held, at)) = self.held.take() {
            let first = self.next.map_or(at, |n| n.max(at));
            self.emit(held, at, first + 1, &mut out);
        }
        out
    }

    fn emit(&mut self, frame: Frame, at: i64, until: i64, out: &mut Vec<Frame>) {
        let first = self.next.map_or(at, |n| n.max(at));
        if until <= first {
            return;
        }
        let tb = self.time_base();
        for slot in first..until - 1 {
            let mut f = frame.clone();
            f.pts = slot;
            f.time_base = tb;
            out.push(f);
        }
        let mut f = frame;
        f.pts = until - 1;
        f.time_base = tb;
        out.push(f);
        self.next = Some(until);
    }
}
