use super::color::ColorSpec;
use super::overlay::Overlay;
use crate::media::{Error, Frame, Result, Rgba};

pub const MAX_COLORS: usize = 4;
pub const DEFAULT_HEIGHT_SHARE: f64 = 0.006;
pub const DEFAULT_TRACK: Rgb = Rgb(16, 16, 24);
pub const DEFAULT_TRACK_OPACITY: f64 = 0.6;
pub const BLUE_PURPLE_PINK: [Rgb; 3] = [Rgb(59, 130, 246), Rgb(168, 85, 247), Rgb(236, 72, 153)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Thickness {
    Pixels(u32),
    ShortSide(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Bar(Edge),
    Contour { start: Corner, clockwise: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProgressBar {
    pub mode: Mode,
    pub thickness: Option<Thickness>,
    pub colors: Vec<Rgb>,
    pub track: Rgb,
    pub track_opacity: f64,
}

impl Default for ProgressBar {
    fn default() -> Self {
        ProgressBar {
            mode: Mode::Bar(Edge::Top),
            thickness: None,
            colors: vec![Rgb(255, 255, 255)],
            track: DEFAULT_TRACK,
            track_opacity: DEFAULT_TRACK_OPACITY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Side {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub horizontal: bool,
    pub reverse: bool,
}

impl Side {
    pub fn length(&self) -> u32 {
        if self.horizontal {
            self.width
        } else {
            self.height
        }
    }

    pub fn start(&self) -> (u32, u32) {
        let x = if self.horizontal && self.reverse {
            self.x + self.width.saturating_sub(1)
        } else {
            self.x
        };
        let y = if !self.horizontal && self.reverse {
            self.y + self.height.saturating_sub(1)
        } else {
            self.y
        };
        (x, y)
    }

    fn spot(&self, along: u32, across: u32) -> (u32, u32) {
        let a = if self.reverse {
            self.length().saturating_sub(1) - along
        } else {
            along
        };
        if self.horizontal {
            (a, across)
        } else {
            (across, a)
        }
    }
}

pub fn fraction(index: u64, total: u64) -> f64 {
    if total <= 1 {
        1.0
    } else {
        (index as f64 / (total - 1) as f64).clamp(0.0, 1.0)
    }
}

fn lerp(a: u8, b: u8, t: f64) -> f64 {
    f64::from(a) + (f64::from(b) - f64::from(a)) * t
}

impl ProgressBar {
    pub fn validate(&self) -> Result<()> {
        if self.colors.is_empty() || self.colors.len() > MAX_COLORS {
            return Err(Error::Invalid(format!(
                "a progress bar takes 1 to {MAX_COLORS} colours, not {}",
                self.colors.len()
            )));
        }
        if !(0.0..=1.0).contains(&self.track_opacity) {
            return Err(Error::Invalid(
                "the progress track's opacity is 0 to 1".into(),
            ));
        }
        match self.thickness {
            Some(Thickness::Pixels(0)) => Err(Error::Invalid(
                "the progress bar's thickness is at least 1 pixel".into(),
            )),
            Some(Thickness::ShortSide(s)) if !(s > 0.0 && s <= 0.5) => Err(Error::Invalid(
                "the progress bar's thickness is a share of the short side above 0 and up to 50%"
                    .into(),
            )),
            _ => Ok(()),
        }
    }

    pub fn thickness_px(&self, width: u32, height: u32) -> u32 {
        let short = width.min(height);
        let t = match self.thickness {
            Some(Thickness::Pixels(p)) => f64::from(p),
            Some(Thickness::ShortSide(s)) => f64::from(short) * s,
            None => f64::from(height) * DEFAULT_HEIGHT_SHARE,
        };
        (t.round() as u32).clamp(1, (short / 2).max(1))
    }

    pub fn color_at(&self, u: f64) -> Rgb {
        let n = self.colors.len();
        let (Some(first), Some(last)) = (self.colors.first(), self.colors.last()) else {
            return Rgb(255, 255, 255);
        };
        if n == 1 {
            return *first;
        }
        let u = u.clamp(0.0, 1.0) * (n - 1) as f64;
        let k = (u.floor() as usize).min(n - 2);
        let f = u - k as f64;
        match (self.colors.get(k), self.colors.get(k + 1)) {
            (Some(a), Some(b)) => Rgb(
                lerp(a.0, b.0, f).round() as u8,
                lerp(a.1, b.1, f).round() as u8,
                lerp(a.2, b.2, f).round() as u8,
            ),
            _ => *last,
        }
    }

    pub fn path(&self, width: u32, height: u32) -> Vec<Side> {
        let t = self.thickness_px(width, height);
        let (w, h) = (width, height);
        let side = |x, y, width, height, horizontal, reverse| Side {
            x,
            y,
            width,
            height,
            horizontal,
            reverse,
        };
        match self.mode {
            Mode::Bar(Edge::Top) => vec![side(0, 0, w, t, true, false)],
            Mode::Bar(Edge::Bottom) => vec![side(0, h - t, w, t, true, false)],
            Mode::Bar(Edge::Left) => vec![side(0, 0, t, h, false, true)],
            Mode::Bar(Edge::Right) => vec![side(w - t, 0, t, h, false, true)],
            Mode::Contour { start, clockwise } => {
                let (wt, ht) = (w - t, h - t);
                let (ring, first) = if clockwise {
                    (
                        [
                            side(0, 0, wt, t, true, false),
                            side(wt, 0, t, ht, false, false),
                            side(t, ht, wt, t, true, true),
                            side(0, t, t, ht, false, true),
                        ],
                        match start {
                            Corner::TopLeft => 0,
                            Corner::TopRight => 1,
                            Corner::BottomRight => 2,
                            Corner::BottomLeft => 3,
                        },
                    )
                } else {
                    (
                        [
                            side(0, 0, t, ht, false, false),
                            side(0, ht, wt, t, true, false),
                            side(wt, t, t, ht, false, true),
                            side(t, 0, wt, t, true, true),
                        ],
                        match start {
                            Corner::TopLeft => 0,
                            Corner::BottomLeft => 1,
                            Corner::BottomRight => 2,
                            Corner::TopRight => 3,
                        },
                    )
                };
                (0..4).map(|k| ring[(first + k) % 4]).collect()
            }
        }
    }

    pub fn strokes(&self, width: u32, height: u32, t: f64) -> Vec<(u32, u32, Rgba)> {
        let path = self.path(width, height);
        let total: f64 = path.iter().map(|s| f64::from(s.length())).sum();
        let filled = t.clamp(0.0, 1.0) * total;
        let track_alpha = (self.track_opacity.clamp(0.0, 1.0) * 255.0).round();
        let mut before = 0.0;
        let mut out = Vec::new();
        for s in &path {
            let mut img = Rgba::new(s.width, s.height);
            let stride = img.stride();
            for along in 0..s.length() {
                let at = before + f64::from(along);
                let cover = (filled - at).clamp(0.0, 1.0);
                let rgb = self.color_at(if filled > 0.0 {
                    (at + 0.5) / filled
                } else {
                    0.0
                });
                let ta = track_alpha / 255.0;
                let alpha = cover + ta * (1.0 - cover);
                let mix = |f: u8, k: u8| {
                    if alpha <= 0.0 {
                        0
                    } else {
                        ((f64::from(f) * cover + f64::from(k) * ta * (1.0 - cover)) / alpha).round()
                            as u8
                    }
                };
                let px = [
                    mix(rgb.0, self.track.0),
                    mix(rgb.1, self.track.1),
                    mix(rgb.2, self.track.2),
                    (alpha * 255.0).round() as u8,
                ];
                let across = if s.horizontal { s.height } else { s.width };
                for c in 0..across {
                    let (px_x, px_y) = s.spot(along, c);
                    let i = px_y as usize * stride + px_x as usize * 4;
                    if let Some(dst) = img.data.get_mut(i..i + 4) {
                        dst.copy_from_slice(&px);
                    }
                }
            }
            before += f64::from(s.length());
            out.push((s.x, s.y, img));
        }
        out
    }

    pub fn layer(&self, width: u32, height: u32, t: f64) -> Rgba {
        let mut full = Rgba::new(width, height);
        for (x, y, img) in self.strokes(width, height, t) {
            for row in 0..img.height {
                let src = row as usize * img.stride();
                let dst = (y + row) as usize * full.stride() + x as usize * 4;
                if let (Some(from), Some(to)) = (
                    img.data.get(src..src + img.stride()),
                    full.data.get_mut(dst..dst + img.stride()),
                ) {
                    to.copy_from_slice(from);
                }
            }
        }
        full
    }

    pub fn draw(&self, frame: &mut Frame, spec: ColorSpec, index: u64, total: u64) -> Result<()> {
        self.validate()?;
        let t = fraction(index, total);
        for (x, y, img) in self.strokes(frame.width, frame.height, t) {
            Overlay::new(&img, spec, frame.format)?.draw(frame, x as i32, y as i32)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 200;
    const H: u32 = 120;

    fn bar(mode: Mode) -> ProgressBar {
        ProgressBar {
            mode,
            thickness: Some(Thickness::Pixels(8)),
            colors: vec![Rgb(255, 255, 255)],
            track: Rgb(0, 0, 0),
            track_opacity: 0.5,
        }
    }

    fn solid(layer: &Rgba) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for y in 0..layer.height {
            for x in 0..layer.width {
                if layer.data[(y * layer.width + x) as usize * 4 + 3] == 255 {
                    out.push((x, y));
                }
            }
        }
        out
    }

    fn bounds(px: &[(u32, u32)]) -> (u32, u32, u32, u32) {
        px.iter().fold((u32::MAX, u32::MAX, 0, 0), |b, &(x, y)| {
            (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y))
        })
    }

    #[test]
    fn the_fraction_is_linear_and_exact_at_both_ends() {
        assert_eq!(fraction(0, 100), 0.0);
        assert_eq!(fraction(99, 100), 1.0);
        assert_eq!(fraction(0, 3), 0.0);
        assert_eq!(fraction(1, 3), 0.5);
        assert_eq!(fraction(2, 3), 1.0);
        assert_eq!(fraction(0, 1), 1.0);
        assert_eq!(fraction(0, 0), 1.0);
        assert_eq!(fraction(500, 100), 1.0);
        for i in 0..99 {
            let step = fraction(i + 1, 100) - fraction(i, 100);
            assert!((step - 1.0 / 99.0).abs() < 1e-12);
        }
    }

    #[test]
    fn a_bar_on_each_edge_fills_from_its_start_to_the_whole_length() {
        let cases = [
            (Edge::Top, (0, 0, 199, 7), true, false),
            (Edge::Bottom, (0, 112, 199, 119), true, false),
            (Edge::Left, (0, 0, 7, 119), false, true),
            (Edge::Right, (192, 0, 199, 119), false, true),
        ];
        for (edge, whole, horizontal, up) in cases {
            let b = bar(Mode::Bar(edge));
            assert!(solid(&b.layer(W, H, 0.0)).is_empty(), "{edge:?} at 0");
            let full = solid(&b.layer(W, H, 1.0));
            assert_eq!(bounds(&full), whole, "{edge:?} at the end");
            assert_eq!(full.len(), 8 * if horizontal { 200 } else { 120 });
            let mid = solid(&b.layer(W, H, 0.5));
            let (x0, y0, x1, y1) = bounds(&mid);
            if horizontal {
                assert_eq!((x0, x1), (0, 99), "{edge:?} at the middle");
                assert_eq!((y0, y1), (whole.1, whole.3));
            } else {
                assert_eq!((y0, y1), (60, 119), "{edge:?} fills upward");
                assert_eq!((x0, x1), (whole.0, whole.2));
            }
            assert_eq!(up, !horizontal);
        }
    }

    #[test]
    fn the_unfilled_part_is_the_track_at_its_opacity() {
        let b = bar(Mode::Bar(Edge::Top));
        let l = b.layer(W, H, 0.25);
        let at = |x: usize, y: usize| &l.data[(y * W as usize + x) * 4..][..4];
        assert_eq!(at(10, 3), &[255, 255, 255, 255]);
        assert_eq!(at(150, 3), &[0, 0, 0, 128]);
        assert_eq!(at(150, 8), &[0, 0, 0, 0]);
    }

    #[test]
    fn the_leading_pixel_is_covered_in_proportion() {
        let b = bar(Mode::Bar(Edge::Top));
        let l = b.layer(W, H, 10.5 / 200.0);
        let px = &l.data[10 * 4..][..4];
        assert_eq!(px[3], 192);
        assert_eq!(l.data[9 * 4 + 3], 255);
    }

    #[test]
    fn a_contour_fills_the_whole_ring_with_no_overlap() {
        for clockwise in [true, false] {
            for start in [
                Corner::TopLeft,
                Corner::TopRight,
                Corner::BottomRight,
                Corner::BottomLeft,
            ] {
                let b = bar(Mode::Contour { start, clockwise });
                let path = b.path(W, H);
                let area: u32 = path.iter().map(|s| s.width * s.height).sum();
                assert_eq!(area, W * H - (W - 16) * (H - 16), "{start:?} {clockwise}");
                assert!(solid(&b.layer(W, H, 0.0)).is_empty());
                let full = solid(&b.layer(W, H, 1.0));
                assert_eq!(full.len() as u32, area);
                assert_eq!(bounds(&full), (0, 0, 199, 119));
                let half = solid(&b.layer(W, H, 0.5)).len() as f64;
                let want = f64::from(path.iter().map(Side::length).sum::<u32>()) / 2.0 * 8.0;
                assert!((half - want).abs() <= 8.0, "{start:?} {clockwise}: {half}");
            }
        }
    }

    #[test]
    fn a_contour_visits_the_corners_in_order_from_its_start() {
        let corner = |p: (u32, u32)| match (p.0 < W / 2, p.1 < H / 2) {
            (true, true) => Corner::TopLeft,
            (false, true) => Corner::TopRight,
            (false, false) => Corner::BottomRight,
            (true, false) => Corner::BottomLeft,
        };
        use Corner::*;
        let clockwise = [TopLeft, TopRight, BottomRight, BottomLeft];
        let counter = [TopLeft, BottomLeft, BottomRight, TopRight];
        for (ring, cw) in [(clockwise, true), (counter, false)] {
            for first in 0..4 {
                let start = ring[first];
                let b = bar(Mode::Contour {
                    start,
                    clockwise: cw,
                });
                let got: Vec<Corner> = b.path(W, H).iter().map(|s| corner(s.start())).collect();
                let want: Vec<Corner> = (0..4).map(|k| ring[(first + k) % 4]).collect();
                assert_eq!(got, want, "{start:?} clockwise {cw}");
                let t = 4.0
                    / b.path(W, H)
                        .iter()
                        .map(|s| f64::from(s.length()))
                        .sum::<f64>();
                let early = solid(&b.layer(W, H, t));
                assert!(!early.is_empty());
                assert!(early.iter().all(|&p| corner(p) == start));
            }
        }
    }

    #[test]
    fn a_contour_starting_at_the_top_left_runs_right_then_down() {
        let b = bar(Mode::Contour {
            start: Corner::TopLeft,
            clockwise: true,
        });
        let total: f64 = b.path(W, H).iter().map(|s| f64::from(s.length())).sum();
        let a = solid(&b.layer(W, H, (192.0 + 50.0) / total));
        let (x0, y0, x1, y1) = bounds(&a);
        assert_eq!((x0, y0, x1), (0, 0, 199));
        assert_eq!(y1, 50 - 1 + 0);
        let ccw = bar(Mode::Contour {
            start: Corner::TopLeft,
            clockwise: false,
        });
        let total: f64 = ccw.path(W, H).iter().map(|s| f64::from(s.length())).sum();
        let a = solid(&ccw.layer(W, H, 50.0 / total));
        assert_eq!(bounds(&a), (0, 0, 7, 49));
    }

    #[test]
    fn the_gradient_samples_its_colours_along_the_list() {
        let g = ProgressBar {
            colors: BLUE_PURPLE_PINK.to_vec(),
            ..ProgressBar::default()
        };
        assert_eq!(g.color_at(0.0), BLUE_PURPLE_PINK[0]);
        assert_eq!(g.color_at(0.5), BLUE_PURPLE_PINK[1]);
        assert_eq!(g.color_at(1.0), BLUE_PURPLE_PINK[2]);
        assert_eq!(g.color_at(0.25), Rgb(114, 108, 247));
        assert_eq!(g.color_at(0.75), Rgb(202, 79, 200));
        assert_eq!(g.color_at(-3.0), BLUE_PURPLE_PINK[0]);
        assert_eq!(g.color_at(9.0), BLUE_PURPLE_PINK[2]);
        let two = ProgressBar {
            colors: vec![Rgb(0, 0, 0), Rgb(200, 100, 50)],
            ..ProgressBar::default()
        };
        assert_eq!(two.color_at(0.5), Rgb(100, 50, 25));
        let four = ProgressBar {
            colors: vec![Rgb(0, 0, 0), Rgb(30, 0, 0), Rgb(30, 60, 0), Rgb(30, 60, 90)],
            ..ProgressBar::default()
        };
        assert_eq!(four.color_at(1.0 / 3.0), Rgb(30, 0, 0));
        assert_eq!(four.color_at(0.5), Rgb(30, 30, 0));
    }

    #[test]
    fn the_gradient_spans_the_filled_part() {
        let b = ProgressBar {
            mode: Mode::Bar(Edge::Top),
            thickness: Some(Thickness::Pixels(4)),
            colors: vec![Rgb(255, 0, 0), Rgb(0, 0, 255)],
            track: Rgb(0, 0, 0),
            track_opacity: 0.0,
        };
        for t in [0.5, 1.0] {
            let l = b.layer(W, H, t);
            let last = (t * 200.0) as usize - 1;
            assert!(l.data[0] >= 253 && l.data[2] <= 2, "start at {t}");
            let end = &l.data[last * 4..][..4];
            assert!(end[2] >= 253 && end[0] <= 2, "end at {t}: {end:?}");
        }
    }

    #[test]
    fn the_thickness_is_pixels_a_share_of_the_short_side_or_the_default() {
        let mut b = ProgressBar::default();
        assert_eq!(b.thickness_px(1080, 1920), 12);
        assert_eq!(b.thickness_px(2560, 1440), 9);
        b.thickness = Some(Thickness::Pixels(5));
        assert_eq!(b.thickness_px(1080, 1920), 5);
        b.thickness = Some(Thickness::ShortSide(0.01));
        assert_eq!(b.thickness_px(1080, 1920), 11);
        assert_eq!(b.thickness_px(2560, 1440), 14);
        b.thickness = Some(Thickness::Pixels(9999));
        assert_eq!(b.thickness_px(100, 80), 40);
    }

    #[test]
    fn bad_settings_are_refused() {
        let mut b = ProgressBar::default();
        assert!(b.validate().is_ok());
        b.colors = Vec::new();
        assert!(b.validate().is_err());
        b.colors = vec![Rgb(0, 0, 0); 5];
        assert!(b.validate().is_err());
        b = ProgressBar {
            track_opacity: 1.5,
            ..ProgressBar::default()
        };
        assert!(b.validate().is_err());
        b = ProgressBar {
            thickness: Some(Thickness::Pixels(0)),
            ..ProgressBar::default()
        };
        assert!(b.validate().is_err());
        b = ProgressBar {
            thickness: Some(Thickness::ShortSide(0.9)),
            ..ProgressBar::default()
        };
        assert!(b.validate().is_err());
    }

    #[test]
    fn drawing_on_a_frame_paints_luma_only_where_the_bar_is() {
        use crate::media::{PixelFormat, TimeBase};
        let mut f = Frame::new(PixelFormat::Yuv420p8, W, H, 0, TimeBase::new(1, 1));
        for p in &mut f.planes {
            p.data.fill(128);
        }
        let b = ProgressBar {
            colors: vec![Rgb(255, 255, 255)],
            thickness: Some(Thickness::Pixels(8)),
            track_opacity: 0.0,
            ..ProgressBar::default()
        };
        b.draw(&mut f, ColorSpec::BT709, 5, 11).unwrap();
        let at = |f: &Frame, x: usize, row: usize| f.planes[0].data[row * f.planes[0].stride + x];
        assert!(at(&f, 10, 3) > 200);
        assert!(at(&f, 190, 3) == 128);
        assert!(at(&f, 10, 20) == 128);
        b.draw(&mut f, ColorSpec::BT709, 10, 11).unwrap();
        assert!(at(&f, 190, 3) > 200);
        assert!(at(&f, 199, 7) > 200);
        assert!(at(&f, 199, 8) == 128);
    }
}
