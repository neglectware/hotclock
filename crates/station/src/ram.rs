//! The RAM loop (architecture 1.5): up to five minutes of the station's emergency playlist, decoded at
//! start into memory as 16-bit PCM (58 MB for five minutes of stereo at 48 kHz) and kept there. It is the
//! last resort when the disk itself is failing, and what safe mode plays when the emergency playlist cannot
//! be read. The items it plays are copies of the emergency items with a `ram://<id>` path and markers
//! clamped to what was decoded; the conductor opens those as memory voices.

use crate::item::Item;
use engine::decode::FileDecoder;
use engine::pool::RamClip;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct RamItem {
    pub source_item_id: i64,
    pub item: Arc<Item>,
    pub clip: Arc<RamClip>,
}

pub struct RamLoop {
    pub items: Vec<RamItem>,
    pub frames: u64,
}

impl RamLoop {
    pub fn seconds(&self, rate: u32) -> f64 {
        self.frames as f64 / f64::from(rate)
    }
}

/// The path a RAM item carries.
pub fn ram_path(item_id: i64) -> PathBuf {
    PathBuf::from(format!("ram://{item_id}"))
}

/// Decode `items` (`(item id, item)`) to at most `budget_s` seconds. Whole items are taken in order until
/// the next would not fit; if the first alone does not fit, it is taken cut to the budget.
pub fn load(items: &[(i64, Arc<Item>)], rate: u32, budget_s: u32) -> Result<RamLoop, String> {
    let budget = u64::from(budget_s) * u64::from(rate);
    let mut out: Vec<RamItem> = Vec::new();
    let mut total = 0u64;
    let mut last_err = String::from("the emergency playlist has no items");
    for (id, item) in items {
        let mut dec = match FileDecoder::open(&item.path, rate, 0.0) {
            Ok(d) => d,
            Err(e) => {
                last_err = format!("{}: {e}", item.path.display());
                continue;
            }
        };
        let mut pcm: Vec<f32> = Vec::new();
        let room = budget - total;
        let mut ok = true;
        loop {
            match dec.decode_next(&mut pcm) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    last_err = format!("{}: {e}", item.path.display());
                    ok = false;
                    break;
                }
            }
            if (pcm.len() / 2) as u64 > room + 4096 {
                break;
            }
        }
        if !ok || pcm.is_empty() {
            continue;
        }
        let mut frames = (pcm.len() / 2) as u64;
        let cut = frames > room;
        if cut {
            if !out.is_empty() {
                break;
            }
            frames = room;
            pcm.truncate(frames as usize * 2);
        }
        let samples: Vec<i16> = pcm
            .iter()
            .map(|&v| (v * 32767.0).round().clamp(-32768.0, 32767.0) as i16)
            .collect();
        let length_ms = (frames * 1000 / u64::from(rate)) as i64;
        let mut it = (**item).clone();
        it.path = ram_path(*id);
        it.file_ms = length_ms.min(it.file_ms.max(1));
        let m = &mut it.markers;
        m.cue_out = m.cue_out.min(length_ms);
        m.segue = m.segue.min(m.cue_out);
        m.attack = m.attack.min(m.segue);
        m.cue_in = m.cue_in.min(m.segue);
        if let Some(f) = m.fade_out {
            m.fade_out = (f < m.cue_out).then_some(f);
        }
        if let Some(i) = m.intro {
            m.intro = (i < m.segue).then_some(i);
        }
        total += frames;
        out.push(RamItem {
            source_item_id: *id,
            item: Arc::new(it),
            clip: Arc::new(RamClip { samples, rate }),
        });
        if cut || total >= budget {
            break;
        }
    }
    if out.is_empty() {
        return Err(last_err);
    }
    Ok(RamLoop {
        items: out,
        frames: total,
    })
}

/// A load in progress on its own thread.
#[derive(Default)]
pub struct RamSlot {
    inner: Mutex<Option<Result<Arc<RamLoop>, String>>>,
}

impl RamSlot {
    pub fn take(&self) -> Option<Result<Arc<RamLoop>, String>> {
        self.inner.lock().ok().and_then(|mut g| g.take())
    }
}

pub fn spawn_load(items: Vec<(i64, Arc<Item>)>, rate: u32, budget_s: u32) -> Arc<RamSlot> {
    let slot = Arc::new(RamSlot::default());
    let s = slot.clone();
    let _ = std::thread::Builder::new()
        .name("ram-loop".into())
        .spawn(move || {
            let r = load(&items, rate, budget_s).map(Arc::new);
            if let Ok(mut g) = s.inner.lock() {
                *g = Some(r);
            }
        });
    slot
}
