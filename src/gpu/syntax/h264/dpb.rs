use super::slice::{Mmco, SliceHeader, SliceType};
use crate::gpu::syntax::error::{Result, ensure};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefMark {
    Unused,
    Short,
    Long,
}

#[derive(Clone, Debug)]
pub struct OutputMeta {
    pub pts: i64,
}

pub struct DpbEntry<F> {
    pub frame: F,
    pub poc: i32,
    pub frame_num: u32,
    pub long_term_frame_idx: u32,
    pub mark: RefMark,
    pub needed_for_output: bool,
    pub non_existing: bool,
    pub meta: Arc<OutputMeta>,
}

pub struct Dpb<F> {
    pub entries: Vec<DpbEntry<F>>,
    pub max_long_term_frame_idx: Option<u32>,
    pub capacity: usize,
    pub max_reorder: usize,
}

pub struct Output<F> {
    pub frame: F,
    pub poc: i32,
    pub meta: Arc<OutputMeta>,
}

impl<F: Clone> Dpb<F> {
    pub fn new() -> Self {
        Dpb {
            entries: Vec::new(),
            max_long_term_frame_idx: None,
            capacity: 16,
            max_reorder: 16,
        }
    }

    fn frame_num_wrap(e: &DpbEntry<F>, cur_frame_num: u32, max_frame_num: u32) -> i32 {
        if e.frame_num > cur_frame_num {
            e.frame_num as i32 - max_frame_num as i32
        } else {
            e.frame_num as i32
        }
    }

    fn num_refs(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.mark != RefMark::Unused)
            .count()
    }

    fn prune(&mut self) {
        self.entries
            .retain(|e| e.needed_for_output || e.mark != RefMark::Unused);
    }

    fn bump(&mut self, out: &mut Vec<Output<F>>) -> bool {
        let Some(i) = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.needed_for_output)
            .min_by_key(|(_, e)| e.poc)
            .map(|(i, _)| i)
        else {
            return false;
        };
        let e = &mut self.entries[i];
        e.needed_for_output = false;
        out.push(Output {
            frame: e.frame.clone(),
            poc: e.poc,
            meta: e.meta.clone(),
        });
        self.prune();
        true
    }

    pub fn flush(&mut self, out: &mut Vec<Output<F>>) {
        while self.bump(out) {}
        self.prune();
    }

    pub fn idr(&mut self, no_output_of_prior_pics: bool, out: &mut Vec<Output<F>>) {
        for e in &mut self.entries {
            e.mark = RefMark::Unused;
        }
        if no_output_of_prior_pics {
            self.entries.clear();
        } else {
            self.flush(out);
        }
        self.max_long_term_frame_idx = None;
    }

    fn sliding_window(
        &mut self,
        cur_frame_num: u32,
        max_frame_num: u32,
        max_num_ref_frames: usize,
    ) {
        let limit = max_num_ref_frames.max(1);
        while self.num_refs() >= limit {
            let victim = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.mark == RefMark::Short)
                .min_by_key(|(_, e)| Self::frame_num_wrap(e, cur_frame_num, max_frame_num))
                .map(|(i, _)| i);
            match victim {
                Some(i) => self.entries[i].mark = RefMark::Unused,
                None => break,
            }
        }
        self.prune();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fill_frame_num_gap(
        &mut self,
        prev_ref_frame_num: u32,
        frame_num: u32,
        max_frame_num: u32,
        max_num_ref_frames: usize,
        make_frame: &mut dyn FnMut(u32) -> (F, i32, Arc<OutputMeta>),
        poc_state: &mut dyn FnMut(u32),
    ) {
        let mut unused = (prev_ref_frame_num + 1) % max_frame_num;
        let mut guard = 0;
        while unused != frame_num && guard < max_frame_num {
            self.sliding_window(unused, max_frame_num, max_num_ref_frames);
            let (frame, poc, meta) = make_frame(unused);
            poc_state(unused);
            self.entries.push(DpbEntry {
                poc,
                frame,
                frame_num: unused,
                long_term_frame_idx: 0,
                mark: RefMark::Short,
                needed_for_output: false,
                non_existing: true,
                meta,
            });
            unused = (unused + 1) % max_frame_num;
            guard += 1;
        }
    }

    pub fn build_ref_lists<R>(
        &self,
        sh: &SliceHeader,
        cur_poc: i32,
        max_frame_num: u32,
        to_ref: impl Fn(&DpbEntry<F>) -> R,
    ) -> Result<[Vec<R>; 2]> {
        let mut lists: [Vec<R>; 2] = [Vec::new(), Vec::new()];
        if sh.slice_type.is_intra() {
            return Ok(lists);
        }
        let cur_fn = sh.frame_num;
        let pic_num = |e: &DpbEntry<F>| Self::frame_num_wrap(e, cur_fn, max_frame_num);
        let short: Vec<&DpbEntry<F>> = self
            .entries
            .iter()
            .filter(|e| e.mark == RefMark::Short)
            .collect();
        let mut long: Vec<&DpbEntry<F>> = self
            .entries
            .iter()
            .filter(|e| e.mark == RefMark::Long)
            .collect();
        long.sort_by_key(|e| e.long_term_frame_idx);
        let mut entry_lists: [Vec<&DpbEntry<F>>; 2] = [Vec::new(), Vec::new()];
        if sh.slice_type != SliceType::B {
            let mut s = short.clone();
            s.sort_by_key(|e| std::cmp::Reverse(pic_num(e)));
            entry_lists[0] = s.into_iter().chain(long.iter().copied()).collect();
        } else {
            let mut before: Vec<&DpbEntry<F>> =
                short.iter().copied().filter(|e| e.poc < cur_poc).collect();
            let mut after: Vec<&DpbEntry<F>> =
                short.iter().copied().filter(|e| e.poc > cur_poc).collect();
            before.sort_by_key(|e| std::cmp::Reverse(e.poc));
            after.sort_by_key(|e| e.poc);
            entry_lists[0] = before
                .iter()
                .chain(after.iter())
                .chain(long.iter())
                .copied()
                .collect();
            entry_lists[1] = after
                .iter()
                .chain(before.iter())
                .chain(long.iter())
                .copied()
                .collect();
            if entry_lists[1].len() > 1 && entry_lists[0].len() == entry_lists[1].len() {
                let same = entry_lists[0]
                    .iter()
                    .zip(entry_lists[1].iter())
                    .all(|(a, b)| std::ptr::eq(*a, *b));
                if same {
                    entry_lists[1].swap(0, 1);
                }
            }
        }
        let nlists = if sh.slice_type == SliceType::B { 2 } else { 1 };
        for l in 0..nlists {
            let n = sh.num_ref_idx_active[l] as usize;
            let mut list: Vec<Option<&DpbEntry<F>>> =
                entry_lists[l].iter().copied().map(Some).collect();
            list.truncate(n);
            list.resize(n, None);
            if !sh.ref_pic_list_mod[l].is_empty() {
                let max_pic_num = max_frame_num as i32;
                let curr_pic_num = cur_fn as i32;
                let mut pred = curr_pic_num;
                let mut ref_idx = 0usize;
                for m in &sh.ref_pic_list_mod[l] {
                    ensure!(ref_idx < n, "too many ref_pic_list_modification entries");
                    list.push(None);
                    let (target, is_long): (Option<&DpbEntry<F>>, bool) = match m.idc {
                        0 | 1 => {
                            let d = m.value as i32 + 1;
                            let no_wrap = if m.idc == 0 {
                                if pred - d < 0 {
                                    pred - d + max_pic_num
                                } else {
                                    pred - d
                                }
                            } else if pred + d >= max_pic_num {
                                pred + d - max_pic_num
                            } else {
                                pred + d
                            };
                            pred = no_wrap;
                            let pn = if no_wrap > curr_pic_num {
                                no_wrap - max_pic_num
                            } else {
                                no_wrap
                            };
                            (short.iter().copied().find(|e| pic_num(e) == pn), false)
                        }
                        2 => (
                            long.iter()
                                .copied()
                                .find(|e| e.long_term_frame_idx == m.value),
                            true,
                        ),
                        _ => continue,
                    };
                    let Some(target) = target else {
                        list.pop();
                        ref_idx += 1;
                        continue;
                    };
                    for c in (ref_idx + 1..=n).rev() {
                        list[c] = list[c - 1];
                    }
                    list[ref_idx] = Some(target);
                    ref_idx += 1;
                    let mut nidx = ref_idx;
                    for c in ref_idx..=n {
                        let keep = match list[c] {
                            None => true,
                            Some(e) => {
                                if is_long {
                                    !(e.mark == RefMark::Long
                                        && e.long_term_frame_idx == target.long_term_frame_idx)
                                } else {
                                    !(e.mark == RefMark::Short && std::ptr::eq(e, target))
                                }
                            }
                        };
                        if keep {
                            list[nidx] = list[c];
                            nidx += 1;
                        }
                    }
                    list.truncate(n);
                }
            }
            let fallback = list
                .iter()
                .flatten()
                .next()
                .copied()
                .or_else(|| self.entries.last());
            let out: Vec<R> = list
                .iter()
                .filter_map(|e| e.or(fallback))
                .map(&to_ref)
                .collect();
            lists[l] = out;
        }
        Ok(lists)
    }

    fn apply_mmcos(
        &mut self,
        mmcos: &[Mmco],
        cur_frame_num: u32,
        max_frame_num: u32,
        cur_long: &mut Option<u32>,
    ) -> bool {
        let mut had5 = false;
        let curr_pic_num = cur_frame_num as i32;
        for m in mmcos {
            match m.op {
                1 => {
                    let pn = curr_pic_num - (m.difference_of_pic_nums_minus1 as i32 + 1);
                    for e in &mut self.entries {
                        if e.mark == RefMark::Short
                            && Self::frame_num_wrap(e, cur_frame_num, max_frame_num) == pn
                        {
                            e.mark = RefMark::Unused;
                        }
                    }
                }
                2 => {
                    for e in &mut self.entries {
                        if e.mark == RefMark::Long && e.long_term_frame_idx == m.long_term_pic_num {
                            e.mark = RefMark::Unused;
                        }
                    }
                }
                3 => {
                    let pn = curr_pic_num - (m.difference_of_pic_nums_minus1 as i32 + 1);
                    for e in &mut self.entries {
                        if e.mark == RefMark::Long && e.long_term_frame_idx == m.long_term_frame_idx
                        {
                            e.mark = RefMark::Unused;
                        }
                    }
                    for e in &mut self.entries {
                        if e.mark == RefMark::Short
                            && Self::frame_num_wrap(e, cur_frame_num, max_frame_num) == pn
                        {
                            e.mark = RefMark::Long;
                            e.long_term_frame_idx = m.long_term_frame_idx;
                        }
                    }
                }
                4 => {
                    let max = m.max_long_term_frame_idx_plus1;
                    for e in &mut self.entries {
                        if e.mark == RefMark::Long && (max == 0 || e.long_term_frame_idx > max - 1)
                        {
                            e.mark = RefMark::Unused;
                        }
                    }
                    self.max_long_term_frame_idx = if max == 0 { None } else { Some(max - 1) };
                }
                5 => {
                    for e in &mut self.entries {
                        e.mark = RefMark::Unused;
                    }
                    self.max_long_term_frame_idx = None;
                    had5 = true;
                }
                6 => {
                    for e in &mut self.entries {
                        if e.mark == RefMark::Long && e.long_term_frame_idx == m.long_term_frame_idx
                        {
                            e.mark = RefMark::Unused;
                        }
                    }
                    *cur_long = Some(m.long_term_frame_idx);
                }
                _ => {}
            }
        }
        self.prune();
        had5
    }

    #[allow(clippy::too_many_arguments)]
    pub fn store_picture(
        &mut self,
        sh: &SliceHeader,
        frame: F,
        poc: i32,
        max_frame_num: u32,
        max_num_ref_frames: usize,
        meta: Arc<OutputMeta>,
        out: &mut Vec<Output<F>>,
    ) {
        let mut mark = RefMark::Unused;
        let mut long_idx = 0;
        let mut had5 = false;
        if sh.nal_ref_idc != 0 {
            if sh.idr {
                if sh.long_term_reference {
                    mark = RefMark::Long;
                    self.max_long_term_frame_idx = Some(0);
                } else {
                    mark = RefMark::Short;
                    self.max_long_term_frame_idx = None;
                }
            } else {
                let mut cur_long = None;
                if sh.adaptive_ref_pic_marking {
                    had5 = self.apply_mmcos(&sh.mmcos, sh.frame_num, max_frame_num, &mut cur_long);
                } else {
                    self.sliding_window(sh.frame_num, max_frame_num, max_num_ref_frames);
                }
                if let Some(i) = cur_long {
                    mark = RefMark::Long;
                    long_idx = i;
                } else {
                    mark = RefMark::Short;
                    if self.num_refs() >= max_num_ref_frames.max(1) && sh.adaptive_ref_pic_marking {
                        self.sliding_window(sh.frame_num, max_frame_num, max_num_ref_frames);
                    }
                }
            }
        }
        if had5 {
            self.flush(out);
        }
        let frame_num = if had5 { 0 } else { sh.frame_num };
        if mark == RefMark::Unused {
            loop {
                let waiting = self.entries.len();
                if waiting < self.capacity {
                    break;
                }
                let min_poc = self
                    .entries
                    .iter()
                    .filter(|e| e.needed_for_output)
                    .map(|e| e.poc)
                    .min();
                if min_poc.is_none_or(|m| poc < m) {
                    out.push(Output { frame, poc, meta });
                    return;
                }
                if !self.bump(out) {
                    break;
                }
            }
        } else {
            while self.entries.len() >= self.capacity {
                if !self.bump(out) {
                    break;
                }
            }
        }
        self.entries.push(DpbEntry {
            frame,
            poc,
            frame_num,
            long_term_frame_idx: long_idx,
            mark,
            needed_for_output: true,
            non_existing: false,
            meta,
        });
        while self.entries.iter().filter(|e| e.needed_for_output).count() > self.max_reorder {
            if !self.bump(out) {
                break;
            }
        }
    }
}

impl<F: Clone> Default for Dpb<F> {
    fn default() -> Self {
        Self::new()
    }
}
