// SPDX-License-Identifier: EUPL-1.2
//! Original screen layout and interaction flow; no third-party UI implementation.
use crate::{
    adb,
    control::Control,
    input,
    terminal::{Editor, Key, Line, Screen, Style, Terminal},
};
use anyhow::{bail, Result};
use std::{
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};
const TICK: Duration = Duration::from_millis(60);
fn line(text: impl Into<String>, style: Style) -> Line {
    Line::new(text, style)
}
fn caption(device: &adb::DeviceInfo) -> String {
    device
        .model
        .clone()
        .unwrap_or_else(|| device.serial.clone())
}
fn screen(title: &str, message: &str, footer: &str) -> Screen {
    Screen {
        lines: vec![
            line(title, Style::Strong),
            line("", Style::Normal),
            line(message, Style::Muted),
        ],
        footer: footer.into(),
    }
}
fn task<T: Send + 'static>(
    terminal: &mut Terminal,
    control: &Control,
    title: &str,
    job: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(job());
    });
    let start = Instant::now();
    loop {
        let glyph = ["-", "\\", "|", "/"][(start.elapsed().as_millis() / 120) as usize % 4];
        terminal.draw(screen(title, &format!("{glyph} Working..."), "Please wait"))?;
        if terminal.key(TICK)? == Some(Key::Interrupt) {
            control.shutdown();
        }
        match rx.try_recv() {
            Ok(result) => {
                if control.is_shutdown() {
                    bail!("Cancelled");
                }
                return result;
            }
            Err(mpsc::TryRecvError::Disconnected) => bail!("Background operation stopped"),
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}
fn refresh(terminal: &mut Terminal, control: &Control, adb: &Path) -> Result<Vec<adb::DeviceInfo>> {
    let path = adb.to_owned();
    task(terminal, control, "Finding devices", move || {
        adb::devices(&path)
    })
}
fn menu(
    terminal: &mut Terminal,
    title: &str,
    device: Option<&adb::DeviceInfo>,
    items: &[String],
    selected: usize,
    notice: &str,
    notice_error: bool,
) -> Result<()> {
    let rows = terminal.size().1.saturating_sub(6);
    let mut lines = vec![line(title, Style::Muted)];
    if rows >= 9 {
        if let Some(device) = device {
            lines.push(line(
                format!(
                    "{} {}",
                    if device.state == "device" { "*" } else { "!" },
                    caption(device)
                ),
                Style::Strong,
            ));
            lines.push(line(
                format!(
                    "  {}  /  {}",
                    device.serial,
                    if device.state == "device" {
                        "Connected"
                    } else {
                        &device.state
                    }
                ),
                Style::Muted,
            ));
        } else {
            lines.push(line("No device connected", Style::Muted));
        }
        lines.push(line("", Style::Normal));
    }
    let reserve = if !notice.is_empty() && rows >= 8 {
        2
    } else {
        0
    };
    let count = rows.saturating_sub(lines.len() + reserve).max(1);
    let start = selected.saturating_sub(count.saturating_sub(1));
    for (index, item) in items.iter().enumerate().skip(start).take(count) {
        lines.push(line(
            format!("{} {item}", if index == selected { ">" } else { " " }),
            if index == selected {
                Style::Accent
            } else {
                Style::Normal
            },
        ));
    }
    if reserve > 0 {
        lines.push(line("", Style::Normal));
        lines.push(line(
            notice,
            if notice_error {
                Style::Error
            } else {
                Style::Muted
            },
        ));
    }
    terminal.draw(Screen {
        lines,
        footer: if terminal.size().0 < 50 {
            "Up/Down  Enter  Esc"
        } else {
            "Up/Down Select   Enter Continue   Esc Back"
        }
        .into(),
    })?;
    Ok(())
}
fn select(
    terminal: &mut Terminal,
    control: &Control,
    title: &str,
    items: &[String],
) -> Result<Option<usize>> {
    let mut selected = 0;
    loop {
        if control.is_shutdown() {
            return Ok(None);
        }
        menu(terminal, title, None, items, selected, "", false)?;
        match terminal.key(TICK)? {
            Some(Key::Up | Key::Char('k')) => selected = selected.saturating_sub(1),
            Some(Key::Down | Key::Char('j')) => {
                selected = (selected + 1).min(items.len().saturating_sub(1))
            }
            Some(Key::Enter) if !items.is_empty() => return Ok(Some(selected)),
            Some(Key::Escape | Key::Interrupt) => return Ok(None),
            _ => {}
        }
    }
}
fn address_valid(value: &str) -> bool {
    value.rsplit_once(':').is_some_and(|(host, port)| {
        !host.is_empty() && port.parse::<u16>().is_ok_and(|port| port > 0)
    })
}
fn form(
    terminal: &mut Terminal,
    control: &Control,
    copy: (&str, &str, &str),
    editor: &mut Editor,
    masked: bool,
    error: &str,
) -> Result<Option<String>> {
    let (title, label, help) = copy;
    let mut validation = error.to_owned();
    loop {
        if control.is_shutdown() {
            return Ok(None);
        }
        let width = terminal.size().0.saturating_sub(6);
        let mut lines = vec![
            line(title, Style::Strong),
            line("", Style::Normal),
            line(label, Style::Muted),
            line(editor.display(masked, width), Style::Accent),
        ];
        if terminal.size().1 < 12 {
            lines = vec![
                line(title, Style::Strong),
                line(editor.display(masked, width), Style::Accent),
            ];
        }
        if terminal.size().1 >= 15 {
            lines.push(line(help, Style::Muted));
            lines.push(line("", Style::Normal));
        }
        if !validation.is_empty() {
            lines.push(line(&validation, Style::Error));
        }
        terminal.draw(Screen {
            lines,
            footer: if terminal.size().0 < 50 {
                "Enter OK   Esc Back"
            } else {
                "Enter Continue   Esc Back   Left/Right Edit"
            }
            .into(),
        })?;
        match terminal.key(TICK)? {
            Some(Key::Escape | Key::Interrupt) => return Ok(None),
            Some(Key::Enter) => {
                let value = editor.value();
                let valid = if masked {
                    value.len() == 6 && value.bytes().all(|b| b.is_ascii_digit())
                } else {
                    address_valid(&value)
                };
                if valid {
                    return Ok(Some(value));
                }
                validation = if masked {
                    "Enter the 6-digit pairing code."
                } else {
                    "Enter an address and port, for example 192.168.1.2:5555."
                }
                .into();
            }
            Some(key) => editor.key(key),
            _ => {}
        }
    }
}
fn connect_form(
    terminal: &mut Terminal,
    control: &Control,
    adb: &Path,
    pairing: bool,
) -> Result<Option<String>> {
    let mut address = Editor::default();
    let mut code = Editor::default();
    let mut error = String::new();
    loop {
        let Some(endpoint) = form(
            terminal,
            control,
            (
                if pairing {
                    "Pair wireless debugging"
                } else {
                    "Connect a device"
                },
                "Address",
                if pairing {
                    "Use the pairing address shown on your phone."
                } else {
                    "Use the connection port, not the pairing port."
                },
            ),
            &mut address,
            false,
            &error,
        )?
        else {
            return Ok(None);
        };
        let pin = if pairing {
            let Some(pin) = form(
                terminal,
                control,
                (
                    "Pair wireless debugging",
                    "Pairing code",
                    "Enter the code displayed on your phone.",
                ),
                &mut code,
                true,
                "",
            )?
            else {
                continue;
            };
            Some(pin)
        } else {
            None
        };
        let path = adb.to_owned();
        let target = endpoint.clone();
        let result = task(
            terminal,
            control,
            if pairing {
                "Pairing device"
            } else {
                "Connecting device"
            },
            move || {
                if let Some(code) = pin {
                    adb::pair(&path, &target, &code)
                } else {
                    adb::connect(&path, &target)
                }
            },
        );
        match result {
            Ok(()) => return Ok(Some(endpoint)),
            Err(e) => error = format!("{e:#}"),
        }
        if control.is_shutdown() {
            return Ok(None);
        }
    }
}
fn session(
    terminal: &mut Terminal,
    control: &Control,
    adb: &Path,
    device: &adb::DeviceInfo,
) -> Result<()> {
    let mut desktop = input::Desktop::new(1.0)?;
    let path = adb.to_owned();
    let serial = device.serial.clone();
    let mut connection = task(terminal, control, "Preparing input", move || {
        adb::Connection::start(&path, &serial, None)
    })?;
    let _session = control.begin_session();
    let mut notice = String::new();
    desktop.run_observed(&mut connection, control.running(), &mut |mode, message| {
        if let Some(message) = message {
            notice = message;
        }
        let (target, description) = match mode {
            input::Mode::Local => ("DESKTOP", "Your keyboard and mouse control this desktop."),
            input::Mode::Remote => ("PHONE", "Your keyboard and mouse control your phone."),
            _ => (
                "SWITCHING",
                "Release the shortcut keys to finish switching.",
            ),
        };
        let mut lines = vec![
            line(format!("* {} / Connected", caption(device)), Style::Muted),
            line("", Style::Normal),
            line(target, Style::Accent),
            line(description, Style::Normal),
        ];
        if !notice.is_empty() {
            lines.push(line("", Style::Normal));
            lines.push(line(&notice, Style::Error));
        }
        terminal.draw(Screen {
            lines,
            footer: "Ctrl+Shift+R Switch   Ctrl+C Stop (desktop)".into(),
        })?;
        let key = terminal.key(Duration::ZERO)?;
        Ok(!(mode == input::Mode::Local && matches!(key, Some(Key::Escape | Key::Interrupt))))
    })
}

pub fn run(adb: &Path, control: &Control) -> Result<()> {
    let mut terminal = Terminal::enter()?;
    let mut notice = String::new();
    let mut notice_error = false;
    let mut devices = match refresh(&mut terminal, control, adb) {
        Ok(devices) => devices,
        Err(e) => {
            notice = format!("{e:#}");
            notice_error = true;
            Vec::new()
        }
    };
    let mut chosen = devices.iter().find(|d| d.state == "device").cloned();
    let mut selected = 0;
    loop {
        if control.is_shutdown() {
            return Ok(());
        }
        let reconnect = chosen.as_ref().is_some_and(|d| d.state != "device");
        let items: Vec<String> = [
            if reconnect {
                "Reconnect"
            } else {
                "Start control"
            },
            "Change device",
            "Connect another device",
            "Pair wireless debugging",
            "Refresh devices",
            "Exit",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        menu(
            &mut terminal,
            "DEVICE",
            chosen.as_ref(),
            &items,
            selected,
            &notice,
            notice_error,
        )?;
        match terminal.key(TICK)? {
            Some(Key::Up | Key::Char('k')) => {
                selected = selected.saturating_sub(1);
                continue;
            }
            Some(Key::Down | Key::Char('j')) => {
                selected = (selected + 1).min(items.len() - 1);
                continue;
            }
            Some(Key::Escape | Key::Interrupt) => return Ok(()),
            Some(Key::Enter) => {}
            _ => continue,
        }
        notice_error = false;
        let result = (|| -> Result<()> {
            match selected {
                0 => {
                    if let Some(device) = chosen.as_mut() {
                        if reconnect {
                            if !address_valid(&device.serial) {
                                bail!("Reconnect the USB cable and approve debugging, then refresh devices.");
                            }
                            let path = adb.to_owned();
                            let serial = device.serial.clone();
                            task(&mut terminal, control, "Reconnecting", move || {
                                adb::connect(&path, &serial)
                            })?;
                            device.state = "device".into();
                        }
                        match session(&mut terminal, control, adb, device) {
                            Ok(()) => notice = "Input stopped. Desktop controls restored.".into(),
                            Err(e) => {
                                // Preserve selection for retry, but only offer reconnect when ADB is offline.
                                if !adb::devices(adb)
                                    .unwrap_or_default()
                                    .iter()
                                    .any(|d| d.serial == device.serial && d.state == "device")
                                {
                                    device.state = "offline".into();
                                }
                                return Err(e);
                            }
                        }
                    } else {
                        notice = "Connect or select an online device first.".into();
                        selected = 2;
                    }
                }
                1 => {
                    devices = refresh(&mut terminal, control, adb)?;
                    let labels: Vec<_> = devices
                        .iter()
                        .map(|d| format!("{} / {} / {}", caption(d), d.serial, d.state))
                        .collect();
                    if labels.is_empty() {
                        notice = "No devices found. Connect or pair a device.".into();
                    } else if let Some(index) =
                        select(&mut terminal, control, "Choose device", &labels)?
                    {
                        chosen = Some(devices[index].clone());
                        selected = 0;
                        notice.clear();
                    }
                }
                2 | 3 => {
                    if let Some(endpoint) =
                        connect_form(&mut terminal, control, adb, selected == 3)?
                    {
                        if selected == 3 {
                            notice = "Paired. Connect using the phone's connection port.".into();
                            selected = 2;
                        } else {
                            devices = refresh(&mut terminal, control, adb)?;
                            chosen = devices
                                .iter()
                                .find(|d| d.serial == endpoint)
                                .cloned()
                                .or_else(|| devices.iter().find(|d| d.state == "device").cloned());
                            selected = 0;
                            notice.clear();
                        }
                    }
                }
                4 => {
                    devices = refresh(&mut terminal, control, adb)?;
                    chosen = chosen
                        .take()
                        .map(|mut previous| {
                            if let Some(current) =
                                devices.iter().find(|d| d.serial == previous.serial)
                            {
                                current.clone()
                            } else {
                                previous.state = "offline".into();
                                previous
                            }
                        })
                        .or_else(|| devices.iter().find(|d| d.state == "device").cloned());
                    notice = format!("{} device(s) found.", devices.len());
                }
                5 => control.shutdown(),
                _ => {}
            }
            Ok(())
        })();
        if let Err(error) = result {
            notice = format!("{error:#}");
            notice_error = true;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn address_validation_handles_ipv4_ipv6_and_bad_ports() {
        assert!(address_valid("host.example:5555"));
        assert!(address_valid("[::1]:12345"));
        for value in ["", ":5555", "host:0", "host:99999", "host"] {
            assert!(!address_valid(value));
        }
    }
}
