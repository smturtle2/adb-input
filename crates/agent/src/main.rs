// SPDX-License-Identifier: EUPL-1.2
use adb_input_protocol::{Packet, READY, WATCHDOG_SECS};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const KEYBOARD: &[u8] = &[
    0x05, 1, 0x09, 6, 0xa1, 1, 0x05, 7, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8,
    0x81, 2, 0x75, 8, 0x95, 1, 0x81, 1, 0x19, 0, 0x29, 0xa4, 0x15, 0, 0x26, 0xa4, 0, 0x75, 8, 0x95,
    6, 0x81, 0, 0xc0,
];
const MOUSE: &[u8] = &[
    0x05, 1, 0x09, 2, 0xa1, 1, 0x09, 1, 0xa1, 0, 0x05, 9, 0x19, 1, 0x29, 5, 0x15, 0, 0x25, 1, 0x75,
    1, 0x95, 5, 0x81, 2, 0x75, 3, 0x95, 1, 0x81, 1, 0x05, 1, 0x09, 0x30, 0x09, 0x31, 0x16, 1, 0x80,
    0x26, 0xff, 0x7f, 0x75, 0x10, 0x95, 2, 0x81, 6, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 8,
    0x95, 1, 0x81, 6, 0xc0, 0xc0,
];

struct Device {
    file: File,
    report_size: usize,
}
impl Device {
    fn create(name: &str, descriptor: &[u8], product: u32, report_size: usize) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uhid")?;
        let mut device = Self { file, report_size };
        let mut event = vec![0u8; 280 + descriptor.len()];
        event[0..4].copy_from_slice(&11u32.to_le_bytes()); // UHID_CREATE2
        event[4..4 + name.len()].copy_from_slice(name.as_bytes());
        event[260..262].copy_from_slice(&(descriptor.len() as u16).to_le_bytes());
        event[262..264].copy_from_slice(&3u16.to_le_bytes()); // external USB HID, emulated
        event[268..272].copy_from_slice(&product.to_le_bytes());
        event[272..276].copy_from_slice(&1u32.to_le_bytes());
        event[280..].copy_from_slice(descriptor);
        device.event(&event)?;
        Ok(device)
    }
    fn event(&mut self, event: &[u8]) -> io::Result<()> {
        if self.file.write(event)? != event.len() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short UHID event"));
        }
        Ok(())
    }
    fn report(&mut self, data: &[u8]) -> io::Result<()> {
        let mut event = Vec::with_capacity(6 + data.len());
        event.extend_from_slice(&12u32.to_le_bytes()); // UHID_INPUT2
        event.extend_from_slice(&(data.len() as u16).to_le_bytes());
        event.extend_from_slice(data);
        self.event(&event)
    }
}
impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.report(&vec![0; self.report_size]);
    }
}
fn run() -> io::Result<()> {
    let start = Instant::now();
    let last = Arc::new(AtomicU64::new(0));
    let watch = last.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if start
            .elapsed()
            .as_secs()
            .saturating_sub(watch.load(Ordering::Relaxed))
            >= WATCHDOG_SECS
        {
            // Process exit closes all UHID descriptors even on a stuck input stream.
            std::process::exit(2);
        }
    });
    let mut keyboard = Device::create("ADB Input Keyboard", KEYBOARD, 1, 8)?;
    let mut mouse = Device::create("ADB Input Mouse", MOUSE, 2, 6)?;
    println!("{READY}");
    io::stdout().flush()?;
    let mut input = io::stdin().lock();
    while let Some(packet) = Packet::read(&mut input)? {
        last.store(start.elapsed().as_secs(), Ordering::Relaxed);
        match packet {
            Packet::Heartbeat => {}
            Packet::Stop => break,
            Packet::Keyboard(r) => keyboard.report(&r)?,
            Packet::Mouse(r) => mouse.report(&r)?,
        }
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("adb-input-agent: {e}");
        std::process::exit(1);
    }
}
