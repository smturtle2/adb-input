// SPDX-License-Identifier: EUPL-1.2
//! Input-device discovery and local grab ownership.
use super::motion::Motion;
use anyhow::{bail, Result};
use evdev::{AbsoluteAxisCode, Device, KeyCode, RelativeAxisCode};
use std::path::PathBuf;

pub(super) struct Source {
    pub(super) device: Device,
    pub(super) keyboard: bool,
    pub(super) pointer: bool,
    pub(super) grabbed: bool,
    pub(super) motion: Motion,
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
                    device,
                    keyboard,
                    pointer,
                    grabbed: false,
                    motion,
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
