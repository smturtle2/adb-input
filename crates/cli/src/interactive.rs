// SPDX-License-Identifier: EUPL-1.2
//! Original screen layout and interaction flow; no third-party UI implementation.
use crate::{
    adb,
    control::Control,
    devices::{Endpoint, SavedDevice, SavedDevices},
    input,
    terminal::{Key, Line, Screen, Style, Terminal},
};
use anyhow::{bail, Result};
use std::{
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};
mod connections;
use connections::{connect_form, connect_saved, manage_saved};

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
            footer: "Ctrl+; Switch   Ctrl+C Stop (desktop)".into(),
        })?;
        let key = terminal.key(Duration::ZERO)?;
        Ok(!(mode == input::Mode::Local && matches!(key, Some(Key::Escape | Key::Interrupt))))
    })
}

fn saved_info(saved: &SavedDevice) -> adb::DeviceInfo {
    adb::DeviceInfo {
        serial: saved.address().unwrap_or_else(|| saved.host.clone()),
        state: "saved".into(),
        model: Some(saved.name().into()),
    }
}

fn device_host(device: &adb::DeviceInfo) -> Option<String> {
    Endpoint::parse(&device.serial)
        .map(|endpoint| endpoint.host)
        .or_else(|| (device.state == "saved").then(|| device.serial.clone()))
}

fn choices(online: Vec<adb::DeviceInfo>, saved: &SavedDevices) -> Vec<adb::DeviceInfo> {
    let mut result: Vec<_> = online
        .into_iter()
        .filter(|device| {
            device.state == "device"
                || device_host(device).is_none_or(|host| saved.find(&host).is_none())
        })
        .collect();
    for device in &mut result {
        if let Some(entry) = device_host(device).and_then(|host| saved.find(&host)) {
            if let Some(label) = &entry.label {
                device.model = Some(label.clone());
            }
        }
    }
    for entry in &saved.entries {
        if !result.iter().any(|device| {
            device.state == "device" && device_host(device).as_deref() == Some(&entry.host)
        }) {
            result.push(saved_info(entry));
        }
    }
    result.sort_by_key(|device| {
        let recent = device_host(device)
            .and_then(|host| saved.entries.iter().position(|entry| entry.host == host))
            .unwrap_or(usize::MAX);
        (device.state != "device", recent)
    });
    result
}

fn remember_device(saved: &mut SavedDevices, device: &adb::DeviceInfo) {
    if let Some(endpoint) = Endpoint::parse(&device.serial) {
        saved.remember(endpoint.host, Some(endpoint.port), device.model.clone());
    }
}

fn saved_notice(saved: &SavedDevices, message: &str) -> String {
    match &saved.warning {
        Some(warning) => format!("{message} {warning}"),
        None => message.into(),
    }
}

fn reconnect_entry(device: &adb::DeviceInfo, saved: &SavedDevices) -> Option<SavedDevice> {
    if let Some(entry) = device_host(device).and_then(|host| saved.find(&host)) {
        return Some(entry.clone());
    }
    let endpoint = Endpoint::parse(&device.serial)?;
    Some(SavedDevice {
        host: endpoint.host,
        port: Some(endpoint.port),
        label: None,
        model: device.model.clone(),
    })
}

fn preferred(devices: &[adb::DeviceInfo]) -> Option<adb::DeviceInfo> {
    devices
        .iter()
        .find(|d| d.state == "device")
        .or_else(|| devices.iter().find(|d| d.state == "saved"))
        .or_else(|| devices.first())
        .cloned()
}

fn retain_choice(
    previous: Option<adb::DeviceInfo>,
    devices: &[adb::DeviceInfo],
) -> Option<adb::DeviceInfo> {
    previous
        .and_then(|previous| {
            devices
                .iter()
                .find(|device| device.serial == previous.serial)
                .or_else(|| {
                    device_host(&previous).and_then(|host| {
                        devices
                            .iter()
                            .find(|device| device_host(device).as_deref() == Some(&host))
                    })
                })
                .cloned()
        })
        .or_else(|| preferred(devices))
}

pub fn run(adb: &Path, control: &Control) -> Result<()> {
    let mut terminal = Terminal::enter()?;
    let mut saved = SavedDevices::open();
    let mut notice = saved.warning.clone().unwrap_or_default();
    let mut notice_error = saved.warning.is_some();
    let mut devices = choices(
        match refresh(&mut terminal, control, adb) {
            Ok(devices) => devices,
            Err(error) => {
                notice = format!("{error:#}");
                notice_error = true;
                Vec::new()
            }
        },
        &saved,
    );
    let mut chosen = preferred(&devices);
    let mut selected = 0;
    loop {
        if control.is_shutdown() {
            return Ok(());
        }
        let reconnect = chosen.as_ref().is_some_and(|d| d.state != "device");
        let items = [
            if reconnect {
                "Reconnect"
            } else {
                "Start control"
            },
            "Choose device",
            "Connect another device",
            "Pair wireless debugging",
            "Refresh devices",
            "Manage saved devices",
            "Exit",
        ]
        .map(String::from);
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
                    if reconnect {
                        let entry = chosen.as_ref().and_then(|device| reconnect_entry(device, &saved))
                            .ok_or_else(|| anyhow::anyhow!("Reconnect the USB cable and approve debugging, then refresh devices."))?;
                        if let Some(device) =
                            connect_saved(&mut terminal, control, adb, &entry, &mut saved)?
                        {
                            chosen = Some(device);
                            notice =
                                saved_notice(&saved, "Connected. Select Start control when ready.");
                        }
                        return Ok(());
                    }
                    if let Some(device) = chosen.as_mut() {
                        remember_device(&mut saved, device);
                        match session(&mut terminal, control, adb, device) {
                            Ok(()) => {
                                notice = saved_notice(
                                    &saved,
                                    "Input stopped. Desktop controls restored.",
                                )
                            }
                            Err(error) => {
                                if !adb::devices(adb)
                                    .unwrap_or_default()
                                    .iter()
                                    .any(|d| d.serial == device.serial && d.state == "device")
                                {
                                    device.state = "offline".into();
                                }
                                return Err(error);
                            }
                        }
                    } else {
                        notice = "Connect or select a saved device first.".into();
                        selected = 2;
                    }
                }
                1 => {
                    devices = choices(refresh(&mut terminal, control, adb)?, &saved);
                    let labels: Vec<_> = devices
                        .iter()
                        .map(|d| format!("{} / {} / {}", caption(d), d.serial, d.state))
                        .collect();
                    if labels.is_empty() {
                        notice = "No devices found. Connect or pair a device.".into();
                    } else if let Some(index) =
                        select(&mut terminal, control, "Choose device", &labels)?
                    {
                        let device = devices[index].clone();
                        if device.state != "device" {
                            if let Some(entry) = reconnect_entry(&device, &saved) {
                                chosen = Some(device);
                                if let Some(connected) =
                                    connect_saved(&mut terminal, control, adb, &entry, &mut saved)?
                                {
                                    chosen = Some(connected);
                                    notice = saved_notice(
                                        &saved,
                                        "Connected. Select Start control when ready.",
                                    );
                                }
                            } else {
                                chosen = Some(device);
                            }
                        } else {
                            remember_device(&mut saved, &device);
                            chosen = Some(device);
                            notice = saved_notice(&saved, "Device selected.");
                        }
                        selected = 0;
                    }
                }
                2 | 3 => {
                    let pairing = selected == 3;
                    if let Some(address) = connect_form(&mut terminal, control, adb, pairing)? {
                        let endpoint = Endpoint::parse(&address).expect("validated endpoint");
                        if pairing {
                            saved.remember(endpoint.host.clone(), None, None);
                            let entry = saved.find(&endpoint.host).expect("just saved").clone();
                            chosen = Some(saved_info(&entry));
                            notice = saved_notice(
                                &saved,
                                "Paired. Enter the connection port when ready.",
                            );
                            if let Some(device) =
                                connect_saved(&mut terminal, control, adb, &entry, &mut saved)?
                            {
                                chosen = Some(device);
                                notice = saved_notice(
                                    &saved,
                                    "Connected. Select Start control when ready.",
                                );
                            }
                        } else {
                            let path = adb.to_owned();
                            let device =
                                task(&mut terminal, control, "Checking device", move || {
                                    Ok(adb::connection_info(&path, &address))
                                })?;
                            remember_device(&mut saved, &device);
                            chosen = Some(device);
                            notice =
                                saved_notice(&saved, "Connected. Select Start control when ready.");
                        }
                        selected = 0;
                    }
                }
                4 => {
                    devices = choices(refresh(&mut terminal, control, adb)?, &saved);
                    chosen = retain_choice(chosen.take(), &devices);
                    notice = saved_notice(
                        &saved,
                        &format!(
                            "{} device(s) found, including saved devices.",
                            devices.len()
                        ),
                    );
                }
                5 => {
                    notice = manage_saved(&mut terminal, control, &mut saved)?;
                    devices = choices(refresh(&mut terminal, control, adb)?, &saved);
                    chosen = retain_choice(chosen.take(), &devices);
                }
                6 => control.shutdown(),
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
