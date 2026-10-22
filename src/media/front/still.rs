use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::mkv;

use super::demux::{Demux, mkv_head};
use super::native;
use super::picture::Color;
use super::probe::{keyframes, probe};
use super::route::{Caps, Prefs, pick_decode};
use super::video::{Source, source};
use crate::media::{Codec, Error, Result, Rgba, Route};

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
    let mut stills = Stills::open(path, height, prefs)?;
    stills.warm = false;
    stills.collect(times, seek)
}

const KEPT: usize = 18;
const GPU_WORKERS: usize = 4;

const WARM: Key = (Route::Gpu, false, 0);

fn gpu_first(d: &Demux, track: usize, route: Route) -> Option<&Demux> {
    (route == Route::Gpu && d.stream(track).is_ok_and(|s| s.codec == Some(Codec::Av1))).then_some(d)
}

pub struct Stills {
    height: u32,
    kind: Kind,
    shared: Arc<Shared>,
    warm: bool,
}

struct Shared {
    kept: Mutex<Vec<Worker>>,
    limit: AtomicUsize,
    warming: AtomicUsize,
    broken: AtomicBool,
}

impl Shared {
    fn put(&self, w: Worker) {
        if w.dec.is_err() {
            return;
        }
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.len() < self.limit.load(Ordering::Relaxed) {
            kept.push(w);
        }
    }

    fn put_warm(&self, w: Worker) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let evicted = if kept.len() < self.limit.load(Ordering::Relaxed) {
            None
        } else {
            kept.iter()
                .position(|k| k.key.0 != Route::Gpu)
                .map(|i| kept.swap_remove(i))
        };
        if kept.len() < self.limit.load(Ordering::Relaxed) {
            kept.push(w);
        }
        drop(kept);
        drop(evicted);
    }

    fn ready(&self, key: Key) -> usize {
        self.kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|w| w.key == key)
            .count()
    }
}

enum Kind {
    Alpha(Arc<mkv::Reader>),
    Mkv {
        r: Arc<mkv::Reader>,
        route: Route,
    },
    File {
        src: Box<Source>,
        prefs: Prefs,
        keys: OnceLock<Vec<f64>>,
    },
}

type Key = (Route, bool, usize);

struct Worker {
    key: Key,
    dec: Result<native::Decoder>,
    cur: Option<mkv::Cursor>,
}

type Emit<'a> = &'a (dyn Fn(usize, Result<Rgba>) + Sync);

const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    shared::<Stills>();
};

impl Stills {
    pub fn open(path: &Path, height: u32, prefs: &Prefs) -> Result<Stills> {
        let kind = match mkv_head(path) {
            Ok(Some(r)) if super::alpha::has_alpha(&r) => {
                probe(path, prefs)?.decodable()?;
                Kind::Alpha(r)
            }
            Ok(Some(r)) => match mkv_route(&r, prefs) {
                Some(route) => {
                    probe(path, prefs)?.decodable()?;
                    Kind::Mkv { r, route }
                }
                None => file(path, prefs)?,
            },
            _ => file(path, prefs)?,
        };
        Ok(Stills {
            height,
            kind,
            shared: Arc::new(Shared {
                kept: Mutex::new(Vec::new()),
                limit: AtomicUsize::new(KEPT),
                warming: AtomicUsize::new(0),
                broken: AtomicBool::new(false),
            }),
            warm: true,
        })
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn keep(&self, decoders: usize) {
        let n = decoders.max(1);
        self.shared.limit.store(n, Ordering::Relaxed);
        self.shared
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .truncate(n);
    }

    pub fn trim(&self) {
        let dropped = std::mem::take(
            &mut *self
                .shared
                .kept
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        drop(dropped);
    }

    pub fn frames(
        &self,
        times: &[f64],
        seek: Seek,
        each: impl FnMut(usize, Result<Rgba>) + Send,
    ) -> Result<()> {
        let each = Mutex::new(each);
        let emit = |i: usize, r: Result<Rgba>| {
            let mut f = each.lock().unwrap_or_else(PoisonError::into_inner);
            f(i, r);
        };
        match &self.kind {
            Kind::Alpha(r) => super::alpha::stills(r, self.height, times, seek, &emit),
            Kind::Mkv { r, route } => self.mkv(r, *route, times, seek, &emit),
            Kind::File { src, .. } => match seek {
                Seek::Keyframe => self.keyframe(src, times, &emit),
                Seek::Exact => self.exact(src, times, &emit),
            },
        }
    }

    fn collect(&self, times: &[f64], seek: Seek) -> Result<Vec<Rgba>> {
        let mut got: Vec<Option<Result<Rgba>>> = (0..times.len()).map(|_| None).collect();
        self.frames(times, seek, |i, r| {
            if let Some(slot) = got.get_mut(i) {
                *slot = Some(r);
            }
        })?;
        got.into_iter().map(|r| r.ok_or_else(failed)?).collect()
    }

    fn take(&self, key: Key, make: impl FnOnce() -> Result<native::Decoder>) -> Worker {
        let mut kept = {
            let mut kept = self
                .shared
                .kept
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            kept.iter()
                .position(|w| w.key == key)
                .map(|i| kept.swap_remove(i))
        };
        if let Some(Worker { dec: Ok(d), .. }) = &mut kept {
            d.reset();
        }
        kept.unwrap_or_else(|| Worker {
            key,
            dec: make(),
            cur: None,
        })
    }

    fn warm_up(&self, d: &Demux) -> (usize, usize) {
        let s = &self.shared;
        let target = GPU_WORKERS.min(s.limit.load(Ordering::Relaxed)).max(1);
        let ready = s.ready(WARM);
        if ready >= target || !self.warm || s.broken.load(Ordering::Relaxed) {
            return (ready, target);
        }
        let missing = target.saturating_sub(ready.max(1) + s.warming.load(Ordering::Relaxed));
        for _ in 0..missing {
            let (shared, d) = (self.shared.clone(), d.clone());
            shared.warming.fetch_add(1, Ordering::Relaxed);
            let spawned = std::thread::Builder::new()
                .name("stills-warm".into())
                .spawn(move || {
                    let dec = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        let mut dec = native::video_decoder(&d, Route::Gpu, 1)?;
                        if dec.route() == Route::Gpu {
                            native::prime(&d, &mut dec)?;
                        }
                        Ok::<_, Error>(dec)
                    }));
                    match dec {
                        Ok(Ok(dec)) if dec.route() == Route::Gpu => shared.put_warm(Worker {
                            key: WARM,
                            dec: Ok(dec),
                            cur: None,
                        }),
                        _ => shared.broken.store(true, Ordering::Relaxed),
                    }
                    shared.warming.fetch_sub(1, Ordering::Relaxed);
                });
            if spawned.is_err() {
                self.shared.warming.fetch_sub(1, Ordering::Relaxed);
            }
        }
        (ready, target)
    }

    fn keys(&self) -> &[f64] {
        match &self.kind {
            Kind::File { src, prefs, keys } => {
                keys.get_or_init(|| keyframes(&src.path, prefs).unwrap_or_else(|_| vec![0.0]))
            }
            Kind::Mkv { .. } | Kind::Alpha(_) => &[],
        }
    }

    fn keyframe(&self, src: &Source, times: &[f64], emit: Emit) -> Result<()> {
        let keys: &[f64] = if times.iter().all(|t| *t <= 0.0) {
            &[0.0]
        } else {
            self.keys()
        };
        let wanted: Vec<f64> = times.iter().map(|t| snap(keys, *t)).collect();
        let jobs: Vec<Vec<f64>> = first_seen(&wanted).into_iter().map(|t| vec![t]).collect();
        let height = self.height;
        let reader = src.mp4();
        self.spread(
            &jobs,
            src.route,
            None,
            |rt| match reader {
                Some(r) => self.take((rt, true, 1), || native::still_decoder(r, rt)),
                None => Worker {
                    key: (rt, true, 1),
                    dec: Err(src.native()),
                    cur: None,
                },
            },
            |w, batch| {
                let t = batch.first().copied().ok_or_else(failed)?;
                Ok(vec![match (reader, w.dec.as_mut()) {
                    (Some(r), Ok(d)) => native::still(r, d, t, height)?,
                    _ => return Err(src.native()),
                }])
            },
            &wanted,
            emit,
        );
        Ok(())
    }

    fn mkv(
        &self,
        r: &Arc<mkv::Reader>,
        route: Route,
        times: &[f64],
        seek: Seek,
        emit: Emit,
    ) -> Result<()> {
        let d = Demux::Mkv(r.clone());
        let v = d.video()?;
        let t = r
            .tracks
            .get(v)
            .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
        let rotation = t.rotation();
        let color = tagged(&d, v);
        let skip = d.stream(v).ok().and_then(|s| native::skipper(&s));
        let start = d.start();
        let shift = d.shift(v);
        let mut keys: Vec<f64> = r
            .cues
            .iter()
            .filter(|c| c.track == t.number)
            .map(|c| (t.seconds(c.time as i64) - start).max(0.0))
            .collect();
        keys.sort_by(f64::total_cmp);
        keys.dedup();
        let snapped = |at: f64| {
            if keys.is_empty() && at > 0.0 {
                at
            } else {
                snap(&keys, at)
            }
        };
        let (wanted, jobs): (Vec<f64>, Vec<Vec<f64>>) = match seek {
            Seek::Keyframe => {
                let wanted: Vec<f64> = times.iter().map(|at| snapped(*at)).collect();
                let jobs = first_seen(&wanted).into_iter().map(|u| vec![u]).collect();
                (wanted, jobs)
            }
            Seek::Exact => {
                let wanted: Vec<f64> = times.iter().map(|at| at.max(0.0)).collect();
                let unique = sorted(times);
                let jobs = if keys.is_empty() {
                    unique.iter().map(|u| vec![*u]).collect()
                } else {
                    batches(&unique, |at| snapped(at).to_bits())
                };
                (wanted, jobs)
            }
        };
        let stills = seek == Seek::Keyframe;
        let threads = if stills { 1 } else { share(jobs.len()) };
        let height = self.height;
        self.spread(
            &jobs,
            route,
            if seek == Seek::Exact {
                gpu_first(&d, v, route)
            } else {
                None
            },
            |rt| {
                let key = (rt, stills, if rt == Route::Gpu { 0 } else { threads });
                let mut w = self.take(key, || {
                    native::video_decoder(&d, rt, threads).map(|mut dec| {
                        if stills {
                            dec.stills();
                        }
                        dec
                    })
                });
                if w.cur.is_none() {
                    w.cur = Some(mkv::Cursor::new(r));
                }
                w
            },
            |w, batch| {
                let dec = w.dec.as_mut().map_err(|e| Error::Backend(e.to_string()))?;
                let cur = w.cur.get_or_insert_with(|| mkv::Cursor::new(r));
                let first = batch.first().copied().ok_or_else(failed)?;
                if stills {
                    return Ok(vec![native::still_mkv(r, cur, dec, first, height)?]);
                }
                cur.seek(r, v, first + start)?;
                let targets: Vec<i64> = batch.iter().map(|at| t.ticks(*at)).collect();
                let mut src = native::Packets::Mkv {
                    r,
                    cur,
                    track: v,
                    shift,
                };
                dec.reset();
                native::exact(
                    &mut src,
                    dec,
                    &targets,
                    rotation,
                    height,
                    color,
                    skip.as_ref(),
                )
            },
            &wanted,
            emit,
        );
        Ok(())
    }

    fn exact(&self, src: &Source, times: &[f64], emit: Emit) -> Result<()> {
        let wanted: Vec<f64> = times.iter().map(|at| at.max(0.0)).collect();
        let unique = sorted(times);
        let height = self.height;
        let reader = match src.mp4() {
            Some(r) => r.clone(),
            None => return Err(src.native()),
        };
        let d = Demux::Mp4(reader.clone());
        let v = d.video()?;
        let rotation = reader.tracks.get(v).map_or(0, |t| t.rotation());
        let color = tagged(&d, v);
        let skip = d.stream(v).ok().and_then(|s| native::skipper(&s));
        let seeks: Vec<(f64, usize, i64)> = unique
            .iter()
            .map(|at| d.seek(v, *at).map(|(key, start)| (*at, key, start)))
            .collect::<Result<_>>()?;
        let jobs = batches(&unique, |at| {
            seeks.iter().find(|s| s.0 == at).map_or(0, |s| s.1)
        });
        let threads = share(jobs.len());
        self.spread(
            &jobs,
            src.route,
            gpu_first(&d, v, src.route),
            |rt| {
                let key = (rt, false, if rt == Route::Gpu { 0 } else { threads });
                self.take(key, || native::video_decoder(&d, rt, threads))
            },
            |w, batch| {
                let dec = w.dec.as_mut().map_err(|e| Error::Backend(e.to_string()))?;
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
                dec.reset();
                native::exact(
                    &mut packets,
                    dec,
                    &targets,
                    rotation,
                    height,
                    color,
                    skip.as_ref(),
                )
            },
            &wanted,
            emit,
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn spread(
        &self,
        jobs: &[Vec<f64>],
        route: Route,
        gpu: Option<&Demux>,
        make: impl Fn(Route) -> Worker + Sync,
        run: impl Fn(&mut Worker, &[f64]) -> Result<Vec<Rgba>> + Sync,
        wanted: &[f64],
        emit: Emit,
    ) {
        let next = AtomicUsize::new(0);
        std::thread::scope(|s| {
            let mut workers = jobs
                .len()
                .min(16)
                .min(self.shared.limit.load(Ordering::Relaxed));
            let mut on_route = 1;
            if let Some(d) = gpu {
                let (ready, target) = self.warm_up(d);
                if ready >= target {
                    workers = workers.min(target);
                }
                on_route = ready.clamp(1, target);
            }
            for w in 0..workers {
                let (next, make, run) = (&next, &make, &run);
                s.spawn(move || {
                    let mut state = make(if w < on_route { route } else { Route::Cpu });
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(batch) = jobs.get(i) else { break };
                        let got =
                            std::panic::catch_unwind(AssertUnwindSafe(|| run(&mut state, batch)))
                                .unwrap_or_else(|_| {
                                    Err(Error::Backend("decoding a still failed".to_string()))
                                });
                        if got.is_err() {
                            state = make(Route::Cpu);
                        }
                        deliver(wanted, batch, got, emit);
                    }
                    self.shared.put(state);
                });
            }
        });
    }
}

fn file(path: &Path, prefs: &Prefs) -> Result<Kind> {
    Ok(Kind::File {
        src: Box::new(source(path, prefs)?),
        prefs: prefs.clone(),
        keys: OnceLock::new(),
    })
}

fn deliver(wanted: &[f64], batch: &[f64], got: Result<Vec<Rgba>>, emit: Emit) {
    match got {
        Ok(stills) => {
            for (k, at) in batch.iter().enumerate() {
                for (i, _) in wanted.iter().enumerate().filter(|(_, w)| *w == at) {
                    emit(i, stills.get(k).cloned().ok_or_else(failed));
                }
            }
        }
        Err(e) => {
            let why = e.to_string();
            let mut first = Some(e);
            for (i, _) in wanted.iter().enumerate().filter(|(_, w)| batch.contains(w)) {
                emit(
                    i,
                    Err(first.take().unwrap_or_else(|| Error::Backend(why.clone()))),
                );
            }
        }
    }
}

fn snap(keys: &[f64], at: f64) -> f64 {
    if at <= 0.0 {
        return 0.0;
    }
    keys.iter()
        .copied()
        .take_while(|k| *k <= at + 1e-6)
        .last()
        .unwrap_or(0.0)
}

fn first_seen(wanted: &[f64]) -> Vec<f64> {
    let mut unique: Vec<f64> = Vec::with_capacity(wanted.len());
    for at in wanted {
        if !unique.contains(at) {
            unique.push(*at);
        }
    }
    unique
}

fn mkv_route(r: &mkv::Reader, prefs: &Prefs) -> Option<Route> {
    let codec = r.tracks.get(r.video()?)?.codec()?;
    if !matches!(codec, Codec::H264 | Codec::Hevc | Codec::Vp9 | Codec::Av1) {
        return None;
    }
    let picked = pick_decode(codec, prefs, &Caps::for_decode(prefs)).ok()?;
    Some(picked.route)
}

fn failed() -> Error {
    Error::Backend("a still worker failed".to_string())
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
