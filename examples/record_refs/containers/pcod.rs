use super::mux::{self, Csp, Timed};
use crate::{Def, OURS, sha, tool, words};
use pito_codecs::mp4;

#[allow(dead_code)]
#[path = "../../../src/pcod/refs.rs"]
mod refs;

use crate::media::refs as media_refs;

use refs::AREA;

fn tone(i: u64, _: u16, rate: u32) -> f32 {
    (0.01 * (std::f64::consts::TAU * 440.0 * i as f64 / f64::from(rate)).sin()) as f32
}

fn tone_source() -> Vec<f32> {
    crate::testref::pcm(
        crate::testref::Sound::Fn(tone),
        refs::TONE_RATE,
        2,
        0,
        refs::TONE_FRAMES,
    )
}

fn vorbis_tone(i: u64, ch: u16, rate: u32) -> f32 {
    let (amp, hz) = if ch == 0 { (0.3, 440.0) } else { (0.2, 660.0) };
    (amp * (std::f64::consts::TAU * hz * i as f64 / f64::from(rate)).sin()) as f32
}

fn vorbis_source() -> Vec<f32> {
    crate::testref::pcm(
        crate::testref::Sound::Fn(vorbis_tone),
        refs::VORBIS_RATE,
        2,
        0,
        refs::VORBIS_FRAMES,
    )
}

fn text<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn s16le(samples: &[f32]) -> Vec<u8> {
    crate::testref::s16_of(samples)
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect()
}

fn tones(defs: &mut Vec<Def>) {
    let raw = || Ok(s16le(&tone_source()));
    for (file, _, codec) in refs::TONES {
        let def = match codec {
            "flac" => Def::fixture(
                AREA,
                file.into(),
                "flac",
                tool(
                    "flac",
                    &[
                        "--force-raw-format",
                        "--endian=little",
                        "--sign=signed",
                        "--channels=2",
                        "--bps=16",
                        "--sample-rate=48000",
                        "--silent",
                        "--force",
                        "-o",
                        "{out}",
                        "{src}",
                    ],
                ),
            )
            .from(raw),
            "mp3" => Def::fixture(
                AREA,
                file.into(),
                "lame",
                tool(
                    "lame",
                    &[
                        "-r",
                        "-s",
                        "48",
                        "--resample",
                        "48",
                        "--bitwidth",
                        "16",
                        "--little-endian",
                        "-m",
                        "j",
                        "--cbr",
                        "-b",
                        "32",
                        "--quiet",
                        "{src}",
                        "{out}",
                    ],
                ),
            )
            .from(raw),
            _ => Def::fixture(
                AREA,
                file.into(),
                OURS,
                words("the tone as uncompressed (escape) ALAC frames in our MP4 writer"),
            )
            .from(|| {
                let pcm = crate::testref::s16_of(&tone_source());
                mux::alac_m4a(&pcm, refs::TONE_RATE)
            }),
        };
        defs.push(def);
    }
}

fn shapes(defs: &mut Vec<Def>) {
    let size = refs::PROBED_SIZE;
    for (file, chroma, bits, interlaced) in refs::PROBE_SHAPES {
        let c = if chroma == 2 { Csp::I422 } else { Csp::I420 };
        let depth = bits.to_string();
        let mut extra = vec!["--crf", "35", "--output-depth", &depth];
        if interlaced {
            extra.push("--tff");
        } else {
            extra.extend_from_slice(&["--output-csp", c.x26x(), "--profile", "high422"]);
        }
        defs.extend(super::remux::x264_mp4(AREA, file, 30, &extra, move || {
            Ok(mux::y4m(
                size,
                (30, 1),
                c,
                bits,
                refs::PROBED_FRAMES,
                &mux::pattern(size),
            ))
        }));
    }
}

#[repr(C)]
struct OggPacket {
    packet: *mut u8,
    bytes: std::ffi::c_long,
    b_o_s: std::ffi::c_long,
    e_o_s: std::ffi::c_long,
    granulepos: i64,
    packetno: i64,
}

impl OggPacket {
    fn empty() -> OggPacket {
        OggPacket {
            packet: std::ptr::null_mut(),
            bytes: 0,
            b_o_s: 0,
            e_o_s: 0,
            granulepos: 0,
            packetno: 0,
        }
    }

    fn take(&self) -> (Vec<u8>, i64) {
        let data = if self.packet.is_null() || self.bytes <= 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(self.packet, self.bytes as usize) }.to_vec()
        };
        (data, self.granulepos)
    }
}

fn library(name: &str) -> Result<libloading::Library, String> {
    let pin = crate::pins()?.pin(name)?;
    let crate::testref::Probe::Library { path, .. } = &pin.probe else {
        return Err(format!("{name} is pinned as a command"));
    };
    unsafe { libloading::Library::new(path) }.map_err(|e| format!("{path}: {e}"))
}

fn vorbis_packets() -> Result<Vec<(Vec<u8>, i64)>, String> {
    use std::ffi::{c_int, c_long, c_void};
    let lib = library("libvorbisenc")?;
    macro_rules! sym {
        ($name:literal, $ty:ty) => {
            *unsafe { lib.get::<$ty>($name) }.map_err(|e| format!("libvorbisenc: {e}"))?
        };
    }
    type V = *mut c_void;
    let info_init = sym!(b"vorbis_info_init", unsafe extern "C" fn(V));
    let encode_init = sym!(
        b"vorbis_encode_init_vbr",
        unsafe extern "C" fn(V, c_long, c_long, f32) -> c_int
    );
    let comment_init = sym!(b"vorbis_comment_init", unsafe extern "C" fn(V));
    let analysis_init = sym!(b"vorbis_analysis_init", unsafe extern "C" fn(V, V) -> c_int);
    let block_init = sym!(b"vorbis_block_init", unsafe extern "C" fn(V, V) -> c_int);
    let headerout = sym!(
        b"vorbis_analysis_headerout",
        unsafe extern "C" fn(V, V, *mut OggPacket, *mut OggPacket, *mut OggPacket) -> c_int
    );
    let buffer = sym!(
        b"vorbis_analysis_buffer",
        unsafe extern "C" fn(V, c_int) -> *mut *mut f32
    );
    let wrote = sym!(
        b"vorbis_analysis_wrote",
        unsafe extern "C" fn(V, c_int) -> c_int
    );
    let blockout = sym!(
        b"vorbis_analysis_blockout",
        unsafe extern "C" fn(V, V) -> c_int
    );
    let analysis = sym!(
        b"vorbis_analysis",
        unsafe extern "C" fn(V, *mut OggPacket) -> c_int
    );
    let addblock = sym!(b"vorbis_bitrate_addblock", unsafe extern "C" fn(V) -> c_int);
    let flush = sym!(
        b"vorbis_bitrate_flushpacket",
        unsafe extern "C" fn(V, *mut OggPacket) -> c_int
    );
    let block_clear = sym!(b"vorbis_block_clear", unsafe extern "C" fn(V) -> c_int);
    let dsp_clear = sym!(b"vorbis_dsp_clear", unsafe extern "C" fn(V));
    let comment_clear = sym!(b"vorbis_comment_clear", unsafe extern "C" fn(V));
    let info_clear = sym!(b"vorbis_info_clear", unsafe extern "C" fn(V));
    let mut vi = vec![0u64; 512];
    let mut vc = vec![0u64; 512];
    let mut vd = vec![0u64; 2048];
    let mut vb = vec![0u64; 2048];
    let (vi, vc, vd, vb) = (
        vi.as_mut_ptr().cast::<c_void>(),
        vc.as_mut_ptr().cast::<c_void>(),
        vd.as_mut_ptr().cast::<c_void>(),
        vb.as_mut_ptr().cast::<c_void>(),
    );
    let rate = refs::VORBIS_RATE;
    let source = vorbis_source();
    let mut out = Vec::new();
    unsafe {
        info_init(vi);
        if encode_init(vi, 2, c_long::from(rate as i32), refs::VORBIS_QUALITY) != 0 {
            info_clear(vi);
            return Err("vorbis_encode_init_vbr refused the settings".into());
        }
        comment_init(vc);
        analysis_init(vd, vi);
        block_init(vd, vb);
        let (mut a, mut b, mut c) = (OggPacket::empty(), OggPacket::empty(), OggPacket::empty());
        headerout(vd, vc, &mut a, &mut b, &mut c);
        out.extend([a.take(), b.take(), c.take()]);
        let mut fed = 0;
        let frames = source.len() / 2;
        loop {
            let n = (frames - fed).min(1024);
            if n > 0 {
                let planes = buffer(vd, n as c_int);
                let (l, r) = (*planes, *planes.add(1));
                for i in 0..n {
                    *l.add(i) = source[(fed + i) * 2];
                    *r.add(i) = source[(fed + i) * 2 + 1];
                }
            }
            wrote(vd, n as c_int);
            fed += n;
            while blockout(vd, vb) == 1 {
                analysis(vb, std::ptr::null_mut());
                addblock(vb);
                let mut op = OggPacket::empty();
                while flush(vd, &mut op) == 1 {
                    out.push(op.take());
                }
            }
            if n == 0 {
                break;
            }
        }
        block_clear(vb);
        dsp_clear(vd);
        comment_clear(vc);
        info_clear(vi);
    }
    Ok(out)
}

fn vorbis_webm() -> Result<Vec<u8>, String> {
    use media_refs::{MkvBlock, MkvTrack, Sizes};
    let packets = vorbis_packets()?;
    let headers: Vec<Vec<u8>> = packets.iter().take(3).map(|p| p.0.clone()).collect();
    let dec = pito_codecs::vorbis::Decoder::from_headers(&headers[0], &headers[1], &headers[2])
        .map_err(text)?;
    let rate = f64::from(refs::VORBIS_RATE);
    let mut blocks = Vec::new();
    let (mut decoded, mut prev, mut done) = (0i64, None, 0i64);
    for (data, granule) in &packets[3..] {
        let n = dec.blocksize(data).map_err(|e| format!("{e:?}"))? as i64;
        if let Some(p) = prev {
            decoded += p / 4 + n / 4;
        }
        prev = Some(n);
        blocks.push(MkvBlock {
            track: 0,
            ms: (done as f64 * 1000.0 / rate).round() as i64,
            key: true,
            data: data.clone(),
            discard_ns: 0,
        });
        done = done.max(*granule);
    }
    let extra = decoded - done;
    if let Some(last) = blocks.last_mut() {
        last.discard_ns = (extra.max(0) as f64 * 1e9 / rate).round() as i64;
    }
    let track = MkvTrack {
        codec: "A_VORBIS",
        private: mux::xiph_private(&headers),
        rate,
        channels: 2,
        ..MkvTrack::default()
    };
    let ms = done as f64 * 1000.0 / rate;
    Ok(media_refs::matroska(
        "webm",
        &[track],
        &blocks,
        ms,
        Sizes::Known,
    ))
}

fn vorbisfile_slice() -> Result<Vec<u8>, String> {
    use std::ffi::{c_char, c_int, c_long, c_void};
    let lib = library("libvorbisfile")?;
    type F = *mut c_void;
    let fopen = *unsafe { lib.get::<unsafe extern "C" fn(*const c_char, F) -> c_int>(b"ov_fopen") }
        .map_err(|e| format!("libvorbisfile: {e}"))?;
    let read = *unsafe {
        lib.get::<unsafe extern "C" fn(
                F,
                *mut c_char,
                c_int,
                c_int,
                c_int,
                c_int,
                *mut c_int,
            ) -> c_long>(b"ov_read")
    }
    .map_err(|e| format!("libvorbisfile: {e}"))?;
    let clear = *unsafe { lib.get::<unsafe extern "C" fn(F) -> c_int>(b"ov_clear") }
        .map_err(|e| format!("libvorbisfile: {e}"))?;
    let path = mux::work(AREA, refs::VORBIS_FILES[0]);
    let name = std::ffi::CString::new(path.to_string_lossy().into_owned()).map_err(text)?;
    let mut vf = vec![0u64; 4096];
    let vf = vf.as_mut_ptr().cast::<c_void>();
    if unsafe { fopen(name.as_ptr(), vf) } != 0 {
        return Err(format!("ov_fopen refused {}", path.display()));
    }
    let mut pcm = Vec::new();
    let mut buf = vec![0u8; 8192];
    let mut section: c_int = 0;
    let failed = loop {
        let n = unsafe {
            read(
                vf,
                buf.as_mut_ptr().cast(),
                buf.len() as c_int,
                0,
                2,
                1,
                &mut section,
            )
        };
        if n < 0 {
            break Some(n);
        }
        if n == 0 {
            break None;
        }
        pcm.extend_from_slice(&buf[..n as usize]);
    };
    unsafe { clear(vf) };
    if let Some(n) = failed {
        return Err(format!("ov_read failed with {n}"));
    }
    let (start, end) = refs::VORBIS_SLICE;
    pcm.get(start as usize * 4..end as usize * 4)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| format!("libvorbisfile decoded only {} frames", pcm.len() / 4))
}

fn vorbis(defs: &mut Vec<Def>) {
    defs.push(
        Def::fixture(
            AREA,
            refs::VORBIS_FILES[0].into(),
            OURS,
            words("libvorbisenc at quality 0.3 of 0.7 s of 440 and 660 Hz, in our Ogg pages"),
        )
        .from(|| Ok(mux::ogg(&vorbis_packets()?, 0x7069_746f)))
        .library("libvorbisenc"),
    );
    defs.push(
        Def::fixture(
            AREA,
            refs::VORBIS_FILES[1].into(),
            OURS,
            words("the same libvorbisenc packets in examples/record_refs WebM, the last block's DiscardPadding ending the decode at the final granule"),
        )
        .from(vorbis_webm)
        .library("libvorbisenc"),
    );
    let (start, end) = refs::VORBIS_SLICE;
    let what = format!(
        "libvorbisfile's 16-bit decode of {{ref:{}}}, frames {start} to {end}",
        refs::VORBIS_FILES[0]
    );
    defs.push(
        Def::tolerance(AREA, refs::vorbis_slice_name(), OURS, words(&what))
            .from(vorbisfile_slice)
            .library("libvorbisfile"),
    );
}

fn smoke(defs: &mut Vec<Def>) {
    let size = refs::SMOKE_SIZE;
    let stream = format!("{}.hevc", refs::SMOKE_HEVC);
    let res = format!("{}x{}", size.0, size.1);
    let n = refs::SMOKE_FRAMES.to_string();
    defs.push(
        Def::digest(
            AREA,
            stream.clone(),
            "x265",
            tool(
                "x265",
                &[
                    "--input",
                    "{src}",
                    "--input-res",
                    &res,
                    "--fps",
                    "30",
                    "--input-csp",
                    "i420",
                    "--frames",
                    &n,
                    "--preset",
                    "ultrafast",
                    "--bframes",
                    "0",
                    "--crf",
                    "36",
                    "--frame-threads",
                    "1",
                    "--no-wpp",
                    "--pools",
                    "none",
                    "--log-level",
                    "error",
                    "--no-info",
                    "--no-progress",
                    "--output",
                    "{out}",
                ],
            ),
            sha,
        )
        .from(move || {
            Ok(mux::raw_video(
                size,
                Csp::I420,
                8,
                refs::SMOKE_FRAMES,
                &mux::pattern(size),
            ))
        }),
    );
    let what = format!("{{ref:{stream}}} in our MP4 writer as hvc1");
    defs.push(
        Def::fixture(AREA, refs::SMOKE_HEVC.into(), OURS, words(&what)).from(move || {
            let coded = mux::hevc(&mux::read(AREA, &stream)?)?;
            Timed {
                coded: &coded,
                pocs: &[],
                size,
                fps: 30,
            }
            .hevc_mp4(mp4::Brand::Mp4)
        }),
    );
}

pub fn defs() -> Vec<Def> {
    let mut defs = Vec::new();
    tones(&mut defs);
    shapes(&mut defs);
    vorbis(&mut defs);
    smoke(&mut defs);
    defs.push(Def::ours(
        AREA,
        refs::GIF_TICKS.into(),
        "src/pcod/refs.rs ticks_gif()",
        refs::ticks_gif,
    ));
    for input in refs::WEBP_INPUTS {
        let webp = format!("images-{input}.webp");
        defs.push(
            Def::digest(
                AREA,
                webp.clone(),
                "magick",
                tool(
                    "magick",
                    &[
                        "png:{src}",
                        "-quality",
                        "75",
                        "-define",
                        "webp:lossless=true",
                        "-define",
                        "webp:method=6",
                        "webp:{out}",
                    ],
                ),
                sha,
            )
            .from(move || refs::webp_png(input)),
        );
        let what = format!("the byte count of {{ref:{webp}}}");
        defs.push(
            Def::exact(AREA, refs::webp_size_name(input), OURS, words(&what))
                .from(move || Ok(format!("{}\n", mux::read(AREA, &webp)?.len()).into_bytes())),
        );
    }
    defs
}
