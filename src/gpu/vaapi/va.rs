use std::ffi::{CStr, c_int, c_uint, c_void};
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use libloading::Library;

use super::Accel;
use super::ffi::{self, VAConfigAttrib, VAImage, VAImageFormat, VASurfaceID};
use crate::gpu::biplanar::{self, Biplanar, Geometry};
use crate::media::Frame;

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

pub struct Display {
    api: &'static Api,
    dpy: ffi::VADisplay,
    vendor: String,
    _device: File,
}

unsafe impl Send for Display {}
unsafe impl Sync for Display {}

static SHARED: Mutex<Option<Arc<Display>>> = Mutex::new(None);

fn shared() -> Result<Arc<Display>, String> {
    let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(d) = shared.as_ref() {
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
            _device: device,
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
        let size = c_uint::try_from(len).map_err(|_| "buffer too large")?;
        let mut id = ffi::VA_INVALID_ID;
        let st = unsafe {
            (self.api.create_buffer)(self.dpy, context, ty, size, 1, data.cast_mut(), &mut id)
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
        unsafe { (self.api.terminate)(self.dpy) };
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
    ids: Vec<VASurfaceID>,
    held: Vec<AtomicBool>,
    size: (u32, u32),
}

impl Pool {
    fn new(
        display: Arc<Display>,
        rt_format: c_uint,
        size: (u32, u32),
        count: usize,
    ) -> Result<Arc<Self>, String> {
        let ids = display.create_surfaces(rt_format, size, count)?;
        Ok(Arc::new(Pool {
            held: ids.iter().map(|_| AtomicBool::new(false)).collect(),
            display,
            ids,
            size,
        }))
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
        self.display
            .release(None, ffi::VA_INVALID_ID, &mut self.ids, ffi::VA_INVALID_ID);
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
    pub gpu: bool,
}

impl Session {
    pub fn new(
        profiles: &[ffi::VAProfile],
        size: (u32, u32),
        count: usize,
        geometry: Geometry,
    ) -> Result<Self, String> {
        with_display(|d| Self::on(d, profiles, size, count, geometry))
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
        let pool = match Pool::new(display.clone(), rt_format, size, count) {
            Ok(p) => p,
            Err(e) => {
                display.release(None, ffi::VA_INVALID_ID, &mut [], config);
                return Err(e);
            }
        };
        let mut s = Self {
            display,
            config,
            context: ffi::VA_INVALID_ID,
            pool,
            image: None,
            size,
            geometry,
            derive: std::env::var_os("PITO_VA_NO_DERIVE").is_none(),
            gpu: false,
        };
        let mut targets = s.pool.ids.clone();
        s.context = s.display.create_context(s.config, size, &mut targets)?;
        Ok(s)
    }

    pub fn vendor(&self) -> &str {
        self.display.vendor()
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

    fn decode_picture<P, Q, S>(
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

    fn read_cpu(&mut self, index: usize) -> Result<Frame, String> {
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
        self.display
            .release(self.image.take(), self.context, &mut [], self.config);
    }
}

pub struct Vpp {
    display: Arc<Display>,
    config: ffi::VAConfigID,
    context: ffi::VAContextID,
}

impl Vpp {
    fn new(display: Arc<Display>) -> Result<Self, String> {
        let config =
            display.create_config(ffi::VAProfileNone, ffi::VAEntrypointVideoProc, &mut [])?;
        let mut vpp = Self {
            display,
            config,
            context: ffi::VA_INVALID_ID,
        };
        vpp.context = vpp.display.create_context(vpp.config, (0, 0), &mut [])?;
        Ok(vpp)
    }

    fn blit(&self, src: &GpuSurface, dst: VASurfaceID, size: (u32, u32)) -> Result<(), String> {
        let (x, y, w, h) = src.geometry.crop;
        let narrow = |v: u32| u16::try_from(v).map_err(|_| "picture too large".to_string());
        let wide = |v: u32| i16::try_from(v).map_err(|_| "picture too large".to_string());
        let from = ffi::VARectangle {
            x: wide(x)?,
            y: wide(y)?,
            width: narrow(w)?,
            height: narrow(h)?,
        };
        let to = ffi::VARectangle {
            x: 0,
            y: 0,
            width: narrow(size.0)?,
            height: narrow(size.1)?,
        };
        let params = ffi::VAProcPipelineParameterBuffer {
            surface: src.id()?,
            surface_region: &from,
            surface_color_standard: ffi::VAProcColorStandardBT709,
            output_region: &to,
            output_background_color: 0xff00_0000,
            output_color_standard: ffi::VAProcColorStandardBT709,
            pipeline_flags: 0,
            filter_flags: 0,
            filters: std::ptr::null_mut(),
            num_filters: 0,
            forward_references: std::ptr::null_mut(),
            num_forward_references: 0,
            backward_references: std::ptr::null_mut(),
            num_backward_references: 0,
            rotation_state: 0,
            blend_state: std::ptr::null(),
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
        };
        let mut buffers = vec![self.display.value_buffer(
            self.context,
            ffi::VAProcPipelineParameterBufferType,
            &params,
        )?];
        let mut groups: Vec<&mut [ffi::VABufferID]> = vec![&mut buffers[..]];
        let done = self.display.picture(self.context, dst, &mut groups);
        self.display.destroy_buffers(&buffers);
        done
    }
}

pub struct Scaler {
    vpp: Vpp,
    pool: Arc<Pool>,
    size: (u32, u32),
    bits: u32,
}

impl Scaler {
    pub fn new(size: (u32, u32), bits: u32, count: usize) -> Result<Self, String> {
        let display = shared()?;
        let (rt_format, _, _) = formats(bits)?;
        let vpp = Vpp::new(display.clone())?;
        let pool = Pool::new(display, rt_format, size, count.max(1))?;
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
    pub fn new(setup: &EncodeSetup) -> Result<Self, String> {
        with_display(|d| Self::on(d, setup))
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
        self.display.destroy_buffers(&self.coded);
        self.display.release(
            self.image.take(),
            self.context,
            &mut self.surfaces,
            self.config,
        );
    }
}
