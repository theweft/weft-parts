// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! ADS1115 model driven by the independent `ads1x1x` driver and by the Linux driver's transfers.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs
)]

use std::time::Duration;

use ads1x1x::{Ads1x1x, DataRate16Bit, FullScaleRange, TargetAddr, channel};
use weft_parts::sim::{ALERT_RESPONSE, Ads1115, Clock, I2cBus, Message, SimTime, VirtualClock};

fn bus_with_clock(a0: f64, a1: f64) -> (I2cBus, VirtualClock) {
    let mut adc = Ads1115::new();
    adc.set_input(0, a0);
    adc.set_input(1, a1);
    let clock = VirtualClock::new();
    let mut bus = I2cBus::with_clock(clock.clone());
    bus.attach(0x48, Box::new(adc)).unwrap();
    (bus, clock)
}

fn bus(a0: f64, a1: f64) -> I2cBus {
    bus_with_clock(a0, a1).0
}

fn read_register(bus: &mut I2cBus, pointer: u8) -> u16 {
    let mut value = [0u8; 2];
    bus.transfer(
        0x48,
        &mut [Message::Write(&[pointer]), Message::Read(&mut value)],
    )
    .unwrap();
    u16::from_be_bytes(value)
}

/// 128 samples per second (the reset DR setting).
const PERIOD_128: Duration = Duration::from_nanos(7_812_500);

/// @relation(PRT-007, scope=function)
#[test]
fn one_shot_reads_with_default_gain() {
    let (mut bus, clock) = bus_with_clock(1.0, 0.25);
    let mut adc = Ads1x1x::new_ads1115(&mut bus, TargetAddr::default());
    // 1.0 V at +-2.048 V full scale: 1.0 / 2.048 * 32768.
    assert_eq!(nb::block!(adc.read(channel::SingleA0)).unwrap(), 16000);
    // The driver polled OS until the conversion finished: at least one period passed.
    assert!(clock.now() >= SimTime::ZERO + PERIOD_128);
    assert_eq!(
        nb::block!(adc.read(channel::DifferentialA0A1)).unwrap(),
        12000
    );
    assert!(clock.now() >= SimTime::ZERO + 2 * PERIOD_128);
}

#[test]
fn faster_data_rate_finishes_sooner() {
    let (mut bus, clock) = bus_with_clock(1.0, 0.0);
    let mut adc = Ads1x1x::new_ads1115(&mut bus, TargetAddr::default());
    adc.set_data_rate(DataRate16Bit::Sps860).unwrap();
    let before = clock.now();
    assert_eq!(nb::block!(adc.read(channel::SingleA0)).unwrap(), 16000);
    let took = clock.now().saturating_duration_since(before);
    // At least 1/860 s = 1.163 ms; the driver's own reads at 100 kHz (about 0.5 ms each) add
    // the rest, but it stays well below one period at the default 128 SPS.
    assert!(took >= Duration::from_nanos(1_162_791), "{took:?}");
    assert!(took < PERIOD_128 / 2, "{took:?}");
}

#[test]
fn os_reads_zero_while_converting() {
    let (mut bus, clock) = bus_with_clock(0.5, 0.0);
    // Single-shot, AIN0, default gain and 128 SPS, OS = 1: start a conversion.
    bus.transfer(0x48, &mut [Message::Write(&[0x01, 0xc5, 0x83])])
        .unwrap();
    let started = clock.now();
    assert_eq!(read_register(&mut bus, 0x01) & 0x8000, 0, "converting");
    assert_eq!(read_register(&mut bus, 0x00), 0, "no result yet");
    clock.advance_to(started + PERIOD_128);
    assert_eq!(read_register(&mut bus, 0x01) & 0x8000, 0x8000, "done");
    assert_eq!(read_register(&mut bus, 0x00), 8000);
}

/// @relation(PRT-007, scope=function)
#[test]
fn rate_deviation_moves_the_end_of_a_conversion() {
    for (ppm, done_after) in [(100_000, 7_102_273), (-100_000, 8_680_556)] {
        let (mut bus, clock) = bus_with_clock(0.5, 0.0);
        bus.target_mut::<Ads1115>(0x48)
            .unwrap()
            .set_rate_deviation_ppm(ppm);
        bus.transfer(0x48, &mut [Message::Write(&[0x01, 0xc5, 0x83])])
            .unwrap();
        // The conversion starts when the last config byte is in, before the stop bit.
        let bus_target = bus.target::<Ads1115>(0x48).unwrap();
        assert_eq!(
            bus_target.conversion_time(),
            Duration::from_nanos(done_after),
            "{ppm} ppm"
        );
        let done_at = bus.next_event().unwrap();
        clock.advance_to(done_at);
        assert_eq!(read_register(&mut bus, 0x00), 8000, "{ppm} ppm");
    }
}

#[test]
fn gain_changes_scale_and_clamps() {
    let mut bus = bus(1.0, 0.0);
    let mut adc = Ads1x1x::new_ads1115(&mut bus, TargetAddr::default());
    adc.set_full_scale_range(FullScaleRange::Within4_096V)
        .unwrap();
    assert_eq!(nb::block!(adc.read(channel::SingleA0)).unwrap(), 8000);
    adc.set_full_scale_range(FullScaleRange::Within0_256V)
        .unwrap();
    assert_eq!(nb::block!(adc.read(channel::SingleA0)).unwrap(), i16::MAX);
}

#[test]
fn continuous_mode_follows_the_input() {
    let (mut bus, clock) = bus_with_clock(0.5, 0.0);
    {
        let adc = Ads1x1x::new_ads1115(&mut bus, TargetAddr::default());
        let mut adc = adc.into_continuous().ok().unwrap();
        adc.select_channel(channel::SingleA0).unwrap();
        assert_eq!(adc.read().unwrap(), 0, "no conversion completed yet");
        clock.advance(PERIOD_128);
        assert_eq!(adc.read().unwrap(), 8000);
    }
    bus.target_mut::<Ads1115>(0x48).unwrap().set_input(0, -0.5);
    let old = i16::from_be_bytes(read_register(&mut bus, 0x00).to_be_bytes());
    assert_eq!(old, 8000, "the last completed conversion");
    clock.advance(PERIOD_128);
    let new = i16::from_be_bytes(read_register(&mut bus, 0x00).to_be_bytes());
    assert_eq!(new, -8000);
}

#[test]
fn linux_regmap_transfers() {
    // ti-ads1015: regmap with 8-bit register and 16-bit big-endian value.
    let mut bus = bus(0.0, 0.0);
    let mut config = [0u8; 2];
    bus.transfer(
        0x48,
        &mut [Message::Write(&[0x01]), Message::Read(&mut config)],
    )
    .unwrap();
    assert_eq!(u16::from_be_bytes(config), 0x8583, "reset value");
    // Thresholds keep what is written.
    bus.transfer(0x48, &mut [Message::Write(&[0x03, 0x12, 0x34])])
        .unwrap();
    let mut hi = [0u8; 2];
    bus.transfer(0x48, &mut [Message::Write(&[0x03]), Message::Read(&mut hi)])
        .unwrap();
    assert_eq!(hi, [0x12, 0x34]);
}

#[test]
fn wrong_address_is_not_acknowledged() {
    let mut bus = bus(0.0, 0.0);
    let mut adc = Ads1x1x::new_ads1115(&mut bus, TargetAddr::Vdd);
    assert!(nb::block!(adc.read(channel::SingleA0)).is_err());
}

fn write_register(bus: &mut I2cBus, address: u16, pointer: u8, value: u16) {
    let [high, low] = value.to_be_bytes();
    bus.transfer(address, &mut [Message::Write(&[pointer, high, low])])
        .unwrap();
}

/// Moves time to the next model event and lets the models catch up.
fn step(bus: &mut I2cBus, clock: &VirtualClock) -> SimTime {
    let t = bus.next_event().unwrap();
    clock.advance_to(t);
    bus.sync();
    t
}

fn adc(bus: &I2cBus) -> &Ads1115 {
    bus.target::<Ads1115>(0x48).unwrap()
}

/// Continuous, AIN0 single-ended, +-2.048 V, 128 SPS; comparator bits in `comp`.
const CONT_AIN0: u16 = 0x4480;
const CONT_AIN1: u16 = 0x5480;

/// @relation(PRT-007, scope=function)
#[test]
fn single_shot_waits_for_wake_up() {
    let mut bus = bus(0.5, 0.0);
    write_register(&mut bus, 0x48, 0x01, 0xc583);
    // The conversion starts when the last config byte is in: 10 + 27 bit times = 370 us.
    let done = bus.next_event().unwrap();
    assert_eq!(
        done,
        SimTime::from_nanos(370_000) + Duration::from_micros(25) + PERIOD_128
    );
}

/// @relation(PRT-007, scope=function)
#[test]
fn os_reads_zero_in_continuous_mode() {
    let (mut bus, _clock) = bus_with_clock(0.5, 0.0);
    write_register(&mut bus, 0x48, 0x01, CONT_AIN0 | 0b11);
    assert_eq!(read_register(&mut bus, 0x01) & 0x8000, 0);
}

/// @relation(PRT-007, scope=function)
#[test]
fn continuous_config_change_applies_after_the_running_conversion() {
    let (mut bus, clock) = bus_with_clock(0.5, 0.25);
    write_register(&mut bus, 0x48, 0x01, CONT_AIN0 | 0b11);
    step(&mut bus, &clock);
    assert_eq!(adc(&bus).conversion(), 8000);
    // Switch to AIN1 while the second conversion runs: it still converts AIN0.
    write_register(&mut bus, 0x48, 0x01, CONT_AIN1 | 0b11);
    step(&mut bus, &clock);
    assert_eq!(adc(&bus).conversion(), 8000, "old settings finish");
    step(&mut bus, &clock);
    assert_eq!(
        adc(&bus).conversion(),
        4000,
        "new settings from the next one"
    );
}

/// @relation(PRT-008, scope=function)
#[test]
fn traditional_comparator_has_hysteresis() {
    let (mut bus, clock) = bus_with_clock(0.5, 0.0);
    write_register(&mut bus, 0x48, 0x02, 4000);
    write_register(&mut bus, 0x48, 0x03, 7000);
    write_register(&mut bus, 0x48, 0x01, CONT_AIN0); // COMP_QUE = 00: one conversion
    assert!(!adc(&bus).alert_pulls_low());
    step(&mut bus, &clock);
    assert!(
        adc(&bus).alert_asserted() && adc(&bus).alert_pulls_low(),
        "8000 > hi"
    );
    bus.target_mut::<Ads1115>(0x48).unwrap().set_input(0, 0.3);
    step(&mut bus, &clock);
    assert!(
        adc(&bus).alert_asserted(),
        "4800 is between the thresholds: stays"
    );
    bus.target_mut::<Ads1115>(0x48).unwrap().set_input(0, 0.2);
    step(&mut bus, &clock);
    assert!(!adc(&bus).alert_asserted(), "3200 < lo: deasserts");
}

/// @relation(PRT-008, scope=function)
#[test]
fn comparator_queue_polarity_and_disable() {
    let (mut bus, clock) = bus_with_clock(0.5, 0.0);
    write_register(&mut bus, 0x48, 0x03, 7000);
    // Active high (COMP_POL), assert after two conversions (COMP_QUE = 01).
    write_register(&mut bus, 0x48, 0x01, CONT_AIN0 | 0b1000 | 0b01);
    assert!(
        adc(&bus).alert_pulls_low(),
        "active high, deasserted: pulled low"
    );
    step(&mut bus, &clock);
    assert!(!adc(&bus).alert_asserted(), "one conversion is not enough");
    step(&mut bus, &clock);
    assert!(adc(&bus).alert_asserted() && !adc(&bus).alert_pulls_low());
    // COMP_QUE = 11 disables the comparator: high-impedance.
    write_register(&mut bus, 0x48, 0x01, CONT_AIN0 | 0b1000 | 0b11);
    assert!(!adc(&bus).alert_asserted() && !adc(&bus).alert_pulls_low());
}

/// @relation(PRT-002, scope=function)
/// @relation(PRT-008, scope=function)
#[test]
fn latched_alerts_clear_by_reading_or_alert_response() {
    let clock = VirtualClock::new();
    let mut bus = I2cBus::with_clock(clock.clone());
    for address in [0x48, 0x49] {
        let mut adc = Ads1115::new();
        adc.set_input(0, 0.5);
        bus.attach(address, Box::new(adc)).unwrap();
        write_register(&mut bus, address, 0x02, 0x8000); // window low threshold: none
        write_register(&mut bus, address, 0x03, 7000);
        // Window mode, latching, one conversion.
        write_register(&mut bus, address, 0x01, CONT_AIN0 | 0b1_0100);
    }
    step(&mut bus, &clock);
    step(&mut bus, &clock);
    for address in [0x48, 0x49] {
        bus.target_mut::<Ads1115>(address)
            .unwrap()
            .set_input(0, 0.1);
    }
    step(&mut bus, &clock);
    step(&mut bus, &clock);
    assert!(adc(&bus).alert_asserted(), "latched although back in range");

    // The lowest address wins the alert response; status bit 1: high threshold.
    let mut byte = [0u8; 1];
    bus.transfer(ALERT_RESPONSE, &mut [Message::Read(&mut byte)])
        .unwrap();
    assert_eq!(byte, [(0x48 << 1) | 1]);
    assert!(!adc(&bus).alert_asserted());
    // 0x49 is still latched; reading its conversion register clears it instead.
    let _ = read_register_at(&mut bus, 0x49, 0x00);
    assert!(!bus.target::<Ads1115>(0x49).unwrap().alert_asserted());
    assert_eq!(
        bus.transfer(ALERT_RESPONSE, &mut [Message::Read(&mut byte)]),
        Err(weft_parts::sim::Nack)
    );
}

fn read_register_at(bus: &mut I2cBus, address: u16, pointer: u8) -> u16 {
    let mut value = [0u8; 2];
    bus.transfer(
        address,
        &mut [Message::Write(&[pointer]), Message::Read(&mut value)],
    )
    .unwrap();
    u16::from_be_bytes(value)
}

/// @relation(PRT-008, scope=function)
#[test]
fn conversion_ready_pulses_for_eight_microseconds() {
    let (mut bus, clock) = bus_with_clock(0.5, 0.0);
    write_register(&mut bus, 0x48, 0x02, 0x0000); // Lo_thresh MSB 0
    write_register(&mut bus, 0x48, 0x03, 0x8000); // Hi_thresh MSB 1: conversion-ready pin
    write_register(&mut bus, 0x48, 0x01, CONT_AIN0);
    let done = step(&mut bus, &clock);
    assert!(adc(&bus).alert_pulls_low(), "pulse starts");
    assert_eq!(bus.next_event(), Some(done + Duration::from_micros(8)));
    step(&mut bus, &clock);
    assert!(!adc(&bus).alert_pulls_low(), "pulse over");
}
