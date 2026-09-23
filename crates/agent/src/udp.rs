// SPDX-License-Identifier: EUPL-1.2
//! Optional authenticated motion ingress. The ADB stream remains the backup.

use adb_input_protocol::udp::{Datagram, Direction, Payload, KEY_LEN, MAX_DATAGRAM_LEN};
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::mpsc::Sender,
    thread,
};

use crate::{motion::MotionBin, Input};

pub(crate) fn bind() -> Option<UdpSocket> {
    UdpSocket::bind("0.0.0.0:0").ok()
}

pub(crate) fn start(
    socket: UdpSocket,
    key: [u8; KEY_LEN],
    sender: Sender<Input>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("adb-input-udp".into())
        .spawn(move || receive(socket, key, sender))
        .map(|_| ())
}

fn receive(socket: UdpSocket, key: [u8; KEY_LEN], sender: Sender<Input>) {
    let mut peer: Option<SocketAddr> = None;
    let mut last_host_counter: Option<u64> = None;
    let mut device_counter = 0u64;
    // An extra byte makes oversized datagrams fail authentication after recv_from
    // truncates them, even if their first bytes form a complete signed frame.
    let mut buffer = [0u8; MAX_DATAGRAM_LEN + 1];
    loop {
        let (length, address) = match socket.recv_from(&mut buffer) {
            Ok(received) => received,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let Some(datagram) = Datagram::decode(&key, Direction::HostToDevice, &buffer[..length])
        else {
            continue;
        };
        if last_host_counter.is_some_and(|last| datagram.counter <= last) {
            continue;
        }
        match datagram.payload {
            Payload::Hello { nonce } if peer.is_none() || peer == Some(address) => {
                peer = Some(address);
                last_host_counter = Some(datagram.counter);
                let Some(counter) = device_counter.checked_add(1) else {
                    break;
                };
                device_counter = counter;
                let ack = Datagram {
                    counter,
                    epoch: datagram.epoch,
                    payload: Payload::Ack { nonce },
                }
                .encode(&key, Direction::DeviceToHost);
                let _ = socket.send_to(&ack, address);
            }
            Payload::MotionBin {
                source_id,
                sequence,
                period_us,
                source_time_us,
                dx,
                dy,
            } if peer == Some(address) => {
                last_host_counter = Some(datagram.counter);
                if sender
                    .send(Input::Udp {
                        epoch: datagram.epoch,
                        bin: MotionBin {
                            source_id,
                            sequence,
                            period_us,
                            source_time_us,
                            dx,
                            dy,
                        },
                    })
                    .is_err()
                {
                    break;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn authenticated_peer_delivers_motion_and_rejects_replay() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        let key = [23; KEY_LEN];
        start(server, key, sender).unwrap();

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let hello = Datagram {
            counter: 1,
            epoch: 0,
            payload: Payload::Hello { nonce: 91 },
        };
        client
            .send_to(&hello.encode(&key, Direction::HostToDevice), address)
            .unwrap();
        let mut buffer = [0u8; MAX_DATAGRAM_LEN + 1];
        let (length, _) = client.recv_from(&mut buffer).unwrap();
        assert!(matches!(
            Datagram::decode(&key, Direction::DeviceToHost, &buffer[..length]),
            Some(Datagram {
                counter: 1,
                payload: Payload::Ack { nonce: 91 },
                ..
            })
        ));

        let motion = Datagram {
            counter: 2,
            epoch: 7,
            payload: Payload::MotionBin {
                source_id: 4,
                sequence: 12,
                period_us: 8_000,
                source_time_us: 123_456,
                dx: 3,
                dy: -2,
            },
        };
        client
            .send_to(&motion.encode(&key, Direction::HostToDevice), address)
            .unwrap();
        let input = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            input,
            Input::Udp {
                epoch: 7,
                bin: MotionBin {
                    source_id: 4,
                    sequence: 12,
                    dx: 3,
                    dy: -2,
                    ..
                }
            }
        ));
        // The same authenticated packet cannot replay another motion sample.
        client
            .send_to(&motion.encode(&key, Direction::HostToDevice), address)
            .unwrap();
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
    }
}
