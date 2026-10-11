use std::ffi::c_void;

#[cfg(target_os = "linux")]
pub use cuda::Device;
#[cfg(target_os = "windows")]
pub use direct3d::Device;

#[cfg(target_os = "windows")]
mod direct3d {
    use super::c_void;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    };
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
    use windows::core::Interface;

    const NVIDIA: u32 = 0x10de;

    pub struct Device(ID3D11Device);

    impl Device {
        pub const KIND: u32 = super::super::ffi::NV_ENC_DEVICE_TYPE_DIRECTX;

        pub fn new() -> Result<Self, String> {
            unsafe {
                let factory: IDXGIFactory1 =
                    CreateDXGIFactory1().map_err(|e| format!("no DXGI: {e}"))?;
                let mut i = 0;
                while let Ok(adapter) = factory.EnumAdapters1(i) {
                    i += 1;
                    let Ok(desc) = adapter.GetDesc1() else {
                        continue;
                    };
                    if desc.VendorId != NVIDIA {
                        continue;
                    }
                    let Ok(adapter) = adapter.cast::<IDXGIAdapter>() else {
                        continue;
                    };
                    let mut device = None;
                    if D3D11CreateDevice(
                        &adapter,
                        D3D_DRIVER_TYPE_UNKNOWN,
                        HMODULE::default(),
                        D3D11_CREATE_DEVICE_FLAG(0),
                        None,
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        None,
                        None,
                    )
                    .is_ok()
                        && let Some(d) = device
                    {
                        return Ok(Self(d));
                    }
                }
            }
            Err("no NVIDIA adapter".into())
        }

        pub fn as_raw(&self) -> *mut c_void {
            self.0.as_raw()
        }
    }
}

#[cfg(target_os = "linux")]
mod cuda {
    use super::c_void;

    type Init = unsafe extern "C" fn(u32) -> i32;
    type GetDevice = unsafe extern "C" fn(*mut i32, i32) -> i32;
    type Retain = unsafe extern "C" fn(*mut *mut c_void, i32) -> i32;
    type Release = unsafe extern "C" fn(i32) -> i32;

    pub struct Device {
        context: *mut c_void,
        ordinal: i32,
        release: Release,
        _library: libloading::Library,
    }

    impl Device {
        pub const KIND: u32 = super::super::ffi::NV_ENC_DEVICE_TYPE_CUDA;

        pub fn new() -> Result<Self, String> {
            unsafe {
                let library = libloading::Library::new("libcuda.so.1")
                    .map_err(|e| format!("no NVIDIA CUDA driver: {e}"))?;
                let init: Init = *library.get(b"cuInit\0").map_err(|e| e.to_string())?;
                let get: GetDevice = *library.get(b"cuDeviceGet\0").map_err(|e| e.to_string())?;
                let retain: Retain = *library
                    .get(b"cuDevicePrimaryCtxRetain\0")
                    .map_err(|e| e.to_string())?;
                let release: Release = *library
                    .get(b"cuDevicePrimaryCtxRelease_v2\0")
                    .map_err(|e| e.to_string())?;
                let st = init(0);
                if st != 0 {
                    return Err(format!("cuInit failed ({st})"));
                }
                let mut ordinal = 0;
                let st = get(&mut ordinal, 0);
                if st != 0 {
                    return Err(format!("no CUDA device ({st})"));
                }
                let mut context = std::ptr::null_mut();
                let st = retain(&mut context, ordinal);
                if st != 0 {
                    return Err(format!("cuDevicePrimaryCtxRetain failed ({st})"));
                }
                if context.is_null() {
                    release(ordinal);
                    return Err("CUDA returned no primary context".into());
                }
                Ok(Self {
                    context,
                    ordinal,
                    release,
                    _library: library,
                })
            }
        }

        pub fn as_raw(&self) -> *mut c_void {
            self.context
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            unsafe {
                (self.release)(self.ordinal);
            }
        }
    }
}
