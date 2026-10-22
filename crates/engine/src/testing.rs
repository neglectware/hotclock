//! Fault injection for tests: wraps any `Output` and fails on command. Public so that the
//! `hotclock` crate's tests and the hardware procedure can use it. Not used in production.

use crate::fault::Fault;
use crate::negotiate::Granted;
use crate::output::{Output, OutputError};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::*};
use std::sync::Arc;

#[derive(Default)]
pub struct FaultControl {
    /// While true, `write` fails with `Lost` and `reopen` fails (the dongle is unplugged).
    pub unplugged: AtomicBool,
    /// The next this-many writes fail with `Xrun`.
    pub xrun_next: AtomicU32,
    /// How many times `reopen` has been called.
    pub reopen_attempts: AtomicU32,
}

pub struct FaultyOutput {
    inner: Box<dyn Output>,
    ctl: Arc<FaultControl>,
    /// Writes that pass without injection after a recovery (the silence refill).
    grace: u32,
}

impl FaultyOutput {
    pub fn new(inner: Box<dyn Output>) -> (Self, Arc<FaultControl>) {
        let ctl = Arc::new(FaultControl::default());
        (
            Self {
                inner,
                ctl: ctl.clone(),
                grace: 0,
            },
            ctl,
        )
    }
}

impl Output for FaultyOutput {
    fn name(&self) -> &str {
        "faulty"
    }
    fn granted(&self) -> &Granted {
        self.inner.granted()
    }
    fn write(&mut self, samples: &[f32]) -> Result<(), OutputError> {
        if self.ctl.unplugged.load(Acquire) {
            return Err(OutputError::new(Fault::Lost, "unplugged (test)"));
        }
        let n = self.ctl.xrun_next.load(Acquire);
        if self.grace > 0 {
            self.grace -= 1;
        } else if n > 0 {
            self.ctl.xrun_next.store(n - 1, Release);
            return Err(OutputError::new(Fault::Xrun, "injected xrun"));
        }
        self.inner.write(samples)
    }
    fn delay_frames(&mut self) -> Option<i64> {
        self.inner.delay_frames()
    }
    fn recover(&mut self, fault: Fault) -> Result<(), OutputError> {
        // The engine refills one buffer of silence after a recovery; let those writes through.
        self.grace = self.inner.granted().periods;
        self.inner.recover(fault)
    }
    fn reopen(&mut self) -> Result<(), String> {
        self.ctl.reopen_attempts.fetch_add(1, AcqRel);
        if self.ctl.unplugged.load(Acquire) {
            return Err("still unplugged (test)".into());
        }
        self.inner.reopen()
    }
    fn finish(&mut self) -> Result<(), String> {
        self.inner.finish()
    }
}
