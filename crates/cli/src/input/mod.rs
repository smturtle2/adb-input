// SPDX-License-Identifier: EUPL-1.2
use crate::{adb::Connection, keys};
use adb_input_protocol::{keyboard_report, mouse_reports, Packet};
use anyhow::{bail, Context, Result};
use evdev::{EventSummary, KeyCode, SynchronizationCode};

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    os::fd::AsRawFd,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

mod motion;
mod source;
pub use source::doctor;
use source::{discover, Source};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Local,
    Arming,
    Remote,
    Releasing,
}

pub struct Desktop {
    sources: HashMap<PathBuf, Source>,
    held: HashMap<PathBuf, BTreeSet<u16>>,
    mode: Mode,
    sensitivity: f64,
    notice: Option<String>,
}
impl Desktop {
    pub fn new(sensitivity: f64) -> Result<Self> {
        let sources: HashMap<_, _> = discover().into_iter().collect();
        if !sources.values().any(|s| s.keyboard) || !sources.values().any(|s| s.pointer) {
            bail!(
                "cannot read keyboard and pointer input; run adb-input doctor to check permissions"
            );
        }
        Ok(Self {
            sources,
            held: HashMap::new(),
            mode: Mode::Local,
            sensitivity,
            notice: None,
        })
    }
    fn chord(&self) -> bool {
        chord(self.held.values().flatten().copied())
    }
    fn all_up(&self) -> bool {
        self.held.values().all(BTreeSet::is_empty)
    }
    fn release_local(&mut self) {
        for source in self.sources.values_mut() {
            source.release();
        }
        self.mode = Mode::Local;
        self.held.clear();
    }
    fn activate(&mut self) -> Result<()> {
        for source in self.sources.values_mut() {
            // An upstream remapper may own the physical device; its virtual output is
            // discovered by capabilities through the same path, with no name assumptions.
            source.grabbed = source.device.grab().is_ok();
        }
        if !self.sources.values().any(|s| s.grabbed && s.keyboard)
            || !self.sources.values().any(|s| s.grabbed && s.pointer)
        {
            self.release_local();
            bail!("no capturable keyboard/pointer pair; input is still on the desktop");
        }
        self.mode = Mode::Remote;

        Ok(())
    }
    pub fn run(&mut self, connection: &mut Connection, running: &AtomicBool) -> Result<()> {
        let mut last = None;
        self.run_observed(connection, running, &mut |mode, notice| {
            if last != Some(mode) {
                eprintln!("{mode:?} — Ctrl+Shift+R switches input; Ctrl+C stops.");
                last = Some(mode);
            }
            if let Some(message) = notice {
                eprintln!("{message}");
            }
            Ok(true)
        })
    }
    pub fn run_observed(
        &mut self,
        connection: &mut Connection,
        running: &AtomicBool,
        observer: &mut dyn FnMut(Mode, Option<String>) -> Result<bool>,
    ) -> Result<()> {
        let result = self.event_loop(connection, running, observer);
        // Restore the desktop before any potentially slow ADB shutdown/cleanup.
        self.release_local();
        let _ = connection.release();
        result
    }
    fn event_loop(
        &mut self,
        connection: &mut Connection,
        running: &AtomicBool,
        observer: &mut dyn FnMut(Mode, Option<String>) -> Result<bool>,
    ) -> Result<()> {
        let mut scan = Instant::now();
        let mut heartbeat = Instant::now();
        while running.load(Ordering::Relaxed) {
            if !observer(self.mode, self.notice.take())? {
                break;
            }
            if heartbeat.elapsed() >= Duration::from_secs(1) {
                connection.heartbeat()?;
                heartbeat = Instant::now();
            }
            if scan.elapsed() >= Duration::from_secs(1) {
                for (path, mut source) in discover() {
                    if self.sources.contains_key(&path) {
                        continue;
                    }
                    if self.mode == Mode::Remote {
                        source.grabbed = source.device.grab().is_ok();
                    }
                    self.sources.insert(path, source);
                }
                scan = Instant::now();
            }
            let paths: Vec<_> = self.sources.keys().cloned().collect();
            let mut polls: Vec<_> = paths
                .iter()
                .map(|p| libc::pollfd {
                    fd: self.sources[p].device.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            let n = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 25) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(std::io::Error::last_os_error().into());
            }
            for (path, poll) in paths.iter().zip(polls) {
                if poll.revents == 0 {
                    continue;
                }
                let events = self
                    .sources
                    .get_mut(path)
                    .unwrap()
                    .device
                    .fetch_events()
                    .map(|events| events.collect::<Vec<_>>());
                let events = match events {
                    Ok(events) => events,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(_) => {
                        self.release_local();
                        self.sources.remove(path);
                        connection.release()?;
                        self.notice =
                            Some("Input source disconnected; returned to desktop.".into());
                        continue;
                    }
                };
                for event in events {
                    if matches!(
                        event.destructure(),
                        EventSummary::Synchronization(_, SynchronizationCode::SYN_DROPPED, _)
                    ) {
                        self.release_local();
                        connection.release()?;
                        self.notice = Some("Input events lost; returned to desktop.".into());
                        continue;
                    }
                    match event.destructure() {
                        EventSummary::Key(_, key, value)
                            if key.0 < KeyCode::BTN_0.0 && value != 2 =>
                        {
                            let code = key.0;
                            let held = self.held.entry(path.clone()).or_default();
                            if value != 0 {
                                held.insert(code);
                            } else {
                                held.remove(&code);
                            }
                            if value == 1 && key == KeyCode::KEY_R && self.chord() {
                                match self.mode {
                                    Mode::Local => self.mode = Mode::Arming,
                                    Mode::Remote => {
                                        connection.release()?;
                                        self.mode = Mode::Releasing;
                                    }
                                    _ => {}
                                }
                            }
                            if self.all_up() {
                                if self.mode == Mode::Arming {
                                    if let Err(e) = self.activate() {
                                        self.notice = Some(e.to_string());
                                    }
                                    continue;
                                }
                                if self.mode == Mode::Releasing {
                                    self.release_local();
                                    continue;
                                }
                            }
                            if self.mode == Mode::Remote {
                                let usages = self
                                    .held
                                    .iter()
                                    .filter(|(p, _)| {
                                        self.sources.get(*p).is_some_and(|s| s.grabbed)
                                    })
                                    .flat_map(|(_, keys)| {
                                        keys.iter().filter_map(|k| keys::usage(*k))
                                    });
                                connection.send(Packet::Keyboard(keyboard_report(usages)))?;
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if self.mode != Mode::Remote {
                        continue;
                    }
                    let source = self.sources.get_mut(path).context("input source removed")?;
                    if !source.grabbed || !source.pointer {
                        continue;
                    }
                    if let Some((x, y, wheel)) = source.motion.update(event, self.sensitivity) {
                        let buttons = self
                            .sources
                            .values()
                            .filter(|s| s.grabbed)
                            .fold(0, |buttons, source| buttons | source.motion.buttons);
                        for packet in mouse_reports(buttons, x, y, wheel) {
                            connection.send(packet)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.release_local();
    }
}
fn chord(keys: impl IntoIterator<Item = u16>) -> bool {
    let keys: HashSet<_> = keys.into_iter().collect();
    keys.contains(&KeyCode::KEY_R.0)
        && (keys.contains(&KeyCode::KEY_LEFTCTRL.0) || keys.contains(&KeyCode::KEY_RIGHTCTRL.0))
        && (keys.contains(&KeyCode::KEY_LEFTSHIFT.0) || keys.contains(&KeyCode::KEY_RIGHTSHIFT.0))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toggle_requires_complete_chord_and_accepts_either_side() {
        assert!(chord([19, 29, 42]));
        assert!(chord([19, 97, 54]));
        assert!(!chord([19, 29]));
        assert!(!chord([19, 29, 56])); // Old Ctrl+Alt+R must pass through.
        assert!(!chord([29, 42]));
        assert_eq!(keys::usage(88), Some(69)); // F12 is now forwarded normally.
    }
}
