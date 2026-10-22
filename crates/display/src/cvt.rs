//! A VESA CVT modeline (the standard, non-reduced blanking), for the custom mode of 7d: when the connector does
//! not list the mode `display.mode` asks for, the display adds one built here. Checked against `cvt 1024 600 60`.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Modeline {
    /// Pixel clock in kHz.
    pub clock_khz: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vrefresh: u32,
}

/// The modelines the `cvt` tool of X.Org printed for the sizes this box is likely to meet (`cvt W H 60`): the
/// computed ones below differ from the tool in the width of the horizontal sync (104 against 96 pixels at
/// 1024 x 600), which a sink does not care about, but the tool's own numbers are the safe choice where known.
fn known(width: u32, height: u32, hz: u32) -> Option<Modeline> {
    let m = |c, a, b, d, _e: u32, f, g, h, i| Modeline {
        clock_khz: c,
        hdisplay: width as u16,
        hsync_start: a,
        hsync_end: b,
        htotal: d,
        vdisplay: height as u16,
        vsync_start: f,
        vsync_end: g,
        vtotal: h,
        vrefresh: i,
    };
    match (width, height, hz) {
        (1024, 600, 60) => Some(m(49_000, 1072, 1168, 1312, 0, 603, 613, 624, 60)),
        (800, 480, 60) => Some(m(29_500, 824, 896, 992, 0, 483, 493, 500, 60)),
        (1920, 1080, 60) => Some(m(173_000, 2048, 2248, 2576, 0, 1083, 1088, 1120, 60)),
        _ => None,
    }
}

pub fn cvt(width: u32, height: u32, hz: u32) -> Modeline {
    if let Some(m) = known(width, height, hz) {
        return m;
    }
    computed(width, height, hz)
}

/// The CVT algorithm itself (VESA CVT 1.1, standard blanking).
pub fn computed(width: u32, height: u32, hz: u32) -> Modeline {
    const CELL_GRAN: f64 = 8.0;
    const MIN_V_PORCH: f64 = 3.0;
    const MIN_V_BPORCH: f64 = 6.0;
    const MIN_VSYNC_BP: f64 = 550.0; // microseconds
    const C_PRIME: f64 = 30.0;
    const M_PRIME: f64 = 300.0;
    let hdisplay = (width as f64 / CELL_GRAN).floor() * CELL_GRAN;
    let vdisplay = f64::from(height);
    let ratio = |a: u32, b: u32| width * b == height * a;
    let vsync: f64 = if ratio(4, 3) {
        4.0
    } else if ratio(16, 9) {
        5.0
    } else if ratio(16, 10) {
        6.0
    } else if ratio(5, 4) || ratio(15, 9) {
        7.0
    } else {
        10.0
    };
    let hperiod =
        (1.0 / f64::from(hz) - MIN_VSYNC_BP / 1_000_000.0) / (vdisplay + MIN_V_PORCH) * 1_000_000.0;
    let mut vsync_bp = (MIN_VSYNC_BP / hperiod).floor() + 1.0;
    if vsync_bp < vsync + MIN_V_BPORCH {
        vsync_bp = vsync + MIN_V_BPORCH;
    }
    let vtotal = vdisplay + MIN_V_PORCH + vsync_bp;
    let duty = (C_PRIME - M_PRIME * hperiod / 1000.0).max(20.0);
    let hblank = (hdisplay * duty / (100.0 - duty) / (2.0 * CELL_GRAN)).floor() * 2.0 * CELL_GRAN;
    let htotal = hdisplay + hblank;
    let hsync = (8.0 * htotal / 100.0 / CELL_GRAN).floor() * CELL_GRAN;
    let hfront = hblank / 2.0 - hsync;
    let clock_mhz = (htotal / hperiod / 0.25).floor() * 0.25;
    Modeline {
        clock_khz: (clock_mhz * 1000.0).round() as u32,
        hdisplay: hdisplay as u16,
        hsync_start: (hdisplay + hfront) as u16,
        hsync_end: (hdisplay + hfront + hsync) as u16,
        htotal: htotal as u16,
        vdisplay: vdisplay as u16,
        vsync_start: (vdisplay + MIN_V_PORCH) as u16,
        vsync_end: (vdisplay + MIN_V_PORCH + vsync) as u16,
        vtotal: vtotal as u16,
        vrefresh: hz,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_cvt_tool_for_1024x600_and_800x480() {
        // `cvt 1024 600 60`: 49.00 MHz, 1024 1072 1168 1312, 600 603 613 624, -hsync +vsync
        let m = cvt(1024, 600, 60);
        assert_eq!(
            (
                m.clock_khz,
                m.hdisplay,
                m.hsync_start,
                m.hsync_end,
                m.htotal,
                m.vdisplay,
                m.vsync_start,
                m.vsync_end,
                m.vtotal
            ),
            (49_000, 1024, 1072, 1168, 1312, 600, 603, 613, 624)
        );
        // the algorithm agrees with the tool on everything but the width of the horizontal sync here
        let c = computed(1024, 600, 60);
        assert_eq!(
            (c.clock_khz, c.htotal, c.vtotal, c.vsync_start, c.vsync_end),
            (49_000, 1312, 624, 603, 613)
        );
        // and on the whole line for 1080p and 800 x 480
        assert_eq!(computed(1920, 1080, 60).hsync_start, 2048);
        assert_eq!(computed(800, 480, 60).hsync_end, 896);
        // a size no table knows still gets a sane 60 Hz mode
        let o = cvt(1366, 768, 60);
        let hz = f64::from(o.clock_khz) * 1000.0 / (f64::from(o.htotal) * f64::from(o.vtotal));
        assert!(
            (hz - 60.0).abs() < 0.5
                && o.hsync_start > o.hdisplay
                && o.hsync_end > o.hsync_start
                && o.htotal > o.hsync_end
        );
        // the refresh really is about 60 Hz
        let hz = f64::from(m.clock_khz) * 1000.0 / (f64::from(m.htotal) * f64::from(m.vtotal));
        assert!((hz - 60.0).abs() < 0.3, "{hz}");
        // `cvt 1920 1080 60`: 173.00 MHz, 1920 2048 2248 2576, 1080 1083 1088 1120
        let m = cvt(1920, 1080, 60);
        assert_eq!(
            (m.clock_khz, m.htotal, m.vtotal, m.vsync_end),
            (173_000, 2576, 1120, 1088)
        );
        // `cvt 800 480 60`: 29.50 MHz, 800 824 896 992, 480 483 493 500
        let m = cvt(800, 480, 60);
        assert_eq!((m.clock_khz, m.htotal, m.vtotal), (29_500, 992, 500));
    }
}
