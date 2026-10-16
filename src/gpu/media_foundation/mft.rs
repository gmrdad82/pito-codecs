use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFDXGIDeviceManager, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform,
    MF_E_NO_MORE_TYPES, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE, MF_TRANSFORM_ASYNC, MF_VERSION,
    MFMediaType_Video, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_FRIENDLY_NAME_Attribute,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFVideoFormat_AV1,
    MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoFormat_P010,
    MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{GUID, HRESULT, Interface, PCSTR, PWSTR, w};

use super::device::{Gpu, SurfaceFormat, ensure_com};
use crate::media::Codec;

type StartupFn = unsafe extern "system" fn(u32, u32) -> HRESULT;
type CreateFn = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;
type CreateBufferFn = unsafe extern "system" fn(u32, *mut *mut c_void) -> HRESULT;
type CreateManagerFn = unsafe extern "system" fn(*mut u32, *mut *mut c_void) -> HRESULT;
type EnumFn = unsafe extern "system" fn(
    GUID,
    u32,
    *const MFT_REGISTER_TYPE_INFO,
    *const MFT_REGISTER_TYPE_INFO,
    *mut *mut *mut c_void,
    *mut u32,
) -> HRESULT;

pub struct MfApi {
    create_media_type: CreateFn,
    create_sample: CreateFn,
    create_memory_buffer: CreateBufferFn,
    create_manager: CreateManagerFn,
    enum_transforms: EnumFn,
}

unsafe fn symbol<F: Copy>(module: HMODULE, name: &std::ffi::CStr) -> Result<F, String> {
    if size_of::<F>() != size_of::<usize>() {
        return Err("not a function pointer type".into());
    }
    let p = unsafe { GetProcAddress(module, PCSTR(name.as_ptr().cast())) }
        .ok_or_else(|| format!("mfplat.dll has no {}", name.to_string_lossy()))?;
    Ok(unsafe { std::mem::transmute_copy::<_, F>(&p) })
}

pub fn api() -> Result<&'static MfApi, String> {
    static API: OnceLock<Result<MfApi, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<MfApi, String> {
    ensure_com()?;
    let module = unsafe { LoadLibraryExW(w!("mfplat.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map_err(|_| "Media Foundation is not installed".to_string())?;
    let (startup, api) = unsafe {
        (
            symbol::<StartupFn>(module, c"MFStartup")?,
            MfApi {
                create_media_type: symbol(module, c"MFCreateMediaType")?,
                create_sample: symbol(module, c"MFCreateSample")?,
                create_memory_buffer: symbol(module, c"MFCreateMemoryBuffer")?,
                create_manager: symbol(module, c"MFCreateDXGIDeviceManager")?,
                enum_transforms: symbol(module, c"MFTEnumEx")?,
            },
        )
    };
    let hr = unsafe { startup(MF_VERSION, 0) };
    if hr.is_err() {
        return Err(format!("MFStartup failed ({hr:?})"));
    }
    Ok(api)
}

fn made<T: Interface>(hr: HRESULT, raw: *mut c_void, what: &str) -> Result<T, String> {
    if hr.is_err() || raw.is_null() {
        return Err(format!("{what} failed ({hr:?})"));
    }
    Ok(unsafe { T::from_raw(raw) })
}

pub fn subtype(codec: Codec) -> GUID {
    match codec {
        Codec::Hevc => MFVideoFormat_HEVC,
        Codec::Av1 => MFVideoFormat_AV1,
        _ => MFVideoFormat_H264,
    }
}

impl MfApi {
    pub fn create_device_manager(
        &self,
        device: &ID3D11Device,
    ) -> Result<IMFDXGIDeviceManager, String> {
        let (mut token, mut raw) = (0u32, std::ptr::null_mut());
        let hr = unsafe { (self.create_manager)(&mut token, &mut raw) };
        let manager: IMFDXGIDeviceManager = made(hr, raw, "MFCreateDXGIDeviceManager")?;
        unsafe { manager.ResetDevice(device, token) }
            .map_err(|e| format!("IMFDXGIDeviceManager::ResetDevice failed: {e}"))?;
        Ok(manager)
    }

    fn media_type(&self) -> Result<IMFMediaType, String> {
        let mut raw = std::ptr::null_mut();
        let hr = unsafe { (self.create_media_type)(&mut raw) };
        made(hr, raw, "MFCreateMediaType")
    }

    fn sample_of(&self, data: &[u8], time: i64) -> Result<IMFSample, String> {
        let len = u32::try_from(data.len()).map_err(|_| "packet is too large".to_string())?;
        let mut raw = std::ptr::null_mut();
        let hr = unsafe { (self.create_memory_buffer)(len, &mut raw) };
        let buffer: IMFMediaBuffer = made(hr, raw, "MFCreateMemoryBuffer")?;
        let mut raw = std::ptr::null_mut();
        let hr = unsafe { (self.create_sample)(&mut raw) };
        let sample: IMFSample = made(hr, raw, "MFCreateSample")?;
        unsafe {
            let (mut ptr, mut max) = (std::ptr::null_mut::<u8>(), 0u32);
            buffer
                .Lock(&mut ptr, Some(&mut max), None)
                .map_err(|e| format!("cannot lock the input buffer: {e}"))?;
            if ptr.is_null() || max < len {
                let _ = buffer.Unlock();
                return Err("input buffer is too small".into());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer
                .Unlock()
                .map_err(|e| format!("cannot unlock the input buffer: {e}"))?;
            buffer
                .SetCurrentLength(len)
                .map_err(|e| format!("cannot size the input buffer: {e}"))?;
            sample
                .AddBuffer(&buffer)
                .map_err(|e| format!("cannot add the input buffer: {e}"))?;
            sample
                .SetSampleTime(time)
                .map_err(|e| format!("cannot timestamp the input: {e}"))?;
        }
        Ok(sample)
    }

    fn decoders(&self, subtype: GUID) -> Result<Vec<IMFActivate>, String> {
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: subtype,
        };
        let flags = MFT_ENUM_FLAG_SYNCMFT.0
            | MFT_ENUM_FLAG_ASYNCMFT.0
            | MFT_ENUM_FLAG_HARDWARE.0
            | MFT_ENUM_FLAG_SORTANDFILTER.0;
        let (mut list, mut count) = (std::ptr::null_mut::<*mut c_void>(), 0u32);
        let hr = unsafe {
            (self.enum_transforms)(
                MFT_CATEGORY_VIDEO_DECODER,
                flags as u32,
                &input,
                std::ptr::null(),
                &mut list,
                &mut count,
            )
        };
        if hr.is_err() {
            return Err(format!("MFTEnumEx failed ({hr:?})"));
        }
        let mut out = Vec::new();
        if !list.is_null() {
            for i in 0..count as usize {
                unsafe {
                    let p = *list.add(i);
                    if !p.is_null() {
                        out.push(IMFActivate::from_raw(p));
                    }
                }
            }
            unsafe { CoTaskMemFree(Some(list as *const c_void)) };
        }
        Ok(out)
    }

    pub fn has_decoder(&self, codec: Codec) -> bool {
        let Ok(list) = self.decoders(subtype(codec)) else {
            return false;
        };
        list.iter().any(|a| unsafe {
            let fits = a
                .ActivateObject::<IMFTransform>()
                .and_then(|t| t.GetAttributes())
                .is_ok_and(|attrs| {
                    attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 0
                        && attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0
                });
            let _ = a.ShutdownObject();
            fits
        })
    }
}

pub enum Poll {
    Frame(IMFSample, i64),
    NeedInput,
    FormatChanged,
}

pub struct Spec {
    pub codec: Codec,
    pub size: (u32, u32),
    pub format: SurfaceFormat,
}

pub struct Mft {
    activate: IMFActivate,
    transform: IMFTransform,
    format: SurfaceFormat,
    name: String,
}

unsafe impl Send for Mft {}

impl Mft {
    pub fn new(api: &MfApi, gpu: &Gpu, spec: &Spec) -> Result<Mft, String> {
        ensure_com()?;
        let (label, missing) = match spec.codec {
            Codec::Hevc => ("HEVC", " (the HEVC Video Extensions are not installed)"),
            Codec::Av1 => ("AV1", " (the AV1 Video Extension is not installed)"),
            _ => ("H.264", ""),
        };
        let candidates = api.decoders(subtype(spec.codec))?;
        if candidates.is_empty() {
            return Err(format!("no {label} decoder MFT{missing}"));
        }
        let mut reasons = Vec::new();
        for a in candidates {
            match Self::configure(api, gpu, a, spec) {
                Ok(m) => return Ok(m),
                Err(e) => reasons.push(e),
            }
        }
        Err(reasons.join("; "))
    }

    fn configure(
        api: &MfApi,
        gpu: &Gpu,
        activate: IMFActivate,
        spec: &Spec,
    ) -> Result<Mft, String> {
        let name = friendly_name(&activate).unwrap_or_else(|| "a decoder MFT".into());
        let transform: IMFTransform = unsafe { activate.ActivateObject() }
            .map_err(|e| format!("{name}: cannot be created: {e}"))?;
        let mft = Mft {
            activate,
            transform,
            format: spec.format,
            name,
        };
        let (transform, name) = (&mft.transform, &mft.name);
        unsafe {
            let attrs = transform
                .GetAttributes()
                .map_err(|e| format!("{name}: no attributes: {e}"))?;
            if attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0 {
                return Err(format!(
                    "{name}: asynchronous decoder MFTs are not supported"
                ));
            }
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                return Err(format!("{name}: does not decode on Direct3D 11"));
            }
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager_param())
                .map_err(|e| format!("{name}: refused the Direct3D device manager: {e}"))?;
        }
        let input = api.media_type()?;
        let (w, h) = spec.size;
        unsafe {
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| e.to_string())?;
            input
                .SetGUID(&MF_MT_SUBTYPE, &subtype(spec.codec))
                .map_err(|e| e.to_string())?;
            input
                .SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(w) << 32) | u64::from(h))
                .map_err(|e| e.to_string())?;
            input
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|e| e.to_string())?;
            if spec.codec == Codec::Hevc {
                let profile = if spec.format == SurfaceFormat::P010 {
                    2
                } else {
                    1
                };
                input
                    .SetUINT32(&MF_MT_MPEG2_PROFILE, profile)
                    .map_err(|e| e.to_string())?;
            }
            mft.transform
                .SetInputType(0, &input, 0)
                .map_err(|e| format!("{}: does not take this stream: {e}", mft.name))?;
        }
        mft.select_output()?;
        unsafe {
            let info = mft
                .transform
                .GetOutputStreamInfo(0)
                .map_err(|e| format!("{}: no output stream info: {e}", mft.name))?;
            if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                return Err(format!(
                    "{}: does not provide Direct3D output samples",
                    mft.name
                ));
            }
            mft.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| e.to_string())?;
            mft.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| e.to_string())?;
        }
        Ok(mft)
    }

    fn select_output(&self) -> Result<(), String> {
        let want = match self.format {
            SurfaceFormat::Nv12 => MFVideoFormat_NV12,
            SurfaceFormat::P010 => MFVideoFormat_P010,
        };
        for i in 0..64 {
            unsafe {
                match self.transform.GetOutputAvailableType(0, i) {
                    Ok(t) => {
                        if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(want) {
                            return self.transform.SetOutputType(0, &t, 0).map_err(|e| {
                                format!("{}: refused {:?} output: {e}", self.name, self.format)
                            });
                        }
                    }
                    Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                    Err(e) => return Err(format!("{}: no output types: {e}", self.name)),
                }
            }
        }
        Err(format!("{}: cannot output {:?}", self.name, self.format))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn input(&self, api: &MfApi, data: &[u8], time: i64) -> Result<bool, String> {
        let sample = api.sample_of(data, time)?;
        match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            Ok(()) => Ok(true),
            Err(e) if e.code() == MF_E_NOTACCEPTING => Ok(false),
            Err(e) => Err(format!("{}: ProcessInput failed: {e}", self.name)),
        }
    }

    pub fn poll(&self) -> Result<Poll, String> {
        let mut out = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(None),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let r = unsafe { self.transform.ProcessOutput(0, &mut out, &mut status) };
        let (sample, _events) = unsafe {
            (
                ManuallyDrop::take(&mut out[0].pSample),
                ManuallyDrop::take(&mut out[0].pEvents),
            )
        };
        match r {
            Ok(()) => {
                let sample = sample.ok_or_else(|| format!("{}: produced no sample", self.name))?;
                let time = unsafe { sample.GetSampleTime() }.unwrap_or(0);
                Ok(Poll::Frame(sample, time))
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Poll::NeedInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                self.select_output()?;
                Ok(Poll::FormatChanged)
            }
            Err(e) => Err(format!("{}: ProcessOutput failed: {e}", self.name)),
        }
    }

    pub fn drain(&self) -> Result<(), String> {
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
            .map_err(|e| format!("{}: drain failed: {e}", self.name))
    }

    pub fn flush(&self) -> Result<(), String> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)
                .map_err(|e| format!("{}: flush failed: {e}", self.name))?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| format!("{}: restart failed: {e}", self.name))
        }
    }
}

impl Drop for Mft {
    fn drop(&mut self) {
        let _ = unsafe { self.activate.ShutdownObject() };
    }
}

fn friendly_name(a: &IMFActivate) -> Option<String> {
    unsafe {
        let mut s = PWSTR::null();
        let mut len = 0u32;
        a.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut s, &mut len)
            .ok()?;
        let name = s.to_string().ok();
        CoTaskMemFree(Some(s.0 as *const c_void));
        name
    }
}
