// SPDX-License-Identifier: EUPL-1.2
//! Shared process cancellation and input-session lifetime.
use crate::{adb, input};
use anyhow::Result;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub(crate) struct Control {
    running: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}
impl Control {
    pub(crate) fn new() -> Result<Self> {
        let running = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (flag, quit) = (running.clone(), shutdown.clone());
        ctrlc::set_handler(move || {
            flag.store(false, Ordering::Relaxed);
            quit.store(true, Ordering::Relaxed);
        })?;
        Ok(Self { running, shutdown })
    }
    pub(crate) fn running(&self) -> &AtomicBool {
        &self.running
    }
    pub(crate) fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
    pub(crate) fn shutdown(&self) {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.store(true, Ordering::Relaxed);
    }
    pub(crate) fn begin_session(&self) -> SessionGuard<'_> {
        self.running.store(true, Ordering::Relaxed);
        SessionGuard(self)
    }
    pub(crate) fn session(
        &self,
        adb: &std::path::Path,
        serial: &str,
        agent: Option<&std::path::Path>,
        sensitivity: f64,
    ) -> Result<()> {
        let mut input = input::Desktop::new(sensitivity)?;
        let _session = self.begin_session();
        let mut connection = adb::Connection::start(adb, serial, agent)?;
        input.run(&mut connection, &self.running)
    }
}
pub(crate) struct SessionGuard<'a>(&'a Control);
impl Drop for SessionGuard<'_> {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Relaxed);
    }
}
