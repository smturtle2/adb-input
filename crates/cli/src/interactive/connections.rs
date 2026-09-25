// SPDX-License-Identifier: EUPL-1.2
//! Pairing, saved-device connection forms and profile management.
use super::{line, remember_device, saved_notice, select, task, TICK};
use crate::{
    adb,
    control::Control,
    devices::{self, Endpoint, SavedDevice, SavedDevices},
    terminal::{Editor, Key, Screen, Style, Terminal},
};
use anyhow::Result;
use std::path::Path;

fn address_valid(value: &str) -> bool {
    Endpoint::parse(value).is_some()
}
#[derive(Clone, Copy)]
enum Field {
    Address,
    PairingCode,
    Name,
    Host,
}
impl Field {
    fn valid(self, value: &str) -> bool {
        match self {
            Self::Address => address_valid(value),
            Self::PairingCode => value.len() == 6 && value.bytes().all(|b| b.is_ascii_digit()),
            Self::Name => !value.is_empty() && value.len() <= 80,
            Self::Host => devices::host(value).is_some(),
        }
    }
    fn error(self) -> &'static str {
        match self {
            Self::Address => "Enter an address and port, for example 192.168.1.2:5555.",
            Self::PairingCode => "Enter the 6-digit pairing code.",
            Self::Name => "Enter a name of 1 to 80 characters.",
            Self::Host => "Enter an IP address or hostname without a port.",
        }
    }
}
fn form(
    terminal: &mut Terminal,
    control: &Control,
    copy: (&str, &str, &str),
    editor: &mut Editor,
    field: Field,
    error: &str,
) -> Result<Option<String>> {
    let (title, label, help) = copy;
    let masked = matches!(field, Field::PairingCode);
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
                let value = editor.value().trim().to_owned();
                if field.valid(&value) {
                    return Ok(Some(value));
                }
                validation = field.error().into();
            }
            Some(key) => editor.key(key),
            _ => {}
        }
    }
}
pub(super) fn connect_form(
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
            Field::Address,
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
                Field::PairingCode,
                "",
            )?
            else {
                continue;
            };
            Some(pin)
        } else {
            None
        };
        let endpoint = Endpoint::parse(&endpoint)
            .expect("validated endpoint")
            .address();
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
pub(super) fn connect_saved(
    terminal: &mut Terminal,
    control: &Control,
    adb: &Path,
    entry: &SavedDevice,
    saved: &mut SavedDevices,
) -> Result<Option<adb::DeviceInfo>> {
    let mut address = Editor::selected(&entry.host);
    let mut number = Editor::selected(&entry.port.map(|p| p.to_string()).unwrap_or_default());
    let mut host_focus = false;
    let mut error = String::new();
    loop {
        if control.is_shutdown() {
            return Ok(None);
        }
        let width = terminal.size().0.saturating_sub(12);
        let mut lines = vec![
            line(format!("Connect {}", entry.name()), Style::Strong),
            line("", Style::Normal),
            line(
                format!(
                    "{} IP/host: {}",
                    if host_focus { ">" } else { " " },
                    if host_focus {
                        address.display(false, width)
                    } else {
                        address.value()
                    }
                ),
                if host_focus {
                    Style::Accent
                } else {
                    Style::Muted
                },
            ),
            line(
                format!(
                    "{} Port: {}",
                    if host_focus { " " } else { ">" },
                    if host_focus {
                        number.value()
                    } else {
                        number.display(false, width)
                    }
                ),
                if host_focus {
                    Style::Muted
                } else {
                    Style::Accent
                },
            ),
        ];
        if terminal.size().1 < 12 {
            lines.remove(1);
        }
        if terminal.size().1 >= 15 {
            lines.push(line(
                "Type to replace [selected text]; Enter reuses the last port.",
                Style::Muted,
            ));
            lines.push(line(
                "Use the connection port, not the pairing port.",
                Style::Muted,
            ));
        }
        if !error.is_empty() {
            lines.push(line(&error, Style::Error));
        }
        terminal.draw(Screen {
            lines,
            footer: if terminal.size().0 < 50 {
                "Tab Field  Enter OK  Esc Back"
            } else {
                "Tab IP/Port   Enter Connect   Esc Back"
            }
            .into(),
        })?;
        match terminal.key(TICK)? {
            Some(Key::Escape | Key::Interrupt) => return Ok(None),
            Some(Key::Tab) => host_focus = !host_focus,
            Some(Key::Up) => host_focus = true,
            Some(Key::Down) => host_focus = false,
            Some(Key::Enter) => {
                let Some(host) = devices::host(&address.value()) else {
                    error = Field::Host.error().into();
                    host_focus = true;
                    continue;
                };
                if host_focus {
                    host_focus = false;
                    continue;
                }
                let Some(port) = devices::port(&number.value()) else {
                    error = "Enter a connection port from 1 to 65535.".into();
                    continue;
                };
                if host != entry.host && saved.find(&host).is_some() {
                    error = "That host already has a saved entry. Select it from the device list."
                        .into();
                    continue;
                }
                let endpoint = Endpoint {
                    host: host.clone(),
                    port,
                }
                .address();
                let path = adb.to_owned();
                let result = task(terminal, control, "Connecting device", move || {
                    adb::connect(&path, &endpoint)?;
                    Ok(adb::connection_info(&path, &endpoint))
                });
                match result {
                    Ok(mut device) => {
                        if host != entry.host && saved.find(&entry.host).is_some() {
                            saved.change_host(&entry.host, host.clone())?;
                        }
                        remember_device(saved, &device);
                        if let Some(label) = saved.find(&host).and_then(|entry| entry.label.clone())
                        {
                            device.model = Some(label);
                        }
                        return Ok(Some(device));
                    }
                    Err(err) => {
                        error = format!("{err:#}");
                        number = Editor::selected(&number.value());
                    }
                }
            }
            Some(key) => {
                if host_focus {
                    address.key(key)
                } else {
                    number.key(key)
                }
            }
            None => {}
        }
    }
}

pub(super) fn manage_saved(
    terminal: &mut Terminal,
    control: &Control,
    saved: &mut SavedDevices,
) -> Result<String> {
    let labels: Vec<_> = saved
        .entries
        .iter()
        .map(|entry| format!("{} / {}", entry.name(), entry.host))
        .collect();
    if labels.is_empty() {
        return Ok("No saved devices yet. Connect or pair a device first.".into());
    }
    let Some(index) = select(terminal, control, "Manage saved devices", &labels)? else {
        return Ok(String::new());
    };
    let entry = saved.entries[index].clone();
    let actions = ["Rename", "Edit IP / hostname", "Remove from saved list"].map(String::from);
    match select(terminal, control, entry.name(), &actions)? {
        Some(0) => {
            let mut name = Editor::selected(entry.name());
            if let Some(name) = form(
                terminal,
                control,
                (
                    "Rename saved device",
                    "Name",
                    "Choose a name for this device.",
                ),
                &mut name,
                Field::Name,
                "",
            )? {
                saved.rename(&entry.host, name);
                return Ok(saved_notice(saved, "Device renamed."));
            }
        }
        Some(1) => {
            let mut address = Editor::selected(&entry.host);
            if let Some(host) = form(
                terminal,
                control,
                (
                    "Edit saved host",
                    "IP / hostname",
                    "The last connection port will be kept.",
                ),
                &mut address,
                Field::Host,
                "",
            )? {
                saved.change_host(&entry.host, devices::host(&host).expect("validated host"))?;
                return Ok(saved_notice(saved, "Host updated."));
            }
        }
        Some(2) => {
            saved.remove(&entry.host);
            return Ok(saved_notice(
                saved,
                "Removed from this list. ADB pairing is unchanged.",
            ));
        }
        _ => {}
    }
    Ok(String::new())
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
