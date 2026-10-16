use std::num::NonZero;
use std::thread::available_parallelism;

#[derive(Clone, Copy, Default)]
pub struct CpuFlags(u32);

impl CpuFlags {
    pub const fn empty() -> Self {
        Self(0)
    }
}

#[inline(always)]
pub(crate) fn rav1d_get_cpu_flags() -> CpuFlags {
    CpuFlags::empty()
}

#[cold]
pub(crate) fn rav1d_init_cpu() {}

#[cold]
pub(crate) fn rav1d_num_logical_processors() -> NonZero<usize> {
    available_parallelism().unwrap_or(NonZero::<usize>::MIN)
}
