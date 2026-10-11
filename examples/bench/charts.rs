use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

type Color = [u8; 3];

const BG: Color = [16, 19, 25];
const TEXT: Color = [232, 235, 240];
const DIM: Color = [138, 146, 160];
const GRID: Color = [44, 50, 62];
const OURS: Color = [255, 138, 61];
const FFMPEG: Color = [98, 110, 128];
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

pub struct Entry {
    pub label: String,
    pub ours: f64,
    pub ffmpeg: f64,
    pub ours_text: String,
    pub ffmpeg_text: String,
    pub speedup: f64,
}

fn x_text(r: f64) -> String {
    if r >= 10.0 {
        format!("{r:.0}X")
    } else {
        format!("{r:.1}X")
    }
}

pub fn grouped(
    path: &Path,
    title: &str,
    subtitle: &str,
    foot: &str,
    entries: &[Entry],
) -> Result<(), Box<dyn std::error::Error>> {
    let width = 1600usize;
    let top = 150i64;
    let row = 70i64;
    let height = top as usize + entries.len() * row as usize + 60;
    let mut c = Canvas::new(width, height);
    c.text(40, 34, title, 4, TEXT);
    c.text(40, 78, subtitle, 2, DIM);
    c.rect(40, 112, 22, 14, OURS);
    c.text(70, 112, "PITO-CODECS", 2, TEXT);
    c.rect(250, 112, 22, 14, FFMPEG);
    c.text(280, 112, "FFMPEG", 2, TEXT);
    let label_w = 560i64;
    let bar_x = 40 + label_w + 20;
    let bar_max = 560i64;
    for (i, e) in entries.iter().enumerate() {
        let y = top + i as i64 * row;
        if i % 2 == 0 {
            c.rect(30, y - 4, width as i64 - 60, row - 4, [21, 25, 33]);
        }
        c.text(40, y + 22, &e.label, 2, TEXT);
        let peak = e.ours.max(e.ffmpeg).max(f64::MIN_POSITIVE);
        let len = |v: f64| ((v / peak) * bar_max as f64).round().max(2.0) as i64;
        c.rect(bar_x, y + 6, len(e.ours), 22, OURS);
        c.text(bar_x + len(e.ours) + 12, y + 9, &e.ours_text, 2, TEXT);
        c.rect(bar_x, y + 34, len(e.ffmpeg), 22, FFMPEG);
        c.text(bar_x + len(e.ffmpeg) + 12, y + 37, &e.ffmpeg_text, 2, DIM);
        let tag = x_text(e.speedup);
        let colour = if e.speedup >= 1.0 { FASTER } else { SLOWER };
        let w = Canvas::text_width(&tag, 3);
        c.text(width as i64 - 50 - w, y + 20, &tag, 3, colour);
    }
    c.text(40, top + entries.len() as i64 * row + 14, foot, 2, DIM);
    c.save(path)
}

pub fn speedup(
    path: &Path,
    title: &str,
    subtitle: &str,
    entries: &[(String, f64)],
) -> Result<(), Box<dyn std::error::Error>> {
    let width = 1600usize;
    let top = 170i64;
    let row = 34i64;
    let height = top as usize + entries.len() * row as usize + 70;
    let mut c = Canvas::new(width, height);
    c.text(40, 34, title, 4, TEXT);
    c.text(40, 78, subtitle, 2, DIM);
    let (lo, hi) = (0.1f64.ln(), 50f64.ln());
    let label_w = 640i64;
    let ax = 40 + label_w + 20;
    let aw = width as i64 - ax - 60;
    let at = |r: f64| ax + ((r.ln() - lo) / (hi - lo) * aw as f64).round() as i64;
    let bottom = top + entries.len() as i64 * row;
    for t in [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 25.0, 50.0] {
        let x = at(t);
        let colour = if t == 1.0 { DIM } else { GRID };
        c.rect(
            x,
            top - 10,
            if t == 1.0 { 2 } else { 1 },
            bottom - top + 14,
            colour,
        );
        let label = x_text(t);
        let w = Canvas::text_width(&label, 2);
        c.text(x - w / 2, top - 34, &label, 2, DIM);
    }
    for (i, (label, r)) in entries.iter().enumerate() {
        let y = top + i as i64 * row;
        c.text(40, y + 6, label, 2, TEXT);
        let r = r.clamp(0.1, 50.0);
        let (x0, x1) = (at(1.0), at(r));
        let colour = if r >= 1.0 { FASTER } else { SLOWER };
        c.rect(x0.min(x1), y + 2, (x1 - x0).abs().max(2), 22, colour);
        let tag = x_text(r);
        if r >= 1.0 {
            c.text(x1 + 10, y + 6, &tag, 2, TEXT);
        } else {
            let w = Canvas::text_width(&tag, 2);
            c.text(x1 - 10 - w, y + 6, &tag, 2, TEXT);
        }
    }
    c.text(
        ax,
        bottom + 26,
        "SLOWER THAN FFMPEG <    > FASTER THAN FFMPEG",
        2,
        DIM,
    );
    c.save(path)
}
