//! Process-level real-time setup: scheduling priority and memory locking. Linux only in effect;
//! elsewhere these report `NotSupported`. Uses safe wrappers only (`thread-priority`, `rustix`,
//! `nix`), so the crate stays free of `unsafe`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Priority {
    /// `SCHED_FIFO` at this priority.
    Fifo(u8),
    /// FIFO was refused; the thread's nice value is now this.
    Nice(i32),
    /// Neither worked.
    Failed(String),
    NotSupported,
}

impl Priority {
    pub fn describe(&self) -> String {
        match self {
            Priority::Fifo(p) => format!("SCHED_FIFO {p}"),
            Priority::Nice(n) => format!("nice {n} (SCHED_FIFO refused)"),
            Priority::Failed(e) => format!("no priority change: {e}"),
            Priority::NotSupported => "not supported on this OS".into(),
        }
    }
}

/// Ask for `SCHED_FIFO` at `prio` for the calling thread; if refused, fall back to nice -10 and
/// let the caller warn (architecture 1.2).
#[cfg(target_os = "linux")]
pub fn request_realtime(prio: u8) -> Priority {
    use thread_priority::{
        set_thread_priority_and_policy, thread_native_id, RealtimeThreadSchedulePolicy,
        ThreadPriority, ThreadPriorityValue, ThreadSchedulePolicy,
    };
    let value = match ThreadPriorityValue::try_from(prio) {
        Ok(v) => v,
        Err(e) => return Priority::Failed(format!("bad priority {prio}: {e}")),
    };
    let fifo = set_thread_priority_and_policy(
        thread_native_id(),
        ThreadPriority::Crossplatform(value),
        ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo),
    );
    match fifo {
        Ok(()) => Priority::Fifo(prio),
        Err(fifo_err) => match rustix::process::nice(-10) {
            Ok(n) => Priority::Nice(n),
            Err(e) => Priority::Failed(format!("FIFO: {fifo_err:?}; nice: {e}")),
        },
    }
}

#[cfg(not(target_os = "linux"))]
pub fn request_realtime(_prio: u8) -> Priority {
    Priority::NotSupported
}

/// `mlockall(MCL_CURRENT | MCL_FUTURE)`, so the audio path never page-faults (architecture 1.2).
#[cfg(target_os = "linux")]
pub fn lock_memory() -> Result<(), String> {
    use nix::sys::mman::{mlockall, MlockAllFlags};
    mlockall(MlockAllFlags::MCL_CURRENT | MlockAllFlags::MCL_FUTURE).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "linux"))]
pub fn lock_memory() -> Result<(), String> {
    Err("mlockall is only used on Linux".into())
}
