use std::borrow::Cow;
use std::f64::consts::PI;

use rayon::prelude::*;

use crate::media::fft::Fft;
use crate::media::{AudioBuffer, Error, Result, Rgba};

const SCALE: i32 = 2;
const ADVANCE: i32 = 6 * SCALE;
const GLYPH_HEIGHT: i32 = 7 * SCALE;
const TOP: i32 = 30;
const BOTTOM: i32 = 34;
const LEFT: i32 = 60;
const RIGHT: i32 = 92;
const BAR_GAP: i32 = 16;
const BAR_WIDTH: i32 = 16;
const TICK: i32 = 5;
const GAP: i32 = 4;
const TITLE_GAP: i32 = 8;
const LARGEST: u32 = 16_384;
pub const MIN_WIDTH: u32 = 320;
pub const MIN_HEIGHT: u32 = 160;
pub const LOWEST_FLOOR_DB: f64 = -200.0;
pub const HIGHEST_FLOOR_DB: f64 = -6.0;

pub(crate) const BACKGROUND: [u8; 3] = [16, 19, 25];
pub(crate) const TEXT: [u8; 3] = [232, 235, 240];
const FRAME: [u8; 3] = [70, 78, 92];

const INFERNO: [[u8; 3]; 10] = [
    [0, 0, 4],
    [27, 12, 66],
    [75, 12, 107],
    [120, 28, 109],
    [165, 44, 96],
    [207, 68, 70],
    [237, 105, 37],
    [251, 154, 6],
    [247, 208, 60],
    [252, 255, 164],
];

const GLYPHS: &[(char, [u8; 7])] = &[
    ('0', [14, 17, 19, 21, 25, 17, 14]),
    ('1', [4, 12, 4, 4, 4, 4, 14]),
    ('2', [14, 17, 1, 2, 4, 8, 31]),
    ('3', [30, 1, 1, 14, 1, 1, 30]),
    ('4', [2, 6, 10, 18, 31, 2, 2]),
    ('5', [31, 16, 30, 1, 1, 17, 14]),
    ('6', [6, 8, 16, 30, 17, 17, 14]),
    ('7', [31, 1, 2, 4, 8, 8, 8]),
    ('8', [14, 17, 17, 14, 17, 17, 14]),
    ('9', [14, 17, 17, 15, 1, 2, 12]),
    ('.', [0, 0, 0, 0, 0, 12, 12]),
    (':', [0, 12, 12, 0, 12, 12, 0]),
    ('-', [0, 0, 0, 31, 0, 0, 0]),
    ('B', [30, 17, 17, 30, 17, 17, 30]),
    ('H', [17, 17, 17, 31, 17, 17, 17]),
    ('d', [1, 1, 13, 19, 17, 17, 15]),
    ('k', [16, 16, 18, 20, 24, 20, 18]),
    ('s', [0, 0, 15, 16, 14, 1, 30]),
    ('z', [0, 0, 31, 2, 4, 8, 31]),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    pub width: u32,
    pub height: u32,
    pub max_hz: f64,
    pub floor_db: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            width: 1200,
            height: 360,
            max_hz: 12_000.0,
            floor_db: -90.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Time,
    Frequency,
    Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub axis: Axis,
    pub value: Option<f64>,
    pub text: String,
    pub at: i32,
    pub area: Area,
}

#[derive(Debug, Clone)]
pub struct Spectrogram {
    pub image: Rgba,
    pub plot: Area,
    pub bar: Area,
    pub labels: Vec<Label>,
    pub seconds: f64,
    pub window: usize,
}

pub fn window_for(rate: u32) -> usize {
    ((f64::from(rate) / 24.0).ceil() as usize)
        .next_power_of_two()
        .clamp(256, 8192)
}

fn check(audio: &AudioBuffer, opts: &Options) -> Result<()> {
    if !(MIN_WIDTH..=LARGEST).contains(&opts.width)
        || !(MIN_HEIGHT..=LARGEST).contains(&opts.height)
    {
        return Err(Error::Invalid(format!(
            "a spectrogram of {}x{}: use {MIN_WIDTH}x{MIN_HEIGHT} to {LARGEST}x{LARGEST}",
            opts.width, opts.height
        )));
    }
    if !opts.max_hz.is_finite() || opts.max_hz < 1.0 {
        return Err(Error::Invalid(format!(
            "a top frequency of {} Hz",
            opts.max_hz
        )));
    }
    if !(LOWEST_FLOOR_DB..=HIGHEST_FLOOR_DB).contains(&opts.floor_db) {
        return Err(Error::Invalid(format!(
            "a floor of {} dB: use {LOWEST_FLOOR_DB} to {HIGHEST_FLOOR_DB}",
            opts.floor_db
        )));
    }
    if audio.rate == 0 || audio.channels == 0 {
        return Err(Error::Invalid(format!(
            "audio of {} channels at {} Hz",
            audio.channels, audio.rate
        )));
    }
    if audio.frames() == 0 {
        return Err(Error::Invalid("no audio to draw".into()));
    }
    Ok(())
}

fn mono(audio: &AudioBuffer) -> Cow<'_, [f32]> {
    let c = usize::from(audio.channels);
    if c == 1 {
        return Cow::Borrowed(&audio.samples[..audio.frames()]);
    }
    let scale = 1.0 / c as f32;
    Cow::Owned(
        audio
            .samples
            .chunks_exact(c)
            .map(|f| f.iter().sum::<f32>() * scale)
            .collect(),
    )
}

struct Analysis {
    n: usize,
    fft: Fft,
    window: Vec<f32>,
    reference: f64,
    rows: Vec<Option<(usize, usize)>>,
}

impl Analysis {
    fn new(rate: u32, max_hz: f64, rows: usize) -> Analysis {
        let n = window_for(rate);
        let window: Vec<f32> = (0..n)
            .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()) as f32)
            .collect();
        let gain = window.iter().map(|&w| f64::from(w)).sum::<f64>() / 2.0;
        let df = f64::from(rate) / n as f64;
        let half = n / 2;
        let rows = (0..rows)
            .map(|r| {
                let hi_hz = max_hz * (rows - r) as f64 / rows as f64;
                let lo_hz = max_hz * (rows - r - 1) as f64 / rows as f64;
                let lo = (lo_hz / df).ceil() as usize;
                let hi = (hi_hz / df).floor() as usize;
                let (lo, hi) = if hi < lo {
                    let mid = ((lo_hz + hi_hz) / 2.0 / df).round() as usize;
                    (mid, mid)
                } else {
                    (lo, hi)
                };
                (lo <= half).then(|| (lo, hi.min(half)))
            })
            .collect();
        Analysis {
            n,
            fft: Fft::new(n),
            window,
            reference: gain * gain,
            rows,
        }
    }

    fn load(&self, samples: &[f32], start: isize, out: &mut [f32]) {
        for (i, (o, &w)) in out.iter_mut().zip(&self.window).enumerate() {
            let at = start + i as isize;
            *o = if at >= 0 && (at as usize) < samples.len() {
                samples[at as usize] * w
            } else {
                0.0
            };
        }
    }

    fn column(&self, samples: &[f32], from: f64, to: f64, scratch: &mut Scratch, floor: f64) {
        let n = self.n;
        let half = n / 2;
        let count = ((to - from) / half as f64).ceil().max(1.0) as usize;
        let start = |j: usize| {
            let centre = from + (j as f64 + 0.5) * (to - from) / count as f64;
            centre.floor() as isize - half as isize
        };
        scratch.power.clear();
        scratch.power.resize(half + 1, 0.0);
        for j in (0..count).step_by(2) {
            self.load(samples, start(j), &mut scratch.re);
            if j + 1 < count {
                self.load(samples, start(j + 1), &mut scratch.im);
            } else {
                scratch.im.fill(0.0);
            }
            self.fft.run(&mut scratch.re, &mut scratch.im);
            let (re, im) = (&scratch.re, &scratch.im);
            for (k, p) in scratch.power.iter_mut().enumerate() {
                let m = (n - k) % n;
                let (ar, ai) = (re[k] + re[m], im[k] - im[m]);
                let (br, bi) = (im[k] + im[m], re[m] - re[k]);
                *p += f64::from(ar * ar + ai * ai + br * br + bi * bi) / 4.0;
            }
        }
        let norm = 1.0 / (count as f64 * self.reference);
        scratch.levels.clear();
        scratch.levels.extend(self.rows.iter().map(|range| {
            let Some((lo, hi)) = *range else {
                return f32::NAN;
            };
            let p = scratch.power[lo..=hi].iter().fold(0.0f64, |a, &b| a.max(b));
            let db = 10.0 * (p * norm).max(1e-30).log10();
            ((db - floor) / -floor).clamp(0.0, 1.0) as f32
        }));
    }
}

#[derive(Default)]
struct Scratch {
    re: Vec<f32>,
    im: Vec<f32>,
    power: Vec<f64>,
    levels: Vec<f32>,
}

pub fn color(t: f32) -> [u8; 3] {
    let pos = t.clamp(0.0, 1.0) * (INFERNO.len() - 1) as f32;
    let i = (pos.floor() as usize).min(INFERNO.len() - 2);
    let f = pos - i as f32;
    let (a, b) = (INFERNO[i], INFERNO[i + 1]);
    [0, 1, 2].map(|c| (f32::from(a[c]) + (f32::from(b[c]) - f32::from(a[c])) * f).round() as u8)
}

struct Canvas {
    image: Rgba,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Canvas {
        let mut image = Rgba::new(width, height);
        for px in image.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], 255]);
        }
        Canvas { image }
    }

    fn put(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x < 0 || y < 0 || x >= self.image.width as i32 || y >= self.image.height as i32 {
            return;
        }
        let at = (y as usize * self.image.width as usize + x as usize) * 4;
        self.image.data[at..at + 3].copy_from_slice(&c);
    }

    fn fill(&mut self, a: Area, c: [u8; 3]) {
        for y in a.y..a.y + a.height {
            for x in a.x..a.x + a.width {
                self.put(x, y, c);
            }
        }
    }

    fn frame(&mut self, a: Area) {
        self.fill(
            Area {
                x: a.x - 1,
                y: a.y - 1,
                width: a.width + 2,
                height: a.height + 2,
            },
            FRAME,
        );
    }

    fn text(&mut self, x: i32, y: i32, text: &str) {
        for (i, ch) in text.chars().enumerate() {
            let rows = glyph(ch);
            let pen = x + i as i32 * ADVANCE;
            for (r, bits) in rows.iter().enumerate() {
                for col in 0..5 {
                    if bits >> (4 - col) & 1 == 1 {
                        self.fill(
                            Area {
                                x: pen + col * SCALE,
                                y: y + r as i32 * SCALE,
                                width: SCALE,
                                height: SCALE,
                            },
                            TEXT,
                        );
                    }
                }
            }
        }
    }
}

fn glyph(c: char) -> [u8; 7] {
    GLYPHS
        .iter()
        .find(|(g, _)| *g == c)
        .or_else(|| GLYPHS.iter().find(|(g, _)| *g == c.to_ascii_uppercase()))
        .map_or([0; 7], |(_, rows)| *rows)
}

pub fn text_width(text: &str) -> i32 {
    text.chars().count() as i32 * ADVANCE - SCALE
}

fn nice(raw: f64) -> f64 {
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw * (1.0 - 1e-9))
        .unwrap_or(10.0 * mag)
}

fn number(v: f64) -> String {
    let v = if v == 0.0 { 0.0 } else { v };
    if (v - v.round()).abs() < 1e-9 {
        format!("{:.0}", v.round())
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn time_step(seconds: f64, target: usize) -> f64 {
    let raw = seconds / target as f64;
    if raw < 10.0 {
        return nice(raw);
    }
    [
        10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0,
    ]
    .into_iter()
    .find(|s| *s >= raw)
    .unwrap_or_else(|| nice(raw / 3600.0) * 3600.0)
}

fn time_text(t: f64, seconds: f64, step: f64) -> String {
    if seconds >= 60.0 {
        let whole = t.round() as u64;
        let (h, m, s) = (whole / 3600, whole / 60 % 60, whole % 60);
        return if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m}:{s:02}")
        };
    }
    let decimals = (-step.log10().floor()).max(0.0) as usize;
    format!("{t:.decimals$}s")
}

fn ticks(top: f64, step: f64) -> Vec<f64> {
    let count = (top / step + 1e-9).floor() as usize;
    (0..=count).map(|k| k as f64 * step).collect()
}

struct Layout {
    width: i32,
    height: i32,
    plot: Area,
    bar: Area,
}

impl Layout {
    fn new(opts: &Options) -> Layout {
        let (width, height) = (opts.width as i32, opts.height as i32);
        let plot = Area {
            x: LEFT,
            y: TOP,
            width: width - LEFT - RIGHT,
            height: height - TOP - BOTTOM,
        };
        let bar = Area {
            x: plot.x + plot.width + BAR_GAP,
            y: plot.y,
            width: BAR_WIDTH,
            height: plot.height,
        };
        Layout {
            width,
            height,
            plot,
            bar,
        }
    }

    fn row_of(&self, area: Area, fraction: f64) -> i32 {
        let y = area.y + (fraction * f64::from(area.height)).round() as i32;
        y.clamp(area.y, area.y + area.height - 1)
    }

    fn left_label(&self, axis: Axis, value: f64, text: String, row: i32, right: i32) -> Label {
        let w = text_width(&text);
        Label {
            axis,
            value: Some(value),
            area: Area {
                x: right - w,
                y: (row - GLYPH_HEIGHT / 2).clamp(0, self.height - GLYPH_HEIGHT),
                width: w,
                height: GLYPH_HEIGHT,
            },
            text,
            at: row,
        }
    }

    fn labels(&self, opts: &Options, seconds: f64) -> Vec<Label> {
        let (plot, bar) = (self.plot, self.bar);
        let mut out = Vec::new();
        let (div, unit) = if opts.max_hz >= 2000.0 {
            (1000.0, "kHz")
        } else {
            (1.0, "Hz")
        };
        let top = opts.max_hz / div;
        let rows_target = (plot.height / 40).clamp(2, 6) as f64;
        let right = plot.x - TICK - GAP;
        for v in ticks(top, nice(top / rows_target)) {
            let row = self.row_of(plot, 1.0 - v / top);
            out.push(self.left_label(Axis::Frequency, v * div, number(v), row, right));
        }
        out.push(title(
            Axis::Frequency,
            unit,
            right - text_width(unit),
            plot.y,
        ));
        let cols_target = (plot.width / 110).clamp(2, 10) as usize;
        let step = time_step(seconds, cols_target);
        for t in ticks(seconds, step) {
            let text = time_text(t, seconds, step);
            let w = text_width(&text);
            let col = (plot.x + (t / seconds * f64::from(plot.width)).round() as i32)
                .clamp(plot.x, plot.x + plot.width - 1);
            out.push(Label {
                axis: Axis::Time,
                value: Some(t),
                area: Area {
                    x: (col - w / 2).clamp(0, self.width - w),
                    y: plot.y + plot.height + TICK + GAP,
                    width: w,
                    height: GLYPH_HEIGHT,
                },
                text,
                at: col,
            });
        }
        let range = -opts.floor_db;
        let step = nice(range / rows_target);
        let left = bar.x + bar.width + TICK + GAP;
        for v in ticks(range, step) {
            let row = self.row_of(bar, v / range);
            let text = number(-v);
            let w = text_width(&text);
            out.push(self.left_label(Axis::Level, -v, text, row, left + w));
        }
        out.push(title(Axis::Level, "dB", bar.x, plot.y));
        out
    }
}

fn title(axis: Axis, text: &str, x: i32, plot_top: i32) -> Label {
    let y = plot_top - TITLE_GAP - GLYPH_HEIGHT;
    Label {
        axis,
        value: None,
        text: text.to_string(),
        at: y,
        area: Area {
            x,
            y,
            width: text_width(text),
            height: GLYPH_HEIGHT,
        },
    }
}

pub fn spectrogram(audio: &AudioBuffer, opts: &Options) -> Result<Spectrogram> {
    check(audio, opts)?;
    let layout = Layout::new(opts);
    let (plot, bar) = (layout.plot, layout.bar);
    let samples = mono(audio);
    let frames = samples.len();
    let seconds = frames as f64 / f64::from(audio.rate);
    let rows = plot.height as usize;
    let cols = plot.width as usize;
    let analysis = Analysis::new(audio.rate, opts.max_hz, rows);
    let n = analysis.n;
    let span = frames as f64 / cols as f64;
    let mut levels = vec![0f32; rows * cols];
    levels.par_chunks_mut(rows).enumerate().for_each_init(
        || Scratch {
            re: vec![0.0; n],
            im: vec![0.0; n],
            ..Scratch::default()
        },
        |scratch, (c, out)| {
            let from = c as f64 * span;
            analysis.column(&samples, from, from + span, scratch, opts.floor_db);
            out.copy_from_slice(&scratch.levels);
        },
    );
    let mut canvas = Canvas::new(opts.width, opts.height);
    canvas.frame(plot);
    canvas.frame(bar);
    for (c, column) in levels.chunks_exact(rows).enumerate() {
        for (r, &t) in column.iter().enumerate() {
            let rgb = if t.is_nan() { BACKGROUND } else { color(t) };
            canvas.put(plot.x + c as i32, plot.y + r as i32, rgb);
        }
    }
    for r in 0..bar.height {
        let t = 1.0 - r as f32 / (bar.height - 1).max(1) as f32;
        let c = color(t);
        for x in bar.x..bar.x + bar.width {
            canvas.put(x, bar.y + r, c);
        }
    }
    let labels = layout.labels(opts, seconds);
    for l in &labels {
        if l.value.is_some() {
            let tick = match l.axis {
                Axis::Frequency => Area {
                    x: plot.x - TICK,
                    y: l.at,
                    width: TICK - 1,
                    height: 1,
                },
                Axis::Time => Area {
                    x: l.at,
                    y: plot.y + plot.height + 1,
                    width: 1,
                    height: TICK - 1,
                },
                Axis::Level => Area {
                    x: bar.x + bar.width + 1,
                    y: l.at,
                    width: TICK - 1,
                    height: 1,
                },
            };
            canvas.fill(tick, FRAME);
        }
        canvas.text(l.area.x, l.area.y, &l.text);
    }
    Ok(Spectrogram {
        image: canvas.image,
        plot,
        bar,
        labels,
        seconds,
        window: n,
    })
}
