use std::collections::VecDeque;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const DOTS: [char; 7] = ['⣀', '⣄', '⣤', '⣦', '⣶', '⣷', '⣿'];
const BAR: usize = 20;
const FRAME: Duration = Duration::from_millis(100);
const WINDOW: Duration = Duration::from_secs(5);
const CYAN: &str = "\x1b[36m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub fn enabled(json: bool, no_progress: bool) -> bool {
    !json
        && !no_progress
        && std::io::stderr().is_terminal()
        && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
}

fn coloured() -> bool {
    let var = std::env::var_os;
    !var("NO_COLOR").is_some_and(|v| !v.is_empty()) && var("CLICOLOR").is_none_or(|v| v != "0")
}

#[derive(Clone, Default)]
struct Snapshot {
    name: Option<String>,
    step: usize,
    steps: usize,
    total: Option<u64>,
    frames: u64,
    elapsed: Duration,
    fps: Option<f64>,
}

struct State {
    snapshot: Snapshot,
    started: Instant,
    samples: VecDeque<(Instant, u64)>,
    width: usize,
    tick: usize,
    done: bool,
}

impl State {
    fn at(&self, now: Instant) -> Snapshot {
        Snapshot {
            elapsed: now.duration_since(self.started),
            ..self.snapshot.clone()
        }
    }
}

struct Shared {
    state: Mutex<State>,
    colour: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct Progress {
    shared: Option<Arc<Shared>>,
    painter: Mutex<Option<JoinHandle<()>>>,
    stop: Arc<AtomicBool>,
}

fn shared() -> Arc<Shared> {
    Arc::new(Shared {
        state: Mutex::new(State {
            snapshot: Snapshot::default(),
            started: Instant::now(),
            samples: VecDeque::new(),
            width: 0,
            tick: 0,
            done: false,
        }),
        colour: coloured(),
    })
}

impl Progress {
    pub fn new(enabled: bool) -> Progress {
        let stop = Arc::new(AtomicBool::new(false));
        if !enabled {
            return Progress {
                shared: None,
                painter: Mutex::new(None),
                stop,
            };
        }
        let shared = shared();
        let painter = {
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("pcod-progress".into())
                .spawn(move || paint(&shared, &stop))
                .ok()
        };
        Progress {
            shared: painter.is_some().then_some(shared),
            painter: Mutex::new(painter),
            stop,
        }
    }

    pub fn step(&self, name: &str, index: usize, total: usize, frames: Option<u64>) {
        let Some(shared) = &self.shared else { return };
        let now = Instant::now();
        let mut s = shared.lock();
        s.snapshot = Snapshot {
            name: Some(name.to_string()),
            step: index,
            steps: total,
            total: frames,
            frames: 0,
            elapsed: Duration::ZERO,
            fps: None,
        };
        s.started = now;
        s.samples.clear();
        s.samples.push_back((now, 0));
    }

    pub fn frame(&self, done: u64) {
        let Some(shared) = &self.shared else { return };
        let now = Instant::now();
        let mut s = shared.lock();
        s.snapshot.frames = done;
        if s.samples
            .back()
            .is_some_and(|(at, _)| now.duration_since(*at) < FRAME / 4)
        {
            if let Some(last) = s.samples.back_mut() {
                last.1 = done;
            }
        } else {
            s.samples.push_back((now, done));
        }
        while s.samples.len() > 2
            && s.samples
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at) > WINDOW)
        {
            s.samples.pop_front();
        }
        s.snapshot.fps = rate(&s.samples);
    }

    pub fn clear(&self) {
        let Some(shared) = &self.shared else { return };
        self.stop.store(true, Ordering::SeqCst);
        let painter = self
            .painter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(painter) = painter {
            let _ = painter.join();
        }
        let mut s = shared.lock();
        if !s.done && s.width > 0 {
            let mut stderr = std::io::stderr().lock();
            let _ = write!(stderr, "\r{}\r", " ".repeat(s.width));
            let _ = stderr.flush();
        }
        s.width = 0;
        s.done = true;
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(shared) = &self.shared {
            shared.lock().done = true;
        }
    }
}

fn paint(shared: &Shared, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        {
            let mut s = shared.lock();
            if s.done {
                return;
            }
            if s.snapshot.name.is_some() {
                let snapshot = s.at(Instant::now());
                let (text, shown) = line(&snapshot, s.tick, shared.colour);
                let pad = " ".repeat(s.width.saturating_sub(shown));
                let mut stderr = std::io::stderr().lock();
                let _ = write!(stderr, "\r{text}{pad}");
                let _ = stderr.flush();
                s.width = shown;
            }
            s.tick = s.tick.wrapping_add(1);
        }
        thread::sleep(FRAME);
    }
}

fn rate(samples: &VecDeque<(Instant, u64)>) -> Option<f64> {
    let (first, last) = (samples.front()?, samples.back()?);
    let seconds = last.0.duration_since(first.0).as_secs_f64();
    (seconds >= 0.5 && last.1 > first.1).then(|| (last.1 - first.1) as f64 / seconds)
}

fn line(snapshot: &Snapshot, tick: usize, colour: bool) -> (String, usize) {
    let paint = |code: &str, text: String| {
        if colour {
            format!("{code}{text}{RESET}")
        } else {
            text
        }
    };
    let mut label = format!(
        "{} {}",
        paint(CYAN, SPINNER[tick % SPINNER.len()].to_string()),
        snapshot.name.as_deref().unwrap_or("")
    );
    if snapshot.steps > 1 {
        label.push_str(&format!(" {}/{}", snapshot.step, snapshot.steps));
    }
    let mut parts = vec![label];
    match snapshot.total.filter(|total| *total > 0) {
        Some(total) => {
            let fraction = (snapshot.frames as f64 / total as f64).clamp(0.0, 1.0);
            parts.push(bar(fraction, colour));
            parts.push(paint(BOLD, format!("{:>3.0}%", (fraction * 100.0).floor())));
            parts.push(speed(snapshot.fps));
            let eta = snapshot
                .fps
                .and_then(|fps| {
                    Duration::try_from_secs_f64(total.saturating_sub(snapshot.frames) as f64 / fps)
                        .ok()
                })
                .filter(|eta| *eta < Duration::from_secs(100 * 3600));
            parts.push(paint(
                DIM,
                format!(
                    "ETA {}",
                    eta.map_or("-:--".into(), |eta| clock(Duration::from_secs(
                        eta.as_secs_f64().ceil() as u64
                    )))
                ),
            ));
        }
        None if snapshot.frames > 0 => {
            parts.push(format!("{} frames", snapshot.frames));
            parts.push(speed(snapshot.fps));
            parts.push(paint(DIM, clock(snapshot.elapsed)));
        }
        None => parts.push(paint(DIM, clock(snapshot.elapsed))),
    }
    let shown = parts.iter().map(|part| visible(part)).sum::<usize>() + 2 * (parts.len() - 1);
    (parts.join("  "), shown)
}

fn bar(fraction: f64, colour: bool) -> String {
    let cells = fraction * BAR as f64;
    let full = (cells.floor() as usize).min(BAR);
    let mut text: String = std::iter::repeat_n(DOTS[DOTS.len() - 1], full).collect();
    if full < BAR {
        let part = ((cells - full as f64) * (DOTS.len() - 1) as f64).floor() as usize;
        text.push(DOTS[part.min(DOTS.len() - 1)]);
        text.extend(std::iter::repeat_n(DOTS[0], BAR - full - 1));
    }
    if colour {
        format!("{CYAN}{text}{RESET}")
    } else {
        text
    }
}

fn speed(fps: Option<f64>) -> String {
    match fps {
        Some(fps) if fps >= 10.0 => format!("{fps:.0} fps"),
        Some(fps) => format!("{fps:.1} fps"),
        None => "- fps".into(),
    }
}

fn clock(time: Duration) -> String {
    let seconds = time.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn visible(text: &str) -> usize {
    let mut count = 0;
    let mut escape = false;
    for c in text.chars() {
        match c {
            '\x1b' => escape = true,
            'm' if escape => escape = false,
            _ if escape => {}
            _ => count += 1,
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(total: Option<u64>, frames: u64, fps: Option<f64>, steps: usize) -> Snapshot {
        Snapshot {
            name: Some("encode".into()),
            step: 1,
            steps,
            total,
            frames,
            elapsed: Duration::from_secs(75),
            fps,
        }
    }

    #[test]
    fn a_known_total_draws_a_bar_a_percentage_a_speed_and_an_eta() {
        let (text, shown) = line(&snapshot(Some(200), 50, Some(25.0), 2), 0, false);
        assert_eq!(
            text,
            format!(
                "⠋ encode 1/2  {}{}   25%  25 fps  ETA 0:06",
                "⣿".repeat(5),
                "⣀".repeat(15)
            )
        );
        assert_eq!(shown, text.chars().count());
        let (text, _) = line(&snapshot(Some(200), 0, None, 1), 11, false);
        assert_eq!(
            text,
            format!("⠙ encode  {}    0%  - fps  ETA -:--", "⣀".repeat(20))
        );
        let (text, _) = line(&snapshot(Some(10), 40, Some(30.0), 1), 0, false);
        assert!(text.contains("⣿".repeat(20).as_str()), "{text}");
        assert!(text.ends_with(" 100%  30 fps  ETA 0:00"), "{text}");
        let (text, _) = line(&snapshot(Some(10), 5, Some(1e-9), 1), 0, false);
        assert!(text.ends_with("  50%  0.0 fps  ETA -:--"), "{text}");
    }

    #[test]
    fn an_unknown_total_counts_frames_or_the_clock() {
        let (text, _) = line(&snapshot(None, 30, Some(7.3), 1), 0, false);
        assert_eq!(text, "⠋ encode  30 frames  7.3 fps  1:15");
        let (text, _) = line(&snapshot(Some(0), 0, None, 1), 0, false);
        assert_eq!(text, "⠋ encode  1:15");
        let mut long = snapshot(None, 0, None, 1);
        long.elapsed = Duration::from_secs(3 * 3600 + 7 * 60 + 9);
        assert_eq!(line(&long, 0, false).0, "⠋ encode  3:07:09");
    }

    #[test]
    fn colour_adds_escapes_that_do_not_count_as_width() {
        let snap = snapshot(Some(200), 100, Some(50.0), 1);
        let (plain, plain_width) = line(&snap, 3, false);
        let (painted, painted_width) = line(&snap, 3, true);
        assert!(painted.contains(CYAN) && painted.contains(BOLD) && painted.contains(DIM));
        assert!(!plain.contains('\x1b'));
        assert_eq!(painted_width, plain_width);
        assert_eq!(visible(&painted), visible(&plain));
        assert_eq!(visible("\x1b[36mab\x1b[0mc"), 3);
    }

    #[test]
    fn the_bar_fills_by_eighths_of_a_cell() {
        assert_eq!(bar(0.0, false), "⣀".repeat(20));
        assert_eq!(bar(1.0, false), "⣿".repeat(20));
        assert_eq!(bar(0.125, false), format!("⣿⣿⣦{}", "⣀".repeat(17)));
        assert_eq!(
            bar(0.5, true),
            format!("{CYAN}{}{}{RESET}", "⣿".repeat(10), "⣀".repeat(10))
        );
        assert_eq!(speed(Some(120.4)), "120 fps");
        assert_eq!(speed(Some(9.96)), "10.0 fps");
        assert_eq!(speed(None), "- fps");
        assert_eq!(clock(Duration::from_secs(59)), "0:59");
        assert_eq!(clock(Duration::from_secs(3600)), "1:00:00");
    }

    #[test]
    fn the_rate_needs_half_a_second_and_progress() {
        let now = Instant::now();
        let mut samples = VecDeque::new();
        assert_eq!(rate(&samples), None);
        samples.push_back((now, 0));
        samples.push_back((now + Duration::from_millis(400), 10));
        assert_eq!(rate(&samples), None);
        samples.push_back((now + Duration::from_secs(2), 100));
        assert_eq!(rate(&samples), Some(50.0));
        samples.push_back((now + Duration::from_secs(3), 100));
        assert_eq!(rate(&samples), Some(100.0 / 3.0));
        let stalled: VecDeque<_> = [(now, 5), (now + Duration::from_secs(1), 5)].into();
        assert_eq!(rate(&stalled), None);
    }

    #[test]
    fn a_disabled_progress_does_nothing_and_an_enabled_one_tracks_frames_without_painting() {
        let off = Progress::new(false);
        off.step("encode", 1, 1, Some(10));
        off.frame(5);
        off.clear();
        assert!(off.shared.is_none());

        let on = Progress {
            shared: Some(shared()),
            painter: Mutex::new(None),
            stop: Arc::new(AtomicBool::new(false)),
        };
        on.step("encode", 2, 3, Some(100));
        for done in [10, 20, 30] {
            on.frame(done);
        }
        {
            let shared = on.shared.as_ref().unwrap();
            let s = shared.lock();
            assert_eq!(s.snapshot.frames, 30);
            assert_eq!((s.snapshot.step, s.snapshot.steps), (2, 3));
            assert_eq!(s.snapshot.total, Some(100));
        }
        on.step("mux", 3, 3, None);
        {
            let s = on.shared.as_ref().unwrap().lock();
            assert_eq!(s.snapshot.frames, 0);
            assert_eq!(s.snapshot.name.as_deref(), Some("mux"));
        }
        on.clear();
        let s = on.shared.as_ref().unwrap().lock();
        assert!(s.done);
        assert_eq!(s.width, 0);
        assert!(on.stop.load(Ordering::SeqCst));
    }

    #[test]
    fn progress_is_off_for_json_and_no_progress() {
        assert!(!enabled(true, false));
        assert!(!enabled(false, true));
    }
}
