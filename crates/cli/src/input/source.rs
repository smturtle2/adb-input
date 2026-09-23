// SPDX-License-Identifier: EUPL-1.2
//! Input-device discovery and local grab ownership.
use super::{bins::MotionBins, motion::Motion};
use crate::clock::{monotonic_us, realtime_to_monotonic_offset_us};
use anyhow::{bail, Result};
use evdev::{AbsoluteAxisCode, Device, InputEvent, KeyCode, RelativeAxisCode};
use std::{io, os::fd::AsRawFd, path::PathBuf, time::UNIX_EPOCH};

pub(super) struct Source {
    pub(super) id: u32,
    pub(super) device: Device,
    pub(super) keyboard: bool,
    pub(super) pointer: bool,
    pub(super) grabbed: bool,
    pub(super) motion: Motion,
    pub(super) bins: MotionBins,
    event_to_monotonic_offset_us: i128,
}

impl Source {
    pub(super) fn timestamp_us(&self, event: InputEvent) -> io::Result<u64> {
        // evdev presents timeval as SystemTime even when EVIOCSCLOCKID selects
        // CLOCK_MONOTONIC. Map the event's native clock to monotonic once, so
        // queued reports retain their original spacing during a delayed read.
        let elapsed = event
            .timestamp()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative evdev timestamp"))?;
        let raw = i128::try_from(elapsed.as_micros())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "evdev timestamp overflow"))?;
        let mapped = raw.saturating_sub(self.event_to_monotonic_offset_us);
        let mapped = u64::try_from(mapped).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "evdev clock moved backwards")
        })?;
        // A realtime-clock step cannot be allowed to schedule a bin far in
        // the future. Ordinary old reports remain old and are dropped later.
        if mapped > monotonic_us()?.saturating_add(1_000_000) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "evdev clock moved forwards",
            ));
        }
        Ok(mapped)
    }
}

fn set_monotonic_event_clock(device: &Device) -> bool {
    let clock_id = libc::CLOCK_MONOTONIC;
    // Linux input.h: EVIOCSCLOCKID = _IOW('E', 0xa0, int).
    (unsafe {
        libc::ioctl(
            device.as_raw_fd(),
            libc::_IOW::<libc::c_int>(u32::from(b'E'), 0xa0),
            &clock_id,
        )
    }) == 0
}
fn capabilities(d: &Device) -> (bool, bool) {
    let keyboard = d
        .supported_keys()
        .is_some_and(|k| k.contains(KeyCode::KEY_A) && k.contains(KeyCode::KEY_ENTER));
    let rel = d.supported_relative_axes().is_some_and(|a| {
        a.contains(RelativeAxisCode::REL_X) && a.contains(RelativeAxisCode::REL_Y)
    });
    let abs = d.supported_absolute_axes().is_some_and(|a| {
        a.contains(AbsoluteAxisCode::ABS_X) && a.contains(AbsoluteAxisCode::ABS_Y)
    });
    let pointer = (rel || abs)
        && d.supported_keys()
            .is_some_and(|k| k.contains(KeyCode::BTN_LEFT));
    (keyboard, pointer)
}
pub(super) fn discover() -> Vec<(PathBuf, Source)> {
    evdev::enumerate()
        .filter_map(|(path, device)| {
            let (keyboard, pointer) = capabilities(&device);
            if !keyboard && !pointer {
                return None;
            }
            if device.set_nonblocking(true).is_err() {
                return None;
            }
            let event_to_monotonic_offset_us = if set_monotonic_event_clock(&device) {
                0
            } else {
                realtime_to_monotonic_offset_us().ok()?
            };
            let mut motion = Motion::default();
            if let Ok(axes) = device.get_absinfo() {
                for (axis, info) in axes {
                    if matches!(axis, AbsoluteAxisCode::ABS_X | AbsoluteAxisCode::ABS_Y) {
                        motion.ranges[axis.0 as usize] =
                            i64::from(info.maximum()) - i64::from(info.minimum());
                    }
                }
            }
            Some((
                path,
                Source {
                    id: 0,
                    device,
                    keyboard,
                    pointer,
                    grabbed: false,
                    motion,
                    bins: MotionBins::default(),
                    event_to_monotonic_offset_us,
                },
            ))
        })
        .collect()
}
impl Source {
    pub(super) fn release(&mut self) {
        if self.grabbed {
            let _ = self.device.ungrab();
            self.grabbed = false;
        }
        self.motion.reset();
        self.bins.reset();
    }
}

pub fn doctor() -> Result<()> {
    let sources = discover();
    let keyboards = sources.iter().filter(|(_, s)| s.keyboard).count();
    let pointers = sources.iter().filter(|(_, s)| s.pointer).count();
    println!("Readable input sources: {keyboards} keyboards, {pointers} pointers");
    for (path, source) in &sources {
        println!(
            "  {}: {}",
            path.display(),
            source.device.name().unwrap_or("unnamed")
        );
    }
    if keyboards == 0 || pointers == 0 {
        bail!("input access incomplete. Grant this user read access to desktop input devices using your distribution's input-device permissions; see README. No device name or event number needs configuration");
    }
    println!(
        "Linux input backend available (independent of Wayland/X11 and remote-input software)."
    );
    Ok(())
}
