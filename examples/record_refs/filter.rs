use crate::testref;
use crate::{Def, OURS, sha, tool, words};

#[allow(dead_code)]
#[path = "../../src/filter/refs.rs"]
mod refs;

#[allow(dead_code)]
#[path = "../../src/media/front/refs.rs"]
mod front;

use pito_codecs::media::Packet;
use pito_codecs::mp4;

fn loudness() -> Vec<Def> {
    let mut defs = Vec::new();
    for (label, rate, seconds, sound) in refs::loudness() {
        defs.push(Def::ours(
            refs::AREA,
            refs::loudness_input(label),
            &format!("src/filter/refs.rs loudness_bytes(\"{label}\")"),
            move || Ok(refs::loudness_bytes(sound, rate, seconds)),
        ));
        defs.push(
            Def::exact(
                refs::AREA,
                refs::loudness_ref(label),
                OURS,
                words(&format!(
                    "src/filter/refs.rs meter_text(meter_summary(\"{label}\"))"
                )),
            )
            .from(move || {
                let l = refs::meter_summary(sound, rate, seconds)?;
                Ok(refs::meter_text(&l).into_bytes())
            }),
        );
    }
    defs
}

fn worked(file: &str) -> Result<Vec<u8>, String> {
    let at = testref::root()
        .join("tmp/record_refs")
        .join(front::AREA)
        .join(file);
    std::fs::read(&at).map_err(|e| format!("{}: {e}", at.display()))
}

fn edges() -> Vec<Def> {
    let mut defs = vec![Def::ours(
        front::AREA,
        front::SOURCE.into(),
        "src/media/front/refs.rs source_bytes()",
        || Ok(front::source_bytes()),
    )];
    for f in &front::FIXTURES {
        let mut all = tool(f.tool(), &[]);
        all.extend(front::args(f));
        let made = if f.hevc {
            Def::digest(front::AREA, f.out(), f.tool(), all, sha)
        } else {
            Def::fixture(front::AREA, f.out(), f.tool(), all)
        };
        defs.push(made.from(|| Ok(front::source_bytes())));
        if f.hevc {
            let stream = f.stream();
            defs.push(
                Def::fixture(
                    front::AREA,
                    f.name.into(),
                    OURS,
                    words(&format!(
                        "examples/record_refs/filter.rs hevc_mp4(\"{stream}\")"
                    )),
                )
                .from(move || hevc_mp4(&worked(&stream)?)),
            );
        }
    }
    defs
}

fn annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i..i + 3] == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map_or(data.len(), |&n| n - 3);
        while end > s && data[end - 1] == 0 {
            end -= 1;
        }
        out.push(&data[s..end]);
    }
    out
}

fn kind(nal: &[u8]) -> u8 {
    nal.first().map_or(63, |h| (h >> 1) & 0x3f)
}

fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

fn hvcc(vps: &[&[u8]], sps: &[&[u8]], pps: &[&[u8]]) -> Result<Vec<u8>, String> {
    let first = sps.first().ok_or("no SPS in the x265 stream")?;
    let rbsp = unescape(first);
    let ptl = rbsp
        .get(3..15)
        .ok_or("an SPS too short for its profile, tier and level")?;
    let mut out = vec![1];
    out.extend_from_slice(ptl);
    out.extend_from_slice(&[0xf0, 0x00, 0xfc, 0xfd, 0xf8, 0xf8, 0, 0, 0x0f]);
    out.push(3);
    for (t, list) in [(32u8, vps), (33, sps), (34, pps)] {
        out.push(0x80 | t);
        let n = u16::try_from(list.len()).map_err(|_| "too many parameter sets")?;
        out.extend_from_slice(&n.to_be_bytes());
        for nal in list {
            let len = u16::try_from(nal.len()).map_err(|_| "a parameter set over 64 KiB")?;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(nal);
        }
    }
    Ok(out)
}

fn hevc_mp4(stream: &[u8]) -> Result<Vec<u8>, String> {
    let nals = annexb(stream);
    let of = |t: u8| -> Vec<&[u8]> { nals.iter().copied().filter(|n| kind(n) == t).collect() };
    let config = hvcc(&of(32), &of(33), &of(34))?;
    let mut units: Vec<(Vec<u8>, bool)> = Vec::new();
    for nal in &nals {
        let t = kind(nal);
        if t > 31 {
            continue;
        }
        let first_slice = nal.get(2).is_some_and(|b| b & 0x80 != 0);
        if first_slice || units.is_empty() {
            units.push((Vec::new(), (16..=23).contains(&t)));
        }
        let Some((au, _)) = units.last_mut() else {
            return Err("a slice with no access unit".into());
        };
        au.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        au.extend_from_slice(nal);
    }
    if units.len() as u64 != front::FRAMES {
        return Err(format!(
            "the x265 stream holds {} pictures, not {}",
            units.len(),
            front::FRAMES
        ));
    }
    let at = testref::scratch(front::AREA).join("hevc-mux.mp4");
    let text = |e: pito_codecs::media::Error| e.to_string();
    let (w, h) = front::SMALL;
    let entry = mp4::SampleEntry::hevc(
        mp4::HevcConfig::parse(&config).map_err(text)?,
        w as u16,
        h as u16,
    );
    let mut m = mp4::Writer::create(&at).map_err(text)?;
    m.set_movie_timescale(1000);
    let track = m
        .add_track(mp4::TrackConfig::new(entry, front::FPS))
        .map_err(text)?;
    for (n, (data, keyframe)) in units.into_iter().enumerate() {
        let packet = Packet {
            data,
            pts: n as i64,
            dts: n as i64,
            keyframe,
        };
        m.push_timed(track, &packet, 1).map_err(text)?;
    }
    m.finish_faststart().map_err(text)?;
    std::fs::read(&at).map_err(|e| format!("{}: {e}", at.display()))
}

pub fn defs() -> Vec<Def> {
    [loudness(), edges()].into_iter().flatten().collect()
}
