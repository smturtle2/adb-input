// SPDX-License-Identifier: EUPL-1.2
//! Bound a source's motion to its measured report cadence before transport.
use std::collections::VecDeque;

const MIN_CADENCE_US: u64 = 100;
const MAX_CADENCE_US: u64 = 50_000;
const MIN_OUTPUT_PERIOD_US: u64 = 1_000;
const MAX_CLUSTER_US: u64 = 1_000;
const HISTORY: usize = 9;
const FAST_CONFIRMATION: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MotionBin {
    pub(super) sequence: u64,
    pub(super) period_us: u32,
    pub(super) source_time_us: u64,
    pub(super) dx: i32,
    pub(super) dy: i32,
}

#[derive(Debug)]
struct Pending {
    dx: i64,
    dy: i64,
    last_dx: i32,
    last_dy: i32,
    last_time_us: u64,
    due_us: u64,
    period_us: u32,
}

#[derive(Debug, Default)]
pub(super) struct MotionBins {
    sequence: u64,
    last_time_us: Option<u64>,
    periods: VecDeque<u64>,
    fast_intervals: VecDeque<u64>,
    pending: Option<Pending>,
}

impl MotionBins {
    pub(super) fn reset(&mut self) {
        let sequence = self.sequence;
        *self = Self {
            sequence,
            ..Self::default()
        };
    }

    fn cadence_us(&self) -> Option<u64> {
        if self.periods.is_empty() {
            return None;
        }
        let mut periods: Vec<_> = self.periods.iter().copied().collect();
        periods.sort_unstable();
        Some(periods[periods.len() / 2])
    }

    fn observe_time(&mut self, source_time_us: u64) -> Option<u64> {
        let interval = self
            .last_time_us
            .and_then(|previous| source_time_us.checked_sub(previous));
        self.last_time_us = Some(source_time_us);
        if let Some(dt) = interval {
            if (MIN_CADENCE_US..MIN_OUTPUT_PERIOD_US).contains(&dt) {
                // Confirm a stable high-rate stream before replacing a slower
                // cadence with what might only be a transient input burst.
                if self.fast_intervals.front().is_some_and(|first| {
                    dt > first.saturating_mul(2) || dt.saturating_mul(2) < *first
                }) {
                    self.fast_intervals.clear();
                }
                self.fast_intervals.push_back(dt);
                if self.fast_intervals.len() > FAST_CONFIRMATION {
                    self.fast_intervals.pop_front();
                }
                if self
                    .cadence_us()
                    .is_some_and(|cadence| cadence < MIN_OUTPUT_PERIOD_US)
                {
                    self.push_period(dt);
                } else if self.fast_intervals.len() == FAST_CONFIRMATION {
                    self.periods.clear();
                    let confirmed: Vec<_> = self.fast_intervals.iter().copied().collect();
                    for interval in confirmed {
                        self.push_period(interval);
                    }
                }
            } else if (MIN_OUTPUT_PERIOD_US..=MAX_CADENCE_US).contains(&dt) {
                self.fast_intervals.clear();
                self.push_period(dt);
            } else {
                self.fast_intervals.clear();
            }
        }
        interval
    }

    fn push_period(&mut self, interval: u64) {
        self.periods.push_back(interval);
        if self.periods.len() > HISTORY {
            self.periods.pop_front();
        }
    }

    pub(super) fn note_discrete(&mut self, source_time_us: u64) {
        self.last_time_us = Some(source_time_us);
    }

    pub(super) fn next_due_us(&self) -> Option<u64> {
        self.pending.as_ref().map(|pending| pending.due_us)
    }

    /// Advance the source stream. At most one completed bin is returned; old
    /// periods are never queued for later replay.
    pub(super) fn accept(
        &mut self,
        dx: i32,
        dy: i32,
        source_time_us: u64,
        now_us: u64,
    ) -> Option<MotionBin> {
        if dx == 0 && dy == 0 {
            self.observe_time(source_time_us);
            return None;
        }
        let previous_period = self.cadence_us();
        let interval = self.observe_time(source_time_us);
        let Some(period) = self.cadence_us() else {
            // The first report has no cadence. Send it immediately. Until a
            // cadence is confirmed, retain the newest subsequent report for
            // a short bootstrap slot instead of losing it or replaying a burst.
            return if interval.is_none() || interval.is_some_and(|dt| dt > MAX_CADENCE_US) {
                self.pending = None;
                let bin = self.make_bin(0, source_time_us, dx, dy);
                (now_us.saturating_sub(source_time_us) <= MAX_CADENCE_US).then_some(bin)
            } else {
                if let Some(pending) = &mut self.pending {
                    pending.dx = i64::from(dx);
                    pending.dy = i64::from(dy);
                    pending.last_dx = dx;
                    pending.last_dy = dy;
                    pending.last_time_us = source_time_us;
                } else {
                    self.pending = Some(Pending {
                        dx: i64::from(dx),
                        dy: i64::from(dy),
                        last_dx: dx,
                        last_dy: dy,
                        last_time_us: source_time_us,
                        due_us: source_time_us.saturating_add(MIN_OUTPUT_PERIOD_US),
                        period_us: 0,
                    });
                }
                None
            };
        };

        if previous_period.is_some_and(|previous| {
            previous >= MIN_OUTPUT_PERIOD_US && period < MIN_OUTPUT_PERIOD_US
        }) {
            // The source has proven a faster cadence. Do not emit a partial
            // slow-period bin containing the transition's burst.
            self.pending = None;
            self.sequence = self.sequence.wrapping_add(1);
        }

        // A pause starts a new stroke, not another installment of old motion.
        if interval.is_some_and(|dt| dt > 4 * period) {
            self.pending = None;
            // A missing sequence marks the discontinuity for the receiver.
            self.sequence = self.sequence.wrapping_add(1);
            let bin = self.make_bin(
                period.max(MIN_OUTPUT_PERIOD_US) as u32,
                source_time_us,
                dx,
                dy,
            );
            return (now_us.saturating_sub(source_time_us) <= period.saturating_mul(2))
                .then_some(bin);
        }

        let completed = if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.period_us == 0 || source_time_us >= pending.due_us)
        {
            self.finish(now_us)
        } else {
            None
        };

        let cluster_limit = (period / 2).min(MAX_CLUSTER_US);
        if let Some(pending) = &mut self.pending {
            if source_time_us.saturating_sub(pending.last_time_us) < cluster_limit {
                pending.dx += i64::from(dx) - i64::from(pending.last_dx);
                pending.dy += i64::from(dy) - i64::from(pending.last_dy);
            } else {
                pending.dx += i64::from(dx);
                pending.dy += i64::from(dy);
            }
            pending.last_dx = dx;
            pending.last_dy = dy;
            pending.last_time_us = source_time_us;
        } else {
            // Bound a normal report to the measured source period. High-rate
            // sources still aggregate for at least one millisecond.
            self.pending = Some(Pending {
                dx: i64::from(dx),
                dy: i64::from(dy),
                last_dx: dx,
                last_dy: dy,
                last_time_us: source_time_us,
                due_us: source_time_us.saturating_add(period.max(MIN_OUTPUT_PERIOD_US)),
                period_us: period.max(MIN_OUTPUT_PERIOD_US) as u32,
            });
        }
        completed
    }

    pub(super) fn take_due(&mut self, now_us: u64) -> Option<MotionBin> {
        if self.next_due_us().is_some_and(|due| due <= now_us) {
            self.finish(now_us)
        } else {
            None
        }
    }

    pub(super) fn take_now(&mut self, now_us: u64) -> Option<MotionBin> {
        self.finish(now_us)
    }

    fn finish(&mut self, now_us: u64) -> Option<MotionBin> {
        let pending = self.pending.take()?;
        let bin = self.make_bin(
            pending.period_us,
            pending.last_time_us,
            pending.dx.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            pending.dy.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        );
        // A stalled host should not replay an old motion trail. The sequence
        // still advances, so the receiver can detect the discarded interval.
        let freshness_us = u64::from(pending.period_us)
            .max(MIN_OUTPUT_PERIOD_US)
            .saturating_mul(2);
        (now_us.saturating_sub(bin.source_time_us) <= freshness_us).then_some(bin)
    }

    fn make_bin(&mut self, period_us: u32, source_time_us: u64, dx: i32, dy: i32) -> MotionBin {
        self.sequence = self.sequence.wrapping_add(1);
        MotionBin {
            sequence: self.sequence,
            period_us,
            source_time_us,
            dx,
            dy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microburst_keeps_latest_and_stale_period_is_not_replayed() {
        let mut bins = MotionBins::default();
        assert_eq!(bins.accept(3, 0, 1_000_000, 1_000_000).unwrap().dx, 3);
        assert_eq!(bins.accept(5, 0, 1_008_000, 1_008_000), None);
        assert_eq!(bins.accept(30, 0, 1_008_010, 1_008_010), None);
        assert_eq!(bins.accept(7, 0, 1_016_000, 1_016_000).unwrap().dx, 30);
        assert_eq!(bins.take_due(1_024_000).unwrap().dx, 7);

        assert_eq!(bins.accept(9, 0, 1_032_000, 1_032_000), None);
        assert_eq!(bins.take_due(1_100_000), None);
        let resumed = bins.accept(11, 0, 1_108_000, 1_108_000).unwrap();
        assert_eq!(resumed.dx, 11);
        assert_eq!(resumed.sequence, 6); // Stale bin and idle transition consumed 4, 5.
        assert_eq!(bins.take_due(1_116_000), None);

        bins.reset();
        assert_eq!(bins.accept(1, 0, 1_200_000, 1_200_000).unwrap().sequence, 7);
    }

    #[test]
    fn bootstrap_after_discrete_keeps_latest_microreport() {
        let mut bins = MotionBins::default();
        bins.note_discrete(1_000_000);
        assert_eq!(bins.accept(4, 0, 1_000_010, 1_000_010), None);
        assert_eq!(bins.accept(9, 0, 1_000_020, 1_000_020), None);
        let bin = bins.take_due(1_001_020).unwrap();
        assert_eq!((bin.period_us, bin.dx), (0, 9));
    }

    #[test]
    fn steady_high_rate_reports_are_aggregated_to_one_millisecond() {
        for cadence_us in [125_u64, 250] {
            let mut bins = MotionBins::default();
            let start = 1_000_000;
            let mut outputs = vec![bins.accept(1, 0, start, start).unwrap()];
            for step in 1..=32 {
                let now = start + step * cadence_us;
                if let Some(bin) = bins.accept(1, 0, now, now) {
                    outputs.push(bin);
                }
                if let Some(bin) = bins.take_due(now) {
                    outputs.push(bin);
                }
            }
            assert!(outputs
                .iter()
                .any(|bin| bin.period_us == 1_000 && bin.dx >= 4));
        }
    }

    #[test]
    fn ordinary_reports_within_period_accumulate_while_microcluster_keeps_latest() {
        let mut bins = MotionBins::default();
        bins.accept(1, 0, 1_000_000, 1_000_000);
        assert_eq!(bins.accept(4, 0, 1_008_000, 1_008_000), None);
        assert_eq!(bins.accept(30, 0, 1_008_010, 1_008_010), None);
        assert_eq!(bins.next_due_us(), Some(1_016_000));
        assert_eq!(bins.accept(5, 0, 1_014_000, 1_014_000), None);
        assert_eq!(bins.take_due(1_016_000).unwrap().dx, 35);
    }
}
