// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Board simulation from a KiCad netlist: models placed by part number, buses by nets,
//! addresses from straps, and the ALERT, RDY, LDAC and WP pins on simulated nets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs
)]

use std::time::Duration;

use weft_parts::Library;
use weft_parts::board::{Board, Rails, kicad};
use weft_parts::sim::{
    Ads1115, At24, BoardSim, Clock, Drive, Mcp4728, Message, NetState, SimTime, VirtualClock,
};

fn hat() -> Board {
    kicad::parse(include_str!("fixtures/hat.net"), &[]).unwrap()
}

fn sim_of(board: &Board) -> (BoardSim, VirtualClock) {
    let library = Library::builtin().unwrap();
    let rails = Rails::from_parts(board, &library);
    let clock = VirtualClock::new();
    let sim = BoardSim::new(board, &library, &rails, clock.clone()).unwrap();
    (sim, clock)
}

fn write(sim: &mut BoardSim, address: u16, bytes: &[u8]) {
    sim.transfer("SDA", address, &mut [Message::Write(bytes)])
        .unwrap();
}

/// @relation(PRT-012, scope=function)
#[test]
fn chips_are_placed_from_the_netlist() {
    let (sim, _) = sim_of(&hat());
    assert_eq!(sim.placement("U1"), Some(("/SDA", 0x48)), "ADDR to GND");
    assert_eq!(sim.placement("U2"), Some(("/SDA", 0x50)), "A2..A0 to GND");
    assert_eq!(sim.placement("U3"), Some(("/SDA", 0x60)), "factory default");
    assert_eq!(sim.placement("R1"), None, "passives have no model");
    assert!(sim.skipped().is_empty(), "{:?}", sim.skipped());
    // Pulls from the resistors to the rails.
    assert_eq!(sim.net("ALERT"), Some(NetState::High), "R1 to +3V3");
    assert_eq!(sim.net("RDY"), Some(NetState::High), "R2 to +3V3");
    assert_eq!(sim.net("EEPROM_WP"), Some(NetState::Low), "R3 to GND");
    assert_eq!(sim.net("LDAC"), Some(NetState::High), "R6 to +3V3");
    assert!(sim.findings().is_empty(), "{:?}", sim.findings());
}

/// @relation(PRT-012, scope=function)
#[test]
fn alert_goes_low_when_the_conversion_crosses_the_threshold() {
    let (mut sim, _) = sim_of(&hat());
    sim.chip_mut::<Ads1115>("U1").unwrap().set_input(0, 0.5);
    write(&mut sim, 0x48, &[0x02, 0x0f, 0xa0]); // Lo_thresh 4000
    write(&mut sim, 0x48, &[0x03, 0x1b, 0x58]); // Hi_thresh 7000
    write(&mut sim, 0x48, &[0x01, 0x44, 0x80]); // continuous AIN0, comparator after 1
    let done = sim.bus("SDA").unwrap().next_event().unwrap();
    sim.run_until(done + Duration::from_millis(1));
    assert_eq!(sim.net("ALERT"), Some(NetState::Low));
    let edges = sim.edges_of("ALERT");
    let last = edges.last().unwrap();
    assert_eq!(
        (last.at, last.state),
        (done, NetState::Low),
        "at the conversion's end"
    );
}

/// @relation(PRT-012, scope=function)
#[test]
fn rdy_is_low_while_the_dac_programs_its_eeprom() {
    let (mut sim, clock) = sim_of(&hat());
    // Single write to channel A with EEPROM: programming starts.
    write(&mut sim, 0x60, &[0x58, 0x08, 0x00]);
    assert_eq!(sim.net("RDY"), Some(NetState::Low));
    let ready = sim.bus("SDA").unwrap().next_event().unwrap();
    assert!(ready > clock.now() + Duration::from_millis(49));
    sim.run_until(ready + Duration::from_millis(1));
    let last = *sim.edges_of("RDY").last().unwrap();
    assert_eq!((last.at, last.state), (ready, NetState::High));
}

/// @relation(PRT-012, scope=function)
#[test]
fn ldac_from_the_host_updates_the_outputs() {
    let (mut sim, _) = sim_of(&hat());
    // Multi-write to channel A with UDAC = 1: the output waits for LDAC (pulled up by R6).
    write(&mut sim, 0x60, &[0x41, 0x08, 0x00]);
    assert!(
        sim.chip::<Mcp4728>("U3")
            .unwrap()
            .output_voltage(0)
            .unwrap()
            < 0.01
    );
    assert!(sim.drive("LDAC", Drive::Low));
    let volts = sim
        .chip::<Mcp4728>("U3")
        .unwrap()
        .output_voltage(0)
        .unwrap();
    assert!((volts - 1.65).abs() < 1e-9, "{volts}");
}

/// @relation(PRT-012, scope=function)
#[test]
fn write_protect_from_the_host() {
    let (mut sim, clock) = sim_of(&hat());
    assert!(sim.drive("EEPROM_WP", Drive::High));
    write(&mut sim, 0x50, &[0x00, 0x10, 0xab]);
    assert_eq!(
        sim.chip::<At24>("U2").unwrap().memory()[0x10],
        0xff,
        "protected"
    );
    // Released: R3 pulls WP low again.
    assert!(sim.drive("EEPROM_WP", Drive::Released));
    assert_eq!(sim.net("EEPROM_WP"), Some(NetState::Low));
    write(&mut sim, 0x50, &[0x00, 0x10, 0xab]);
    sim.run_until(clock.now() + Duration::from_millis(5));
    assert_eq!(sim.chip::<At24>("U2").unwrap().memory()[0x10], 0xab);
}

/// @relation(PRT-012, scope=function)
#[test]
fn open_drain_alerts_share_a_net() {
    let mut board = hat();
    // A second ADS1115 with ADDR to VDD (0x49) and its ALERT on the same net.
    board.set_mpn("U4", "ADS1115IDGSR");
    for (pin, net) in [
        ("1", "+3V3"),
        ("2", "/ALERT"),
        ("3", "GND"),
        ("8", "+3V3"),
        ("9", "/SDA"),
        ("10", "/SCL"),
    ] {
        board.connect(net, "U4", pin);
    }
    let (mut sim, _) = sim_of(&board);
    assert_eq!(sim.placement("U4"), Some(("/SDA", 0x49)));
    sim.chip_mut::<Ads1115>("U1").unwrap().set_input(0, 0.5);
    for address in [0x48, 0x49] {
        write(&mut sim, address, &[0x03, 0x1b, 0x58]); // Hi_thresh 7000
    }
    write(&mut sim, 0x48, &[0x01, 0x44, 0x80]); // U1 comparing; U4 disabled
    let t = sim.now() + Duration::from_millis(10);
    sim.run_until(t);
    assert_eq!(
        sim.net("ALERT"),
        Some(NetState::Low),
        "U1 pulls low, U4 released"
    );
    assert!(sim.findings().is_empty(), "{:?}", sim.findings());
}

/// @relation(PRT-012, scope=function)
#[test]
fn contention_and_floating_inputs_are_findings() {
    let (mut sim, _) = sim_of(&hat());
    sim.chip_mut::<Ads1115>("U1").unwrap().set_input(0, 0.5);
    write(&mut sim, 0x48, &[0x03, 0x1b, 0x58]);
    write(&mut sim, 0x48, &[0x01, 0x44, 0x80]);
    let t = sim.now() + Duration::from_millis(10);
    sim.run_until(t);
    // The host drives ALERT high push-pull while the ADC pulls it low.
    sim.drive("ALERT", Drive::High);
    assert_eq!(sim.net("ALERT"), Some(NetState::Contention));
    let finding = sim.findings().last().unwrap();
    assert!(finding.message.contains("contention"), "{finding:?}");

    // WP left floating: the 24C32 datasheet requires VSS or VCC.
    let mut board = hat();
    board.rewire("U2", "7", "/WP_NC");
    let (sim, _) = sim_of(&board);
    assert_eq!(sim.net("WP_NC"), Some(NetState::Floating));
    let finding = &sim.findings()[0];
    assert_eq!(finding.at, SimTime::ZERO);
    assert!(finding.message.contains("read by U2.wp"), "{finding:?}");
}

/// @relation(PRT-012, scope=function)
#[test]
fn an_undefined_address_strap_is_an_error() {
    let mut board = hat();
    board.rewire("U1", "1", "/ADDR_NC");
    let library = Library::builtin().unwrap();
    let rails = Rails::from_parts(&board, &library);
    let error = BoardSim::new(&board, &library, &rails, VirtualClock::new()).unwrap_err();
    assert!(error.contains("address is undefined"), "{error}");
}
