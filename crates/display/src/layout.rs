//! The legibility rule (architecture 11.2), the layout class it selects, and the rectangles of every zone for
//! the compact layouts of 11.2b (1024 x 600 primary, 800 x 480 fallback) and the full layout of 11.2.
//! Pure arithmetic: nothing here draws, so the tests can check it against the spec's tables.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    pub fn contains(&self, o: &Rect) -> bool {
        o.x >= self.x && o.y >= self.y && o.right() <= self.right() && o.bottom() <= self.bottom()
    }
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(
            self.x + d,
            self.y + d,
            (self.w - 2 * d).max(0),
            (self.h - 2 * d).max(0),
        )
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.w == 0 || self.h == 0 {
            return *o;
        }
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        Rect::new(
            x,
            y,
            self.right().max(o.right()) - x,
            self.bottom().max(o.bottom()) - y,
        )
    }
    pub fn clip(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.max(o.x), self.y.max(o.y));
        Rect::new(
            x,
            y,
            (self.right().min(o.right()) - x).max(0),
            (self.bottom().min(o.bottom()) - y).max(0),
        )
    }
}

/// Which arrangement the screen shows. `rotate` draws the `single` layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Single,
    Split,
    /// The processing view (11.2c): its own zones, from the text size (`scene::proc::layout`).
    Processing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variant {
    /// The 1024 x 600 compact layout, the primary target (31 px minimum text on the 7-inch panel).
    C1024,
    /// The 800 x 480 compact layout, the supported fallback (19 px).
    C800,
    /// The full layout of 11.2, 1280 x 720 and up.
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    pub w: i32,
    pub h: i32,
    pub variant: Variant,
    /// Pixels per inch from the pixel diagonal and `display.diagonal_in`.
    pub ppi: f64,
    /// The legibility rule's minimum font size in pixels (cap height 0.7 of it).
    pub min_px: i32,
    /// Lines of minimum-size text that fit in the height: under 40 is compact.
    pub lines: f64,
    /// The smallest text this layout draws: the minimum, except at the 800 x 480 fallback, whose type sizes are 19 px
    /// by the spec even when the panel's rule asks for more (the mode is then degraded and the display warns).
    pub floor_px: i32,
    pub degraded: bool,
}

/// 16 minutes of arc of cap height at 70 cm is 3.26 mm; with cap = 0.7 of the font size that is 0.182 inch of font
/// size (the spec's rounded figure), scaled with the viewing distance.
const FONT_INCH_AT_70CM: f64 = 0.182;

pub fn pixel_density(w: i32, h: i32, diagonal_in: f64) -> f64 {
    f64::from(w).hypot(f64::from(h)) / diagonal_in
}

pub fn min_font_px(w: i32, h: i32, diagonal_in: f64, view_distance_cm: f64) -> i32 {
    (FONT_INCH_AT_70CM * view_distance_cm / 70.0 * pixel_density(w, h, diagonal_in)).round() as i32
}

impl Plan {
    pub fn new(w: i32, h: i32, diagonal_in: f64, view_distance_cm: f64) -> Plan {
        let ppi = pixel_density(w, h, diagonal_in);
        let min_px = min_font_px(w, h, diagonal_in, view_distance_cm).max(1);
        let lines = f64::from(h) / f64::from(min_px);
        let variant = if lines >= 40.0 {
            Variant::Full
        } else if h < 540 {
            Variant::C800
        } else {
            Variant::C1024
        };
        let (floor_px, degraded) = if variant == Variant::C800 && min_px > 19 {
            (19, true)
        } else {
            (min_px, false)
        };
        Plan {
            w,
            h,
            variant,
            ppi,
            min_px,
            lines,
            floor_px,
            degraded,
        }
    }

    /// The 800 x 480 fallback layout on whatever mode the connector offers (7d: the panel refused 1024 x 600).
    pub fn with_fallback_layout(mut self) -> Plan {
        self.variant = Variant::C800;
        self.floor_px = self.min_px.min(19);
        self.degraded = self.min_px > 19;
        self
    }

    /// The variant whose rectangles a view uses: `split` on a full-size screen uses the compact split layout
    /// scaled up (11.2b defines `split` only in the compact sizes).
    pub fn effective(&self, kind: Kind) -> Variant {
        match (self.variant, kind) {
            (Variant::Full, Kind::Split) => Variant::C1024,
            (v, _) => v,
        }
    }

    /// The design canvas of a compact variant.
    pub fn base(v: Variant) -> (i32, i32) {
        match v {
            Variant::C800 => (800, 480),
            _ => (1024, 600),
        }
    }

    /// Scale from the compact design to this screen: `(kx, ky)` for rectangles, `k` for type.
    pub fn scales(&self, v: Variant) -> (f64, f64, f64) {
        let (bw, bh) = Plan::base(v);
        let (kx, ky) = (
            f64::from(self.w) / f64::from(bw),
            f64::from(self.h) / f64::from(bh),
        );
        (kx, ky, kx.min(ky))
    }

    /// A design length scaled for type, never under the floor.
    pub fn px(&self, design: f64, k: f64) -> i32 {
        ((design * k).round() as i32).max(self.floor_px)
    }

    /// The full layout's scale `s = min(width / 1920, height / 1080)`.
    pub fn s(&self) -> f64 {
        (f64::from(self.w) / 1920.0).min(f64::from(self.h) / 1080.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub name: String,
    pub rect: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub zones: Vec<Zone>,
    /// The 2 px divider of `split`.
    pub divider: Option<Rect>,
    /// The rotation's 3 px time bar: the bottom edge of the header zone.
    pub variant: Variant,
    pub kind: Kind,
}

impl Layout {
    pub fn zone(&self, name: &str) -> Rect {
        self.zones
            .iter()
            .find(|z| z.name == name)
            .map(|z| z.rect)
            .unwrap_or_default()
    }
    pub fn has(&self, name: &str) -> bool {
        self.zones.iter().any(|z| z.name == name)
    }
}

/// Zone heights of a compact design, top to bottom.
fn heights(v: Variant, kind: Kind) -> &'static [(&'static str, i32)] {
    match (v, kind) {
        (Variant::C800, Kind::Single) => &[
            ("header", 40),
            ("now", 200),
            ("next", 96),
            ("meters", 88),
            ("alarm", 56),
        ],
        (Variant::C800, Kind::Split) => &[
            ("header", 36),
            ("now", 176),
            ("next", 48),
            ("meters", 88),
            ("alarms", 100),
        ],
        (_, Kind::Single) => &[
            ("header", 44),
            ("now", 280),
            ("next", 76),
            ("meters", 96),
            ("alarm", 104),
        ],
        (_, Kind::Split) => &[
            ("header", 44),
            ("now", 220),
            ("next", 44),
            ("meters", 68),
            ("alarms", 184),
        ],
        // the processing view's zones come from its text size (`scene::proc::layout`)
        (_, Kind::Processing) => &[],
    }
}

/// The shared top bar of `split`, and each half's width, in the design.
fn split_geometry(v: Variant) -> (i32, i32) {
    match v {
        Variant::C800 => (32, 399),
        _ => (40, 511),
    }
}

pub fn layout(plan: &Plan, kind: Kind) -> Layout {
    if kind == Kind::Processing {
        return crate::scene::proc::layout(plan, false).0;
    }
    let v = plan.effective(kind);
    if v == Variant::Full {
        return full_layout(plan);
    }
    let (kx, ky, _) = plan.scales(v);
    let (w, h) = (plan.w, plan.h);
    let rows = heights(v, kind);
    let top = if kind == Kind::Split {
        split_geometry(v).0
    } else {
        0
    };
    // Cumulative design heights, each boundary rounded once, so the rows sum to the screen height exactly.
    let mut ys = vec![0];
    let mut cum = top;
    ys.push(top);
    for (_, hh) in rows {
        cum += hh;
        ys.push(cum);
    }
    let scaled: Vec<i32> = ys
        .iter()
        .map(|&y| {
            if y >= cum {
                h
            } else {
                (f64::from(y) * ky).round() as i32
            }
        })
        .collect();
    let mut zones = vec![];
    let mut divider = None;
    match kind {
        Kind::Processing => {}
        Kind::Single => {
            for (i, (name, _)) in rows.iter().enumerate() {
                zones.push(Zone {
                    name: (*name).into(),
                    rect: Rect::new(0, scaled[i + 1], w, scaled[i + 2] - scaled[i + 1]),
                });
            }
        }
        Kind::Split => {
            zones.push(Zone {
                name: "bar".into(),
                rect: Rect::new(0, 0, w, scaled[1]),
            });
            let (_, half) = split_geometry(v);
            let lw = (f64::from(half) * kx).round() as i32;
            let rx = (f64::from(half + 2) * kx).round() as i32;
            let divider_rect = Rect::new(lw, scaled[1], rx - lw, h - scaled[1]);
            divider = Some(divider_rect);
            for (side, x0, ww) in [("L", 0, lw), ("R", rx, w - rx)] {
                for (i, (name, _)) in rows.iter().enumerate() {
                    zones.push(Zone {
                        name: format!("{side}.{name}"),
                        rect: Rect::new(x0, scaled[i + 1], ww, scaled[i + 2] - scaled[i + 1]),
                    });
                }
            }
        }
    }
    Layout {
        zones,
        divider,
        variant: v,
        kind,
    }
}

/// The percent table of 11.2: x, y, w, h as fractions of the screen.
pub const FULL_ZONES: [(&str, f64, f64, f64, f64); 8] = [
    ("header", 0.0, 0.0, 100.0, 5.5),
    ("now", 0.0, 5.5, 46.0, 46.5),
    ("next", 46.0, 5.5, 54.0, 46.5),
    ("meters", 0.0, 52.0, 24.0, 28.0),
    ("stages", 24.0, 52.0, 30.0, 28.0),
    ("processing", 54.0, 52.0, 46.0, 28.0),
    ("asrun", 0.0, 80.0, 62.0, 20.0),
    ("alarms", 62.0, 80.0, 38.0, 20.0),
];

fn full_layout(plan: &Plan) -> Layout {
    let (w, h) = (f64::from(plan.w), f64::from(plan.h));
    // Boundaries rounded once so neighbors share an edge and the rows sum to the height.
    let r = |frac: f64, total: f64| (frac / 100.0 * total).round() as i32;
    let zones = FULL_ZONES
        .iter()
        .map(|&(name, x, y, zw, zh)| {
            let (x0, x1) = (r(x, w), r(x + zw, w));
            let (y0, y1) = (r(y, h), r(y + zh, h));
            Zone {
                name: name.into(),
                rect: Rect::new(x0, y0, x1 - x0, y1 - y0),
            }
        })
        .collect();
    Layout {
        zones,
        divider: None,
        variant: Variant::Full,
        kind: Kind::Single,
    }
}

/// What the full layout drops when text would fall under the minimum (11.2): in this order, each step also
/// dropping everything before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Drops {
    pub album: bool,
    pub queue_rows_beyond_3: bool,
    pub asrun_rows_beyond_4: bool,
    pub spectrum_32: bool,
    pub stage_numerals: bool,
    pub pre_overlay: bool,
}

/// Reference text sizes (at 1920 x 1080) of the elements that can drop, in drop order.
pub const DROP_REF_PX: [f64; 6] = [28.0, 28.0, 26.0, 20.0, 20.0, 20.0];

pub fn drops(plan: &Plan) -> Drops {
    let s = plan.s();
    let mut last = None;
    for (i, r) in DROP_REF_PX.iter().enumerate() {
        if r * s < f64::from(plan.min_px) {
            last = Some(i);
        }
    }
    let n = last.map_or(0, |i| i + 1);
    Drops {
        album: n >= 1,
        queue_rows_beyond_3: n >= 2,
        asrun_rows_beyond_4: n >= 3,
        spectrum_32: n >= 4,
        stage_numerals: n >= 5,
        pre_overlay: n >= 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_reproduces_the_specs_table() {
        // 7-inch 1024 x 600: 169.5 ppi, 31 px, 19 lines, compact.
        let p = Plan::new(1024, 600, 7.0, 70.0);
        assert!((p.ppi - 169.5).abs() < 0.1);
        assert_eq!(p.min_px, 31);
        assert_eq!(p.variant, Variant::C1024);
        assert!((p.lines - 19.35).abs() < 0.1);
        // 7-inch 800 x 480: 24 px, the layout's 19 px is below it, degraded
        let p = Plan::new(800, 480, 7.0, 70.0);
        assert_eq!(
            (p.min_px, p.variant, p.degraded, p.floor_px),
            (24, Variant::C800, true, 19)
        );
        // 9-inch 800 x 480: 19 px, about 25 lines
        let p = Plan::new(800, 480, 9.0, 70.0);
        assert_eq!(
            (p.min_px, p.variant, p.degraded),
            (19, Variant::C800, false)
        );
        assert!((p.lines - 25.0).abs() < 0.5);
        // 9-inch 1024 x 600: 24 px
        assert_eq!(Plan::new(1024, 600, 9.0, 70.0).min_px, 24);
        // 21.5-inch 1920 x 1080: 19 px, full; 1280 x 720: 12 px, full
        let p = Plan::new(1920, 1080, 21.5, 70.0);
        assert_eq!((p.min_px, p.variant), (19, Variant::Full));
        assert_eq!(Plan::new(1280, 720, 21.5, 70.0).variant, Variant::Full);
        // a 15.6-inch 1080p panel is full (about 141 ppi, 26 px, 41 lines)
        let p = Plan::new(1920, 1080, 15.6, 70.0);
        assert_eq!((p.min_px, p.variant), (26, Variant::Full));
        // the same 1080p on 7 inches: 57 px, compact; on 10 inches: 40 px, still compact (27 lines)
        assert_eq!(Plan::new(1920, 1080, 7.0, 70.0).min_px, 57);
        assert_eq!(Plan::new(1920, 1080, 10.0, 70.0).variant, Variant::C1024);
        // the viewing distance changes the rule
        assert!(Plan::new(1024, 600, 7.0, 100.0).min_px > 31);
    }

    #[test]
    fn compact_rectangles_are_the_specs_and_sum_to_the_height() {
        let p = Plan::new(1024, 600, 7.0, 70.0);
        let l = layout(&p, Kind::Single);
        let want = [
            ("header", Rect::new(0, 0, 1024, 44)),
            ("now", Rect::new(0, 44, 1024, 280)),
            ("next", Rect::new(0, 324, 1024, 76)),
            ("meters", Rect::new(0, 400, 1024, 96)),
            ("alarm", Rect::new(0, 496, 1024, 104)),
        ];
        for (n, r) in want {
            assert_eq!(l.zone(n), r, "{n}");
        }
        let l = layout(&p, Kind::Split);
        assert_eq!(l.zone("bar"), Rect::new(0, 0, 1024, 40));
        assert_eq!(l.zone("L.header"), Rect::new(0, 40, 511, 44));
        assert_eq!(l.zone("R.header"), Rect::new(513, 40, 511, 44));
        assert_eq!(l.zone("L.now"), Rect::new(0, 84, 511, 220));
        assert_eq!(l.zone("L.next"), Rect::new(0, 304, 511, 44));
        assert_eq!(l.zone("L.meters"), Rect::new(0, 348, 511, 68));
        assert_eq!(l.zone("R.alarms"), Rect::new(513, 416, 511, 184));
        assert_eq!(l.divider, Some(Rect::new(511, 40, 2, 560)));
        let p = Plan::new(800, 480, 9.0, 70.0);
        let l = layout(&p, Kind::Single);
        assert_eq!(l.zone("now"), Rect::new(0, 40, 800, 200));
        assert_eq!(l.zone("alarm"), Rect::new(0, 424, 800, 56));
        let l = layout(&p, Kind::Split);
        assert_eq!(l.zone("L.now"), Rect::new(0, 68, 399, 176));
        assert_eq!(l.zone("R.alarms"), Rect::new(401, 380, 399, 100));
    }

    #[test]
    fn every_layout_partitions_the_screen() {
        for (w, h, d) in [
            (1024, 600, 7.0),
            (800, 480, 9.0),
            (800, 480, 7.0),
            (1280, 720, 21.5),
            (1920, 1080, 21.5),
            (1920, 1080, 15.6),
            (1366, 768, 7.0),
            (1920, 1080, 7.0),
        ] {
            let p = Plan::new(w, h, d, 70.0);
            for kind in [Kind::Single, Kind::Split] {
                let l = layout(&p, kind);
                let mut cover = vec![0u8; (w * h) as usize];
                for z in &l.zones {
                    assert!(
                        Rect::new(0, 0, w, h).contains(&z.rect),
                        "{w}x{h} {kind:?} {}",
                        z.name
                    );
                    for y in z.rect.y..z.rect.bottom() {
                        for x in z.rect.x..z.rect.right() {
                            cover[(y * w + x) as usize] += 1;
                        }
                    }
                }
                if let Some(d) = l.divider {
                    for y in d.y..d.bottom() {
                        for x in d.x..d.right() {
                            cover[(y * w + x) as usize] += 1;
                        }
                    }
                }
                assert!(
                    cover.iter().all(|&c| c == 1),
                    "{w}x{h} {kind:?}: not an exact partition"
                );
            }
        }
    }

    #[test]
    fn full_rectangles_follow_the_percent_table_within_a_pixel() {
        for (w, h) in [(1280, 720), (1920, 1080), (2560, 1440)] {
            let p = Plan::new(w, h, 21.5, 70.0);
            let l = layout(&p, Kind::Single);
            for &(n, x, y, zw, zh) in &FULL_ZONES {
                let r = l.zone(n);
                let e = |a: i32, frac: f64, tot: i32| {
                    (f64::from(a) - frac / 100.0 * f64::from(tot)).abs()
                };
                assert!(e(r.x, x, w) <= 1.0 && e(r.y, y, h) <= 1.0, "{n} origin");
                assert!(e(r.w, zw, w) <= 1.0 && e(r.h, zh, h) <= 1.0, "{n} size");
            }
        }
        // the processing zone at 1080p is the spec's 883 x 302
        let r = layout(&Plan::new(1920, 1080, 21.5, 70.0), Kind::Single).zone("processing");
        assert_eq!((r.w, r.h), (883, 302));
        let r = layout(&Plan::new(1280, 720, 21.5, 70.0), Kind::Single).zone("processing");
        assert_eq!((r.w, r.h), (589, 202));
    }

    #[test]
    fn drops_follow_the_order() {
        assert_eq!(drops(&Plan::new(1920, 1080, 21.5, 70.0)), Drops::default());
        assert_eq!(drops(&Plan::new(1280, 720, 21.5, 70.0)), Drops::default());
        // a denser screen asks for more: 1280 x 720 on 15 inches
        let d = drops(&Plan::new(1280, 720, 15.0, 70.0));
        assert!(d.album && d.queue_rows_beyond_3);
        // every step implies the earlier ones
        for diag in [10.0, 14.0, 16.0, 18.0, 21.5, 27.0] {
            let d = drops(&Plan::new(1280, 720, diag, 70.0));
            assert!(!d.pre_overlay || d.stage_numerals);
            assert!(!d.stage_numerals || d.spectrum_32);
            assert!(!d.spectrum_32 || d.asrun_rows_beyond_4);
            assert!(!d.asrun_rows_beyond_4 || d.queue_rows_beyond_3);
            assert!(!d.queue_rows_beyond_3 || d.album);
        }
    }
}
