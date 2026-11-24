//! Night color for the status display (the box's own panel only; never the web pages or the stream): the panel warms
//! up after sunset and back after sunrise, the way f.lux and Night Shift do.
//!
//! - **The white point** of a color temperature is a black body's: the CIE 1931 2-degree chromaticity of the
//!   Planckian locus ([`PLANCK_XY`], computed from Planck's law and the CIE observer by
//!   `tools/blackbody-table.py`), converted to linear sRGB and taken relative to 6500 K, so 6500 K is the panel's
//!   own white (identity) and every lower temperature is that much warmer. The largest channel is 1.0.
//! - **The table** ([`Lut`]) applies that white in linear light: `LUT[i] = encode(decode(i / 255) * w * gain)`.
//!   Multiplying the gamma-encoded values instead (Redshift) darkens the mid tones of the reduced channels far more
//!   than the temperature says, which is the harsh, orange look to avoid. `gain` is the optional night dim.
//! - **When**: `sun` (sunset and sunrise from the NOAA solar position algorithm, at a place from the time zone, a
//!   one-off internet lookup or set by hand), `schedule` (fixed times) or `off`. The panel reaches the night look
//!   at sunset after `transition` minutes of warming, and starts back at sunrise. Transitions are linear in time and
//!   interpolated in mired (1e6 / K), so equal steps look equal.
//!
//! Everything here is a pure function of the settings, the place, the zone and the time: the display uses it to
//! draw and the library service uses the same code for the Settings page.

use crate::config::DisplayConfig;
use crate::timespec::Zone;
use serde_json::{json, Value};
use std::path::PathBuf;

/// The day white point: the panel untouched.
pub const DAY_K: f64 = 6500.0;
/// The range of the night temperature setting.
pub const MIN_K: f64 = 1900.0;
pub const MAX_K: f64 = 6500.0;
/// The most the night dim may take away, as a fraction.
pub const MAX_DIM: f64 = 0.5;
/// How long the Preview button shows the night look.
pub const PREVIEW_S: i64 = 15;

const DAY_MS: i64 = 86_400_000;

/// The Planckian locus, CIE 1931 2-degree observer: `(kelvin, x, y)` every 100 K from 1000 K to 10000 K. Planck's
/// law (CODATA 2018 constants) weighted by the CIE's 1 nm color-matching functions from 360 to 830 nm
/// (`tools/blackbody-table.py` prints it). 2800 K to 2900 K brackets CIE illuminant A (2856 K: 0.44757, 0.40745).
#[rustfmt::skip]
pub const PLANCK_XY: [(u32, f64, f64); 91] = [
    (1000, 0.65275, 0.34446), (1100, 0.63876, 0.35650), (1200, 0.62504, 0.36745), (1300, 0.61163, 0.37723),
    (1400, 0.59852, 0.38579), (1500, 0.58572, 0.39312), (1600, 0.57323, 0.39926), (1700, 0.56107, 0.40427),
    (1800, 0.54925, 0.40822), (1900, 0.53778, 0.41120), (2000, 0.52668, 0.41330), (2100, 0.51596, 0.41460),
    (2200, 0.50563, 0.41521), (2300, 0.49569, 0.41520), (2400, 0.48614, 0.41467), (2500, 0.47699, 0.41368),
    (2600, 0.46824, 0.41230), (2700, 0.45986, 0.41060), (2800, 0.45186, 0.40863), (2900, 0.44422, 0.40644),
    (3000, 0.43693, 0.40407), (3100, 0.42998, 0.40157), (3200, 0.42336, 0.39895), (3300, 0.41705, 0.39626),
    (3400, 0.41103, 0.39350), (3500, 0.40530, 0.39072), (3600, 0.39984, 0.38791), (3700, 0.39464, 0.38510),
    (3800, 0.38968, 0.38229), (3900, 0.38495, 0.37951), (4000, 0.38044, 0.37675), (4100, 0.37614, 0.37402),
    (4200, 0.37203, 0.37133), (4300, 0.36811, 0.36869), (4400, 0.36437, 0.36609), (4500, 0.36079, 0.36354),
    (4600, 0.35737, 0.36105), (4700, 0.35410, 0.35861), (4800, 0.35097, 0.35622), (4900, 0.34797, 0.35389),
    (5000, 0.34510, 0.35161), (5100, 0.34235, 0.34939), (5200, 0.33972, 0.34722), (5300, 0.33719, 0.34510),
    (5400, 0.33476, 0.34304), (5500, 0.33243, 0.34104), (5600, 0.33020, 0.33908), (5700, 0.32805, 0.33717),
    (5800, 0.32598, 0.33532), (5900, 0.32400, 0.33351), (6000, 0.32208, 0.33175), (6100, 0.32024, 0.33004),
    (6200, 0.31847, 0.32837), (6300, 0.31676, 0.32675), (6400, 0.31511, 0.32517), (6500, 0.31353, 0.32363),
    (6600, 0.31199, 0.32213), (6700, 0.31051, 0.32067), (6800, 0.30909, 0.31925), (6900, 0.30771, 0.31786),
    (7000, 0.30637, 0.31651), (7100, 0.30508, 0.31520), (7200, 0.30384, 0.31392), (7300, 0.30263, 0.31267),
    (7400, 0.30146, 0.31145), (7500, 0.30033, 0.31027), (7600, 0.29924, 0.30911), (7700, 0.29818, 0.30798),
    (7800, 0.29715, 0.30688), (7900, 0.29615, 0.30581), (8000, 0.29519, 0.30477), (8100, 0.29425, 0.30374),
    (8200, 0.29334, 0.30275), (8300, 0.29245, 0.30178), (8400, 0.29160, 0.30083), (8500, 0.29076, 0.29990),
    (8600, 0.28995, 0.29900), (8700, 0.28916, 0.29811), (8800, 0.28840, 0.29725), (8900, 0.28765, 0.29641),
    (9000, 0.28693, 0.29558), (9100, 0.28622, 0.29478), (9200, 0.28553, 0.29399), (9300, 0.28486, 0.29322),
    (9400, 0.28421, 0.29247), (9500, 0.28358, 0.29173), (9600, 0.28296, 0.29101), (9700, 0.28235, 0.29031),
    (9800, 0.28177, 0.28962), (9900, 0.28119, 0.28895), (10000, 0.28063, 0.28829),
];

// ---- color --------------------------------------------------------------------------------------------------

/// Mired (micro reciprocal degrees) of a temperature in kelvin.
pub fn mired(kelvin: f64) -> f64 {
    1e6 / kelvin
}

/// The temperature `p` of the way from `from_k` to `to_k`, interpolated in mired (0 is `from_k`, 1 is `to_k`).
pub fn kelvin_between(from_k: f64, to_k: f64, p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    1e6 / (mired(from_k) + (mired(to_k) - mired(from_k)) * p)
}

/// The Planckian chromaticity at `kelvin` (1000 K to 10000 K), between the table's rows linearly in mired.
pub fn planck_xy(kelvin: f64) -> (f64, f64) {
    let k = kelvin.clamp(1000.0, 10000.0);
    let i = (((k - 1000.0) / 100.0).floor() as usize).min(PLANCK_XY.len() - 2);
    let (k0, x0, y0) = PLANCK_XY[i];
    let (k1, x1, y1) = PLANCK_XY[i + 1];
    let t = (mired(f64::from(k0)) - mired(k)) / (mired(f64::from(k0)) - mired(f64::from(k1)));
    (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)
}

/// Linear sRGB (IEC 61966-2-1 matrix) of the chromaticity `x, y` at luminance 1.
pub fn xy_to_linear_srgb(x: f64, y: f64) -> [f64; 3] {
    let (cx, cy, cz) = (x / y, 1.0, (1.0 - x - y) / y);
    [
        3.2406 * cx - 1.5372 * cy - 0.4986 * cz,
        -0.9689 * cx + 1.8758 * cy + 0.0415 * cz,
        0.0557 * cx - 0.2040 * cy + 1.0570 * cz,
    ]
}

/// The white point of `kelvin` as linear sRGB channel gains: the black body relative to the 6500 K one (so 6500 K
/// is `[1, 1, 1]`), no channel below 0, the largest channel 1.
pub fn white_point(kelvin: f64) -> [f64; 3] {
    let (x, y) = planck_xy(kelvin);
    let (x65, y65) = planck_xy(DAY_K);
    let (w, d) = (xy_to_linear_srgb(x, y), xy_to_linear_srgb(x65, y65));
    let rel = [
        (w[0] / d[0]).max(0.0),
        (w[1] / d[1]).max(0.0),
        (w[2] / d[2]).max(0.0),
    ];
    let m = rel[0].max(rel[1]).max(rel[2]);
    [rel[0] / m, rel[1] / m, rel[2] / m]
}

/// sRGB transfer function, encoded (0 to 1) to linear light.
pub fn srgb_decode(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light (0 to 1) to sRGB encoded.
pub fn srgb_encode(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// The per-channel lookup table applied to every pixel of the panel's frame, as the last step before it is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lut {
    r: [u8; 256],
    g: [u8; 256],
    b: [u8; 256],
    identity: bool,
}

impl Default for Lut {
    fn default() -> Self {
        Self::identity()
    }
}

impl Lut {
    pub fn identity() -> Lut {
        let mut t = [0u8; 256];
        for (i, v) in t.iter_mut().enumerate() {
            *v = i as u8;
        }
        Lut {
            r: t,
            g: t,
            b: t,
            identity: true,
        }
    }

    /// The table for a white point (linear channel gains) and an overall `gain` (1 minus the dim), applied in
    /// linear light.
    pub fn new(white: [f64; 3], gain: f64) -> Lut {
        let mut ch = [[0u8; 256]; 3];
        for (c, t) in ch.iter_mut().enumerate() {
            let w = (white[c] * gain).clamp(0.0, 1.0);
            for (i, v) in t.iter_mut().enumerate() {
                let lin = srgb_decode(i as f64 / 255.0) * w;
                *v = (srgb_encode(lin) * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        let identity = ch
            .iter()
            .all(|t| t.iter().enumerate().all(|(i, &v)| v as usize == i));
        Lut {
            r: ch[0],
            g: ch[1],
            b: ch[2],
            identity,
        }
    }

    /// The table for a temperature and a dim (0 to 0.5).
    pub fn for_look(kelvin: f64, dim: f64) -> Lut {
        Lut::new(white_point(kelvin), 1.0 - dim.clamp(0.0, MAX_DIM))
    }

    /// True when the table changes nothing: the pass over the pixels is skipped.
    pub fn is_identity(&self) -> bool {
        self.identity
    }

    #[inline]
    pub fn map(&self, r: u8, g: u8, b: u8) -> (u8, u8, u8) {
        (self.r[r as usize], self.g[g as usize], self.b[b as usize])
    }

    /// The three tables, red, green, blue.
    pub fn tables(&self) -> [&[u8; 256]; 3] {
        [&self.r, &self.g, &self.b]
    }

    /// Apply the table to RGBA pixels in place (alpha untouched). Nothing is done for the identity.
    pub fn apply_rgba(&self, px: &mut [u8]) {
        if self.identity {
            return;
        }
        for p in px.chunks_exact_mut(4) {
            let (r, g, b) = self.map(p[0], p[1], p[2]);
            p[0] = r;
            p[1] = g;
            p[2] = b;
        }
    }
}

// ---- the sun (NOAA solar position algorithm) ---------------------------------------------------------------

/// The sun's declination (degrees) and the equation of time (minutes) at the unix time `ms`, from the NOAA solar
/// calculator's formulas (Meeus, "Astronomical Algorithms").
fn sun_position(ms: i64) -> (f64, f64) {
    let jd = ms as f64 / DAY_MS as f64 + 2_440_587.5;
    let jc = (jd - 2_451_545.0) / 36_525.0;
    let l0 = (280.466_46 + jc * (36_000.769_83 + jc * 0.000_303_2)).rem_euclid(360.0);
    let m = 357.529_11 + jc * (35_999.050_29 - 0.000_153_7 * jc);
    let e = 0.016_708_634 - jc * (0.000_042_037 + 0.000_000_126_7 * jc);
    let mr = m.to_radians();
    let c = mr.sin() * (1.914_602 - jc * (0.004_817 + 0.000_014 * jc))
        + (2.0 * mr).sin() * (0.019_993 - 0.000_101 * jc)
        + (3.0 * mr).sin() * 0.000_289;
    let true_long = l0 + c;
    let omega = (125.04 - 1934.136 * jc).to_radians();
    let app_long = true_long - 0.005_69 - 0.004_78 * omega.sin();
    let obliq0 =
        23.0 + (26.0 + (21.448 - jc * (46.815 + jc * (0.000_59 - jc * 0.001_813))) / 60.0) / 60.0;
    let obliq = obliq0 + 0.002_56 * omega.cos();
    let decl = (obliq.to_radians().sin() * app_long.to_radians().sin())
        .asin()
        .to_degrees();
    let y = (obliq.to_radians() / 2.0).tan().powi(2);
    let l0r = l0.to_radians();
    let eq = 4.0
        * (y * (2.0 * l0r).sin() - 2.0 * e * mr.sin() + 4.0 * e * y * mr.sin() * (2.0 * l0r).cos()
            - 0.5 * y * y * (4.0 * l0r).sin()
            - 1.25 * e * e * (2.0 * mr).sin())
        .to_degrees();
    (decl, eq)
}

/// The sun's elevation above the horizon in degrees (geometric, no refraction) at `ms`, seen from `lat`, `lon`
/// (degrees, east positive).
pub fn sun_elevation(lat: f64, lon: f64, ms: i64) -> f64 {
    let (decl, eq) = sun_position(ms);
    let utc_min = ms.rem_euclid(DAY_MS) as f64 / 60_000.0;
    let tst = (utc_min + eq + 4.0 * lon).rem_euclid(1440.0);
    let ha = (tst / 4.0 - 180.0).to_radians();
    let (la, de) = (lat.to_radians(), decl.to_radians());
    let cz = (la.sin() * de.sin() + la.cos() * de.cos() * ha.cos()).clamp(-1.0, 1.0);
    90.0 - cz.acos().to_degrees()
}

/// One day of the sun at a place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SunDay {
    /// Sunrise and sunset (unix ms): the upper limb on the horizon with standard refraction (zenith 90.833 degrees).
    Normal { rise: i64, set: i64 },
    /// The sun stays up all day (midnight sun).
    AlwaysUp,
    /// The sun stays down all day (polar night).
    AlwaysDown,
}

/// The instant nearest `near` whose UTC time of day is `minutes`.
fn nearest_at(near: i64, minutes: f64) -> i64 {
    let mut t = near.div_euclid(DAY_MS) * DAY_MS + (minutes * 60_000.0).round() as i64;
    while t - near > DAY_MS / 2 {
        t -= DAY_MS;
    }
    while near - t > DAY_MS / 2 {
        t += DAY_MS;
    }
    t
}

/// The hour angle of sunrise in degrees, or which way it fails (> 1: never rises, < -1: never sets).
fn sunrise_hour_angle(lat: f64, decl: f64) -> Result<f64, SunDay> {
    let (la, de) = (lat.to_radians(), decl.to_radians());
    let c = 90.833_f64.to_radians().cos() / (la.cos() * de.cos()) - la.tan() * de.tan();
    if c > 1.0 {
        Err(SunDay::AlwaysDown)
    } else if c < -1.0 {
        Err(SunDay::AlwaysUp)
    } else {
        Ok(c.acos().to_degrees())
    }
}

/// Sunrise and sunset around the solar noon nearest `near` (pass local noon of the day wanted), refined three
/// times at the event's own instant.
pub fn sun_day(lat: f64, lon: f64, near: i64) -> SunDay {
    let lat = lat.clamp(-89.99, 89.99);
    let noon0 = nearest_at(near, 720.0 - 4.0 * lon);
    let (decl, eq) = sun_position(noon0);
    let noon = nearest_at(noon0, 720.0 - 4.0 * lon - eq);
    let ha = match sunrise_hour_angle(lat, decl) {
        Ok(h) => h,
        Err(d) => return d,
    };
    let event = |sign: f64, ha0: f64| -> i64 {
        let mut t = noon + (sign * ha0 * 4.0 * 60_000.0) as i64;
        for _ in 0..3 {
            let (decl, eq) = sun_position(t);
            // near the polar edge the refined angle can fail: keep the estimate from noon
            let h = sunrise_hour_angle(lat, decl).unwrap_or(ha0);
            let n = nearest_at(noon, 720.0 - 4.0 * lon - eq);
            t = n + (sign * h * 4.0 * 60_000.0) as i64;
        }
        t
    };
    SunDay::Normal {
        rise: event(-1.0, ha),
        set: event(1.0, ha),
    }
}

// ---- where: the time zone's city, an internet lookup, or by hand ----------------------------------------

/// `+404251-0740023` (or `+4043-07400`), the ISO 6709 coordinates of `zone.tab`, to degrees `(lat, lon)`.
pub fn parse_iso6709(s: &str) -> Option<(f64, f64)> {
    let split = s[1..].find(['+', '-'])? + 1;
    let (a, b) = s.split_at(split);
    let part = |p: &str, deg_digits: usize| -> Option<f64> {
        let sign = match p.as_bytes().first()? {
            b'+' => 1.0,
            b'-' => -1.0,
            _ => return None,
        };
        let d = &p[1..];
        if !d.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let deg: f64 = d.get(..deg_digits)?.parse().ok()?;
        let min: f64 = d.get(deg_digits..deg_digits + 2)?.parse().ok()?;
        let sec: f64 = match d.len() - deg_digits {
            2 => 0.0,
            4 => d.get(deg_digits + 2..)?.parse().ok()?,
            _ => return None,
        };
        Some(sign * (deg + min / 60.0 + sec / 3600.0))
    };
    Some((part(a, 2)?, part(b, 3)?))
}

/// The coordinates of `zone` in the text of `zone1970.tab` or `zone.tab` (country, coordinates, zone, comment).
pub fn coords_in_tab(tab: &str, zone: &str) -> Option<(f64, f64)> {
    tab.lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .find(|f| f.len() >= 3 && f[2] == zone)
        .and_then(|f| parse_iso6709(f[1]))
}

/// The zone a link names, from the `L TARGET LINK` lines of `tzdata.zi` (`US/Eastern` is `America/New_York`).
pub fn link_target(tzdata_zi: &str, zone: &str) -> Option<String> {
    tzdata_zi.lines().find_map(|l| {
        let mut f = l.split_whitespace();
        (f.next() == Some("L"))
            .then(|| (f.next(), f.next()))
            .and_then(|(t, n)| (n == Some(zone)).then(|| t.map(str::to_string)))
            .flatten()
    })
}

/// The system's time-zone database (`HOTCLOCK_ZONEINFO` replaces it in tests).
pub fn zoneinfo_dir() -> PathBuf {
    std::env::var_os("HOTCLOCK_ZONEINFO")
        .map_or_else(|| PathBuf::from("/usr/share/zoneinfo"), PathBuf::from)
}

/// The coordinates of a time zone's principal city, from tzdata's `zone1970.tab`, then `zone.tab`; a link is
/// followed first (`tzdata.zi`, or the file being a symbolic link). `None` for a zone with no city (`UTC`).
pub fn zone_coords(zone: &str) -> Option<(f64, f64)> {
    let dir = zoneinfo_dir();
    let tabs: Vec<String> = ["zone1970.tab", "zone.tab"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(dir.join(f)).ok())
        .collect();
    let find = |z: &str| tabs.iter().find_map(|t| coords_in_tab(t, z));
    if let Some(c) = find(zone) {
        return Some(c);
    }
    let target = std::fs::read_to_string(dir.join("tzdata.zi"))
        .ok()
        .and_then(|t| link_target(&t, zone))
        .or_else(|| {
            let real = std::fs::canonicalize(dir.join(zone)).ok()?;
            let base = std::fs::canonicalize(&dir).ok()?;
            Some(real.strip_prefix(base).ok()?.to_string_lossy().into_owned())
        })?;
    find(&target)
}

/// The zones of `zone1970.tab` and `zone.tab`, sorted, with `UTC`: the choices of the time zone dropdown.
pub fn zone_names() -> Vec<String> {
    let dir = zoneinfo_dir();
    let mut v: Vec<String> = ["zone1970.tab", "zone.tab"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(dir.join(f)).ok())
        .flat_map(|t| {
            t.lines()
                .filter(|l| !l.starts_with('#'))
                .filter_map(|l| l.split('\t').nth(2).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .collect();
    v.push("UTC".into());
    v.sort();
    v.dedup();
    v
}

// ---- the settings ----------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Sun,
    Schedule,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocationSource {
    Timezone,
    Internet,
    Manual,
}

/// The `display.night_*` keys, parsed.
#[derive(Clone, Debug, PartialEq)]
pub struct NightSettings {
    pub mode: Mode,
    /// The night temperature, kelvin.
    pub kelvin: f64,
    /// The night dim, 0 to 0.5.
    pub dim: f64,
    pub transition_ms: i64,
    /// `schedule`: minutes of the local day when night begins (the panel is warm from then) and ends.
    pub on_min: i64,
    pub off_min: i64,
    pub location: LocationSource,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// `system` or an IANA name.
    pub time_zone: String,
}

/// `HH:MM` to minutes of the day.
pub fn parse_hhmm(s: &str) -> Option<i64> {
    let (h, m) = s.trim().split_once(':')?;
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit());
    if !digits(h) || h.len() > 2 || !digits(m) || m.len() != 2 {
        return None;
    }
    let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 60 + m)
}

impl NightSettings {
    pub fn from_config(d: &DisplayConfig) -> NightSettings {
        NightSettings {
            mode: match d.night_mode.as_str() {
                "schedule" => Mode::Schedule,
                "off" => Mode::Off,
                _ => Mode::Sun,
            },
            kelvin: f64::from(d.night_kelvin).clamp(MIN_K, MAX_K),
            dim: (f64::from(d.night_dim_percent) / 100.0).clamp(0.0, MAX_DIM),
            transition_ms: i64::from(d.night_transition_min) * 60_000,
            on_min: parse_hhmm(&d.night_on).unwrap_or(20 * 60),
            off_min: parse_hhmm(&d.night_off).unwrap_or(7 * 60),
            location: match d.night_location.as_str() {
                "internet" => LocationSource::Internet,
                "manual" => LocationSource::Manual,
                _ => LocationSource::Timezone,
            },
            latitude: d.night_latitude,
            longitude: d.night_longitude,
            time_zone: d.night_time_zone.clone(),
        }
    }

    /// The `night_*` keys of a `[display]` block as JSON, the library service's `display_night` payload.
    pub fn config_json(d: &DisplayConfig) -> Value {
        json!({
            "night_mode": d.night_mode, "night_kelvin": d.night_kelvin, "night_dim_percent": d.night_dim_percent,
            "night_transition_min": d.night_transition_min, "night_on": d.night_on, "night_off": d.night_off,
            "night_location": d.night_location, "night_latitude": d.night_latitude,
            "night_longitude": d.night_longitude, "night_time_zone": d.night_time_zone,
        })
    }

    /// Set the `night_*` keys of `d` from [`NightSettings::config_json`]; a key missing or of the wrong type keeps
    /// its value.
    pub fn apply_config_json(d: &mut DisplayConfig, v: &Value) {
        let s = |k: &str, to: &mut String| {
            if let Some(x) = v[k].as_str() {
                *to = x.to_string();
            }
        };
        let n = |k: &str, to: &mut u32| {
            if let Some(x) = v[k].as_u64().and_then(|x| u32::try_from(x).ok()) {
                *to = x;
            }
        };
        s("night_mode", &mut d.night_mode);
        n("night_kelvin", &mut d.night_kelvin);
        n("night_dim_percent", &mut d.night_dim_percent);
        n("night_transition_min", &mut d.night_transition_min);
        s("night_on", &mut d.night_on);
        s("night_off", &mut d.night_off);
        s("night_location", &mut d.night_location);
        s("night_time_zone", &mut d.night_time_zone);
        if v.get("night_latitude").is_some() {
            d.night_latitude = v["night_latitude"].as_f64();
        }
        if v.get("night_longitude").is_some() {
            d.night_longitude = v["night_longitude"].as_f64();
        }
    }

    /// The zone the times are shown in and the schedule runs in.
    pub fn zone(&self) -> Result<Zone, String> {
        if self.time_zone == "system" {
            Ok(Zone::system())
        } else {
            Zone::named(&self.time_zone)
        }
    }
}

/// Where the sun is computed for.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub lat: f64,
    pub lon: f64,
    /// How it was found, for the Settings page: `time zone America/New_York`, `internet (Boston)`, `set by hand`.
    pub from: String,
}

impl Place {
    pub fn to_json(&self) -> Value {
        json!({"lat": self.lat, "lon": self.lon, "from": self.from})
    }
    pub fn from_json(v: &Value) -> Option<Place> {
        Some(Place {
            lat: v["lat"].as_f64()?,
            lon: v["lon"].as_f64()?,
            from: v["from"].as_str().unwrap_or("").to_string(),
        })
    }
}

/// The place for the settings: by hand, the internet lookup when there is one (else the time zone), or the time
/// zone's principal city.
pub fn resolve_place(
    s: &NightSettings,
    zone: &Zone,
    internet: Option<&Place>,
) -> Result<Place, String> {
    if s.location == LocationSource::Manual {
        return match (s.latitude, s.longitude) {
            (Some(lat), Some(lon)) => Ok(Place {
                lat,
                lon,
                from: "set by hand".into(),
            }),
            _ => Err(
                "the location is set by hand, but the latitude or the longitude is empty".into(),
            ),
        };
    }
    if s.location == LocationSource::Internet {
        if let Some(p) = internet {
            return Ok(p.clone());
        }
    }
    match zone_coords(zone.name()) {
        Some((lat, lon)) => Ok(Place {
            lat,
            lon,
            from: format!(
                "time zone {}{}",
                zone.name(),
                if s.location == LocationSource::Internet {
                    " (the internet lookup has no answer yet)"
                } else {
                    ""
                }
            ),
        }),
        None => Err(format!(
            "the time zone {} has no city to take the sun from; set the location by hand",
            zone.name()
        )),
    }
}

// ---- now: the phase, the temperature, the next change ---------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Off,
    Day,
    Warming,
    Night,
    Cooling,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Off => "off",
            Phase::Day => "day",
            Phase::Warming => "warming",
            Phase::Night => "night",
            Phase::Cooling => "cooling",
        }
    }
}

/// The night look at one instant, and what the Settings page says about it.
#[derive(Clone, Debug, PartialEq)]
pub struct NightState {
    pub phase: Phase,
    /// 0 is day, 1 is the full night look.
    pub progress: f64,
    pub kelvin: f64,
    pub dim: f64,
    /// The sun on the local day of the instant (when the place is known).
    pub today: Option<SunDay>,
    /// When the next phase starts, and which.
    pub next: Option<(i64, Phase)>,
    /// Why the look stays day (no place, a bad zone).
    pub error: Option<String>,
}

impl NightState {
    fn day(phase: Phase, error: Option<String>) -> NightState {
        NightState {
            phase,
            progress: 0.0,
            kelvin: DAY_K,
            dim: 0.0,
            today: None,
            next: None,
            error,
        }
    }

    /// `Day (6500 K)`, `Warming: 4100 K`, `Night (3400 K)`, `Cooling: 4100 K`, `Off`.
    pub fn text(&self) -> String {
        let k = (self.kelvin / 10.0).round() * 10.0;
        match self.phase {
            Phase::Off => "Off".into(),
            Phase::Day => format!("Day ({k:.0} K)"),
            Phase::Warming => format!("Warming: {k:.0} K"),
            Phase::Night => format!("Night ({k:.0} K)"),
            Phase::Cooling => format!("Cooling: {k:.0} K"),
        }
    }

    pub fn lut(&self) -> Lut {
        if self.progress <= 0.0 {
            Lut::identity()
        } else {
            Lut::for_look(self.kelvin, self.dim)
        }
    }
}

/// The ramps around the night: warming over `transition` before night starts, cooling over `transition` after it
/// ends. `night_now` is whether the last boundary before `t` started a night; `day_started` is the last end of a
/// night before `t`, `night_starts` the next start after it.
fn ramp(
    t: i64,
    night_now: bool,
    day_started: Option<i64>,
    night_starts: Option<i64>,
    tr: i64,
) -> (f64, Phase) {
    if night_now {
        return (1.0, Phase::Night);
    }
    if tr <= 0 {
        return (0.0, Phase::Day);
    }
    let cool = day_started.map_or(0.0, |r| (1.0 - (t - r) as f64 / tr as f64).clamp(0.0, 1.0));
    let warm = night_starts.map_or(0.0, |s| (1.0 - (s - t) as f64 / tr as f64).clamp(0.0, 1.0));
    if cool <= 0.0 && warm <= 0.0 {
        (0.0, Phase::Day)
    } else if warm >= cool {
        (warm, Phase::Warming)
    } else {
        (cool, Phase::Cooling)
    }
}

/// The night look at `t` for the settings, the place (for `sun`) and the zone; `preview_until` forces the full
/// night look until then.
pub fn evaluate(
    s: &NightSettings,
    place: Option<&Place>,
    zone: &Zone,
    t: i64,
    preview_until: Option<i64>,
) -> NightState {
    let local_noon = |k: i64| -> Option<i64> {
        let d = zone
            .civil(t)
            .date()
            .checked_add(jiff::Span::new().days(k))
            .ok()?;
        zone.compatible(d.at(12, 0, 0, 0))
    };
    let today = place.and_then(|p| local_noon(0).map(|n| sun_day(p.lat, p.lon, n)));
    // the boundaries of the nights from two days before to two days after: (instant, starts a night)
    let mut bounds: Vec<(i64, bool)> = vec![];
    let mut error = None;
    match s.mode {
        Mode::Off => {}
        Mode::Sun => match place {
            None => error = Some("no location".to_string()),
            Some(p) => {
                for k in -2..=2 {
                    if let Some(SunDay::Normal { rise, set }) =
                        local_noon(k).map(|n| sun_day(p.lat, p.lon, n))
                    {
                        bounds.push((rise, false));
                        bounds.push((set, true));
                    }
                }
            }
        },
        Mode::Schedule => {
            if s.on_min != s.off_min {
                for k in -2..=2 {
                    let Some(d) = zone
                        .civil(t)
                        .date()
                        .checked_add(jiff::Span::new().days(k))
                        .ok()
                    else {
                        continue;
                    };
                    let at = |m: i64| zone.compatible(d.at((m / 60) as i8, (m % 60) as i8, 0, 0));
                    if let Some(on) = at(s.on_min) {
                        bounds.push((on, true));
                    }
                    if let Some(off) = at(s.off_min) {
                        bounds.push((off, false));
                    }
                }
            }
        }
    }
    bounds.sort_unstable();
    let last = bounds.iter().rev().find(|b| b.0 <= t).copied();
    let night_now = match (last, s.mode, place) {
        (Some((_, starts_night)), _, _) => starts_night,
        // no boundary in five days: the midnight sun or the polar night
        (None, Mode::Sun, Some(p)) => sun_elevation(p.lat, p.lon, t) < -0.833,
        _ => false,
    };
    let day_started = bounds.iter().rev().find(|b| b.0 <= t && !b.1).map(|b| b.0);
    let next_day = bounds.iter().find(|b| b.0 > t && !b.1).map(|b| b.0);
    let night_starts = bounds.iter().find(|b| b.0 > t && b.1).map(|b| b.0);
    let tr = s.transition_ms.max(0);
    let mut st = if s.mode == Mode::Off {
        NightState::day(Phase::Off, None)
    } else if error.is_some() {
        NightState::day(Phase::Day, error)
    } else {
        let (p, phase) = ramp(t, night_now, day_started, night_starts, tr);
        let next = match phase {
            Phase::Night => next_day.map(|r| (r, if tr > 0 { Phase::Cooling } else { Phase::Day })),
            Phase::Warming => night_starts.map(|n| (n, Phase::Night)),
            Phase::Cooling => day_started.map(|r| (r + tr, Phase::Day)),
            _ => night_starts.map(|n| {
                if tr > 0 {
                    (n - tr, Phase::Warming)
                } else {
                    (n, Phase::Night)
                }
            }),
        };
        NightState {
            phase,
            progress: p,
            kelvin: kelvin_between(DAY_K, s.kelvin, p),
            dim: s.dim * p,
            today: None,
            next,
            error: None,
        }
    };
    st.today = today;
    if preview_until.is_some_and(|u| t < u) {
        st.phase = Phase::Night;
        st.progress = 1.0;
        st.kelvin = s.kelvin;
        st.dim = s.dim;
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ny() -> Zone {
        Zone::named("America/New_York").unwrap()
    }

    /// Unix ms of a local time in a zone.
    fn local(z: &Zone, y: i16, mo: i8, d: i8, h: i8, mi: i8) -> i64 {
        z.compatible(jiff::civil::date(y, mo, d).at(h, mi, 0, 0))
            .unwrap()
    }

    fn settings() -> NightSettings {
        NightSettings::from_config(&DisplayConfig::default())
    }

    #[test]
    fn the_table_reproduces_illuminant_a_and_the_6500_k_point() {
        // CIE illuminant A is a black body at 2856 K: x 0.44757, y 0.40745
        let (x, y) = planck_xy(2856.0);
        assert!(
            (x - 0.44757).abs() < 0.0003 && (y - 0.40745).abs() < 0.0003,
            "{x} {y}"
        );
        let (x, y) = planck_xy(6500.0);
        assert!(
            (x - 0.3135).abs() < 0.0003 && (y - 0.3236).abs() < 0.0003,
            "{x} {y}"
        );
    }

    #[test]
    fn white_points_agree_with_an_independent_reference() {
        // The reference: the Planckian locus by the cubic spline of Kim et al. (2002, US patent 7,024,034), through the
        // same sRGB matrix and taken relative to its own 6500 K: independent of the table and its interpolation.
        let kim = |t: f64| -> (f64, f64) {
            let x = if t <= 4000.0 {
                -0.266_123_9e9 / t.powi(3) - 0.234_358_9e6 / t.powi(2)
                    + 0.877_695_6e3 / t
                    + 0.179_910
            } else {
                -3.025_846_9e9 / t.powi(3)
                    + 2.107_037_9e6 / t.powi(2)
                    + 0.222_634_7e3 / t
                    + 0.240_390
            };
            let y = if t <= 2222.0 {
                -1.106_381_4 * x.powi(3) - 1.348_110_20 * x.powi(2) + 2.185_558_32 * x
                    - 0.202_196_83
            } else if t <= 4000.0 {
                -0.954_947_6 * x.powi(3) - 1.374_185_93 * x.powi(2) + 2.091_370_15 * x
                    - 0.167_488_67
            } else {
                3.081_758_0 * x.powi(3) - 5.873_386_70 * x.powi(2) + 3.751_129_97 * x - 0.370_014_83
            };
            (x, y)
        };
        let reference = |t: f64| -> [f64; 3] {
            let (x, y) = kim(t);
            let (x6, y6) = kim(6500.0);
            let (w, d) = (xy_to_linear_srgb(x, y), xy_to_linear_srgb(x6, y6));
            let r = [
                (w[0] / d[0]).max(0.0),
                (w[1] / d[1]).max(0.0),
                (w[2] / d[2]).max(0.0),
            ];
            let m = r[0].max(r[1]).max(r[2]);
            [r[0] / m, r[1] / m, r[2] / m]
        };
        // and the values themselves, so a change of the method shows here (linear sRGB, largest channel 1)
        let expected = [
            (1900.0, [1.0, 0.247, 0.000]),
            (2700.0, [1.0, 0.441, 0.100]),
            (3400.0, [1.0, 0.586, 0.239]),
            (4500.0, [1.0, 0.771, 0.506]),
            (6500.0, [1.0, 1.000, 1.000]),
        ];
        for (k, want) in expected {
            let got = white_point(k);
            let r = reference(k);
            for c in 0..3 {
                assert!(
                    (got[c] - r[c]).abs() < 0.01,
                    "{k} K channel {c}: {got:?} vs Kim {r:?}"
                );
                assert!(
                    (got[c] - want[c]).abs() < 0.005,
                    "{k} K channel {c}: {got:?} vs {want:?}"
                );
            }
        }
        // 6500 K is the panel's own white: within 1 % of identity (it is exact)
        let w = white_point(6500.0);
        assert!(w.iter().all(|c| (c - 1.0).abs() < 0.01), "{w:?}");
        assert!(Lut::for_look(6500.0, 0.0).is_identity());
    }

    #[test]
    fn the_table_is_applied_in_linear_light_is_monotonic_and_identity_by_day() {
        let id = Lut::identity();
        assert!(id.is_identity());
        assert_eq!(Lut::new([1.0; 3], 1.0), id);
        let off = evaluate(
            &NightSettings {
                mode: Mode::Off,
                ..settings()
            },
            None,
            &ny(),
            0,
            None,
        );
        assert_eq!(off.phase, Phase::Off);
        assert!(off.lut().is_identity());
        for (k, dim) in [
            (1900.0, 0.0),
            (2700.0, 0.0),
            (3400.0, 0.0),
            (3400.0, 0.3),
            (4500.0, 0.5),
            (6400.0, 0.0),
        ] {
            let l = Lut::for_look(k, dim);
            assert!(!l.is_identity());
            for t in l.tables() {
                assert_eq!(t[0], 0, "black stays black");
                assert!(t.windows(2).all(|w| w[0] <= w[1]), "{k} K: not monotonic");
            }
        }
        // linear light: half of the green light at a mid grey is not half of the encoded value
        let w = [1.0, 0.5, 0.25];
        let l = Lut::new(w, 1.0);
        let (r, g, b) = l.map(255, 255, 255);
        assert_eq!(r, 255);
        assert_eq!(g, (srgb_encode(0.5) * 255.0).round() as u8); // 188, where the encoded product gives 128
        assert_eq!(b, (srgb_encode(0.25) * 255.0).round() as u8);
        let (_, g, _) = l.map(128, 128, 128);
        assert_eq!(
            g,
            (srgb_encode(srgb_decode(128.0 / 255.0) * 0.5) * 255.0).round() as u8
        );
        // the dim scales the light: 30 % less light on white
        let (r, _, _) = Lut::for_look(6500.0, 0.3).map(255, 255, 255);
        assert_eq!(r, (srgb_encode(0.7) * 255.0).round() as u8);
        // 3400 K white on the panel
        assert_eq!(
            Lut::for_look(3400.0, 0.0).map(255, 255, 255),
            (255, 201, 134)
        );
    }

    #[test]
    fn transitions_are_interpolated_in_mired() {
        assert_eq!(kelvin_between(6500.0, 3400.0, 0.0), 6500.0);
        assert!((kelvin_between(6500.0, 3400.0, 1.0) - 3400.0).abs() < 1e-9);
        // halfway is the mean of the mireds (153.8 and 294.1: 224.0 mired, 4465 K), not the mean of the kelvins (4950 K)
        let mid = kelvin_between(6500.0, 3400.0, 0.5);
        assert!((mid - 1e6 / ((1e6 / 6500.0 + 1e6 / 3400.0) / 2.0)).abs() < 1e-6);
        assert!((mid - 4464.6).abs() < 1.0, "{mid}");
        // equal steps of progress are equal steps of mired
        let m: Vec<f64> = (0..=4)
            .map(|i| mired(kelvin_between(6500.0, 1900.0, f64::from(i) / 4.0)))
            .collect();
        for w in m.windows(3) {
            assert!(((w[1] - w[0]) - (w[2] - w[1])).abs() < 1e-9);
        }
    }

    #[test]
    fn sunrise_and_sunset_in_new_york_match_the_published_times() {
        // Published times for New York City (40.7128 N, 74.0060 W), local time (sunrise-and-sunset.com and
        // sunrisesunset.com): 21 Jun 2026 05:25 / 20:30 EDT, 21 Dec 2026 07:17 / 16:31 EST, 15 Oct 2026 07:08 / 18:14 EDT.
        let z = ny();
        for ((y, m, d), (rh, rm), (sh, sm)) in [
            ((2026, 6, 21), (5, 25), (20, 30)),
            ((2026, 12, 21), (7, 17), (16, 31)),
            ((2026, 10, 15), (7, 8), (18, 14)),
        ] {
            let noon = local(&z, y, m, d, 12, 0);
            let SunDay::Normal { rise, set } = sun_day(40.7128, -74.0060, noon) else {
                panic!("no sunrise in New York");
            };
            for (got, want, what) in [
                (rise, local(&z, y, m, d, rh, rm), "sunrise"),
                (set, local(&z, y, m, d, sh, sm), "sunset"),
            ] {
                // the published minute is truncated or rounded: compare with its middle
                let diff = (got - (want + 30_000)).abs();
                eprintln!(
                    "{y}-{m:02}-{d:02} {what}: computed {}, published {:02}:{:02}",
                    z.local_string(got),
                    z.civil(want).hour(),
                    z.civil(want).minute()
                );
                assert!(
                    diff <= 120_000,
                    "{y}-{m}-{d} {what}: {} vs {}",
                    z.local_string(got),
                    z.local_string(want)
                );
            }
        }
    }

    #[test]
    fn the_sun_mode_warms_before_sunset_and_cools_after_sunrise() {
        let z = ny();
        let p = Place {
            lat: 40.7128,
            lon: -74.006,
            from: "test".into(),
        };
        let s = settings();
        let noon = local(&z, 2026, 10, 15, 12, 0);
        let SunDay::Normal { rise, set } = sun_day(p.lat, p.lon, noon) else {
            panic!()
        };
        let at = |t: i64| evaluate(&s, Some(&p), &z, t, None);
        let st = at(noon);
        assert_eq!(
            (st.phase, st.text()),
            (Phase::Day, "Day (6500 K)".to_string())
        );
        assert!(st.lut().is_identity());
        assert_eq!(st.next, Some((set - 3_600_000, Phase::Warming)));
        // halfway through the hour before sunset: halfway in mired
        let st = at(set - 1_800_000);
        assert_eq!(st.phase, Phase::Warming);
        assert!((st.kelvin - kelvin_between(6500.0, 3400.0, 0.5)).abs() < 1.0);
        assert_eq!(st.text(), "Warming: 4460 K");
        assert_eq!(st.next, Some((set, Phase::Night)));
        let st = at(set + 60_000);
        assert_eq!(st.text(), "Night (3400 K)");
        // the next sunrise starts the way back; an hour after it, day
        let st = at(set + 6 * 3_600_000);
        assert_eq!(st.phase, Phase::Night);
        let (next_rise, ph) = st.next.unwrap();
        assert_eq!(ph, Phase::Cooling);
        assert!(
            next_rise > set
                && next_rise - rise > 23 * 3_600_000
                && next_rise - rise < 25 * 3_600_000
        );
        assert_eq!(at(next_rise + 15 * 60_000).phase, Phase::Cooling);
        assert!((at(next_rise + 15 * 60_000).progress - 0.75).abs() < 0.01);
        assert_eq!(at(next_rise + 3_600_000).phase, Phase::Day);
        // continuous: no step bigger than a minute's worth of the ramp, minute by minute around the boundaries
        for (a, b) in [
            (set - 4_000_000, set + 200_000),
            (next_rise - 200_000, next_rise + 4_000_000),
        ] {
            let mut prev = at(a).progress;
            for t in (a..b).step_by(60_000) {
                let p = at(t).progress;
                assert!(
                    (p - prev).abs() <= 1.0 / 60.0 + 1e-9,
                    "a step at {}",
                    z.local_string(t)
                );
                prev = p;
            }
        }
        // the preview: the full night look now, whatever the hour
        let st = evaluate(&s, Some(&p), &z, noon, Some(noon + 15_000));
        assert_eq!((st.kelvin, st.progress), (3400.0, 1.0));
        assert!(!st.lut().is_identity());
        assert!(
            evaluate(&s, Some(&p), &z, noon + 16_000, Some(noon + 15_000))
                .lut()
                .is_identity()
        );
    }

    #[test]
    fn the_schedule_mode_uses_fixed_times() {
        let z = ny();
        let s = NightSettings {
            mode: Mode::Schedule,
            on_min: 20 * 60,
            off_min: 7 * 60,
            ..settings()
        };
        let at = |h: i8, m: i8| evaluate(&s, None, &z, local(&z, 2026, 10, 15, h, m), None);
        assert_eq!(at(12, 0).phase, Phase::Day);
        assert_eq!(at(19, 30).phase, Phase::Warming);
        assert_eq!(at(20, 0).phase, Phase::Night);
        assert_eq!(at(3, 0).phase, Phase::Night);
        assert_eq!(at(7, 30).phase, Phase::Cooling);
        assert_eq!(at(8, 1).phase, Phase::Day);
        // a schedule inside one day (on 01:00, off 05:00) works the same
        let s = NightSettings {
            on_min: 60,
            off_min: 300,
            ..s
        };
        assert_eq!(
            evaluate(&s, None, &z, local(&z, 2026, 10, 15, 2, 0), None).phase,
            Phase::Night
        );
        assert_eq!(
            evaluate(&s, None, &z, local(&z, 2026, 10, 15, 12, 0), None).phase,
            Phase::Day
        );
    }

    #[test]
    fn polar_days_and_nights_stay_day_or_night_without_a_crash() {
        let z = Zone::named("Arctic/Longyearbyen").unwrap();
        let svalbard = Place {
            lat: 78.22,
            lon: 15.65,
            from: "test".into(),
        };
        let s = settings();
        let summer = local(&z, 2026, 6, 21, 23, 0);
        assert_eq!(
            sun_day(svalbard.lat, svalbard.lon, summer),
            SunDay::AlwaysUp
        );
        let st = evaluate(&s, Some(&svalbard), &z, summer, None);
        assert_eq!((st.phase, st.next), (Phase::Day, None));
        assert!(st.lut().is_identity());
        let winter = local(&z, 2026, 12, 21, 12, 0);
        assert_eq!(
            sun_day(svalbard.lat, svalbard.lon, winter),
            SunDay::AlwaysDown
        );
        let st = evaluate(&s, Some(&svalbard), &z, winter, None);
        assert_eq!((st.phase, st.next), (Phase::Night, None));
        // the south pole, and the extreme latitudes themselves
        for lat in [-90.0, 90.0, 89.99, -89.99] {
            for t in [summer, winter] {
                let _ = evaluate(
                    &s,
                    Some(&Place {
                        lat,
                        lon: 0.0,
                        from: String::new(),
                    }),
                    &z,
                    t,
                    None,
                );
            }
        }
        // the first sunrise after the polar night is found, and cooled into
        let feb = local(&z, 2026, 2, 16, 12, 0);
        assert!(matches!(
            sun_day(svalbard.lat, svalbard.lon, feb),
            SunDay::Normal { .. }
        ));
    }

    #[test]
    fn time_zones_give_their_principal_city() {
        assert_eq!(
            parse_iso6709("+404251-0740023"),
            Some((40.0 + 42.0 / 60.0 + 51.0 / 3600.0, -(74.0 + 23.0 / 3600.0)))
        );
        assert_eq!(parse_iso6709("+5130-00008"), Some((51.5, -(8.0 / 60.0))));
        assert_eq!(
            parse_iso6709("-3352+15113"),
            Some((-(33.0 + 52.0 / 60.0), 151.0 + 13.0 / 60.0))
        );
        assert_eq!(parse_iso6709("40N"), None);
        let tab = "# comment\nUS\t+404251-0740023\tAmerica/New_York\tEastern (most areas)\nGB,GG,IM,JE\t+513030-0000731\tEurope/London\n";
        let (lat, lon) = coords_in_tab(tab, "America/New_York").unwrap();
        assert_eq!(
            ((lat * 100.0).round() / 100.0, (lon * 100.0).round() / 100.0),
            (40.71, -74.01)
        );
        assert!(coords_in_tab(tab, "Europe/London").is_some());
        assert_eq!(coords_in_tab(tab, "Etc/UTC"), None);
        assert_eq!(
            link_target(
                "Z America/New_York -4:56:2 - LMT 1883\nL America/New_York US/Eastern\n",
                "US/Eastern"
            )
            .as_deref(),
            Some("America/New_York")
        );
        // the box's own tzdata, where there is one
        if std::path::Path::new("/usr/share/zoneinfo/zone1970.tab").exists()
            && std::env::var_os("HOTCLOCK_ZONEINFO").is_none()
        {
            let (lat, lon) = zone_coords("America/New_York").unwrap();
            assert!(
                (lat - 40.71).abs() < 0.01 && (lon + 74.01).abs() < 0.01,
                "{lat} {lon}"
            );
            assert!(zone_coords("Europe/Berlin").is_some());
            assert_eq!(zone_coords("UTC"), None);
            assert!(zone_names().iter().any(|z| z == "America/New_York"));
        }
    }

    #[test]
    fn the_place_comes_from_the_setting() {
        let z = ny();
        let mut s = settings();
        s.location = LocationSource::Manual;
        assert!(resolve_place(&s, &z, None).is_err());
        s.latitude = Some(52.5);
        s.longitude = Some(13.4);
        assert_eq!(resolve_place(&s, &z, None).unwrap().lat, 52.5);
        s.location = LocationSource::Internet;
        let net = Place {
            lat: 42.36,
            lon: -71.06,
            from: "internet (Boston)".into(),
        };
        assert_eq!(resolve_place(&s, &z, Some(&net)).unwrap(), net);
        let utc = Zone::utc();
        assert!(resolve_place(
            &NightSettings {
                location: LocationSource::Timezone,
                ..s
            },
            &utc,
            None
        )
        .is_err());
        // a sun mode with no place stays day and says why
        let st = evaluate(&settings(), None, &z, 0, None);
        assert_eq!(st.phase, Phase::Day);
        assert!(st.error.is_some() && st.lut().is_identity());
    }

    #[test]
    fn times_of_day_parse() {
        assert_eq!(parse_hhmm("20:00"), Some(1200));
        assert_eq!(parse_hhmm("7:05"), Some(425));
        assert_eq!(parse_hhmm(" 23:59 "), Some(1439));
        assert_eq!(parse_hhmm("7:5"), None);
        assert_eq!(parse_hhmm("-1:30"), None);
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("07:60"), None);
        assert_eq!(parse_hhmm("night"), None);
    }
}
