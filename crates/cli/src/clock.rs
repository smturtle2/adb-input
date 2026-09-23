// SPDX-License-Identifier: EUPL-1.2
//! Host clock shared by evdev event timestamps and transport heartbeats.
use std::io;

fn clock_us(clock: libc::clockid_t) -> io::Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(clock, &mut time) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let seconds = u64::try_from(time.tv_sec)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative clock time"))?;
    let nanos = u64::try_from(time.tv_nsec)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative clock time"))?;
    Ok(seconds
        .saturating_mul(1_000_000)
        .saturating_add(nanos / 1_000))
}

pub(crate) fn monotonic_us() -> io::Result<u64> {
    clock_us(libc::CLOCK_MONOTONIC)
}

pub(crate) fn realtime_to_monotonic_offset_us() -> io::Result<i128> {
    let before = monotonic_us()?;
    let realtime = clock_us(libc::CLOCK_REALTIME)?;
    let after = monotonic_us()?;
    let midpoint = before.saturating_add(after.saturating_sub(before) / 2);
    Ok(i128::from(realtime) - i128::from(midpoint))
}
