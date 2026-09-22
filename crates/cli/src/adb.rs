// SPDX-License-Identifier: EUPL-1.2
use adb_input_protocol::{Packet, READY};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn output(adb: &Path, serial: Option<&str>, args: &[&str]) -> Result<String> {
    output_input(adb, serial, args, None)
}
pub fn pair(adb: &Path, address: &str, code: &str) -> Result<()> {
    let response = output_input(adb, None, &["pair", address], Some(&format!("{code}\n")))?;
    if !response.contains("Successfully paired") {
        bail!("Pairing failed. Check the pairing address and code on your phone.");
    }
    Ok(())
}
fn output_input(
    adb: &Path,
    serial: Option<&str>,
    args: &[&str],
    input: Option<&str>,
) -> Result<String> {
    let mut command = Command::new(adb);
    if let Some(s) = serial {
        command.args(["-s", s]);
    }
    let mut child = command
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot execute adb; install Android platform-tools or use --adb")?;
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input.as_bytes())?;
    }
    let stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let _ = BufReader::new(stderr).take(65536).read_to_string(&mut text);
        text
    });
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        BufReader::new(stdout)
            .take(1024 * 1024)
            .read_to_string(&mut text)
            .map(|_| text)
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("adb command timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let text = reader
        .join()
        .map_err(|_| anyhow::anyhow!("adb output reader failed"))??;
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        bail!(
            "adb {} failed: {} {}",
            args.first().unwrap_or(&""),
            text.trim(),
            errors.trim()
        );
    }
    Ok(text)
}
pub fn connect(adb: &Path, endpoint: &str) -> Result<()> {
    let response = output(adb, None, &["connect", endpoint])?;
    if response.to_lowercase().contains("failed") {
        bail!("{}", response.trim());
    }
    // adb sometimes reports a failed connect with exit status zero.
    if output(adb, Some(endpoint), &["get-state"])?.trim() != "device" {
        bail!("device is not connected");
    }
    Ok(())
}
pub fn select(adb: &Path, requested: Option<String>) -> Result<String> {
    if let Some(serial) = requested {
        if output(adb, Some(&serial), &["get-state"])?.trim() != "device" {
            bail!("device is not online");
        }
        return Ok(serial);
    }
    let devices: Vec<_> = devices(adb)?
        .into_iter()
        .filter(|d| d.state == "device")
        .collect();
    if devices.len() != 1 {
        bail!("expected one online ADB device; found {}. Use connect, --connect ADDRESS, or --device SERIAL", devices.len());
    }
    Ok(devices[0].serial.clone())
}

#[derive(Clone)]
pub struct DeviceInfo {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
}
pub fn devices(adb: &Path) -> Result<Vec<DeviceInfo>> {
    Ok(parse_devices(&output(adb, None, &["devices", "-l"])?))
}
fn parse_devices(text: &str) -> Vec<DeviceInfo> {
    text.lines()
        .filter_map(|line| {
            if line.starts_with("List of devices") || line.starts_with('*') {
                return None;
            }
            let mut fields = line.split_whitespace();
            let serial = fields.next()?.to_owned();
            let state = fields.next()?.to_owned();
            let model =
                fields.find_map(|field| field.strip_prefix("model:").map(|m| m.replace('_', " ")));
            Some(DeviceInfo {
                serial,
                state,
                model,
            })
        })
        .collect()
}

pub struct Connection {
    adb: PathBuf,
    serial: String,
    remote: String,
    child: Option<Child>,
    input: Option<ChildStdin>,
}
impl Connection {
    pub fn start(adb: &Path, serial: &str, override_agent: Option<&Path>) -> Result<Self> {
        let abi = output(
            adb,
            Some(serial),
            &["shell", "getprop", "ro.product.cpu.abi"],
        )?;
        let bytes: &[u8] = match abi.trim() {
            "arm64-v8a" => include_bytes!(concat!(env!("OUT_DIR"), "/agent-AARCH64")),
            "x86_64" => include_bytes!(concat!(env!("OUT_DIR"), "/agent-X86_64")),
            other if override_agent.is_none() => bail!(
                "unsupported Android ABI: {other}; release agents support arm64-v8a and x86_64"
            ),
            _ => &[],
        };
        if override_agent.is_none() && bytes.is_empty() {
            bail!("this development build has no embedded agent; run cargo xtask build, or use --agent PATH");
        }
        let id = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        let mut connection = Self {
            adb: adb.into(),
            serial: serial.into(),
            remote: format!("/data/local/tmp/adb-input-{id}"),
            child: None,
            input: None,
        };
        let temporary = std::env::temp_dir().join(format!("adb-input-{id}"));
        let source = if let Some(path) = override_agent {
            path
        } else {
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(bytes)?;
            &temporary
        };
        let pushed = output(
            adb,
            Some(serial),
            &[
                "push",
                source.to_str().context("agent path is not UTF-8")?,
                &connection.remote,
            ],
        );
        if override_agent.is_none() {
            let _ = fs::remove_file(&temporary);
        }
        pushed?;
        output(
            adb,
            Some(serial),
            &["shell", "chmod", "700", &connection.remote],
        )?;
        let mut child = Command::new(adb)
            .args(["-s", serial, "shell", "-T", &connection.remote])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        connection.input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        connection.child = Some(child);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            use std::io::Read;
            let result = BufReader::new(stdout)
                .take(256)
                .read_line(&mut line)
                .map(|_| line);
            let _ = tx.send(result);
        });
        let greeting = rx
            .recv_timeout(Duration::from_secs(8))
            .context("Android agent did not become ready")??;
        if greeting.trim() != READY {
            bail!("unexpected Android agent response: {greeting:?}");
        }
        let fd = connection.input.as_ref().unwrap().as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }

        Ok(connection)
    }
    pub fn send(&mut self, packet: Packet) -> Result<()> {
        if let Some(status) = self.child.as_mut().unwrap().try_wait()? {
            bail!("ADB agent exited: {status}");
        }
        let input = self.input.as_mut().context("input connection closed")?;
        let mut poll = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, 1000) };
        if result <= 0 || poll.revents & libc::POLLOUT == 0 {
            bail!("ADB input transport stopped responding");
        }
        let bytes = packet.encode();
        if input.write(&bytes)? != bytes.len() {
            bail!("short ADB packet write");
        }
        Ok(())
    }
    pub fn heartbeat(&mut self) -> Result<()> {
        self.send(Packet::Heartbeat)
    }
    pub fn release(&mut self) -> Result<()> {
        self.send(Packet::Keyboard([0; 8]))?;
        self.send(Packet::Mouse([0; 6]))
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        if self.child.is_some() && self.input.is_some() {
            let _ = self.release();
            let _ = self.send(Packet::Stop);
        }
        self.input.take();
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Err(e) = output(
            &self.adb,
            Some(&self.serial),
            &["shell", "rm", "-f", &self.remote],
        ) {
            eprintln!("Agent cleanup pending ({}): {e}", self.remote);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_listing_preserves_connection_states() {
        let list = parse_devices("List of devices attached\nphone:5555 device model:Galaxy_S25 transport_id:1\nusb unauthorized\nold offline\n\n");
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].model.as_deref(), Some("Galaxy S25"));
        assert_eq!(list[1].state, "unauthorized");
        assert_eq!(list[2].state, "offline");
    }
}
