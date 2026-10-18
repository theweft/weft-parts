// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! The chip data format: one TOML file per chip.
//!
//! A chip has bus-level facts that hold for every package (address rule, device tree, simulation
//! model) and one or more packages, each with its own part number prefixes and pinout.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Bus a chip is controlled over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BusKind {
    /// I2C (and SMBus).
    I2c,
    /// SPI.
    Spi,
}

impl std::fmt::Display for BusKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::I2c => "i2c",
            Self::Spi => "spi",
        })
    }
}

/// How an I2C address is set.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Address {
    /// Always this address.
    Fixed {
        /// The address.
        value: u16,
    },
    /// One pin selects the address by what it connects to: `gnd`, `vdd` or another pin role.
    Strap {
        /// Pin role of the strap pin.
        pin: String,
        /// Level -> address.
        levels: BTreeMap<String, u16>,
    },
    /// Each pin is one address bit (gnd = 0, vdd = 1), most significant first, added to `base`.
    Bits {
        /// Address with all bits 0.
        base: u16,
        /// Pin roles, most significant bit first.
        pins: Vec<String>,
    },
}

impl Address {
    /// Every address the chip can have.
    pub fn possible(&self) -> BTreeSet<u16> {
        match self {
            Self::Fixed { value } => BTreeSet::from([*value]),
            Self::Strap { levels, .. } => levels.values().copied().collect(),
            Self::Bits { base, pins } => {
                let count = 1u16.checked_shl(u32::try_from(pins.len()).unwrap_or(u32::MAX));
                (0..count.unwrap_or(0)).map(|bits| base | bits).collect()
            }
        }
    }

    /// Pin roles the rule reads.
    pub fn pins(&self) -> Vec<&str> {
        match self {
            Self::Fixed { .. } => Vec::new(),
            Self::Strap { pin, .. } => vec![pin.as_str()],
            Self::Bits { pins, .. } => pins.iter().map(String::as_str).collect(),
        }
    }
}

/// Linux device tree facts. Bindings are the source of truth; this only points at them and adds
/// what the binding cannot know (which pin a GPIO property refers to).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceTree {
    /// Compatible strings the Linux driver binds to.
    pub compatible: Vec<String>,
    /// Binding schema path in the Linux source tree.
    pub binding: String,
    /// Driver source path in the Linux source tree.
    #[serde(default)]
    pub driver: Option<String>,
    /// Device tree property (`reset-gpios`, `interrupts`) -> pin role.
    #[serde(default)]
    pub gpios: BTreeMap<String, String>,
}

/// One package: part numbers and pinout.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    /// Package name, such as `VSSOP-10`.
    pub name: String,
    /// Part number prefixes for this package.
    pub mpn: Vec<String>,
    /// Pin role -> pin number.
    pub pins: BTreeMap<String, String>,
}

/// Simulation model reference.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sim {
    /// Model name in [`crate::sim`].
    pub model: String,
    /// Model parameters, such as memory size.
    #[serde(default)]
    pub params: BTreeMap<String, u64>,
    /// Behaviour the model implements.
    #[serde(default)]
    pub covers: Vec<String>,
    /// Behaviour the model does not implement.
    #[serde(default)]
    pub missing: Vec<String>,
}

/// One chip file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chip {
    /// Manufacturer and part family, such as `TI ADS1115`.
    pub name: String,
    /// Datasheet or product page.
    #[serde(default)]
    pub datasheet: Option<String>,
    /// Control bus.
    pub bus: BusKind,
    /// I2C address rule.
    #[serde(default)]
    pub address: Option<Address>,
    /// Linux device tree facts.
    #[serde(default)]
    pub devicetree: Option<DeviceTree>,
    /// Packages.
    pub package: Vec<Package>,
    /// Simulation model.
    #[serde(default)]
    pub sim: Option<Sim>,
}

const LEVELS: [&str; 2] = ["gnd", "vdd"];

impl Chip {
    /// Consistency problems, as sentences. Empty means valid.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.package.is_empty() {
            out.push("has no [[package]]".to_owned());
        }
        let bus_roles: &[&str] = match self.bus {
            BusKind::I2c => &["sda", "scl"],
            BusKind::Spi => &["sck", "cs"],
        };
        for package in &self.package {
            let p = &package.name;
            if package.mpn.iter().all(|m| m.trim().is_empty()) {
                out.push(format!("package {p} has no part number prefix"));
            }
            for role in bus_roles {
                if !package.pins.contains_key(*role) {
                    out.push(format!("package {p} has no `{role}` pin"));
                }
            }
            let mut numbers = BTreeSet::new();
            for (role, number) in &package.pins {
                if !numbers.insert(number) {
                    out.push(format!(
                        "package {p} assigns pin {number} twice (again as {role})"
                    ));
                }
            }
            let mut needed: Vec<&str> =
                self.address.as_ref().map(Address::pins).unwrap_or_default();
            if let Some(dt) = &self.devicetree {
                needed.extend(dt.gpios.values().map(String::as_str));
            }
            for role in needed {
                if !package.pins.contains_key(role) {
                    out.push(format!(
                        "package {p} has no `{role}` pin, which the chip refers to"
                    ));
                }
            }
            if let Some(Address::Strap { levels, .. }) = &self.address {
                for level in levels.keys() {
                    if !LEVELS.contains(&level.as_str()) && !package.pins.contains_key(level) {
                        out.push(format!(
                            "strap level `{level}` is neither gnd, vdd nor a pin of package {p}"
                        ));
                    }
                }
            }
        }
        match (self.bus, &self.address) {
            (BusKind::I2c, None) => out.push("is an I2C chip without [address]".to_owned()),
            (BusKind::Spi, Some(_)) => out.push("is an SPI chip but has [address]".to_owned()),
            _ => {}
        }
        if let Some(dt) = &self.devicetree {
            if dt.compatible.is_empty() {
                out.push("devicetree has no compatible string".to_owned());
            }
            for compatible in &dt.compatible {
                if compatible.matches(',').count() != 1 {
                    out.push(format!(
                        "compatible \"{compatible}\" is not \"vendor,device\""
                    ));
                }
            }
            if !dt.binding.starts_with("Documentation/devicetree/bindings/")
                || std::path::Path::new(&dt.binding)
                    .extension()
                    .is_none_or(|e| e != "yaml")
            {
                out.push(format!(
                    "binding {} is not a Linux binding schema path",
                    dt.binding
                ));
            }
        }
        if let Some(sim) = &self.sim
            && let Err(error) = crate::sim::build(sim)
        {
            out.push(error);
        }
        out
    }
}
