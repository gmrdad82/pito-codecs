#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::{c_int, c_void};
use std::hint::assert_unchecked;
use std::ptr::NonNull;
use std::sync::Arc;
use std::{array, mem};

use crate::av1::cpu::extensions::To as _;
use crate::av1::cpu::pod::{AsBytes, FromBytes, FromZeroes};

use crate::av1::cpu::c_arc::RawArc;
use crate::av1::cpu::disjoint_mut::{
    AsMutPtr, DisjointImmutGuard, DisjointMut, DisjointMutGuard, SliceBounds,
};
use crate::av1::cpu::error::{Dav1dResult, Rav1dError, Rav1dResult};
use crate::av1::cpu::ffi_safe::FFISafe;
use crate::av1::cpu::include::common::bitdepth::BitDepth;
use crate::av1::cpu::include::common::validate::validate_input;
use crate::av1::cpu::include::dav1d::common::{Dav1dDataProps, Rav1dDataProps};
use crate::av1::cpu::include::dav1d::headers::{
    DRav1d, Dav1dFrameHeader, Dav1dITUTT35, Dav1dPixelLayout, Dav1dSequenceHeader,
    Rav1dContentLightLevel, Rav1dFrameHeader, Rav1dITUTT35, Rav1dMasteringDisplay,
    Rav1dPixelLayout, Rav1dSequenceHeader,
};
use crate::av1::cpu::pixels::Pixels;
use crate::av1::cpu::send_sync_non_null::SendSyncNonNull;
use crate::av1::cpu::strided::Strided;
use crate::av1::cpu::with_offset::WithOffset;

pub const RAV1D_PICTURE_ALIGNMENT: usize = 64;
pub const DAV1D_PICTURE_ALIGNMENT: usize = RAV1D_PICTURE_ALIGNMENT;

#[derive(Default)]
#[repr(C)]
pub struct Dav1dPictureParameters {
    pub w: c_int,
    pub h: c_int,
    pub layout: Dav1dPixelLayout,
    pub bpc: c_int,
}

#[derive(Clone, Default)]
#[repr(C)]
pub struct Rav1dPictureParameters {
    pub w: c_int,
    pub h: c_int,
    pub layout: Rav1dPixelLayout,
    pub bpc: u8,
}

impl From<Dav1dPictureParameters> for Rav1dPictureParameters {
    fn from(value: Dav1dPictureParameters) -> Self {
        let Dav1dPictureParameters { w, h, layout, bpc } = value;
        Self {
            w,
            h,
            layout: layout.try_into().unwrap(),
            bpc: bpc.try_into().unwrap(),
        }
    }
}

impl From<Rav1dPictureParameters> for Dav1dPictureParameters {
    fn from(value: Rav1dPictureParameters) -> Self {
        let Rav1dPictureParameters { w, h, layout, bpc } = value;
        Self {
            w,
            h,
            layout: layout.into(),
            bpc: bpc.into(),
        }
    }
}

#[derive(Default)]
#[repr(C)]
pub struct Dav1dPicture {
    pub seq_hdr: Option<NonNull<Dav1dSequenceHeader>>,
    pub frame_hdr: Option<NonNull<Dav1dFrameHeader>>,
    pub data: [Option<NonNull<c_void>>; 3],
    pub stride: [isize; 2],
    pub p: Dav1dPictureParameters,
    pub m: Dav1dDataProps,
    pub content_light: Option<NonNull<Rav1dContentLightLevel>>,
    pub mastering_display: Option<NonNull<Rav1dMasteringDisplay>>,
    pub itut_t35: Option<NonNull<Dav1dITUTT35>>,
    pub n_itut_t35: usize,
    pub reserved: [usize; 4],
    pub frame_hdr_ref: Option<RawArc<DRav1d<Rav1dFrameHeader, Dav1dFrameHeader>>>,
    pub seq_hdr_ref: Option<RawArc<DRav1d<Rav1dSequenceHeader, Dav1dSequenceHeader>>>,
    pub content_light_ref: Option<RawArc<Rav1dContentLightLevel>>,
    pub mastering_display_ref: Option<RawArc<Rav1dMasteringDisplay>>,
    pub itut_t35_ref: Option<RawArc<DRav1d<Box<[Rav1dITUTT35]>, Box<[Dav1dITUTT35]>>>>,
    pub reserved_ref: [usize; 4],
    pub r#ref: Option<RawArc<Rav1dPictureData>>,
    pub allocator_data: Option<SendSyncNonNull<c_void>>,
}

#[derive(Clone)]
#[repr(C, align(64))]
pub struct AlignedPixelChunk([u8; RAV1D_PICTURE_ALIGNMENT]);

unsafe impl crate::av1::cpu::pod::FromZeroes for AlignedPixelChunk {}

unsafe impl crate::av1::cpu::pod::FromBytes for AlignedPixelChunk {}

unsafe impl crate::av1::cpu::pod::AsBytes for AlignedPixelChunk {}

const _: () = assert!(mem::align_of::<AlignedPixelChunk>() == RAV1D_PICTURE_ALIGNMENT);
const _: () = assert!(mem::size_of::<AlignedPixelChunk>() == RAV1D_PICTURE_ALIGNMENT);

const RAV1D_PICTURE_GUARANTEED_MULTIPLE: usize = 64;

const RAV1D_PICTURE_MULTIPLE: usize = 64 * 64;

pub struct Rav1dPictureDataComponentInner {
    ptr: NonNull<u8>,

    len: usize,

    stride: isize,
}

impl Rav1dPictureDataComponentInner {
    unsafe fn new(ptr: Option<NonNull<u8>>, len: usize, stride: isize) -> Self {
        let ptr = match ptr {
            None => {
                return Self {
                    ptr: NonNull::<AlignedPixelChunk>::dangling().cast(),
                    len: 0,
                    stride,
                };
            }
            Some(ptr) => ptr,
        };

        assert!(len != 0);
        assert!(ptr.cast::<AlignedPixelChunk>().is_aligned());

        let ptr = if stride < 0 {
            let ptr = ptr.as_ptr();

            let ptr = unsafe { ptr.offset(-stride).sub(len) };
            NonNull::new(ptr).unwrap()
        } else {
            ptr
        };

        assert!(len % RAV1D_PICTURE_MULTIPLE == 0);
        Self { ptr, len, stride }
    }

    pub fn wrap_buf<BD: BitDepth>(buf: &mut [BD::Pixel], stride: usize) -> Self {
        let buf = AsBytes::as_bytes_mut(buf);
        let ptr = NonNull::new(buf.as_mut_ptr()).unwrap();
        assert!(ptr.cast::<AlignedPixelChunk>().is_aligned());
        let len = buf.len();
        assert!(len % RAV1D_PICTURE_GUARANTEED_MULTIPLE == 0);
        let stride = (stride * mem::size_of::<BD::Pixel>()) as isize;
        Self { ptr, len, stride }
    }
}

unsafe impl AsMutPtr for Rav1dPictureDataComponentInner {
    type Target = u8;

    #[inline]
    unsafe fn as_mut_ptr(ptr: *mut Self) -> *mut Self::Target {
        let this = unsafe { &*ptr };

        unsafe { assert_unchecked(this.ptr.cast::<AlignedPixelChunk>().is_aligned()) };

        this.ptr.as_ptr()
    }

    #[inline]
    fn len(&self) -> usize {
        unsafe { assert_unchecked(self.len % RAV1D_PICTURE_GUARANTEED_MULTIPLE == 0) };
        self.len
    }
}

pub struct Rav1dPictureDataComponent(DisjointMut<Rav1dPictureDataComponentInner>);

impl Pixels for Rav1dPictureDataComponent {
    fn byte_len(&self) -> usize {
        self.0.len()
    }

    fn as_byte_mut_ptr(&self) -> *mut u8 {
        self.0.as_mut_ptr()
    }
}

impl Strided for Rav1dPictureDataComponent {
    fn stride(&self) -> isize {
        unsafe { (*self.0.inner()).stride }
    }
}

impl Rav1dPictureDataComponent {
    pub fn wrap_buf<BD: BitDepth>(buf: &mut [BD::Pixel], stride: usize) -> Self {
        Self(DisjointMut::new(
            Rav1dPictureDataComponentInner::wrap_buf::<BD>(buf, stride),
        ))
    }

    pub fn pixel_offset<BD: BitDepth>(&self) -> usize {
        let stride = self.stride();
        if stride >= 0 {
            return 0;
        }
        BD::pxstride(self.byte_len() - (-stride) as usize)
    }

    pub fn with_offset<BD: BitDepth>(&self) -> Rav1dPictureDataComponentOffset<'_> {
        Rav1dPictureDataComponentOffset {
            data: self,
            offset: self.pixel_offset::<BD>(),
        }
    }

    fn as_strided_byte_mut_ptr(&self) -> *mut u8 {
        let ptr = self.0.as_mut_ptr();
        let stride = self.stride();
        if stride < 0 {
            let ptr = unsafe { ptr.add(self.byte_len()) };

            let ptr = unsafe { ptr.offset(stride) };
            ptr
        } else {
            ptr
        }
    }

    pub fn as_strided_mut_ptr<BD: BitDepth>(&self) -> *mut BD::Pixel {
        self.as_strided_byte_mut_ptr().cast()
    }

    pub fn as_strided_ptr<BD: BitDepth>(&self) -> *const BD::Pixel {
        self.as_strided_mut_ptr::<BD>().cast_const()
    }

    fn as_dav1d(&self) -> Option<NonNull<c_void>> {
        if self.byte_len() == 0 {
            None
        } else {
            NonNull::new(self.as_strided_byte_mut_ptr().cast())
        }
    }

    pub fn copy_from(&self, src: &Self) {
        let dst = &mut *self.0.index_mut(..);
        let src = &*src.0.index(..);
        dst.clone_from_slice(src);
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn index<'a, BD: BitDepth>(
        &'a self,
        index: usize,
    ) -> DisjointImmutGuard<'a, Rav1dPictureDataComponentInner, BD::Pixel> {
        self.0.element_as(index)
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn index_mut<'a, BD: BitDepth>(
        &'a self,
        index: usize,
    ) -> DisjointMutGuard<'a, Rav1dPictureDataComponentInner, BD::Pixel> {
        self.0.mut_element_as(index)
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn slice<'a, BD, I>(
        &'a self,
        index: I,
    ) -> DisjointImmutGuard<'a, Rav1dPictureDataComponentInner, [BD::Pixel]>
    where
        BD: BitDepth,
        I: SliceBounds,
    {
        self.0.slice_as(index)
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn slice_mut<'a, BD, I>(
        &'a self,
        index: I,
    ) -> DisjointMutGuard<'a, Rav1dPictureDataComponentInner, [BD::Pixel]>
    where
        BD: BitDepth,
        I: SliceBounds,
    {
        self.0.mut_slice_as(index)
    }
}

pub type Rav1dPictureDataComponentOffset<'a> = WithOffset<&'a Rav1dPictureDataComponent>;
pub type FFISafeRav1dPictureDataComponentOffset<'a> =
    WithOffset<*const FFISafe<'a, Rav1dPictureDataComponent>>;

impl<'a> Rav1dPictureDataComponentOffset<'a> {
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn index<BD: BitDepth>(
        &self,
    ) -> DisjointImmutGuard<'a, Rav1dPictureDataComponentInner, BD::Pixel> {
        self.data.index::<BD>(self.offset)
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn index_mut<BD: BitDepth>(
        &self,
    ) -> DisjointMutGuard<'a, Rav1dPictureDataComponentInner, BD::Pixel> {
        self.data.index_mut::<BD>(self.offset)
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn slice<BD: BitDepth>(
        &self,
        len: usize,
    ) -> DisjointImmutGuard<'a, Rav1dPictureDataComponentInner, [BD::Pixel]> {
        self.data.slice::<BD, _>((self.offset.., ..len))
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn slice_mut<BD: BitDepth>(
        &self,
        len: usize,
    ) -> DisjointMutGuard<'a, Rav1dPictureDataComponentInner, [BD::Pixel]> {
        self.data.slice_mut::<BD, _>((self.offset.., ..len))
    }
}

pub struct Rav1dPictureData {
    pub data: [Rav1dPictureDataComponent; 3],
    pub(crate) allocator_data: Option<SendSyncNonNull<c_void>>,
    pub(crate) allocator: Rav1dPicAllocator,
}

impl Drop for Rav1dPictureData {
    fn drop(&mut self) {
        let Self {
            data,
            allocator_data,
            allocator,
        } = self;
        allocator.dealloc_picture_data(data, *allocator_data);
    }
}

#[derive(Clone, Default)]
#[repr(C)]
pub struct Rav1dPicture {
    pub seq_hdr: Option<Arc<DRav1d<Rav1dSequenceHeader, Dav1dSequenceHeader>>>,
    pub frame_hdr: Option<Arc<DRav1d<Rav1dFrameHeader, Dav1dFrameHeader>>>,
    pub data: Option<Arc<Rav1dPictureData>>,
    pub stride: [isize; 2],
    pub p: Rav1dPictureParameters,
    pub m: Rav1dDataProps,
    pub content_light: Option<Arc<Rav1dContentLightLevel>>,
    pub mastering_display: Option<Arc<Rav1dMasteringDisplay>>,
    pub itut_t35: Arc<DRav1d<Box<[Rav1dITUTT35]>, Box<[Dav1dITUTT35]>>>,
}

impl From<Dav1dPicture> for Rav1dPicture {
    fn from(value: Dav1dPicture) -> Self {
        let Dav1dPicture {
            seq_hdr: _,
            frame_hdr: _,
            data: _,
            stride,
            p,
            m,
            content_light: _,
            mastering_display: _,
            itut_t35: _,
            n_itut_t35: _,
            reserved: _,
            frame_hdr_ref,
            seq_hdr_ref,
            content_light_ref,
            mastering_display_ref,
            itut_t35_ref,
            reserved_ref: _,
            r#ref: data_ref,
            allocator_data: _,
        } = value;
        Self {
            seq_hdr: seq_hdr_ref.map(|raw| unsafe { raw.into_arc() }),

            frame_hdr: frame_hdr_ref.map(|raw| unsafe { raw.into_arc() }),
            data: data_ref.map(|raw| unsafe { raw.into_arc() }),
            stride,
            p: p.into(),
            m: m.into(),
            content_light: content_light_ref.map(|raw| unsafe { raw.into_arc() }),
            mastering_display: mastering_display_ref.map(|raw| unsafe { raw.into_arc() }),

            itut_t35: itut_t35_ref
                .map(|raw| unsafe { raw.into_arc() })
                .unwrap_or_default(),
        }
    }
}

impl From<Rav1dPicture> for Dav1dPicture {
    fn from(value: Rav1dPicture) -> Self {
        let Rav1dPicture {
            seq_hdr,
            frame_hdr,
            data,
            stride,
            p,
            m,
            content_light,
            mastering_display,
            itut_t35,
        } = value;
        Self {
            seq_hdr: seq_hdr.as_ref().map(|arc| (&arc.as_ref().dav1d).into()),

            frame_hdr: frame_hdr.as_ref().map(|arc| (&arc.as_ref().dav1d).into()),
            data: data
                .as_ref()
                .map(|arc| arc.data.each_ref().map(|data| data.as_dav1d()))
                .unwrap_or_default(),
            stride,
            p: p.into(),
            m: m.into(),
            content_light: content_light.as_ref().map(|arc| arc.as_ref().into()),
            mastering_display: mastering_display.as_ref().map(|arc| arc.as_ref().into()),

            itut_t35: Some(NonNull::new(itut_t35.dav1d.as_ptr().cast_mut()).unwrap()),
            n_itut_t35: itut_t35.len(),
            reserved: Default::default(),
            frame_hdr_ref: frame_hdr.map(RawArc::from_arc),
            seq_hdr_ref: seq_hdr.map(RawArc::from_arc),
            content_light_ref: content_light.map(RawArc::from_arc),
            mastering_display_ref: mastering_display.map(RawArc::from_arc),
            itut_t35_ref: Some(itut_t35).map(RawArc::from_arc),
            reserved_ref: Default::default(),

            allocator_data: data.as_ref().and_then(|arc| arc.allocator_data),
            r#ref: data.map(RawArc::from_arc),
        }
    }
}

impl Rav1dPicture {
    pub fn lf_offsets<BD: BitDepth>(&self, y: c_int) -> [Rav1dPictureDataComponentOffset<'_>; 3] {
        let layout = self.p.layout;
        let has_chroma = layout != Rav1dPixelLayout::I400;
        let data = &self.data.as_ref().unwrap().data;
        array::from_fn(|i| {
            let data = &data[has_chroma as usize * i];
            let ss_ver = layout == Rav1dPixelLayout::I420 && i != 0;
            data.with_offset::<BD>() + (y as isize * data.pixel_stride::<BD>() >> ss_ver as u8)
        })
    }
}

#[derive(Clone)]
#[repr(C)]
pub struct Dav1dPicAllocator {
    pub cookie: Option<SendSyncNonNull<c_void>>,

    pub alloc_picture_callback: Option<
        unsafe extern "C" fn(
            pic: *mut Dav1dPicture,
            cookie: Option<SendSyncNonNull<c_void>>,
        ) -> Dav1dResult,
    >,

    pub release_picture_callback: Option<
        unsafe extern "C" fn(pic: *mut Dav1dPicture, cookie: Option<SendSyncNonNull<c_void>>) -> (),
    >,
}

#[derive(Clone)]
#[repr(C)]
pub struct Rav1dPicAllocator {
    pub cookie: Option<SendSyncNonNull<c_void>>,

    pub alloc_picture_callback: unsafe extern "C" fn(
        pic: *mut Dav1dPicture,
        cookie: Option<SendSyncNonNull<c_void>>,
    ) -> Dav1dResult,

    pub release_picture_callback:
        unsafe extern "C" fn(pic: *mut Dav1dPicture, cookie: Option<SendSyncNonNull<c_void>>) -> (),
}

impl TryFrom<Dav1dPicAllocator> for Rav1dPicAllocator {
    type Error = Rav1dError;

    fn try_from(value: Dav1dPicAllocator) -> Result<Self, Self::Error> {
        let Dav1dPicAllocator {
            cookie,
            alloc_picture_callback,
            release_picture_callback,
        } = value;
        Ok(Self {
            cookie,
            alloc_picture_callback: validate_input!(
                alloc_picture_callback.ok_or(Rav1dError::InvalidArgument)
            )?,
            release_picture_callback: validate_input!(
                release_picture_callback.ok_or(Rav1dError::InvalidArgument)
            )?,
        })
    }
}

impl From<Rav1dPicAllocator> for Dav1dPicAllocator {
    fn from(value: Rav1dPicAllocator) -> Self {
        let Rav1dPicAllocator {
            cookie,
            alloc_picture_callback,
            release_picture_callback,
        } = value;
        Self {
            cookie,
            alloc_picture_callback: Some(alloc_picture_callback),
            release_picture_callback: Some(release_picture_callback),
        }
    }
}

impl Rav1dPicAllocator {
    pub fn alloc_picture_data(
        &self,
        w: c_int,
        h: c_int,
        seq_hdr: Arc<DRav1d<Rav1dSequenceHeader, Dav1dSequenceHeader>>,
        frame_hdr: Option<Arc<DRav1d<Rav1dFrameHeader, Dav1dFrameHeader>>>,
    ) -> Rav1dResult<Rav1dPicture> {
        let pic = Rav1dPicture {
            p: Rav1dPictureParameters {
                w,
                h,
                layout: seq_hdr.layout,
                bpc: 8 + 2 * seq_hdr.hbd,
            },
            seq_hdr: Some(seq_hdr),
            frame_hdr,
            ..Default::default()
        };
        let mut pic_c = pic.to::<Dav1dPicture>();

        let result = unsafe { (self.alloc_picture_callback)(&mut pic_c, self.cookie) };
        result.try_to::<Rav1dResult>().unwrap()?;

        let data = pic_c.data;
        let allocator_data = pic_c.allocator_data;
        let mut pic = pic_c.to::<Rav1dPicture>();
        let len = pic.p.pic_len(pic.stride);

        pic.data = Some(Arc::new(Rav1dPictureData {
            data: array::from_fn(|i| {
                let ptr = data[i].map(|ptr| ptr.cast::<u8>());
                let len = len[(i != 0) as usize];
                let stride = pic.stride[(i != 0) as usize];

                let component = unsafe { Rav1dPictureDataComponentInner::new(ptr, len, stride) };
                Rav1dPictureDataComponent(DisjointMut::new(component))
            }),
            allocator_data,
            allocator: self.clone(),
        }));
        Ok(pic)
    }

    pub fn dealloc_picture_data(
        &self,
        data: &mut [Rav1dPictureDataComponent; 3],
        allocator_data: Option<SendSyncNonNull<c_void>>,
    ) {
        let data = data.each_mut().map(|data| data.as_dav1d());
        let mut pic_c = Dav1dPicture {
            data,
            allocator_data,
            ..Default::default()
        };

        unsafe {
            (self.release_picture_callback)(&mut pic_c, self.cookie);
        }
    }
}
