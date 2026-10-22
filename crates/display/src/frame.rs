//! The 220-byte version 3 meter frame (architecture 11.3), decoded. Levels are `i16` in 0.01 dB with `-32768`
//! meaning "no value"; the spectra are `round((dB + 90) * 255 / 90)` bytes.

pub const FRAME_LEN: usize = 220;
pub const NONE: i16 = -32768;

/// Stage order of bytes 32 to 67.
pub const STAGES: [&str; 6] = [
    "agc",
    "multiband",
    "wideband",
    "limiter",
    "clipper",
    "safety_limiter",
];

#[derive(Clone, Debug, PartialEq)]
pub struct StageMeter {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub gr: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub version: u8,
    pub flags: u8,
    pub seq: u32,
    pub audible_frame: u64,
    pub peak: [Option<f64>; 2],
    pub ppm: [Option<f64>; 2],
    pub momentary: Option<f64>,
    pub short_term: Option<f64>,
    pub stage_mask: u8,
    pub band_count: u8,
    pub chain_cfg_rev: u16,
    pub stages: [StageMeter; 6],
    pub band_gr: [Option<f64>; 6],
    /// Spectrum in dB relative to full scale, 64 log-spaced bands.
    pub post: [f64; 64],
    pub pre: [f64; 64],
    pub integrated: Option<f64>,
    pub window: Option<f64>,
    pub target: Option<f64>,
    pub gain_db: f64,
    /// 0 off, 1 converging, 2 on target, 3 frozen, 4 at limit.
    pub loudness_state: u8,
}

fn lv(v: i16) -> Option<f64> {
    (v != NONE).then(|| f64::from(v) / 100.0)
}

fn i16_at(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}

impl Frame {
    pub fn bypassed(&self) -> bool {
        self.flags & 1 != 0
    }

    /// The sample peak into the processing chain, in dBFS: the input of the first stage that is on (AGC, multiband,
    /// wideband, limiter, clipper), or, with the chain bypassed, the program's own peak (the louder channel).
    pub fn input_peak(&self) -> Option<f64> {
        if !self.bypassed() {
            for i in 0..5 {
                if self.stage_enabled(i) {
                    if let Some(v) = self.stages[i].input {
                        return Some(v);
                    }
                }
            }
        }
        match (self.peak[0], self.peak[1]) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// Stage `i` (bit order AGC, multiband, wideband, limiter, clipper, safety limiter, loudness controller) is active.
    pub fn stage_enabled(&self, i: usize) -> bool {
        self.stage_mask & (1 << i) != 0
    }

    /// Decode a frame. `None` when the length or the version is wrong; a version 2 frame (208 bytes) is accepted
    /// with the loudness fields empty.
    pub fn parse(b: &[u8]) -> Option<Frame> {
        if b.len() < 208 || !(b[0] == 3 || b[0] == 2) {
            return None;
        }
        let v3 = b[0] == 3 && b.len() >= FRAME_LEN;
        let stage = |i: usize| {
            let o = 32 + i * 6;
            StageMeter {
                input: lv(i16_at(b, o)),
                output: lv(i16_at(b, o + 2)),
                gr: lv(i16_at(b, o + 4)),
            }
        };
        let mut post = [0.0; 64];
        let mut pre = [0.0; 64];
        for i in 0..64 {
            post[i] = f64::from(b[80 + i]) * 90.0 / 255.0 - 90.0;
            pre[i] = f64::from(b[144 + i]) * 90.0 / 255.0 - 90.0;
        }
        Some(Frame {
            version: b[0],
            flags: b[1],
            seq: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            audible_frame: u64::from_le_bytes(b[8..16].try_into().ok()?),
            peak: [lv(i16_at(b, 16)), lv(i16_at(b, 18))],
            ppm: [lv(i16_at(b, 20)), lv(i16_at(b, 22))],
            momentary: lv(i16_at(b, 24)),
            short_term: lv(i16_at(b, 26)),
            stage_mask: b[28],
            band_count: b[29],
            chain_cfg_rev: u16::from_le_bytes([b[30], b[31]]),
            stages: [stage(0), stage(1), stage(2), stage(3), stage(4), stage(5)],
            band_gr: std::array::from_fn(|i| lv(i16_at(b, 68 + i * 2))),
            post,
            pre,
            integrated: if v3 { lv(i16_at(b, 208)) } else { None },
            window: if v3 { lv(i16_at(b, 210)) } else { None },
            target: if v3 { lv(i16_at(b, 212)) } else { None },
            gain_db: if v3 {
                f64::from(i16_at(b, 214)) / 100.0
            } else {
                0.0
            },
            loudness_state: if v3 { b[216] } else { 0 },
        })
    }

    /// Build the wire bytes of a frame from the fields (the feeds and the tests make frames this way).
    pub fn to_bytes(&self) -> [u8; FRAME_LEN] {
        let mut b = [0u8; FRAME_LEN];
        let put = |b: &mut [u8; FRAME_LEN], o: usize, v: Option<f64>| {
            let x = v.map_or(NONE, |d| {
                (d * 100.0).round().clamp(-32767.0, 32767.0) as i16
            });
            b[o..o + 2].copy_from_slice(&x.to_le_bytes());
        };
        b[0] = 3;
        b[1] = self.flags;
        b[2..4].copy_from_slice(&(FRAME_LEN as u16).to_le_bytes());
        b[4..8].copy_from_slice(&self.seq.to_le_bytes());
        b[8..16].copy_from_slice(&self.audible_frame.to_le_bytes());
        put(&mut b, 16, self.peak[0]);
        put(&mut b, 18, self.peak[1]);
        put(&mut b, 20, self.ppm[0]);
        put(&mut b, 22, self.ppm[1]);
        put(&mut b, 24, self.momentary);
        put(&mut b, 26, self.short_term);
        b[28] = self.stage_mask;
        b[29] = self.band_count;
        b[30..32].copy_from_slice(&self.chain_cfg_rev.to_le_bytes());
        for (i, s) in self.stages.iter().enumerate() {
            put(&mut b, 32 + i * 6, s.input);
            put(&mut b, 34 + i * 6, s.output);
            put(&mut b, 36 + i * 6, s.gr);
        }
        for (i, g) in self.band_gr.iter().enumerate() {
            put(&mut b, 68 + i * 2, *g);
        }
        for i in 0..64 {
            b[80 + i] = ((self.post[i] + 90.0) * 255.0 / 90.0)
                .round()
                .clamp(0.0, 255.0) as u8;
            b[144 + i] = ((self.pre[i] + 90.0) * 255.0 / 90.0)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
        put(&mut b, 208, self.integrated);
        put(&mut b, 210, self.window);
        put(&mut b, 212, self.target);
        put(&mut b, 214, Some(self.gain_db));
        b[216] = self.loudness_state;
        b
    }

    /// A silent frame: no signal, every band at the floor, the chain bypassed (what the station sends in M1).
    pub fn silent() -> Frame {
        let none = StageMeter {
            input: None,
            output: None,
            gr: None,
        };
        Frame {
            version: 3,
            flags: 1,
            seq: 0,
            audible_frame: 0,
            peak: [None, None],
            ppm: [None, None],
            momentary: None,
            short_term: None,
            stage_mask: 0,
            band_count: 0,
            chain_cfg_rev: 0,
            stages: std::array::from_fn(|_| none.clone()),
            band_gr: [None; 6],
            post: [-90.0; 64],
            pre: [-90.0; 64],
            integrated: None,
            window: None,
            target: None,
            gain_db: 0.0,
            loudness_state: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_the_servers_decoder_agree() {
        let mut f = Frame::silent();
        f.flags = 0;
        f.ppm = [Some(-20.0), Some(-21.5)];
        f.peak = [Some(-19.9), None];
        f.window = Some(-14.1);
        f.integrated = Some(-14.0);
        f.target = Some(-14.0);
        f.band_gr[1] = Some(-6.5);
        f.stages[1].gr = Some(-6.5);
        f.stage_mask = 0b10;
        f.post[30] = -30.0;
        let bytes = f.to_bytes();
        let g = Frame::parse(&bytes).unwrap();
        assert_eq!(g.ppm, f.ppm);
        assert_eq!(g.peak, f.peak);
        assert_eq!(g.window, Some(-14.1));
        assert_eq!(g.band_gr[1], Some(-6.5));
        assert!(g.stage_enabled(1) && !g.stage_enabled(0));
        // the same decoding as the server's JSON view of the frame
        let j = serve::meters::frame_json(&bytes);
        assert_eq!(j["ppm_l"].as_f64(), Some(-20.0));
        assert_eq!(j["window"].as_f64(), Some(-14.1));
        assert_eq!(j["band_gr"][1].as_f64(), Some(-6.5));
        let sp = j["spectrum_post"][30].as_f64().unwrap();
        assert!((sp - g.post[30]).abs() < 1e-9);
        assert!(Frame::parse(&bytes[..100]).is_none());
    }
}
