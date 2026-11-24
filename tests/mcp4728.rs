// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! MCP4728 model driven by the independent `mcp4728` driver and by the Linux driver's transfers.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs
)]

use mcp4728::{
    Channel, ChannelState, GainMode, MCP4728, OutputEnableMode, PowerDownMode, VoltageReferenceMode,
};
use std::time::Duration;

use weft_parts::sim::{I2cBus, Mcp4728, Message, VirtualClock};

/// Datasheet maximum EEPROM write time.
const T_WRITE: Duration = Duration::from_millis(50);

fn bus_with_clock() -> (I2cBus, VirtualClock) {
    let clock = VirtualClock::new();
    let mut bus = I2cBus::with_clock(clock.clone());
    bus.attach(0x60, Box::new(Mcp4728::new())).unwrap();
    (bus, clock)
}

fn bus() -> I2cBus {
    bus_with_clock().0
}

fn status(bus: &mut I2cBus) -> u8 {
    let mut status = [0u8; 3];
    bus.transfer(0x60, &mut [Message::Read(&mut status)])
        .unwrap();
    status[0]
}

fn model(bus: &I2cBus) -> &Mcp4728 {
    bus.target::<Mcp4728>(0x60).unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn fast_write_and_read_back() {
    let mut bus = bus();
    let mut dac = MCP4728::new(&mut bus, 0x60);
    dac.fast_write(483, 279, 297, 4095).unwrap();
    let registers = dac.read().unwrap();
    assert_eq!(registers.channel_a_input.channel_state.value, 483);
    assert_eq!(registers.channel_d_input.channel_state.value, 4095);
    assert_eq!(
        registers.channel_a_eeprom.channel_state.value, 0,
        "fast write skips EEPROM"
    );
    // VDD reference (factory default), VDD = 3.3 V.
    assert!(close(
        model(&bus).output_voltage(3).unwrap(),
        3.3 * 4095.0 / 4096.0
    ));
}

#[test]
fn internal_reference_and_gain() {
    let mut bus = bus();
    let state = ChannelState {
        voltage_reference_mode: VoltageReferenceMode::Internal,
        power_down_mode: PowerDownMode::Normal,
        gain_mode: GainMode::TimesTwo,
        value: 1000,
    };
    MCP4728::new(&mut bus, 0x60)
        .multi_write(&[(Channel::B, OutputEnableMode::Update, state)])
        .unwrap();
    assert!(close(
        model(&bus).output_voltage(1).unwrap(),
        2.048 * 1000.0 / 4096.0 * 2.0
    ));
    MCP4728::new(&mut bus, 0x60)
        .write_power_down_mode(
            PowerDownMode::Normal,
            PowerDownMode::PowerDownOneK,
            PowerDownMode::Normal,
            PowerDownMode::Normal,
        )
        .unwrap();
    assert_eq!(model(&bus).output_voltage(1), None);
}

#[test]
fn output_waits_for_ldac_or_software_update() {
    let mut bus = bus();
    bus.target_mut::<Mcp4728>(0x60).unwrap().set_ldac(false);
    let state = ChannelState {
        value: 2048,
        ..ChannelState::new()
    };
    {
        let mut dac = MCP4728::new(&mut bus, 0x60);
        dac.multi_write(&[(Channel::A, OutputEnableMode::NoUpdate, state)])
            .unwrap();
    }
    assert!(
        close(model(&bus).output_voltage(0).unwrap(), 0.0),
        "held until update"
    );
    {
        let mut dac = MCP4728::new(&mut bus, 0x60);
        dac.general_call_software_update().unwrap();
    }
    assert!(close(model(&bus).output_voltage(0).unwrap(), 1.024));
}

#[test]
fn eeprom_survives_general_call_reset() {
    let (mut bus, clock) = bus_with_clock();
    let mut dac = MCP4728::new(&mut bus, 0x60);
    let state = ChannelState {
        value: 100,
        ..ChannelState::new()
    };
    dac.single_write(Channel::C, OutputEnableMode::Update, &state)
        .unwrap();
    clock.advance(T_WRITE);
    dac.fast_write(0, 0, 0, 0).unwrap();
    assert_eq!(dac.read().unwrap().channel_c_input.channel_state.value, 0);
    dac.general_call_reset().unwrap();
    assert_eq!(dac.read().unwrap().channel_c_input.channel_state.value, 100);
}

/// @relation(PRT-010, scope=function)
#[test]
fn linux_driver_transfers() {
    // drivers/iio/dac/mcp4728.c, mcp4728_program_channel_cfg: multi-write, 3 bytes.
    let mut bus = bus();
    let channel = 2u8;
    let value: u16 = 0x0abc;
    let [high, low] = value.to_be_bytes();
    let out = [(0x08 << 3) | (channel << 1), 0x80 | (1 << 4) | high, low];
    bus.transfer(0x60, &mut [Message::Write(&out)]).unwrap();

    // mcp4728_init_channels_data: 24-byte read, value at inbuf[i * 6 + 1..=2].
    let mut inbuf = [0u8; 24];
    bus.transfer(0x60, &mut [Message::Read(&mut inbuf)])
        .unwrap();
    let i = usize::from(channel);
    assert_eq!(
        u16::from(inbuf[i * 6 + 1] & 0x0f) << 8 | u16::from(inbuf[i * 6 + 2]),
        0x0abc
    );
    assert_eq!(inbuf[i * 6 + 1] >> 7, 1, "internal reference");

    // mcp4728_program_eeprom: sequential write from channel A with all four channels, then
    // poll 3 bytes until RDY.
    let mut sw = vec![0x0a << 3];
    for v in [1u16, 2, 3, 4] {
        sw.extend(v.to_be_bytes());
    }
    bus.transfer(0x60, &mut [Message::Write(&sw)]).unwrap();
    let mut polls = 0;
    while status(&mut bus) & 0x80 == 0 {
        polls += 1;
    }
    // A 3-byte read is 38 bit times (380 us at 100 kHz): about 50 ms / 380 us polls.
    assert!((125..=135).contains(&polls), "{polls} polls");
    assert_eq!(model(&bus).eeprom(3).unwrap().value, 4);
}

/// @relation(PRT-010, scope=function)
#[test]
fn commands_during_eeprom_programming_are_ignored() {
    let (mut bus, clock) = bus_with_clock();
    let state = ChannelState {
        value: 700,
        ..ChannelState::new()
    };
    MCP4728::new(&mut bus, 0x60)
        .single_write(Channel::A, OutputEnableMode::Update, &state)
        .unwrap();
    assert!(
        model(&bus).rdy_pulls_low(),
        "RDY/BSY pin low while programming"
    );
    assert_eq!(status(&mut bus) & 0x80, 0, "RDY bit low");
    assert_eq!(
        model(&bus).eeprom(0).unwrap().value,
        0,
        "not programmed yet"
    );
    // Acknowledged but dropped.
    MCP4728::new(&mut bus, 0x60).fast_write(1, 2, 3, 4).unwrap();
    assert_eq!(model(&bus).input(1).unwrap().value, 0);
    MCP4728::new(&mut bus, 0x60).general_call_reset().unwrap();
    assert_eq!(
        model(&bus).input(0).unwrap().value,
        700,
        "reset ignored too"
    );

    clock.advance(T_WRITE);
    assert_eq!(status(&mut bus) & 0x80, 0x80);
    assert!(!model(&bus).rdy_pulls_low());
    assert_eq!(model(&bus).eeprom(0).unwrap().value, 700);
    MCP4728::new(&mut bus, 0x60).fast_write(1, 2, 3, 4).unwrap();
    assert_eq!(model(&bus).input(1).unwrap().value, 2);
}

/// @relation(PRT-010, scope=function)
#[test]
fn sequential_write_stopped_early_programs_what_it_got() {
    let (mut bus, clock) = bus_with_clock();
    // Sequential write from channel B, but only B and C are sent.
    let mut sw = vec![(0x0a << 3) | (1 << 1)];
    for v in [11u16, 22] {
        sw.extend(v.to_be_bytes());
    }
    bus.transfer(0x60, &mut [Message::Write(&sw)]).unwrap();
    assert!(model(&bus).busy());
    clock.advance(T_WRITE);
    bus.sync();
    assert!(!model(&bus).busy());
    let eeprom: Vec<u16> = (0..4)
        .map(|c| model(&bus).eeprom(c).unwrap().value)
        .collect();
    assert_eq!(eeprom, [0, 11, 22, 0]);
}

#[test]
fn unsupported_command_is_not_acknowledged() {
    let mut bus = bus();
    // Write address bits (0b011...) is not modelled.
    assert!(
        bus.transfer(0x60, &mut [Message::Write(&[0x61, 0x66, 0x6f])])
            .is_err()
    );
}
