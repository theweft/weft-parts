// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Board simulation: the chip models of a [`Board`], on simulated I2C buses and digital nets.
//!
//! Built from the board's netlist (weft-design docs/21): every component whose part number the
//! library knows and whose chip has a model becomes a model; components on the same SDA and SCL
//! nets share an [`I2cBus`]; addresses come from the strap wiring ([`crate::board::address`]).
//! Model pins besides the bus ([`I2cTarget::pins`]) connect to the nets they are on. A net's
//! pull comes from resistors to a rail; a net that is a rail is fixed at its level.
//!
//! Nets settle after every transfer, at every model event ([`I2cTarget::next_event`]) and when a
//! test drives a net; each change is recorded as an [`Edge`] with its time. Floating or
//! contended inputs and fights against a rail are reported as [`Finding`]s instead of guessed.
//! Edges inside a transfer (a pin that changes at a byte's acknowledge) appear at the end of the
//! transfer.

use std::collections::BTreeMap;

use super::{
    Clock, Drive, I2cBus, I2cTarget, Message, Nack, NetState, PinDir, Pull, SimTime, resolve,
};
use crate::board::{self, Board, Rails};
use crate::chip::BusKind;
use crate::library::Library;

/// Settling rounds before a net is reported as oscillating.
const MAX_ROUNDS: usize = 16;
/// Model events processed by one [`BoardSim::run_until`] before it gives up.
const MAX_EVENTS: usize = 1_000_000;

/// A net changed level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// When.
    pub at: SimTime,
    /// The net, by its full name.
    pub net: String,
    /// The new level.
    pub state: NetState,
}

/// Something on the board that hardware would not do reliably.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// When it was seen.
    pub at: SimTime,
    /// The net, by its full name.
    pub net: String,
    /// What is wrong.
    pub message: String,
}

/// A model placed on the board.
#[derive(Debug, Clone)]
struct Placed {
    bus: String,
    address: u16,
}

/// One simulated net.
#[derive(Debug, Clone, Default)]
struct SimNet {
    /// `Some(high)` when the net is a rail.
    rail: Option<bool>,
    pull: Pull,
    /// What a test drives onto the net.
    external: Drive,
    /// Model pins: (refdes, role, direction).
    pins: Vec<(String, &'static str, PinDir)>,
    /// `None` until the first settle.
    state: Option<NetState>,
}

/// The chip models of a board, their buses and nets.
pub struct BoardSim {
    clock: Box<dyn Clock>,
    board: Board,
    /// By SDA net.
    buses: BTreeMap<String, I2cBus>,
    /// SDA net -> SCL net of each bus.
    scl: BTreeMap<String, String>,
    chips: BTreeMap<String, Placed>,
    nets: BTreeMap<String, SimNet>,
    rails: Rails,
    edges: Vec<Edge>,
    findings: Vec<Finding>,
    skipped: Vec<String>,
}

impl std::fmt::Debug for BoardSim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoardSim")
            .field("buses", &self.buses.keys())
            .field("chips", &self.chips.keys())
            .field("nets", &self.nets.keys())
            .finish_non_exhaustive()
    }
}

impl BoardSim {
    /// Places the models of `board`'s chips. `rails` names the ground and supply nets (see
    /// [`Rails::from_parts`]); `clock` drives every bus, so pass a [`VirtualClock`] clone to step
    /// time from a test.
    ///
    /// # Errors
    /// When a modelled chip's bus pins are unconnected, its address is undefined, two chips share
    /// an address, or one SDA net meets two SCL nets.
    ///
    /// [`VirtualClock`]: super::VirtualClock
    /// @relation(PRT-012, scope=function)
    // The clock is shared by cloning it into every bus; taking it by value reads naturally at
    // the call site (`BoardSim::new(…, clock.clone())`).
    #[allow(clippy::needless_pass_by_value)]
    pub fn new<C: Clock + Clone + 'static>(
        board: &Board,
        library: &Library,
        rails: &Rails,
        clock: C,
    ) -> Result<Self, String> {
        let mut sim = Self {
            clock: Box::new(clock.clone()),
            board: board.clone(),
            buses: BTreeMap::new(),
            scl: BTreeMap::new(),
            chips: BTreeMap::new(),
            nets: BTreeMap::new(),
            rails: rails.clone(),
            edges: Vec::new(),
            findings: Vec::new(),
            skipped: Vec::new(),
        };
        for (refdes, component) in &board.components {
            let Some(mpn) = component.mpn.as_deref() else {
                continue;
            };
            let Some(part) = library.lookup(mpn) else {
                continue; // passives, connectors and other parts without chip data
            };
            let Some(sim_ref) = &part.chip.sim else {
                sim.skipped
                    .push(format!("{refdes} ({}): no model", part.name()));
                continue;
            };
            if part.chip.bus != BusKind::I2c {
                sim.skipped.push(format!(
                    "{refdes} ({}): {} is not simulated yet",
                    part.name(),
                    part.chip.bus
                ));
                continue;
            }
            let model = super::build(sim_ref)?;
            let sda = board.role_net(refdes, &part, "sda")?.to_owned();
            let scl = board.role_net(refdes, &part, "scl")?.to_owned();
            let address = board::address(board, refdes, &part, rails)?;
            if let Some(other) = sim.scl.get(&sda)
                && *other != scl
            {
                return Err(format!(
                    "{refdes}: SDA net {sda} is paired with SCL net {other} elsewhere, here with \
                     {scl}."
                ));
            }
            sim.scl.insert(sda.clone(), scl);
            let pins = model.pins();
            sim.buses
                .entry(sda.clone())
                .or_insert_with(|| I2cBus::with_clock(clock.clone()))
                .attach(address, model)
                .map_err(|_| format!("{refdes}: address {address:#04x} on {sda} is used twice."))?;
            sim.chips
                .insert(refdes.clone(), Placed { bus: sda, address });
            for (role, dir) in pins {
                match board.role_net(refdes, &part, role) {
                    Ok(net) => sim.net_mut(net).pins.push((refdes.clone(), *role, *dir)),
                    Err(message) if *dir == PinDir::Input => sim.findings.push(Finding {
                        at: SimTime::ZERO,
                        net: String::new(),
                        message: format!("{message} The input is undefined."),
                    }),
                    Err(_) => {} // an unconnected output is fine
                }
            }
        }
        sim.settle();
        Ok(sim)
    }

    /// The simulated net `name`, created on first use with its rail and pull.
    fn net_mut(&mut self, name: &str) -> &mut SimNet {
        if !self.nets.contains_key(name) {
            let rail = if self.rails.power.contains(name) {
                Some(true)
            } else if self.rails.ground.contains(name) {
                Some(false)
            } else {
                None
            };
            let pull =
                self.board
                    .nets_via_resistor(name)
                    .into_iter()
                    .fold(Pull::None, |pull, other| {
                        if self.rails.power.contains(other) {
                            pull.with(Pull::Up)
                        } else if self.rails.ground.contains(other) {
                            pull.with(Pull::Down)
                        } else {
                            pull
                        }
                    });
            self.nets.insert(
                name.to_owned(),
                SimNet {
                    rail,
                    pull,
                    ..SimNet::default()
                },
            );
        }
        // Inserted above when missing.
        self.nets.entry(name.to_owned()).or_default()
    }

    /// The current time of the board's clock.
    pub fn now(&self) -> SimTime {
        self.clock.now()
    }

    /// The full name of the net called `label` (see [`Board::find_net`]).
    pub fn net_name(&self, label: &str) -> Option<&str> {
        self.board.find_net(label)
    }

    /// The bus on SDA net `label`.
    pub fn bus(&self, label: &str) -> Option<&I2cBus> {
        self.buses.get(self.net_name(label)?)
    }

    /// The bus on SDA net `label`, for drivers that need `&mut I2cBus` (embedded-hal). Call
    /// [`settle`](Self::settle) afterwards so the nets follow.
    pub fn bus_mut(&mut self, label: &str) -> Option<&mut I2cBus> {
        let name = self.board.find_net(label)?.to_owned();
        self.buses.get_mut(&name)
    }

    /// The model of component `refdes`, if it is a `T`.
    pub fn chip<T: I2cTarget>(&self, refdes: &str) -> Option<&T> {
        let placed = self.chips.get(refdes)?;
        self.buses.get(&placed.bus)?.target(placed.address)
    }

    /// The model of component `refdes`, if it is a `T`, for setting inputs such as an ADC
    /// voltage. Call [`settle`](Self::settle) afterwards if a pin may change.
    pub fn chip_mut<T: I2cTarget>(&mut self, refdes: &str) -> Option<&mut T> {
        let placed = self.chips.get(refdes)?;
        self.buses.get_mut(&placed.bus)?.target_mut(placed.address)
    }

    /// The bus (SDA net) and address of component `refdes`.
    pub fn placement(&self, refdes: &str) -> Option<(&str, u16)> {
        self.chips.get(refdes).map(|p| (p.bus.as_str(), p.address))
    }

    /// Runs a transfer on the bus on SDA net `bus`, then lets the nets settle.
    ///
    /// # Errors
    /// [`Nack`] when the bus is unknown or the transfer is not acknowledged.
    pub fn transfer(
        &mut self,
        bus: &str,
        address: u16,
        messages: &mut [Message<'_>],
    ) -> Result<(), Nack> {
        let result = self
            .bus_mut(bus)
            .ok_or(Nack)
            .and_then(|b| b.transfer(address, messages));
        self.settle();
        result
    }

    /// The level of net `label`, if it is simulated (a model pin is on it or a test drives it).
    pub fn net(&self, label: &str) -> Option<NetState> {
        self.nets.get(self.net_name(label)?)?.state
    }

    /// Drives net `label` from outside the board (a test, a host GPIO), then lets the nets
    /// settle. False for a net the board does not have.
    pub fn drive(&mut self, label: &str, drive: Drive) -> bool {
        let Some(name) = self.board.find_net(label).map(str::to_owned) else {
            return false;
        };
        self.net_mut(&name).external = drive;
        self.settle();
        true
    }

    /// Advances time to `t`, settling the nets at every model event on the way. Meant for a
    /// virtual clock; with a clock that follows real time it stops at the first event that
    /// lies in the future.
    pub fn run_until(&mut self, t: SimTime) {
        for _ in 0..MAX_EVENTS {
            let next = self.buses.values().filter_map(I2cBus::next_event).min();
            match next {
                Some(event) if event <= t => {
                    self.clock.advance_to(event);
                    if self.clock.now() < event {
                        break; // the clock does not move on request
                    }
                    self.settle();
                }
                _ => break,
            }
        }
        self.clock.advance_to(t);
        self.settle();
    }

    /// Advances every model to the clock's time and resolves every net until nothing changes,
    /// telling input pins about each change.
    /// @relation(PRT-012, scope=function)
    pub fn settle(&mut self) {
        for bus in self.buses.values_mut() {
            bus.sync();
        }
        let now = self.clock.now();
        for _ in 0..MAX_ROUNDS {
            let mut changes = Vec::new();
            for (name, net) in &self.nets {
                let drives: Vec<Drive> = std::iter::once(net.external)
                    .chain(
                        net.pins
                            .iter()
                            .filter(|(_, _, dir)| *dir == PinDir::Output)
                            .map(|(refdes, role, _)| self.pin_drive(refdes, role)),
                    )
                    .collect();
                let state = match net.rail {
                    Some(high) => {
                        let fights =
                            drives.contains(&(if high { Drive::Low } else { Drive::High }));
                        if fights {
                            NetState::Contention
                        } else if high {
                            NetState::High
                        } else {
                            NetState::Low
                        }
                    }
                    None => resolve(drives, net.pull),
                };
                if net.state != Some(state) {
                    changes.push((name.clone(), state));
                }
            }
            if changes.is_empty() {
                return;
            }
            for (name, state) in changes {
                self.apply(now, &name, state);
            }
        }
        self.findings.push(Finding {
            at: now,
            net: String::new(),
            message: format!("nets did not settle within {MAX_ROUNDS} rounds (oscillation)"),
        });
    }

    fn pin_drive(&self, refdes: &str, role: &str) -> Drive {
        self.chips
            .get(refdes)
            .and_then(|p| self.buses.get(&p.bus)?.target_dyn(p.address))
            .map_or(Drive::Released, |model| model.pin_drive(role))
    }

    fn apply(&mut self, now: SimTime, name: &str, state: NetState) {
        let Some(net) = self.nets.get_mut(name) else {
            return;
        };
        net.state = Some(state);
        let inputs: Vec<(String, &'static str)> = net
            .pins
            .iter()
            .filter(|(_, _, dir)| *dir == PinDir::Input)
            .map(|(refdes, role, _)| (refdes.clone(), *role))
            .collect();
        let rail = net.rail.is_some();
        self.edges.push(Edge {
            at: now,
            net: name.to_owned(),
            state,
        });
        let problem = match state {
            NetState::Contention if rail => Some("a pin drives against the rail (short)"),
            NetState::Contention => Some("a push-pull high fights a low (contention)"),
            NetState::Floating if !inputs.is_empty() => Some("floats with inputs on it"),
            _ => None,
        };
        if let Some(problem) = problem {
            let readers: Vec<String> = inputs
                .iter()
                .map(|(refdes, role)| format!("{refdes}.{role}"))
                .collect();
            let message = if readers.is_empty() {
                format!("{name} {problem}")
            } else {
                format!("{name} {problem}; read by {}", readers.join(", "))
            };
            self.findings.push(Finding {
                at: now,
                net: name.to_owned(),
                message,
            });
        }
        for (refdes, role) in inputs {
            let Some(placed) = self.chips.get(&refdes) else {
                continue;
            };
            if let Some(model) = self
                .buses
                .get_mut(&placed.bus)
                .and_then(|b| b.target_dyn_mut(placed.address))
            {
                model.pin_input(role, state);
            }
        }
    }

    /// Every net change so far, in order.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// The changes of net `label` so far.
    pub fn edges_of(&self, label: &str) -> Vec<&Edge> {
        let Some(name) = self.net_name(label) else {
            return Vec::new();
        };
        self.edges.iter().filter(|e| e.net == name).collect()
    }

    /// Problems seen so far.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Components with chip data that are not simulated, and why.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }
}
