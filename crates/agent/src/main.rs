// SPDX-License-Identifier: EUPL-1.2
use adb_input_protocol::{mouse_hid_reports, Packet, READY, WATCHDOG_SECS};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    net::UdpSocket,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError, TryRecvError},
        Arc,
    },
    time::{Duration, Instant},
};

mod motion;
mod udp;
use motion::{MotionBin, MotionSampler, MouseOutput};

enum Input {
    Adb(io::Result<Option<Packet>>),
    Udp { epoch: u64, bin: MotionBin },
}

struct Session {
    armed: bool,
    epoch: u64,
    udp_socket: Option<UdpSocket>,
    sender: mpsc::Sender<Input>,
}

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

fn emit_mouse(device: &mut Device, output: MouseOutput) -> io::Result<()> {
    for report in mouse_hid_reports(output.buttons, output.dx, output.dy, output.wheel) {
        device.report(&report)?;
    }
    Ok(())
}

fn read_input(sender: mpsc::Sender<Input>) {
    let mut input = io::stdin().lock();
    loop {
        let packet = Packet::read(&mut input);
        let done = !matches!(&packet, Ok(Some(p)) if !matches!(p, Packet::Stop));
        if sender.send(Input::Adb(packet)).is_err() || done {
            break;
        }
    }
}

fn ack_control(control_id: u64) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "ACK {control_id}")?;
    output.flush()
}

fn handle_input(
    input: Input,
    keyboard: &mut Device,
    mouse: &mut Device,
    sampler: &mut MotionSampler,
    session: &mut Session,
    last: &AtomicU64,
    start: Instant,
) -> io::Result<bool> {
    let packet = match input {
        Input::Udp { epoch, bin } => {
            if session.armed && epoch == session.epoch {
                // ADB is still the ordered backup for this same source sequence.
                sampler.accept_bin(bin, Instant::now());
            }
            return Ok(true);
        }
        Input::Adb(packet) => packet,
    };
    let Some(packet) = packet? else {
        return Ok(false);
    };
    // The writer owns the watchdog progress. A blocked UHID write must still time out.
    last.store(start.elapsed().as_secs(), Ordering::Relaxed);
    match packet {
        Packet::Heartbeat { source_time_us } => {
            sampler.observe_clock(source_time_us, Instant::now());
        }
        Packet::Stop => return Ok(false),
        Packet::UdpKey(key) => {
            if let Some(socket) = session.udp_socket.take() {
                let _ = udp::start(socket, key, session.sender.clone());
            }
        }
        Packet::Arm {
            epoch,
            control_id,
            source_time_us,
        } => {
            sampler.reset();
            sampler.observe_clock(source_time_us, Instant::now());
            keyboard.report(&[0; 8])?;
            mouse.report(&[0; 6])?;
            session.epoch = epoch;
            session.armed = true;
            ack_control(control_id)?;
        }
        Packet::Release { epoch, control_id } => {
            session.armed = false;
            session.epoch = epoch;
            sampler.reset();
            keyboard.report(&[0; 8])?;
            mouse.report(&[0; 6])?;
            ack_control(control_id)?;
        }
        Packet::Keyboard { control_id, report } => {
            if session.armed {
                keyboard.report(&report)?;
            }
            ack_control(control_id)?;
        }
        Packet::Mouse {
            control_id,
            buttons,
            dx,
            dy,
            wheel,
            ..
        } => {
            if session.armed {
                if let Some(output) = sampler.flush_at(Instant::now()) {
                    emit_mouse(mouse, output)?;
                }
                let output = sampler.discrete(buttons, dx, dy, wheel);
                emit_mouse(mouse, output)?;
            }
            ack_control(control_id)?;
        }
        Packet::MotionBin {
            source_id,
            sequence,
            period_us,
            source_time_us,
            dx,
            dy,
        } => {
            if session.armed {
                sampler.accept_bin(
                    MotionBin {
                        source_id,
                        sequence,
                        period_us,
                        source_time_us,
                        dx,
                        dy,
                    },
                    Instant::now(),
                );
            }
        }
    }
    Ok(true)
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
    let udp_socket = udp::bind();
    let udp_port = udp_socket
        .as_ref()
        .and_then(|socket| socket.local_addr().ok())
        .map_or(0, |address| address.port());
    println!("{READY} {udp_port}");
    io::stdout().flush()?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn({
        let sender = sender.clone();
        move || read_input(sender)
    });
    let mut session = Session {
        armed: false,
        epoch: 0,
        udp_socket,
        sender,
    };
    let mut sampler = MotionSampler::default();
    loop {
        // Consume the whole currently available batch so that sampling sees
        // its newest bin. Do not emit a stale early bin from an ADB burst.
        loop {
            match receiver.try_recv() {
                Ok(packet) => {
                    if !handle_input(
                        packet,
                        &mut keyboard,
                        &mut mouse,
                        &mut sampler,
                        &mut session,
                        &last,
                        start,
                    )? {
                        return Ok(());
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "input reader stopped",
                    ));
                }
            }
        }
        let now = Instant::now();
        if let Some(output) = sampler.take_immediate(now) {
            emit_mouse(&mut mouse, output)?;
        }
        if sampler.next_tick().is_some_and(|deadline| now >= deadline) {
            if let Some(output) = sampler.tick(now) {
                emit_mouse(&mut mouse, output)?;
            }
            continue;
        }
        let received = match sampler.next_tick() {
            Some(deadline) => receiver.recv_timeout(deadline.saturating_duration_since(now)),
            None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(packet) => {
                if !handle_input(
                    packet,
                    &mut keyboard,
                    &mut mouse,
                    &mut sampler,
                    &mut session,
                    &last,
                    start,
                )? {
                    return Ok(());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "input reader stopped",
                ));
            }
        }
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("adb-input-agent: {e}");
        std::process::exit(1);
    }
}
