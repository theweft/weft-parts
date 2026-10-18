// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! 24C32 model driven by the independent `eeprom24x` driver.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs
)]

use eeprom24x::{Eeprom24x, SlaveAddr};
use weft_parts::sim::{At24, I2cBus, Message};

fn bus() -> I2cBus {
    let mut bus = I2cBus::new();
    bus.attach(0x50, Box::new(At24::new(4096, 32).unwrap()))
        .unwrap();
    bus
}

#[test]
fn bytes_and_pages_round_trip() {
    let mut bus = bus();
    let mut eeprom = Eeprom24x::new_24x32(&mut bus, SlaveAddr::default());
    assert_eq!(eeprom.read_byte(0x123).unwrap(), 0xff, "erased");
    eeprom.write_byte(0x123, 0xab).unwrap();
    assert_eq!(eeprom.read_byte(0x123).unwrap(), 0xab);
    eeprom.write_page(0x40, &[1, 2, 3, 4]).unwrap();
    let mut data = [0u8; 6];
    eeprom.read_data(0x3f, &mut data).unwrap();
    assert_eq!(data, [0xff, 1, 2, 3, 4, 0xff]);
}

#[test]
fn page_write_wraps_within_the_page() {
    let mut bus = bus();
    // Two bytes starting at the last byte of page 0: the second lands at offset 0 of page 0.
    bus.transfer(0x50, &mut [Message::Write(&[0x00, 0x1f, 0xaa, 0xbb])])
        .unwrap();
    let memory = bus.target::<At24>(0x50).unwrap().memory();
    assert_eq!(
        (memory[0x1f], memory[0x00], memory[0x20]),
        (0xaa, 0xbb, 0xff)
    );
}

#[test]
fn sequential_read_wraps_at_the_end() {
    let mut bus = bus();
    bus.target_mut::<At24>(0x50).unwrap().memory_mut()[0] = 0x42;
    let mut data = [0u8; 2];
    bus.transfer(
        0x50,
        &mut [Message::Write(&[0x0f, 0xff]), Message::Read(&mut data)],
    )
    .unwrap();
    assert_eq!(data, [0xff, 0x42]);
}
