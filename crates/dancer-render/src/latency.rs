//! Measuring display latency (spec §11.2: "measure it; don't assume").
//!
//! # What can and cannot be measured
//!
//! The scheduler starts a move early by `impact_cell × frame_duration` plus
//! whatever the display costs. That second term has two parts:
//!
//! - **Present cost**, from deciding to draw to `UpdateLayeredWindow` returning.
//!   Measurable, and measured here. Phase 0.2 clocked it at 0.066–0.112 ms, so it
//!   is small — but "small" was itself a measurement, not an assumption.
//! - **Compositor delay**, from the call returning to photons leaving the panel.
//!   **Not observable from inside the process.** DWM composites on its own
//!   schedule, and nothing the app can call reports when a frame was actually
//!   scanned out.
//!
//! So this measures what it can and reports it honestly. The unmeasurable part is
//! a constant, and a constant is exactly what §9.2's offset slider absorbs — the
//! user trims by eye until the dancer looks on the beat, and whatever DWM adds is
//! inside that number whether we model it or not.
//!
//! The practical consequence: getting this slightly wrong is not fatal. Getting
//! `impact_cell × frame_duration` wrong *is*, and that one is exact.

use std::time::Duration;

/// How many recent frames to keep. A couple of seconds at 60 Hz — long enough to
/// be stable, short enough to follow a machine that has started struggling.
const WINDOW: usize = 120;

/// Rolling median of present cost.
///
/// Median rather than mean: a single scheduling hiccup or a GC pause elsewhere on
/// the machine can produce a present ten times the usual, and one outlier should
/// not shift what the scheduler believes about every subsequent frame.
#[derive(Debug, Default)]
pub struct LatencyMonitor {
    samples: Vec<Duration>,
    next: usize,
    /// The tick interval the caller drives the loop at, if it knows it.
    tick: Duration,
}

impl LatencyMonitor {
    pub fn new(tick: Duration) -> Self {
        Self {
            samples: Vec::with_capacity(WINDOW),
            next: 0,
            tick,
        }
    }

    pub fn record(&mut self, present: Duration) {
        if self.samples.len() < WINDOW {
            self.samples.push(present);
        } else {
            self.samples[self.next] = present;
            self.next = (self.next + 1) % WINDOW;
        }
    }

    pub fn samples(&self) -> usize {
        self.samples.len()
    }

    /// Median present cost, or zero before anything has been measured.
    pub fn present_median(&self) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let mut v = self.samples.clone();
        v.sort_unstable();
        v[v.len() / 2]
    }

    /// What to hand the scheduler as `render_latency`.
    ///
    /// Present cost plus half the tick interval: a cell change becomes visible at
    /// the next tick, which on average is half an interval away. Both terms are
    /// measured or known; the compositor's share is deliberately absent, for the
    /// reason in the module docs.
    pub fn render_latency(&self) -> Duration {
        self.present_median() + self.tick / 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_micros(n * 1000)
    }

    #[test]
    fn reports_zero_until_it_has_measured_something() {
        let m = LatencyMonitor::new(Duration::ZERO);
        assert_eq!(m.present_median(), Duration::ZERO);
        assert_eq!(m.samples(), 0);
    }

    #[test]
    fn one_outlier_does_not_move_the_median() {
        // The reason this is a median: one stall must not convince the scheduler
        // that every frame now costs ten times as much.
        let mut m = LatencyMonitor::new(Duration::ZERO);
        for _ in 0..20 {
            m.record(Duration::from_micros(100));
        }
        m.record(ms(50));
        assert_eq!(m.present_median(), Duration::from_micros(100));
    }

    #[test]
    fn the_window_rolls_rather_than_growing() {
        let mut m = LatencyMonitor::new(Duration::ZERO);
        for _ in 0..WINDOW * 3 {
            m.record(Duration::from_micros(100));
        }
        assert_eq!(m.samples(), WINDOW);
    }

    #[test]
    fn it_follows_a_machine_that_has_started_struggling() {
        let mut m = LatencyMonitor::new(Duration::ZERO);
        for _ in 0..WINDOW {
            m.record(Duration::from_micros(100));
        }
        for _ in 0..WINDOW {
            m.record(ms(5));
        }
        assert_eq!(m.present_median(), ms(5));
    }

    #[test]
    fn render_latency_includes_half_a_tick() {
        let mut m = LatencyMonitor::new(ms(8));
        m.record(Duration::from_micros(100));
        assert_eq!(m.render_latency(), Duration::from_micros(100) + ms(4));
    }
}
