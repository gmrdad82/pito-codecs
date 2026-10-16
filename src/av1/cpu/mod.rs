#![allow(
    non_upper_case_globals,
    clippy::wrong_self_convention,
    clippy::manual_is_multiple_of,
    clippy::missing_safety_doc,
    unsafe_op_in_unsafe_fn,
    clippy::derivable_impls,
    clippy::erasing_op,
    clippy::ptr_eq,
    clippy::arc_with_non_send_sync,
    clippy::borrow_deref_ref,
    clippy::borrowed_box,
    clippy::cast_abs_to_unsigned,
    clippy::clone_on_copy,
    clippy::collapsible_else_if,
    clippy::collapsible_if,
    clippy::doc_overindented_list_items,
    clippy::duplicate_underscore_argument,
    clippy::explicit_auto_deref,
    clippy::identity_op,
    clippy::incompatible_msrv,
    clippy::int_plus_one,
    clippy::into_iter_on_ref,
    clippy::large_const_arrays,
    clippy::large_enum_variant,
    clippy::len_without_is_empty,
    clippy::len_zero,
    clippy::let_and_return,
    clippy::let_underscore_lock,
    clippy::manual_div_ceil,
    clippy::manual_range_contains,
    clippy::manual_saturating_arithmetic,
    clippy::module_inception,
    clippy::misrefactored_assign_op,
    clippy::needless_borrow,
    clippy::needless_late_init,
    clippy::needless_lifetimes,
    clippy::needless_option_as_deref,
    clippy::needless_range_loop,
    clippy::needless_return,
    clippy::neg_multiply,
    clippy::nonminimal_bool,
    clippy::option_map_unit_fn,
    clippy::partialeq_to_none,
    clippy::precedence,
    clippy::redundant_closure,
    clippy::redundant_pattern_matching,
    clippy::redundant_static_lifetimes,
    clippy::search_is_some,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::unit_arg,
    clippy::uninlined_format_args,
    clippy::unnecessary_cast,
    clippy::unnecessary_fallible_conversions,
    clippy::unnecessary_map_on_constructor,
    clippy::unnecessary_mut_passed,
    clippy::unnecessary_lazy_evaluations,
    clippy::unneeded_wildcard_pattern,
    clippy::upper_case_acronyms,
    clippy::useless_conversion,
    clippy::zero_prefixed_literal
)]

pub(crate) mod include {
    pub(crate) mod common {
        pub(crate) mod attributes;
        pub(crate) mod bitdepth;
        pub(crate) mod dump;
        pub(crate) mod intops;
        pub(crate) mod validate;
    }
    pub(crate) mod dav1d {
        pub(crate) mod common;
        pub(crate) mod data;
        pub(crate) mod dav1d;
        pub(crate) mod headers;
        pub(crate) mod picture;
    }
}

pub(crate) mod align;
pub(crate) mod c_arc;
pub(crate) mod c_box;
pub(crate) mod cdef;
pub(crate) mod cdef_apply;
pub(crate) mod cdf;
pub(crate) mod const_fn;
pub(crate) mod cpu;
pub(crate) mod ctx;
pub(crate) mod cursor;
pub(crate) mod data;
pub(crate) mod decode;
pub(crate) mod dequant_tables;
pub(crate) mod disjoint_mut;
pub(crate) mod enum_map;
pub(crate) mod env;
pub(crate) mod error;
pub(crate) mod ffi_safe;
pub(crate) mod fg_apply;
pub(crate) mod filmgrain;
pub(crate) mod getbits;
pub(crate) mod pic_or_buf;
pub(crate) mod pixels;
pub(crate) mod relaxed_atomic;
pub(crate) mod send_sync_non_null;
pub(crate) mod strided;
pub(crate) mod with_offset;
pub(crate) mod wrap_fn_ptr;

pub(crate) mod extensions;
pub(crate) mod in_range;
pub(crate) mod internal;
pub(crate) mod intra_edge;
pub(crate) mod ipred;
pub(crate) mod ipred_prepare;
pub(crate) mod iter;
pub(crate) mod itx;
pub(crate) mod itx_1d;
pub(crate) mod levels;
pub(crate) mod lf_apply;
pub(crate) mod lf_mask;
pub(crate) mod log;
pub(crate) mod loopfilter;
pub(crate) mod looprestoration;
pub(crate) mod lr_apply;
pub(crate) mod mc;
pub(crate) mod msac;
pub(crate) mod obu;
pub(crate) mod pal;
pub(crate) mod picture;
pub(crate) mod pool;
pub(crate) mod qm;
pub(crate) mod recon;
pub(crate) mod refmvs;

pub(crate) mod atom;
pub(crate) mod decoder;
pub(crate) mod enums;
pub(crate) mod flags;
pub(crate) mod pod;
pub(crate) mod scan;
pub(crate) mod sync;
pub(crate) mod tables;
pub(crate) mod thread_task;
pub(crate) mod warpmv;
pub(crate) mod wedge;

pub use decoder::{Decoder, Layout, Picture, SequenceInfo};

use std::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Once};
use std::{cmp, mem, thread};

use crate::av1::cpu::sync::Mutex;

use crate::av1::cpu::cpu::{rav1d_init_cpu, rav1d_num_logical_processors};
use crate::av1::cpu::decode::rav1d_decode_frame_exit;
pub use crate::av1::cpu::error::{Dav1dResult, Rav1dError, Rav1dResult};
use crate::av1::cpu::extensions::OptionError as _;
use crate::av1::cpu::in_range::InRange;
use crate::av1::cpu::include::common::bitdepth::BitDepth8;
use crate::av1::cpu::include::common::bitdepth::BitDepth16;
use crate::av1::cpu::include::common::intops::clip;
use crate::av1::cpu::include::common::validate::validate_input;
use crate::av1::cpu::include::dav1d::data::Rav1dData;
use crate::av1::cpu::include::dav1d::dav1d::{
    RAV1D_MAX_THREADS, Rav1dDecodeFrameType, Rav1dInloopFilterType, Rav1dSettings,
};
use crate::av1::cpu::include::dav1d::headers::Rav1dFilmGrainData;
use crate::av1::cpu::include::dav1d::picture::Rav1dPicture;
use crate::av1::cpu::internal::{
    Rav1dBitDepthDSPContext, Rav1dContext, Rav1dContextFrameThread, Rav1dContextTaskThread,
    Rav1dContextTaskType, Rav1dFrameContext, Rav1dState, Rav1dTaskContext,
    Rav1dTaskContextTaskThread, TaskThreadData,
};
use crate::av1::cpu::iter::wrapping_iter;
use crate::av1::cpu::log::{Rav1dLog as _, Rav1dLogger};
use crate::av1::cpu::obu::rav1d_parse_obus;
use crate::av1::cpu::picture::{PictureFlags, rav1d_picture_alloc_copy};
use crate::av1::cpu::send_sync_non_null::SendSyncNonNull;
use crate::av1::cpu::thread_task::{FRAME_ERROR, rav1d_task_delayed_fg, rav1d_worker_task};

#[cold]
fn init_internal() {
    rav1d_init_cpu();
}

const DAV1D_VERSION: &CStr = c"966d63c1";
const RAV1D_VERSION: &str = match DAV1D_VERSION.to_str() {
    Ok(version) => version,
    Err(_) => unreachable!(),
};

pub const fn rav1d_version() -> &'static str {
    RAV1D_VERSION
}

pub const DAV1D_API_VERSION_MAJOR: u8 = 7;
pub const DAV1D_API_VERSION_MINOR: u8 = 0;
pub const DAV1D_API_VERSION_PATCH: u8 = 0;

impl Default for Rav1dSettings {
    fn default() -> Self {
        Self {
            n_threads: InRange::new(0).unwrap(),
            max_frame_delay: InRange::new(0).unwrap(),
            apply_grain: true,
            operating_point: InRange::new(0).unwrap(),
            all_layers: true,
            frame_size_limit: 0,
            allocator: Default::default(),
            logger: Some(Rav1dLogger::default()),
            strict_std_compliance: false,
            output_invisible_frames: false,
            inloop_filters: Rav1dInloopFilterType::all(),
            decode_frame_type: Rav1dDecodeFrameType::All,
        }
    }
}

struct NumThreads {
    n_tc: u16,
    n_fc: u16,
}

#[cold]
fn get_num_threads(s: &Rav1dSettings) -> NumThreads {
    let n_tc = if s.n_threads.get() != 0 {
        s.n_threads.get()
    } else {
        clip(
            rav1d_num_logical_processors().get(),
            1,
            const { RAV1D_MAX_THREADS as u16 },
        )
    };
    let n_fc = if s.max_frame_delay.get() != 0 {
        cmp::min(s.max_frame_delay.get(), n_tc)
    } else {
        cmp::min((n_tc as f64).sqrt().ceil() as u16, 8)
    };
    NumThreads { n_fc, n_tc }
}

#[cold]
pub(crate) fn rav1d_open(s: &Rav1dSettings) -> Rav1dResult<Arc<Rav1dContext>> {
    static INITTED: Once = Once::new();
    INITTED.call_once(|| init_internal());

    validate_input!((
        !s.allocator.is_default() || s.allocator.cookie.is_none(),
        Rav1dError::InvalidArgument
    ))?;

    let frame_size_limit;
    if mem::size_of::<usize>() < 8 && s.frame_size_limit.wrapping_sub(1) >= 8192 * 8192 {
        frame_size_limit = 8192 * 8192;
        if s.frame_size_limit != 0 {
            writeln!(
                s.logger,
                "Frame size limit reduced from {} to {}.",
                s.frame_size_limit, frame_size_limit,
            );
        }
    } else {
        frame_size_limit = s.frame_size_limit;
    }

    let NumThreads { n_tc, n_fc } = get_num_threads(s);

    let ttd = TaskThreadData {
        cur: (n_fc as u32).into(),
        reset_task_cur: AtomicU32::new(u32::MAX),
        ..Default::default()
    };

    let task_thread = Arc::new(ttd);

    let fc = (0..n_fc)
        .map(|i| {
            let mut fc = Rav1dFrameContext::default(i.into());
            fc.task_thread.finished = AtomicBool::new(true);
            fc.task_thread.ttd = Arc::clone(&task_thread);
            let f = fc.data.get_mut();
            f.lf.last_sharpness = u8::MAX;
            fc
        })
        .collect();

    let state = Mutex::new(Rav1dState {
        frame_thread: Rav1dContextFrameThread {
            out_delayed: if n_fc > 1 {
                (0..n_fc).map(|_| Default::default()).collect()
            } else {
                Box::new([])
            },
            ..Default::default()
        },
        ..Default::default()
    });

    let tc = (0..n_tc)
        .map(|n| {
            let task_thread = Arc::clone(&task_thread);
            let thread_data = Arc::new(Rav1dTaskContextTaskThread::new(task_thread));
            let thread_data_copy = Arc::clone(&thread_data);
            let task = if n_tc > 1 {
                let handle = thread::Builder::new()
                    .name(format!("rav1d-worker-{n}"))
                    .spawn(|| rav1d_worker_task(thread_data_copy))
                    .unwrap();
                Rav1dContextTaskType::Worker(handle)
            } else {
                Rav1dContextTaskType::Single(Mutex::new(Box::new(Rav1dTaskContext::new(
                    thread_data_copy,
                ))))
            };
            Rav1dContextTaskThread { task, thread_data }
        })
        .collect();

    let c = Rav1dContext {
        allocator: s.allocator.clone(),
        logger: s.logger.clone(),
        apply_grain: s.apply_grain,
        operating_point: s.operating_point.get(),
        all_layers: s.all_layers,
        frame_size_limit,
        strict_std_compliance: s.strict_std_compliance,
        output_invisible_frames: s.output_invisible_frames,
        inloop_filters: s.inloop_filters,
        decode_frame_type: s.decode_frame_type,
        fc,
        task_thread,
        state,
        tc,
        ..Default::default()
    };

    let mut c = Arc::new(c);

    if c.allocator.is_default() {
        let c = Arc::get_mut(&mut c).unwrap();

        c.allocator.cookie = Some(SendSyncNonNull::from_ref(&c.picture_pool).cast::<c_void>());
    }
    let c = c;

    for tc in c.tc.iter() {
        if let Rav1dContextTaskType::Worker(handle) = &tc.task {
            *tc.thread_data.c.lock() = Some(Arc::clone(&c));
            handle.thread().unpark();
        }
    }

    Ok(c)
}

impl Rav1dFilmGrainData {
    fn has_grain(&self) -> bool {
        self.num_y_points != 0
            || self.num_uv_points[0] != 0
            || self.num_uv_points[1] != 0
            || self.clip_to_restricted_range && self.chroma_scaling_from_luma
    }
}

impl Rav1dPicture {
    fn has_grain(&self) -> bool {
        self.frame_hdr.as_ref().unwrap().film_grain.data.has_grain()
    }
}

fn output_image(c: &Rav1dContext, state: &mut Rav1dState, out: &mut Rav1dPicture) -> Rav1dResult {
    let mut res = Ok(());

    let use_cache = !c.all_layers && state.max_spatial_id != 0;
    let r#in = if !use_cache {
        &mut state.out
    } else {
        &mut state.cache
    };
    if !c.apply_grain || !r#in.p.has_grain() {
        *out = mem::take(&mut r#in.p);
    } else {
        res = rav1d_apply_grain(c, out, &r#in.p);
    }
    let _ = mem::take(r#in);

    if use_cache && state.out.p.data.is_some() {
        state.cache = mem::take(&mut state.out);
    }
    res
}

fn output_picture_ready(c: &Rav1dContext, state: &mut Rav1dState, drain: bool) -> bool {
    if state.cached_error.is_some() {
        return true;
    }
    if !c.all_layers && state.max_spatial_id != 0 {
        if state.out.p.data.is_some() && state.cache.p.data.is_some() {
            if state.max_spatial_id == state.cache.p.frame_hdr.as_ref().unwrap().spatial_id
                || state.out.flags.contains(PictureFlags::NEW_TEMPORAL_UNIT)
            {
                return true;
            }
            state.cache = mem::take(&mut state.out);
            return false;
        } else {
            if state.cache.p.data.is_some() && drain {
                return true;
            } else {
                if state.out.p.data.is_some() {
                    state.cache = mem::take(&mut state.out);
                    return false;
                }
            }
        }
    }
    state.out.p.data.is_some()
}

fn drain_picture(c: &Rav1dContext, state: &mut Rav1dState, out: &mut Rav1dPicture) -> Rav1dResult {
    let mut drained = false;
    for _ in 0..c.fc.len() {
        let next = state.frame_thread.next;
        let fc = &c.fc[next as usize];
        let mut task_thread_lock = c.task_thread.lock.lock();
        while !fc.task_thread.finished.load(Ordering::SeqCst) {
            fc.task_thread.cond.wait(&mut task_thread_lock);
        }
        let out_delayed = &mut state.frame_thread.out_delayed[next as usize];
        if out_delayed.p.data.is_some() || fc.task_thread.error.load(Ordering::SeqCst) != 0 {
            let first = c.task_thread.first.load(Ordering::SeqCst);
            if first as usize + 1 < c.fc.len() {
                c.task_thread.first.fetch_add(1, Ordering::SeqCst);
            } else {
                c.task_thread.first.store(0, Ordering::SeqCst);
            }
            let _ = c.task_thread.reset_task_cur.compare_exchange(
                first,
                u32::MAX,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            let cur = c.task_thread.cur.get();
            if cur != 0 && (cur as usize) < c.fc.len() {
                c.task_thread.cur.set(cur - 1);
            }
            drained = true;
        } else if drained {
            break;
        }
        state.frame_thread.next = (state.frame_thread.next + 1) % c.fc.len() as u32;
        drop(task_thread_lock);
        mem::take(&mut *fc.task_thread.retval.try_lock().unwrap())
            .err_or(())
            .inspect_err(|_| {
                state.cached_error_props = out_delayed.p.m.clone();
                let _ = mem::take(out_delayed);
            })?;
        if out_delayed.p.data.is_some() {
            let progress = out_delayed.progress.as_ref().unwrap()[1].load(Ordering::Relaxed);
            if (out_delayed.visible || c.output_invisible_frames) && progress != FRAME_ERROR {
                state.out = out_delayed.clone();
                state.event_flags |= out_delayed.flags.into();
            }
            let _ = mem::take(out_delayed);
            if output_picture_ready(c, state, false) {
                return output_image(c, state, out);
            }
        }
    }
    if output_picture_ready(c, state, true) {
        return output_image(c, state, out);
    }
    Err(Rav1dError::TryAgain)
}

fn gen_picture(c: &Rav1dContext, state: &mut Rav1dState) -> Rav1dResult {
    if output_picture_ready(c, state, false) {
        return Ok(());
    }

    let Rav1dData {
        data: r#in,
        m: props,
    } = mem::take(&mut state.in_0);
    let Some(mut r#in) = r#in else { return Ok(()) };
    while !r#in.is_empty() {
        let len = rav1d_parse_obus(c, state, &r#in, &props);
        if let Ok(len) = len {
            r#in.slice_in_place(len..);
        }

        if output_picture_ready(c, state, false) {
            if !r#in.is_empty() {
                state.in_0 = Rav1dData {
                    data: Some(r#in),
                    m: props,
                }
            }
            break;
        }
        len?;
    }
    Ok(())
}

pub(crate) fn rav1d_send_data(c: &Rav1dContext, in_0: &mut Rav1dData) -> Rav1dResult {
    let state = &mut *c.state.try_lock().unwrap();
    if let Some(data) = in_0.data.as_ref() {
        let sz = data.len();
        validate_input!((sz > 0 && sz <= usize::MAX / 2, Rav1dError::InvalidArgument))?;
        state.drain = false;
    }
    if state.in_0.data.is_some() {
        return Err(Rav1dError::TryAgain);
    }
    state.in_0 = in_0.clone();
    let res = gen_picture(c, state);
    if res.is_ok() {
        let _ = mem::take(in_0);
    }
    res
}

pub(crate) fn rav1d_get_picture(c: &Rav1dContext, out: &mut Rav1dPicture) -> Rav1dResult {
    let state = &mut *c.state.try_lock().unwrap();
    let drain = mem::replace(&mut state.drain, true);
    gen_picture(c, state)?;
    mem::take(&mut state.cached_error).err_or(())?;
    if output_picture_ready(c, state, c.fc.len() == 1) {
        return output_image(c, state, out);
    }
    if c.fc.len() > 1 && drain {
        return drain_picture(c, state, out);
    }
    Err(Rav1dError::TryAgain)
}

pub(crate) fn rav1d_apply_grain(
    c: &Rav1dContext,
    out: &mut Rav1dPicture,
    in_0: &Rav1dPicture,
) -> Rav1dResult {
    if !in_0.has_grain() {
        *out = in_0.clone();
        return Ok(());
    }
    let res = rav1d_picture_alloc_copy(&c.logger, out, in_0.p.w, in_0);
    if res.is_err() {
        let _ = mem::take(out);
        return res;
    } else {
        if c.tc.len() > 1 {
            rav1d_task_delayed_fg(c, out, in_0);
        } else {
            let bpc = out.p.bpc;
            let dsp = Rav1dBitDepthDSPContext::get(bpc).unwrap();
            let fg = &dsp.fg;
            use fg_apply::rav1d_apply_grain;
            match bpc {
                8 => rav1d_apply_grain::<BitDepth8>(fg, out, in_0),
                10 | 12 => rav1d_apply_grain::<BitDepth16>(fg, out, in_0),
                _ => {}
            }
        }
        return Ok(());
    };
}

pub(crate) fn rav1d_flush(c: &Rav1dContext) {
    let state = &mut *c.state.try_lock().unwrap();

    let old_state = mem::take(state);
    state.tiles = old_state.tiles;
    state.n_tiles = old_state.n_tiles;
    state.frame_thread = old_state.frame_thread;
    state.operating_point_idc = old_state.operating_point_idc;
    state.max_spatial_id = old_state.max_spatial_id;
    state.frame_flags = old_state.frame_flags;
    state.event_flags = old_state.event_flags;

    if c.fc.len() == 1 && c.tc.len() == 1 {
        return;
    }
    c.flush.store(true, Ordering::SeqCst);
    if c.tc.len() > 1 {
        let mut task_thread_lock = c.task_thread.lock.lock();
        for tc in c.tc.iter() {
            while !tc.flushed() {
                tc.thread_data.cond.wait(&mut task_thread_lock);
            }
        }
        for fc in c.fc.iter() {
            fc.task_thread.tasks.clear();
        }
        c.task_thread.first.store(0, Ordering::SeqCst);
        c.task_thread.cur.set(c.fc.len() as u32);
        c.task_thread
            .reset_task_cur
            .store(u32::MAX, Ordering::SeqCst);
        c.task_thread.cond_signaled.store(0, Ordering::SeqCst);
    }
    if c.fc.len() > 1 {
        for fc in wrapping_iter(c.fc.iter(), state.frame_thread.next as usize) {
            let _ = rav1d_decode_frame_exit(c, fc, Err(Rav1dError::Other));
            *fc.task_thread.retval.try_lock().unwrap() = None;
            let out_delayed = &mut state.frame_thread.out_delayed[fc.index];
            if out_delayed.p.frame_hdr.is_some() {
                let _ = mem::take(out_delayed);
            }
        }
        state.frame_thread.next = 0;
    }
    c.flush.store(false, Ordering::SeqCst);
}

#[cold]
pub(crate) fn rav1d_close(c: &Rav1dContext) {
    rav1d_flush(c);
    c.tell_worker_threads_to_die();
}

impl Rav1dContext {
    fn tell_worker_threads_to_die(&self) {
        if self.tc.is_empty() {
            return;
        }
        let ttd = &*self.task_thread;
        let _task_thread_lock = ttd.lock.lock();
        for tc in self.tc.iter() {
            tc.thread_data.die.set(true);
        }
        ttd.cond.notify_all();
    }
}

#[cfg(test)]
mod tests;
