use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

type Color = [u8; 3];

const BG: Color = [16, 19, 25];
const TEXT: Color = [232, 235, 240];
const DIM: Color = [138, 146, 160];
const GRID: Color = [44, 50, 62];
const OURS: Color = [220, 110, 34];
const FFMPEG: Color = [74, 142, 230];
const SEP: Color = [70, 78, 92];
const FASTER: Color = [74, 201, 125];
const SLOWER: Color = [235, 88, 88];

const GLYPHS: &[(char, [u8; 7])] = &[
    ('A', [14, 17, 17, 31, 17, 17, 17]),
    ('B', [30, 17, 17, 30, 17, 17, 30]),
    ('C', [14, 17, 16, 16, 16, 17, 14]),
    ('D', [30, 17, 17, 17, 17, 17, 30]),
    ('E', [31, 16, 16, 30, 16, 16, 31]),
    ('F', [31, 16, 16, 30, 16, 16, 16]),
    ('G', [14, 17, 16, 23, 17, 17, 15]),
    ('H', [17, 17, 17, 31, 17, 17, 17]),
    ('I', [14, 4, 4, 4, 4, 4, 14]),
    ('J', [7, 2, 2, 2, 2, 18, 12]),
    ('K', [17, 18, 20, 24, 20, 18, 17]),
    ('L', [16, 16, 16, 16, 16, 16, 31]),
    ('M', [17, 27, 21, 21, 17, 17, 17]),
    ('N', [17, 25, 21, 19, 17, 17, 17]),
    ('O', [14, 17, 17, 17, 17, 17, 14]),
    ('P', [30, 17, 17, 30, 16, 16, 16]),
    ('Q', [14, 17, 17, 17, 21, 18, 13]),
    ('R', [30, 17, 17, 30, 20, 18, 17]),
    ('S', [15, 16, 16, 14, 1, 1, 30]),
    ('T', [31, 4, 4, 4, 4, 4, 4]),
    ('U', [17, 17, 17, 17, 17, 17, 14]),
    ('V', [17, 17, 17, 17, 17, 10, 4]),
    ('W', [17, 17, 17, 21, 21, 21, 10]),
    ('X', [17, 17, 10, 4, 10, 17, 17]),
    ('Y', [17, 17, 10, 4, 4, 4, 4]),
    ('Z', [31, 1, 2, 4, 8, 16, 31]),
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
    (',', [0, 0, 0, 0, 12, 4, 8]),
    (':', [0, 12, 12, 0, 12, 12, 0]),
    ('-', [0, 0, 0, 31, 0, 0, 0]),
    ('+', [0, 4, 4, 31, 4, 4, 0]),
    ('/', [1, 1, 2, 4, 8, 16, 16]),
    ('(', [2, 4, 8, 8, 8, 4, 2]),
    (')', [8, 4, 2, 2, 2, 4, 8]),
    ('%', [25, 26, 2, 4, 8, 11, 19]),
    ('=', [0, 0, 31, 0, 31, 0, 0]),
    ('<', [2, 4, 8, 16, 8, 4, 2]),
    ('>', [8, 4, 2, 1, 2, 4, 8]),
    ('\'', [4, 4, 8, 0, 0, 0, 0]),
    ('_', [0, 0, 0, 0, 0, 0, 31]),
    ('×', [0, 17, 10, 4, 10, 17, 0]),
    ('*', [0, 21, 14, 31, 14, 21, 0]),
    ('†', [4, 14, 4, 4, 4, 4, 0]),
    (';', [0, 12, 12, 0, 12, 4, 8]),
    ('|', [4, 4, 4, 4, 4, 4, 4]),
];

fn glyph(c: char) -> [u8; 7] {
    let c = c.to_ascii_uppercase();
    GLYPHS
        .iter()
        .find(|(g, _)| *g == c)
        .map(|(_, rows)| *rows)
        .unwrap_or([0; 7])
}

pub struct Canvas {
    width: usize,
    height: usize,
    px: Vec<u8>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        let mut px = Vec::with_capacity(width * height * 3);
        for _ in 0..width * height {
            px.extend_from_slice(&BG);
        }
        Self { width, height, px }
    }

    pub fn rect(&mut self, x: i64, y: i64, w: i64, h: i64, c: Color) {
        let x0 = x.clamp(0, self.width as i64) as usize;
        let x1 = (x + w).clamp(0, self.width as i64) as usize;
        let y0 = y.clamp(0, self.height as i64) as usize;
        let y1 = (y + h).clamp(0, self.height as i64) as usize;
        for row in y0..y1 {
            for col in x0..x1 {
                let at = (row * self.width + col) * 3;
                self.px[at..at + 3].copy_from_slice(&c);
            }
        }
    }

    pub fn text_width(text: &str, scale: usize) -> i64 {
        (text.chars().count() * 6 * scale) as i64 - scale as i64
    }

    pub fn text(&mut self, x: i64, y: i64, text: &str, scale: usize, c: Color) {
        let s = scale as i64;
        let mut pen = x;
        for ch in text.chars() {
            let rows = glyph(ch);
            for (r, bits) in rows.iter().enumerate() {
                for col in 0..5 {
                    if bits >> (4 - col) & 1 == 1 {
                        self.rect(pen + col as i64 * s, y + r as i64 * s, s, s, c);
                    }
                }
            }
            pen += 6 * s;
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = BufWriter::new(File::create(path)?);
        let mut enc = png::Encoder::new(file, self.width as u32, self.height as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Best);
        let mut w = enc.write_header()?;
        w.write_image_data(&self.px)?;
        Ok(())
    }
}

type Res = Result<(), Box<dyn std::error::Error>>;

const WIDTH: i64 = 1080;
const LEFT: i64 = 40;
const ROW: i64 = 124;
const RATIO_ROW: i64 = 72;
const GROUP: i64 = 50;
const NOTE: i64 = 30;

pub struct Pair {
    pub group: String,
    pub label: String,
    pub ours: f64,
    pub ffmpeg: f64,
    pub ours_text: String,
    pub ffmpeg_text: String,
    pub speedup: f64,
}

pub struct Ratio {
    pub group: String,
    pub label: String,
    pub speedup: f64,
}

fn wrap(text: &str, scale: usize) -> Vec<String> {
    let max = ((WIDTH - 2 * LEFT) / (6 * scale as i64)) as usize;
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > max {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn note_lines(notes: &[String]) -> Vec<String> {
    notes.iter().flat_map(|n| wrap(n, 3)).collect()
}

fn number(n: f64) -> String {
    if n >= 10.0 {
        format!("{n:.0}")
    } else {
        format!("{n:.1}")
    }
}

pub fn times(r: f64) -> String {
    let (n, word) = if r >= 1.0 {
        (r, "FASTER")
    } else {
        (1.0 / r, "SLOWER")
    };
    let n = number(n);
    if n == "1.0" {
        "SAME SPEED".into()
    } else {
        format!("{n}× {word}")
    }
}

fn tone(r: f64) -> Color {
    if number(r.max(1.0 / r)) == "1.0" {
        DIM
    } else if r >= 1.0 {
        FASTER
    } else {
        SLOWER
    }
}

fn nice_step(max: f64) -> f64 {
    let raw = max / 5.0;
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * mag)
        .find(|s| raw <= *s)
        .unwrap_or(10.0 * mag)
}

fn tick_text(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

fn header(c: &mut Canvas, title: &str, unit: &str, subtitle: &str) {
    c.text(LEFT, 32, title, 4, TEXT);
    c.text(LEFT, 80, unit, 3, TEXT);
    c.text(LEFT, 118, subtitle, 2, DIM);
}

fn group_row(c: &mut Canvas, y: i64, name: &str, first: bool) {
    if !first {
        c.rect(LEFT, y + 6, WIDTH - 2 * LEFT, 2, SEP);
    }
    c.text(LEFT, y + 22, name, 2, DIM);
}

type Slots = Vec<(i64, Option<(i64, String)>)>;

fn layout<'a>(groups: impl Iterator<Item = &'a str>, top: i64, row: i64) -> (Slots, i64) {
    let mut at = Vec::new();
    let mut y = top;
    let mut last: Option<&str> = None;
    for g in groups {
        let mut head = None;
        if !g.is_empty() && last != Some(g) {
            head = Some((y, g.to_string()));
            y += GROUP;
        }
        at.push((y, head));
        y += row;
        last = Some(g);
    }
    (at, y)
}

fn draw_groups(c: &mut Canvas, at: &Slots) {
    for (i, (gy, name)) in at.iter().filter_map(|(_, h)| h.as_ref()).enumerate() {
        group_row(c, *gy, name, i == 0);
    }
}

pub fn paired(
    path: &Path,
    title: &str,
    unit: &str,
    subtitle: &str,
    notes: &[String],
    entries: &[Pair],
) -> Res {
    let top = 250i64;
    let (at, bottom) = layout(entries.iter().map(|e| e.group.as_str()), top, ROW);
    let notes = note_lines(notes);
    let height = bottom + 40 + notes.len() as i64 * NOTE + 30;
    let mut c = Canvas::new(WIDTH as usize, height as usize);
    header(&mut c, title, unit, subtitle);
    c.rect(LEFT, 154, 30, 21, OURS);
    c.text(LEFT + 42, 154, "PITO-CODECS", 3, TEXT);
    let fx = LEFT + 42 + Canvas::text_width("PITO-CODECS", 3) + 50;
    c.rect(fx, 154, 30, 21, FFMPEG);
    c.text(fx + 42, 154, "FFMPEG", 3, TEXT);
    let max = entries
        .iter()
        .map(|e| e.ours.max(e.ffmpeg))
        .fold(0.0, f64::max)
        .max(f64::MIN_POSITIVE);
    let step = nice_step(max);
    let peak = (max / step).ceil() * step;
    let aw = WIDTH - 2 * LEFT - 230;
    let x = |v: f64| LEFT + (v / peak * aw as f64).round() as i64;
    let mut grid = Vec::new();
    let mut t = 0.0;
    while t <= peak + step / 2.0 {
        let gx = x(t);
        grid.push((gx, t == 0.0));
        let label = tick_text(t);
        let w = Canvas::text_width(&label, 2);
        c.text(
            gx - if t == 0.0 { 0 } else { w / 2 },
            top - 40,
            &label,
            2,
            DIM,
        );
        t += step;
    }
    draw_groups(&mut c, &at);
    for (e, (y, _)) in entries.iter().zip(&at) {
        let y = *y;
        for &(gx, zero) in &grid {
            c.rect(
                gx,
                y + 28,
                if zero { 2 } else { 1 },
                64,
                if zero { DIM } else { GRID },
            );
        }
        c.text(LEFT, y, &e.label, 3, TEXT);
        let tag = times(e.speedup);
        let tw = Canvas::text_width(&tag, 3);
        let lw = Canvas::text_width(&e.label, 3);
        let ty = if LEFT + lw + 24 + tw > WIDTH - LEFT {
            y + 34
        } else {
            y
        };
        c.text(WIDTH - LEFT - tw, ty, &tag, 3, tone(e.speedup));
        for (i, (v, text, colour)) in [
            (e.ours, &e.ours_text, OURS),
            (e.ffmpeg, &e.ffmpeg_text, FFMPEG),
        ]
        .into_iter()
        .enumerate()
        {
            let by = y + 32 + i as i64 * 32;
            let len = (x(v) - LEFT).max(2);
            c.rect(LEFT, by, len, 26, colour);
            c.text(LEFT + len + 12, by + 3, text, 3, TEXT);
        }
    }
    for (i, line) in notes.iter().enumerate() {
        c.text(LEFT, bottom + 30 + i as i64 * NOTE, line, 3, DIM);
    }
    c.save(path)
}

pub fn ratios(
    path: &Path,
    title: &str,
    unit: &str,
    subtitle: &str,
    notes: &[String],
    entries: &[Ratio],
) -> Res {
    let top = 270i64;
    let (at, bottom) = layout(entries.iter().map(|e| e.group.as_str()), top, RATIO_ROW);
    let notes = note_lines(notes);
    let height = bottom + 40 + notes.len() as i64 * NOTE + 30;
    let mut c = Canvas::new(WIDTH as usize, height as usize);
    header(&mut c, title, unit, subtitle);
    let worst = entries
        .iter()
        .map(|e| e.speedup.max(1.0 / e.speedup))
        .fold(1.0, f64::max);
    let decades = [2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0];
    let bound = decades.into_iter().find(|d| *d >= worst).unwrap_or(1000.0);
    let cx = WIDTH / 2;
    let half = cx - LEFT - 90;
    let x =
        |r: f64| cx + (r.clamp(1.0 / bound, bound).ln() / bound.ln() * half as f64).round() as i64;
    c.text(LEFT, 160, "< SLOWER THAN FFMPEG", 2, DIM);
    let fw = Canvas::text_width("FASTER THAN FFMPEG >", 2);
    c.text(WIDTH - LEFT - fw, 160, "FASTER THAN FFMPEG >", 2, DIM);
    let mut ticks = vec![1.0];
    ticks.extend(decades.into_iter().filter(|d| *d <= bound));
    let mut grid = Vec::new();
    for side in [1.0f64, -1.0] {
        let mut edge: Option<i64> = None;
        for &t in &ticks {
            if t == 1.0 && side < 0.0 {
                continue;
            }
            let r = t.powf(side);
            let gx = x(r);
            grid.push((gx, t == 1.0));
            let (a, b) = if t == 1.0 {
                ("1×".to_string(), "SAME")
            } else {
                (
                    format!("{}×", tick_text(t)),
                    if side > 0.0 { "FASTER" } else { "SLOWER" },
                )
            };
            let w = Canvas::text_width(&a, 2).max(Canvas::text_width(b, 2));
            let (l, r_) = (gx - w / 2, gx + w / 2);
            let fits = match edge {
                None => true,
                Some(e) if side > 0.0 => l > e + 14,
                Some(e) => r_ < e - 14,
            };
            if !fits || l < LEFT || r_ > WIDTH - LEFT {
                continue;
            }
            edge = Some(if side > 0.0 { r_ } else { l });
            c.text(gx - Canvas::text_width(&a, 2) / 2, top - 62, &a, 2, DIM);
            c.text(gx - Canvas::text_width(b, 2) / 2, top - 42, b, 2, DIM);
        }
    }
    draw_groups(&mut c, &at);
    for (e, (y, _)) in entries.iter().zip(&at) {
        let y = *y;
        for &(gx, one) in &grid {
            c.rect(
                gx,
                y + 26,
                if one { 2 } else { 1 },
                30,
                if one { DIM } else { GRID },
            );
        }
        c.text(LEFT, y, &e.label, 3, TEXT);
        let (x0, x1) = (x(1.0), x(e.speedup));
        let colour = if e.speedup >= 1.0 { FASTER } else { SLOWER };
        c.rect(x0.min(x1), y + 30, (x1 - x0).abs().max(2), 22, colour);
        let tag = format!("{}×", number(e.speedup.max(1.0 / e.speedup)));
        let tw = Canvas::text_width(&tag, 3);
        let tx = if e.speedup >= 1.0 {
            x1.max(x0) + 10
        } else {
            x1.min(x0) - 10 - tw
        };
        c.text(tx, y + 31, &tag, 3, TEXT);
    }
    for (i, line) in notes.iter().enumerate() {
        c.text(LEFT, bottom + 30 + i as i64 * NOTE, line, 3, DIM);
    }
    c.save(path)
}
