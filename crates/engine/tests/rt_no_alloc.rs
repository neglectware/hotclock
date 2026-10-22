//! Architecture 1.2: no allocation on the rt-out thread. A global allocator counts every
//! allocation, free and reallocation made by a thread that has registered as the rt thread, and
//! the test asserts the count is zero after minutes of mixed playback including an xrun.
//! (The 10-minute mixed-playback version with voices is M1.2 acceptance 6; this is its scaffold.)

use common::VirtualClock;
use engine::output::NullOutput;
use engine::rt::{Command, CommandKind, RtHandle, RtOptions};
use engine::testing::FaultyOutput;
use engine::{HwRequest, ToneSource};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::sync::Arc;

thread_local! {
    static IS_RT: Cell<bool> = const { Cell::new(false) };
}
static RT_ALLOCS: AtomicU64 = AtomicU64::new(0);
static SIZES: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
fn note(kind: u64, size: usize) {
    let i = RT_ALLOCS.fetch_add(1, Relaxed) as usize;
    if i < SIZES.len() {
        SIZES[i].store(kind << 56 | size as u64, Relaxed);
    }
}

struct Counting;

// SAFETY: forwards every call to the system allocator unchanged; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if IS_RT.try_with(|f| f.get()).unwrap_or(false) {
            note(1, l.size());
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if IS_RT.try_with(|f| f.get()).unwrap_or(false) {
            note(2, l.size());
        }
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if IS_RT.try_with(|f| f.get()).unwrap_or(false) {
            note(3, l.size());
        }
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn rt_thread_never_allocates() {
    // 10 virtual minutes: the virtual clock makes the paced Null backend run flat out.
    let clock = Arc::new(VirtualClock::new(0));
    let null = NullOutput::new(&HwRequest::defaults(), clock.clone());
    let (faulty, ctl) = FaultyOutput::new(Box::new(null));
    let frames = 10 * 60 * 48_000u64;
    let opts = RtOptions {
        max_frames: Some(frames),
        on_start: Some(Arc::new(|| IS_RT.with(|f| f.set(true)))),
        on_exit: Some(Arc::new(|| IS_RT.with(|f| f.set(false)))),
        ..RtOptions::default()
    };
    let mut h = RtHandle::start(
        Box::new(faulty),
        Box::new(ToneSource::new(48_000, 1000.0, -20.0)),
        clock,
        opts,
    )
    .unwrap();
    // Exercise commands, a stall-free xrun and gain changes while it runs.
    ctl.xrun_next.store(3, Release);
    for i in 0..50u64 {
        h.send(Command {
            at_frame: i * 500_000,
            kind: CommandKind::SetGain(if i % 2 == 0 { 0.5 } else { 1.0 }),
        });
        h.send(Command::now(CommandKind::SetToneHz(440.0 + i as f32)));
    }
    while !h.is_finished() {
        while h.poll_event().is_some() {}
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let s = h.stats();
    let allocs = RT_ALLOCS.load(Relaxed);
    println!(
        "rt-out allocations over {} frames (10 min): {allocs} (limit 0); xruns handled: {}",
        s.source_frames, s.xruns
    );
    for (i, v) in SIZES.iter().enumerate().take(allocs as usize) {
        let v = v.load(Relaxed);
        println!(
            "  #{i}: kind {} (1 alloc, 2 free, 3 realloc) size {}",
            v >> 56,
            v & 0xFF_FFFF_FFFF_FFFF
        );
    }
    assert_eq!(s.xruns, 3);
    assert_eq!(s.source_frames, frames);
    assert_eq!(allocs, 0);
    h.wait();
}

/// The same rule holds through a device loss and recovery: the hand-off to the recovery thread,
/// the polling wait and the fade-in allocate nothing on the rt thread.
#[test]
fn rt_thread_never_allocates_through_device_loss() {
    use common::RealClock;
    use std::time::Duration;
    RT_ALLOCS.store(0, Relaxed);
    let clock = Arc::new(RealClock::new());
    let null = NullOutput::new(&HwRequest::defaults(), clock.clone());
    let (faulty, ctl) = FaultyOutput::new(Box::new(null));
    let opts = RtOptions {
        recovery_retry_ns: 100_000_000,
        on_start: Some(Arc::new(|| IS_RT.with(|f| f.set(true)))),
        on_exit: Some(Arc::new(|| IS_RT.with(|f| f.set(false)))),
        ..RtOptions::default()
    };
    let mut h = RtHandle::start(
        Box::new(faulty),
        Box::new(ToneSource::new(48_000, 1000.0, -20.0)),
        clock,
        opts,
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    ctl.unplugged.store(true, Release);
    std::thread::sleep(Duration::from_millis(500));
    ctl.unplugged.store(false, Release);
    std::thread::sleep(Duration::from_millis(500));
    let s = h.stats();
    while h.poll_event().is_some() {}
    h.stop();
    let allocs = RT_ALLOCS.load(Relaxed);
    println!(
        "rt-out allocations through a device loss: {allocs} (limit 0); lost {} restored {}",
        s.device_lost, s.device_restored
    );
    assert_eq!((s.device_lost, s.device_restored), (1, 1));
    assert_eq!(allocs, 0);
}
