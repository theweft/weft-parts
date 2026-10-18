// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! ADS1115 model driven by the independent `ads1x1x` driver and by the Linux driver's transfers.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs
)]

use ads1x1x::{Ads1x1x, FullScaleRange, TargetAddr, channel};
use weft_parts::sim::{Ads1115, I2cBus, Message};

fn bus(a0: f64, a1: f64) -> I2cBus {
    let mut adc = Ads1115::new();
    adc.set_input(0, a0);
    adc.set_input(1, a1);
    let mut bus = I2cBus::new();
    bus.attach(0x48, Box::new(adc)).unwrap();
    bus
}

#[test]
fn one_shot_reads_with_default_gain() {
    let mut bus = bus(1.0, 0.25);
    let mut adc = Ads1x1x::new_ads1115(&mut bus, TargetAddr::default());
    // 1.0 V at +-2.048 V full scale: 1.0 / 2.048 * 32768.
    assert_eq!(nb::block!(adc.read(channel::SingleA0)).unwrap(), 16000);
    assert_eq!(
        nb::block!(adc.read(channel::DifferentialA0A1)).unwrap(),
        12000
    );
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
    let mut bus = bus(0.5, 0.0);
    {
        let adc = Ads1x1x::new_ads1115(&mut bus, TargetAddr::default());
        let mut adc = adc.into_continuous().ok().unwrap();
        adc.select_channel(channel::SingleA0).unwrap();
        assert_eq!(adc.read().unwrap(), 8000);
    }
    bus.target_mut::<Ads1115>(0x48).unwrap().set_input(0, -0.5);
    let mut value = [0u8; 2];
    bus.transfer(
        0x48,
        &mut [Message::Write(&[0x00]), Message::Read(&mut value)],
    )
    .unwrap();
    assert_eq!(i16::from_be_bytes(value), -8000);
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
