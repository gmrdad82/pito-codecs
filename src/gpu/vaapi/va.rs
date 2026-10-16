use std::ffi::{CStr, c_int, c_uint, c_void};
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use libloading::Library;

use super::ffi::{self, VAConfigAttrib, VAImage, VAImageFormat, VASurfaceID};
use super::host::HostBuffer;
use super::{Accel, LAG, SLACK};
use crate::gpu::biplanar::{self, Biplanar, Geometry};
use crate::media::{Frame, Rgba};

struct Api {
    _va: Library,
    _drm: Library,
    initialize: ffi::vaInitialize,
    terminate: ffi::vaTerminate,
    error_str: ffi::vaErrorStr,
    vendor_string: ffi::vaQueryVendorString,
    set_info_callback: ffi::vaSetInfoCallback,
    set_error_callback: ffi::vaSetErrorCallback,
    get_config_attributes: ffi::vaGetConfigAttributes,
    create_config: ffi::vaCreateConfig,
    destroy_config: ffi::vaDestroyConfig,
    create_surfaces: ffi::vaCreateSurfaces,
    destroy_surfaces: ffi::vaDestroySurfaces,
    create_context: ffi::vaCreateContext,
    destroy_context: ffi::vaDestroyContext,
    create_buffer: ffi::vaCreateBuffer,
    destroy_buffer: ffi::vaDestroyBuffer,
    begin_picture: ffi::vaBeginPicture,
    render_picture: ffi::vaRenderPicture,
    end_picture: ffi::vaEndPicture,
    sync_surface: ffi::vaSyncSurface,
    sync_surface2: Option<ffi::vaSyncSurface2>,
    create_image: ffi::vaCreateImage,
    destroy_image: ffi::vaDestroyImage,
    get_image: ffi::vaGetImage,
    derive_image: ffi::vaDeriveImage,
    export_surface: ffi::vaExportSurfaceHandle,
    put_image: ffi::vaPutImage,
    map_buffer: ffi::vaMapBuffer,
    unmap_buffer: ffi::vaUnmapBuffer,
    get_display_drm: ffi::vaGetDisplayDRM,
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn api() -> Result<&'static Api, String> {
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Api, String> {
    let va = unsafe { Library::new("libva.so.2") }.map_err(|e| format!("libva.so.2: {e}"))?;
    let drm =
        unsafe { Library::new("libva-drm.so.2") }.map_err(|e| format!("libva-drm.so.2: {e}"))?;
    macro_rules! sym {
        ($lib:expr, $name:ident) => {{
            let s = unsafe { $lib.get::<ffi::$name>(concat!(stringify!($name), "\0").as_bytes()) };
            *s.map_err(|e| format!("{}: {e}", stringify!($name)))?
        }};
    }
    Ok(Api {
        initialize: sym!(va, vaInitialize),
        terminate: sym!(va, vaTerminate),
        error_str: sym!(va, vaErrorStr),
        vendor_string: sym!(va, vaQueryVendorString),
        set_info_callback: sym!(va, vaSetInfoCallback),
        set_error_callback: sym!(va, vaSetErrorCallback),
        get_config_attributes: sym!(va, vaGetConfigAttributes),
        create_config: sym!(va, vaCreateConfig),
        destroy_config: sym!(va, vaDestroyConfig),
        create_surfaces: sym!(va, vaCreateSurfaces),
        destroy_surfaces: sym!(va, vaDestroySurfaces),
        create_context: sym!(va, vaCreateContext),
        destroy_context: sym!(va, vaDestroyContext),
        create_buffer: sym!(va, vaCreateBuffer),
        destroy_buffer: sym!(va, vaDestroyBuffer),
        begin_picture: sym!(va, vaBeginPicture),
        render_picture: sym!(va, vaRenderPicture),
        end_picture: sym!(va, vaEndPicture),
        sync_surface: sym!(va, vaSyncSurface),
        sync_surface2: unsafe { va.get::<ffi::vaSyncSurface2>(b"vaSyncSurface2\0") }
            .ok()
            .map(|s| *s),
        create_image: sym!(va, vaCreateImage),
        destroy_image: sym!(va, vaDestroyImage),
        get_image: sym!(va, vaGetImage),
        derive_image: sym!(va, vaDeriveImage),
        export_surface: sym!(va, vaExportSurfaceHandle),
        put_image: sym!(va, vaPutImage),
        map_buffer: sym!(va, vaMapBuffer),
        unmap_buffer: sym!(va, vaUnmapBuffer),
        get_display_drm: sym!(drm, vaGetDisplayDRM),
        _va: va,
        _drm: drm,
    })
}

const SYNC_TIMEOUT_NS: u64 = 5_000_000_000;

pub struct Display {
    api: &'static Api,
    dpy: ffi::VADisplay,
    vendor: String,
    device: Option<File>,
    dead: AtomicBool,
    bounded: AtomicBool,
}

unsafe impl Send for Display {}
unsafe impl Sync for Display {}

static SHARED: Mutex<Option<Arc<Display>>> = Mutex::new(None);

fn shared() -> Result<Arc<Display>, String> {
    let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(d) = shared.as_ref()
        && !d.dead()
    {
        return Ok(d.clone());
    }
    let d = Arc::new(Display::open()?);
    *shared = Some(d.clone());
    Ok(d)
}

fn replace_shared(old: &Arc<Display>, new: Arc<Display>) {
    let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    if shared.as_ref().is_none_or(|d| Arc::ptr_eq(d, old)) {
        *shared = Some(new);
    }
}

fn with_display<T>(make: impl Fn(Arc<Display>) -> Result<T, String>) -> Result<T, String> {
    let display = shared()?;
    match make(display.clone()) {
        Ok(s) => Ok(s),
        Err(first) => {
            let fresh =
                Arc::new(Display::open().map_err(|e| format!("{first}; a new display: {e}"))?);
            let s = make(fresh.clone()).map_err(|_| first)?;
            replace_shared(&display, fresh);
            Ok(s)
        }
    }
}

impl Display {
    pub fn open() -> Result<Self, String> {
        let api = api()?;
        let mut why = "no DRM render node (/dev/dri/renderD*)".to_string();
        for n in 128..144 {
            let path = format!("/dev/dri/renderD{n}");
            let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
                continue;
            };
            match Self::init(api, file) {
                Ok(d) => return Ok(d),
                Err(e) => why = format!("{path}: {e}"),
            }
        }
        Err(why)
    }

    fn init(api: &'static Api, device: File) -> Result<Self, String> {
        let dpy = unsafe { (api.get_display_drm)(device.as_raw_fd()) };
        if dpy.is_null() {
            return Err("vaGetDisplayDRM failed".into());
        }
        let mut d = Self {
            api,
            dpy,
            vendor: String::new(),
            device: Some(device),
            dead: AtomicBool::new(false),
            bounded: AtomicBool::new(api.sync_surface2.is_some()),
        };
        unsafe {
            (api.set_info_callback)(dpy, None, std::ptr::null_mut());
            (api.set_error_callback)(dpy, None, std::ptr::null_mut());
        }
        let (mut major, mut minor): (c_int, c_int) = (0, 0);
        let st = unsafe { (api.initialize)(dpy, &mut major, &mut minor) };
        d.check(st, "vaInitialize")?;
        let vendor = unsafe { (api.vendor_string)(dpy) };
        if !vendor.is_null() {
            d.vendor = unsafe { CStr::from_ptr(vendor) }
                .to_string_lossy()
                .into_owned();
        }
        Ok(d)
    }

    pub fn vendor(&self) -> &str {
        &self.vendor
    }

    pub fn dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }

    fn live(&self) -> Result<(), String> {
        if self.dead() {
            return Err("the VA-API display stopped answering after a GPU wait timed out".into());
        }
        Ok(())
    }

    fn check(&self, st: ffi::VAStatus, what: &str) -> Result<(), String> {
        if st == ffi::VA_STATUS_SUCCESS {
            return Ok(());
        }
        let text = unsafe { (self.api.error_str)(st) };
        let text = if text.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        };
        Err(format!("{what}: {text} ({st:#x})"))
    }

    pub fn attributes(
        &self,
        profile: ffi::VAProfile,
        entrypoint: ffi::VAEntrypoint,
        types: &[ffi::VAConfigAttribType],
    ) -> Result<Vec<u32>, String> {
        let mut attrs: Vec<VAConfigAttrib> = types
            .iter()
            .map(|&type_| VAConfigAttrib { type_, value: 0 })
            .collect();
        let n = c_int::try_from(attrs.len()).map_err(|_| "too many attributes")?;
        let st = unsafe {
            (self.api.get_config_attributes)(self.dpy, profile, entrypoint, attrs.as_mut_ptr(), n)
        };
        self.check(st, "vaGetConfigAttributes")?;
        Ok(attrs.into_iter().map(|a| a.value).collect())
    }

    fn create_config(
        &self,
        profile: ffi::VAProfile,
        entrypoint: ffi::VAEntrypoint,
        attrs: &mut [VAConfigAttrib],
    ) -> Result<ffi::VAConfigID, String> {
        self.live()?;
        let n = c_int::try_from(attrs.len()).map_err(|_| "too many attributes")?;
        let mut id = ffi::VA_INVALID_ID;
        let st = unsafe {
            (self.api.create_config)(
                self.dpy,
                profile,
                entrypoint,
                attrs.as_mut_ptr(),
                n,
                &mut id,
            )
        };
        self.check(st, "vaCreateConfig")?;
        Ok(id)
    }

    fn create_surfaces(
        &self,
        rt_format: c_uint,
        size: (u32, u32),
        count: usize,
    ) -> Result<Vec<VASurfaceID>, String> {
        self.live()?;
        let count_c = c_uint::try_from(count).map_err(|_| "too many surfaces")?;
        let mut surfaces = vec![ffi::VA_INVALID_SURFACE; count];
        let st = unsafe {
            (self.api.create_surfaces)(
                self.dpy,
                rt_format,
                size.0,
                size.1,
                surfaces.as_mut_ptr(),
                count_c,
                std::ptr::null_mut(),
                0,
            )
        };
        self.check(st, "vaCreateSurfaces")?;
        Ok(surfaces)
    }

    fn import(
        &self,
        rt_format: c_uint,
        size: (u32, u32),
        descriptor: &mut ffi::VADRMPRIMESurfaceDescriptor,
    ) -> Result<VASurfaceID, String> {
        let attribute = |type_, value| ffi::VASurfaceAttrib {
            type_,
            flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
            value,
        };
        let mut attributes = [
            attribute(
                ffi::VASurfaceAttribMemoryType,
                ffi::VAGenericValue {
                    type_: ffi::VAGenericValueTypeInteger,
                    value: ffi::VAGenericValueUnion {
                        i: ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 as c_int,
                    },
                },
            ),
            attribute(
                ffi::VASurfaceAttribExternalBufferDescriptor,
                ffi::VAGenericValue {
                    type_: ffi::VAGenericValueTypePointer,
                    value: ffi::VAGenericValueUnion {
                        p: (descriptor as *mut ffi::VADRMPRIMESurfaceDescriptor).cast(),
                    },
                },
            ),
        ];
        let mut surface = ffi::VA_INVALID_SURFACE;
        let st = unsafe {
            (self.api.create_surfaces)(
                self.dpy,
                rt_format,
                size.0,
                size.1,
                &mut surface,
                1,
                attributes.as_mut_ptr().cast(),
                attributes.len() as c_uint,
            )
        };
        self.check(st, "vaCreateSurfaces (import)")?;
        Ok(surface)
    }

    fn create_rgba(&self, size: (u32, u32)) -> Result<VASurfaceID, String> {
        let mut attributes = [ffi::VASurfaceAttrib {
            type_: ffi::VASurfaceAttribPixelFormat,
            flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
            value: ffi::VAGenericValue {
                type_: ffi::VAGenericValueTypeInteger,
                value: ffi::VAGenericValueUnion {
                    i: ffi::VA_FOURCC_RGBA as c_int,
                },
            },
        }];
        let mut surface = ffi::VA_INVALID_SURFACE;
        let st = unsafe {
            (self.api.create_surfaces)(
                self.dpy,
                ffi::VA_RT_FORMAT_RGB32,
                size.0,
                size.1,
                &mut surface,
                1,
                attributes.as_mut_ptr().cast(),
                attributes.len() as c_uint,
            )
        };
        self.check(st, "vaCreateSurfaces (RGBA)")?;
        Ok(surface)
    }

    fn create_rgba_image(&self, size: (u32, u32)) -> Result<VAImage, String> {
        let mut format = VAImageFormat {
            fourcc: ffi::VA_FOURCC_RGBA,
            byte_order: 1,
            bits_per_pixel: 32,
            depth: 32,
            red_mask: 0x0000_00ff,
            green_mask: 0x0000_ff00,
            blue_mask: 0x00ff_0000,
            alpha_mask: 0xff00_0000,
            ..Default::default()
        };
        let mut image = VAImage {
            image_id: ffi::VA_INVALID_ID,
            buf: ffi::VA_INVALID_ID,
            ..Default::default()
        };
        let (w, h) = (
            c_int::try_from(size.0).map_err(|_| "picture too wide")?,
            c_int::try_from(size.1).map_err(|_| "picture too tall")?,
        );
        let st = unsafe { (self.api.create_image)(self.dpy, &mut format, w, h, &mut image) };
        self.check(st, "vaCreateImage (RGBA)")?;
        Ok(image)
    }

    fn create_context(
        &self,
        config: ffi::VAConfigID,
        size: (u32, u32),
        targets: &mut [VASurfaceID],
    ) -> Result<ffi::VAContextID, String> {
        let (w, h) = (
            c_int::try_from(size.0).map_err(|_| "picture too wide")?,
            c_int::try_from(size.1).map_err(|_| "picture too tall")?,
        );
        self.live()?;
        let n = c_int::try_from(targets.len()).map_err(|_| "too many surfaces")?;
        let mut context = ffi::VA_INVALID_ID;
        let st = unsafe {
            (self.api.create_context)(
                self.dpy,
                config,
                w,
                h,
                ffi::VA_PROGRESSIVE,
                targets.as_mut_ptr(),
                n,
                &mut context,
            )
        };
        self.check(st, "vaCreateContext")?;
        Ok(context)
    }

    fn buffer(
        &self,
        context: ffi::VAContextID,
        ty: ffi::VABufferType,
        data: *const c_void,
        len: usize,
    ) -> Result<ffi::VABufferID, String> {
        self.live()?;
        let size = c_uint::try_from(len).map_err(|_| "buffer too large")?;
        let mut id = ffi::VA_INVALID_ID;
        let st = unsafe {
            (self.api.create_buffer)(self.dpy, context, ty, size, 1, data.cast_mut(), &mut id)
        };
        self.check(st, "vaCreateBuffer")?;
        Ok(id)
    }

    fn array_buffer<T>(
        &self,
        context: ffi::VAContextID,
        ty: ffi::VABufferType,
        values: &[T],
    ) -> Result<ffi::VABufferID, String> {
        self.live()?;
        let size = c_uint::try_from(std::mem::size_of::<T>()).map_err(|_| "buffer too large")?;
        let count = c_uint::try_from(values.len()).map_err(|_| "too many elements")?;
        let mut id = ffi::VA_INVALID_ID;
        let st = unsafe {
            (self.api.create_buffer)(
                self.dpy,
                context,
                ty,
                size,
                count,
                values.as_ptr().cast::<c_void>().cast_mut(),
                &mut id,
            )
        };
        self.check(st, "vaCreateBuffer")?;
        Ok(id)
    }

    fn value_buffer<T>(
        &self,
        context: ffi::VAContextID,
        ty: ffi::VABufferType,
        value: &T,
    ) -> Result<ffi::VABufferID, String> {
        self.buffer(
            context,
            ty,
            (value as *const T).cast(),
            std::mem::size_of::<T>(),
        )
    }

    fn destroy_buffers(&self, buffers: &[ffi::VABufferID]) {
        if self.dead() {
            return;
        }
        for &id in buffers {
            unsafe { (self.api.destroy_buffer)(self.dpy, id) };
        }
    }

    fn render(
        &self,
        context: ffi::VAContextID,
        buffers: &mut [ffi::VABufferID],
    ) -> Result<(), String> {
        let n = c_int::try_from(buffers.len()).map_err(|_| "too many buffers")?;
        let st = unsafe { (self.api.render_picture)(self.dpy, context, buffers.as_mut_ptr(), n) };
        self.check(st, "vaRenderPicture")
    }

    fn picture(
        &self,
        context: ffi::VAContextID,
        surface: VASurfaceID,
        groups: &mut [&mut [ffi::VABufferID]],
    ) -> Result<(), String> {
        self.live()?;
        let st = unsafe { (self.api.begin_picture)(self.dpy, context, surface) };
        self.check(st, "vaBeginPicture")?;
        let mut rendered = Ok(());
        for group in groups.iter_mut() {
            if group.is_empty() {
                continue;
            }
            rendered = self.render(context, group);
            if rendered.is_err() {
                break;
            }
        }
        let st = unsafe { (self.api.end_picture)(self.dpy, context) };
        rendered?;
        self.check(st, "vaEndPicture")
    }

    fn sync(&self, surface: VASurfaceID) -> Result<(), String> {
        self.live()?;
        if let Some(sync2) = self.api.sync_surface2
            && self.bounded.load(Ordering::Relaxed)
        {
            let st = unsafe { sync2(self.dpy, surface, SYNC_TIMEOUT_NS) };
            match st {
                ffi::VA_STATUS_ERROR_TIMEDOUT => {
                    self.dead.store(true, Ordering::Release);
                    return Err(format!(
                        "vaSyncSurface2: the GPU did not finish within {} s",
                        SYNC_TIMEOUT_NS / 1_000_000_000
                    ));
                }
                ffi::VA_STATUS_ERROR_UNIMPLEMENTED => self.bounded.store(false, Ordering::Relaxed),
                st => return self.check(st, "vaSyncSurface2"),
            }
        }
        let st = unsafe { (self.api.sync_surface)(self.dpy, surface) };
        self.check(st, "vaSyncSurface")
    }

    fn create_image(&self, bits: u32, size: (u32, u32)) -> Result<VAImage, String> {
        let (_, fourcc, bits_per_pixel) = formats(bits)?;
        let mut format = VAImageFormat {
            fourcc,
            byte_order: 1,
            bits_per_pixel,
            ..Default::default()
        };
        let mut image = VAImage {
            image_id: ffi::VA_INVALID_ID,
            buf: ffi::VA_INVALID_ID,
            ..Default::default()
        };
        let (w, h) = (
            c_int::try_from(size.0).map_err(|_| "picture too wide")?,
            c_int::try_from(size.1).map_err(|_| "picture too tall")?,
        );
        let st = unsafe { (self.api.create_image)(self.dpy, &mut format, w, h, &mut image) };
        self.check(st, "vaCreateImage")?;
        Ok(image)
    }

    fn with_mapped<T>(
        &self,
        buf: ffi::VABufferID,
        f: impl FnOnce(*mut c_void) -> Result<T, String>,
    ) -> Result<T, String> {
        self.live()?;
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let st = unsafe { (self.api.map_buffer)(self.dpy, buf, &mut ptr) };
        self.check(st, "vaMapBuffer")?;
        let result = if ptr.is_null() {
            Err("vaMapBuffer returned no data".to_string())
        } else {
            f(ptr)
        };
        let st = unsafe { (self.api.unmap_buffer)(self.dpy, buf) };
        let value = result?;
        self.check(st, "vaUnmapBuffer")?;
        Ok(value)
    }

    fn with_image<T>(
        &self,
        image: &VAImage,
        f: impl FnOnce(&mut [u8]) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_mapped(image.buf, |ptr| {
            let data = unsafe {
                std::slice::from_raw_parts_mut(ptr.cast::<u8>(), image.data_size as usize)
            };
            f(data)
        })
    }

    fn derive(&self, surface: VASurfaceID) -> Result<VAImage, String> {
        self.sync(surface)?;
        let mut image = VAImage {
            image_id: ffi::VA_INVALID_ID,
            buf: ffi::VA_INVALID_ID,
            ..Default::default()
        };
        let st = unsafe { (self.api.derive_image)(self.dpy, surface, &mut image) };
        self.check(st, "vaDeriveImage")?;
        Ok(image)
    }

    fn export(
        &self,
        surface: VASurfaceID,
        separate: bool,
    ) -> Result<ffi::VADRMPRIMESurfaceDescriptor, String> {
        self.sync(surface)?;
        let mut d = ffi::VADRMPRIMESurfaceDescriptor::default();
        let layout = if separate {
            ffi::VA_EXPORT_SURFACE_SEPARATE_LAYERS
        } else {
            ffi::VA_EXPORT_SURFACE_COMPOSED_LAYERS
        };
        let st = unsafe {
            (self.api.export_surface)(
                self.dpy,
                surface,
                ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                ffi::VA_EXPORT_SURFACE_READ_ONLY | layout,
                (&mut d as *mut ffi::VADRMPRIMESurfaceDescriptor).cast(),
            )
        };
        self.check(st, "vaExportSurfaceHandle")?;
        Ok(d)
    }

    fn destroy_image(&self, image: &VAImage) {
        if self.dead() {
            return;
        }
        unsafe { (self.api.destroy_image)(self.dpy, image.image_id) };
    }

    fn get_image(
        &self,
        surface: VASurfaceID,
        image: &VAImage,
        size: (u32, u32),
    ) -> Result<(), String> {
        self.sync(surface)?;
        let st = unsafe {
            (self.api.get_image)(self.dpy, surface, 0, 0, size.0, size.1, image.image_id)
        };
        self.check(st, "vaGetImage")
    }

    fn put_image(
        &self,
        surface: VASurfaceID,
        image: &VAImage,
        size: (u32, u32),
    ) -> Result<(), String> {
        self.live()?;
        let (w, h) = size;
        let st = unsafe {
            (self.api.put_image)(self.dpy, surface, image.image_id, 0, 0, w, h, 0, 0, w, h)
        };
        self.check(st, "vaPutImage")
    }

    fn release(
        &self,
        image: Option<VAImage>,
        context: ffi::VAContextID,
        surfaces: &mut [VASurfaceID],
        config: ffi::VAConfigID,
    ) {
        if self.dead() {
            return;
        }
        unsafe {
            if let Some(image) = image {
                (self.api.destroy_image)(self.dpy, image.image_id);
            }
            if context != ffi::VA_INVALID_ID {
                (self.api.destroy_context)(self.dpy, context);
            }
            if !surfaces.is_empty() {
                let n = c_int::try_from(surfaces.len()).unwrap_or(0);
                (self.api.destroy_surfaces)(self.dpy, surfaces.as_mut_ptr(), n);
            }
            if config != ffi::VA_INVALID_ID {
                (self.api.destroy_config)(self.dpy, config);
            }
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        if self.dead() {
            std::mem::forget(self.device.take());
            return;
        }
        unsafe { (self.api.terminate)(self.dpy) };
    }
}

pub fn open_display() -> Result<Arc<Display>, String> {
    Display::open().map(Arc::new)
}

fn on_display<T>(
    display: Option<&Arc<Display>>,
    make: impl Fn(Arc<Display>) -> Result<T, String>,
) -> Result<T, String> {
    match display {
        Some(d) => make(d.clone()),
        None => with_display(make),
    }
}

pub fn formats(bits: u32) -> Result<(c_uint, u32, u32), String> {
    match bits {
        8 => Ok((ffi::VA_RT_FORMAT_YUV420, ffi::VA_FOURCC_NV12, 12)),
        10 => Ok((ffi::VA_RT_FORMAT_YUV420_10, ffi::VA_FOURCC_P010, 24)),
        _ => Err(format!("{bits}-bit pictures are not taken through VA-API")),
    }
}

fn biplanar_view<'a>(image: &VAImage, data: &'a [u8]) -> Result<Biplanar<'a>, String> {
    if !matches!(
        image.format.fourcc,
        ffi::VA_FOURCC_NV12 | ffi::VA_FOURCC_P010
    ) || image.num_planes < 2
    {
        return Err(format!(
            "the image is not NV12 / P010 (fourcc {:#x}, {} planes)",
            image.format.fourcc, image.num_planes
        ));
    }
    let [p0, p1, _] = image.pitches;
    if p0 != p1 {
        return Err(format!("NV12 planes with different pitches ({p0}, {p1})"));
    }
    let luma = data
        .get(image.offsets[0] as usize..)
        .ok_or("NV12 luma offset out of range")?;
    let chroma = data
        .get(image.offsets[1] as usize..)
        .ok_or("NV12 chroma offset out of range")?;
    Ok(Biplanar {
        luma,
        chroma,
        stride: p0 as usize,
        rows: usize::from(image.height),
    })
}

pub struct Pool {
    display: Arc<Display>,
    id: u64,
    ids: Vec<VASurfaceID>,
    held: Vec<AtomicBool>,
    size: (u32, u32),
    bound: Mutex<(ffi::VAContextID, ffi::VAConfigID)>,
}

impl Pool {
    fn new(
        display: Arc<Display>,
        rt_format: c_uint,
        size: (u32, u32),
        count: usize,
    ) -> Result<Arc<Self>, String> {
        static POOLS: AtomicU64 = AtomicU64::new(1);
        let ids = display.create_surfaces(rt_format, size, count)?;
        Ok(Arc::new(Pool {
            held: ids.iter().map(|_| AtomicBool::new(false)).collect(),
            display,
            id: POOLS.fetch_add(1, Ordering::Relaxed),
            ids,
            size,
            bound: Mutex::new((ffi::VA_INVALID_ID, ffi::VA_INVALID_ID)),
        }))
    }

    fn bind(&self, context: ffi::VAContextID, config: ffi::VAConfigID) {
        *self.bound.lock().unwrap_or_else(PoisonError::into_inner) = (context, config);
    }

    fn take(self: &Arc<Self>, geometry: Geometry) -> Option<GpuSurface> {
        let index = self.held.iter().position(|h| {
            h.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        })?;
        Some(GpuSurface {
            pool: self.clone(),
            index,
            geometry,
            pts: 0,
        })
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let (context, config) = *self.bound.get_mut().unwrap_or_else(PoisonError::into_inner);
        self.display.release(None, context, &mut self.ids, config);
    }
}

pub struct GpuSurface {
    pool: Arc<Pool>,
    index: usize,
    geometry: Geometry,
    pub pts: i64,
}

impl Drop for GpuSurface {
    fn drop(&mut self) {
        if let Some(h) = self.pool.held.get(self.index) {
            h.store(false, Ordering::Release);
        }
    }
}

impl GpuSurface {
    pub fn width(&self) -> u32 {
        self.geometry.crop.2
    }

    pub fn height(&self) -> u32 {
        self.geometry.crop.3
    }

    fn id(&self) -> Result<VASurfaceID, String> {
        self.pool
            .ids
            .get(self.index)
            .copied()
            .ok_or_else(|| "surface out of range".to_string())
    }

    pub fn key(&self) -> (u64, u32) {
        (self.pool.id, self.index as u32)
    }

    pub fn sync(&self) -> Result<(), String> {
        self.pool.display.sync(self.id()?)
    }

    pub fn dmabuf(&self, separate: bool) -> Result<DmaBuf, String> {
        let d = self.pool.display.export(self.id()?, separate)?;
        let count = (d.num_objects as usize).min(4);
        let mut objects = Vec::with_capacity(count);
        for o in &d.objects[..count] {
            if o.fd < 0 {
                return Err("vaExportSurfaceHandle gave no file descriptor".into());
            }
            objects.push(DmaObject {
                fd: unsafe { OwnedFd::from_raw_fd(o.fd) },
                size: o.size,
                modifier: o.drm_format_modifier,
            });
        }
        let layers = d.layers[..(d.num_layers as usize).min(4)]
            .iter()
            .map(|l| DmaLayer {
                drm_format: l.drm_format,
                planes: (0..(l.num_planes as usize).min(4))
                    .map(|i| DmaPlane {
                        object: l.object_index[i] as usize,
                        offset: l.offset[i],
                        pitch: l.pitch[i],
                    })
                    .collect(),
            })
            .collect();
        Ok(DmaBuf {
            fourcc: d.fourcc,
            width: d.width,
            height: d.height,
            crop: self.geometry.crop,
            objects,
            layers,
        })
    }

    pub fn download(&self) -> Result<Frame, String> {
        let surface = self.id()?;
        let d = &self.pool.display;
        let geometry = self.geometry;
        if let Ok(image) = d.derive(surface) {
            let frame = d.with_image(&image, |data| {
                biplanar::to_frame(&biplanar_view(&image, data)?, &geometry)
            });
            d.destroy_image(&image);
            if let Ok(f) = frame {
                return Ok(Frame { pts: self.pts, ..f });
            }
        }
        let image = d.create_image(geometry.bits, self.pool.size)?;
        let frame = d.get_image(surface, &image, self.pool.size).and_then(|()| {
            d.with_image(&image, |data| {
                biplanar::to_frame(&biplanar_view(&image, data)?, &geometry)
            })
        });
        d.destroy_image(&image);
        frame.map(|f| Frame { pts: self.pts, ..f })
    }
}

pub struct DmaObject {
    pub fd: OwnedFd,
    pub size: u32,
    pub modifier: u64,
}

pub struct DmaPlane {
    pub object: usize,
    pub offset: u32,
    pub pitch: u32,
}

pub struct DmaLayer {
    pub drm_format: u32,
    pub planes: Vec<DmaPlane>,
}

pub struct DmaBuf {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub crop: (u32, u32, u32, u32),
    pub objects: Vec<DmaObject>,
    pub layers: Vec<DmaLayer>,
}

pub enum Picture {
    Cpu(Frame),
    Gpu(GpuSurface),
}

impl Picture {
    pub fn at(self, pts: i64) -> Self {
        match self {
            Picture::Cpu(f) => Picture::Cpu(Frame { pts, ..f }),
            Picture::Gpu(mut g) => {
                g.pts = pts;
                Picture::Gpu(g)
            }
        }
    }
}

pub struct Session {
    display: Arc<Display>,
    config: ffi::VAConfigID,
    context: ffi::VAContextID,
    pool: Arc<Pool>,
    image: Option<VAImage>,
    size: (u32, u32),
    geometry: Geometry,
    derive: bool,
    staging: Option<Staging>,
    staged: bool,
    pub gpu: bool,
}

struct Staged {
    surface: VASurfaceID,
    buffer: HostBuffer,
    holds: Option<usize>,
    order: u64,
}

struct Staging {
    vpp: Vpp,
    slots: Vec<Staged>,
    pitch: usize,
    chroma: usize,
    size: (u32, u32),
    bits: u32,
    order: u64,
    most: usize,
}

impl Staging {
    fn new(
        display: Arc<Display>,
        geometry: Geometry,
        surface: (u32, u32),
        (count, most): (usize, usize),
    ) -> Result<Self, String> {
        let (w, h) = (
            geometry.crop.2.next_multiple_of(2),
            geometry.crop.3.next_multiple_of(2),
        );
        if geometry.crop.0.saturating_add(w) > surface.0
            || geometry.crop.1.saturating_add(h) > surface.1
        {
            return Err("the picture has no even-sized area to copy".into());
        }
        let bps = if geometry.bits > 8 { 2 } else { 1 };
        let pitch = (w as usize * bps).next_multiple_of(256);
        let chroma = pitch * h as usize;
        let vpp = Vpp::new(display.clone())?;
        let mut staging = Self {
            vpp,
            slots: Vec::with_capacity(most),
            pitch,
            chroma,
            size: (w, h),
            bits: geometry.bits,
            order: 0,
            most,
        };
        for _ in 0..count.min(most) {
            let slot = staging.slot()?;
            staging.slots.push(slot);
        }
        Ok(staging)
    }

    fn slot(&self) -> Result<Staged, String> {
        let ten = self.bits > 8;
        let (rt_format, fourcc, _) = formats(self.bits)?;
        let (w, h) = self.size;
        let buffer = HostBuffer::new(self.chroma + self.pitch * (h as usize / 2))?;
        let pitch = u32::try_from(self.pitch).map_err(|_| "picture too wide")?;
        let mut layer = ffi::VADRMPRIMELayer {
            drm_format: if ten {
                ffi::DRM_FORMAT_P010
            } else {
                ffi::DRM_FORMAT_NV12
            },
            num_planes: 2,
            ..Default::default()
        };
        layer.offset[1] = u32::try_from(self.chroma).map_err(|_| "picture too large")?;
        layer.pitch = [pitch, pitch, 0, 0];
        let mut descriptor = ffi::VADRMPRIMESurfaceDescriptor {
            fourcc,
            width: w,
            height: h,
            num_objects: 1,
            num_layers: 1,
            ..Default::default()
        };
        descriptor.objects[0] = ffi::VADRMPRIMEObject {
            fd: buffer.fd(),
            size: u32::try_from(buffer.len()).map_err(|_| "picture too large")?,
            drm_format_modifier: ffi::DRM_FORMAT_MOD_LINEAR,
        };
        descriptor.layers[0] = layer;
        let surface = self
            .vpp
            .display
            .import(rt_format, (w, h), &mut descriptor)?;
        Ok(Staged {
            surface,
            buffer,
            holds: None,
            order: 0,
        })
    }

    fn copy(
        &mut self,
        src: VASurfaceID,
        index: usize,
        crop: (u32, u32, u32, u32),
    ) -> Result<(), String> {
        for slot in &mut self.slots {
            if slot.holds == Some(index) {
                slot.holds = None;
            }
        }
        if self.slots.iter().all(|s| s.holds.is_some())
            && self.slots.len() < self.most
            && let Ok(slot) = self.slot()
        {
            self.slots.push(slot);
        }
        let Some(slot) = self
            .slots
            .iter_mut()
            .min_by_key(|s| (s.holds.is_some(), s.order))
        else {
            return Err("no staging surfaces".into());
        };
        slot.holds = None;
        let region = (crop.0, crop.1, self.size.0, self.size.1);
        self.vpp.copy(src, region, slot.surface, self.size)?;
        self.order += 1;
        slot.holds = Some(index);
        slot.order = self.order;
        Ok(())
    }

    fn read(&mut self, index: usize, crop: (u32, u32, u32, u32)) -> Option<Result<Frame, String>> {
        let slot = self.slots.iter_mut().find(|s| s.holds == Some(index))?;
        slot.holds = None;
        let display = &self.vpp.display;
        if let Err(e) = display.sync(slot.surface) {
            return Some(Err(e));
        }
        let data = slot.buffer.data();
        let (Some(luma), Some(chroma)) = (data.get(..self.chroma), data.get(self.chroma..)) else {
            return Some(Err("staging buffer too small".into()));
        };
        let view = Biplanar {
            luma,
            chroma,
            stride: self.pitch,
            rows: self.size.1 as usize,
        };
        let geometry = Geometry {
            crop: (0, 0, crop.2, crop.3),
            bits: self.bits,
        };
        Some(biplanar::to_frame(&view, &geometry))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let mut ids: Vec<VASurfaceID> = self.slots.iter().map(|s| s.surface).collect();
        self.vpp
            .display
            .release(None, ffi::VA_INVALID_ID, &mut ids, ffi::VA_INVALID_ID);
    }
}

impl Session {
    pub fn new(
        display: Option<&Arc<Display>>,
        profiles: &[ffi::VAProfile],
        size: (u32, u32),
        count: usize,
        geometry: Geometry,
    ) -> Result<Self, String> {
        on_display(display, |d| Self::on(d, profiles, size, count, geometry))
    }

    fn on(
        display: Arc<Display>,
        profiles: &[ffi::VAProfile],
        size: (u32, u32),
        count: usize,
        geometry: Geometry,
    ) -> Result<Self, String> {
        let (rt_format, _, _) = formats(geometry.bits)?;
        let mut config = None;
        let mut why = "no VA-API profile for the stream".to_string();
        for &profile in profiles {
            let value = match display.attributes(
                profile,
                ffi::VAEntrypointVLD,
                &[ffi::VAConfigAttribRTFormat],
            ) {
                Ok(v) => v.first().copied().unwrap_or(0),
                Err(e) => {
                    why = e;
                    continue;
                }
            };
            if value == ffi::VA_ATTRIB_NOT_SUPPORTED || value & rt_format == 0 {
                why = format!(
                    "profile {profile} does not decode to {}-bit 4:2:0",
                    geometry.bits
                );
                continue;
            }
            let mut attr = [VAConfigAttrib {
                type_: ffi::VAConfigAttribRTFormat,
                value: rt_format,
            }];
            match display.create_config(profile, ffi::VAEntrypointVLD, &mut attr) {
                Ok(id) => {
                    config = Some(id);
                    break;
                }
                Err(e) => why = e,
            }
        }
        let config = config.ok_or(why)?;
        let size = (size.0.next_multiple_of(2), size.1.next_multiple_of(2));
        let pool = match Pool::new(display.clone(), rt_format, size, count) {
            Ok(p) => p,
            Err(e) => {
                display.release(None, ffi::VA_INVALID_ID, &mut [], config);
                return Err(e);
            }
        };
        pool.bind(ffi::VA_INVALID_ID, config);
        let mut s = Self {
            display,
            config,
            context: ffi::VA_INVALID_ID,
            pool,
            image: None,
            size,
            geometry,
            derive: std::env::var_os("PITO_VA_NO_DERIVE").is_none(),
            staging: None,
            staged: std::env::var_os("PITO_VA_NO_STAGING").is_none(),
            gpu: false,
        };
        let mut targets = s.pool.ids.clone();
        s.context = s.display.create_context(s.config, size, &mut targets)?;
        s.pool.bind(s.context, s.config);
        Ok(s)
    }

    pub fn vendor(&self) -> &str {
        self.display.vendor()
    }

    pub fn direct(&mut self) {
        self.staged = false;
        self.staging = None;
    }

    fn image(&mut self) -> Result<VAImage, String> {
        if let Some(image) = self.image {
            return Ok(image);
        }
        let image = self.display.create_image(self.geometry.bits, self.size)?;
        self.image = Some(image);
        Ok(image)
    }

    fn surface(&self, index: usize) -> Result<VASurfaceID, String> {
        self.pool
            .ids
            .get(index)
            .copied()
            .ok_or_else(|| "surface out of range".to_string())
    }

    fn get_image(&mut self, index: usize) -> Result<VAImage, String> {
        let surface = self.surface(index)?;
        let image = self.image()?;
        self.display.get_image(surface, &image, self.size)?;
        Ok(image)
    }

    pub(super) fn decode_picture<P, Q, S>(
        &mut self,
        target: usize,
        pic: &P,
        iq: Option<&Q>,
        slices: &[(S, Vec<u8>)],
    ) -> Result<(), String> {
        let surface = self.surface(target)?;
        let mut buffers = Vec::with_capacity(2 + 2 * slices.len());
        let decoded = self.decode_into(surface, &mut buffers, pic, iq, slices);
        self.display.destroy_buffers(&buffers);
        decoded
    }

    fn decode_into<P, Q, S>(
        &self,
        surface: VASurfaceID,
        buffers: &mut Vec<ffi::VABufferID>,
        pic: &P,
        iq: Option<&Q>,
        slices: &[(S, Vec<u8>)],
    ) -> Result<(), String> {
        let d = &self.display;
        let ctx = self.context;
        buffers.push(d.value_buffer(ctx, ffi::VAPictureParameterBufferType, pic)?);
        if let Some(iq) = iq {
            buffers.push(d.value_buffer(ctx, ffi::VAIQMatrixBufferType, iq)?);
        }
        let head = buffers.len();
        for (params, data) in slices {
            buffers.push(d.value_buffer(ctx, ffi::VASliceParameterBufferType, params)?);
            buffers.push(d.buffer(
                ctx,
                ffi::VASliceDataBufferType,
                data.as_ptr().cast(),
                data.len(),
            )?);
        }
        let (picture, slice_buffers) = buffers.split_at_mut(head);
        let mut groups: Vec<&mut [ffi::VABufferID]> = vec![picture];
        groups.extend(slice_buffers.chunks_mut(2));
        d.picture(ctx, surface, &mut groups)
    }

    pub(super) fn decode_groups<P, S>(
        &mut self,
        target: usize,
        pic: &P,
        groups: &[(&[S], &[u8])],
    ) -> Result<(), String> {
        let surface = self.surface(target)?;
        let mut buffers = Vec::with_capacity(1 + 2 * groups.len());
        let decoded = self.groups_into(surface, &mut buffers, pic, groups);
        self.display.destroy_buffers(&buffers);
        decoded
    }

    fn groups_into<P, S>(
        &self,
        surface: VASurfaceID,
        buffers: &mut Vec<ffi::VABufferID>,
        pic: &P,
        groups: &[(&[S], &[u8])],
    ) -> Result<(), String> {
        let d = &self.display;
        let ctx = self.context;
        buffers.push(d.value_buffer(ctx, ffi::VAPictureParameterBufferType, pic)?);
        for (params, data) in groups {
            buffers.push(d.array_buffer(ctx, ffi::VASliceParameterBufferType, params)?);
            buffers.push(d.buffer(
                ctx,
                ffi::VASliceDataBufferType,
                data.as_ptr().cast(),
                data.len(),
            )?);
        }
        let (picture, slice_buffers) = buffers.split_at_mut(1);
        let mut groups: Vec<&mut [ffi::VABufferID]> = vec![picture];
        groups.extend(slice_buffers.chunks_mut(2));
        d.picture(ctx, surface, &mut groups)
    }

    fn read_cpu(&mut self, index: usize) -> Result<Frame, String> {
        if let Some(staging) = &mut self.staging
            && let Some(frame) = staging.read(index, self.geometry.crop)
        {
            match frame {
                Ok(frame) => return Ok(frame),
                Err(_) => {
                    self.staging = None;
                    self.staged = false;
                }
            }
        }
        if self.derive {
            let surface = self.surface(index)?;
            match self.display.derive(surface) {
                Ok(image) => {
                    let geometry = self.geometry;
                    let frame = self.display.with_image(&image, |data| {
                        biplanar::to_frame(&biplanar_view(&image, data)?, &geometry)
                    });
                    self.display.destroy_image(&image);
                    if frame.is_ok() {
                        return frame;
                    }
                    self.derive = false;
                }
                Err(_) => self.derive = false,
            }
        }
        let image = self.get_image(index)?;
        let geometry = self.geometry;
        self.display.with_image(&image, |data| {
            biplanar::to_frame(&biplanar_view(&image, data)?, &geometry)
        })
    }
}

impl Accel for Session {
    fn surfaces(&self) -> &[VASurfaceID] {
        &self.pool.ids
    }

    fn held(&self, index: usize) -> bool {
        self.pool
            .held
            .get(index)
            .is_some_and(|h| h.load(Ordering::Acquire))
    }

    fn decode_h264(
        &mut self,
        target: usize,
        pic: &ffi::VAPictureParameterBufferH264,
        iq: &ffi::VAIQMatrixBufferH264,
        slices: &[(ffi::VASliceParameterBufferH264, Vec<u8>)],
    ) -> Result<(), String> {
        self.decode_picture(target, pic, Some(iq), slices)
    }

    fn decode_hevc(
        &mut self,
        target: usize,
        pic: &ffi::VAPictureParameterBufferHEVC,
        iq: Option<&ffi::VAIQMatrixBufferHEVC>,
        slices: &[(ffi::VASliceParameterBufferHEVC, Vec<u8>)],
    ) -> Result<(), String> {
        self.decode_picture(target, pic, iq, slices)
    }

    fn copies(&self) -> bool {
        !self.gpu && self.staged
    }

    fn prefetch(&mut self, index: usize) {
        if self.gpu || !self.staged {
            return;
        }
        let Ok(surface) = self.surface(index) else {
            return;
        };
        if self.staging.is_none() {
            match Staging::new(
                self.display.clone(),
                self.geometry,
                self.size,
                (LAG + 2, LAG + SLACK + 2),
            ) {
                Ok(s) => self.staging = Some(s),
                Err(_) => {
                    self.staged = false;
                    return;
                }
            }
        }
        if let Some(staging) = &mut self.staging
            && staging.copy(surface, index, self.geometry.crop).is_err()
        {
            self.staging = None;
            self.staged = false;
        }
    }

    fn read(&mut self, index: usize) -> Result<Picture, String> {
        if !self.gpu {
            return self.read_cpu(index).map(Picture::Cpu);
        }
        let flag = self.pool.held.get(index).ok_or("surface out of range")?;
        flag.store(true, Ordering::Release);
        Ok(Picture::Gpu(GpuSurface {
            pool: self.pool.clone(),
            index,
            geometry: self.geometry,
            pts: 0,
        }))
    }

    fn fill(&mut self, index: usize, from: Option<usize>) -> Result<(), String> {
        let surface = self.surface(index)?;
        let image = match from {
            Some(from) => self.get_image(from)?,
            None => {
                let image = self.image()?;
                let ten = self.geometry.bits > 8;
                self.display.with_image(&image, |data| {
                    if ten {
                        for w in data.as_chunks_mut::<2>().0 {
                            w.copy_from_slice(&0x8000u16.to_le_bytes());
                        }
                    } else {
                        data.fill(128);
                    }
                    Ok(())
                })?;
                image
            }
        };
        self.display.put_image(surface, &image, self.size)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.display.release(
            self.image.take(),
            ffi::VA_INVALID_ID,
            &mut [],
            ffi::VA_INVALID_ID,
        );
    }
}

pub struct Vpp {
    display: Arc<Display>,
    config: ffi::VAConfigID,
    context: ffi::VAContextID,
    owned: bool,
}

impl Vpp {
    fn new(display: Arc<Display>) -> Result<Self, String> {
        let config =
            display.create_config(ffi::VAProfileNone, ffi::VAEntrypointVideoProc, &mut [])?;
        let mut vpp = Self {
            display,
            config,
            context: ffi::VA_INVALID_ID,
            owned: true,
        };
        vpp.context = vpp.display.create_context(vpp.config, (0, 0), &mut [])?;
        Ok(vpp)
    }

    fn blit(&self, src: &GpuSurface, dst: VASurfaceID, size: (u32, u32)) -> Result<(), String> {
        self.copy(src.id()?, src.geometry.crop, dst, size)
    }

    fn copy(
        &self,
        src: VASurfaceID,
        crop: (u32, u32, u32, u32),
        dst: VASurfaceID,
        size: (u32, u32),
    ) -> Result<(), String> {
        self.compose(src, crop, dst, size, None)
    }

    fn compose(
        &self,
        src: VASurfaceID,
        crop: (u32, u32, u32, u32),
        dst: VASurfaceID,
        size: (u32, u32),
        over: Option<Over>,
    ) -> Result<(), String> {
        let from = rectangle(crop)?;
        let to = rectangle((0, 0, size.0, size.1))?;
        let video = pipeline(
            src,
            &from,
            &to,
            ffi::VAProcColorStandardBT709,
            std::ptr::null(),
        );
        let mut buffers = vec![self.display.value_buffer(
            self.context,
            ffi::VAProcPipelineParameterBufferType,
            &video,
        )?];
        let blend = ffi::VABlendState {
            flags: ffi::VA_BLEND_PREMULTIPLIED_ALPHA,
            global_alpha: 1.0,
            ..Default::default()
        };
        if let Some(o) = over {
            let painted = rectangle((0, 0, o.rect.2, o.rect.3))?;
            let placed = rectangle(o.rect)?;
            let mut layer = pipeline(
                o.surface,
                &painted,
                &placed,
                o.standard,
                (&blend as *const ffi::VABlendState).cast(),
            );
            layer.output_color_properties.chroma_sample_location =
                ffi::VA_CHROMA_SITING_VERTICAL_CENTER | ffi::VA_CHROMA_SITING_HORIZONTAL_CENTER;
            match self.display.value_buffer(
                self.context,
                ffi::VAProcPipelineParameterBufferType,
                &layer,
            ) {
                Ok(id) => buffers.push(id),
                Err(e) => {
                    self.display.destroy_buffers(&buffers);
                    return Err(e);
                }
            }
        }
        let mut groups: Vec<&mut [ffi::VABufferID]> = vec![&mut buffers[..]];
        let done = self.display.picture(self.context, dst, &mut groups);
        self.display.destroy_buffers(&buffers);
        done
    }
}

fn rectangle((x, y, w, h): (u32, u32, u32, u32)) -> Result<ffi::VARectangle, String> {
    let narrow = |v: u32| u16::try_from(v).map_err(|_| "picture too large".to_string());
    let wide = |v: u32| i16::try_from(v).map_err(|_| "picture too large".to_string());
    Ok(ffi::VARectangle {
        x: wide(x)?,
        y: wide(y)?,
        width: narrow(w)?,
        height: narrow(h)?,
    })
}

fn pipeline(
    surface: VASurfaceID,
    from: &ffi::VARectangle,
    to: &ffi::VARectangle,
    standard: c_int,
    blend_state: *const c_void,
) -> ffi::VAProcPipelineParameterBuffer {
    ffi::VAProcPipelineParameterBuffer {
        surface,
        surface_region: from,
        surface_color_standard: standard,
        output_region: to,
        output_background_color: 0xff00_0000,
        output_color_standard: standard,
        pipeline_flags: 0,
        filter_flags: 0,
        filters: std::ptr::null_mut(),
        num_filters: 0,
        forward_references: std::ptr::null_mut(),
        num_forward_references: 0,
        backward_references: std::ptr::null_mut(),
        num_backward_references: 0,
        rotation_state: 0,
        blend_state,
        mirror_state: 0,
        additional_outputs: std::ptr::null_mut(),
        num_additional_outputs: 0,
        input_surface_flag: 0,
        output_surface_flag: 0,
        input_color_properties: ffi::VAProcColorProperties::default(),
        output_color_properties: ffi::VAProcColorProperties::default(),
        processing_mode: 0,
        output_hdr_metadata: std::ptr::null_mut(),
        va_reserved: [0; ffi::PROC_RESERVED],
    }
}

#[derive(Clone, Copy)]
struct Over {
    surface: VASurfaceID,
    rect: (u32, u32, u32, u32),
    standard: c_int,
}

const PAINTED: usize = 3;

struct Painted {
    surface: VASurfaceID,
    image: VAImage,
    size: (u32, u32),
}

struct Shown {
    layer: Arc<Rgba>,
    size: (u32, u32),
    over: Option<Over>,
}

struct Painter {
    display: Arc<Display>,
    ring: Vec<Painted>,
    next: usize,
    layer: Option<Shown>,
}

impl Painter {
    fn new(display: Arc<Display>) -> Self {
        Self {
            display,
            ring: Vec::new(),
            next: 0,
            layer: None,
        }
    }

    fn paint(&mut self, layer: &Arc<Rgba>, size: (u32, u32)) -> Result<Option<Over>, String> {
        if let Some(shown) = &self.layer
            && Arc::ptr_eq(&shown.layer, layer)
            && shown.size == size
        {
            return Ok(shown.over);
        }
        self.layer = None;
        if (layer.width, layer.height) != size {
            return Err(format!(
                "the overlay is {}x{}, the picture {}x{}",
                layer.width, layer.height, size.0, size.1
            ));
        }
        let over = match painted_box(layer) {
            Some(rect) => Some(self.upload(layer, rect, size)?),
            None => None,
        };
        self.layer = Some(Shown {
            layer: layer.clone(),
            size,
            over,
        });
        Ok(over)
    }

    fn upload(
        &mut self,
        layer: &Rgba,
        (x, y, w, h): (u32, u32, u32, u32),
        size: (u32, u32),
    ) -> Result<Over, String> {
        let want = (w.next_multiple_of(64), h.next_multiple_of(64));
        let i = self.next % PAINTED;
        self.next = self.next.wrapping_add(1);
        let fits = self
            .ring
            .get(i)
            .is_some_and(|p| p.size.0 >= want.0 && p.size.1 >= want.1);
        if !fits {
            let surface = self.display.create_rgba(want)?;
            let image = match self.display.create_rgba_image(want) {
                Ok(image) => image,
                Err(e) => {
                    self.display.release(
                        None,
                        ffi::VA_INVALID_ID,
                        &mut [surface],
                        ffi::VA_INVALID_ID,
                    );
                    return Err(e);
                }
            };
            let fresh = Painted {
                surface,
                image,
                size: want,
            };
            if i < self.ring.len() {
                let mut old = std::mem::replace(&mut self.ring[i], fresh);
                self.display.release(
                    Some(old.image),
                    ffi::VA_INVALID_ID,
                    std::slice::from_mut(&mut old.surface),
                    ffi::VA_INVALID_ID,
                );
            } else {
                self.ring.push(fresh);
            }
        }
        let slot = self.ring.get(i).ok_or("no overlay surface")?;
        let image = slot.image;
        let (pitch, offset) = (image.pitches[0] as usize, image.offsets[0] as usize);
        let width = layer.width as usize;
        self.display.with_image(&image, |data| {
            data.fill(0);
            for r in 0..h as usize {
                let from = ((y as usize + r) * width + x as usize) * 4;
                let src = layer
                    .data
                    .get(from..from + w as usize * 4)
                    .ok_or("the overlay is shorter than its size")?;
                let at = offset + r * pitch;
                let dst = data
                    .get_mut(at..at + w as usize * 4)
                    .ok_or("the overlay image is too small")?;
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    let a = u32::from(s[3]);
                    for k in 0..3 {
                        d[k] = ((u32::from(s[k]) * a + 127) / 255) as u8;
                    }
                    d[3] = s[3];
                }
            }
            Ok(())
        })?;
        self.display.put_image(slot.surface, &image, slot.size)?;
        Ok(Over {
            surface: slot.surface,
            rect: (x, y, w, h),
            standard: if size.1 >= 720 {
                ffi::VAProcColorStandardBT709
            } else {
                ffi::VAProcColorStandardBT601
            },
        })
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        for p in &mut self.ring {
            self.display.release(
                Some(p.image),
                ffi::VA_INVALID_ID,
                std::slice::from_mut(&mut p.surface),
                ffi::VA_INVALID_ID,
            );
        }
    }
}

fn painted_box(layer: &Rgba) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = (layer.width as usize, layer.height as usize);
    let rows = layer.data.get(..w * h * 4)?.chunks_exact(w * 4);
    let mut span: Option<(usize, usize, usize, usize)> = None;
    for (y, row) in rows.enumerate() {
        let alpha = || row.iter().skip(3).step_by(4);
        let Some(first) = alpha().position(|&a| a != 0) else {
            continue;
        };
        let last = w - 1 - alpha().rev().position(|&a| a != 0).unwrap_or(0);
        span = Some(match span {
            Some((x0, y0, x1, _)) => (x0.min(first), y0, x1.max(last), y),
            None => (first, y, last, y),
        });
    }
    let (x0, y0, x1, y1) = span?;
    let (x0, y0) = (x0 & !1, y0 & !1);
    let (x1, y1) = (((x1 + 2) & !1).min(w), ((y1 + 2) & !1).min(h));
    Some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
}

pub struct Scaler {
    vpp: Vpp,
    pool: Arc<Pool>,
    size: (u32, u32),
    bits: u32,
}

impl Scaler {
    pub fn new(
        display: Option<&Arc<Display>>,
        size: (u32, u32),
        bits: u32,
        count: usize,
    ) -> Result<Self, String> {
        let display = match display {
            Some(d) => d.clone(),
            None => shared()?,
        };
        let (rt_format, _, _) = formats(bits)?;
        let mut vpp = Vpp::new(display.clone())?;
        let pool = Pool::new(display, rt_format, size, count.max(1))?;
        pool.bind(vpp.context, vpp.config);
        vpp.owned = false;
        Ok(Self {
            vpp,
            pool,
            size,
            bits,
        })
    }

    pub fn scale(&self, src: &GpuSurface) -> Result<GpuSurface, String> {
        if !Arc::ptr_eq(&src.pool.display, &self.vpp.display) {
            return Err("the GPU frame is on another VA display".into());
        }
        let geometry = Geometry {
            crop: (0, 0, self.size.0, self.size.1),
            bits: self.bits,
        };
        let mut out = self
            .pool
            .take(geometry)
            .ok_or_else(|| format!("all {} scaled frames are still held", self.pool.ids.len()))?;
        self.vpp.blit(src, out.id()?, self.size)?;
        out.pts = src.pts;
        Ok(out)
    }
}

impl Drop for Vpp {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        self.display
            .release(None, self.context, &mut [], self.config);
    }
}

pub fn probe() -> Result<Arc<Display>, String> {
    static PROBE: OnceLock<Result<(), String>> = OnceLock::new();
    PROBE.get_or_init(|| shared().map(|_| ())).clone()?;
    shared()
}

fn write_frame(
    image: &VAImage,
    data: &mut [u8],
    frame: &Frame,
    size: (u32, u32),
) -> Result<(), String> {
    if !matches!(
        image.format.fourcc,
        ffi::VA_FOURCC_NV12 | ffi::VA_FOURCC_P010
    ) || image.num_planes < 2
    {
        return Err(format!(
            "the input image is not NV12 / P010 (fourcc {:#x})",
            image.format.fourcc
        ));
    }
    let [p0, p1, _] = image.pitches;
    let (o0, o1) = (image.offsets[0] as usize, image.offsets[1] as usize);
    biplanar::from_frame(frame, data, (o0, p0 as usize), (o1, p1 as usize), size)
}

pub struct Slot {
    pub input: VASurfaceID,
    pub coded: ffi::VABufferID,
}

pub struct EncodeSession {
    display: Arc<Display>,
    config: ffi::VAConfigID,
    context: ffi::VAContextID,
    surfaces: Vec<VASurfaceID>,
    coded: Vec<ffi::VABufferID>,
    image: Option<VAImage>,
    size: (u32, u32),
    bits: u32,
    inputs: usize,
    derive: bool,
    vpp: Option<Vpp>,
    painter: Option<Painter>,
}

pub struct EncodeSetup {
    pub profile: ffi::VAProfile,
    pub rate_control: u32,
    pub size: (u32, u32),
    pub bits: u32,
    pub inputs: usize,
    pub references: usize,
    pub coded_size: usize,
}

impl EncodeSession {
    pub fn new(display: Option<&Arc<Display>>, setup: &EncodeSetup) -> Result<Self, String> {
        on_display(display, |d| Self::on(d, setup))
    }

    fn on(display: Arc<Display>, setup: &EncodeSetup) -> Result<Self, String> {
        let (rt_format, _, _) = formats(setup.bits)?;
        let mut why = String::new();
        let mut config = None;
        for entrypoint in [ffi::VAEntrypointEncSlice, ffi::VAEntrypointEncSliceLP] {
            let mut attrs = [
                VAConfigAttrib {
                    type_: ffi::VAConfigAttribRTFormat,
                    value: rt_format,
                },
                VAConfigAttrib {
                    type_: ffi::VAConfigAttribRateControl,
                    value: setup.rate_control,
                },
                VAConfigAttrib {
                    type_: ffi::VAConfigAttribEncPackedHeaders,
                    value: ffi::VA_ENC_PACKED_HEADER_SEQUENCE
                        | ffi::VA_ENC_PACKED_HEADER_PICTURE
                        | ffi::VA_ENC_PACKED_HEADER_SLICE,
                },
            ];
            match display.create_config(setup.profile, entrypoint, &mut attrs) {
                Ok(id) => {
                    config = Some(id);
                    break;
                }
                Err(e) => why = e,
            }
        }
        let config = config.ok_or(why)?;
        let mut s = Self {
            display,
            config,
            context: ffi::VA_INVALID_ID,
            surfaces: Vec::new(),
            coded: Vec::new(),
            image: None,
            size: setup.size,
            bits: setup.bits,
            inputs: setup.inputs,
            derive: std::env::var_os("PITO_VA_NO_DERIVE").is_none(),
            vpp: None,
            painter: None,
        };
        s.surfaces =
            s.display
                .create_surfaces(rt_format, setup.size, setup.inputs + setup.references)?;
        s.context = s
            .display
            .create_context(s.config, setup.size, &mut s.surfaces)?;
        for _ in 0..setup.inputs {
            let id = s.display.buffer(
                s.context,
                ffi::VAEncCodedBufferType,
                std::ptr::null(),
                setup.coded_size,
            )?;
            s.coded.push(id);
        }
        Ok(s)
    }

    pub fn vendor(&self) -> &str {
        self.display.vendor()
    }

    pub fn slot(&self, i: usize) -> Option<Slot> {
        Some(Slot {
            input: *self.surfaces.get(i)?,
            coded: *self.coded.get(i)?,
        })
    }

    pub fn reference(&self, i: usize) -> Option<VASurfaceID> {
        self.surfaces.get(self.inputs + i).copied()
    }

    pub fn blit(
        &mut self,
        surface: VASurfaceID,
        src: &GpuSurface,
        size: (u32, u32),
    ) -> Result<(), String> {
        if !Arc::ptr_eq(&src.pool.display, &self.display) {
            return Err("the GPU frame is on another VA display".into());
        }
        if self.vpp.is_none() {
            self.vpp = Some(Vpp::new(self.display.clone())?);
        }
        match &self.vpp {
            Some(vpp) => vpp.blit(src, surface, size),
            None => Err("no VA-API video processor".into()),
        }
    }

    pub fn composes(&self) -> bool {
        self.display.vendor().starts_with("Mesa")
    }

    pub fn blit_over(
        &mut self,
        surface: VASurfaceID,
        src: &GpuSurface,
        size: (u32, u32),
        layer: &Arc<Rgba>,
    ) -> Result<(), String> {
        if !self.composes() {
            return Err(format!(
                "drawing an overlay on the GPU is tested only on Mesa's VA-API, not {}",
                self.display.vendor()
            ));
        }
        if !Arc::ptr_eq(&src.pool.display, &self.display) {
            return Err("the GPU frame is on another VA display".into());
        }
        if self.vpp.is_none() {
            self.vpp = Some(Vpp::new(self.display.clone())?);
        }
        let painter = self
            .painter
            .get_or_insert_with(|| Painter::new(self.display.clone()));
        let over = painter.paint(layer, size)?;
        match &self.vpp {
            Some(vpp) => vpp.compose(src.id()?, src.geometry.crop, surface, size, over),
            None => Err("no VA-API video processor".into()),
        }
    }

    pub fn upload(&mut self, surface: VASurfaceID, frame: &Frame) -> Result<(), String> {
        let size = self.size;
        if self.derive {
            match self.display.derive(surface) {
                Ok(image) => {
                    let written = self
                        .display
                        .with_image(&image, |data| write_frame(&image, data, frame, size));
                    self.display.destroy_image(&image);
                    if written.is_ok() {
                        return Ok(());
                    }
                    self.derive = false;
                }
                Err(_) => self.derive = false,
            }
        }
        let image = match self.image {
            Some(i) => i,
            None => {
                let i = self.display.create_image(self.bits, self.size)?;
                self.image = Some(i);
                i
            }
        };
        self.display
            .with_image(&image, |data| write_frame(&image, data, frame, size))?;
        self.display.put_image(surface, &image, self.size)
    }

    pub fn encode(
        &self,
        input: VASurfaceID,
        params: &[(ffi::VABufferType, Vec<u8>)],
    ) -> Result<(), String> {
        let mut buffers = Vec::with_capacity(params.len());
        let mut made = Ok(());
        for (ty, bytes) in params {
            match self
                .display
                .buffer(self.context, *ty, bytes.as_ptr().cast(), bytes.len())
            {
                Ok(id) => buffers.push(id),
                Err(e) => {
                    made = Err(e);
                    break;
                }
            }
        }
        let result = made.and_then(|()| {
            let mut groups: Vec<&mut [ffi::VABufferID]> = vec![&mut buffers[..]];
            self.display.picture(self.context, input, &mut groups)
        });
        self.display.destroy_buffers(&buffers);
        result
    }

    pub fn collect(&self, input: VASurfaceID, coded: ffi::VABufferID) -> Result<Vec<u8>, String> {
        self.display.sync(input)?;
        self.display.with_mapped(coded, |ptr| {
            let mut out = Vec::new();
            let mut seg = ptr.cast::<ffi::VACodedBufferSegment>();
            let mut guard = 0;
            while !seg.is_null() && guard < 64 {
                let s = unsafe { *seg };
                if !s.buf.is_null() && s.size > 0 {
                    let bytes =
                        unsafe { std::slice::from_raw_parts(s.buf.cast::<u8>(), s.size as usize) };
                    out.extend_from_slice(bytes);
                }
                seg = s.next.cast();
                guard += 1;
            }
            Ok(out)
        })
    }
}

impl Drop for EncodeSession {
    fn drop(&mut self) {
        self.painter = None;
        self.vpp = None;
        self.display.destroy_buffers(&self.coded);
        self.display.release(
            self.image.take(),
            self.context,
            &mut self.surfaces,
            self.config,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::painted_box;
    use crate::media::Rgba;

    #[test]
    fn the_painted_box_covers_every_visible_pixel_on_even_bounds() {
        let mut layer = Rgba::new(16, 10);
        assert_eq!(painted_box(&layer), None);
        for (x, y) in [(3, 3), (10, 6)] {
            layer.data[(y * 16 + x) * 4 + 3] = 1;
        }
        assert_eq!(painted_box(&layer), Some((2, 2, 10, 6)));
        layer.data[(9 * 16 + 15) * 4 + 3] = 255;
        assert_eq!(painted_box(&layer), Some((2, 2, 14, 8)));
    }
}
