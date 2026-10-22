//! Drawing: the operation list a zone is described by, the fonts, and the canvas that executes it into an
//! offscreen RGBA buffer (a `tiny-skia` pixmap) independent of KMS. Every glyph run and every artwork drawn is
//! logged, so the tests can check sizes and rectangles without a screen.
//!
//! A zone is a list of operations. The renderer hashes the list and redraws the zone only when the hash changed
//! (damage-only redraw, 11.1), so the same description serves the screen and the screenshots.

use crate::layout::Rect;
use fontdue::{Font, FontSettings};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use tiny_skia::{FillRule, Mask, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color(r, g, b, 255)
    }
    pub fn with_alpha(self, a: u8) -> Color {
        Color(self.0, self.1, self.2, a)
    }
}

/// The dark theme of 11.2 (the same colors as the web page's style sheet).
pub mod theme {
    use super::Color;
    pub const BG: Color = Color::rgb(0x0b, 0x0e, 0x13);
    pub const PANEL: Color = Color::rgb(0x14, 0x1a, 0x23);
    pub const PANEL2: Color = Color::rgb(0x1d, 0x25, 0x31);
    pub const LINE: Color = Color::rgb(0x2a, 0x34, 0x42);
    pub const TEXT: Color = Color::rgb(0xe8, 0xed, 0xf2);
    pub const DIM: Color = Color::rgb(0x8a, 0x96, 0xa5);
    pub const AMBER: Color = Color::rgb(0xf5, 0xa6, 0x23);
    pub const RED: Color = Color::rgb(0xff, 0x5c, 0x5c);
    pub const RED_BG: Color = Color::rgb(0x5a, 0x16, 0x1b);
    pub const GREEN: Color = Color::rgb(0x3e, 0xcf, 0x8e);
    pub const BLUE: Color = Color::rgb(0x4f, 0x74, 0x9c);
    pub const BLUE_LINE: Color = Color::rgb(0x8f, 0xb4, 0xdc);
    pub const GREY: Color = Color::rgb(0x55, 0x5f, 0x6c);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontKind {
    Sans,
    Bold,
    /// Monospaced digits, bold.
    Mono,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextOp {
    /// The anchor: the left edge, the center or the right edge, by `align`.
    pub x: i32,
    pub baseline: i32,
    pub text: String,
    pub size: i32,
    pub font: FontKind,
    pub color: Color,
    pub align: Align,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    Rect(Rect, Color),
    RoundRect(Rect, i32, Color),
    Text(TextOp),
    /// Artwork scaled into `rect`; `loaded` is part of the description so the zone redraws when the image arrives.
    Image {
        rect: Rect,
        url: String,
        loaded: bool,
    },
    Placeholder(Rect),
    /// Points in 1/8 pixel; filled, stroked (width in 1/8 pixel) and dashed (on, off in pixels).
    Path {
        pts: Vec<(i32, i32)>,
        closed: bool,
        fill: Option<Color>,
        stroke: Option<(Color, i32)>,
        dash: Option<(i32, i32)>,
    },
}

pub fn q8(v: f64) -> i32 {
    (v * 8.0).round() as i32
}

/// A zone: its rectangle, its background and its operations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ZoneOps {
    pub name: String,
    pub rect: Rect,
    pub bg: Color,
    pub ops: Vec<Op>,
}

impl ZoneOps {
    pub fn signature(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}

// ---- fonts ------------------------------------------------------------------------------------------------------

const SANS: &[u8] = include_bytes!("../fonts/DejaVuSans.ttf");
const BOLD: &[u8] = include_bytes!("../fonts/DejaVuSans-Bold.ttf");
const MONO: &[u8] = include_bytes!("../fonts/DejaVuSansMono-Bold.ttf");

pub struct Fonts {
    sans: Font,
    bold: Font,
    mono: Font,
    /// Advance widths and kerning by `(font, char, size)`: measuring is most of what the scene does.
    adv: std::cell::RefCell<HashMap<(FontKind, char, i32), f32>>,
    kern: std::cell::RefCell<HashMap<(FontKind, char, char, i32), f32>>,
}

/// DejaVu's cap height and digit height are 0.729 of the font size.
pub const CAP_RATIO: f32 = 0.729;

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

impl Fonts {
    pub fn new() -> Fonts {
        let load = |b: &[u8]| Font::from_bytes(b, FontSettings::default()).expect("bundled font");
        Fonts {
            sans: load(SANS),
            bold: load(BOLD),
            mono: load(MONO),
            adv: Default::default(),
            kern: Default::default(),
        }
    }

    fn font(&self, k: FontKind) -> &Font {
        match k {
            FontKind::Sans => &self.sans,
            FontKind::Bold => &self.bold,
            FontKind::Mono => &self.mono,
        }
    }

    pub fn advance(&self, k: FontKind, c: char, size: i32) -> f32 {
        *self
            .adv
            .borrow_mut()
            .entry((k, c, size))
            .or_insert_with(|| self.font(k).metrics(c, size as f32).advance_width)
    }

    pub fn kerning(&self, k: FontKind, a: char, b: char, size: i32) -> f32 {
        *self
            .kern
            .borrow_mut()
            .entry((k, a, b, size))
            .or_insert_with(|| {
                self.font(k)
                    .horizontal_kern(a, b, size as f32)
                    .unwrap_or(0.0)
            })
    }

    /// The width of `text` at `size` pixels, in pixels.
    pub fn width(&self, k: FontKind, text: &str, size: i32) -> i32 {
        let mut w = 0.0f32;
        let mut prev: Option<char> = None;
        for c in text.chars() {
            if let Some(p) = prev {
                w += self.kerning(k, p, c, size);
            }
            w += self.advance(k, c, size);
            prev = Some(c);
        }
        w.ceil() as i32
    }

    /// `text`, cut and ended with an ellipsis if it does not fit in `max_w`.
    pub fn fit(&self, k: FontKind, text: &str, size: i32, max_w: i32) -> String {
        if self.width(k, text, size) <= max_w {
            return text.to_string();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            while chars.last() == Some(&' ') {
                chars.pop();
            }
            let cand: String = chars.iter().collect::<String>() + "\u{2026}";
            if self.width(k, &cand, size) <= max_w {
                return cand;
            }
        }
        "\u{2026}".into()
    }

    /// Word-wrapped lines, at most `max_lines`; the last is ellipsized when text remains.
    pub fn wrap(
        &self,
        k: FontKind,
        text: &str,
        size: i32,
        max_w: i32,
        max_lines: usize,
    ) -> Vec<String> {
        let mut words: std::collections::VecDeque<String> =
            text.split_whitespace().map(String::from).collect();
        let mut lines: Vec<String> = vec![];
        let mut cur = String::new();
        while let Some(word) = words.pop_front() {
            let cand = if cur.is_empty() {
                word.clone()
            } else {
                format!("{cur} {word}")
            };
            if self.width(k, &cand, size) <= max_w {
                cur = cand;
                continue;
            }
            if cur.is_empty() {
                // one word wider than the line: break it by characters
                let chars: Vec<char> = word.chars().collect();
                let mut head = String::new();
                let mut i = 0;
                while i < chars.len() {
                    let t = format!("{head}{}", chars[i]);
                    if self.width(k, &t, size) > max_w && !head.is_empty() {
                        break;
                    }
                    head = t;
                    i += 1;
                }
                let tail: String = chars[i..].iter().collect();
                if !tail.is_empty() {
                    words.push_front(tail);
                }
                lines.push(head);
            } else {
                lines.push(std::mem::take(&mut cur));
                words.push_front(word);
            }
            if lines.len() == max_lines {
                break;
            }
        }
        if lines.len() < max_lines && !cur.is_empty() {
            lines.push(cur);
            words.clear();
        }
        if lines.len() == max_lines && !words.is_empty() {
            let mut more = lines.pop().unwrap_or_default();
            for w in &words {
                more.push(' ');
                more.push_str(w);
            }
            lines.push(self.fit(k, &more, size, max_w));
        }
        lines
    }
}

// ---- the log ------------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct GlyphRun {
    pub zone: String,
    pub text: String,
    pub size: i32,
    pub font: FontKind,
    pub baseline: i32,
    /// The ink bounding box of the run, before any clipping.
    pub bbox: Rect,
    /// The pen box: the advance width times the font height, for overlap checks between runs.
    pub color: Color,
}

#[derive(Clone, Debug)]
pub struct ArtLog {
    pub zone: String,
    pub rect: Rect,
    /// `image` when artwork was drawn, `placeholder` otherwise.
    pub kind: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct DrawLog {
    pub glyphs: Vec<GlyphRun>,
    pub art: Vec<ArtLog>,
    /// Filled rectangles named by the scene (meter bars and the like): `(zone, rect, color)`.
    pub rects: Vec<(String, Rect, Color)>,
}

// ---- the canvas ----------------------------------------------------------------------------------------------------

struct Glyph {
    xmin: i32,
    ymin: i32,
    w: usize,
    h: usize,
    advance: f32,
    cov: Vec<u8>,
}

/// Decoded artwork, by URL: the full image and its scaled copies.
#[derive(Default)]
pub struct ImageStore {
    full: HashMap<String, image::RgbaImage>,
    scaled: HashMap<(String, i32, i32), image::RgbaImage>,
}

impl ImageStore {
    pub fn insert(&mut self, url: &str, mut img: image::RgbaImage) {
        // composite onto the panel color once, so a blit is a row copy
        for p in img.pixels_mut() {
            if p.0[3] != 255 {
                let a = u32::from(p.0[3]);
                for (i, bg) in [theme::PANEL2.0, theme::PANEL2.1, theme::PANEL2.2]
                    .into_iter()
                    .enumerate()
                {
                    p.0[i] = ((u32::from(p.0[i]) * a + u32::from(bg) * (255 - a)) / 255) as u8;
                }
                p.0[3] = 255;
            }
        }
        self.scaled.retain(|k, _| k.0 != url);
        self.full.insert(url.to_string(), img);
    }
    pub fn has(&self, url: &str) -> bool {
        self.full.contains_key(url)
    }
    pub fn len(&self) -> usize {
        self.full.len()
    }
    pub fn is_empty(&self) -> bool {
        self.full.is_empty()
    }
    /// Forget everything not in `keep` (the cache is bounded by what is on the screen and in the queue).
    pub fn retain(&mut self, keep: &dyn Fn(&str) -> bool) {
        self.full.retain(|k, _| keep(k));
        self.scaled.retain(|k, _| keep(&k.0));
    }
    fn get(&mut self, url: &str, w: i32, h: i32) -> Option<&image::RgbaImage> {
        let key = (url.to_string(), w, h);
        if !self.scaled.contains_key(&key) {
            let src = self.full.get(url)?;
            let img = image::imageops::resize(
                src,
                w as u32,
                h as u32,
                image::imageops::FilterType::Triangle,
            );
            self.scaled.insert(key.clone(), img);
        }
        self.scaled.get(&key)
    }
}

pub struct Canvas {
    pub pm: Pixmap,
    fonts: Fonts,
    glyphs: HashMap<(FontKind, char, i32), Glyph>,
    pub images: ImageStore,
    pub log: DrawLog,
    mask: Option<Mask>,
}

fn blend(dst: &mut [u8], c: Color, cov: u32) {
    // dst is premultiplied RGBA on an opaque background; c is straight color with alpha.
    let a = u32::from(c.3) * cov / 255;
    if a == 0 {
        return;
    }
    for i in 0..3 {
        let s = [c.0, c.1, c.2][i] as u32;
        dst[i] = ((s * a + u32::from(dst[i]) * (255 - a) + 127) / 255) as u8;
    }
    dst[3] = 255;
}

impl Canvas {
    pub fn new(w: u32, h: u32) -> Canvas {
        let mut pm = Pixmap::new(w, h).expect("pixmap");
        pm.fill(tiny_skia::Color::from_rgba8(
            theme::BG.0,
            theme::BG.1,
            theme::BG.2,
            255,
        ));
        Canvas {
            pm,
            fonts: Fonts::new(),
            glyphs: HashMap::new(),
            images: ImageStore::default(),
            log: DrawLog::default(),
            mask: Mask::new(w, h),
        }
    }

    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }

    pub fn width(&self) -> i32 {
        self.pm.width() as i32
    }
    pub fn height(&self) -> i32 {
        self.pm.height() as i32
    }

    pub fn fill_rect(&mut self, r: Rect, c: Color, clip: Rect) {
        let r = r
            .clip(&clip)
            .clip(&Rect::new(0, 0, self.width(), self.height()));
        if r.w == 0 || r.h == 0 {
            return;
        }
        let w = self.pm.width() as usize;
        let data = self.pm.data_mut();
        let row_len = r.w as usize * 4;
        if c.3 == 255 {
            // the first row pixel by pixel, the others by copying it: a plain fill is most of what a redraw does
            let o0 = (r.y as usize * w + r.x as usize) * 4;
            for px in data[o0..o0 + row_len].chunks_exact_mut(4) {
                px.copy_from_slice(&[c.0, c.1, c.2, 255]);
            }
            for y in r.y + 1..r.bottom() {
                let o = (y as usize * w + r.x as usize) * 4;
                data.copy_within(o0..o0 + row_len, o);
            }
        } else {
            for y in r.y..r.bottom() {
                for x in r.x..r.right() {
                    let o = (y as usize * w + x as usize) * 4;
                    blend(&mut data[o..o + 4], c, 255);
                }
            }
        }
    }

    fn glyph(&mut self, k: FontKind, ch: char, size: i32) -> &Glyph {
        let fonts = &self.fonts;
        self.glyphs.entry((k, ch, size)).or_insert_with(|| {
            let (m, cov) = fonts.font(k).rasterize(ch, size as f32);
            Glyph {
                xmin: m.xmin,
                ymin: m.ymin,
                w: m.width,
                h: m.height,
                advance: m.advance_width,
                cov,
            }
        })
    }

    /// Draw one run of text clipped to `clip`, log it under `zone`, and return its ink box.
    pub fn text(&mut self, t: &TextOp, zone: &str, clip: Rect) -> Rect {
        let total = self.fonts.width(t.font, &t.text, t.size);
        let x0 = match t.align {
            Align::Left => t.x,
            Align::Center => t.x - total / 2,
            Align::Right => t.x - total,
        };
        let (w, h) = (self.width(), self.height());
        let mut pen = x0 as f32;
        let mut prev: Option<char> = None;
        let mut ink: Option<Rect> = None;
        for ch in t.text.chars() {
            if let Some(p) = prev {
                pen += self.fonts.kerning(t.font, p, ch, t.size);
            }
            prev = Some(ch);
            let (xmin, ymin, gw, gh, adv) = {
                let g = self.glyph(t.font, ch, t.size);
                (g.xmin, g.ymin, g.w, g.h, g.advance)
            };
            let gx = pen.round() as i32 + xmin;
            let gy = t.baseline - ymin - gh as i32;
            if gw > 0 && gh > 0 {
                let r = Rect::new(gx, gy, gw as i32, gh as i32);
                ink = Some(ink.map_or(r, |i| i.union(&r)));
                let vis = r.clip(&clip).clip(&Rect::new(0, 0, w, h));
                if vis.w > 0 && vis.h > 0 {
                    let cov = &self.glyphs[&(t.font, ch, t.size)].cov;
                    let data = self.pm.data_mut();
                    for y in vis.y..vis.bottom() {
                        for x in vis.x..vis.right() {
                            let c = u32::from(cov[(y - gy) as usize * gw + (x - gx) as usize]);
                            if c > 0 {
                                let o = (y as usize * w as usize + x as usize) * 4;
                                blend(&mut data[o..o + 4], t.color, c);
                            }
                        }
                    }
                }
            }
            pen += adv;
        }
        let bbox = ink.unwrap_or(Rect::new(x0, t.baseline, 0, 0));
        self.log.glyphs.push(GlyphRun {
            zone: zone.to_string(),
            text: t.text.clone(),
            size: t.size,
            font: t.font,
            baseline: t.baseline,
            bbox,
            color: t.color,
        });
        bbox
    }

    pub fn round_rect(&mut self, r: Rect, radius: i32, c: Color, clip: Rect) {
        let rr = r.clip(&clip);
        if rr.w == 0 || rr.h == 0 {
            return;
        }
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        let (w, h) = (self.width() as usize, self.height() as usize);
        let data = self.pm.data_mut();
        // distance-to-corner coverage with 1 px antialiasing
        for y in rr.y.max(0)..rr.bottom().min(h as i32) {
            for x in rr.x.max(0)..rr.right().min(w as i32) {
                let (cx, cy) = (
                    if x < r.x + radius {
                        r.x + radius
                    } else if x >= r.right() - radius {
                        r.right() - radius - 1
                    } else {
                        x
                    },
                    if y < r.y + radius {
                        r.y + radius
                    } else if y >= r.bottom() - radius {
                        r.bottom() - radius - 1
                    } else {
                        y
                    },
                );
                let d = f64::from(x - cx).hypot(f64::from(y - cy));
                let cov = ((f64::from(radius) - d + 0.5).clamp(0.0, 1.0) * 255.0) as u32;
                if cov > 0 {
                    let o = (y as usize * w + x as usize) * 4;
                    blend(&mut data[o..o + 4], c, cov);
                }
            }
        }
    }

    pub fn image(&mut self, r: Rect, url: &str, clip: Rect) -> bool {
        let (w, h) = (self.pm.width() as i32, self.pm.height() as i32);
        let Some(img) = self.images.get(url, r.w, r.h) else {
            return false;
        };
        let vis = r.clip(&clip).clip(&Rect::new(0, 0, w, h));
        let data = self.pm.data_mut();
        let raw = img.as_raw();
        for y in vis.y..vis.bottom() {
            let so = ((y - r.y) as usize * r.w as usize + (vis.x - r.x) as usize) * 4;
            let o = (y as usize * w as usize + vis.x as usize) * 4;
            let n = vis.w as usize * 4;
            data[o..o + n].copy_from_slice(&raw[so..so + n]);
        }
        true
    }

    pub fn path(
        &mut self,
        pts: &[(i32, i32)],
        closed: bool,
        fill: Option<Color>,
        stroke: Option<(Color, i32)>,
        dash: Option<(i32, i32)>,
        clip: Rect,
    ) {
        if pts.len() < 2 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0 as f32 / 8.0, pts[0].1 as f32 / 8.0);
        for p in &pts[1..] {
            pb.line_to(p.0 as f32 / 8.0, p.1 as f32 / 8.0);
        }
        if closed {
            pb.close();
        }
        let Some(path) = pb.finish() else { return };
        // paths are clipped to their zone with a mask, so a stroke on a zone's edge never bleeds into its neighbor
        if let Some(m) = self.mask.as_mut() {
            m.clear();
            if let Some(r) = tiny_skia::Rect::from_xywh(
                clip.x as f32,
                clip.y as f32,
                clip.w as f32,
                clip.h as f32,
            ) {
                m.fill_path(
                    &PathBuilder::from_rect(r),
                    FillRule::Winding,
                    false,
                    Transform::identity(),
                );
            }
        }
        let paint = |c: Color| {
            let mut p = Paint::default();
            p.set_color_rgba8(c.0, c.1, c.2, c.3);
            p.anti_alias = true;
            p
        };
        if let Some(c) = fill {
            self.pm.fill_path(
                &path,
                &paint(c),
                FillRule::Winding,
                Transform::identity(),
                self.mask.as_ref(),
            );
        }
        if let Some((c, wd)) = stroke {
            let mut s = Stroke {
                width: wd as f32 / 8.0,
                ..Stroke::default()
            };
            if let Some((on, off)) = dash {
                s.dash = StrokeDash::new(vec![on as f32, off as f32], 0.0);
            }
            self.pm.stroke_path(
                &path,
                &paint(c),
                &s,
                Transform::identity(),
                self.mask.as_ref(),
            );
        }
    }

    /// Execute one zone: clear it to its background, then every operation, clipped to the zone.
    pub fn draw_zone(&mut self, z: &ZoneOps) {
        let clip = z.rect;
        self.fill_rect(z.rect, z.bg, clip);
        for op in &z.ops {
            match op {
                Op::Rect(r, c) => {
                    self.fill_rect(*r, *c, clip);
                    self.log.rects.push((z.name.clone(), *r, *c));
                }
                Op::RoundRect(r, rad, c) => self.round_rect(*r, *rad, *c, clip),
                Op::Text(t) => {
                    self.text(t, &z.name, clip);
                }
                Op::Image { rect, url, .. } => {
                    let ok = self.image(*rect, url, clip);
                    if !ok {
                        self.placeholder(*rect, clip);
                    }
                    self.log.art.push(ArtLog {
                        zone: z.name.clone(),
                        rect: *rect,
                        kind: if ok { "image" } else { "placeholder" },
                    });
                }
                Op::Placeholder(r) => {
                    self.placeholder(*r, clip);
                    self.log.art.push(ArtLog {
                        zone: z.name.clone(),
                        rect: *r,
                        kind: "placeholder",
                    });
                }
                Op::Path {
                    pts,
                    closed,
                    fill,
                    stroke,
                    dash,
                } => {
                    self.path(pts, *closed, *fill, *stroke, *dash, clip);
                }
            }
        }
    }

    fn placeholder(&mut self, r: Rect, clip: Rect) {
        self.round_rect(r, (r.w / 16).max(2), theme::PANEL2, clip);
        // a plain record: a ring and a hub
        let (cx, cy) = (
            f64::from(r.x) + f64::from(r.w) / 2.0,
            f64::from(r.y) + f64::from(r.h) / 2.0,
        );
        let rad = f64::from(r.w.min(r.h)) * 0.32;
        let ring = |rr: f64| -> Vec<(i32, i32)> {
            (0..48)
                .map(|i| {
                    let a = f64::from(i) / 48.0 * std::f64::consts::TAU;
                    (q8(cx + rr * a.cos()), q8(cy + rr * a.sin()))
                })
                .collect()
        };
        self.path(
            &ring(rad),
            true,
            None,
            Some((theme::GREY, q8(f64::from(r.w) / 24.0 + 1.0))),
            None,
            clip,
        );
        self.path(&ring(rad * 0.22), true, Some(theme::GREY), None, None, clip);
    }

    /// The pixels of `r`, row by row.
    pub fn copy_region(&self, r: Rect) -> Vec<u8> {
        let w = self.pm.width() as usize;
        let src = self.pm.data();
        let mut out = Vec::with_capacity((r.w * r.h * 4) as usize);
        for y in r.y..r.bottom() {
            let s = (y as usize * w + r.x as usize) * 4;
            out.extend_from_slice(&src[s..s + r.w as usize * 4]);
        }
        out
    }

    /// The bounding box of what differs between `before` (from [`Canvas::copy_region`]) and the canvas now.
    pub fn changed_bbox(&self, before: &[u8], r: Rect) -> Option<Rect> {
        let w = self.pm.width() as usize;
        let src = self.pm.data();
        let rw = r.w as usize * 4;
        let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0usize, usize::MAX, 0usize);
        for row in 0..r.h as usize {
            let s = ((r.y as usize + row) * w + r.x as usize) * 4;
            let (a, b) = (&before[row * rw..(row + 1) * rw], &src[s..s + rw]);
            if a == b {
                continue;
            }
            let first = a.iter().zip(b).position(|(p, q)| p != q).unwrap_or(0) / 4;
            let last = rw - a.iter().zip(b).rev().position(|(p, q)| p != q).unwrap_or(0);
            x0 = x0.min(first);
            x1 = x1.max(last.div_ceil(4));
            y0 = y0.min(row);
            y1 = row + 1;
        }
        (y0 != usize::MAX).then(|| {
            Rect::new(
                r.x + x0 as i32,
                r.y + y0 as i32,
                (x1 - x0) as i32,
                (y1 - y0) as i32,
            )
        })
    }

    /// The pixels as RGBA bytes (the canvas is opaque).
    pub fn rgba(&self) -> &[u8] {
        self.pm.data()
    }

    /// Copy `r` into an XRGB8888 buffer (little-endian: B, G, R, X) with row pitch `pitch` bytes. The destination
    /// is write-combined video memory on the real output, so each row is converted in ordinary memory first and
    /// then stored in one piece: byte stores straight into it cost about 25 times more.
    pub fn copy_xrgb(&self, r: Rect, dst: &mut [u8], pitch: usize) {
        let w = self.pm.width() as usize;
        let src = self.pm.data();
        let r = r.clip(&Rect::new(0, 0, self.width(), self.height()));
        if r.w == 0 || r.h == 0 {
            return;
        }
        let mut row = vec![0u8; r.w as usize * 4];
        for y in r.y..r.bottom() {
            let s = (y as usize * w + r.x as usize) * 4;
            for (d, px) in row
                .chunks_exact_mut(4)
                .zip(src[s..s + r.w as usize * 4].chunks_exact(4))
            {
                d.copy_from_slice(&[px[2], px[1], px[0], 0]);
            }
            let d = y as usize * pitch + r.x as usize * 4;
            dst[d..d + row.len()].copy_from_slice(&row);
        }
    }

    pub fn save_png(&self, path: &std::path::Path) -> Result<(), String> {
        let img =
            image::RgbaImage::from_raw(self.pm.width(), self.pm.height(), self.pm.data().to_vec())
                .ok_or("bad buffer")?;
        img.save(path)
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The text of an op list's runs, for the scene tests.
pub fn text_ops(z: &ZoneOps) -> impl Iterator<Item = &TextOp> {
    z.ops
        .iter()
        .filter_map(|o| if let Op::Text(t) = o { Some(t) } else { None })
}
