// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Simulated time: instants, clock sources and chip clock domains.
//!
//! Models never read a clock themselves. The bus tells each model what time it is
//! ([`I2cTarget::advance`](super::I2cTarget::advance)) before every event, so the same model runs
//! on whichever time base the caller uses:
//!
//! - [`VirtualClock`]: time moves only when the simulation says so (bus transfers, driver delays).
//!   Deterministic; for host tests and, later, for loosely timed co-simulation.
//! - [`HostClock`]: the monotonic clock of the machine the models run on. weft-sim uses it inside
//!   the guest, where KVM, HVF or WHPX tie guest time to real time.
//!
//! A model keeps its own timing in a [`ClockDomain`]: the chip's oscillator or conversion rate,
//! with an optional deviation, so timing tolerances from the datasheet can be tested.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A point in simulated time: nanoseconds since the simulation started.
///
/// An integer, not [`Instant`], so a virtual clock can produce it. `u64` nanoseconds cover about
/// 584 years. Arithmetic saturates instead of wrapping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SimTime(u64);

impl SimTime {
    /// The start of the simulation.
    pub const ZERO: Self = Self(0);

    /// The instant `nanos` nanoseconds after the start.
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Nanoseconds since the start.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Time from `earlier` to `self`, or zero when `earlier` is later.
    pub fn saturating_duration_since(self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

impl std::ops::Add<Duration> for SimTime {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self {
        Self(self.0.saturating_add(nanos(rhs)))
    }
}

impl std::ops::AddAssign<Duration> for SimTime {
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl std::fmt::Display for SimTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{:09} s",
            self.0 / 1_000_000_000,
            self.0 % 1_000_000_000
        )
    }
}

/// A duration in whole nanoseconds, saturating at `u64::MAX`.
fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// Where the current time comes from.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// The current time.
    fn now(&self) -> SimTime;
    /// The simulation has used time up to `t`, for example to the end of a bus transfer.
    /// A virtual clock moves forward to `t`; a clock that follows real time ignores it.
    fn advance_to(&self, t: SimTime) {
        let _ = t;
    }
}

/// A clock that moves only when told to. Clones share the same time, so a test can hand one
/// clone to an [`I2cBus`](super::I2cBus) and another to a driver as its delay
/// (`embedded_hal::delay::DelayNs`).
#[derive(Debug, Clone, Default)]
pub struct VirtualClock(Arc<AtomicU64>);

impl VirtualClock {
    /// A clock at [`SimTime::ZERO`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves the clock forward by `d`.
    pub fn advance(&self, d: Duration) {
        let d = nanos(d);
        // try_update with a closure that always returns Some cannot fail.
        let _ = self.0.try_update(Ordering::SeqCst, Ordering::SeqCst, |t| {
            Some(t.saturating_add(d))
        });
    }
}

impl Clock for VirtualClock {
    fn now(&self) -> SimTime {
        SimTime(self.0.load(Ordering::SeqCst))
    }

    fn advance_to(&self, t: SimTime) {
        self.0.fetch_max(t.0, Ordering::SeqCst);
    }
}

/// The host's monotonic clock (`CLOCK_MONOTONIC` on Linux), counted from when this clock was
/// created. Copies share the same start, so models on different buses agree on the time.
#[derive(Debug, Clone, Copy)]
pub struct HostClock {
    start: Instant,
}

impl HostClock {
    /// A clock that starts at [`SimTime::ZERO`] now.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for HostClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for HostClock {
    fn now(&self) -> SimTime {
        SimTime(nanos(self.start.elapsed()))
    }
}

/// A clock inside a chip (an oscillator, a conversion rate) or on a bus (SCL), with an optional
/// deviation from its nominal frequency.
///
/// Models convert their own cycles to time with it and compute every edge from a fixed origin
/// (`origin + cycles(n)`) instead of adding up rounded periods, so long runs do not drift.
/// Edges are rounded up to the next nanosecond.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockDomain {
    hz: u64,
    ppm: i32,
}

impl ClockDomain {
    /// Highest nominal frequency: keeps one period at 1 ns or more with any allowed deviation.
    pub const MAX_HZ: u64 = 500_000_000;
    /// Largest deviation either way, in parts per million (±50 %).
    pub const MAX_DEVIATION_PPM: i32 = 500_000;

    /// A clock at `hz` without the range check, for constants known to be in range.
    pub(super) const fn nominal(hz: u64) -> Self {
        Self { hz, ppm: 0 }
    }

    /// A clock at its nominal frequency, or `None` unless `1 <= hz <= MAX_HZ`.
    pub fn new(hz: u64) -> Option<Self> {
        (1..=Self::MAX_HZ)
            .contains(&hz)
            .then_some(Self { hz, ppm: 0 })
    }

    /// The same clock running `ppm` parts per million fast (positive) or slow (negative), clamped
    /// to [`MAX_DEVIATION_PPM`](Self::MAX_DEVIATION_PPM). A datasheet tolerance of ±10 % is
    /// ±100 000 ppm.
    #[must_use]
    pub fn with_deviation_ppm(self, ppm: i32) -> Self {
        Self {
            ppm: ppm.clamp(-Self::MAX_DEVIATION_PPM, Self::MAX_DEVIATION_PPM),
            ..self
        }
    }

    /// Nominal frequency in hertz.
    pub fn hz(&self) -> u64 {
        self.hz
    }

    /// Deviation from the nominal frequency, in parts per million.
    pub fn deviation_ppm(&self) -> i32 {
        self.ppm
    }

    /// Actual frequency in hertz, times 10^6 (exact).
    fn micro_hz(&self) -> u128 {
        let factor = u128::try_from(1_000_000_i64 + i64::from(self.ppm)).unwrap_or(1);
        u128::from(self.hz) * factor
    }

    /// The time `n` cycles take.
    pub fn cycles(&self, n: u64) -> Duration {
        // n / (hz * (1 + ppm/1e6)) seconds = n * 1e15 / micro_hz nanoseconds, rounded up.
        let ns = (u128::from(n) * 1_000_000_000_000_000).div_ceil(self.micro_hz());
        Duration::from_nanos(u64::try_from(ns).unwrap_or(u64::MAX))
    }

    /// Complete cycles within `d`: the largest `n` with `cycles(n) <= d`.
    pub fn cycles_in(&self, d: Duration) -> u64 {
        let n = d.as_nanos() * self.micro_hz() / 1_000_000_000_000_000;
        u64::try_from(n).unwrap_or(u64::MAX)
    }

    /// The first cycle edge after `now`, counting edges from `origin`. Before `origin`, the first
    /// edge after it.
    pub fn next_edge(&self, origin: SimTime, now: SimTime) -> SimTime {
        let done = self.cycles_in(now.saturating_duration_since(origin));
        let edge = if now < origin { 1 } else { done + 1 };
        origin + self.cycles(edge)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn cycles_and_cycles_in_agree() {
        for hz in [
            1,
            3,
            7,
            128,
            475,
            860,
            100_000,
            400_000,
            32_768,
            ClockDomain::MAX_HZ,
        ] {
            for ppm in [-500_000, -100_000, -1, 0, 1, 100_000, 500_000] {
                let domain = ClockDomain::new(hz).unwrap();
                let domain = domain.with_deviation_ppm(ppm);
                for n in [0, 1, 2, 3, 10, 999, 1_000_003] {
                    let d = domain.cycles(n);
                    assert_eq!(domain.cycles_in(d), n, "hz {hz} ppm {ppm} n {n}");
                    if let Some(before) = d.checked_sub(Duration::from_nanos(1)) {
                        assert!(domain.cycles_in(before) < n, "hz {hz} ppm {ppm} n {n}");
                    }
                }
            }
        }
    }

    #[test]
    fn no_drift_over_many_cycles() {
        // 475 Hz has no whole-nanosecond period; a million periods must still be exact.
        let domain = ClockDomain::new(475).unwrap();
        let exact_ns = 1_000_000u128 * 1_000_000_000 / 475;
        let got = domain.cycles(1_000_000).as_nanos();
        assert!(got - exact_ns <= 1, "{got} vs {exact_ns}");
    }

    #[test]
    fn deviation_changes_the_period() {
        let domain = ClockDomain::new(1_000).unwrap();
        assert_eq!(domain.cycles(1), Duration::from_millis(1));
        // 10 % fast: 1 ms / 1.1, rounded up.
        assert_eq!(
            domain.with_deviation_ppm(100_000).cycles(1),
            Duration::from_nanos(909_091)
        );
        assert_eq!(
            domain.with_deviation_ppm(-100_000).cycles(1),
            Duration::from_nanos(1_111_112)
        );
        assert_eq!(domain.with_deviation_ppm(i32::MAX).deviation_ppm(), 500_000);
    }

    #[test]
    fn next_edge_counts_from_the_origin() {
        let domain = ClockDomain::new(1_000).unwrap();
        let origin = SimTime::from_nanos(500);
        assert_eq!(
            domain.next_edge(origin, SimTime::ZERO),
            SimTime::from_nanos(1_000_500)
        );
        assert_eq!(
            domain.next_edge(origin, origin),
            SimTime::from_nanos(1_000_500)
        );
        assert_eq!(
            domain.next_edge(origin, SimTime::from_nanos(1_000_500)),
            SimTime::from_nanos(2_000_500)
        );
        assert!(ClockDomain::new(0).is_none());
        assert!(ClockDomain::new(ClockDomain::MAX_HZ + 1).is_none());
    }

    #[test]
    fn virtual_clock_clones_share_time_and_never_go_back() {
        let clock = VirtualClock::new();
        let other = clock.clone();
        clock.advance(Duration::from_micros(3));
        assert_eq!(other.now(), SimTime::from_nanos(3_000));
        other.advance_to(SimTime::from_nanos(1_000));
        assert_eq!(clock.now(), SimTime::from_nanos(3_000));
        other.advance_to(SimTime::from_nanos(5_000));
        assert_eq!(clock.now(), SimTime::from_nanos(5_000));
    }
}
