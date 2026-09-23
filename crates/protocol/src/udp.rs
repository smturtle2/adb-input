// SPDX-License-Identifier: EUPL-1.2
//! Confidential, authenticated datagrams for the optional motion fast path.
use crate::Packet;
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload as AeadPayload},
    ChaCha20Poly1305, Nonce,
};

pub const KEY_LEN: usize = 32;
pub const TAG_LEN: usize = 16;
pub const MOTION_DATAGRAM_LEN: usize = 8 + 8 + 33 + TAG_LEN;
pub const CONTROL_DATAGRAM_LEN: usize = 8 + 8 + 1 + 8 + TAG_LEN;
pub const MAX_DATAGRAM_LEN: usize = MOTION_DATAGRAM_LEN;

const DOMAIN: &[u8] = b"adb-input/udp/v2\0";
const HELLO_TAG: u8 = 6;
const ACK_TAG: u8 = 7;

/// Distinct nonce prefixes allow both directions to share one session key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    HostToDevice,
    DeviceToHost,
}

impl Direction {
    fn nonce(self, counter: u64) -> Nonce {
        let mut bytes = [0; 12];
        bytes[..4].copy_from_slice(match self {
            Self::HostToDevice => b"h2d\0",
            Self::DeviceToHost => b"d2h\0",
        });
        bytes[4..].copy_from_slice(&counter.to_le_bytes());
        Nonce::from(bytes)
    }
}

fn aad(counter: u64) -> [u8; 25] {
    let mut bytes = [0; 25];
    bytes[..DOMAIN.len()].copy_from_slice(DOMAIN);
    bytes[DOMAIN.len()..].copy_from_slice(&counter.to_le_bytes());
    bytes
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Payload {
    MotionBin {
        source_id: u32,
        sequence: u64,
        period_us: u32,
        source_time_us: u64,
        dx: i32,
        dy: i32,
    },
    Hello {
        nonce: u64,
    },
    Ack {
        nonce: u64,
    },
}

/// The counter is unique per sending direction and session key. Never wrap or
/// reuse it; receivers should reject counters at or below the last accepted one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Datagram {
    pub counter: u64,
    pub epoch: u64,
    pub payload: Payload,
}

impl Datagram {
    pub fn encode(&self, key: &[u8; KEY_LEN], direction: Direction) -> Vec<u8> {
        let mut plaintext = Vec::with_capacity(match self.payload {
            Payload::MotionBin { .. } => 8 + 33,
            Payload::Hello { .. } | Payload::Ack { .. } => 8 + 1 + 8,
        });
        plaintext.extend_from_slice(&self.epoch.to_le_bytes());
        match self.payload {
            Payload::MotionBin {
                source_id,
                sequence,
                period_us,
                source_time_us,
                dx,
                dy,
            } => plaintext.extend_from_slice(
                &Packet::MotionBin {
                    source_id,
                    sequence,
                    period_us,
                    source_time_us,
                    dx,
                    dy,
                }
                .encode(),
            ),
            Payload::Hello { nonce } => {
                plaintext.push(HELLO_TAG);
                plaintext.extend_from_slice(&nonce.to_le_bytes());
            }
            Payload::Ack { nonce } => {
                plaintext.push(ACK_TAG);
                plaintext.extend_from_slice(&nonce.to_le_bytes());
            }
        }
        let cipher = ChaCha20Poly1305::new_from_slice(key).expect("valid session key");
        let ciphertext = cipher
            .encrypt(
                &direction.nonce(self.counter),
                AeadPayload {
                    msg: &plaintext,
                    aad: &aad(self.counter),
                },
            )
            .expect("short datagram fits the AEAD message limit");
        let mut bytes = Vec::with_capacity(8 + ciphertext.len());
        bytes.extend_from_slice(&self.counter.to_le_bytes());
        bytes.extend_from_slice(&ciphertext);
        bytes
    }

    pub fn decode(
        key: &[u8; KEY_LEN],
        expected_direction: Direction,
        bytes: &[u8],
    ) -> Option<Self> {
        if !matches!(bytes.len(), MOTION_DATAGRAM_LEN | CONTROL_DATAGRAM_LEN) {
            return None;
        }
        let counter = u64::from_le_bytes(bytes[..8].try_into().ok()?);
        let cipher = ChaCha20Poly1305::new_from_slice(key).ok()?;
        let plaintext = cipher
            .decrypt(
                &expected_direction.nonce(counter),
                AeadPayload {
                    msg: &bytes[8..],
                    aad: &aad(counter),
                },
            )
            .ok()?;

        let epoch = u64::from_le_bytes(plaintext[..8].try_into().ok()?);
        let wire = &plaintext[8..];
        let payload = match wire {
            [HELLO_TAG, nonce @ ..] if nonce.len() == 8 => Payload::Hello {
                nonce: u64::from_le_bytes(nonce.try_into().ok()?),
            },
            [ACK_TAG, nonce @ ..] if nonce.len() == 8 => Payload::Ack {
                nonce: u64::from_le_bytes(nonce.try_into().ok()?),
            },
            [5, ..] if wire.len() == 33 => {
                let mut reader = wire;
                match Packet::read(&mut reader).ok()?? {
                    Packet::MotionBin {
                        source_id,
                        sequence,
                        period_us,
                        source_time_us,
                        dx,
                        dy,
                    } if reader.is_empty() => Payload::MotionBin {
                        source_id,
                        sequence,
                        period_us,
                        source_time_us,
                        dx,
                        dy,
                    },
                    _ => return None,
                }
            }
            _ => return None,
        };
        Some(Self {
            counter,
            epoch,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; KEY_LEN] = [0x53; KEY_LEN];

    fn motion(counter: u64) -> Datagram {
        Datagram {
            counter,
            epoch: u64::MAX - 1,
            payload: Payload::MotionBin {
                source_id: 7,
                sequence: u64::MAX,
                period_us: 8_333,
                source_time_us: 123_456_789,
                dx: -32_767,
                dy: 32_767,
            },
        }
    }

    #[test]
    fn encrypted_motion_and_reachability_roundtrip() {
        let cases = [
            motion(1),
            Datagram {
                counter: 2,
                epoch: 42,
                payload: Payload::Hello { nonce: u64::MAX },
            },
            Datagram {
                counter: 3,
                epoch: 42,
                payload: Payload::Ack { nonce: u64::MAX },
            },
        ];
        for datagram in cases {
            let direction = match datagram.payload {
                Payload::Ack { .. } => Direction::DeviceToHost,
                _ => Direction::HostToDevice,
            };
            let encoded = datagram.encode(&KEY, direction);
            assert_eq!(Datagram::decode(&KEY, direction, &encoded), Some(datagram));
            assert_eq!(
                encoded.len(),
                match datagram.payload {
                    Payload::MotionBin { .. } => MOTION_DATAGRAM_LEN,
                    _ => CONTROL_DATAGRAM_LEN,
                }
            );
            let wrong_direction = match direction {
                Direction::HostToDevice => Direction::DeviceToHost,
                Direction::DeviceToHost => Direction::HostToDevice,
            };
            assert_eq!(Datagram::decode(&KEY, wrong_direction, &encoded), None);
        }
        let encoded = motion(1).encode(&KEY, Direction::HostToDevice);
        let original = Packet::MotionBin {
            source_id: 7,
            sequence: u64::MAX,
            period_us: 8_333,
            source_time_us: 123_456_789,
            dx: -32_767,
            dy: 32_767,
        }
        .encode();
        assert!(!encoded
            .windows(original.len())
            .any(|bytes| bytes == original));
    }

    #[test]
    fn rejects_tampering_wrong_key_and_trailing_bytes() {
        let encoded = motion(7).encode(&KEY, Direction::HostToDevice);
        assert_eq!(
            Datagram::decode(&[0x54; KEY_LEN], Direction::HostToDevice, &encoded),
            None
        );
        for index in [0, 8, 10, MOTION_DATAGRAM_LEN - 1] {
            let mut tampered = encoded.clone();
            tampered[index] ^= 1;
            assert_eq!(
                Datagram::decode(&KEY, Direction::HostToDevice, &tampered),
                None
            );
        }
        assert_eq!(
            Datagram::decode(&KEY, Direction::HostToDevice, &encoded[..encoded.len() - 1]),
            None
        );
        let mut appended = encoded;
        appended.push(0);
        assert_eq!(
            Datagram::decode(&KEY, Direction::HostToDevice, &appended),
            None
        );
    }

    #[test]
    fn authenticated_non_motion_payload_is_not_a_motion_frame() {
        let counter = 19;
        let mut plaintext = 5_u64.to_le_bytes().to_vec();
        plaintext.extend_from_slice(&[0; 33]);
        plaintext[8] = 1; // A signed Keyboard-shaped frame is still disallowed.
        let cipher = ChaCha20Poly1305::new_from_slice(&KEY).unwrap();
        let ciphertext = cipher
            .encrypt(
                &Direction::HostToDevice.nonce(counter),
                AeadPayload {
                    msg: &plaintext,
                    aad: &aad(counter),
                },
            )
            .unwrap();
        let mut encoded = counter.to_le_bytes().to_vec();
        encoded.extend_from_slice(&ciphertext);
        assert_eq!(encoded.len(), MOTION_DATAGRAM_LEN);
        assert_eq!(
            Datagram::decode(&KEY, Direction::HostToDevice, &encoded),
            None
        );
    }

    #[test]
    fn counters_allow_strict_replay_and_reordering_rejection() {
        let mut highest = None;
        for (counter, accepted) in [(3, true), (3, false), (2, false), (4, true)] {
            let encoded = motion(counter).encode(&KEY, Direction::HostToDevice);
            let packet = Datagram::decode(&KEY, Direction::HostToDevice, &encoded).unwrap();
            let fresh = highest.is_none_or(|last| packet.counter > last);
            assert_eq!(fresh, accepted);
            if fresh {
                highest = Some(packet.counter);
            }
        }
    }
}
