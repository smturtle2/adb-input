// SPDX-License-Identifier: EUPL-1.2
//! Accumulate pointer input until a complete evdev report is available.
use evdev::{
    AbsoluteAxisCode, EventSummary, InputEvent, KeyCode, RelativeAxisCode, SynchronizationCode,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct MotionReport {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) wheel: i32,
}

#[derive(Default)]
pub(super) struct Motion {
    x: f64,
    y: f64,
    wheel: i32,
    pub(super) buttons: u8,
    dirty: bool,
    previous: [Option<i32>; 2],
    pub(super) ranges: [i64; 2],
}

impl Motion {
    pub(super) fn reset(&mut self) {
        *self = Self {
            ranges: self.ranges,
            ..Self::default()
        };
    }

    pub(super) fn update(&mut self, event: InputEvent, sensitivity: f64) -> Option<MotionReport> {
        match event.destructure() {
            EventSummary::Key(_, key, value)
                if (KeyCode::BTN_LEFT.0..=KeyCode::BTN_EXTRA.0).contains(&key.0) =>
            {
                let bit = 1 << (key.0 - KeyCode::BTN_LEFT.0);
                if value != 0 {
                    self.buttons |= bit
                } else {
                    self.buttons &= !bit
                };
                self.dirty = true;
            }
            EventSummary::RelativeAxis(_, RelativeAxisCode::REL_X, value) => {
                self.x += f64::from(value) * sensitivity;
                self.dirty = true;
            }
            EventSummary::RelativeAxis(_, RelativeAxisCode::REL_Y, value) => {
                self.y += f64::from(value) * sensitivity;
                self.dirty = true;
            }
            EventSummary::RelativeAxis(_, RelativeAxisCode::REL_WHEEL, value) => {
                self.wheel = self.wheel.saturating_add(value);
                self.dirty = true;
            }
            EventSummary::AbsoluteAxis(
                _,
                axis @ (AbsoluteAxisCode::ABS_X | AbsoluteAxisCode::ABS_Y),
                value,
            ) => {
                let axis = axis.0 as usize;
                if let Some(previous) = self.previous[axis] {
                    let range = self.ranges[axis].max(1) as f64;
                    let delta = (i64::from(value) - i64::from(previous)) as f64 / range
                        * 2000.0
                        * sensitivity;
                    if axis == 0 {
                        self.x += delta
                    } else {
                        self.y += delta
                    };
                    self.dirty = true;
                }
                self.previous[axis] = Some(value);
            }
            EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _) if self.dirty => {
                let x = self.x as i32;
                let y = self.y as i32;
                let wheel = self.wheel;
                self.x -= f64::from(x);
                self.y -= f64::from(y);
                self.wheel = 0;
                self.dirty = false;
                return Some(MotionReport { x, y, wheel });
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::EventType;

    fn event(kind: EventType, code: u16, value: i32) -> InputEvent {
        InputEvent::new(kind.0, code, value)
    }

    fn report(motion: &mut Motion) -> Option<MotionReport> {
        motion.update(
            event(
                EventType::SYNCHRONIZATION,
                SynchronizationCode::SYN_REPORT.0,
                0,
            ),
            1.0,
        )
    }

    fn output(x: i32, y: i32, wheel: i32) -> Option<MotionReport> {
        Some(MotionReport { x, y, wheel })
    }

    #[test]
    fn relative_reports_preserve_fractions_buttons_and_wheel() {
        let mut motion = Motion::default();
        for (kind, code, value) in [
            (EventType::KEY, KeyCode::BTN_LEFT.0, 1),
            (EventType::KEY, KeyCode::BTN_EXTRA.0, 1),
            (EventType::RELATIVE, RelativeAxisCode::REL_X.0, 3),
            (EventType::RELATIVE, RelativeAxisCode::REL_Y.0, -3),
            (EventType::RELATIVE, RelativeAxisCode::REL_WHEEL.0, 2),
        ] {
            assert_eq!(motion.update(event(kind, code, value), 0.5), None);
        }
        assert_eq!(report(&mut motion), output(1, -1, 2));
        assert_eq!(motion.buttons, 0b10001);
        assert_eq!(report(&mut motion), None);
        motion.update(
            event(EventType::RELATIVE, RelativeAxisCode::REL_X.0, 1),
            0.5,
        );
        motion.update(
            event(EventType::RELATIVE, RelativeAxisCode::REL_Y.0, -1),
            0.5,
        );
        motion.update(event(EventType::KEY, KeyCode::BTN_LEFT.0, 0), 1.0);
        assert_eq!(report(&mut motion), output(1, -1, 0));
        assert_eq!(motion.buttons, 0b10000);
    }

    #[test]
    fn absolute_motion_reanchors_after_release_and_keeps_axis_ranges() {
        let mut motion = Motion {
            ranges: [1000, 2000],
            ..Motion::default()
        };
        let x = |value| event(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_X.0, value);
        let y = |value| event(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_Y.0, value);
        motion.update(x(100), 1.0);
        motion.update(y(500), 1.0);
        assert_eq!(report(&mut motion), None);
        motion.update(x(150), 2.0);
        motion.update(y(400), 2.0);
        assert_eq!(report(&mut motion), output(200, -200, 0));
        motion.update(event(EventType::KEY, KeyCode::BTN_LEFT.0, 1), 1.0);
        motion.reset();
        assert_eq!(motion.buttons, 0);
        assert_eq!(report(&mut motion), None);
        motion.update(x(900), 1.0);
        motion.update(y(1000), 1.0);
        assert_eq!(report(&mut motion), None);
        motion.update(x(950), 1.0);
        motion.update(y(900), 1.0);
        assert_eq!(report(&mut motion), output(100, -100, 0));
    }

    #[test]
    fn discrete_reports_keep_button_and_wheel_state() {
        let mut motion = Motion::default();
        motion.update(event(EventType::KEY, KeyCode::BTN_LEFT.0, 1), 1.0);
        assert_eq!(report(&mut motion), output(0, 0, 0));
        motion.update(
            event(EventType::RELATIVE, RelativeAxisCode::REL_WHEEL.0, 2),
            1.0,
        );
        assert_eq!(report(&mut motion), output(0, 0, 2));
    }
}
