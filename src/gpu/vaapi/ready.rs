use std::collections::VecDeque;

use super::{Accel, LAG, Picture, SLACK, backend};
use crate::media::Result;

const BURST: usize = 2;

struct Waiting {
    index: usize,
    pts: i64,
    out: u64,
    copied: Option<u64>,
}

#[derive(Default)]
pub struct Ready {
    queue: VecDeque<Waiting>,
    submitted: u64,
}

impl Ready {
    pub fn clear(&mut self) {
        self.queue.clear();
    }

    pub fn submitted(&mut self) {
        self.submitted += 1;
    }

    pub fn holds(&self, index: usize) -> bool {
        self.queue.iter().any(|w| w.index == index)
    }

    pub fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.queue.iter().map(|w| w.index)
    }

    pub fn push(&mut self, index: usize, pts: i64) {
        self.queue.push_back(Waiting {
            index,
            pts,
            out: self.submitted,
            copied: None,
        });
    }

    pub fn drain<A: Accel>(
        &mut self,
        accel: &mut A,
        referenced: impl Fn(usize) -> bool,
        keep: usize,
        frames: &mut Vec<Picture>,
    ) -> Result<()> {
        let deferred = accel.copies();
        for w in &mut self.queue {
            let late = self.submitted >= w.out + LAG as u64;
            if w.copied.is_none() && (!deferred || late || !referenced(w.index)) {
                accel.prefetch(w.index);
                w.copied = Some(self.submitted);
            }
        }
        let mut read = 0;
        while let Some(front) = self.queue.front() {
            let due = if deferred {
                keep == 0
                    || self.queue.len() > keep + SLACK
                    || (read < BURST
                        && front
                            .copied
                            .is_some_and(|at| self.submitted >= at + LAG as u64))
            } else {
                self.queue.len() > keep
            };
            if !due {
                break;
            }
            let Some(w) = self.queue.pop_front() else {
                break;
            };
            if w.copied.is_none() {
                accel.prefetch(w.index);
            }
            let picture = accel.read(w.index).map_err(backend)?;
            frames.push(picture.at(w.pts));
            read += 1;
        }
        Ok(())
    }
}
