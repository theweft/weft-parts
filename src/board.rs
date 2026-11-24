// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Tool-neutral board model: components, their part numbers, values and fields, and which net each
//! pin is on.
//!
//! Shared by weft-boardchecker (static checks) and [`crate::sim::BoardSim`] (simulation), so both
//! read a board the same way. Importers fill it: [`kicad`] (KiCad netlist, schematic level, with
//! pin names and pin types); weft-boardchecker reads IPC-2581 and IPC-D-356A.
//!
//! [`address`] derives an I2C address from the strap wiring, the rule both consumers use.

pub mod kicad;

use std::collections::{BTreeMap, BTreeSet};

use crate::chip::Address;
use crate::library::Part;

/// One component pin.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PinRef {
    /// Reference designator, such as `U3`.
    pub refdes: String,
    /// Pin (pad) number in the component's package, such as `9`.
    pub pin: String,
}

/// What the design source says about one pin, where it says anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinInfo {
    /// Pin name in the symbol, such as `SDA` (KiCad `pinfunction`).
    pub name: Option<String>,
    /// Electrical type as the source spells it, such as `bidirectional`, `open_collector` or
    /// `power_in` (KiCad `pintype`). Only as good as the symbol: chip data stays authoritative.
    pub kind: Option<String>,
}

/// One placed component.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Component {
    /// Manufacturer part number, if the source carried one.
    pub mpn: Option<String>,
    /// Value, such as `10k` or `100n` (resistors, capacitors) or a part name.
    pub value: Option<String>,
    /// Every other field of the symbol, by name.
    pub fields: BTreeMap<String, String>,
    /// Pin number -> net name, for connected pins only.
    pub pins: BTreeMap<String, String>,
    /// Pin number -> name and type, where the source has them.
    pub pin_info: BTreeMap<String, PinInfo>,
}

/// A board: components and nets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Board {
    /// Reference designator -> component.
    pub components: BTreeMap<String, Component>,
    /// Net name -> pins on that net.
    pub nets: BTreeMap<String, Vec<PinRef>>,
}

impl Board {
    /// An empty board.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `refdes` pin `pin` is on `net`. Creates the component if needed.
    pub fn connect(&mut self, net: &str, refdes: &str, pin: &str) {
        let component = self.components.entry(refdes.to_owned()).or_default();
        component.pins.insert(pin.to_owned(), net.to_owned());
        let pins = self.nets.entry(net.to_owned()).or_default();
        let pin_ref = PinRef {
            refdes: refdes.to_owned(),
            pin: pin.to_owned(),
        };
        if !pins.contains(&pin_ref) {
            pins.push(pin_ref);
        }
    }

    /// Moves `refdes` pin `pin` to `net` (for what-if checks and tests).
    pub fn rewire(&mut self, refdes: &str, pin: &str, net: &str) {
        let old = self.net_of(refdes, pin).map(str::to_owned);
        if let Some(old) = old
            && let Some(pins) = self.nets.get_mut(&old)
        {
            pins.retain(|p| !(p.refdes == refdes && p.pin == pin));
            if pins.is_empty() {
                self.nets.remove(&old);
            }
        }
        self.connect(net, refdes, pin);
    }

    /// Adds a component that may have no connected pins (still needed for part numbers).
    pub fn add_component(&mut self, refdes: &str) {
        self.components.entry(refdes.to_owned()).or_default();
    }

    /// Sets the part number of `refdes`. Creates the component if needed.
    pub fn set_mpn(&mut self, refdes: &str, mpn: &str) {
        let mpn = mpn.trim();
        if !mpn.is_empty() {
            self.components.entry(refdes.to_owned()).or_default().mpn = Some(mpn.to_owned());
        }
    }

    /// The net `refdes` pin `pin` is on.
    pub fn net_of(&self, refdes: &str, pin: &str) -> Option<&str> {
        self.components
            .get(refdes)?
            .pins
            .get(pin)
            .map(String::as_str)
    }

    /// Pins on `net` (empty for an unknown net).
    pub fn pins_on(&self, net: &str) -> &[PinRef] {
        self.nets.get(net).map_or(&[], Vec::as_slice)
    }

    /// The net called `label`: the exact name, or else the one net whose last path segment is
    /// `label` (KiCad prefixes local labels with their sheet, `/CM5_GPIO (…)/EEPROM_nWP`).
    /// `None` when unknown or ambiguous.
    /// @relation(PRT-011, scope=function)
    pub fn find_net(&self, label: &str) -> Option<&str> {
        if let Some((name, _)) = self.nets.get_key_value(label) {
            return Some(name);
        }
        let mut found = self
            .nets
            .keys()
            .filter(|name| name.rsplit('/').next() == Some(label));
        match (found.next(), found.next()) {
            (Some(name), None) => Some(name),
            _ => None,
        }
    }

    /// Whether `refdes` looks like a resistor: designator `R<digit>…` with two connected pins.
    pub fn is_resistor(&self, refdes: &str) -> bool {
        let mut chars = refdes.chars();
        chars.next() == Some('R')
            && chars.next().is_some_and(|c| c.is_ascii_digit())
            && self
                .components
                .get(refdes)
                .is_some_and(|c| c.pins.len() == 2)
    }

    /// Nets one resistor away from `net`.
    pub fn nets_via_resistor(&self, net: &str) -> Vec<&str> {
        let mut out = Vec::new();
        for pin_ref in self.pins_on(net) {
            if !self.is_resistor(&pin_ref.refdes) {
                continue;
            }
            if let Some(component) = self.components.get(&pin_ref.refdes) {
                for (pin, other) in &component.pins {
                    if *pin != pin_ref.pin && other != net && !out.contains(&other.as_str()) {
                        out.push(other.as_str());
                    }
                }
            }
        }
        out
    }

    /// The net of `part`'s pin `role` on component `refdes`.
    ///
    /// # Errors
    /// When the chip file has no such role or the pin is not connected.
    pub fn role_net(&self, refdes: &str, part: &Part<'_>, role: &str) -> Result<&str, String> {
        let pin = part
            .pin(role)
            .ok_or_else(|| format!("Chip file \"{}\" has no `{role}` pin.", part.name()))?;
        self.net_of(refdes, pin)
            .ok_or_else(|| format!("{refdes} pin {pin} ({role}) is not connected."))
    }
}

/// Nets known to be ground or supply, besides the chip's own `gnd` and `vdd` pins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rails {
    /// Ground nets.
    pub ground: BTreeSet<String>,
    /// Supply nets.
    pub power: BTreeSet<String>,
}

impl Rails {
    /// The `gnd` and `vdd` nets of every component whose part number `library` knows.
    pub fn from_parts(board: &Board, library: &crate::Library) -> Self {
        let mut rails = Self::default();
        for (refdes, component) in &board.components {
            let Some(part) = component.mpn.as_deref().and_then(|m| library.lookup(m)) else {
                continue;
            };
            if let Ok(net) = board.role_net(refdes, &part, "gnd") {
                rails.ground.insert(net.to_owned());
            }
            if let Ok(net) = board.role_net(refdes, &part, "vdd") {
                rails.power.insert(net.to_owned());
            }
        }
        rails
    }
}

/// The I2C address of `refdes` from its chip's address rule and the strap wiring: a strap net is
/// read directly, or through one resistor if nothing is connected directly.
///
/// # Errors
/// When the chip has no address rule, a strap pin is unconnected, or the strap level is undefined
/// or ambiguous; the message names the component, pin and net.
/// @relation(PRT-012, scope=function)
pub fn address(board: &Board, refdes: &str, part: &Part<'_>, rails: &Rails) -> Result<u16, String> {
    let Some(rule) = &part.chip.address else {
        return Err(format!(
            "Chip file \"{}\" has no address rule.",
            part.name()
        ));
    };
    match rule {
        Address::Fixed { value } => Ok(*value),
        Address::Strap { pin, levels } => {
            let net = board.role_net(refdes, part, pin)?;
            let found = strap_levels(board, rails, net, refdes, part, pin);
            let addresses: BTreeSet<u16> = found
                .iter()
                .filter_map(|l| levels.get(l))
                .copied()
                .collect();
            let mut iter = addresses.iter();
            match (iter.next(), iter.next()) {
                (Some(address), None) => Ok(*address),
                (None, _) => Err(format!(
                    "{refdes} address pin ({pin}) is on net {net}, which is none of {}; the \
                     address is undefined.",
                    levels.keys().cloned().collect::<Vec<_>>().join(", ")
                )),
                (Some(_), Some(_)) => Err(format!(
                    "{refdes} address pin ({pin}) is tied to several levels ({}).",
                    found.into_iter().collect::<Vec<_>>().join(", ")
                )),
            }
        }
        Address::Bits { base, pins } => {
            let mut address = *base;
            for (bit, pin) in pins.iter().rev().enumerate() {
                let net = board.role_net(refdes, part, pin)?;
                let found = strap_levels(board, rails, net, refdes, part, pin);
                match (found.contains("gnd"), found.contains("vdd")) {
                    (true, false) => {}
                    (false, true) => address |= 1 << bit,
                    _ => {
                        return Err(format!(
                            "{refdes} address pin {pin} is on net {net}, which is neither ground \
                             nor supply."
                        ));
                    }
                }
            }
            Ok(address)
        }
    }
}

/// Levels a strap net is tied to: `gnd`, `vdd` or other pin roles of the same part. Looks
/// through one resistor if nothing is connected directly.
fn strap_levels(
    board: &Board,
    rails: &Rails,
    net: &str,
    refdes: &str,
    part: &Part<'_>,
    own_role: &str,
) -> BTreeSet<String> {
    let component = board.components.get(refdes);
    let direct = |net: &str| {
        let mut out = BTreeSet::new();
        let part_net = |role: &str| {
            part.package
                .pins
                .get(role)
                .and_then(|p| component.and_then(|c| c.pins.get(p)))
        };
        if rails.ground.contains(net) || part_net("gnd").is_some_and(|n| n == net) {
            out.insert("gnd".to_owned());
        }
        if rails.power.contains(net) || part_net("vdd").is_some_and(|n| n == net) {
            out.insert("vdd".to_owned());
        }
        for (role, pin) in &part.package.pins {
            if role != own_role
                && role != "gnd"
                && role != "vdd"
                && board.net_of(refdes, pin) == Some(net)
            {
                out.insert(role.clone());
            }
        }
        out
    };
    let found = direct(net);
    if !found.is_empty() {
        return found;
    }
    board
        .nets_via_resistor(net)
        .into_iter()
        .flat_map(direct)
        .collect()
}
