use crate::audio::{Bytes, cut, f32_bytes, lib_path, mux};
use crate::testref::{self, Kind};
use crate::{Def, OURS, Take, sha, words};
use std::path::Path;

#[allow(dead_code)]
#[path = "../../src/vorbis/refs.rs"]
mod refs;
#[allow(dead_code)]
#[path = "../../src/s16/refs.rs"]
mod s16;

use refs::{AREA, Case, End};

const ORDER: [&[usize]; 8] = [
    &[0],
    &[0, 1],
    &[0, 2, 1],
    &[0, 1, 2, 3],
    &[0, 2, 1, 3, 4],
    &[0, 2, 1, 5, 3, 4],
    &[0, 2, 1, 6, 5, 3, 4],
    &[0, 2, 1, 7, 5, 6, 3, 4],
];

pub struct Encoded {
    pub headers: [Vec<u8>; 3],
    pub packets: Vec<(Vec<u8>, i64)>,
    pub rate: u32,
    pub channels: u16,
    pub nominal: i64,
}

fn source(c: &Case) -> Vec<Vec<f32>> {
    use std::f64::consts::TAU;
    let n = c.frames();
    (0..usize::from(c.channels))
        .map(|j| {
            let f = 220.0 * (j + 1) as f64 + 37.0;
            let a = 0.1 + 0.08 * j as f64;
            (0..n)
                .map(|i| {
                    let t = i as f64 / f64::from(c.rate);
                    let click = if c.clicks && (t + 0.043) % 0.31 < 0.0015 {
                        0.6 * (9000.0 * TAU * t).sin()
                    } else {
                        0.0
                    };
                    (a * (f * TAU * t).sin()
                        + 0.1 * (f * 7.3 * TAU * t).sin() * (-3.0 * t).exp()
                        + click) as f32
                })
                .collect()
        })
        .collect()
}

#[cfg(target_os = "linux")]
mod lib {
    use super::{Encoded, lib_path};
    use libloading::{Library, Symbol};
    use std::ffi::{CString, c_char, c_float, c_int, c_long, c_void};

    #[repr(C)]
    struct Packet {
        packet: *mut u8,
        bytes: c_long,
        b_o_s: c_long,
        e_o_s: c_long,
        granulepos: i64,
        packetno: i64,
    }

    impl Packet {
        fn empty() -> Packet {
            Packet {
                packet: std::ptr::null_mut(),
                bytes: 0,
                b_o_s: 0,
                e_o_s: 0,
                granulepos: 0,
                packetno: 0,
            }
        }

        fn data(&self) -> Vec<u8> {
            if self.packet.is_null() || self.bytes <= 0 {
                return Vec::new();
            }
            unsafe { std::slice::from_raw_parts(self.packet, self.bytes as usize) }.to_vec()
        }
    }

    fn open(name: &str) -> Result<Library, String> {
        crate::lib_version(name)?;
        let path = lib_path(name)?;
        unsafe { Library::new(&path) }.map_err(|e| format!("{path}: {e}"))
    }

    fn sym<'a, T>(lib: &'a Library, name: &str) -> Result<Symbol<'a, T>, String> {
        unsafe { lib.get::<T>(name.as_bytes()) }.map_err(|e| format!("no {name}: {e}"))
    }

    type Ptr = *mut c_void;

    pub fn encode(
        planes: &[Vec<f32>],
        rate: u32,
        quality: f32,
        title: &str,
    ) -> Result<Encoded, String> {
        let lib = open("libvorbisenc")?;
        let channels = planes.len();
        let frames = planes.first().map_or(0, Vec::len);
        let mut vi = vec![0u64; 64];
        let mut vc = vec![0u64; 16];
        let mut vd = vec![0u64; 256];
        let mut vb = vec![0u64; 256];
        let (vi, vc, vd, vb) = (
            vi.as_mut_ptr() as Ptr,
            vc.as_mut_ptr() as Ptr,
            vd.as_mut_ptr() as Ptr,
            vb.as_mut_ptr() as Ptr,
        );
        unsafe {
            let info_init: Symbol<unsafe extern "C" fn(Ptr)> = sym(&lib, "vorbis_info_init")?;
            let init_vbr: Symbol<unsafe extern "C" fn(Ptr, c_long, c_long, c_float) -> c_int> =
                sym(&lib, "vorbis_encode_init_vbr")?;
            let comment_init: Symbol<unsafe extern "C" fn(Ptr)> = sym(&lib, "vorbis_comment_init")?;
            let add_tag: Symbol<unsafe extern "C" fn(Ptr, *const c_char, *const c_char)> =
                sym(&lib, "vorbis_comment_add_tag")?;
            let analysis_init: Symbol<unsafe extern "C" fn(Ptr, Ptr) -> c_int> =
                sym(&lib, "vorbis_analysis_init")?;
            let block_init: Symbol<unsafe extern "C" fn(Ptr, Ptr) -> c_int> =
                sym(&lib, "vorbis_block_init")?;
            let headerout: Symbol<
                unsafe extern "C" fn(Ptr, Ptr, *mut Packet, *mut Packet, *mut Packet) -> c_int,
            > = sym(&lib, "vorbis_analysis_headerout")?;
            let buffer: Symbol<unsafe extern "C" fn(Ptr, c_int) -> *mut *mut f32> =
                sym(&lib, "vorbis_analysis_buffer")?;
            let wrote: Symbol<unsafe extern "C" fn(Ptr, c_int) -> c_int> =
                sym(&lib, "vorbis_analysis_wrote")?;
            let blockout: Symbol<unsafe extern "C" fn(Ptr, Ptr) -> c_int> =
                sym(&lib, "vorbis_analysis_blockout")?;
            let analysis: Symbol<unsafe extern "C" fn(Ptr, *mut Packet) -> c_int> =
                sym(&lib, "vorbis_analysis")?;
            let addblock: Symbol<unsafe extern "C" fn(Ptr) -> c_int> =
                sym(&lib, "vorbis_bitrate_addblock")?;
            let flush: Symbol<unsafe extern "C" fn(Ptr, *mut Packet) -> c_int> =
                sym(&lib, "vorbis_bitrate_flushpacket")?;
            let block_clear: Symbol<unsafe extern "C" fn(Ptr) -> c_int> =
                sym(&lib, "vorbis_block_clear")?;
            let dsp_clear: Symbol<unsafe extern "C" fn(Ptr)> = sym(&lib, "vorbis_dsp_clear")?;
            let comment_clear: Symbol<unsafe extern "C" fn(Ptr)> =
                sym(&lib, "vorbis_comment_clear")?;
            let info_clear: Symbol<unsafe extern "C" fn(Ptr)> = sym(&lib, "vorbis_info_clear")?;
            info_init(vi);
            if init_vbr(vi, channels as c_long, c_long::from(rate), quality) != 0 {
                info_clear(vi);
                return Err(format!(
                    "libvorbisenc refused {channels} channels at {rate} Hz, quality {quality}"
                ));
            }
            let nominal = *(vi as *const c_long).add(3);
            comment_init(vc);
            let (k, v) = (
                CString::new("TITLE").unwrap_or_default(),
                CString::new(title).unwrap_or_default(),
            );
            add_tag(vc, k.as_ptr(), v.as_ptr());
            analysis_init(vd, vi);
            block_init(vd, vb);
            let (mut a, mut b, mut c) = (Packet::empty(), Packet::empty(), Packet::empty());
            headerout(vd, vc, &mut a, &mut b, &mut c);
            let headers = [a.data(), b.data(), c.data()];
            let mut packets = Vec::new();
            let drain = |packets: &mut Vec<(Vec<u8>, i64)>| {
                while blockout(vd, vb) == 1 {
                    analysis(vb, std::ptr::null_mut());
                    addblock(vb);
                    let mut op = Packet::empty();
                    while flush(vd, &mut op) == 1 {
                        packets.push((op.data(), op.granulepos));
                    }
                }
            };
            let mut at = 0;
            while at < frames {
                let n = (frames - at).min(1024);
                let buf = buffer(vd, n as c_int);
                for (ch, plane) in planes.iter().enumerate() {
                    let dst = std::slice::from_raw_parts_mut(*buf.add(ch), n);
                    dst.copy_from_slice(&plane[at..at + n]);
                }
                wrote(vd, n as c_int);
                drain(&mut packets);
                at += n;
            }
            wrote(vd, 0);
            drain(&mut packets);
            block_clear(vb);
            dsp_clear(vd);
            comment_clear(vc);
            info_clear(vi);
            Ok(Encoded {
                headers,
                packets,
                rate,
                channels: channels as u16,
                nominal,
            })
        }
    }

    pub fn decode(ogg: &std::path::Path) -> Result<(usize, Vec<f32>), String> {
        let lib = open("libvorbisfile")?;
        let path = CString::new(ogg.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let mut vf = vec![0u64; 1024];
        let vf = vf.as_mut_ptr() as Ptr;
        unsafe {
            let fopen: Symbol<unsafe extern "C" fn(*const c_char, Ptr) -> c_int> =
                sym(&lib, "ov_fopen")?;
            let info: Symbol<unsafe extern "C" fn(Ptr, c_int) -> *const c_int> =
                sym(&lib, "ov_info")?;
            let read: Symbol<
                unsafe extern "C" fn(Ptr, *mut *mut *mut f32, c_int, *mut c_int) -> c_long,
            > = sym(&lib, "ov_read_float")?;
            let clear: Symbol<unsafe extern "C" fn(Ptr) -> c_int> = sym(&lib, "ov_clear")?;
            if fopen(path.as_ptr(), vf) != 0 {
                return Err(format!("libvorbisfile could not open {}", ogg.display()));
            }
            let vi = info(vf, -1);
            if vi.is_null() {
                clear(vf);
                return Err("libvorbisfile gave no stream info".into());
            }
            let channels = *vi.add(1) as usize;
            let order = super::ORDER.get(channels.wrapping_sub(1)).copied();
            let mut out = Vec::new();
            let mut section: c_int = 0;
            loop {
                let mut pcm: *mut *mut f32 = std::ptr::null_mut();
                let n = read(vf, &mut pcm, 4096, &mut section);
                if n < 0 {
                    clear(vf);
                    return Err(format!("libvorbisfile read failed with {n}"));
                }
                if n == 0 {
                    break;
                }
                let planes: Vec<&[f32]> = (0..channels)
                    .map(|ch| std::slice::from_raw_parts(*pcm.add(ch), n as usize))
                    .collect();
                for i in 0..n as usize {
                    match order {
                        Some(o) => out.extend(o.iter().map(|&ch| planes[ch][i])),
                        None => out.extend(planes.iter().map(|p| p[i])),
                    }
                }
            }
            clear(vf);
            Ok((channels, out))
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod lib {
    use super::Encoded;

    pub fn encode(_: &[Vec<f32>], _: u32, _: f32, _: &str) -> Result<Encoded, String> {
        Err("libvorbisenc loads only on Linux".into())
    }

    pub fn decode(_: &std::path::Path) -> Result<(usize, Vec<f32>), String> {
        Err("libvorbisfile loads only on Linux".into())
    }
}

pub fn encode(c: &Case) -> Result<Encoded, String> {
    lib::encode(&source(c), c.rate, c.quality, c.name)
}

pub fn ogg(e: &Encoded, packets: usize) -> Vec<u8> {
    let crc = pito_codecs::opus::ogg::crc32;
    let mut out = Vec::new();
    let mut seq = 0u32;
    let mut page = |flags: u8, granule: i64, list: &[&[u8]], out: &mut Vec<u8>| {
        let mut lacing = Vec::new();
        let mut body = Vec::new();
        for p in list {
            lacing.extend(std::iter::repeat_n(255u8, p.len() / 255));
            lacing.push((p.len() % 255) as u8);
            body.extend_from_slice(p);
        }
        let mut pg = b"OggS\0".to_vec();
        pg.push(flags);
        pg.extend(granule.to_le_bytes());
        pg.extend(0x7069_746fu32.to_le_bytes());
        pg.extend(seq.to_le_bytes());
        pg.extend([0u8; 4]);
        pg.push(lacing.len() as u8);
        pg.extend(lacing);
        pg.extend(body);
        let sum = crc(&pg);
        pg[22..26].copy_from_slice(&sum.to_le_bytes());
        out.extend(pg);
        seq += 1;
    };
    page(0x02, 0, &[&e.headers[0]], &mut out);
    page(0, 0, &[&e.headers[1], &e.headers[2]], &mut out);
    let packets = &e.packets[..packets.min(e.packets.len())];
    let mut held: Vec<&[u8]> = Vec::new();
    let (mut segs, mut bytes) = (0, 0);
    for (k, (p, granule)) in packets.iter().enumerate() {
        held.push(p);
        segs += p.len() / 255 + 1;
        bytes += p.len();
        let last = k + 1 == packets.len();
        if last || bytes >= 4000 || segs >= 200 {
            page(if last { 0x04 } else { 0 }, *granule, &held, &mut out);
            held.clear();
            segs = 0;
            bytes = 0;
        }
    }
    out
}

fn xiph(headers: &[Vec<u8>; 3]) -> Vec<u8> {
    let mut out = vec![2u8];
    for h in &headers[..2] {
        out.extend(std::iter::repeat_n(255u8, h.len() / 255));
        out.push((h.len() % 255) as u8);
    }
    for h in headers {
        out.extend_from_slice(h);
    }
    out
}

fn timeline(e: &Encoded, packets: usize) -> Result<(Vec<i64>, i64, usize), String> {
    let [a, b, c] = &e.headers;
    let dec = pito_codecs::vorbis::Decoder::from_headers(a, b, c).map_err(|e| e.to_string())?;
    let packets = &e.packets[..packets.min(e.packets.len())];
    let sizes = packets
        .iter()
        .map(|(p, _)| dec.blocksize(p).map_err(|e| format!("{e:?}")))
        .collect::<Result<Vec<usize>, String>>()?;
    let first = packets.first().ok_or("no audio packets")?;
    let lead = dec.lead_in(&first.0).map_err(|e| format!("{e:?}"))?;
    let mut at = vec![0i64];
    let mut pos = lead as i64;
    for k in 1..sizes.len() {
        if k > 1 {
            pos += (sizes[k - 2] / 4 + sizes[k - 1] / 4) as i64;
        }
        at.push(pos);
    }
    let end = lead as i64
        + sizes
            .windows(2)
            .map(|w| (w[0] / 4 + w[1] / 4) as i64)
            .sum::<i64>();
    Ok((at, end, lead))
}

const MARK: [u8; 24] = [0xA5; 24];

fn matroska(e: &Encoded, frames: usize, webm: bool, cluster: f64) -> Bytes {
    let text = |e: pito_codecs::media::Error| e.to_string();
    let (at, end, lead) = timeline(e, e.packets.len())?;
    let opts = pito_codecs::mkv::Options {
        webm,
        cluster_seconds: cluster,
    };
    let mut w = pito_codecs::mkv::Writer::new(std::io::Cursor::new(Vec::new()), opts);
    let cfg = pito_codecs::mkv::TrackConfig {
        codec_private: xiph(&e.headers),
        sample_rate: e.rate,
        channels: u32::from(e.channels),
        ..pito_codecs::mkv::TrackConfig::new(
            pito_codecs::mkv::TrackKind::Audio,
            "A_VORBIS",
            pito_codecs::media::TimeBase::new(1, e.rate),
        )
    };
    let track = w.add_track(cfg).map_err(text)?;
    let last = e.packets.len() - 1;
    for (k, (p, _)) in e.packets.iter().enumerate() {
        if k == last {
            w.push_block(track, p, at[k], None, true, Some(&MARK))
                .map_err(text)?;
        } else {
            w.push_sample(track, p, at[k], None, true).map_err(text)?;
        }
    }
    let mut file = w.finish().map_err(text)?.into_inner();
    let discard = (end - lead as i64 - frames as i64) * 1_000_000_000 / i64::from(e.rate);
    let found = file
        .windows(MARK.len())
        .position(|w| w == MARK)
        .ok_or("no reserved bytes for DiscardPadding")?;
    let start = file[..found]
        .windows(2)
        .rposition(|w| w == [0x75, 0xa1])
        .ok_or("no BlockAdditions to overwrite")?;
    let span = found + MARK.len() - start;
    let mut patch = vec![0x75, 0xa2, 0x88];
    patch.extend(discard.to_be_bytes());
    let void = span - patch.len() - 2;
    patch.extend([0xec, 0x80 | void as u8]);
    patch.extend(std::iter::repeat_n(0u8, void));
    file[start..start + span].copy_from_slice(&patch);
    Ok(file)
}

fn mp4(e: &Encoded, frames: usize) -> Bytes {
    let (at, end, lead) = timeline(e, e.packets.len())?;
    let mut timed: crate::audio::Packets = Vec::with_capacity(e.packets.len());
    for (k, (p, _)) in e.packets.iter().enumerate() {
        let next = at.get(k + 1).copied().unwrap_or(end);
        timed.push((p.clone(), (next - at[k]) as u32));
    }
    let mut t = mux::Track::new(e.rate, e.channels, timed);
    t.format = *b"mp4a";
    let bitrate = u32::try_from(e.nominal.max(0)).unwrap_or(0);
    t.entry = mux::sound_entry(
        e.channels,
        16,
        e.rate,
        &[(b"esds", mux::esds(0xdd, &xiph(&e.headers), bitrate))],
    );
    t.priming = lead as u32;
    t.length = frames as u64;
    mux::mp4(&[t], pito_codecs::mp4::Brand::Mp4)
}

fn with_lib(mut d: Def, lib: &'static str) -> Def {
    d.lib = Some(lib);
    d
}

fn pinned(
    name: String,
    what: &str,
    lib: &'static str,
    f: impl Fn(&Path) -> Bytes + 'static,
) -> Def {
    Def {
        area: AREA,
        name,
        file: String::new(),
        kind: Kind::BitExact,
        make: Some(Box::new(move |out| {
            let dir = out.parent().ok_or("an output with no folder")?;
            testref::write_atomic(out, &f(dir)?)
        })),
        tool: OURS,
        args: words(what),
        take: Take::Digest(sha),
        lib: Some(lib),
        settle: None,
    }
}

fn decoded(ogg: &str) -> String {
    format!("{ogg}.vorbisfile.f32")
}

fn full_decode(ogg: String) -> Def {
    let what = format!("libvorbisfile ov_read_float of {ogg}, channels in WAVE order");
    pinned(decoded(&ogg), &what, "libvorbisfile", move |dir| {
        let (_, pcm) = lib::decode(&dir.join(&ogg))?;
        Ok(f32_bytes(&pcm))
    })
}

type Pick = Box<dyn Fn(&[f32]) -> Vec<f32>>;

fn windows(c: &Case, ogg: &str, file: &str, seam: bool) -> Vec<Def> {
    let full = decoded(ogg);
    let channels = c.channels;
    let ch = usize::from(channels);
    let stride = c.stride();
    let w = refs::window_frames(c.channels);
    let take = |name: String, what: String, pick: Pick| {
        let full = full.clone();
        cut(AREA, name, &format!("{what} of {full}"), move |dir| {
            let pcm = crate::audio::f32s(dir, &full)?;
            Ok(f32_bytes(&pick(&pcm)))
        })
    };
    let mut defs = vec![take(
        refs::every(file),
        format!("every {stride}th frame"),
        Box::new(move |p| refs::decimate(p, channels, stride)),
    )];
    defs.push(match refs::end(file) {
        End::Head => take(
            refs::head(file),
            format!("the first {w} frames"),
            Box::new(move |p| p[..(w * ch).min(p.len())].to_vec()),
        ),
        End::Tail => take(
            refs::tail(file),
            format!("the last {w} frames"),
            Box::new(move |p| p[p.len().saturating_sub(w * ch)..].to_vec()),
        ),
    });
    if seam {
        let (a, b) = refs::seam_span(c);
        defs.push(take(
            refs::seam(file),
            format!("frames {a} to {b}"),
            Box::new(move |p| p[(a * ch).min(p.len())..(b * ch).min(p.len())].to_vec()),
        ));
    }
    defs
}

fn vorbis() -> Vec<Def> {
    let mut defs = Vec::new();
    for c in &refs::CASES {
        let what = format!("libvorbisenc VBR quality {}, our Ogg pages", c.quality);
        defs.push(with_lib(
            crate::audio::fixture(AREA, c.ogg(), &what, move |_| {
                Ok(ogg(&encode(c)?, usize::MAX))
            }),
            "libvorbisenc",
        ));
        let what = "the rate, channels, nominal bitrate and tags libvorbisenc set";
        defs.push(with_lib(
            crate::audio::made(AREA, c.info(), Kind::BitExact, what, move |_| {
                let e = encode(c)?;
                Ok(format!(
                    "sample_rate={}\nchannels={}\nbit_rate={}\nTAG:TITLE={}\n",
                    e.rate, e.channels, e.nominal, c.name
                )
                .into_bytes())
            }),
            "libvorbisenc",
        ));
        defs.push(full_decode(c.ogg()));
        defs.extend(windows(c, &c.ogg(), &c.ogg(), false));
    }
    for (c, ogg_name) in [(&refs::LONG, "long_96k.ogg"), (&refs::HIGH, "high_88k.ogg")] {
        let what = format!("libvorbisenc VBR quality {}, our Ogg pages", c.quality);
        defs.push(pinned(
            ogg_name.to_string(),
            &what,
            "libvorbisenc",
            move |_| Ok(ogg(&encode(c)?, usize::MAX)),
        ));
        defs.push(full_decode(ogg_name.to_string()));
    }
    for m in &refs::MADE {
        let c = m.case();
        let ogg_name = format!("{}.ogg", c.name);
        let what = format!(
            "the libvorbisenc packets of {ogg_name} in our {} writer{}",
            m.file.rsplit('.').next().unwrap_or_default(),
            if m.mp4() {
                ", the priming span edited out"
            } else {
                ", DiscardPadding on the last block"
            }
        );
        defs.push(with_lib(
            crate::audio::fixture(AREA, m.file.to_string(), &what, move |_| {
                let c = m.case();
                let e = encode(&c)?;
                if m.mp4() {
                    mp4(&e, c.frames())
                } else {
                    let cluster = if m.file == refs::LONG_WEBM {
                        refs::CLUSTER_SECONDS
                    } else {
                        5.0
                    };
                    matroska(&e, c.frames(), m.file.ends_with(".webm"), cluster)
                }
            }),
            "libvorbisenc",
        ));
        let c = m.case();
        defs.extend(windows(
            &refs::named(c.name),
            &ogg_name,
            m.file,
            m.file == refs::LONG_WEBM,
        ));
    }
    defs.push(Def::ours(
        AREA,
        refs::CUT_SHORT.into(),
        &format!("src/vorbis/refs.rs cut_short({})", refs::LONG_WEBM),
        move || {
            refs::cut_short(
                &testref::root()
                    .join("tmp/record_refs")
                    .join(AREA)
                    .join(refs::LONG_WEBM),
            )
        },
    ));
    let cut_ogg = "cut_short.ogg".to_string();
    defs.push(pinned(
        cut_ogg.clone(),
        &format!(
            "our Ogg pages of the libvorbisenc packets left in {}",
            refs::CUT_SHORT
        ),
        "libvorbisenc",
        move |dir| {
            let r = pito_codecs::mkv::Reader::open(dir.join(refs::CUT_SHORT))
                .map_err(|e| e.to_string())?;
            let mut s = r.stream();
            let mut data = Vec::new();
            let mut n = 0;
            while s.next_into(&mut data).map_err(|e| e.to_string())?.is_some() {
                n += 1;
            }
            Ok(ogg(&encode(&refs::LONG)?, n))
        },
    ));
    defs.push(full_decode(cut_ogg.clone()));
    defs.extend(windows(&refs::LONG, &cut_ogg, refs::CUT_SHORT, false));
    defs
}

fn planar(t: &s16::Track) -> Vec<Vec<f32>> {
    (0..t.channels)
        .map(|ch| {
            (0..t.frames() as u64)
                .map(|i| (t.tone)(i, ch, t.rate))
                .collect()
        })
        .collect()
}

fn interleaved(t: &s16::Track) -> Vec<f32> {
    let p = planar(t);
    (0..t.frames())
        .flat_map(|i| p.iter().map(move |c| c[i]))
        .collect()
}

fn work(file: &str, k: usize, ext: &str) -> String {
    format!("{file}.t{k}.{ext}")
}

fn ints(t: &s16::Track, bits: u32) -> Vec<i32> {
    crate::audio::ints(&interleaved(t), bits)
}

fn raw(samples: &[i32], bytes: usize, big: bool) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|&s| {
            let le = s.to_le_bytes();
            let mut v = le[..bytes].to_vec();
            if big {
                v.reverse();
            }
            v
        })
        .collect()
}

fn tool_steps(f: &s16::Fixture) -> Vec<Def> {
    let mut defs = Vec::new();
    for (k, t) in f.tracks.iter().enumerate() {
        let t = t.clone();
        match t.codec {
            "mp3" => {
                let wav = {
                    let t = t.clone();
                    move || Ok(crate::audio::wav16(&interleaved(&t), t.rate, t.channels))
                };
                defs.push(
                    Def::digest(
                        s16::AREA,
                        work(&f.file, k, "mp3"),
                        "lame",
                        crate::tool("lame", &["--quiet", "-b", "256", "{src}", "{out}"]),
                        sha,
                    )
                    .from(wav),
                );
            }
            "flac" => {
                let bits = u32::from(t.bits);
                let ch = t.channels.to_string();
                let rate = t.rate.to_string();
                let bps = bits.to_string();
                let pcm = {
                    let t = t.clone();
                    move || Ok(raw(&ints(&t, bits), bits as usize / 8, false))
                };
                defs.push(
                    Def::digest(
                        s16::AREA,
                        work(&f.file, k, "flac"),
                        "flac",
                        crate::tool(
                            "flac",
                            &[
                                "--silent",
                                "--force-raw-format",
                                "--endian=little",
                                "--sign=signed",
                                &format!("--channels={ch}"),
                                &format!("--bps={bps}"),
                                &format!("--sample-rate={rate}"),
                                "-o",
                                "{out}",
                                "{src}",
                            ],
                        ),
                        sha,
                    )
                    .from(pcm),
                );
            }
            _ => {}
        }
    }
    defs
}

fn opus_track(t: &s16::Track) -> Result<mux::Track, String> {
    use pito_codecs::opus::enc::{Encoder, EncoderConfig};
    let text = |e: pito_codecs::media::Error| e.to_string();
    let mut enc = Encoder::new(EncoderConfig::new(
        t.channels,
        96_000 * u32::from(t.channels),
    ))
    .map_err(text)?;
    let mut buf = pito_codecs::media::AudioBuffer::new(48_000, t.channels);
    buf.samples = interleaved(t);
    let mut packets = enc.encode(&buf).map_err(text)?;
    packets.extend(enc.flush().map_err(text)?);
    let step = enc.frame_samples() as u32;
    let timed: crate::audio::Packets = packets.into_iter().map(|p| (p.data, step)).collect();
    let head = enc.codec_private();
    let mut tr = mux::Track::new(48_000, t.channels, timed);
    tr.priming = u32::from(enc.pre_skip());
    tr.length = t.frames() as u64;
    tr.format = *b"Opus";
    let mut dops = vec![0u8, t.channels as u8];
    dops.extend(enc.pre_skip().to_be_bytes());
    dops.extend(48_000u32.to_be_bytes());
    dops.extend([0, 0, 0]);
    tr.entry = mux::sound_entry(t.channels, 16, 48_000, &[(b"dOps", dops)]);
    tr.mkv_id = "A_OPUS";
    tr.private = head;
    Ok(tr)
}

fn ac3_track(t: &s16::Track) -> Result<mux::Track, String> {
    use crate::audio::ac3;
    let case = ac3::Case {
        name: "s16",
        rate: t.rate,
        acmod: if t.channels == 1 { 1 } else { 2 },
        lfe: false,
        kbps: 192,
        bandwidth: 4,
        coupling: false,
        remat: t.channels == 2,
        short: false,
    };
    let frames = ac3::Writer::new(&case).encode(&interleaved(t), false)?;
    let (code, _) = ac3::frame_bytes(case.rate, case.kbps).ok_or("no AC-3 frame size")?;
    let timed: crate::audio::Packets = frames.into_iter().map(|f| (f.bytes, 1536)).collect();
    let mut tr = mux::Track::new(t.rate, t.channels, timed);
    tr.priming = ac3::DELAY as u32;
    tr.length = t.frames() as u64;
    tr.format = *b"ac-3";
    let dac3 = (u32::from(case.fscod()) << 22)
        | (8 << 17)
        | (u32::from(case.acmod) << 11)
        | (u32::from(code >> 1) << 5);
    tr.entry = mux::sound_entry(
        t.channels,
        16,
        t.rate,
        &[(b"dac3", dac3.to_be_bytes()[1..].to_vec())],
    );
    tr.mkv_id = "A_AC3";
    Ok(tr)
}

fn alac_track(t: &s16::Track, ext: &str) -> Result<mux::Track, String> {
    use crate::audio::alac;
    let bits = u32::from(t.bits);
    let packets = alac::encode(
        &ints(t, bits),
        usize::from(t.channels),
        bits,
        &alac::opts(8, 2, bits >= 24),
    );
    let cookie = alac::cookie(
        usize::from(t.channels),
        bits,
        t.rate,
        alac::max_frame(&packets),
    );
    let n = t.frames();
    let timed: crate::audio::Packets = packets
        .into_iter()
        .enumerate()
        .map(|(k, p)| (p, (n - k * alac::FRAME).min(alac::FRAME) as u32))
        .collect();
    let mut tr = mux::Track::new(t.rate, t.channels, timed);
    tr.bits = bits as u16;
    tr.format = *b"alac";
    let entry = if ext == "mov" {
        alac::Entry::QtWave
    } else {
        alac::Entry::Iso
    };
    tr.entry = alac::sample_entry(entry, usize::from(t.channels), bits, t.rate, &cookie);
    tr.mkv_id = "A_ALAC";
    tr.private = cookie;
    Ok(tr)
}

fn flac_track(t: &s16::Track, data: &[u8]) -> Result<mux::Track, String> {
    let (head, packets) = crate::audio::flac_frames(data)?;
    let mut tr = mux::Track::new(t.rate, t.channels, packets);
    tr.bits = u16::from(t.bits);
    tr.format = *b"fLaC";
    let mut dfla = vec![0u8; 4];
    dfla.extend_from_slice(&head[4..]);
    tr.entry = mux::sound_entry(t.channels, tr.bits, t.rate, &[(b"dfLa", dfla)]);
    tr.mkv_id = "A_FLAC";
    tr.private = head;
    Ok(tr)
}

fn mp3_track(t: &s16::Track, data: &[u8]) -> Result<mux::Track, String> {
    let (packets, priming) = crate::audio::mp3_frames(data)?;
    let mut tr = mux::Track::new(t.rate, t.channels, packets);
    tr.priming = priming;
    tr.length = t.frames() as u64;
    tr.format = *b"mp4a";
    tr.entry = mux::sound_entry(
        t.channels,
        16,
        t.rate,
        &[(b"esds", mux::esds(0x6b, &[], 128_000))],
    );
    tr.mkv_id = "A_MPEG/L3";
    Ok(tr)
}

fn waveformatex(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
    let align = channels * bits.div_ceil(8);
    let mut w = tag.to_le_bytes().to_vec();
    w.extend(channels.to_le_bytes());
    w.extend(rate.to_le_bytes());
    w.extend((rate * u32::from(align)).to_le_bytes());
    w.extend(align.to_le_bytes());
    w.extend(bits.to_le_bytes());
    w.extend(0u16.to_le_bytes());
    w
}

struct Pcm {
    bits: u16,
    float: bool,
    big: bool,
    law: Option<bool>,
}

fn pcm_kind(codec: &str) -> Option<Pcm> {
    let law = match codec {
        "pcm_alaw" => Some(false),
        "pcm_mulaw" => Some(true),
        _ => None,
    };
    if law.is_some() {
        return Some(Pcm {
            bits: 8,
            float: false,
            big: false,
            law,
        });
    }
    let rest = codec.strip_prefix("pcm_")?;
    let float = rest.starts_with('f');
    let bits = rest[1..3].parse().ok()?;
    Some(Pcm {
        bits,
        float,
        big: rest.ends_with("be"),
        law: None,
    })
}

fn pcm_bytes(t: &s16::Track, p: &Pcm) -> Vec<u8> {
    let pcm = interleaved(t);
    match p.law {
        Some(mu) => crate::audio::ints(&pcm, 16)
            .iter()
            .map(|&s| {
                if mu {
                    s16::mulaw(s as i16)
                } else {
                    s16::alaw(s as i16)
                }
            })
            .collect(),
        None if p.float => pcm
            .iter()
            .flat_map(|v| {
                if p.big {
                    v.to_be_bytes()
                } else {
                    v.to_le_bytes()
                }
            })
            .collect(),
        None => raw(
            &crate::audio::ints(&pcm, u32::from(p.bits)),
            usize::from(p.bits / 8),
            p.big,
        ),
    }
}

fn pcm_track(t: &s16::Track, ext: &str) -> Result<mux::Track, String> {
    let p = pcm_kind(t.codec).ok_or(format!("no PCM layout for {}", t.codec))?;
    let data = pcm_bytes(t, &p);
    let frame = usize::from(t.channels) * usize::from(p.bits / 8);
    let packets: crate::audio::Packets = data
        .chunks(1024 * frame)
        .map(|c| (c.to_vec(), (c.len() / frame) as u32))
        .collect();
    let mut tr = mux::Track::new(t.rate, t.channels, packets);
    tr.bits = p.bits;
    let ch = t.channels;
    match ext {
        "mp4" => {
            tr.format = if p.float { *b"fpcm" } else { *b"ipcm" };
            let pcmc = vec![0, 0, 0, 0, u8::from(!p.big), p.bits as u8];
            tr.entry = mux::sound_entry(ch, p.bits, t.rate, &[(b"pcmC", pcmc)]);
        }
        "mov" => {
            let (format, enda): ([u8; 4], bool) = match (p.law, p.bits, p.float, p.big) {
                (Some(true), ..) => (*b"ulaw", false),
                (Some(false), ..) => (*b"alaw", false),
                (None, 16, _, false) => (*b"sowt", false),
                (None, 16, _, true) => (*b"twos", false),
                (None, 24, _, big) => (*b"in24", !big),
                (None, 32, false, big) => (*b"in32", !big),
                (None, _, _, big) => (*b"fl32", !big),
            };
            tr.format = format;
            let boxes: Vec<(&[u8; 4], Vec<u8>)> = if enda {
                vec![(b"enda", vec![0, 1])]
            } else {
                Vec::new()
            };
            tr.entry = mux::sound_entry(ch, p.bits, t.rate, &boxes);
        }
        _ => match p.law {
            Some(mu) => {
                tr.mkv_id = "A_MS/ACM";
                tr.private = waveformatex(if mu { 7 } else { 6 }, ch, t.rate, 8);
            }
            None => {
                tr.mkv_id = match (p.float, p.big) {
                    (true, _) => "A_PCM/FLOAT/IEEE",
                    (false, true) => "A_PCM/INT/BIG",
                    (false, false) => "A_PCM/INT/LIT",
                };
            }
        },
    }
    Ok(tr)
}

fn vorbis_track(t: &s16::Track) -> Result<(Encoded, usize), String> {
    let e = lib::encode(&planar(t), t.rate, 0.4, "s16")?;
    Ok((e, t.frames()))
}

fn bit_depth(file: &mut [u8], bits: u16) -> Result<(), String> {
    let at = file
        .windows(3)
        .position(|w| w == [0x56, 0xbb, 0x87])
        .ok_or("no reserved SeekPreRoll before the Audio element")?;
    let audio = at + 10;
    if file.get(audio) != Some(&0xe1) {
        return Err("the Audio element does not follow SeekPreRoll".into());
    }
    let size = usize::from(file[audio + 1] & 0x7f);
    let body = file[audio + 2..audio + 2 + size].to_vec();
    let mut patched = vec![0xe1, 0x80 | (size + 4) as u8];
    patched.extend(body);
    patched.extend([0x62, 0x64, 0x81, bits as u8]);
    patched.extend([0xec, 0x84, 0, 0, 0, 0]);
    let span = 10 + 2 + size;
    if patched.len() != span {
        return Err(format!("patch of {} bytes over {span}", patched.len()));
    }
    file[at..at + span].copy_from_slice(&patched);
    Ok(())
}

fn container(f: &s16::Fixture, dir: &Path) -> Bytes {
    let ext = f.file.rsplit('.').next().unwrap_or_default();
    let mut tracks = Vec::new();
    let mut depth = None;
    for (k, t) in f.tracks.iter().enumerate() {
        let mut tr = match t.codec {
            "aac" => crate::audio::aac_track(
                &interleaved(t),
                t.rate,
                t.channels,
                if t.rate < 32_000 { 40 } else { 96 } * u32::from(t.channels),
            )?,
            "opus" => opus_track(t)?,
            "ac3" => ac3_track(t)?,
            "alac" => alac_track(t, ext)?,
            "mp3" => mp3_track(t, &crate::audio::read(dir, &work(&f.file, k, "mp3"))?)?,
            "flac" => flac_track(t, &crate::audio::read(dir, &work(&f.file, k, "flac"))?)?,
            "vorbis" => {
                let (e, frames) = vorbis_track(t)?;
                return matroska(&e, frames, false, 5.0);
            }
            "wmav2" => {
                let mut tr = mux::Track::new(t.rate, t.channels, vec![(vec![0x5a; 743], 2048); 3]);
                tr.mkv_id = "A_MS/ACM";
                tr.private = waveformatex(0x0161, t.channels, t.rate, 16);
                tr
            }
            _ => pcm_track(t, ext)?,
        };
        tr.delay = t.delay;
        if ext == "mkv"
            && let Some(p) = pcm_kind(t.codec).filter(|p| p.bits > 16 && !p.float)
        {
            tr.reserve = true;
            depth = Some(p.bits);
        }
        tracks.push(tr);
    }
    match (ext, depth) {
        ("mkv", Some(bits)) => {
            let mut file = mux::mkv(&tracks, false)?;
            bit_depth(&mut file, bits)?;
            Ok(file)
        }
        ("3gp", _) => {
            let mut file = mux::mp4(&tracks, pito_codecs::mp4::Brand::Mp4)?;
            file[8..12].copy_from_slice(b"3gp4");
            Ok(file)
        }
        _ => mux::by_ext(ext, &tracks),
    }
}

fn s16() -> Vec<Def> {
    let mut defs = Vec::new();
    for f in s16::fixtures() {
        defs.extend(tool_steps(&f));
        let codecs: Vec<&str> = f.tracks.iter().map(|t| t.codec).collect();
        let what = format!(
            "our encoders, lame and flac where named, in our muxers: {}",
            codecs.join(", ")
        );
        let lib = codecs.contains(&"vorbis").then_some("libvorbisenc");
        let mut d = crate::audio::fixture(s16::AREA, f.file.clone(), &what, move |dir| {
            container(&f, dir)
        });
        d.lib = lib;
        defs.push(d);
    }
    defs
}

pub fn defs() -> Vec<Def> {
    let mut defs = vorbis();
    defs.extend(s16());
    defs
}
