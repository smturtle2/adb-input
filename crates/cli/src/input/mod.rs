// SPDX-License-Identifier: EUPL-1.2
use crate::{adb::Connection, clock::monotonic_us, keys};
use adb_input_protocol::{keyboard_report, Packet};
use anyhow::{bail, Context, Result};
use evdev::{EventSummary, KeyCode, SynchronizationCode};

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    os::fd::AsRawFd,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod bins;
mod motion;
mod source;
use bins::MotionBin;
pub use source::doctor;
use source::{discover, Source};

type DiscoveredSources = Vec<(PathBuf, Source)>;

struct DiscoveryWorker {
    requests: SyncSender<HashSet<PathBuf>>,
    results: Receiver<DiscoveredSources>,
    thread: JoinHandle<()>,
    pending: bool,
}

impl DiscoveryWorker {
    fn start() -> Result<Self> {
        let (requests, request_rx) = mpsc::sync_channel::<HashSet<PathBuf>>(1);
        let (result_tx, results) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("adb-input-discovery".into())
            .spawn(move || {
                while let Ok(known) = request_rx.recv() {
                    let added = discover()
                        .into_iter()
                        .filter(|(path, _)| !known.contains(path))
                        .collect();
                    if result_tx.send(added).is_err() {
                        break;
                    }
                }
            })
            .context("failed to start input discovery worker")?;
        Ok(Self {
            requests,
            results,
            thread,
            pending: false,
        })
    }

    fn request(&mut self, known: HashSet<PathBuf>) -> Result<()> {
        if !self.pending {
            self.requests
                .try_send(known)
                .context("failed to schedule input discovery")?;
            self.pending = true;
        }
        Ok(())
    }

    fn take(&mut self) -> Result<Option<DiscoveredSources>> {
        match self.results.try_recv() {
            Ok(sources) => {
                self.pending = false;
                Ok(Some(sources))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => bail!("input discovery worker stopped"),
        }
    }

    fn stop(self) -> Result<()> {
        drop(self.requests);
        drop(self.results);
        self.thread
            .join()
            .map_err(|_| anyhow::anyhow!("input discovery worker panicked"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Local,
    Arming,
    Remote,
    Releasing,
}

pub struct Desktop {
    sources: HashMap<PathBuf, Source>,
    next_source_id: u32,
    held: HashMap<PathBuf, BTreeSet<u16>>,
    mode: Mode,
    sensitivity: f64,
    last_buttons: u8,
    notice: Option<String>,
}
impl Desktop {
    pub fn new(sensitivity: f64) -> Result<Self> {
        let mut next_source_id = 1u32;
        let sources: HashMap<_, _> = discover()
            .into_iter()
            .map(|(path, mut source)| {
                source.id = next_source_id;
                next_source_id = next_source_id
                    .checked_add(1)
                    .context("too many input sources")?;
                Ok((path, source))
            })
            .collect::<Result<_>>()?;
        if !sources.values().any(|s| s.keyboard) || !sources.values().any(|s| s.pointer) {
            bail!(
                "cannot read keyboard and pointer input; run adb-input doctor to check permissions"
            );
        }
        Ok(Self {
            sources,
            next_source_id,
            held: HashMap::new(),
            mode: Mode::Local,
            sensitivity,
            last_buttons: 0,
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
        self.last_buttons = 0;
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
        self.last_buttons = 0;

        Ok(())
    }
    pub fn run(&mut self, connection: &mut Connection, running: &AtomicBool) -> Result<()> {
        let mut last = None;
        self.run_observed(connection, running, &mut |mode, notice| {
            if last != Some(mode) {
                eprintln!("{mode:?} — Ctrl+; switches input; Ctrl+C stops.");
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
        let mut discovery = match DiscoveryWorker::start() {
            Ok(discovery) => discovery,
            Err(error) => {
                self.release_local();
                let _ = connection.release();
                return Err(error);
            }
        };
        let result = self.event_loop(connection, running, observer, &mut discovery);
        // Restore the desktop before any potentially slow ADB shutdown/cleanup.
        self.release_local();
        let _ = connection.release();
        result.and(discovery.stop())
    }
    fn flush_bins(&mut self, connection: &mut Connection, now_us: u64, force: bool) -> Result<()> {
        if self.mode != Mode::Remote {
            return Ok(());
        }
        let mut ready = Vec::new();
        for source in self.sources.values_mut().filter(|source| source.grabbed) {
            let bin = if force {
                source.bins.take_now(now_us)
            } else {
                source.bins.take_due(now_us)
            };
            if let Some(bin) = bin {
                ready.push((source.id, bin));
            }
        }
        ready.sort_unstable_by_key(|(source_id, bin)| (bin.source_time_us, *source_id));
        for (source_id, bin) in ready {
            send_bin(connection, source_id, bin)?;
        }
        Ok(())
    }

    fn next_poll_timeout_ms(&self, now_us: u64) -> i32 {
        if self.mode != Mode::Remote {
            return 25;
        }
        let wait_us = self
            .sources
            .values()
            .filter(|source| source.grabbed)
            .filter_map(|source| source.bins.next_due_us())
            .map(|due| due.saturating_sub(now_us))
            .min();
        wait_us
            .map(|us| us.div_ceil(1_000).min(25) as i32)
            .unwrap_or(25)
    }

    fn event_loop(
        &mut self,
        connection: &mut Connection,
        running: &AtomicBool,
        observer: &mut dyn FnMut(Mode, Option<String>) -> Result<bool>,
        discovery: &mut DiscoveryWorker,
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
            if let Some(sources) = discovery.take()? {
                for (path, mut source) in sources {
                    if self.sources.contains_key(&path) {
                        continue;
                    }
                    source.id = self.next_source_id;
                    self.next_source_id = self
                        .next_source_id
                        .checked_add(1)
                        .context("too many input sources")?;
                    if self.mode == Mode::Remote {
                        source.grabbed = source.device.grab().is_ok();
                    }
                    self.sources.insert(path, source);
                }
            }
            if scan.elapsed() >= Duration::from_secs(1) {
                discovery.request(self.sources.keys().cloned().collect())?;
                scan = Instant::now();
            }
            let now_us = monotonic_us()?;
            let paths: Vec<_> = self.sources.keys().cloned().collect();
            let mut polls: Vec<_> = paths
                .iter()
                .map(|p| libc::pollfd {
                    fd: self.sources[p].device.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            let timeout_ms = self.next_poll_timeout_ms(now_us);
            let n =
                unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, timeout_ms) };
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
                            if value == 1 && key == KeyCode::KEY_SEMICOLON && self.chord() {
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
                                    match self.activate() {
                                        Ok(()) => connection.arm()?,
                                        Err(error) => self.notice = Some(error.to_string()),
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
                                connection.keyboard(keyboard_report(usages))?;
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
                    if let Some(motion) = source.motion.update(event, self.sensitivity) {
                        let source_id = source.id;
                        let source_time_us = source.timestamp_us(event)?;
                        let now_us = monotonic_us()?;
                        let buttons = self
                            .sources
                            .values()
                            .filter(|s| s.grabbed)
                            .fold(0, |buttons, source| buttons | source.motion.buttons);
                        if buttons != self.last_buttons || motion.wheel != 0 {
                            self.flush_bins(connection, now_us, true)?;
                            self.sources
                                .get_mut(path)
                                .context("input source removed")?
                                .bins
                                .note_discrete(source_time_us);
                            connection.mouse(
                                buttons,
                                motion.x,
                                motion.y,
                                motion.wheel,
                                source_id,
                                source_time_us,
                            )?;
                            self.last_buttons = buttons;
                        } else if motion.x != 0 || motion.y != 0 {
                            let bin = self
                                .sources
                                .get_mut(path)
                                .context("input source removed")?
                                .bins
                                .accept(motion.x, motion.y, source_time_us, now_us);
                            if let Some(bin) = bin {
                                send_bin(connection, source_id, bin)?;
                            }
                        }
                    }
                }
            }
            // Read every ready evdev report before closing a due interval.
            // The kernel may already hold a later report for that same bin.
            self.flush_bins(connection, monotonic_us()?, false)?;
        }
        Ok(())
    }
}
fn send_bin(connection: &mut Connection, source_id: u32, bin: MotionBin) -> Result<()> {
    connection.send(Packet::MotionBin {
        source_id,
        sequence: bin.sequence,
        period_us: bin.period_us,
        source_time_us: bin.source_time_us,
        dx: bin.dx,
        dy: bin.dy,
    })
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.release_local();
    }
}
fn chord(keys: impl IntoIterator<Item = u16>) -> bool {
    let keys: HashSet<_> = keys.into_iter().collect();
    keys.contains(&KeyCode::KEY_SEMICOLON.0)
        && (keys.contains(&KeyCode::KEY_LEFTCTRL.0) || keys.contains(&KeyCode::KEY_RIGHTCTRL.0))
        && !keys.iter().any(|&key| {
            matches!(
                KeyCode(key),
                KeyCode::KEY_LEFTSHIFT
                    | KeyCode::KEY_RIGHTSHIFT
                    | KeyCode::KEY_LEFTALT
                    | KeyCode::KEY_RIGHTALT
                    | KeyCode::KEY_LEFTMETA
                    | KeyCode::KEY_RIGHTMETA
            )
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toggle_requires_complete_chord_and_accepts_either_side() {
        let chord = |keys: &[KeyCode]| super::chord(keys.iter().map(|key| key.0));
        let semicolon = KeyCode::KEY_SEMICOLON;
        for ctrl in [KeyCode::KEY_LEFTCTRL, KeyCode::KEY_RIGHTCTRL] {
            assert!(chord(&[ctrl, semicolon]));
            assert!(!chord(&[ctrl]));
            for modifier in [
                KeyCode::KEY_LEFTSHIFT,
                KeyCode::KEY_RIGHTSHIFT,
                KeyCode::KEY_LEFTALT,
                KeyCode::KEY_RIGHTALT,
                KeyCode::KEY_LEFTMETA,
                KeyCode::KEY_RIGHTMETA,
            ] {
                assert!(!chord(&[ctrl, modifier, semicolon]));
            }
        }
        assert!(!chord(&[semicolon]));
        assert!(!chord(&[
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_R,
        ]));
    }
}
