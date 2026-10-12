use std::path::Path;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::mkv;

use super::demux::{Demux, mkv_head};
use super::native;
use super::probe::keyframes;
use super::route::{Caps, Prefs, pick_decode};
use super::types::{Codec, Error, Result, Rgba, Route};
use super::video::{Source, source};

pub fn first_frame(path: &Path, height: u32, prefs: &Prefs) -> Result<Rgba> {
    frames_at(path, &[0.0], height, prefs)?
        .pop()
        .ok_or_else(|| Error::Invalid("no frame".to_string()))
}

pub fn frames_at(path: &Path, times: &[f64], height: u32, prefs: &Prefs) -> Result<Vec<Rgba>> {
    if let Ok(Some(r)) = mkv_head(path)
        && let Some(route) = mkv_route(&r, prefs)
    {
        return mkv_stills(&r, times, height, route);
    }
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

fn mkv_stills(
    r: &Arc<mkv::Reader>,
    times: &[f64],
    height: u32,
    route: Route,
) -> Result<Vec<Rgba>> {
    let d = Demux::Mkv(r.clone());
    let v = d.video()?;
    let t = r
        .tracks
        .get(v)
        .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
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
    let wanted: Vec<f64> = times.iter().map(|at| snap(*at)).collect();
    let mut unique: Vec<f64> = Vec::with_capacity(wanted.len());
    for at in &wanted {
        if !unique.contains(at) {
            unique.push(*at);
        }
    }
    let workers = unique.len().min(16);
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<Result<Rgba>>>> =
        Mutex::new((0..unique.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for w in 0..workers {
            let (d, next, out, unique) = (&d, &next, &out, &unique);
            s.spawn(move || {
                let route = if w == 0 { route } else { Route::Cpu };
                let mut dec = native::video_decoder(d, route);
                let mut cur = mkv::Cursor::new(r);
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&at) = unique.get(i) else { break };
                    let got = match &mut dec {
                        Ok(dc) => std::panic::catch_unwind(AssertUnwindSafe(|| {
                            native::still_mkv(r, &mut cur, dc, at, height)
                        }))
                        .unwrap_or_else(|_| {
                            Err(Error::Backend(format!(
                                "decoding the still at {at:.3} s failed"
                            )))
                        }),
                        Err(e) => Err(Error::Backend(e.to_string())),
                    };
                    if got.is_err() {
                        dec = native::video_decoder(d, Route::Cpu);
                        cur = mkv::Cursor::new(r);
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
    let done = out
        .into_inner()
        .map_err(|_| Error::Backend("a still worker failed".to_string()))?;
    let mut stills: Vec<Rgba> = Vec::with_capacity(unique.len());
    for r in done {
        match r {
            Some(r) => stills.push(r?),
            None => return Err(Error::Backend("a still worker failed".to_string())),
        }
    }
    wanted
        .iter()
        .map(|at| {
            unique
                .iter()
                .position(|u| u == at)
                .and_then(|j| stills.get(j).cloned())
                .ok_or_else(|| Error::Backend("a still worker failed".to_string()))
        })
        .collect()
}

fn still(src: &Source, key: f64, height: u32) -> Result<Rgba> {
    if src.route != Route::Ffmpeg {
        return Err(src.native());
    }
    let f = src.ffmpeg()?;
    let mut args = f.decoder_args(src.ffmpeg_codec.as_deref());
    args.extend([
        "-threads".into(),
        "2".into(),
        "-skip_frame".into(),
        "nokey".into(),
    ]);
    if key > 0.0 {
        args.extend(["-ss".into(), format!("{:.6}", (key - 0.0005).max(0.0))]);
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
