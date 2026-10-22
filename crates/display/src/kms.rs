//! The KMS/DRM output (architecture 11.1): dumb buffers, mode setting from `display.mode` without trusting the
//! panel's EDID for the resolution (7d), page flips with damage-only copies, and the hotplug check.
//!
//! The mode comes from `display.mode`. If the connector lists exactly that size it is used; otherwise a CVT
//! mode is added as a custom mode. If the kernel refuses it, the display logs the failure, runs on the mode the
//! connector offers with the 800 x 480 fallback layout, and puts an alarm on the screen.

#![allow(unsafe_code)]

use crate::canvas::{theme, Canvas};
use crate::cvt;
use crate::layout::Rect;
use crate::output::Output;
use common::config::{parse_mode, DisplayConfig};
use drm::buffer::DrmFourcc;
use drm::control::{
    self, connector, crtc, dumbbuffer::DumbBuffer, dumbbuffer::DumbMapping, framebuffer,
    Device as ControlDevice, Mode, PageFlipFlags,
};
use drm::Device;
use std::os::fd::{AsFd, BorrowedFd};
use std::time::{Duration, Instant};

struct Card(std::fs::File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Device for Card {}
impl ControlDevice for Card {}

struct Slot {
    fb: framebuffer::Handle,
    /// Kept so the buffer outlives its mapping; destroyed with the slot.
    buf: Option<DumbBuffer>,
    map: Option<DumbMapping<'static>>,
    pitch: usize,
}

/// A text-mode console switched to graphics for as long as the display runs, so the kernel's console does not
/// draw over it, and put back afterwards.
struct VtGuard(i32);

impl VtGuard {
    fn new() -> Option<VtGuard> {
        // SAFETY: isatty and ioctl on the process's own standard input.
        unsafe {
            if libc::isatty(0) != 1 {
                return None;
            }
            const KDSETMODE: libc::c_ulong = 0x4B3A;
            const KD_GRAPHICS: libc::c_int = 0x01;
            if libc::ioctl(0, KDSETMODE, KD_GRAPHICS) != 0 {
                tracing::warn!(
                    "kms: cannot switch the console to graphics mode: {}",
                    std::io::Error::last_os_error()
                );
                return None;
            }
        }
        Some(VtGuard(0))
    }
}

impl Drop for VtGuard {
    fn drop(&mut self) {
        // SAFETY: restores the mode set in `new`.
        unsafe {
            libc::ioctl(self.0, 0x4B3A as libc::c_ulong, 0x00 as libc::c_int);
        }
    }
}

pub struct Kms {
    card: Card,
    crtc: crtc::Handle,
    conn: connector::Handle,
    conn_name: String,
    w: i32,
    h: i32,
    slots: [Slot; 2],
    front: usize,
    flip_at: Option<Instant>,
    stale: [Vec<Rect>; 2],
    describe: String,
    alarm: Option<String>,
    fallback_layout: bool,
    last_check: Instant,
    _vt: Option<VtGuard>,
}

fn mode_str(m: &Mode) -> String {
    format!(
        "{}x{}@{}{}",
        m.size().0,
        m.size().1,
        m.vrefresh(),
        if m.mode_type().contains(control::ModeTypeFlags::PREFERRED) {
            "*"
        } else {
            ""
        }
    )
}

fn cards(cfg: &DisplayConfig) -> Vec<std::path::PathBuf> {
    if let Some(d) = &cfg.device {
        return vec![d.into()];
    }
    let mut v: Vec<_> = std::fs::read_dir("/dev/dri")
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("card"))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn open_card(p: &std::path::Path) -> Result<Card, String> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(p)
        .map(Card)
        .map_err(|e| format!("{}: {e}", p.display()))
}

fn make_slot(card: &Card, w: u32, h: u32) -> Result<Slot, String> {
    let mut buf = card
        .create_dumb_buffer((w, h), DrmFourcc::Xrgb8888, 32)
        .map_err(|e| format!("create_dumb_buffer: {e}"))?;
    let fb = card
        .add_framebuffer(&buf, 24, 32)
        .map_err(|e| format!("add_framebuffer: {e}"))?;
    let pitch = drm::buffer::Buffer::pitch(&buf) as usize;
    let map = card
        .map_dumb_buffer(&mut buf)
        .map_err(|e| format!("map_dumb_buffer: {e}"))?;
    // SAFETY: the mapping does not borrow the buffer's memory, only its handle for the type; the slot keeps the
    // buffer alive until after the mapping is dropped (field order in `Drop for Slot`).
    let map: DumbMapping<'static> =
        unsafe { std::mem::transmute::<DumbMapping<'_>, DumbMapping<'static>>(map) };
    let mut s = Slot {
        fb,
        buf: Some(buf),
        map: Some(map),
        pitch,
    };
    // start from the background color, so the first flip shows no garbage
    let bg = [theme::BG.2, theme::BG.1, theme::BG.0, 0];
    if let Some(m) = s.map.as_mut() {
        for px in m.as_mut().chunks_exact_mut(4) {
            px.copy_from_slice(&bg);
        }
    }
    Ok(s)
}

fn free_slot(card: &Card, mut s: Slot) {
    s.map = None;
    let _ = card.destroy_framebuffer(s.fb);
    if let Some(b) = s.buf.take() {
        let _ = card.destroy_dumb_buffer(b);
    }
}

fn custom_mode(w: u32, h: u32, hz: u32) -> Mode {
    let m = cvt::cvt(w, h, hz);
    let mut name = [0 as std::ffi::c_char; 32];
    for (i, b) in format!("{w}x{h}").bytes().take(31).enumerate() {
        name[i] = b as std::ffi::c_char;
    }
    let raw = drm_ffi::drm_sys::drm_mode_modeinfo {
        clock: m.clock_khz,
        hdisplay: m.hdisplay,
        hsync_start: m.hsync_start,
        hsync_end: m.hsync_end,
        htotal: m.htotal,
        hskew: 0,
        vdisplay: m.vdisplay,
        vsync_start: m.vsync_start,
        vsync_end: m.vsync_end,
        vtotal: m.vtotal,
        vscan: 0,
        vrefresh: m.vrefresh,
        flags: control::ModeFlags::NHSYNC.bits() | control::ModeFlags::PVSYNC.bits(),
        type_: control::ModeTypeFlags::USERDEF.bits(),
        name,
    };
    Mode::from(raw)
}

impl Kms {
    /// Open the first card with a connected connector (or the configured one) and set the mode.
    pub fn open(cfg: &DisplayConfig) -> Result<Kms, String> {
        let (w, h, hz) = parse_mode(&cfg.mode).ok_or("bad display.mode")?;
        let mut last_err = "no DRM device".to_string();
        for path in cards(cfg) {
            let card = match open_card(&path) {
                Ok(c) => c,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            let res = match card.resource_handles() {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("{}: {e}", path.display());
                    continue;
                }
            };
            let mut chosen = None;
            for &ch in &res.connectors {
                let Ok(info) = card.get_connector(ch, true) else {
                    continue;
                };
                let name = info.to_string();
                if cfg.connector.as_ref().is_some_and(|c| *c != name) {
                    continue;
                }
                if info.state() == connector::State::Connected {
                    chosen = Some((ch, info, name));
                    break;
                }
            }
            let Some((ch, info, name)) = chosen else {
                last_err = format!("{}: no connected output", path.display());
                continue;
            };
            let modes: Vec<String> = info.modes().iter().map(mode_str).collect();
            let preferred = info
                .modes()
                .iter()
                .find(|m| m.mode_type().contains(control::ModeTypeFlags::PREFERRED))
                .map(mode_str);
            let describe = format!(
                "{} on {}: panel EDID lists {} modes, preferred {} ({}); display.mode asks for {w}x{h}@{hz}",
                name,
                path.display(),
                modes.len(),
                preferred.as_deref().unwrap_or("none"),
                modes.join(" ")
            );
            tracing::info!("kms: {describe}");
            // the mode: an exact size from the list, nearest to the asked refresh; else the custom one
            let listed = info
                .modes()
                .iter()
                .filter(|m| {
                    u32::from(m.size().0) == w
                        && u32::from(m.size().1) == h
                        && !m.flags().contains(control::ModeFlags::INTERLACE)
                })
                .min_by_key(|m| (m.vrefresh() as i64 - i64::from(hz)).abs())
                .copied();
            let (mode, custom) = match listed {
                Some(m) => (m, false),
                None => {
                    tracing::warn!("kms: {name} lists no {w}x{h} mode: adding a custom CVT mode");
                    (custom_mode(w, h, hz), true)
                }
            };
            // the crtc: the one the connector's encoder drives now, else the first one an encoder can use
            let mut crtc_h = info
                .current_encoder()
                .and_then(|e| card.get_encoder(e).ok())
                .and_then(|e| e.crtc());
            if crtc_h.is_none() {
                for &e in info.encoders() {
                    if let Ok(enc) = card.get_encoder(e) {
                        if let Some(c) = res.filter_crtcs(enc.possible_crtcs()).first() {
                            crtc_h = Some(*c);
                            break;
                        }
                    }
                }
            }
            let Some(crtc_h) = crtc_h else {
                last_err = format!("{name}: no CRTC can drive this connector");
                continue;
            };
            let vt = VtGuard::new();
            let try_mode = |card: &Card, mode: Mode, w: u32, h: u32| -> Result<[Slot; 2], String> {
                let s0 = make_slot(card, w, h)?;
                let s1 = make_slot(card, w, h)?;
                if let Err(e) = card.set_crtc(crtc_h, Some(s0.fb), (0, 0), &[ch], Some(mode)) {
                    free_slot(card, s0);
                    free_slot(card, s1);
                    return Err(format!("set_crtc {}: {e}", mode_str(&mode)));
                }
                Ok([s0, s1])
            };
            let (slots, final_mode, alarm, fallback) = match try_mode(&card, mode, w, h) {
                Ok(s) => (s, mode, None, false),
                Err(e) => {
                    // the panel (or the driver) refuses: run on what the connector offers, with the 800 x 480 layout
                    tracing::error!("kms: {e}; falling back to the connector's own mode and the 800 x 480 layout");
                    let alt = info
                        .modes()
                        .iter()
                        .find(|m| m.mode_type().contains(control::ModeTypeFlags::PREFERRED))
                        .or_else(|| info.modes().first())
                        .copied();
                    let Some(alt) = alt else {
                        last_err = format!("{name}: {e}, and the connector offers no mode");
                        continue;
                    };
                    let (aw, ah) = (u32::from(alt.size().0), u32::from(alt.size().1));
                    match try_mode(&card, alt, aw, ah) {
                        Ok(s) => (
                            s,
                            alt,
                            Some(format!("display: {w}x{h} refused ({e}); running {aw}x{ah}")),
                            true,
                        ),
                        Err(e2) => {
                            last_err = format!("{name}: {e}; fallback: {e2}");
                            continue;
                        }
                    }
                }
            };
            let (fw, fh) = (final_mode.size().0 as i32, final_mode.size().1 as i32);
            tracing::info!(
                "kms: {name}: mode {} set on {:?}",
                mode_str(&final_mode),
                crtc_h
            );
            return Ok(Kms {
                card,
                crtc: crtc_h,
                conn: ch,
                conn_name: name,
                w: fw,
                h: fh,
                slots,
                front: 0,
                flip_at: None,
                stale: [vec![], vec![]],
                describe: format!(
                    "{describe}; mode in use {}{}",
                    mode_str(&final_mode),
                    if custom { " (custom)" } else { "" }
                ),
                alarm,
                fallback_layout: fallback,
                last_check: Instant::now(),
                _vt: vt,
            });
        }
        Err(last_err)
    }

    fn drain_events(&mut self) -> Result<(), String> {
        let fd = self.card.as_fd();
        let mut p = libc::pollfd {
            fd: std::os::fd::AsRawFd::as_raw_fd(&fd),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, zero timeout.
        let n = unsafe { libc::poll(&mut p, 1, 0) };
        if n > 0 && p.revents & libc::POLLIN != 0 {
            let evs = self
                .card
                .receive_events()
                .map_err(|e| format!("receive_events: {e}"))?;
            for e in evs {
                if let control::Event::PageFlip(_) = e {
                    self.flip_at = None;
                }
            }
        }
        if let Some(t) = self.flip_at {
            if t.elapsed() > Duration::from_secs(2) {
                return Err("a page flip did not complete in 2 s".into());
            }
        }
        Ok(())
    }
}

fn add_damage(list: &mut Vec<Rect>, d: &[Rect]) {
    list.extend_from_slice(d);
    if list.len() > 8 {
        let u = list.iter().fold(Rect::default(), |a, r| a.union(r));
        list.clear();
        list.push(u);
    }
}

impl Output for Kms {
    fn size(&self) -> (i32, i32) {
        (self.w, self.h)
    }

    fn present(&mut self, canvas: &Canvas, damage: &[Rect]) -> Result<(), String> {
        self.drain_events()?;
        for s in &mut self.stale {
            add_damage(s, damage);
        }
        if self.flip_at.is_some() {
            return Ok(());
        }
        let back = 1 - self.front;
        if self.stale[back].is_empty() {
            return Ok(());
        }
        let rects = std::mem::take(&mut self.stale[back]);
        let pitch = self.slots[back].pitch;
        if let Some(m) = self.slots[back].map.as_mut() {
            for r in &rects {
                canvas.copy_xrgb(*r, m.as_mut(), pitch);
            }
        }
        match self
            .card
            .page_flip(self.crtc, self.slots[back].fb, PageFlipFlags::EVENT, None)
        {
            Ok(()) => {
                self.front = back;
                self.flip_at = Some(Instant::now());
                Ok(())
            }
            Err(e) => {
                // put the damage back: the buffer was written but not shown
                self.stale[back] = rects;
                Err(format!("page_flip: {e}"))
            }
        }
    }

    fn connected(&mut self) -> bool {
        if self.last_check.elapsed() < Duration::from_secs(1) {
            return true;
        }
        self.last_check = Instant::now();
        match self.card.get_connector(self.conn, false) {
            Ok(i) => i.state() == connector::State::Connected,
            Err(_) => false,
        }
    }

    fn alarm(&self) -> Option<String> {
        self.alarm.clone()
    }

    fn force_fallback_layout(&self) -> bool {
        self.fallback_layout
    }

    fn describe(&self) -> String {
        format!("{} {}", self.conn_name, self.describe)
    }
}
