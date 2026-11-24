// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Clock domains and clocks through the public API.
#![allow(clippy::unwrap_used, missing_docs)]

use std::time::Duration;

use weft_parts::sim::{Clock, ClockDomain, SimTime, VirtualClock};

/// @relation(PRT-006, scope=function)
#[test]
fn edges_round_trip_and_do_not_drift() {
    for hz in [1, 7, 128, 475, 860, 100_000, 32_768] {
        for ppm in [-100_000, 0, 20, 100_000] {
            let domain = ClockDomain::new(hz).unwrap().with_deviation_ppm(ppm);
            for n in [0, 1, 3, 1_000_003] {
                assert_eq!(domain.cycles_in(domain.cycles(n)), n, "{hz} Hz {ppm} ppm");
            }
        }
    }
    // A million periods at 475 Hz (no whole-nanosecond period) are within 1 ns of exact.
    let exact = 1_000_000u128 * 1_000_000_000 / 475;
    let got = ClockDomain::new(475).unwrap().cycles(1_000_000).as_nanos();
    assert!(got - exact <= 1);
}

/// @relation(PRT-006, scope=function)
#[test]
fn next_edge_counts_from_the_origin() {
    let domain = ClockDomain::new(1_000).unwrap();
    let origin = SimTime::from_nanos(500);
    assert_eq!(
        domain.next_edge(origin, SimTime::from_nanos(1_000_500)),
        SimTime::from_nanos(2_000_500)
    );
}

/// @relation(PRT-004, scope=function)
#[test]
fn virtual_clock_is_shared_and_monotonic() {
    let clock = VirtualClock::new();
    let other = clock.clone();
    clock.advance(Duration::from_micros(3));
    other.advance_to(SimTime::from_nanos(1_000));
    assert_eq!(clock.now(), SimTime::from_nanos(3_000));
}
