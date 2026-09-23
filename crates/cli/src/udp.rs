// SPDX-License-Identifier: EUPL-1.2
//! Optional authenticated motion path. The ADB stream remains the ordered copy.
use adb_input_protocol::udp::{Datagram, Direction, Payload, KEY_LEN, MAX_DATAGRAM_LEN};
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

pub(crate) struct MotionUdp {
    socket: UdpSocket,
    key: [u8; KEY_LEN],
    next_host_counter: u64,
}

impl MotionUdp {
    pub(crate) fn connect(serial: &str, port: u16, key: [u8; KEY_LEN]) -> io::Result<Option<Self>> {
        let Some(address) = serial
            .parse::<SocketAddr>()
            .ok()
            .filter(SocketAddr::is_ipv4)
        else {
            return Ok(None);
        };
        if port == 0 {
            return Ok(None);
        }
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.connect(SocketAddr::new(address.ip(), port))?;
        socket.set_read_timeout(Some(Duration::from_millis(50)))?;
        let nonce = random_nonce()?;
        let deadline = Instant::now() + Duration::from_millis(400);
        let mut buffer = [0; MAX_DATAGRAM_LEN + 1];
        let mut next_host_counter = 1u64;
        while Instant::now() < deadline {
            let hello = Datagram {
                counter: next_host_counter,
                epoch: 0,
                payload: Payload::Hello { nonce },
            }
            .encode(&key, Direction::HostToDevice);
            next_host_counter += 1;
            socket.send(&hello)?;
            match socket.recv(&mut buffer) {
                Ok(size) => {
                    if let Some(Datagram {
                        counter,
                        epoch: 0,
                        payload: Payload::Ack { nonce: received },
                    }) = Datagram::decode(&key, Direction::DeviceToHost, &buffer[..size])
                    {
                        if received == nonce && counter > 0 {
                            socket.set_read_timeout(None)?;
                            socket.set_nonblocking(true)?;
                            return Ok(Some(Self {
                                socket,
                                key,
                                next_host_counter,
                            }));
                        }
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }

    pub(crate) fn send_motion(&mut self, epoch: u64, payload: Payload) {
        // ADB carries the ordered copy, so a lost datagram needs no retry.
        let Some(next) = self.next_host_counter.checked_add(1) else {
            return;
        };
        let datagram = Datagram {
            counter: self.next_host_counter,
            epoch,
            payload,
        }
        .encode(&self.key, Direction::HostToDevice);
        self.next_host_counter = next;
        let _ = self.socket.send(&datagram);
    }
}

fn random_nonce() -> io::Result<u64> {
    use std::{fs::File, io::Read};
    let mut nonce = [0; 8];
    File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    Ok(u64::from_le_bytes(nonce))
}

pub(crate) fn random_key() -> io::Result<[u8; KEY_LEN]> {
    use std::{fs::File, io::Read};
    let mut key = [0; KEY_LEN];
    File::open("/dev/urandom")?.read_exact(&mut key)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_reachability_precedes_motion() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let key = [19; KEY_LEN];
        let server = std::thread::spawn(move || {
            let mut buffer = [0; MAX_DATAGRAM_LEN + 1];
            let (size, peer) = listener.recv_from(&mut buffer).unwrap();
            let hello = Datagram::decode(&key, Direction::HostToDevice, &buffer[..size]).unwrap();
            let Payload::Hello { nonce } = hello.payload else {
                panic!("expected reachability challenge");
            };
            let ack = Datagram {
                counter: 1,
                epoch: hello.epoch,
                payload: Payload::Ack { nonce },
            };
            listener
                .send_to(&ack.encode(&key, Direction::DeviceToHost), peer)
                .unwrap();
            let (size, _) = listener.recv_from(&mut buffer).unwrap();
            Datagram::decode(&key, Direction::HostToDevice, &buffer[..size]).unwrap()
        });
        let mut sender = MotionUdp::connect("127.0.0.1:5555", port, key)
            .unwrap()
            .unwrap();
        let payload = Payload::MotionBin {
            source_id: 2,
            sequence: 7,
            period_us: 8_000,
            source_time_us: 42,
            dx: 3,
            dy: -1,
        };
        sender.send_motion(1, payload);
        assert_eq!(
            server.join().unwrap(),
            Datagram {
                counter: 2,
                epoch: 1,
                payload
            }
        );
    }
}
