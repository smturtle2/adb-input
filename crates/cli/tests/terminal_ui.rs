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
    time::{Duration, Instant},
};
struct Ui {
    child: Child,
    master: File,
    slave: File,
    original: libc::termios,
    directory: PathBuf,
    output: String,
}
impl Ui {
    fn start(no_color: bool) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "adb-input-ui-{}-{}",
            std::process::id(),
            if no_color { 1 } else { 0 }
        ));
        fs::create_dir_all(&directory).unwrap();
        let adb = directory.join("adb");
        fs::write(&adb,"#!/bin/sh\ncase \"$1\" in\ndevices) printf 'List of devices attached\\nphone:5555 device model:Test_Phone\\n';;\nconnect) echo 'connection refused' >&2; exit 1;;\n*) exit 1;;\nesac\n").unwrap();
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
    ui.until("Change device");
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
