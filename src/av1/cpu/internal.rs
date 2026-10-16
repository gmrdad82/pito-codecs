use std::ffi::{c_int, c_uint};
use std::mem;
use std::ops::{Deref, Range};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;

use crate::av1::cpu::atom::{Atom, Atomic};
use crate::av1::cpu::pod::{AsBytes, FromBytes, FromZeroes};
use crate::av1::cpu::sync::{Condvar, Mutex, RwLock, RwLockReadGuard};

use crate::av1::cpu::align::{Align16, Align64, AlignedVec2, AlignedVec64};
use crate::av1::cpu::cdef::Rav1dCdefDSPContext;
use crate::av1::cpu::cdf::{CdfContext, CdfThreadContext};
use crate::av1::cpu::cpu::{CpuFlags, rav1d_get_cpu_flags};
use crate::av1::cpu::disjoint_mut::{
    DisjointImmutGuard, DisjointMut, DisjointMutArcSlice, DisjointMutGuard,
};
use crate::av1::cpu::env::BlockContext;
use crate::av1::cpu::error::Rav1dError;
use crate::av1::cpu::filmgrain::{GRAIN_HEIGHT, GRAIN_WIDTH, Rav1dFilmGrainDSPContext};
use crate::av1::cpu::include::common::bitdepth::{BPC, BitDepth, BitDepth8, BitDepth16};
use crate::av1::cpu::include::dav1d::common::Rav1dDataProps;
use crate::av1::cpu::include::dav1d::data::Rav1dData;
use crate::av1::cpu::include::dav1d::dav1d::{
    Rav1dDecodeFrameType, Rav1dEventFlags, Rav1dInloopFilterType,
};
use crate::av1::cpu::include::dav1d::headers::{
    DRav1d, Dav1dFrameHeader, Dav1dSequenceHeader, Rav1dContentLightLevel, Rav1dFrameHeader,
    Rav1dITUTT35, Rav1dMasteringDisplay, Rav1dSequenceHeader, Rav1dWarpedMotionParams,
};
use crate::av1::cpu::include::dav1d::picture::{Rav1dPicAllocator, Rav1dPicture};
use crate::av1::cpu::ipred::Rav1dIntraPredDSPContext;
use crate::av1::cpu::itx::Rav1dInvTxfmDSPContext;
use crate::av1::cpu::levels::{Av1Block, Filter2d, SegmentId, TxfmType, WHT_WHT};
use crate::av1::cpu::lf_mask::{Av1Filter, Av1FilterLUT, Av1Restoration, Av1RestorationUnit};
use crate::av1::cpu::log::Rav1dLogger;
use crate::av1::cpu::loopfilter::Rav1dLoopFilterDSPContext;
use crate::av1::cpu::looprestoration::Rav1dLoopRestorationDSPContext;
use crate::av1::cpu::lr_apply::LrRestorePlanes;
use crate::av1::cpu::mc::Rav1dMCDSPContext;
use crate::av1::cpu::msac::{MsacContext, Rav1dMsacDSPContext};
use crate::av1::cpu::pal::Rav1dPalDSPContext;
use crate::av1::cpu::picture::{PictureFlags, Rav1dThreadPicture};
use crate::av1::cpu::pool::MemPool;
use crate::av1::cpu::recon::{
    BackupIpredEdgeFn, CopyPalBlockFn, FilterSbrowFn, ReadCoefBlocksFn, ReadPalPlaneFn,
    ReadPalUVFn, ReconBInterFn, ReconBIntraFn, rav1d_backup_ipred_edge, rav1d_copy_pal_block_uv,
    rav1d_copy_pal_block_y, rav1d_filter_sbrow, rav1d_filter_sbrow_cdef,
    rav1d_filter_sbrow_deblock_cols, rav1d_filter_sbrow_deblock_rows, rav1d_filter_sbrow_lr,
    rav1d_filter_sbrow_resize, rav1d_read_coef_blocks, rav1d_read_pal_plane, rav1d_read_pal_uv,
    rav1d_recon_b_inter, rav1d_recon_b_intra,
};
use crate::av1::cpu::refmvs::{
    Rav1dRefmvsDSPContext, RefMvsFrame, RefMvsTemporalBlock, RefmvsTile,
};
use crate::av1::cpu::relaxed_atomic::RelaxedAtomic;
use crate::av1::cpu::thread_task::{Rav1dTaskIndex, Rav1dTasks};

#[derive(Default)]
pub struct Rav1dDSPContext {
    pub pal: Rav1dPalDSPContext,
    pub refmvs: Rav1dRefmvsDSPContext,
    pub msac: Rav1dMsacDSPContext,
}

impl Rav1dDSPContext {
    pub const fn default() -> Self {
        Self {
            pal: Rav1dPalDSPContext::default(),
            refmvs: Rav1dRefmvsDSPContext::default(),
            msac: Rav1dMsacDSPContext::default(),
        }
    }

    pub const fn new(flags: CpuFlags) -> Self {
        Self {
            pal: Rav1dPalDSPContext::new(flags),
            refmvs: Rav1dRefmvsDSPContext::new(flags),
            msac: Rav1dMsacDSPContext::new(flags),
        }
    }

    pub fn get() -> &'static Self {
        static DSP: OnceLock<Rav1dDSPContext> = OnceLock::new();
        DSP.get_or_init(|| {
            let flags = rav1d_get_cpu_flags();
            Self::new(flags)
        })
    }
}

impl Default for &'static Rav1dDSPContext {
    fn default() -> Self {
        Rav1dDSPContext::get()
    }
}

pub(crate) struct Rav1dBitDepthDSPContext {
    pub fg: Rav1dFilmGrainDSPContext,
    pub ipred: Rav1dIntraPredDSPContext,
    pub mc: Rav1dMCDSPContext,
    pub itx: Rav1dInvTxfmDSPContext,
    pub lf: Rav1dLoopFilterDSPContext,
    pub cdef: Rav1dCdefDSPContext,
    pub lr: Rav1dLoopRestorationDSPContext,
}

impl Rav1dBitDepthDSPContext {
    pub const fn _default<BD: BitDepth>() -> Self {
        Self {
            fg: Rav1dFilmGrainDSPContext::default::<BD>(),
            ipred: Rav1dIntraPredDSPContext::default::<BD>(),
            mc: Rav1dMCDSPContext::default::<BD>(),
            itx: Rav1dInvTxfmDSPContext::default::<BD>(),
            lf: Rav1dLoopFilterDSPContext::default::<BD>(),
            cdef: Rav1dCdefDSPContext::default::<BD>(),
            lr: Rav1dLoopRestorationDSPContext::default::<BD>(),
        }
    }

    pub const fn new<BD: BitDepth>(flags: CpuFlags, bpc: u8) -> Self {
        Self {
            fg: Rav1dFilmGrainDSPContext::new::<BD>(flags),
            ipred: Rav1dIntraPredDSPContext::new::<BD>(flags),
            mc: Rav1dMCDSPContext::new::<BD>(flags),
            itx: Rav1dInvTxfmDSPContext::new::<BD>(flags, bpc),
            lf: Rav1dLoopFilterDSPContext::new::<BD>(flags),
            cdef: Rav1dCdefDSPContext::new::<BD>(flags),
            lr: Rav1dLoopRestorationDSPContext::new::<BD>(flags, bpc),
        }
    }

    pub fn get(bpc: u8) -> Option<&'static Self> {
        static BPC8: OnceLock<Rav1dBitDepthDSPContext> = OnceLock::new();
        static BPC10: OnceLock<Rav1dBitDepthDSPContext> = OnceLock::new();
        static BPC12: OnceLock<Rav1dBitDepthDSPContext> = OnceLock::new();
        Some(match bpc {
            8 => BPC8.get_or_init(|| {
                let flags = rav1d_get_cpu_flags();
                Self::new::<BitDepth8>(flags, bpc)
            }),
            10 => BPC10.get_or_init(|| {
                let flags = rav1d_get_cpu_flags();
                Self::new::<BitDepth16>(flags, bpc)
            }),
            12 => BPC12.get_or_init(|| {
                let flags = rav1d_get_cpu_flags();
                Self::new::<BitDepth16>(flags, bpc)
            }),
            _ => return None,
        })
    }
}

impl Default for &'static Rav1dBitDepthDSPContext {
    fn default() -> Self {
        Rav1dBitDepthDSPContext::get(8).unwrap()
    }
}

#[derive(Clone, Default)]
pub(crate) struct Rav1dTileGroupHeader {
    pub start: c_int,
    pub end: c_int,
}

#[derive(Clone, Default)]
pub(crate) struct Rav1dTileGroup {
    pub data: Rav1dData,
    pub hdr: Rav1dTileGroupHeader,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskType {
    Init = 0,
    InitCdf = 1,
    TileEntropy = 2,
    EntropyProgress = 3,
    TileReconstruction = 4,
    DeblockCols = 5,
    DeblockRows = 6,
    Cdef = 7,
    SuperResolution = 8,
    LoopRestoration = 9,
    ReconstructionProgress = 10,
    FgPrep = 11,
    FgApply = 12,
}

impl Default for TaskType {
    fn default() -> Self {
        Self::Init
    }
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct Rav1dContextFrameThread {
    pub out_delayed: Box<[Rav1dThreadPicture]>,
    pub next: c_uint,
}

pub type GrainLut<Entry> = [[Entry; GRAIN_WIDTH]; GRAIN_HEIGHT + 1];

pub struct GrainBD<BD: BitDepth> {
    pub grain_lut: Align16<[GrainLut<BD::Entry>; 3]>,

    pub scaling: Align64<[BD::Scaling; 3]>,
}

impl<BD: BitDepth> Default for GrainBD<BD> {
    fn default() -> Self {
        Self {
            grain_lut: Default::default(),
            scaling: Default::default(),
        }
    }
}

pub enum Grain {
    Bpc8(GrainBD<BitDepth8>),
    Bpc16(GrainBD<BitDepth16>),
}

impl Default for Grain {
    fn default() -> Self {
        Self::Bpc8(Default::default())
    }
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct TaskThreadDataDelayedFg {
    pub in_0: Rav1dPicture,
    pub out: Rav1dPicture,
    pub type_0: TaskType,
    pub grain: Grain,
}

unsafe impl Send for TaskThreadDataDelayedFg {}

unsafe impl Sync for TaskThreadDataDelayedFg {}

#[derive(Default)]
#[repr(C)]
pub(crate) struct TaskThreadData {
    pub lock: Mutex<()>,
    pub cond: Condvar,
    pub first: AtomicU32,
    pub cur: RelaxedAtomic<u32>,

    pub reset_task_cur: AtomicU32,
    pub cond_signaled: AtomicI32,
    pub delayed_fg_exec: RelaxedAtomic<i32>,
    pub delayed_fg_cond: Condvar,
    pub delayed_fg_progress: [AtomicI32; 2],
    pub delayed_fg: RwLock<TaskThreadDataDelayedFg>,
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct Rav1dContextRefs {
    pub p: Rav1dThreadPicture,
    pub segmap: Option<DisjointMutArcSlice<SegmentId>>,
    pub refmvs: Option<DisjointMutArcSlice<RefMvsTemporalBlock>>,
    pub refpoc: [c_uint; 7],
}

pub(crate) enum Rav1dContextTaskType {
    Worker(JoinHandle<()>),

    Single(Mutex<Box<Rav1dTaskContext>>),
}

pub(crate) struct Rav1dContextTaskThread {
    pub task: Rav1dContextTaskType,

    pub thread_data: Arc<Rav1dTaskContextTaskThread>,
}

impl Rav1dContextTaskThread {
    pub fn flushed(&self) -> bool {
        self.thread_data.flushed.get()
    }
}

#[derive(Default)]
pub struct Rav1dState {
    pub(crate) tiles: Vec<Rav1dTileGroup>,
    pub(crate) n_tiles: c_int,
    pub(crate) seq_hdr: Option<Arc<DRav1d<Rav1dSequenceHeader, Dav1dSequenceHeader>>>,
    pub(crate) frame_hdr: Option<Arc<DRav1d<Rav1dFrameHeader, Dav1dFrameHeader>>>,
    pub(crate) content_light: Option<Arc<Rav1dContentLightLevel>>,
    pub(crate) mastering_display: Option<Arc<Rav1dMasteringDisplay>>,

    pub(crate) itut_t35: Vec<Rav1dITUTT35>,

    pub(crate) in_0: Rav1dData,
    pub(crate) out: Rav1dThreadPicture,
    pub(crate) cache: Rav1dThreadPicture,
    pub(crate) frame_thread: Rav1dContextFrameThread,

    pub(crate) refs: [Rav1dContextRefs; 8],
    pub(crate) cdf: [CdfThreadContext; 8],

    pub(crate) operating_point_idc: c_uint,
    pub(crate) max_spatial_id: u8,
    pub(crate) drain: bool,
    pub(crate) frame_flags: PictureFlags,
    pub(crate) event_flags: Rav1dEventFlags,
    pub(crate) cached_error_props: Rav1dDataProps,
    pub(crate) cached_error: Option<Rav1dError>,
}

#[derive(Default)]
#[repr(C)]
#[repr(align(64))]
pub struct Rav1dContext {
    pub(crate) state: Mutex<Rav1dState>,

    pub(crate) fc: Box<[Rav1dFrameContext]>,

    pub(crate) tc: Box<[Rav1dContextTaskThread]>,

    pub(crate) flush: AtomicBool,

    pub(crate) task_thread: Arc<TaskThreadData>,

    pub dsp: &'static Rav1dDSPContext,

    pub(crate) allocator: Rav1dPicAllocator,
    pub(crate) apply_grain: bool,
    pub(crate) operating_point: u8,
    pub(crate) all_layers: bool,
    pub(crate) frame_size_limit: c_uint,
    pub(crate) strict_std_compliance: bool,
    pub(crate) output_invisible_frames: bool,
    pub(crate) inloop_filters: Rav1dInloopFilterType,
    pub(crate) decode_frame_type: Rav1dDecodeFrameType,

    pub(crate) logger: Option<Rav1dLogger>,

    pub(crate) picture_pool: Arc<MemPool<u8>>,
}

unsafe impl Send for Rav1dContext {}

unsafe impl Sync for Rav1dContext {}

#[derive(Default)]
#[repr(C)]
pub struct Rav1dTask {
    pub frame_idx: c_uint,
    pub tile_idx: c_uint,

    pub type_0: TaskType,

    pub sby: c_int,

    pub recon_progress: c_int,
    pub deblock_progress: c_int,
    pub deps_skip: RelaxedAtomic<i32>,

    pub next: Atomic<Rav1dTaskIndex>,
}

impl Rav1dTask {
    pub fn init(frame_idx: c_uint) -> Self {
        Self {
            type_0: TaskType::Init,
            frame_idx,
            sby: 0,
            deblock_progress: 0,
            recon_progress: 0,
            ..Default::default()
        }
    }

    pub fn next(&self) -> Rav1dTaskIndex {
        self.next.load(Ordering::SeqCst)
    }

    pub fn set_next(&self, next: Rav1dTaskIndex) {
        self.next.store(next, Ordering::SeqCst)
    }
}

impl Rav1dTask {
    pub fn without_next(&self) -> Self {
        Self {
            frame_idx: self.frame_idx,
            tile_idx: self.tile_idx,
            type_0: self.type_0,
            sby: self.sby,
            recon_progress: self.recon_progress,
            deblock_progress: self.deblock_progress,
            deps_skip: self.deps_skip.clone(),
            next: Default::default(),
        }
    }
}

#[derive(Default)]
#[repr(C)]
pub struct ScalableMotionParams {
    pub scale: c_int,
    pub step: c_int,
}

pub(crate) struct Rav1dFrameContextBdFn {
    pub recon_b_intra: ReconBIntraFn,
    pub recon_b_inter: ReconBInterFn,
    pub filter_sbrow: FilterSbrowFn,
    pub filter_sbrow_deblock_cols: FilterSbrowFn,
    pub filter_sbrow_deblock_rows: FilterSbrowFn,
    pub filter_sbrow_cdef: fn(&Rav1dContext, &Rav1dFrameData, &mut Rav1dTaskContext, c_int) -> (),
    pub filter_sbrow_resize: FilterSbrowFn,
    pub filter_sbrow_lr: FilterSbrowFn,
    pub backup_ipred_edge: BackupIpredEdgeFn,
    pub read_coef_blocks: ReadCoefBlocksFn,
    pub copy_pal_block_y: CopyPalBlockFn,
    pub copy_pal_block_uv: CopyPalBlockFn,
    pub read_pal_plane: ReadPalPlaneFn,
    pub read_pal_uv: ReadPalUVFn,
}

impl Rav1dFrameContextBdFn {
    pub const fn new<BD: BitDepth>() -> Self {
        Self {
            recon_b_inter: rav1d_recon_b_inter::<BD>,
            recon_b_intra: rav1d_recon_b_intra::<BD>,
            filter_sbrow: rav1d_filter_sbrow::<BD>,
            filter_sbrow_deblock_cols: rav1d_filter_sbrow_deblock_cols::<BD>,
            filter_sbrow_deblock_rows: rav1d_filter_sbrow_deblock_rows::<BD>,
            filter_sbrow_cdef: rav1d_filter_sbrow_cdef::<BD>,
            filter_sbrow_resize: rav1d_filter_sbrow_resize::<BD>,
            filter_sbrow_lr: rav1d_filter_sbrow_lr::<BD>,
            backup_ipred_edge: rav1d_backup_ipred_edge::<BD>,
            read_coef_blocks: rav1d_read_coef_blocks::<BD>,
            copy_pal_block_y: rav1d_copy_pal_block_y::<BD>,
            copy_pal_block_uv: rav1d_copy_pal_block_uv::<BD>,
            read_pal_plane: rav1d_read_pal_plane::<BD>,
            read_pal_uv: rav1d_read_pal_uv::<BD>,
        }
    }

    pub const fn get(bpc: BPC) -> &'static Self {
        const BPC8: Rav1dFrameContextBdFn = Rav1dFrameContextBdFn::new::<BitDepth8>();
        const BPC16: Rav1dFrameContextBdFn = Rav1dFrameContextBdFn::new::<BitDepth16>();
        match bpc {
            BPC::BPC8 => &BPC8,
            BPC::BPC16 => &BPC16,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct CodedBlockInfo(i16);

impl Atom for CodedBlockInfo {
    type Repr = i16;

    fn pack(self) -> i16 {
        self.0
    }

    fn unpack(src: i16) -> Self {
        Self(src)
    }
}

impl CodedBlockInfo {
    const TXTP_BITS: u8 = (TxfmType::BITS - WHT_WHT.leading_zeros()) as u8;

    pub const fn eob(&self) -> i16 {
        self.0 >> Self::TXTP_BITS
    }

    pub const fn txtp(&self) -> TxfmType {
        (self.0 & ((1 << Self::TXTP_BITS) - 1)) as TxfmType
    }

    pub const fn new(eob: i16, txtp: TxfmType) -> Self {
        debug_assert!(eob << Self::TXTP_BITS >> Self::TXTP_BITS == eob);
        Self((eob << Self::TXTP_BITS) | (txtp as i16))
    }
}

#[derive(Default)]
#[repr(C)]
pub struct Pal {
    data: DisjointMut<AlignedVec64<u8>>,
}

type PalArray<BD> = [[<BD as BitDepth>::Pixel; 8]; 3];

impl Pal {
    pub fn resize(&mut self, n: usize) {
        self.data.resize(n * 8 * 3, Default::default());
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn index<'a: 'b, 'b, BD: BitDepth>(
        &'a self,
        index: usize,
    ) -> DisjointImmutGuard<'b, AlignedVec64<u8>, PalArray<BD>> {
        self.data.element_as(index)
    }

    pub fn index_mut<'a: 'b, 'b, BD: BitDepth>(
        &'a self,
        index: usize,
    ) -> DisjointMutGuard<'b, AlignedVec64<u8>, PalArray<BD>> {
        self.data.mut_element_as(index)
    }
}

#[derive(Default)]
#[repr(C)]
pub struct Rav1dFrameContextFrameThread {
    pub next_tile_row: [RelaxedAtomic<i32>; 2],

    pub b: DisjointMut<Vec<Av1Block>>,

    pub cbi: Vec<RelaxedAtomic<CodedBlockInfo>>,

    pub pal: Pal,

    pub pal_idx: DisjointMut<AlignedVec64<u8>>,

    pub cf: DisjointMut<AlignedVec64<u8>>,

    pub tile_start_off: Vec<u32>,
}

#[derive(Default)]
pub(crate) struct TxLpfRightEdge {
    inner: DisjointMut<Vec<u8>>,
}

impl TxLpfRightEdge {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            inner: DisjointMut::default(),
        }
    }

    pub fn resize(&mut self, right_edge_size: usize, value: u8) {
        self.inner.resize(right_edge_size * 32 * 2, value)
    }

    pub fn get<'a>(
        &'a self,
        index_y: Range<usize>,
        index_uv: Range<usize>,
    ) -> (
        impl 'a + Deref<Target = [u8]>,
        impl 'a + Deref<Target = [u8]>,
    ) {
        let mid = self.inner.len() / 2;
        assert!(index_y.end <= mid);
        let (uv_start, uv_end) = (index_uv.start + mid, index_uv.end + mid);
        (
            self.inner.index(index_y),
            self.inner.index(uv_start..uv_end),
        )
    }

    pub fn copy_from_slice_y(&self, index: Range<usize>, src: &[u8]) {
        #[allow(unused_mut)]
        let mut slice_mut = self.inner.index_mut(index);
        slice_mut.copy_from_slice(src);
    }

    pub fn copy_from_slice_uv(&self, index: Range<usize>, src: &[u8]) {
        let mid = self.inner.len() / 2;
        #[allow(unused_mut)]
        let mut slice_mut = self.inner.index_mut(index.start + mid..index.end + mid);
        slice_mut.copy_from_slice(src);
    }
}

#[derive(Default)]
#[repr(C)]
pub struct Rav1dFrameContextLf {
    pub level: DisjointMut<AlignedVec2<u8>>,
    pub mask: Vec<Av1Filter>,
    pub lr_mask: Vec<Av1Restoration>,
    pub lim_lut: Align16<Av1FilterLUT>,
    pub lvl: [Align16<[[[u8; 2]; 8]; 4]>; 8],
    pub last_sharpness: u8,
    pub tx_lpf_right_edge: TxLpfRightEdge,

    pub cdef_line_buf: DisjointMut<AlignedVec64<u8>>,
    pub lr_line_buf: DisjointMut<AlignedVec64<u8>>,
    pub cdef_line: [[usize; 3]; 2],
    pub cdef_lpf_line: [usize; 3],
    pub lr_lpf_line: [usize; 3],

    pub start_of_tile_row: Vec<u8>,
    pub restore_planes: LrRestorePlanes,
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct Rav1dFrameContextTaskThread {
    pub lock: Mutex<()>,
    pub cond: Condvar,
    pub ttd: Arc<TaskThreadData>,
    pub tasks: Rav1dTasks,
    pub init_done: AtomicI32,
    pub done: [AtomicI32; 2],
    pub retval: Mutex<Option<Rav1dError>>,
    pub finished: AtomicBool,
    pub update_set: RelaxedAtomic<bool>,
    pub error: AtomicI32,
    pub task_counter: AtomicI32,
}

#[derive(Default)]
pub(crate) struct Rav1dFrameContextFrameThreadProgress {
    pub entropy: AtomicI32,
    pub deblock: AtomicI32,
    pub frame: RwLock<Vec<AtomicU32>>,
    pub copy_lpf: RwLock<Vec<AtomicU32>>,
}

pub(crate) struct Rav1dFrameContext {
    pub index: usize,

    pub data: RwLock<Rav1dFrameData>,
    pub in_cdf: RwLock<CdfThreadContext>,
    pub task_thread: Rav1dFrameContextTaskThread,
    pub frame_thread_progress: Rav1dFrameContextFrameThreadProgress,
}

impl Rav1dFrameContext {
    pub fn default(index: usize) -> Self {
        Self {
            index,
            data: Default::default(),
            in_cdf: Default::default(),
            task_thread: Default::default(),
            frame_thread_progress: Default::default(),
        }
    }

    pub fn in_cdf<'a>(&'a self) -> RwLockReadGuard<'a, CdfThreadContext> {
        self.in_cdf.try_read().unwrap()
    }

    pub fn frame_hdr(&self) -> Arc<DRav1d<Rav1dFrameHeader, Dav1dFrameHeader>> {
        self.data
            .try_read()
            .unwrap()
            .frame_hdr
            .as_ref()
            .unwrap()
            .clone()
    }
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct Rav1dFrameData {
    pub seq_hdr: Option<Arc<DRav1d<Rav1dSequenceHeader, Dav1dSequenceHeader>>>,
    pub frame_hdr: Option<Arc<DRav1d<Rav1dFrameHeader, Dav1dFrameHeader>>>,
    pub refp: [Rav1dThreadPicture; 7],

    pub cur: Rav1dPicture,

    pub sr_cur: Rav1dThreadPicture,
    pub mvs: Option<DisjointMutArcSlice<RefMvsTemporalBlock>>,
    pub ref_mvs: [Option<DisjointMutArcSlice<RefMvsTemporalBlock>>; 7],
    pub cur_segmap: Option<DisjointMutArcSlice<SegmentId>>,
    pub prev_segmap: Option<DisjointMutArcSlice<SegmentId>>,
    pub refpoc: [c_uint; 7],
    pub refrefpoc: [[c_uint; 7]; 7],
    pub gmv_warp_allowed: [u8; 7],
    pub out_cdf: CdfThreadContext,
    pub tiles: Vec<Rav1dTileGroup>,

    pub svc: [[ScalableMotionParams; 2]; 7],
    pub resize_step: [c_int; 2],
    pub resize_start: [c_int; 2],

    pub ts: Vec<Rav1dTileState>,
    pub dsp: &'static Rav1dBitDepthDSPContext,

    pub ipred_edge: DisjointMut<AlignedVec64<u8>>,
    pub ipred_edge_off: usize,

    pub b4_stride: isize,
    pub w4: c_int,
    pub h4: c_int,
    pub bw: c_int,
    pub bh: c_int,
    pub sb128w: c_int,
    pub sb128h: c_int,
    pub sbh: c_int,
    pub sb_shift: c_int,
    pub sb_step: c_int,
    pub sr_sb128w: c_int,
    pub dq: [[[RelaxedAtomic<u16>; 2]; 3]; SegmentId::COUNT],
    pub qm: [[Option<&'static [u8]>; 3]; 19],
    pub a: Vec<BlockContext>,
    pub rf: RefMvsFrame,
    pub jnt_weights: [[u8; 7]; 7],
    pub bitdepth_max: c_int,

    pub frame_thread: Rav1dFrameContextFrameThread,
    pub lf: Rav1dFrameContextLf,
    pub lowest_pixel_mem: DisjointMut<Vec<[[c_int; 2]; 7]>>,
}

impl Rav1dFrameData {
    pub fn bd_fn(&self) -> &'static Rav1dFrameContextBdFn {
        let bpc = BPC::from_bitdepth_max(self.bitdepth_max);
        Rav1dFrameContextBdFn::get(bpc)
    }

    pub fn frame_hdr(&self) -> &Rav1dFrameHeader {
        self.frame_hdr.as_ref().unwrap()
    }

    pub fn seq_hdr(&self) -> &Rav1dSequenceHeader {
        self.seq_hdr.as_ref().unwrap()
    }
}

#[derive(Default)]
#[repr(C)]
pub struct Rav1dTileStateTiling {
    pub col_start: i32,
    pub col_end: i32,
    pub row_start: i32,
    pub row_end: i32,

    pub col: i32,
    pub row: i32,
}

#[derive(Default)]
#[repr(C)]
pub struct Rav1dTileStateFrameThread {
    pub pal_idx: RelaxedAtomic<u32>,

    pub cbi_idx: RelaxedAtomic<u32>,

    pub cf: RelaxedAtomic<u32>,
}

#[derive(Default)]
#[repr(C, align(32))]
pub struct Rav1dTileStateContext {
    pub cdf: CdfContext,
    pub msac: MsacContext,
}

#[derive(Default)]
#[repr(C, align(32))]
pub struct Rav1dTileState {
    pub context: Mutex<Rav1dTileStateContext>,

    pub tiling: Rav1dTileStateTiling,

    pub progress: [AtomicI32; 2],
    pub frame_thread: [Rav1dTileStateFrameThread; 2],

    pub lowest_pixel: usize,

    pub dqmem: [[[RelaxedAtomic<u16>; 2]; 3]; SegmentId::COUNT],
    pub dq: RelaxedAtomic<TileStateRef>,
    pub last_qidx: RelaxedAtomic<u8>,
    pub last_delta_lf: RelaxedAtomic<[i8; 4]>,
    pub lflvlmem: RwLock<[Align16<[[[u8; 2]; 8]; 4]>; 8]>,
    pub lflvl: RelaxedAtomic<TileStateRef>,

    pub lr_ref: RwLock<[Av1RestorationUnit; 3]>,
}

#[derive(Clone, Copy, Default)]
#[repr(u8)]
pub enum TileStateRef {
    #[default]
    Frame,
    Local,
}

impl TileStateRef {
    pub const fn from_repr(v: u8) -> Option<Self> {
        if v == Self::Frame as u8 {
            return Some(Self::Frame);
        }
        if v == Self::Local as u8 {
            return Some(Self::Local);
        }
        None
    }
}

impl Atom for TileStateRef {
    type Repr = u8;

    fn pack(self) -> Self::Repr {
        self as u8
    }

    fn unpack(src: Self::Repr) -> Self {
        Self::from_repr(src).unwrap_or_default()
    }
}

pub const CF_LEN: usize = 32 * 32;

#[repr(align(64))]
pub struct Cf([i32; CF_LEN]);

unsafe impl crate::av1::cpu::pod::FromZeroes for Cf {}

impl Cf {
    #[allow(unused)]
    pub fn select<BD: BitDepth>(&self) -> &[BD::Coef; CF_LEN] {
        FromBytes::ref_from_prefix(AsBytes::as_bytes(&self.0)).unwrap()
    }

    pub fn select_mut<BD: BitDepth>(&mut self) -> &mut [BD::Coef; CF_LEN] {
        FromBytes::mut_from_prefix(AsBytes::as_bytes_mut(&mut self.0)).unwrap()
    }
}

pub struct AlPal([u16; 8 * 3 * 32 * 2]);

unsafe impl crate::av1::cpu::pod::FromZeroes for AlPal {}

impl AlPal {
    pub fn select_mut<BD: BitDepth>(&mut self) -> &mut [[[[BD::Pixel; 8]; 3]; 32]; 2] {
        FromBytes::mut_from_prefix(AsBytes::as_bytes_mut(&mut self.0)).unwrap()
    }
}

pub const COMPINTER_LEN: usize = 128 * 128;
pub const SEG_MASK_LEN: usize = 128 * 128;

#[repr(C, align(64))]
pub struct ScratchCompinter {
    pub compinter: [[i16; COMPINTER_LEN]; 2],
    pub seg_mask: [u8; SEG_MASK_LEN],
}

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchCompinter {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchCompinter {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchCompinter {}

const SCRATCH_COMPINTER_SIZE: usize = mem::size_of::<ScratchCompinter>();
pub const SCRATCH_LAP_LEN: usize = 128 * 32;

#[repr(C, align(64))]
pub struct ScratchLapInter([u8; SCRATCH_COMPINTER_SIZE]);

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchLapInter {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchLapInter {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchLapInter {}

impl ScratchLapInter {
    pub fn lap_mut<BD: BitDepth>(&mut self) -> &mut [BD::Pixel; SCRATCH_LAP_LEN] {
        FromBytes::mut_from_prefix(&mut self.0).unwrap()
    }

    pub fn inter_mut(&mut self) -> &mut ScratchCompinter {
        FromBytes::mut_from_prefix(&mut self.0).unwrap()
    }
}

pub const EMU_EDGE_LEN: usize = 320 * (256 + 7);

#[repr(C, align(64))]
pub struct ScratchEmuEdge([u8; EMU_EDGE_LEN * 2]);

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchEmuEdge {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchEmuEdge {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchEmuEdge {}

impl ScratchEmuEdge {
    pub fn buf_mut<BD: BitDepth>(&mut self) -> &mut [BD::Pixel; EMU_EDGE_LEN] {
        FromBytes::mut_from_prefix(&mut self.0).unwrap()
    }
}

#[repr(C)]
pub struct ScratchInter {
    pub lap_inter: ScratchLapInter,
    pub emu_edge: ScratchEmuEdge,
}

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchInter {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchInter {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchInter {}

#[repr(C, align(64))]
pub struct ScratchPal {
    pub pal_order: [[u8; 8]; 64],
    pub pal_ctx: [u8; 64],
}

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchPal {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchPal {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchPal {}

#[repr(C, align(64))]
pub struct ScratchLevelsPal([u8; 32 * 34]);

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchLevelsPal {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchLevelsPal {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchLevelsPal {}

impl ScratchLevelsPal {
    pub fn levels_mut(&mut self) -> &mut [u8; 32 * 34] {
        &mut self.0
    }

    pub fn pal_mut(&mut self) -> &mut ScratchPal {
        FromBytes::mut_from_prefix(&mut self.0).unwrap()
    }
}

pub const SCRATCH_INTER_INTRA_BUF_LEN: usize = 64 * 64;

#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct ScratchInterIntraBuf([u16; SCRATCH_INTER_INTRA_BUF_LEN * 2]);

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchInterIntraBuf {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchInterIntraBuf {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchInterIntraBuf {}

impl ScratchInterIntraBuf {
    pub fn buf_mut<BD: BitDepth>(&mut self) -> &mut [BD::Pixel; SCRATCH_INTER_INTRA_BUF_LEN] {
        FromBytes::mut_from_prefix(AsBytes::as_bytes_mut(&mut self.0)).unwrap()
    }
}

pub const SCRATCH_EDGE_LEN: usize = 257;

#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct ScratchEdgeBuf([u8; SCRATCH_EDGE_LEN * 2 + 62]);

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchEdgeBuf {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchEdgeBuf {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchEdgeBuf {}

impl ScratchEdgeBuf {
    pub fn buf_mut<BD: BitDepth>(&mut self) -> &mut [BD::Pixel; SCRATCH_EDGE_LEN] {
        FromBytes::mut_from_prefix(&mut self.0).unwrap()
    }
}

#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub struct ScratchPalBuf([u8; 8 * 3 * 2]);

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchPalBuf {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchPalBuf {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchPalBuf {}

impl ScratchPalBuf {
    pub fn buf<BD: BitDepth>(&self) -> &[[BD::Pixel; 8]; 3] {
        FromBytes::ref_from_prefix(&self.0).unwrap()
    }

    pub fn buf_mut<BD: BitDepth>(&mut self) -> &mut [[BD::Pixel; 8]; 3] {
        FromBytes::mut_from_prefix(&mut self.0).unwrap()
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct ScratchInterIntraEdgePal {
    pub interintra: ScratchInterIntraBuf,
    pub edge: ScratchEdgeBuf,
    pub pal: ScratchPalBuf,

    _padding: [u8; 16],
}

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchInterIntraEdgePal {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchInterIntraEdgePal {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchInterIntraEdgePal {}

pub const SCRATCH_AC_TXTP_LEN: usize = 32 * 32;

#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct ScratchAcTxtpMap([u8; SCRATCH_AC_TXTP_LEN * 2]);

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchAcTxtpMap {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchAcTxtpMap {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchAcTxtpMap {}

impl ScratchAcTxtpMap {
    pub fn ac_mut(&mut self) -> &mut [i16; SCRATCH_AC_TXTP_LEN] {
        FromBytes::mut_from_prefix(&mut self.0).unwrap()
    }

    pub fn txtp_map(&self) -> &[TxfmType; SCRATCH_AC_TXTP_LEN] {
        FromBytes::ref_from_prefix(&self.0).unwrap()
    }

    pub fn txtp_map_mut(&mut self) -> &mut [TxfmType; SCRATCH_AC_TXTP_LEN] {
        FromBytes::mut_from_prefix(&mut self.0).unwrap()
    }
}

#[repr(C)]
pub struct ScratchInterIntra {
    pub levels_pal: ScratchLevelsPal,
    pub ac_txtp_map: ScratchAcTxtpMap,
    pub pal_idx_y: [u8; 32 * 64],
    pub pal_idx_uv: [u8; 64 * 64],
    pub interintra_edge_pal: ScratchInterIntraEdgePal,
}

unsafe impl crate::av1::cpu::pod::FromZeroes for ScratchInterIntra {}

unsafe impl crate::av1::cpu::pod::FromBytes for ScratchInterIntra {}

unsafe impl crate::av1::cpu::pod::AsBytes for ScratchInterIntra {}

const SCRATCH_SIZE: usize = mem::size_of::<ScratchInter>();

#[repr(C)]
pub struct TaskContextScratch([u8; SCRATCH_SIZE]);

unsafe impl crate::av1::cpu::pod::FromZeroes for TaskContextScratch {}

impl TaskContextScratch {
    pub fn inter_mut(&mut self) -> &mut ScratchInter {
        FromBytes::mut_from_prefix(&mut self.0).unwrap()
    }

    pub fn inter_intra(&self) -> &ScratchInterIntra {
        FromBytes::ref_from_prefix(&self.0).unwrap()
    }

    pub fn inter_intra_mut(&mut self) -> &mut ScratchInterIntra {
        FromBytes::mut_from_prefix(&mut self.0).unwrap()
    }
}

#[derive(Default)]
#[repr(C)]
pub struct Rav1dTaskContextFrameThread {
    pub pass: c_int,
}

#[repr(C)]
pub(crate) struct Rav1dTaskContextTaskThread {
    pub cond: Condvar,
    pub ttd: Arc<TaskThreadData>,
    pub flushed: RelaxedAtomic<bool>,
    pub die: RelaxedAtomic<bool>,
    pub c: Mutex<Option<Arc<Rav1dContext>>>,
}

impl Rav1dTaskContextTaskThread {
    pub(crate) fn new(ttd: Arc<TaskThreadData>) -> Self {
        Self {
            cond: Condvar::new(),
            ttd,
            flushed: Default::default(),
            die: Default::default(),
            c: Default::default(),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Bxy {
    pub x: c_int,
    pub y: c_int,
}

#[repr(C)]
pub(crate) struct Rav1dTaskContext {
    pub ts: usize,
    pub b: Bxy,
    pub l: BlockContext,
    pub a: usize,
    pub rt: RefmvsTile,
    pub cf: Cf,
    pub al_pal: AlPal,
    pub pal_sz_uv: [[u8; 32]; 2],
    pub scratch: TaskContextScratch,

    pub warpmv: Rav1dWarpedMotionParams,

    pub lf_mask: Option<usize>,
    pub top_pre_cdef_toggle: c_int,
    pub cur_sb_cdef_idx: usize,

    pub tl_4x4_filter: Filter2d,
    pub frame_thread: Rav1dTaskContextFrameThread,
    pub task_thread: Arc<Rav1dTaskContextTaskThread>,
}

impl Rav1dTaskContext {
    pub(crate) fn new(task_thread: Arc<Rav1dTaskContextTaskThread>) -> Self {
        Self {
            ts: Default::default(),
            b: Default::default(),
            l: Default::default(),
            a: Default::default(),
            rt: Default::default(),
            cf: FromZeroes::new_zeroed(),
            al_pal: FromZeroes::new_zeroed(),
            pal_sz_uv: Default::default(),
            scratch: FromZeroes::new_zeroed(),
            warpmv: Default::default(),
            lf_mask: Default::default(),
            top_pre_cdef_toggle: Default::default(),
            cur_sb_cdef_idx: Default::default(),
            tl_4x4_filter: Default::default(),
            frame_thread: Default::default(),
            task_thread,
        }
    }
}
