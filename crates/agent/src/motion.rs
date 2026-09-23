// SPDX-License-Identifier: EUPL-1.2
//! Sample the newest source-time bin at the source cadence. Old queued bins
//! are replaced. A short, bounded tail bridges missing bins after steady motion.
use adb_input_protocol::WATCHDOG_SECS;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

const HISTORY: usize = 31;
const CLOCK_HISTORY: usize = 16;
const PREDICTION_TICKS: u32 = 2;

#[derive(Default)]
struct ClockBaseline {
    offsets: VecDeque<(u64, i128)>,
}

impl ClockBaseline {
    fn prune(&mut self, arrival_us: u64) {
        let window_us = WATCHDOG_SECS.saturating_mul(1_000_000);
        while self
            .offsets
            .front()
            .is_some_and(|(at, _)| arrival_us.saturating_sub(*at) > window_us)
        {
            self.offsets.pop_front();
        }
    }

    fn baseline(&self, fallback: i128) -> i128 {
        self.offsets
            .iter()
            .map(|(_, offset)| *offset)
            .min()
            .unwrap_or(fallback)
    }

    fn push(&mut self, arrival_us: u64, offset: i128) {
        self.offsets.push_back((arrival_us, offset));
        if self.offsets.len() > CLOCK_HISTORY {
            self.offsets.pop_front();
        }
    }

    fn observe_heartbeat(&mut self, arrival_us: u64, source_time_us: u64) {
        self.prune(arrival_us);
        self.push(
            arrival_us,
            i128::from(arrival_us) - i128::from(source_time_us),
        );
    }

    fn observe_motion(&mut self, arrival_us: u64, source_time_us: u64) -> (i128, i128) {
        self.prune(arrival_us);
        let offset = i128::from(arrival_us) - i128::from(source_time_us);
        let baseline = self.baseline(offset);
        if self.offsets.is_empty() || offset < baseline {
            self.push(arrival_us, offset);
        }
        (offset, baseline)
    }
}

#[derive(Clone, Copy)]
struct Pending {
    delta: [i32; 2],
    sequence: u64,
    source_time_us: u64,
    arrival: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct MouseOutput {
    pub buttons: u8,
    pub dx: i32,
    pub dy: i32,
    pub wheel: i32,
}

pub(super) struct MotionBin {
    pub source_id: u32,
    pub sequence: u64,
    pub period_us: u32,
    pub source_time_us: u64,
    pub dx: i32,
    pub dy: i32,
}

#[derive(Default)]
struct Source {
    last_sequence: Option<u64>,
    discontinuity: bool,
    period: Duration,
    pending: Option<Pending>,
    previous: [i32; 2],
    previous_predicted: bool,
    // The remainder of division by two keeps odd displacements exact over
    // leading and trailing samples without accumulating movement debt.
    remainder: [i64; 2],
    recent: [[i32; 2]; 3],
    recent_count: usize,
    last_real: Option<[i32; 2]>,
    last_real_source_time_us: Option<u64>,
    last_real_sequence: Option<u64>,
    last_real_sampled_at: Option<Instant>,
    bin_gaps_us: VecDeque<u64>,
    prediction_budget: [i64; 2],
    predicted: [i64; 2],
    speculative: [i64; 2],
    missing_ticks: u32,
    immediate: bool,
    next_tick: Option<Instant>,
}

impl Source {
    fn clear_filter(&mut self) {
        self.pending = None;
        self.previous = [0; 2];
        self.previous_predicted = false;
        self.remainder = [0; 2];
        self.immediate = false;
        self.next_tick = None;
        self.recent_count = 0;
        self.last_real = None;
        self.last_real_source_time_us = None;
        self.last_real_sequence = None;
        self.last_real_sampled_at = None;
        self.bin_gaps_us.clear();
        self.speculative = [0; 2];
        self.clear_prediction();
    }

    fn clear_prediction(&mut self) {
        self.prediction_budget = [0; 2];
        self.predicted = [0; 2];
        self.missing_ticks = 0;
    }

    fn record_fresh(&mut self, pending: Pending, now: Instant) {
        let delta = pending.delta;
        if let Some(previous) = self.last_real_source_time_us {
            if let Some(gap) = pending
                .source_time_us
                .checked_sub(previous)
                .filter(|gap| *gap > 0)
            {
                self.bin_gaps_us.push_back(gap);
                if self.bin_gaps_us.len() > HISTORY {
                    self.bin_gaps_us.pop_front();
                }
            }
        }
        self.last_real_source_time_us = Some(pending.source_time_us);
        self.last_real_sequence = Some(pending.sequence);
        self.last_real_sampled_at = Some(now);
        self.last_real = Some(delta);
        if self.recent_count == 3 {
            self.recent.copy_within(1..3, 0);
            self.recent[2] = delta;
        } else {
            self.recent[self.recent_count] = delta;
            self.recent_count += 1;
        }
    }

    fn prediction_deadline(&self) -> Option<Instant> {
        if self.recent_count != 3 || self.bin_gaps_us.len() < 2 {
            return None;
        }
        let mut gaps: Vec<_> = self.bin_gaps_us.iter().copied().collect();
        gaps.sort_unstable();
        let median = gaps[gaps.len() / 2];
        let mut deviations: Vec<_> = gaps.iter().map(|gap| gap.abs_diff(median)).collect();
        deviations.sort_unstable();
        let mad = deviations[deviations.len() / 2];
        // The source's bin cadence is not the raw device report cadence.
        // A half report period plus robust variation keeps ordinary bins from
        // triggering speculative movement before their usual delivery time.
        let upper = gaps[(19 * gaps.len()).div_ceil(20) - 1]; // empirical p95
        let gap_us = median
            .saturating_add(mad.saturating_mul(2))
            .max(upper)
            .saturating_add((self.period.as_micros() as u64) / 2);
        self.last_real_sampled_at?
            .checked_add(Duration::from_micros(gap_us))
    }

    fn consistency(a: [i32; 2], b: [i32; 2]) -> f64 {
        let dot = i128::from(a[0]) * i128::from(b[0]) + i128::from(a[1]) * i128::from(b[1]);
        let norm =
            |v: [i32; 2]| i128::from(v[0]) * i128::from(v[0]) + i128::from(v[1]) * i128::from(v[1]);
        if dot <= 0 {
            return 0.0;
        }
        let dot = dot as f64;
        (dot * dot / (norm(a) as f64 * norm(b) as f64)).clamp(0.0, 1.0)
    }

    fn start_prediction(&mut self) {
        if self.recent_count != 3 {
            return;
        }
        let confidence = Self::consistency(self.recent[0], self.recent[1])
            .min(Self::consistency(self.recent[1], self.recent[2]));
        for axis in 0..2 {
            let last = i64::from(self.recent[2][axis]);
            let bounded = ((last.abs() as f64 * confidence).round() as i64).min(last.abs());
            let desired = last.signum() * bounded;
            self.prediction_budget[axis] = if desired.signum() == self.speculative[axis].signum() {
                desired.signum() * (desired.abs() - self.speculative[axis].abs()).max(0)
            } else {
                desired
            };
        }
    }

    fn predict(&mut self) -> [i32; 2] {
        if self.missing_ticks == 0 {
            self.start_prediction();
            // Confidence requires another uninterrupted run of three sampled
            // real bins. Alternating delivery gaps must not mint a new motion
            // budget on every received packet.
            self.recent_count = 0;
        }
        self.missing_ticks = self.missing_ticks.saturating_add(1);
        if self.missing_ticks > PREDICTION_TICKS {
            return [0; 2];
        }
        let shift = self.missing_ticks.min(63);
        let mut current = [0; 2];
        for (axis, item) in current.iter_mut().enumerate() {
            let budget = self.prediction_budget[axis];
            let magnitude = budget.unsigned_abs();
            let target = budget.signum() * (magnitude - (magnitude >> shift)) as i64;
            *item = (target - self.predicted[axis]) as i32;
            self.predicted[axis] = target;
            self.speculative[axis] += i64::from(*item);
        }
        current
    }

    fn reconcile(&mut self, pending: Pending) -> [i32; 2] {
        if self
            .last_real_sequence
            .is_some_and(|last| pending.sequence > last.saturating_add(1))
        {
            // The prediction stands in for bins discarded by latest-only
            // sampling; do not also subtract it from the newly arrived bin.
            self.speculative = [0; 2];
        }
        let mut corrected = pending.delta;
        for (axis, item) in corrected.iter_mut().enumerate() {
            let debt = self.speculative[axis];
            let real = i64::from(*item);
            if debt.signum() == real.signum() {
                // Never consume the whole fresh bin to repay a forecast.
                let repayment = debt.signum() * debt.abs().min(real.abs() / 2);
                *item = (real - repayment) as i32;
                self.speculative[axis] -= repayment;
            } else if real != 0 {
                self.speculative[axis] = 0;
            }
        }
        corrected
    }

    fn accept(&mut self, bin: MotionBin, offset: i128, baseline: i128, now: Instant) {
        let MotionBin {
            sequence,
            period_us,
            source_time_us,
            dx,
            dy,
            ..
        } = bin;
        let delta = [dx, dy];
        if self.last_sequence.is_some_and(|last| sequence <= last) {
            return;
        }
        let sequence_gap = self
            .last_sequence
            .is_some_and(|last| sequence > last.saturating_add(1));
        self.last_sequence = Some(sequence);

        // The source clock and phone clock have different epochs. Their
        // smallest observed offset is the low-latency path; excess offset is
        // transport age. Keep the baseline when dropping stale arrivals.
        let cadence_us = if period_us == 0 {
            self.period.as_micros() as u64
        } else {
            u64::from(period_us)
        };
        let freshness_us = cadence_us.saturating_mul(4).clamp(16_000, 50_000);
        if offset - baseline > i128::from(freshness_us) {
            // Keep the bounded tail running through a transport stall. The
            // next fresh bin must not blend with this rejected observation.
            self.discontinuity = true;
            return;
        }
        if sequence_gap || self.discontinuity {
            self.clear_filter();
        }
        self.discontinuity = false;
        self.period = Duration::from_micros(u64::from(period_us.max(1_000)));
        // The first bin has no reliable cadence. Deliver it directly; the
        // next bin starts the two-sample filter once the cadence is known.
        if period_us == 0 {
            self.clear_filter();
            self.period = Duration::ZERO;
        }
        if let Some(last) = self.last_real {
            if i128::from(last[0]) * i128::from(delta[0])
                + i128::from(last[1]) * i128::from(delta[1])
                <= 0
            {
                // A reversal cannot inherit the old tail or its prediction.
                self.clear_filter();
            }
        }
        self.clear_prediction();
        self.pending = Some(Pending {
            delta,
            sequence,
            source_time_us,
            arrival: now,
        });
        // A normal FIR tail can leave a separate prediction deadline armed.
        // Fresh motion during that wait must not sit until the deadline.
        if self.next_tick.is_none() || self.previous == [0; 2] {
            self.next_tick = None;
            self.immediate = true;
        }
    }

    fn sample(&mut self, current: [i32; 2], predicted: bool) -> [i32; 2] {
        let mut output = [0; 2];
        for axis in 0..2 {
            let numerator =
                i64::from(self.previous[axis]) + i64::from(current[axis]) + self.remainder[axis];
            // Round a half-count toward the new motion. At a stroke start,
            // a one-count bin must move now rather than vanishing for a tick.
            let value = (numerator + numerator.signum()) / 2;
            self.remainder[axis] = numerator - 2 * value;
            output[axis] = value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        }
        self.previous = current;
        self.previous_predicted = predicted;
        output
    }

    fn take_immediate(&mut self, now: Instant) -> Option<[i32; 2]> {
        if !self.immediate {
            return None;
        }
        self.immediate = false;
        let pending = self.pending.take()?;
        let current = self.reconcile(pending);
        self.record_fresh(pending, now);
        if self.period.is_zero() {
            return Some(current);
        }
        let output = self.sample(current, false);
        self.next_tick = (self.previous != [0; 2]).then_some(now + self.period);
        Some(output)
    }

    fn tick(&mut self, now: Instant) -> Option<[i32; 2]> {
        let deadline = self.next_tick?;
        if now < deadline {
            return None;
        }
        // A delayed wakeup represents one current sample, not a queue of
        // missed ticks. The pending real bin is stale too after a long wakeup.
        if now.saturating_duration_since(deadline) > self.period.saturating_mul(2) {
            self.clear_filter();
            return None;
        }
        let (current, predicted) = if let Some(fresh) = self.pending.take() {
            let corrected = self.reconcile(fresh);
            self.record_fresh(fresh, now);
            (corrected, false)
        } else if self.missing_ticks != 0
            || self.prediction_deadline().is_some_and(|due| now >= due)
        {
            (self.predict(), true)
        } else {
            ([0; 2], false)
        };
        let output = self.sample(current, predicted);
        self.next_tick = if self.previous != [0; 2] {
            Some(now + self.period)
        } else {
            self.prediction_deadline().filter(|due| *due > now)
        };
        Some(output)
    }

    fn flush_at(&mut self, now: Instant) -> [i64; 2] {
        self.immediate = false;
        self.next_tick = None;
        self.clear_prediction();
        self.recent_count = 0;
        self.last_real = None;
        self.last_real_source_time_us = None;
        self.last_real_sampled_at = None;
        self.bin_gaps_us.clear();
        if self.previous_predicted {
            // A click must not release the second half of an unobserved bin.
            // That un-emitted half is also not debt against a pending real bin.
            for axis in 0..2 {
                let numerator = i64::from(self.previous[axis]) + self.remainder[axis];
                let un_emitted = (numerator + numerator.signum()) / 2;
                let debt = self.speculative[axis];
                if debt.signum() == un_emitted.signum() {
                    self.speculative[axis] -= debt.signum() * debt.abs().min(un_emitted.abs());
                }
            }
            self.previous = [0; 2];
            self.previous_predicted = false;
            self.remainder = [0; 2];
        }
        let pending = self.pending.take().filter(|pending| {
            now.saturating_duration_since(pending.arrival) <= self.period.saturating_mul(2)
        });
        let current = pending.map_or([0; 2], |pending| self.reconcile(pending));
        self.speculative = [0; 2];
        self.last_real_sequence = None;
        if self.period.is_zero() {
            return current.map(i64::from);
        }
        let leading = self.sample(current, false);
        let trailing = self.sample([0; 2], false);
        [
            i64::from(leading[0]) + i64::from(trailing[0]),
            i64::from(leading[1]) + i64::from(trailing[1]),
        ]
    }
}

#[derive(Default)]
pub(super) struct MotionSampler {
    buttons: u8,
    sources: HashMap<u32, Source>,
    clock: ClockBaseline,
    start: Option<Instant>,
}

impl MotionSampler {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn accept_bin(&mut self, bin: MotionBin, now: Instant) {
        if self
            .sources
            .get(&bin.source_id)
            .and_then(|source| source.last_sequence)
            .is_some_and(|last| bin.sequence <= last)
        {
            // An out-of-order packet must not lower the shared clock floor.
            return;
        }
        let start = *self.start.get_or_insert(now);
        let arrival_us = now.saturating_duration_since(start).as_micros() as u64;
        let (offset, baseline) = self.clock.observe_motion(arrival_us, bin.source_time_us);
        self.sources
            .entry(bin.source_id)
            .or_default()
            .accept(bin, offset, baseline, now);
    }

    pub fn observe_clock(&mut self, host_time_us: u64, now: Instant) {
        let start = *self.start.get_or_insert(now);
        let arrival_us = now.saturating_duration_since(start).as_micros() as u64;
        self.clock.observe_heartbeat(arrival_us, host_time_us);
    }

    pub fn next_tick(&self) -> Option<Instant> {
        self.sources
            .values()
            .filter_map(|source| source.next_tick)
            .min()
    }

    fn output(&self, delta: [i64; 2]) -> Option<MouseOutput> {
        (delta != [0; 2]).then_some(MouseOutput {
            buttons: self.buttons,
            dx: delta[0].clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            dy: delta[1].clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            wheel: 0,
        })
    }

    pub fn take_immediate(&mut self, now: Instant) -> Option<MouseOutput> {
        let mut total = [0_i64; 2];
        for source in self.sources.values_mut() {
            if let Some(delta) = source.take_immediate(now) {
                for axis in 0..2 {
                    total[axis] = total[axis].saturating_add(i64::from(delta[axis]));
                }
            }
        }
        self.output(total)
    }

    pub fn tick(&mut self, now: Instant) -> Option<MouseOutput> {
        let mut total = [0_i64; 2];
        for source in self.sources.values_mut() {
            if let Some(delta) = source.tick(now) {
                for axis in 0..2 {
                    total[axis] = total[axis].saturating_add(i64::from(delta[axis]));
                }
            }
        }
        self.output(total)
    }

    pub fn flush_at(&mut self, now: Instant) -> Option<MouseOutput> {
        let mut total = [0_i64; 2];
        for source in self.sources.values_mut() {
            let delta = source.flush_at(now);
            for axis in 0..2 {
                total[axis] = total[axis].saturating_add(delta[axis]);
            }
        }
        self.output(total)
    }

    pub fn discrete(&mut self, buttons: u8, dx: i32, dy: i32, wheel: i32) -> MouseOutput {
        self.buttons = buttons;
        MouseOutput {
            buttons,
            dx,
            dy,
            wheel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn move_x(output: Option<MouseOutput>) -> i32 {
        output.map_or(0, |output| output.dx)
    }

    fn bin(sampler: &mut MotionSampler, t: Instant, sequence: u64, dx: i32) {
        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence,
                period_us: 8_000,
                source_time_us: 1_000_000 + sequence * 8_000,
                dx,
                dy: 0,
            },
            t,
        );
    }

    #[test]
    fn newest_bin_is_sampled_once_and_split_without_drift() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        bin(&mut sampler, t, 1, 50);
        bin(&mut sampler, t, 2, 11);
        bin(&mut sampler, t, 3, 21);
        assert_eq!(move_x(sampler.take_immediate(t)), 11);
        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence: 2,
                period_us: 8_000,
                source_time_us: 2_000_000,
                dx: 900,
                dy: 0,
            },
            t,
        ); // stale sequence and impossible timestamp cannot poison clock
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(8))), 10);
        assert_eq!(sampler.next_tick(), None);

        bin(&mut sampler, t + Duration::from_millis(16), 4, 12);
        assert_eq!(
            move_x(sampler.take_immediate(t + Duration::from_millis(16))),
            6
        );

        bin(&mut sampler, t + Duration::from_millis(20), 5, -15);
        assert_eq!(
            move_x(sampler.take_immediate(t + Duration::from_millis(20))),
            -8
        );
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(28))), -7);
        assert_eq!(sampler.tick(t + Duration::from_millis(36)), None);
        assert_eq!(sampler.next_tick(), None);
    }

    #[test]
    fn one_count_restart_is_visible_without_waiting_for_the_tail() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence: 1,
                period_us: 8_000,
                source_time_us: 1_000_000,
                dx: 1,
                dy: -1,
            },
            t,
        );
        let first = sampler.take_immediate(t).unwrap();
        assert_eq!([first.dx, first.dy], [1, -1]);
        assert_eq!(sampler.tick(t + Duration::from_millis(8)), None);
    }

    #[test]
    fn steady_samples_blend_neighbors_but_a_late_tick_drops_the_old_tail() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        bin(&mut sampler, t, 1, 20);
        assert_eq!(move_x(sampler.take_immediate(t)), 10);
        bin(&mut sampler, t + Duration::from_millis(8), 2, 40);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(8))), 30);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(16))), 20);
        assert_eq!(sampler.next_tick(), None);

        bin(&mut sampler, t + Duration::from_millis(24), 3, 10);
        assert_eq!(
            move_x(sampler.take_immediate(t + Duration::from_millis(24))),
            5
        );
        bin(&mut sampler, t + Duration::from_millis(32), 4, 12);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(50))), 0);
        assert_eq!(sampler.next_tick(), None);
    }

    #[test]
    fn discrete_flush_preserves_click_position_and_bootstrap_is_direct() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence: 1,
                period_us: 0,
                source_time_us: 1_000_000,
                dx: 7,
                dy: -3,
            },
            t,
        );
        assert_eq!(sampler.take_immediate(t).unwrap().dx, 7);
        assert_eq!(sampler.next_tick(), None);
        bin(&mut sampler, t, 2, 11);
        assert_eq!(move_x(sampler.take_immediate(t)), 6);
        bin(&mut sampler, t, 3, 9);
        assert_eq!(move_x(sampler.flush_at(t)), 14); // remaining 5 + newest 9
        assert_eq!(
            sampler.discrete(1, 4, 0, 1),
            MouseOutput {
                buttons: 1,
                dx: 4,
                dy: 0,
                wheel: 1
            }
        );
        assert_eq!(sampler.next_tick(), None);
        sampler.reset();
        assert_eq!(sampler.discrete(0, 0, 0, 0).buttons, 0);
    }

    #[test]
    fn a_transport_stall_or_missing_bin_does_not_replay_old_motion() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        bin(&mut sampler, t, 1, 20);
        assert_eq!(move_x(sampler.take_immediate(t)), 10);
        // Host source time is only 8 ms newer, but phone arrival is 100 ms
        // later. Its stale displacement is dropped; an armed tail is kept
        // until the next tick, rather than being cut off on packet receipt.
        bin(&mut sampler, t + Duration::from_millis(100), 2, 400);
        assert!(sampler.next_tick().is_some());
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(100))), 0);
        assert_eq!(sampler.next_tick(), None);

        // A later fresh bin resumes without inheriting the discarded one.
        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence: 3,
                period_us: 8_000,
                source_time_us: 1_100_000,
                dx: 12,
                dy: 0,
            },
            t + Duration::from_millis(101),
        );
        assert_eq!(
            move_x(sampler.take_immediate(t + Duration::from_millis(101))),
            6
        );
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(109))), 6);

        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence: 4,
                period_us: 8_000,
                source_time_us: 1_119_000,
                dx: 18,
                dy: 0,
            },
            t + Duration::from_millis(120),
        );
        assert_eq!(
            move_x(sampler.take_immediate(t + Duration::from_millis(120))),
            9
        );
        // Sequence 5 was lost; bin 6 must not blend with bin 4's tail.
        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence: 6,
                period_us: 8_000,
                source_time_us: 1_120_000,
                dx: 14,
                dy: 0,
            },
            t + Duration::from_millis(121),
        );
        assert_eq!(
            move_x(sampler.take_immediate(t + Duration::from_millis(121))),
            7
        );
    }

    #[test]
    fn steady_motion_predicts_a_short_tail_with_at_most_one_bin_of_overshoot() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        for sequence in 1..=3 {
            let now = t + Duration::from_millis((sequence - 1) * 8);
            bin(&mut sampler, now, sequence, 20);
            if sequence == 1 {
                assert_eq!(move_x(sampler.take_immediate(now)), 10);
            } else {
                assert_eq!(move_x(sampler.tick(now)), 20);
            }
        }
        let mut tail = vec![move_x(sampler.tick(t + Duration::from_millis(24)))];
        while let Some(due) = sampler.next_tick() {
            tail.push(move_x(sampler.tick(due)));
        }
        assert_eq!(tail[0], 10); // ordinary FIR tail at T
        assert_eq!(tail[1], 5); // prediction begins at learned bin deadline
        assert_eq!(tail.iter().sum::<i32>(), 25); // 10 FIR tail + 15 speculative
        assert_eq!(sampler.next_tick(), None);
    }

    #[test]
    fn late_rejection_does_not_resurrect_old_motion() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        for sequence in 1..=3 {
            let now = t + Duration::from_millis((sequence - 1) * 8);
            bin(&mut sampler, now, sequence, 128);
            if sequence == 1 {
                sampler.take_immediate(now);
            } else {
                sampler.tick(now);
            }
        }
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(24))), 64);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(28))), 32);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(36))), 48);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(44))), 16);
        assert_eq!(sampler.next_tick(), None);
        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence: 4,
                period_us: 8_000,
                source_time_us: 1_032_000,
                dx: 900,
                dy: 0,
            },
            t + Duration::from_millis(60),
        );
        assert_eq!(sampler.next_tick(), None);
        assert_eq!(sampler.take_immediate(t + Duration::from_millis(60)), None);
        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence: 5,
                period_us: 8_000,
                source_time_us: 1_080_000,
                dx: 40,
                dy: 0,
            },
            t + Duration::from_millis(65),
        );
        assert_eq!(
            move_x(sampler.take_immediate(t + Duration::from_millis(65))),
            20
        );
    }

    #[test]
    fn reversal_and_release_clear_the_prediction() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        for sequence in 1..=3 {
            let now = t + Duration::from_millis((sequence - 1) * 8);
            bin(&mut sampler, now, sequence, 20);
            if sequence == 1 {
                sampler.take_immediate(now);
            } else {
                sampler.tick(now);
            }
        }
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(24))), 10);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(28))), 5);
        bin(&mut sampler, t + Duration::from_millis(29), 4, -20);
        assert_eq!(
            move_x(sampler.take_immediate(t + Duration::from_millis(29))),
            -10
        );
        assert_eq!(move_x(sampler.flush_at(t + Duration::from_millis(29))), -10);
        assert_eq!(sampler.next_tick(), None);
        sampler.reset();
        assert_eq!(sampler.next_tick(), None);
        assert_eq!(sampler.tick(t + Duration::from_millis(40)), None);
    }

    #[test]
    fn one_fresh_bin_after_a_gap_does_not_rearm_prediction() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        for sequence in 1..=3 {
            let now = t + Duration::from_millis((sequence - 1) * 8);
            bin(&mut sampler, now, sequence, 20);
            if sequence == 1 {
                sampler.take_immediate(now);
            } else {
                sampler.tick(now);
            }
        }
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(24))), 10);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(28))), 5);
        bin(&mut sampler, t + Duration::from_millis(30), 4, 20);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(36))), 10);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(44))), 5);
        assert_eq!(sampler.tick(t + Duration::from_millis(52)), None);
        assert_eq!(sampler.next_tick(), None);
    }

    #[test]
    fn learned_bin_cadence_does_not_predict_between_ordinary_bins() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        for sequence in 1..=4 {
            let now = t + Duration::from_millis((sequence - 1) * 14);
            sampler.accept_bin(
                MotionBin {
                    source_id: 1,
                    sequence,
                    period_us: 8_000,
                    source_time_us: 1_000_000 + sequence * 14_000,
                    dx: 20,
                    dy: 0,
                },
                now,
            );
            assert_eq!(move_x(sampler.take_immediate(now)), 10);
            let tail = now + Duration::from_millis(8);
            assert_eq!(move_x(sampler.tick(tail)), 10);
        }
        // The raw report period is 8 ms; sampled bins arrive every 14 ms.
        // Prediction waits beyond the normal next-bin delivery time.
        let due = sampler.next_tick().unwrap();
        assert!(due > t + Duration::from_millis(56));
        assert_eq!(move_x(sampler.tick(due)), 5);
    }

    #[test]
    fn click_flush_cancels_unemitted_prediction_and_old_pending_motion() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        for sequence in 1..=3 {
            let now = t + Duration::from_millis((sequence - 1) * 8);
            bin(&mut sampler, now, sequence, 20);
            if sequence == 1 {
                sampler.take_immediate(now);
            } else {
                sampler.tick(now);
            }
        }
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(24))), 10);
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(28))), 5);
        assert_eq!(sampler.flush_at(t + Duration::from_millis(29)), None);
        assert_eq!(sampler.next_tick(), None);

        let mut sampler = MotionSampler::default();
        for sequence in 1..=3 {
            let now = t + Duration::from_millis((sequence - 1) * 8);
            bin(&mut sampler, now, sequence, 20);
            if sequence == 1 {
                sampler.take_immediate(now);
            } else {
                sampler.tick(now);
            }
        }
        sampler.tick(t + Duration::from_millis(24));
        assert_eq!(move_x(sampler.tick(t + Duration::from_millis(28))), 5);
        bin(&mut sampler, t + Duration::from_millis(29), 4, 20);
        assert_eq!(move_x(sampler.flush_at(t + Duration::from_millis(29))), 15);
        // The emitted 5-count forecast plus the click flush equals the real bin.

        let mut sampler = MotionSampler::default();
        bin(&mut sampler, t, 1, 20);
        sampler.take_immediate(t);
        bin(&mut sampler, t + Duration::from_millis(8), 2, 200);
        // The overdue pending bin is not dumped immediately before the click.
        assert_eq!(move_x(sampler.flush_at(t + Duration::from_millis(30))), 10);
    }

    #[test]
    fn heartbeat_keeps_clock_baseline_across_idle_before_rejecting_late_motion() {
        let t = Instant::now();
        let mut sampler = MotionSampler::default();
        sampler.observe_clock(1_000_000, t);
        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence: 1,
                period_us: 8_000,
                source_time_us: 1_000_000,
                dx: 20,
                dy: 0,
            },
            t,
        );
        assert_eq!(move_x(sampler.take_immediate(t)), 10);
        sampler.tick(t + Duration::from_millis(8));
        sampler.tick(t + Duration::from_millis(16));
        sampler.observe_clock(6_000_000, t + Duration::from_secs(5));
        sampler.accept_bin(
            MotionBin {
                source_id: 1,
                sequence: 2,
                period_us: 8_000,
                source_time_us: 6_010_000,
                dx: 200,
                dy: 0,
            },
            t + Duration::from_millis(5_080),
        );
        assert_eq!(
            sampler.take_immediate(t + Duration::from_millis(5_080)),
            None
        );
        assert_eq!(sampler.next_tick(), None);
    }

    #[test]
    fn directional_confidence_handles_full_i32_range() {
        let delta = [i32::MAX, i32::MIN];
        let mut source = Source {
            recent: [delta; 3],
            recent_count: 3,
            ..Source::default()
        };
        source.start_prediction();
        assert_eq!(
            source.prediction_budget,
            [i64::from(delta[0]), i64::from(delta[1])]
        );
        assert_eq!(Source::consistency(delta, [-delta[0], i32::MAX]), 0.0);
    }
}
