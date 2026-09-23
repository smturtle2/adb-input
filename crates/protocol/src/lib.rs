// SPDX-License-Identifier: EUPL-1.2
use std::io::{self, Read};

pub mod udp;

pub const READY: &str = "ADB-INPUT 6";
pub const WATCHDOG_SECS: u64 = 5;

#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    Heartbeat {
        source_time_us: u64,
    },
    Keyboard {
        control_id: u64,
        report: [u8; 8],
    },
    Mouse {
        control_id: u64,
        buttons: u8,
        dx: i32,
        dy: i32,
        wheel: i32,
        source_id: u32,
        source_time_us: u64,
    },
    MotionBin {
        source_id: u32,
        sequence: u64,
        period_us: u32,
        source_time_us: u64,
        dx: i32,
        dy: i32,
    },
    Stop,
    Release {
        epoch: u64,
        control_id: u64,
    },
    Arm {
        epoch: u64,
        control_id: u64,
        source_time_us: u64,
    },
    UdpKey([u8; 32]),
}
impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Heartbeat { source_time_us } => {
                [vec![0], source_time_us.to_le_bytes().to_vec()].concat()
            }
            Self::Keyboard { control_id, report } => {
                let mut bytes = Vec::with_capacity(17);
                bytes.push(1);
                bytes.extend_from_slice(&control_id.to_le_bytes());
                bytes.extend_from_slice(report);
                bytes
            }
            Self::Mouse {
                control_id,
                buttons,
                dx,
                dy,
                wheel,
                source_id,
                source_time_us,
            } => {
                let mut bytes = Vec::with_capacity(34);
                bytes.push(2);
                bytes.extend_from_slice(&control_id.to_le_bytes());
                bytes.push(*buttons);
                bytes.extend_from_slice(&dx.to_le_bytes());
                bytes.extend_from_slice(&dy.to_le_bytes());
                bytes.extend_from_slice(&wheel.to_le_bytes());
                bytes.extend_from_slice(&source_id.to_le_bytes());
                bytes.extend_from_slice(&source_time_us.to_le_bytes());
                bytes
            }
            Self::Stop => vec![3],
            Self::Release { epoch, control_id } => {
                let mut bytes = Vec::with_capacity(17);
                bytes.push(4);
                bytes.extend_from_slice(&epoch.to_le_bytes());
                bytes.extend_from_slice(&control_id.to_le_bytes());
                bytes
            }
            Self::Arm {
                epoch,
                control_id,
                source_time_us,
            } => {
                let mut bytes = Vec::with_capacity(25);
                bytes.push(7);
                bytes.extend_from_slice(&epoch.to_le_bytes());
                bytes.extend_from_slice(&control_id.to_le_bytes());
                bytes.extend_from_slice(&source_time_us.to_le_bytes());
                bytes
            }
            Self::UdpKey(key) => {
                let mut bytes = Vec::with_capacity(33);
                bytes.push(6);
                bytes.extend_from_slice(key);
                bytes
            }
            Self::MotionBin {
                source_id,
                sequence,
                period_us,
                source_time_us,
                dx,
                dy,
            } => {
                let mut bytes = Vec::with_capacity(33);
                bytes.push(5);
                bytes.extend_from_slice(&source_id.to_le_bytes());
                bytes.extend_from_slice(&sequence.to_le_bytes());
                bytes.extend_from_slice(&period_us.to_le_bytes());
                bytes.extend_from_slice(&source_time_us.to_le_bytes());
                bytes.extend_from_slice(&dx.to_le_bytes());
                bytes.extend_from_slice(&dy.to_le_bytes());
                bytes
            }
        }
    }
    pub fn read(reader: &mut impl Read) -> io::Result<Option<Self>> {
        let mut tag = [0];
        if reader.read(&mut tag)? == 0 {
            return Ok(None);
        }
        Ok(Some(match tag[0] {
            0 => {
                let mut source_time_us = [0; 8];
                reader.read_exact(&mut source_time_us)?;
                Self::Heartbeat {
                    source_time_us: u64::from_le_bytes(source_time_us),
                }
            }
            1 => {
                let mut data = [0; 16];
                reader.read_exact(&mut data)?;
                Self::Keyboard {
                    control_id: u64::from_le_bytes(data[0..8].try_into().unwrap()),
                    report: data[8..16].try_into().unwrap(),
                }
            }
            2 => {
                let mut data = [0; 33];
                reader.read_exact(&mut data)?;
                Self::Mouse {
                    control_id: u64::from_le_bytes(data[0..8].try_into().unwrap()),
                    buttons: data[8],
                    dx: i32::from_le_bytes(data[9..13].try_into().unwrap()),
                    dy: i32::from_le_bytes(data[13..17].try_into().unwrap()),
                    wheel: i32::from_le_bytes(data[17..21].try_into().unwrap()),
                    source_id: u32::from_le_bytes(data[21..25].try_into().unwrap()),
                    source_time_us: u64::from_le_bytes(data[25..33].try_into().unwrap()),
                }
            }
            3 => Self::Stop,
            4 => {
                let mut data = [0; 16];
                reader.read_exact(&mut data)?;
                Self::Release {
                    epoch: u64::from_le_bytes(data[0..8].try_into().unwrap()),
                    control_id: u64::from_le_bytes(data[8..16].try_into().unwrap()),
                }
            }
            5 => {
                let mut data = [0; 32];
                reader.read_exact(&mut data)?;
                Self::MotionBin {
                    source_id: u32::from_le_bytes(data[0..4].try_into().unwrap()),
                    sequence: u64::from_le_bytes(data[4..12].try_into().unwrap()),
                    period_us: u32::from_le_bytes(data[12..16].try_into().unwrap()),
                    source_time_us: u64::from_le_bytes(data[16..24].try_into().unwrap()),
                    dx: i32::from_le_bytes(data[24..28].try_into().unwrap()),
                    dy: i32::from_le_bytes(data[28..32].try_into().unwrap()),
                }
            }
            6 => {
                let mut key = [0; 32];
                reader.read_exact(&mut key)?;
                Self::UdpKey(key)
            }
            7 => {
                let mut data = [0; 24];
                reader.read_exact(&mut data)?;
                Self::Arm {
                    epoch: u64::from_le_bytes(data[0..8].try_into().unwrap()),
                    control_id: u64::from_le_bytes(data[8..16].try_into().unwrap()),
                    source_time_us: u64::from_le_bytes(data[16..24].try_into().unwrap()),
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown input packet",
                ))
            }
        }))
    }
}

pub fn keyboard_report(usages: impl IntoIterator<Item = u16>) -> [u8; 8] {
    let keys: std::collections::BTreeSet<_> = usages.into_iter().collect();
    let mut report = [0; 8];
    let mut count = 0;
    for key in keys {
        if (224..=231).contains(&key) {
            report[0] |= 1 << (key - 224);
        } else if (4..=164).contains(&key) {
            count += 1;
            if count <= 6 {
                report[count + 1] = key as u8;
            }
        }
    }
    if count > 6 {
        report[2..].fill(1);
    }
    report
}

pub fn mouse_hid_reports(buttons: u8, mut x: i32, mut y: i32, mut wheel: i32) -> Vec<[u8; 6]> {
    let mut result = Vec::new();
    loop {
        let dx = x.clamp(-32767, 32767);
        let dy = y.clamp(-32767, 32767);
        let dw = wheel.clamp(-127, 127);
        let [x0, x1] = (dx as i16).to_le_bytes();
        let [y0, y1] = (dy as i16).to_le_bytes();
        result.push([buttons, x0, x1, y0, y1, dw as i8 as u8]);
        x -= dx;
        y -= dy;
        wheel -= dw;
        if x == 0 && y == 0 && wheel == 0 {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_and_protocol_roundtrip() {
        assert_eq!(keyboard_report([224, 4, 4]), [1, 0, 4, 0, 0, 0, 0, 0]);
        assert_eq!(&keyboard_report(4..11)[2..], &[1; 6]);
        assert_eq!(keyboard_report([]), [0; 8]);
        let reports = mouse_hid_reports(1, 40000, -15, 130);
        assert_eq!(
            reports,
            [[1, 255, 127, 241, 255, 127], [1, 65, 28, 0, 0, 3]]
        );
        let packets = [
            Packet::Mouse {
                control_id: 2,
                buttons: 1,
                dx: 40_000,
                dy: -15,
                wheel: 130,
                source_id: 7,
                source_time_us: 123_456_789,
            },
            Packet::MotionBin {
                source_id: 7,
                sequence: 4,
                period_us: 8_000,
                source_time_us: 123_456_789,
                dx: 31,
                dy: -13,
            },
            Packet::Keyboard {
                control_id: 1,
                report: keyboard_report([224, 4]),
            },
            Packet::Heartbeat {
                source_time_us: 123_456_789,
            },
            Packet::UdpKey([7; 32]),
            Packet::Arm {
                epoch: 1,
                control_id: 3,
                source_time_us: 123_456_789,
            },
            Packet::Release {
                epoch: 2,
                control_id: 4,
            },
            Packet::Stop,
        ];
        let data: Vec<_> = packets.iter().flat_map(Packet::encode).collect();
        let mut reader = data.as_slice();
        for packet in packets {
            assert_eq!(Packet::read(&mut reader).unwrap(), Some(packet));
        }
        assert_eq!(Packet::read(&mut reader).unwrap(), None);
        assert!(Packet::read(&mut &[1, 0][..]).is_err());
        assert!(Packet::read(&mut &[2, 0][..]).is_err());
        assert!(Packet::read(&mut &[255][..]).is_err());
    }
}
