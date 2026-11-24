// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Bus semantics shared by all models.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs
)]

use std::time::Duration;

use embedded_hal::i2c::{Error as _, ErrorKind, NoAcknowledgeSource};
use embedded_hal::i2c::{I2c, Operation};
use weft_parts::sim::{
    ALERT_RESPONSE, AddressInUse, At24, Clock, ClockDomain, I2cBus, Message, Nack, SimTime,
    VirtualClock,
};

#[test]
fn address_conflicts_and_unknown_targets() {
    let mut bus = I2cBus::new();
    bus.attach(0x50, Box::new(At24::new(4096, 32).unwrap()))
        .unwrap();
    assert_eq!(
        bus.attach(0x50, Box::new(At24::new(4096, 32).unwrap())),
        Err(AddressInUse(0x50))
    );
    assert_eq!(bus.transfer(0x51, &mut [Message::Write(&[0])]), Err(Nack));
}

/// @relation(PRT-002, scope=function)
#[test]
fn embedded_hal_adjacent_writes_are_one_message() {
    let mut bus = I2cBus::new();
    bus.attach(0x50, Box::new(At24::new(4096, 32).unwrap()))
        .unwrap();
    // Address in one operation, data in the next: no repeated start, so the data is written.
    bus.transaction(
        0x50,
        &mut [Operation::Write(&[0x00, 0x10]), Operation::Write(&[0x77])],
    )
    .unwrap();
    assert_eq!(bus.target::<At24>(0x50).unwrap().memory()[0x10], 0x77);
    // Wait out the write cycle.
    bus.sync();
    while bus.target::<At24>(0x50).unwrap().busy() {
        bus.transfer(0x50, &mut [Message::Write(&[])]).ok();
    }
    // Linux semantics: the second message starts again, so 0x66 is an address byte.
    bus.transfer(
        0x50,
        &mut [Message::Write(&[0x00, 0x11]), Message::Write(&[0x66])],
    )
    .unwrap();
    assert_eq!(bus.target::<At24>(0x50).unwrap().memory()[0x11], 0xff);
}

/// @relation(PRT-005, scope=function)
#[test]
fn transfers_take_their_bit_time_and_move_the_clock() {
    let clock = VirtualClock::new();
    let mut bus = I2cBus::with_clock(clock.clone());
    bus.attach(0x50, Box::new(At24::new(4096, 32).unwrap()))
        .unwrap();
    let mut data = [0u8; 2];
    // Start + address (10), 2 bytes (18), repeated start + address (10), 2 bytes (18), stop (1).
    bus.transfer(
        0x50,
        &mut [Message::Write(&[0x00, 0x00]), Message::Read(&mut data)],
    )
    .unwrap();
    assert_eq!(clock.now(), SimTime::from_nanos(57 * 10_000));
    bus.set_speed(ClockDomain::new(400_000).unwrap());
    bus.transfer(0x50, &mut [Message::Read(&mut data)]).unwrap();
    // 10 + 18 + 1 bits at 2.5 us.
    assert_eq!(clock.now(), SimTime::from_nanos(570_000 + 29 * 2_500));
    // An unknown address still takes start, address and stop.
    assert_eq!(bus.transfer(0x51, &mut [Message::Write(&[0])]), Err(Nack));
    assert_eq!(
        clock.now(),
        SimTime::from_nanos(570_000 + 29 * 2_500 + 11 * 2_500)
    );
}

/// @relation(PRT-004, scope=function)
#[test]
fn a_transfer_never_starts_before_the_previous_one_ended() {
    // A clock that stands still (like real time between two quick transfers).
    #[derive(Debug)]
    struct Frozen;
    impl Clock for Frozen {
        fn now(&self) -> SimTime {
            SimTime::ZERO
        }
    }
    let mut bus = I2cBus::with_clock(Frozen);
    bus.attach(0x50, Box::new(At24::new(4096, 32).unwrap()))
        .unwrap();
    // A write cycle started at the first stop ends 5 ms later, measured on the bus's own timeline.
    bus.transfer(0x50, &mut [Message::Write(&[0x00, 0x00, 0x01])])
        .unwrap();
    // Start + address (10), three bytes (27), stop (1): the cycle starts at 380 us.
    let stop = SimTime::from_nanos(38 * 10_000);
    assert_eq!(bus.next_event(), Some(stop + Duration::from_millis(5)));
    // The next transfer starts where the first ended, not at the frozen clock's zero.
    let mut data = [0u8; 1];
    assert_eq!(
        bus.transfer(0x50, &mut [Message::Read(&mut data)]),
        Err(Nack)
    );
}

/// @relation(PRT-003, scope=function)
#[test]
fn embedded_hal_says_which_byte_was_not_acknowledged() {
    let mut bus = I2cBus::new();
    bus.attach(0x60, Box::new(weft_parts::sim::Mcp4728::new()))
        .unwrap();
    let kind = |e: weft_parts::sim::HalNack| e.kind();
    let missing = bus.write(0x61, &[0]).unwrap_err();
    assert_eq!(
        kind(missing),
        ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
    );
    // Write address bits (0b011...) is not modelled: the command byte is rejected.
    let rejected = bus.write(0x60, &[0x61, 0x66, 0x6f]).unwrap_err();
    assert_eq!(
        kind(rejected),
        ErrorKind::NoAcknowledge(NoAcknowledgeSource::Data)
    );
    assert!(!rejected.address());
}

/// @relation(PRT-002, scope=function)
#[test]
fn alert_response_address_is_reserved() {
    let mut bus = I2cBus::new();
    assert_eq!(
        bus.attach(ALERT_RESPONSE, Box::new(At24::new(4096, 32).unwrap())),
        Err(AddressInUse(ALERT_RESPONSE))
    );
    // Nobody alerting: the response is not acknowledged.
    let mut byte = [0u8; 1];
    assert_eq!(
        bus.transfer(ALERT_RESPONSE, &mut [Message::Read(&mut byte)]),
        Err(Nack)
    );
}
