use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use windows::Win32::Foundation::{HMODULE, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11_VIDEO_DECODER_DESC, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Multithread, ID3D11Texture2D, ID3D11VideoDevice,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::Win32::Media::MediaFoundation::{IMFDXGIBuffer, IMFDXGIDeviceManager, IMFSample};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::{GUID, Interface};

use super::mft::MfApi;
use crate::gpu::biplanar::Biplanar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceFormat {
    Nv12,
    P010,
}

impl SurfaceFormat {
    pub fn bits(self) -> u32 {
        match self {
            Self::Nv12 => 8,
            Self::P010 => 10,
        }
    }

    fn dxgi(self) -> DXGI_FORMAT {
        match self {
            Self::Nv12 => DXGI_FORMAT_NV12,
            Self::P010 => DXGI_FORMAT_P010,
        }
    }
}

thread_local! {
    static COM: Result<(), String> = init_com();
}

fn init_com() -> Result<(), String> {
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_ok() || hr == RPC_E_CHANGED_MODE {
        Ok(())
    } else {
        Err(format!("COM initialisation failed ({hr:?})"))
    }
}

pub fn ensure_com() -> Result<(), String> {
    COM.with(Clone::clone)
}

pub struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    manager: IMFDXGIDeviceManager,
    name: String,
    lost: AtomicBool,
    gate: Mutex<()>,
}

unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

static SHARED: Mutex<Option<Arc<Gpu>>> = Mutex::new(None);

impl Gpu {
    pub fn shared(api: &MfApi) -> Result<Arc<Gpu>, String> {
        ensure_com()?;
        let mut slot = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(g) = slot.as_ref().filter(|g| g.usable()) {
            return Ok(g.clone());
        }
        let g = Arc::new(Gpu::create(api)?);
        *slot = Some(g.clone());
        Ok(g)
    }

    fn create(api: &MfApi) -> Result<Gpu, String> {
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|e| format!("no Direct3D 11 video device: {e}"))?;
        let (device, context) = device
            .zip(context)
            .ok_or("Direct3D 11 returned no device")?;
        let mt: ID3D11Multithread = device
            .cast()
            .map_err(|e| format!("no ID3D11Multithread: {e}"))?;
        let _ = unsafe { mt.SetMultithreadProtected(true) };
        let video: ID3D11VideoDevice = device
            .cast()
            .map_err(|e| format!("the Direct3D 11 device has no video decoding: {e}"))?;
        let manager = api.create_device_manager(&device)?;
        let name = adapter_name(&device).unwrap_or_else(|| "an unknown adapter".into());
        Ok(Gpu {
            device,
            context,
            video,
            manager,
            name,
            lost: AtomicBool::new(false),
            gate: Mutex::new(()),
        })
    }

    fn usable(&self) -> bool {
        !self.lost.load(Ordering::Relaxed)
            && unsafe { self.device.GetDeviceRemovedReason() }.is_ok()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn manager_param(&self) -> usize {
        self.manager.as_raw() as usize
    }

    pub fn note_failure(&self) {
        if !self.usable() {
            self.lost.store(true, Ordering::Relaxed);
        }
    }

    pub fn supports(
        &self,
        profile: GUID,
        format: SurfaceFormat,
        size: (u32, u32),
    ) -> Result<(), String> {
        unsafe {
            let ok = self
                .video
                .CheckVideoDecoderFormat(&profile, format.dxgi())
                .map_err(|e| format!("DXVA format check failed: {e}"))?;
            if !ok.as_bool() {
                return Err(format!(
                    "the GPU has no DXVA decoder for this profile in {format:?}"
                ));
            }
            let desc = D3D11_VIDEO_DECODER_DESC {
                Guid: profile,
                SampleWidth: size.0,
                SampleHeight: size.1,
                OutputFormat: format.dxgi(),
            };
            let configs = self
                .video
                .GetVideoDecoderConfigCount(&desc)
                .map_err(|e| format!("DXVA configuration query failed: {e}"))?;
            if configs == 0 {
                return Err(format!(
                    "the GPU's DXVA decoder does not take {}x{} pictures",
                    size.0, size.1
                ));
            }
        }
        Ok(())
    }
}

fn adapter_name(device: &ID3D11Device) -> Option<String> {
    let dxgi: IDXGIDevice = device.cast().ok()?;
    let desc = unsafe { dxgi.GetAdapter().ok()?.GetDesc().ok()? };
    let len = desc
        .Description
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(desc.Description.len());
    Some(String::from_utf16_lossy(&desc.Description[..len]))
}

#[derive(Default)]
pub struct Readback {
    staging: Option<(ID3D11Texture2D, D3D11_TEXTURE2D_DESC)>,
}

unsafe impl Send for Readback {}

impl Readback {
    pub fn read<R>(
        &mut self,
        gpu: &Gpu,
        sample: &IMFSample,
        format: SurfaceFormat,
        f: impl FnOnce(&Biplanar) -> Result<R, String>,
    ) -> Result<R, String> {
        let r = self.copy(gpu, sample, format, f);
        if r.is_err() {
            gpu.note_failure();
        }
        r
    }

    fn copy<R>(
        &mut self,
        gpu: &Gpu,
        sample: &IMFSample,
        format: SurfaceFormat,
        f: impl FnOnce(&Biplanar) -> Result<R, String>,
    ) -> Result<R, String> {
        let (tex, index) = unsafe {
            let buffer = sample
                .GetBufferByIndex(0)
                .map_err(|e| format!("decoder output has no buffer: {e}"))?;
            let dxgi: IMFDXGIBuffer = buffer.cast().map_err(|_| {
                "decoder output is in system memory: the decoder is not using the GPU".to_string()
            })?;
            let mut tex: Option<ID3D11Texture2D> = None;
            dxgi.GetResource(
                &ID3D11Texture2D::IID,
                &mut tex as *mut Option<ID3D11Texture2D> as *mut *mut c_void,
            )
            .map_err(|e| format!("decoder output is not a 2D texture: {e}"))?;
            (
                tex.ok_or("decoder output has no texture")?,
                dxgi.GetSubresourceIndex()
                    .map_err(|e| format!("no subresource index: {e}"))?,
            )
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        if desc.Format != format.dxgi() {
            return Err(format!(
                "decoded surface has DXGI format {} instead of {format:?}",
                desc.Format.0
            ));
        }
        if desc.Width == 0 || desc.Height == 0 || desc.Width > 16384 || desc.Height > 16384 {
            return Err(format!("decoded surface is {}x{}", desc.Width, desc.Height));
        }
        let staging = self.staging_for(gpu, &desc)?;
        let _gate = gpu.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            gpu.context
                .CopySubresourceRegion(staging, 0, 0, 0, 0, &tex, index, None);
            gpu.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| format!("cannot map the decoded surface: {e}"))?;
        }
        let result = (|| {
            let stride = mapped.RowPitch as usize;
            let rows = desc.Height as usize;
            let luma_len = stride.checked_mul(rows).ok_or("surface size overflows")?;
            let chroma_len = stride
                .checked_mul(rows.div_ceil(2))
                .ok_or("surface size overflows")?;
            if mapped.pData.is_null() || stride == 0 {
                return Err("the decoded surface is not mapped".to_string());
            }
            let all = unsafe {
                std::slice::from_raw_parts(mapped.pData as *const u8, luma_len + chroma_len)
            };
            let (luma, chroma) = all.split_at(luma_len);
            f(&Biplanar {
                luma,
                chroma,
                stride,
                rows,
            })
        })();
        unsafe { gpu.context.Unmap(staging, 0) };
        result
    }

    fn staging_for(
        &mut self,
        gpu: &Gpu,
        src: &D3D11_TEXTURE2D_DESC,
    ) -> Result<&ID3D11Texture2D, String> {
        let fits = self.staging.as_ref().is_some_and(|(_, d)| {
            d.Width == src.Width && d.Height == src.Height && d.Format == src.Format
        });
        if !fits {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: src.Width,
                Height: src.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: src.Format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)) }
                .map_err(|e| format!("cannot create the readback texture: {e}"))?;
            self.staging = Some((tex.ok_or("no readback texture")?, desc));
        }
        self.staging
            .as_ref()
            .map(|(t, _)| t)
            .ok_or_else(|| "no readback texture".to_string())
    }
}
