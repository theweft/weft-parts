// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Bus semantics shared by all models.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs
)]

use embedded_hal::i2c::{I2c, Operation};
use weft_parts::sim::{AddressInUse, At24, I2cBus, Message, Nack};

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
    // Linux semantics: the second message starts again, so 0x66 is an address byte.
    bus.transfer(
        0x50,
        &mut [Message::Write(&[0x00, 0x11]), Message::Write(&[0x66])],
    )
    .unwrap();
    assert_eq!(bus.target::<At24>(0x50).unwrap().memory()[0x11], 0xff);
}
