// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! The chip data: every file loads, is listed, validates and resolves.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs
)]

use std::path::Path;

use weft_parts::library::BUILTIN;
use weft_parts::{Address, Library};

#[test]
fn builtin_loads_without_overlaps() {
    let library = Library::builtin().unwrap();
    assert_eq!(library.chips().count(), BUILTIN.len());
    assert!(library.overlaps().is_empty(), "{:?}", library.overlaps());
}

#[test]
fn every_chip_file_is_listed() {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "toml") {
                out.push(path.to_string_lossy().into_owned());
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    walk(&root.join("chips"), &mut files);
    let mut files: Vec<String> = files
        .iter()
        .map(|f| {
            Path::new(f)
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    files.sort();
    let listed: Vec<String> = BUILTIN.iter().map(|(p, _)| (*p).to_owned()).collect();
    assert_eq!(files, listed, "update library::BUILTIN");
}

#[test]
fn lookup_selects_the_package() {
    let library = Library::builtin().unwrap();
    let part = library.lookup("ads1115idgsr").unwrap();
    assert_eq!(part.name(), "TI ADS1115, VSSOP-10");
    assert_eq!(part.pin("sda"), Some("9"));
    assert!(
        library.lookup("ADS1115IRUGR").is_none(),
        "X2QFN has another pinout"
    );
    assert_eq!(
        library.lookup("24LC32AT-I/SN").unwrap().pin("wp"),
        Some("7")
    );
}

#[test]
fn possible_addresses() {
    let library = Library::builtin().unwrap();
    let addresses = |mpn: &str| {
        let part = library.lookup(mpn).unwrap();
        part.chip
            .address
            .as_ref()
            .map(Address::possible)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>()
    };
    assert_eq!(addresses("ADS1115IDGSR"), [0x48, 0x49, 0x4a, 0x4b]);
    assert_eq!(addresses("MCP4728-E/UN"), [0x60]);
    assert_eq!(addresses("AT24C32"), (0x50..=0x57).collect::<Vec<_>>());
}

#[test]
fn project_files_override_builtin() {
    let mut library = Library::builtin().unwrap();
    let local = r#"
name = "Local ADS1115 variant"
bus = "i2c"
address = { kind = "fixed", value = 0x48 }
[[package]]
name = "custom"
mpn = ["ADS1115IDGS"]
pins = { sda = "1", scl = "2" }
"#;
    library.add_toml(local, "local.toml").unwrap();
    assert_eq!(
        library.lookup("ADS1115IDGSR").unwrap().chip.name,
        "Local ADS1115 variant"
    );
}

#[test]
fn every_model_builds() {
    for (origin, chip) in Library::builtin().unwrap().chips() {
        let sim = chip
            .sim
            .as_ref()
            .unwrap_or_else(|| panic!("{origin} has no model"));
        weft_parts::sim::build(sim).unwrap();
    }
}

fn problems(toml: &str) -> String {
    Library::default()
        .add_toml(toml, "bad.toml")
        .unwrap_err()
        .to_string()
}

#[test]
fn rejects_inconsistent_chips() {
    let base = |extra: &str| {
        format!(
            "name = \"X\"\nbus = \"i2c\"\n{extra}\n[[package]]\nname = \"P\"\nmpn = [\"X1\"]\npins = {{ sda = \"1\", scl = \"2\", addr = \"3\" }}\n"
        )
    };
    assert!(problems(&base("")).contains("without [address]"));
    assert!(
        problems(&base(
            "address = { kind = \"strap\", pin = \"a0\", levels = { gnd = 0x10 } }"
        ))
        .contains("no `a0` pin")
    );
    assert!(
        problems(&base(
            "address = { kind = \"strap\", pin = \"addr\", levels = { foo = 0x10 } }"
        ))
        .contains("strap level `foo`")
    );
    assert!(problems(&base("address = { kind = \"fixed\", value = 1 }\n[devicetree]\ncompatible = [\"nocomma\"]\nbinding = \"x.yaml\"")).contains("not \"vendor,device\""));
    assert!(
        problems(&base(
            "address = { kind = \"fixed\", value = 1 }\n[sim]\nmodel = \"nope\""
        ))
        .contains("unknown sim model")
    );
    assert!(
        problems(&base(
            "address = { kind = \"fixed\", value = 1 }\nunknown = 1"
        ))
        .contains("unknown field")
    );
}
