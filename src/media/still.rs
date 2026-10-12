use std::path::Path;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::mkv;

use super::demux::{Demux, mkv_head};
use super::native;
use super::picture::Color;
use super::probe::keyframes;
use super::route::{Caps, Prefs, pick_decode};
use super::types::{Codec, Error, Result, Rgba, Route};
use super::video::{Source, source};

pub fn first_frame(path: &Path, height: u32, prefs: &Prefs) -> Result<Rgba> {
    frames_at(path, &[0.0], height, prefs)?
        .pop()
        .ok_or_else(|| Error::Invalid("no frame".to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Seek {
    #[default]
    Keyframe,
    Exact,
}

pub fn frames_at(path: &Path, times: &[f64], height: u32, prefs: &Prefs) -> Result<Vec<Rgba>> {
    frames_at_seek(path, times, height, Seek::Keyframe, prefs)
}

pub fn frames_at_seek(
    path: &Path,
    times: &[f64],
    height: u32,
    seek: Seek,
    prefs: &Prefs,
) -> Result<Vec<Rgba>> {
    if let Ok(Some(r)) = mkv_head(path)
        && let Some(route) = mkv_route(&r, prefs)
    {
        return mkv_stills(&r, times, height, route, seek);
    }
    match seek {
        Seek::Keyframe => keyframe_stills(path, times, height, prefs),
        Seek::Exact => exact_stills(path, times, height, prefs),
    }
}

fn keyframe_stills(path: &Path, times: &[f64], height: u32, prefs: &Prefs) -> Result<Vec<Rgba>> {
    let src = source(path, prefs)?;
    let keys = if times.iter().all(|t| *t <= 0.0) {
        vec![0.0]
    } else {
        keyframes(path, prefs)?
    };
    let snap = |t: f64| {
        keys.iter()
            .copied()
            .take_while(|k| *k <= t + 1e-6)
            .last()
            .unwrap_or(0.0)
    };
    let times: Vec<f64> = times.iter().map(|t| snap(*t)).collect();
    let workers = times.len().min(16);
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<Result<Rgba>>>> =
        Mutex::new((0..times.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for w in 0..workers {
            let (src, next, out, times) = (&src, &next, &out, &times);
            s.spawn(move || {
                let route = if w == 0 { src.route } else { Route::Cpu };
                let mut dec = match (src.route, &src.reader) {
                    (Route::Cpu | Route::Gpu, Some(r)) => native::still_decoder(r, route).ok(),
                    _ => None,
                };
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&t) = times.get(i) else { break };
                    let repeat = times.get(..i).is_some_and(|b| b.contains(&t));
                    let got = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        match (repeat, &mut dec) {
                            (true, _) => None,
                            (false, Some(d)) => Some(match &src.reader {
                                Some(r) => native::still(r, d, t, height),
                                None => Err(src.native()),
                            }),
                            (false, None) => Some(still(src, t, height)),
                        }
                    }));
                    let got = got.unwrap_or_else(|_| {
                        dec = None;
                        Some(Err(Error::Backend(format!(
                            "decoding the still at {t:.3} s failed"
                        ))))
                    });
                    if let (Some(got), Ok(mut o)) = (got, out.lock()) {
                        o[i] = Some(got);
                    }
                }
            });
        }
    });
    let done = out
        .into_inner()
        .map_err(|_| Error::Backend("a still worker failed".to_string()))?;
    let mut stills: Vec<Rgba> = Vec::with_capacity(times.len());
    for (i, r) in done.into_iter().enumerate() {
        match r {
            Some(r) => stills.push(r?),
            None => {
                let same = times[..i].iter().position(|o| *o == times[i]);
                match same.and_then(|j| stills.get(j)) {
                    Some(s) => stills.push(s.clone()),
                    None => return Err(Error::Backend("a still worker failed".to_string())),
                }
            }
        }
    }
    Ok(stills)
}

fn mkv_route(r: &mkv::Reader, prefs: &Prefs) -> Option<Route> {
    let codec = r.tracks.get(r.video()?)?.codec()?;
    if !matches!(codec, Codec::H264 | Codec::Hevc | Codec::Vp9) {
        return None;
    }
    let picked = pick_decode(codec, prefs, &Caps::detect(prefs)).ok()?;
    (picked.route != Route::Ffmpeg).then_some(picked.route)
}

fn failed() -> Error {
    Error::Backend("a still worker failed".to_string())
}

fn spread<W>(
    jobs: usize,
    route: Route,
    make: impl Fn(Route) -> W + Sync,
    run: impl Fn(&mut W, usize) -> Result<Vec<Rgba>> + Sync,
) -> Result<Vec<Rgba>> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<Result<Vec<Rgba>>>>> = Mutex::new((0..jobs).map(|_| None).collect());
    std::thread::scope(|s| {
        for w in 0..jobs.min(16) {
            let (next, out, make, run) = (&next, &out, &make, &run);
            s.spawn(move || {
                let mut state = make(if w == 0 { route } else { Route::Cpu });
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= jobs {
                        break;
                    }
                    let got = std::panic::catch_unwind(AssertUnwindSafe(|| run(&mut state, i)))
                        .unwrap_or_else(|_| {
                            Err(Error::Backend("decoding a still failed".to_string()))
                        });
                    if got.is_err() {
                        state = make(Route::Cpu);
                    }
                    if let Ok(mut o) = out.lock()
                        && let Some(slot) = o.get_mut(i)
                    {
                        *slot = Some(got);
                    }
                }
            });
        }
    });
    let done = out.into_inner().map_err(|_| failed())?;
    let mut stills = Vec::new();
    for r in done {
        stills.extend(r.ok_or_else(failed)??);
    }
    Ok(stills)
}

fn gather(wanted: &[f64], unique: &[f64], stills: &[Rgba]) -> Result<Vec<Rgba>> {
    wanted
        .iter()
        .map(|at| {
            unique
                .iter()
                .position(|u| u == at)
                .and_then(|j| stills.get(j).cloned())
                .ok_or_else(failed)
        })
        .collect()
}

fn share(jobs: usize) -> usize {
    (native::threads() / jobs.clamp(1, 16)).max(1)
}

fn sorted(times: &[f64]) -> Vec<f64> {
    let mut unique: Vec<f64> = times.iter().map(|t| t.max(0.0)).collect();
    unique.sort_by(f64::total_cmp);
    unique.dedup();
    unique
}

fn batches<K: PartialEq>(unique: &[f64], key: impl Fn(f64) -> K) -> Vec<Vec<f64>> {
    let mut jobs: Vec<(K, Vec<f64>)> = Vec::new();
    for &at in unique {
        let k = key(at);
        match jobs.last_mut() {
            Some((last, batch)) if *last == k => batch.push(at),
            _ => jobs.push((k, vec![at])),
        }
    }
    jobs.into_iter().map(|(_, b)| b).collect()
}

fn tagged(d: &Demux, track: usize) -> Color {
    let (matrix, full_range) = d
        .stream(track)
        .ok()
        .and_then(|s| s.color)
        .unwrap_or((2, false));
    Color { matrix, full_range }
}

fn mkv_stills(
    r: &Arc<mkv::Reader>,
    times: &[f64],
    height: u32,
    route: Route,
    seek: Seek,
) -> Result<Vec<Rgba>> {
    let d = Demux::Mkv(r.clone());
    let v = d.video()?;
    let t = r
        .tracks
        .get(v)
        .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
    let rotation = t.rotation();
    let color = tagged(&d, v);
    let mut keys: Vec<f64> = r
        .cues
        .iter()
        .filter(|c| c.track == t.number)
        .map(|c| t.seconds(c.time as i64).max(0.0))
        .collect();
    keys.sort_by(f64::total_cmp);
    keys.dedup();
    let snap = |at: f64| {
        if at <= 0.0 {
            0.0
        } else if keys.is_empty() {
            at
        } else {
            keys.iter()
                .copied()
                .take_while(|k| *k <= at + 1e-6)
                .last()
                .unwrap_or(0.0)
        }
    };
    let (wanted, unique, jobs) = match seek {
        Seek::Keyframe => {
            let wanted: Vec<f64> = times.iter().map(|at| snap(*at)).collect();
            let mut unique: Vec<f64> = Vec::with_capacity(wanted.len());
            for at in &wanted {
                if !unique.contains(at) {
                    unique.push(*at);
                }
            }
            let jobs = unique.iter().map(|u| vec![*u]).collect();
            (wanted, unique, jobs)
        }
        Seek::Exact => {
            let wanted: Vec<f64> = times.iter().map(|at| at.max(0.0)).collect();
            let unique = sorted(times);
            let jobs = if keys.is_empty() {
                unique.iter().map(|u| vec![*u]).collect()
            } else {
                batches(&unique, |at| snap(at).to_bits())
            };
            (wanted, unique, jobs)
        }
    };
    let threads = match seek {
        Seek::Keyframe => 1,
        Seek::Exact => share(jobs.len()),
    };
    let stills = spread(
        jobs.len(),
        route,
        |rt| (native::video_decoder(&d, rt, threads), mkv::Cursor::new(r)),
        |(dec, cur), j| {
            let dec = dec.as_mut().map_err(|e| Error::Backend(e.to_string()))?;
            let batch = jobs.get(j).ok_or_else(failed)?;
            let first = batch.first().copied().ok_or_else(failed)?;
            match seek {
                Seek::Keyframe => Ok(vec![native::still_mkv(r, cur, dec, first, height)?]),
                Seek::Exact => {
                    cur.seek(r, v, first)?;
                    let targets: Vec<i64> = batch.iter().map(|at| t.ticks(*at)).collect();
                    let mut src = native::Packets::Mkv { r, cur, track: v };
                    native::exact(&mut src, dec, &targets, rotation, height, color)
                }
            }
        },
    )?;
    gather(&wanted, &unique, &stills)
}

fn exact_stills(path: &Path, times: &[f64], height: u32, prefs: &Prefs) -> Result<Vec<Rgba>> {
    let src = source(path, prefs)?;
    let wanted: Vec<f64> = times.iter().map(|at| at.max(0.0)).collect();
    let unique = sorted(times);
    let reader = match (src.route, &src.reader) {
        (Route::Ffmpeg, _) => {
            let stills = unique
                .iter()
                .map(|at| ffmpeg_still(&src, *at, height, true))
                .collect::<Result<Vec<_>>>()?;
            return gather(&wanted, &unique, &stills);
        }
        (_, Some(r)) => r.clone(),
        (_, None) => return Err(src.native()),
    };
    let d = Demux::Mp4(reader.clone());
    let v = d.video()?;
    let rotation = reader.tracks.get(v).map_or(0, |t| t.rotation());
    let color = tagged(&d, v);
    let seeks: Vec<(f64, usize, i64)> = unique
        .iter()
        .map(|at| d.seek(v, *at).map(|(key, start)| (*at, key, start)))
        .collect::<Result<_>>()?;
    let jobs = batches(&unique, |at| {
        seeks
            .iter()
            .find(|s| s.0 == at)
            .map_or(0, |s| s.1)
    });
    let threads = share(jobs.len());
    let stills = spread(
        jobs.len(),
        src.route,
        |rt| native::video_decoder(&d, rt, threads),
        |dec, j| {
            let dec = dec.as_mut().map_err(|e| Error::Backend(e.to_string()))?;
            let batch = jobs.get(j).ok_or_else(failed)?;
            let plan: Vec<(usize, i64)> = batch
                .iter()
                .filter_map(|at| seeks.iter().find(|s| s.0 == *at).map(|s| (s.1, s.2)))
                .collect();
            let key = plan.first().map(|p| p.0).ok_or_else(failed)?;
            let targets: Vec<i64> = plan.iter().map(|p| p.1).collect();
            let mut packets = native::Packets::Indexed {
                d: &d,
                track: v,
                next: key,
            };
            native::exact(&mut packets, dec, &targets, rotation, height, color)
        },
    )?;
    gather(&wanted, &unique, &stills)
}

fn still(src: &Source, key: f64, height: u32) -> Result<Rgba> {
    ffmpeg_still(src, key, height, false)
}

fn ffmpeg_still(src: &Source, key: f64, height: u32, exact: bool) -> Result<Rgba> {
    if src.route != Route::Ffmpeg {
        return Err(src.native());
    }
    let f = src.ffmpeg()?;
    let mut args = f.decoder_args(src.ffmpeg_codec.as_deref());
    args.extend(["-threads".into(), "2".into()]);
    if !exact {
        args.extend(["-skip_frame".into(), "nokey".into()]);
    }
    if key > 0.0 {
        let at = if exact { key } else { (key - 0.0005).max(0.0) };
        args.extend(["-ss".into(), format!("{at:.6}")]);
    }
    args.extend([
        "-i".into(),
        src.path.display().to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        format!("scale=-2:{}:flags=bilinear", height.max(2)),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "-".into(),
    ]);
    let data = f.run_args(&args)?;
    let h = height.max(2);
    let w = (data.len() / 4 / h as usize) as u32;
    if w == 0 || data.len() != w as usize * h as usize * 4 {
        return Err(Error::Backend(format!(
            "ffmpeg gave {} bytes for a still {h} high",
            data.len()
        )));
    }
    Ok(Rgba {
        width: w,
        height: h,
        data,
    })
}
