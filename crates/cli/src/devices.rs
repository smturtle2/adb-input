// SPDX-License-Identifier: EUPL-1.2
//! Saved wireless destinations. Pairing credentials remain owned by ADB.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::IpAddr,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::PathBuf,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

pub fn host(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with('[') {
        return value
            .strip_prefix('[')?
            .strip_suffix(']')?
            .parse::<std::net::Ipv6Addr>()
            .ok()
            .map(|address| address.to_string());
    }
    if let Ok(address) = value.parse::<IpAddr>() {
        return Some(address.to_string());
    }
    if value.len() > 253 || value.is_empty() {
        return None;
    }
    let name = value.strip_suffix('.').unwrap_or(value);
    name.split('.')
        .all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        .then(|| name.to_ascii_lowercase())
}

pub fn port(value: &str) -> Option<u16> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<u16>().ok().filter(|port| *port != 0)
}

impl Endpoint {
    pub fn parse(value: &str) -> Option<Self> {
        let (address, number) = value.trim().rsplit_once(':')?;
        // IPv6 endpoints require brackets, unlike the standalone host field.
        if address.contains(':') && !(address.starts_with('[') && address.ends_with(']')) {
            return None;
        }
        Some(Self {
            host: host(address)?,
            port: port(number)?,
        })
    }

    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedDevice {
    pub host: String,
    pub port: Option<u16>,
    pub label: Option<String>,
    pub model: Option<String>,
}

impl SavedDevice {
    pub fn name(&self) -> &str {
        self.label
            .as_deref()
            .or(self.model.as_deref())
            .unwrap_or(&self.host)
    }

    pub fn address(&self) -> Option<String> {
        Some(
            Endpoint {
                host: self.host.clone(),
                port: self.port?,
            }
            .address(),
        )
    }
}

pub struct SavedDevices {
    path: Option<PathBuf>,
    pub entries: Vec<SavedDevice>,
    pub warning: Option<String>,
}

impl SavedDevices {
    pub fn open() -> Self {
        let path = std::env::var_os("XDG_STATE_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|home| home.is_absolute())
                    .map(|home| home.join(".local/state"))
            })
            .map(|root| root.join("adb-input/devices.json"));
        match path {
            Some(path) => Self::load(path),
            None => Self {
                path: None,
                entries: Vec::new(),
                warning: Some(
                    "Saved devices unavailable: HOME and XDG_STATE_HOME are unset.".into(),
                ),
            },
        }
    }

    fn load(path: PathBuf) -> Self {
        let result = match fs::read_to_string(&path) {
            Ok(text) => Self::decode(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        };
        match result {
            Ok(entries) => Self { path: Some(path), entries, warning: None },
            Err(error) => Self {
                path: None,
                entries: Vec::new(),
                warning: Some(format!("Cannot read saved devices at {}: {error}. Existing file preserved; changes are temporary.", path.display())),
            },
        }
    }

    fn decode(text: &str) -> Result<Vec<SavedDevice>> {
        let value: Value = serde_json::from_str(text)?;
        if value["version"].as_u64() != Some(1) {
            bail!("unsupported saved-device format");
        }
        let rows = value["devices"].as_array().context("missing device list")?;
        let mut entries = Vec::<SavedDevice>::new();
        for row in rows {
            let address =
                host(row["host"].as_str().context("missing host")?).context("invalid host")?;
            if entries.iter().any(|entry| entry.host == address) {
                bail!("duplicate saved host");
            }
            let port = if row["port"].is_null() {
                None
            } else {
                Some(
                    row["port"]
                        .as_u64()
                        .and_then(|p| u16::try_from(p).ok())
                        .filter(|p| *p > 0)
                        .context("invalid port")?,
                )
            };
            let optional_name = |key: &str| -> Result<Option<String>> {
                if row[key].is_null() {
                    return Ok(None);
                }
                let value = row[key].as_str().context("invalid device name")?.trim();
                if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                    bail!("invalid device name");
                }
                Ok(Some(value.to_owned()))
            };
            entries.push(SavedDevice {
                host: address,
                port,
                label: optional_name("label")?,
                model: optional_name("model")?,
            });
        }
        Ok(entries)
    }

    pub fn find(&self, address: &str) -> Option<&SavedDevice> {
        self.entries.iter().find(|entry| entry.host == address)
    }

    pub fn remember(&mut self, address: String, port: Option<u16>, model: Option<String>) {
        self.modify(move |entries| {
            let mut entry = entries
                .iter()
                .position(|entry| entry.host == address)
                .map(|index| entries.remove(index))
                .unwrap_or(SavedDevice {
                    host: address,
                    port: None,
                    label: None,
                    model: None,
                });
            if port.is_some() {
                entry.port = port;
            }
            if model.is_some() {
                entry.model = model;
            }
            entries.insert(0, entry);
        });
    }

    pub fn rename(&mut self, address: &str, label: String) {
        self.modify(|entries| {
            if let Some(entry) = entries.iter_mut().find(|entry| entry.host == address) {
                entry.label = Some(label);
            }
        });
    }

    pub fn change_host(&mut self, old: &str, new: String) -> Result<()> {
        self.modify(|entries| {
            if old != new && entries.iter().any(|entry| entry.host == new) {
                bail!("That host already has a saved entry.");
            }
            let entry = entries
                .iter_mut()
                .find(|entry| entry.host == old)
                .context("saved device no longer exists")?;
            entry.host = new;
            Ok(())
        })
    }

    pub fn remove(&mut self, address: &str) {
        self.modify(|entries| entries.retain(|entry| entry.host != address));
    }

    fn modify<T>(&mut self, change: impl FnOnce(&mut Vec<SavedDevice>) -> T) -> T {
        // Load the latest records while locked: a CLI connection must not erase
        // edits made by an already-open TUI, or vice versa.
        if self.path.is_none() {
            return change(&mut self.entries);
        }
        let transaction = self.locked_entries();
        let lock = match transaction {
            Ok((lock, entries)) => {
                self.entries = entries;
                Some(lock)
            }
            Err(error) => {
                self.warning = Some(format!(
                    "Could not update saved devices: {error:#}. Changes are temporary."
                ));
                None
            }
        };
        let result = change(&mut self.entries);
        if lock.is_some() {
            self.warning = self
                .write()
                .err()
                .map(|error| format!("Could not save devices: {error:#}. Changes are temporary."));
        }
        drop(lock);
        result
    }

    fn locked_entries(&self) -> Result<(fs::File, Vec<SavedDevice>)> {
        let path = self.path.as_ref().context("saved devices unavailable")?;
        fs::create_dir_all(path.parent().context("invalid saved-device path")?)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_extension("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let entries = match fs::read_to_string(path) {
            Ok(text) => Self::decode(&text)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        Ok((lock, entries))
    }

    fn write(&self) -> Result<()> {
        let path = self.path.as_ref().context("saved devices unavailable")?;
        let parent = path.parent().context("invalid saved-device path")?;
        fs::create_dir_all(parent)?;
        let rows: Vec<_> = self.entries.iter().map(|entry| json!({
            "host": entry.host, "port": entry.port, "label": entry.label, "model": entry.model,
        })).collect();
        let data = serde_json::to_vec_pretty(&json!({ "version": 1, "devices": rows }))?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let temp = parent.join(format!(".devices-{}-{nonce}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        let result = (|| -> Result<()> {
            file.write_all(&data)?;
            file.sync_all()?;
            fs::rename(&temp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_roundtrip_without_confusing_ipv6_or_pairing_ports() {
        for address in ["phone.example:5555", "[::1]:12345", "192.168.1.2:65535"] {
            assert_eq!(Endpoint::parse(address).unwrap().address(), address);
        }
        for bad in [
            "",
            ":5555",
            "host:0",
            "host:99999",
            "host:+123",
            "host",
            "::1:5555",
            "bad host:5555",
            "[host]:12",
        ] {
            assert!(Endpoint::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn history_survives_restart_and_preserves_names_and_connection_ports() {
        let path =
            std::env::temp_dir().join(format!("adb-input-history-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut saved = SavedDevices::load(path.clone());
        let mut other_session = SavedDevices::load(path.clone());
        saved.remember("phone".into(), Some(5555), Some("Test Phone".into()));
        saved.rename("phone", "My phone".into());
        other_session.remember("other".into(), Some(4444), None);
        saved.remember("phone".into(), Some(6666), None);
        saved.remember("phone".into(), None, None); // pairing must not replace connection port
        let mut loaded = SavedDevices::load(path.clone());
        assert_eq!(loaded.entries, saved.entries);
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].name(), "My phone");
        assert_eq!(loaded.entries[0].port, Some(6666));
        assert!(loaded.change_host("phone", "other".into()).is_err());
        loaded.change_host("phone", "new-host".into()).unwrap();
        loaded.remove("other");
        assert_eq!(SavedDevices::load(path.clone()).entries[0].host, "new-host");
        fs::write(&path, "corrupt").unwrap();
        let mut broken = SavedDevices::load(path.clone());
        broken.remember("phone".into(), None, None);
        assert!(broken.warning.is_some());
        assert_eq!(fs::read_to_string(&path).unwrap(), "corrupt");
        fs::remove_file(path.with_extension("lock")).unwrap();
        fs::remove_file(path).unwrap();
    }
}
