// SPDX-License-Identifier: EUPL-1.2
//! End-to-end terminal tests use a PTY and a fake ADB, never a phone or input grab.
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::PermissionsExt,
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
const BASIC_ADB: &str = "#!/bin/sh\ncase \"$1\" in\ndevices) printf 'List of devices attached\\nphone:5555 device model:Test_Phone\\n';;\nconnect) echo 'connection refused' >&2; exit 1;;\n*) exit 1;;\nesac\n";
static NEXT_UI: AtomicU64 = AtomicU64::new(0);

struct Ui {
    child: Child,
    master: File,
    slave: File,
    original: libc::termios,
    directory: PathBuf,
    output: String,
    state: PathBuf,
}
impl Ui {
    fn start(no_color: bool) -> Self {
        Self::start_with(no_color, BASIC_ADB, None)
    }
    fn start_with(no_color: bool, fixture: &str, state: Option<PathBuf>) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "adb-input-ui-{}-{}",
            std::process::id(),
            NEXT_UI.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let state = state.unwrap_or_else(|| directory.join("state"));
        let adb = directory.join("adb");
        fs::write(&adb, fixture).unwrap();
        fs::set_permissions(&adb, fs::Permissions::from_mode(0o700)).unwrap();
        let (mut master, mut slave) = (0, 0);
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let mut original = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut original) },
            0
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_adb-input"));
        command
            .args(["--adb", adb.to_str().unwrap()])
            .env("TERM", "xterm-256color")
            .env("XDG_STATE_HOME", &state)
            .env("ADB_INPUT_TEST_DIR", &directory)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        if no_color {
            command.env("NO_COLOR", "1");
        } else {
            command.env_remove("NO_COLOR");
        }
        let child = command.spawn().unwrap();
        Self {
            child,
            master,
            slave,
            original,
            directory,
            output: String::new(),
            state,
        }
    }
    fn until(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.output.contains(needle) {
            assert!(
                Instant::now() < deadline,
                "UI never showed {needle:?}: {:?}",
                self.output
            );
            let mut poll = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 50) } > 0 {
                let mut bytes = [0; 16384];
                let n = self.master.read(&mut bytes).unwrap();
                self.output.push_str(&String::from_utf8_lossy(&bytes[..n]));
            }
        }
    }
    fn send(&mut self, text: &[u8]) {
        self.output.clear();
        self.master.write_all(text).unwrap();
    }
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "UI did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        self.until("\x1b[?1049l");
        let mut actual = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut actual) },
            0
        );
        assert_eq!(actual.c_lflag, self.original.c_lflag);
        assert_eq!(actual.c_iflag, self.original.c_iflag);
        assert_eq!(actual.c_oflag, self.original.c_oflag);
    }
}
impl Drop for Ui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}
#[test]
fn navigate_edit_retry_cancel_and_restore_terminal() {
    let mut ui = Ui::start(false);
    ui.until("Test Phone");
    assert!(ui.output.contains("\x1b[1;36m"));
    ui.send(b"\x1b[B\x1b[B\r");
    ui.until("Connect a device");
    ui.send(b"bad\r");
    ui.until("Enter an address and port");
    ui.send(b"\x01\x1b[3~\x1b[3~\x1b[3~host:5555\r");
    ui.until("connection refused");
    assert!(ui.output.contains("host:5555"));
    ui.send(b"\x1b");
    ui.until("Choose device");
    ui.send(b"\x03");
    ui.finish();
}
#[test]
fn monochrome_resize_and_signal_cleanup() {
    let mut ui = Ui::start(true);
    ui.until("Test Phone");
    assert!(!ui.output.contains("\x1b[1;36m"));
    let size = libc::winsize {
        ws_row: 8,
        ws_col: 25,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    ui.output.clear();
    unsafe {
        libc::ioctl(ui.slave.as_raw_fd(), libc::TIOCSWINSZ, &size);
    }
    ui.until("Resize terminal");
    unsafe {
        libc::kill(ui.child.id() as i32, libc::SIGTERM);
    }
    ui.finish();
}

const SAVED_ADB: &str = r#"#!/bin/sh
set -eu
state="$ADB_INPUT_TEST_DIR/connected"
case "$1" in
  devices)
    printf 'List of devices attached\n'
    if [ -f "$state" ]; then printf '%s device model:Test_Phone\n' "$(cat "$state")"; fi
    ;;
  connect)
    case "$2" in *:5556) echo 'connection refused' >&2; exit 1;; esac
    printf '%s' "$2" > "$state"
    echo "connected to $2"
    ;;
  pair)
    read -r pin
    test "$pin" = 123456
    echo "Successfully paired to $2"
    ;;
  -s)
    test "$3" = get-state
    test "$(cat "$state")" = "$2"
    echo device
    ;;
  *) exit 1;;
esac
"#;

fn saved_rows(ui: &Ui) -> serde_json::Value {
    serde_json::from_slice(&fs::read(ui.state.join("adb-input/devices.json")).unwrap()).unwrap()
}

#[test]
fn saved_device_reconnects_by_port_after_restart_and_can_be_managed() {
    let mut first = Ui::start_with(false, SAVED_ADB, None);
    first.until("No device connected");
    first.send(b"\x1b[B\x1b[B\r");
    first.until("Connect a device");
    first.send(b"10.0.0.2:5555\r");
    first.until("Connected. Select Start control");
    assert_eq!(saved_rows(&first)["devices"][0]["port"], 5555);
    first.send(b"\x1b[B\x1b[B\x1b[B\x1b[B\x1b[B\r");
    first.until("Manage saved devices");
    first.send(b"\r");
    first.until("Remove from saved list");
    first.send(b"\r");
    first.until("Rename saved device");
    first.send(b"My Phone\r");
    first.until("Device renamed.");
    first.send(b"\x03");
    first.finish();

    let mut second = Ui::start_with(false, SAVED_ADB, Some(first.state.clone()));
    second.until("My Phone");
    second.send(b"\r");
    second.until("[5555]");
    second.send(b"5556\r");
    second.until("connection refused");
    assert_eq!(saved_rows(&second)["devices"][0]["port"], 5555);
    second.send(b"\x1b");
    second.until("Reconnect");
    second.send(b"\r");
    second.until("[5555]");
    second.send(b"6666\r");
    second.until("Connected. Select Start control");
    let rows = saved_rows(&second);
    assert_eq!(rows["devices"].as_array().unwrap().len(), 1);
    assert_eq!(rows["devices"][0]["port"], 6666);
    assert_eq!(rows["devices"][0]["label"], "My Phone");
    second.send(b"\x03");
    second.finish();

    let mut third = Ui::start_with(false, SAVED_ADB, Some(first.state.clone()));
    third.until("My Phone");
    third.send(b"\r");
    third.until("[6666]");
    third.send(b"\t[::1]\r7777\r");
    third.until("Connected. Select Start control");
    let rows = saved_rows(&third);
    assert_eq!(rows["devices"].as_array().unwrap().len(), 1);
    assert_eq!(rows["devices"][0]["host"], "::1");
    assert_eq!(rows["devices"][0]["port"], 7777);
    third.send(b"\x1b[B\x1b[B\x1b[B\x1b[B\x1b[B\r");
    third.until("Manage saved devices");
    third.send(b"\r");
    third.until("Remove from saved list");
    third.send(b"\x1b[B\x1b[B\r");
    third.until("Removed from this list.");
    assert!(saved_rows(&third)["devices"].as_array().unwrap().is_empty());
    third.send(b"\x03");
    third.finish();
}

#[test]
fn pairing_keeps_host_but_never_saves_pairing_port_or_code() {
    let mut ui = Ui::start_with(false, SAVED_ADB, None);
    ui.until("No device connected");
    ui.send(b"\x1b[B\x1b[B\x1b[B\r");
    ui.until("Use the pairing address");
    ui.send(b"10.0.0.3:37000\r");
    ui.until("Pairing code");
    ui.send(b"123456\r");
    ui.until("Connect 10.0.0.3");
    let rows = saved_rows(&ui);
    assert_eq!(rows["devices"][0]["host"], "10.0.0.3");
    assert!(rows["devices"][0]["port"].is_null());
    let raw = fs::read_to_string(ui.state.join("adb-input/devices.json")).unwrap();
    assert!(!raw.contains("37000"));
    assert!(!raw.contains("123456"));
    ui.send(b"42000\r");
    ui.until("Connected. Select Start control");
    assert_eq!(saved_rows(&ui)["devices"][0]["port"], 42000);
    ui.send(b"\x03");
    ui.finish();

    let status = Command::new(env!("CARGO_BIN_EXE_adb-input"))
        .args([
            "--adb",
            ui.directory.join("adb").to_str().unwrap(),
            "connect",
            "10.0.0.3:43000",
        ])
        .env("XDG_STATE_HOME", &ui.state)
        .env("ADB_INPUT_TEST_DIR", &ui.directory)
        .status()
        .unwrap();
    assert!(status.success());
    let rows = saved_rows(&ui);
    assert_eq!(rows["devices"].as_array().unwrap().len(), 1);
    assert_eq!(rows["devices"][0]["port"], 43000);
}
