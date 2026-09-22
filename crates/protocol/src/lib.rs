// SPDX-License-Identifier: EUPL-1.2
use std::io::{self, Read};

pub const READY: &str = "ADB-INPUT 1";
pub const WATCHDOG_SECS: u64 = 5;

#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    Heartbeat,
    Keyboard([u8; 8]),
    Mouse([u8; 6]),
    Stop,
}
impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Heartbeat => vec![0],
            Self::Keyboard(r) => [vec![1], r.to_vec()].concat(),
            Self::Mouse(r) => [vec![2], r.to_vec()].concat(),
            Self::Stop => vec![3],
        }
    }
    pub fn read(reader: &mut impl Read) -> io::Result<Option<Self>> {
        let mut tag = [0];
        if reader.read(&mut tag)? == 0 {
            return Ok(None);
        }
        Ok(Some(match tag[0] {
            0 => Self::Heartbeat,
            1 => {
                let mut r = [0; 8];
                reader.read_exact(&mut r)?;
                Self::Keyboard(r)
            }
            2 => {
                let mut r = [0; 6];
                reader.read_exact(&mut r)?;
                Self::Mouse(r)
            }
            3 => Self::Stop,
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

pub fn mouse_reports(buttons: u8, mut x: i32, mut y: i32, mut wheel: i32) -> Vec<Packet> {
    let mut result = Vec::new();
    loop {
        let dx = x.clamp(-32767, 32767);
        let dy = y.clamp(-32767, 32767);
        let dw = wheel.clamp(-127, 127);
        let [x0, x1] = (dx as i16).to_le_bytes();
        let [y0, y1] = (dy as i16).to_le_bytes();
        result.push(Packet::Mouse([buttons, x0, x1, y0, y1, dw as i8 as u8]));
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
        let packets = mouse_reports(1, 40000, -15, 130);
        assert_eq!(
            packets,
            [
                Packet::Mouse([1, 255, 127, 241, 255, 127]),
                Packet::Mouse([1, 65, 28, 0, 0, 3])
            ]
        );
        let data: Vec<_> = packets.iter().flat_map(Packet::encode).collect();
        let mut reader = data.as_slice();
        for packet in packets {
            assert_eq!(Packet::read(&mut reader).unwrap(), Some(packet));
        }
        assert_eq!(Packet::read(&mut reader).unwrap(), None);
        assert!(Packet::read(&mut &[1, 0][..]).is_err());
        assert!(Packet::read(&mut &[255][..]).is_err());
    }
}
