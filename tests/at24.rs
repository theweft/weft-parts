// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! 24C32 model driven by the independent `eeprom24x` driver.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs
)]

use std::time::Duration;

use eeprom24x::{Eeprom24x, SlaveAddr, Storage};
use embedded_storage::{ReadStorage, Storage as _};
use weft_parts::sim::{At24, Clock, I2cBus, Message, Nack, VirtualClock};

const T_WR: Duration = Duration::from_millis(5);

fn bus_with_clock() -> (I2cBus, VirtualClock) {
    let clock = VirtualClock::new();
    let mut bus = I2cBus::with_clock(clock.clone());
    bus.attach(0x50, Box::new(At24::new(4096, 32).unwrap()))
        .unwrap();
    (bus, clock)
}

fn bus() -> I2cBus {
    bus_with_clock().0
}

#[test]
fn bytes_and_pages_round_trip() {
    let (mut bus, clock) = bus_with_clock();
    let mut eeprom = Eeprom24x::new_24x32(&mut bus, SlaveAddr::default());
    assert_eq!(eeprom.read_byte(0x123).unwrap(), 0xff, "erased");
    eeprom.write_byte(0x123, 0xab).unwrap();
    assert!(eeprom.read_byte(0x123).is_err(), "busy during t_WR");
    clock.advance(T_WR);
    assert_eq!(eeprom.read_byte(0x123).unwrap(), 0xab);
    eeprom.write_page(0x40, &[1, 2, 3, 4]).unwrap();
    clock.advance(T_WR);
    let mut data = [0u8; 6];
    eeprom.read_data(0x3f, &mut data).unwrap();
    assert_eq!(data, [0xff, 1, 2, 3, 4, 0xff]);
}

#[test]
fn storage_waits_out_each_page_with_the_virtual_clock() {
    let (mut bus, clock) = bus_with_clock();
    let eeprom = Eeprom24x::new_24x32(&mut bus, SlaveAddr::default());
    // The driver delays 5 ms after each page; the delay moves the bus's virtual time.
    let mut storage = Storage::new(eeprom, clock.clone());
    let data: Vec<u8> = (0..70).collect();
    storage.write(0x1f, &data).unwrap(); // four pages: 1 + 32 + 32 + 5 bytes
    let mut back = vec![0u8; 70];
    storage.read(0x1f, &mut back).unwrap();
    assert_eq!(back, data);
    assert!(clock.now().as_nanos() >= 4 * 5_000_000);
}

#[test]
fn acknowledge_polling_ends_with_the_write_cycle() {
    let (mut bus, clock) = bus_with_clock();
    bus.transfer(0x50, &mut [Message::Write(&[0x00, 0x00, 0x5a])])
        .unwrap();
    let written = clock.now();
    let mut polls = 0;
    while bus.transfer(0x50, &mut [Message::Write(&[])]) == Err(Nack) {
        polls += 1;
    }
    // Each poll is a start, the address byte and a stop: 11 bit times = 110 us at 100 kHz.
    assert!((44..=46).contains(&polls), "{polls} polls");
    let waited = clock.now().saturating_duration_since(written);
    assert!(waited >= T_WR && waited < T_WR + Duration::from_micros(250));
}

#[test]
fn address_only_write_starts_no_cycle() {
    let mut bus = bus();
    let mut data = [0u8; 1];
    bus.transfer(0x50, &mut [Message::Write(&[0x00, 0x10])])
        .unwrap();
    bus.transfer(0x50, &mut [Message::Read(&mut data)]).unwrap();
    assert_eq!(data, [0xff]);
    assert!(!bus.target::<At24>(0x50).unwrap().busy());
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
