use std::ffi::{CStr, c_void};
use std::sync::OnceLock;

use super::device::Device;
use super::ffi::*;
use crate::gpu::Rate;
use crate::media::Codec;

struct Api {
    list: NV_ENCODE_API_FUNCTION_LIST,
    _library: libloading::Library,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

#[cfg(target_os = "linux")]
unsafe fn open_library() -> Result<libloading::Library, libloading::Error> {
    unsafe { libloading::Library::new("libnvidia-encode.so.1") }
}

#[cfg(target_os = "windows")]
unsafe fn open_library() -> Result<libloading::Library, libloading::Error> {
    use libloading::os::windows::{LOAD_LIBRARY_SEARCH_SYSTEM32, Library};
    unsafe { Library::load_with_flags("nvEncodeAPI64.dll", LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map(Into::into)
}

fn load() -> Result<Api, String> {
    unsafe {
        let library = open_library().map_err(|e| format!("no NVIDIA encoder driver: {e}"))?;
        let version: MaxSupportedVersion = *library
            .get(b"NvEncodeAPIGetMaxSupportedVersion\0")
            .map_err(|e| e.to_string())?;
        let create: CreateInstance = *library
            .get(b"NvEncodeAPICreateInstance\0")
            .map_err(|e| e.to_string())?;
        let mut v = 0;
        if version(&mut v) != NV_ENC_SUCCESS {
            return Err("cannot query the NVENC version".into());
        }
        let ours = ((NVENCAPI_VERSION & 0xf) << 4) | ((NVENCAPI_VERSION >> 24) & 0xf);
        if v < ours {
            return Err(format!(
                "the NVIDIA driver's NVENC {}.{} is older than {}.{}",
                v >> 4,
                v & 0xf,
                ours >> 4,
                ours & 0xf
            ));
        }
        let mut list: NV_ENCODE_API_FUNCTION_LIST = std::mem::zeroed();
        list.version = NV_ENCODE_API_FUNCTION_LIST_VER;
        if create(&mut list) != NV_ENC_SUCCESS {
            return Err("NvEncodeAPICreateInstance failed".into());
        }
        Ok(Api {
            list,
            _library: library,
        })
    }
}

fn codec_guid(codec: Codec) -> Guid {
    match codec {
        Codec::Hevc => NV_ENC_CODEC_HEVC_GUID,
        _ => NV_ENC_CODEC_H264_GUID,
    }
}

pub struct Caps {
    pub ten_bit: bool,
    pub max_b_frames: u32,
    pub min_size: (u32, u32),
    pub max_size: (u32, u32),
}

pub struct Setup {
    pub codec: Codec,
    pub size: (u32, u32),
    pub fps: (u32, u32),
    pub profile: Guid,
    pub ten_bit: bool,
    pub rate: Rate,
    pub gop: u32,
    pub b_frames: u32,
    pub level: u32,
    pub slots: usize,
}

pub struct Locked<'a> {
    pub data: &'a mut [u8],
    pub pitch: usize,
}

struct Slot {
    input: *mut c_void,
    output: *mut c_void,
}

pub struct Session {
    api: &'static Api,
    handle: Handle,
    slots: Vec<Slot>,
    size: (u32, u32),
    format: u32,
    sample: usize,
    _device: Device,
}

unsafe impl Send for Session {}

fn status(what: &str, s: NvStatus, handle: Handle, api: &Api) -> String {
    let detail = (!handle.is_null())
        .then(|| {
            api.list.nvEncGetLastErrorString.and_then(|f| unsafe {
                let p = f(handle);
                (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
            })
        })
        .flatten()
        .filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_graphic() || c == ' '));
    match detail {
        Some(d) => format!("{what} failed ({s}): {d}"),
        None => format!("{what} failed ({s})"),
    }
}

fn need<T>(f: Option<T>, name: &str) -> Result<T, String> {
    f.ok_or_else(|| format!("the NVIDIA driver has no {name}"))
}

impl Session {
    pub fn open() -> Result<Session, String> {
        let api = api()?;
        let device = Device::new()?;
        let open = need(
            api.list.nvEncOpenEncodeSessionEx,
            "nvEncOpenEncodeSessionEx",
        )?;
        let handle = unsafe {
            let mut p: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = std::mem::zeroed();
            p.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
            p.deviceType = Device::KIND;
            p.device = device.as_raw();
            p.apiVersion = NVENCAPI_VERSION;
            let mut handle: Handle = std::ptr::null_mut();
            let s = open(&mut p, &mut handle);
            if s != NV_ENC_SUCCESS || handle.is_null() {
                let e = status("opening an NVENC session", s, handle, api);
                if !handle.is_null()
                    && let Some(d) = api.list.nvEncDestroyEncoder
                {
                    d(handle);
                }
                return Err(e);
            }
            handle
        };
        Ok(Session {
            api,
            handle,
            slots: Vec::new(),
            size: (0, 0),
            format: NV_ENC_BUFFER_FORMAT_NV12,
            sample: 1,
            _device: device,
        })
    }

    pub fn caps(&self, codec: Codec) -> Result<Caps, String> {
        let f = need(self.api.list.nvEncGetEncodeCaps, "nvEncGetEncodeCaps")?;
        let get = |cap: u32| -> Result<u32, String> {
            unsafe {
                let mut p: NV_ENC_CAPS_PARAM = std::mem::zeroed();
                p.version = NV_ENC_CAPS_PARAM_VER;
                p.capsToQuery = cap;
                let mut v = 0;
                let s = f(self.handle, codec_guid(codec), &mut p, &mut v);
                if s != NV_ENC_SUCCESS {
                    return Err(status(
                        &format!("asking NVENC about {codec:?}"),
                        s,
                        self.handle,
                        self.api,
                    ));
                }
                Ok(v.max(0) as u32)
            }
        };
        Ok(Caps {
            ten_bit: get(NV_ENC_CAPS_SUPPORT_10BIT_ENCODE).unwrap_or(0) != 0,
            max_b_frames: get(NV_ENC_CAPS_NUM_MAX_BFRAMES)?,
            min_size: (get(NV_ENC_CAPS_WIDTH_MIN)?, get(NV_ENC_CAPS_HEIGHT_MIN)?),
            max_size: (get(NV_ENC_CAPS_WIDTH_MAX)?, get(NV_ENC_CAPS_HEIGHT_MAX)?),
        })
    }

    pub fn initialize(&mut self, p: &Setup) -> Result<(), String> {
        (self.format, self.sample) = if p.ten_bit {
            (NV_ENC_BUFFER_FORMAT_YUV420_10BIT, 2)
        } else {
            (NV_ENC_BUFFER_FORMAT_NV12, 1)
        };
        let preset_fn = need(
            self.api.list.nvEncGetEncodePresetConfigEx,
            "nvEncGetEncodePresetConfigEx",
        )?;
        let init_fn = need(
            self.api.list.nvEncInitializeEncoder,
            "nvEncInitializeEncoder",
        )?;
        let guid = codec_guid(p.codec);
        unsafe {
            let mut preset: Box<NV_ENC_PRESET_CONFIG> = Box::new(std::mem::zeroed());
            preset.version = NV_ENC_PRESET_CONFIG_VER;
            preset.presetCfg.version = NV_ENC_CONFIG_VER;
            let s = preset_fn(
                self.handle,
                guid,
                NV_ENC_PRESET_P4_GUID,
                NV_ENC_TUNING_INFO_HIGH_QUALITY,
                &mut *preset,
            );
            if s != NV_ENC_SUCCESS {
                return Err(status("reading the NVENC preset", s, self.handle, self.api));
            }
            let mut cfg: Box<NV_ENC_CONFIG> = Box::new(preset.presetCfg);
            cfg.version = NV_ENC_CONFIG_VER;
            cfg.profileGUID = p.profile;
            cfg.gopLength = p.gop;
            cfg.frameIntervalP = p.b_frames as i32 + 1;
            rate_control(&mut cfg.rcParams, p.rate);
            match p.codec {
                Codec::Hevc => {
                    let h = &mut cfg.encodeCodecConfig.hevcConfig;
                    h.level = p.level;
                    h.tier = NV_ENC_TIER_HEVC_MAIN;
                    h.minCUSize = NV_ENC_HEVC_CUSIZE_AUTOSELECT;
                    h.maxCUSize = NV_ENC_HEVC_CUSIZE_AUTOSELECT;
                    h.idrPeriod = p.gop;
                    h.flags &= !(HEVC_OUTPUT_AUD
                        | HEVC_DISABLE_SPSPPS
                        | HEVC_REPEAT_SPSPPS
                        | HEVC_ENABLE_INTRA_REFRESH
                        | HEVC_CHROMA_FORMAT_MASK
                        | HEVC_PIXEL_BIT_DEPTH_MASK);
                    h.flags |= 1 << HEVC_CHROMA_FORMAT_SHIFT;
                    if p.ten_bit {
                        h.flags |= 2 << HEVC_PIXEL_BIT_DEPTH_SHIFT;
                    }
                    h.maxNumRefFramesInDPB = 0;
                    h.useBFramesAsRef = NV_ENC_BFRAME_REF_MODE_DISABLED;
                }
                _ => {
                    let h = &mut cfg.encodeCodecConfig.h264Config;
                    h.level = p.level;
                    h.idrPeriod = p.gop;
                    h.flags &= !(H264_OUTPUT_AUD | H264_REPEAT_SPSPPS);
                    h.entropyCodingMode = if p.profile == NV_ENC_H264_PROFILE_BASELINE_GUID {
                        NV_ENC_H264_ENTROPY_CODING_MODE_CAVLC
                    } else {
                        NV_ENC_H264_ENTROPY_CODING_MODE_CABAC
                    };
                    h.maxNumRefFrames = 0;
                    h.useBFramesAsRef = NV_ENC_BFRAME_REF_MODE_DISABLED;
                    let vui = &mut h.h264VUIParameters;
                    vui.timingInfoPresentFlag = 1;
                    vui.numUnitInTicks = p.fps.1;
                    vui.timeScale = p.fps.0.saturating_mul(2);
                }
            }
            let mut init: Box<NV_ENC_INITIALIZE_PARAMS> = Box::new(std::mem::zeroed());
            init.version = NV_ENC_INITIALIZE_PARAMS_VER;
            init.encodeGUID = guid;
            init.presetGUID = NV_ENC_PRESET_P4_GUID;
            init.encodeWidth = p.size.0;
            init.encodeHeight = p.size.1;
            init.darWidth = p.size.0;
            init.darHeight = p.size.1;
            init.frameRateNum = p.fps.0;
            init.frameRateDen = p.fps.1;
            init.enablePTD = 1;
            init.encodeConfig = &mut *cfg;
            init.tuningInfo = NV_ENC_TUNING_INFO_HIGH_QUALITY;
            init.bufferFormat = self.format;
            let s = init_fn(self.handle, &mut *init);
            if s != NV_ENC_SUCCESS {
                return Err(status("initialising NVENC", s, self.handle, self.api));
            }
        }
        self.size = p.size;
        let coded = (p.size.0 as usize * p.size.1 as usize * self.sample * 3 / 2).max(1 << 20);
        let coded = u32::try_from(coded).map_err(|_| "picture too large for NVENC".to_string())?;
        for _ in 0..p.slots.max(1) {
            self.add_slot(coded)?;
        }
        Ok(())
    }

    fn add_slot(&mut self, coded: u32) -> Result<(), String> {
        let create_in = need(
            self.api.list.nvEncCreateInputBuffer,
            "nvEncCreateInputBuffer",
        )?;
        let create_out = need(
            self.api.list.nvEncCreateBitstreamBuffer,
            "nvEncCreateBitstreamBuffer",
        )?;
        unsafe {
            let mut i: NV_ENC_CREATE_INPUT_BUFFER = std::mem::zeroed();
            i.version = NV_ENC_CREATE_INPUT_BUFFER_VER;
            i.width = self.size.0;
            i.height = self.size.1;
            i.memoryHeap = NV_ENC_MEMORY_HEAP_SYSMEM_CACHED;
            i.bufferFmt = self.format;
            let s = create_in(self.handle, &mut i);
            if s != NV_ENC_SUCCESS {
                return Err(status(
                    "creating an NVENC input buffer",
                    s,
                    self.handle,
                    self.api,
                ));
            }
            let mut o: NV_ENC_CREATE_BITSTREAM_BUFFER = std::mem::zeroed();
            o.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
            o.size = coded;
            o.memoryHeap = NV_ENC_MEMORY_HEAP_SYSMEM_CACHED;
            let s = create_out(self.handle, &mut o);
            if s != NV_ENC_SUCCESS {
                if let Some(d) = self.api.list.nvEncDestroyInputBuffer {
                    d(self.handle, i.inputBuffer);
                }
                return Err(status(
                    "creating an NVENC output buffer",
                    s,
                    self.handle,
                    self.api,
                ));
            }
            self.slots.push(Slot {
                input: i.inputBuffer,
                output: o.bitstreamBuffer,
            });
        }
        Ok(())
    }

    pub fn slots(&self) -> usize {
        self.slots.len()
    }

    pub fn sequence_params(&self) -> Result<Vec<u8>, String> {
        let f = need(
            self.api.list.nvEncGetSequenceParams,
            "nvEncGetSequenceParams",
        )?;
        let mut buf = vec![0u8; 1024];
        let mut len = 0u32;
        unsafe {
            let mut p: NV_ENC_SEQUENCE_PARAM_PAYLOAD = std::mem::zeroed();
            p.version = NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER;
            p.inBufferSize = buf.len() as u32;
            p.spsppsBuffer = buf.as_mut_ptr().cast();
            p.outSPSPPSPayloadSize = &mut len;
            let s = f(self.handle, &mut p);
            if s != NV_ENC_SUCCESS {
                return Err(status(
                    "reading the NVENC parameter sets",
                    s,
                    self.handle,
                    self.api,
                ));
            }
        }
        buf.truncate(len as usize);
        Ok(buf)
    }

    pub fn submit(
        &mut self,
        slot: usize,
        pts: i64,
        fill: impl FnOnce(Locked<'_>),
    ) -> Result<bool, String> {
        let s = self.slots.get(slot).ok_or("no such NVENC buffer")?;
        let (input, output) = (s.input, s.output);
        let lock = need(self.api.list.nvEncLockInputBuffer, "nvEncLockInputBuffer")?;
        let unlock = need(
            self.api.list.nvEncUnlockInputBuffer,
            "nvEncUnlockInputBuffer",
        )?;
        let encode = need(self.api.list.nvEncEncodePicture, "nvEncEncodePicture")?;
        let (w, h) = self.size;
        let pitch = unsafe {
            let mut l: NV_ENC_LOCK_INPUT_BUFFER = std::mem::zeroed();
            l.version = NV_ENC_LOCK_INPUT_BUFFER_VER;
            l.inputBuffer = input;
            let st = lock(self.handle, &mut l);
            if st != NV_ENC_SUCCESS || l.bufferDataPtr.is_null() {
                return Err(status(
                    "locking an NVENC input buffer",
                    st,
                    self.handle,
                    self.api,
                ));
            }
            let pitch = l.pitch as usize;
            if pitch < w as usize * self.sample {
                unlock(self.handle, input);
                return Err(format!(
                    "NVENC gave an input pitch of {pitch} bytes for {w}-pixel rows"
                ));
            }
            let len = pitch * h as usize * 3 / 2;
            fill(Locked {
                data: std::slice::from_raw_parts_mut(l.bufferDataPtr.cast::<u8>(), len),
                pitch,
            });
            let st = unlock(self.handle, input);
            if st != NV_ENC_SUCCESS {
                return Err(status(
                    "unlocking an NVENC input buffer",
                    st,
                    self.handle,
                    self.api,
                ));
            }
            l.pitch
        };
        let st = unsafe {
            let mut e: NV_ENC_PIC_PARAMS = std::mem::zeroed();
            e.version = NV_ENC_PIC_PARAMS_VER;
            e.inputWidth = w;
            e.inputHeight = h;
            e.inputPitch = pitch;
            e.inputBuffer = input;
            e.outputBitstream = output;
            e.bufferFmt = self.format;
            e.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
            e.inputTimeStamp = pts as u64;
            e.inputDuration = 1;
            encode(self.handle, &mut e)
        };
        match st {
            NV_ENC_SUCCESS => Ok(true),
            NV_ENC_ERR_NEED_MORE_INPUT => Ok(false),
            s => Err(status("encoding a picture", s, self.handle, self.api)),
        }
    }

    pub fn end_of_stream(&mut self) -> Result<(), String> {
        let encode = need(self.api.list.nvEncEncodePicture, "nvEncEncodePicture")?;
        let st = unsafe {
            let mut e: NV_ENC_PIC_PARAMS = std::mem::zeroed();
            e.version = NV_ENC_PIC_PARAMS_VER;
            e.encodePicFlags = NV_ENC_PIC_FLAG_EOS;
            encode(self.handle, &mut e)
        };
        if st != NV_ENC_SUCCESS {
            return Err(status("ending the stream", st, self.handle, self.api));
        }
        Ok(())
    }

    pub fn read<R>(
        &mut self,
        slot: usize,
        take: impl FnOnce(&[u8], i64, bool) -> R,
    ) -> Result<R, String> {
        let output = self.slots.get(slot).ok_or("no such NVENC buffer")?.output;
        let lock = need(self.api.list.nvEncLockBitstream, "nvEncLockBitstream")?;
        let unlock = need(self.api.list.nvEncUnlockBitstream, "nvEncUnlockBitstream")?;
        unsafe {
            let mut l: NV_ENC_LOCK_BITSTREAM = std::mem::zeroed();
            l.version = NV_ENC_LOCK_BITSTREAM_VER;
            l.outputBitstream = output;
            let st = lock(self.handle, &mut l);
            if st != NV_ENC_SUCCESS {
                return Err(status(
                    "reading an NVENC bitstream",
                    st,
                    self.handle,
                    self.api,
                ));
            }
            let n = l.bitstreamSizeInBytes as usize;
            let bytes = if l.bitstreamBufferPtr.is_null() || n == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts(l.bitstreamBufferPtr.cast::<u8>(), n)
            };
            let out = take(
                bytes,
                l.outputTimeStamp as i64,
                l.pictureType == NV_ENC_PIC_TYPE_IDR,
            );
            let st = unlock(self.handle, output);
            if st != NV_ENC_SUCCESS {
                return Err(status(
                    "releasing an NVENC bitstream",
                    st,
                    self.handle,
                    self.api,
                ));
            }
            Ok(out)
        }
    }
}

fn rate_control(rc: &mut NV_ENC_RC_PARAMS, rate: Rate) {
    rc.version = NV_ENC_RC_PARAMS_VER;
    rc.multiPass = NV_ENC_MULTI_PASS_DISABLED;
    rc.flags &= !RC_ENABLE_LOOKAHEAD;
    rc.lookaheadDepth = 0;
    rc.averageBitRate = 0;
    rc.maxBitRate = 0;
    rc.vbvBufferSize = 0;
    rc.vbvInitialDelay = 0;
    match rate {
        Rate::Crf(n) => {
            rc.rateControlMode = NV_ENC_PARAMS_RC_VBR;
            rc.targetQuality = n.min(51);
            rc.targetQualityLSB = 0;
        }
        Rate::Qp(q) => {
            let q = u32::from(q.min(51));
            rc.rateControlMode = NV_ENC_PARAMS_RC_CONSTQP;
            rc.constQP = NV_ENC_QP {
                qpInterP: q,
                qpInterB: q,
                qpIntra: q,
            };
        }
        Rate::Bitrate(kbps) => {
            let bps = kbps.max(1).saturating_mul(1000);
            rc.rateControlMode = NV_ENC_PARAMS_RC_CBR;
            rc.averageBitRate = bps;
            rc.maxBitRate = bps;
            rc.vbvBufferSize = bps;
            rc.vbvInitialDelay = bps;
        }
        Rate::Vbr { kbps, max_kbps } => {
            let peak = max_kbps.max(kbps).max(1).saturating_mul(1000);
            rc.rateControlMode = NV_ENC_PARAMS_RC_VBR;
            rc.averageBitRate = kbps.max(1).saturating_mul(1000);
            rc.maxBitRate = peak;
            rc.vbvBufferSize = peak;
            rc.vbvInitialDelay = peak;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            for s in self.slots.drain(..) {
                if let Some(d) = self.api.list.nvEncDestroyInputBuffer {
                    d(self.handle, s.input);
                }
                if let Some(d) = self.api.list.nvEncDestroyBitstreamBuffer {
                    d(self.handle, s.output);
                }
            }
            if let Some(d) = self.api.list.nvEncDestroyEncoder {
                d(self.handle);
            }
        }
    }
}
