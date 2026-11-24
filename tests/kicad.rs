// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! KiCad netlist importer against a fixture in KiCad 10's export format, and optionally against
//! a real export (set `WEFT_KICAD_NETLIST` to the CM5 IO Board netlist, see `just kicad-real`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs
)]

use weft_parts::board::{PinInfo, kicad};

fn hat() -> weft_parts::board::Board {
    kicad::parse(include_str!("fixtures/hat.net"), &[]).unwrap()
}

/// @relation(PRT-011, scope=function)
#[test]
fn components_values_fields_and_part_numbers() {
    let board = hat();
    assert_eq!(board.components.len(), 11);
    let adc = &board.components["U1"];
    assert_eq!(
        adc.mpn.as_deref(),
        Some("ADS1115IDGSR"),
        "from the MPN field"
    );
    assert_eq!(adc.value.as_deref(), Some("ADS1115"));
    assert_eq!(adc.fields["Manufacturer"], "Texas Instruments");
    assert_eq!(adc.fields["Datasheet"], "", "a field without a value");
    assert_eq!(board.components["R2"].value.as_deref(), Some("100k"));
    // Escaped quotes inside strings.
    assert_eq!(
        board.components["C1"].fields["Description"],
        r#"Decoupling, 0.020" (0.5 mm) pitch note"#
    );
    // Another field order: the first non-empty field named wins.
    let other = kicad::parse(include_str!("fixtures/hat.net"), &["Manufacturer", "MPN"]).unwrap();
    assert_eq!(
        other.components["U1"].mpn.as_deref(),
        Some("Texas Instruments")
    );
}

/// @relation(PRT-011, scope=function)
#[test]
fn nets_pin_names_and_types() {
    let board = hat();
    assert_eq!(board.net_of("U1", "2"), Some("/ALERT"));
    assert_eq!(
        board.components["U1"].pin_info["2"],
        PinInfo {
            name: Some("ALERT/RDY".into()),
            kind: Some("open_collector".into()),
        }
    );
    assert_eq!(board.pins_on("/SDA").len(), 5);
    // Labels resolve to the sheet-prefixed name; power nets have none.
    assert_eq!(board.find_net("SDA"), Some("/SDA"));
    assert_eq!(board.find_net("GND"), Some("GND"));
    assert_eq!(board.find_net("NOPE"), None);
    assert_eq!(board.nets_via_resistor("/ALERT"), ["+3V3"]);
}

/// @relation(PRT-011, scope=function)
#[test]
fn real_kicad_export() {
    let Ok(path) = std::env::var("WEFT_KICAD_NETLIST") else {
        eprintln!("WEFT_KICAD_NETLIST not set: skipped (run `just kicad-real`)");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("netlist");
    // The CM5 IO Board keeps the part number in Field6.
    let board = kicad::parse(&text, &["Field6"]).unwrap();
    // 83 symbols, minus the five M.2 screws marked exclude_from_board, which the S-expression
    // netlist leaves out (KiCad's XML netlist keeps them).
    assert_eq!(board.components.len(), 78);
    assert_eq!(board.nets.len(), 210);
    let r1 = &board.components["R1"];
    assert_eq!(r1.value.as_deref(), Some("1k"));
    assert_eq!(r1.mpn.as_deref(), Some("RK73H1ETTP1001F"));
    let wp = board
        .find_net("EEPROM_nWP")
        .expect("labelled CM5 control net");
    assert_eq!(board.net_of("Module1", "20"), Some(wp));
    let info = &board.components["Module1"].pin_info["20"];
    assert_eq!(info.name.as_deref(), Some("EEPROM_nWP_20"));
    assert_eq!(
        info.kind.as_deref(),
        Some("passive"),
        "as good as the symbol"
    );
}
