// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Behavioural chip models: what a chip does with the bytes on its bus, and when.
//!
//! A model implements [`I2cTarget`] (or [`SpiTarget`]) at byte level. An [`I2cBus`] routes
//! transfers to models by address, with the message semantics of Linux `i2c_msg` lists, so the
//! same models can sit behind weft-sim's in-guest bus module (Linux drivers in QEMU), behind the
//! embedded-hal `I2c` trait (Rust MCU drivers in host tests), or in plain unit tests.
//!
//! Analog values and pins are set and observed through each model's own methods
//! (for example [`Ads1115::set_input`] or [`Mcp4728::output_voltage`]).
//!
//! # Time
//!
//! Models are time-aware but never read a clock: the bus calls [`I2cTarget::advance`] with the
//! time of every bus event, and the model finishes whatever is due by then (a conversion, an
//! EEPROM write cycle). The bus takes the time from a [`Clock`]: a [`VirtualClock`] in host tests
//! (time moves only with bus traffic and driver delays, so runs are deterministic) or a
//! [`HostClock`] in weft-sim. Each transfer takes as long as its bits at the bus speed
//! (100 kHz by default), so a driver that polls a busy bit sees the chip finish after the right
//! amount of virtual time. Models keep their own timing in a [`ClockDomain`].

mod ads1115;
mod at24;
#[cfg(feature = "embedded-hal")]
mod hal;
mod mcp4728;
mod time;

use std::any::Any;
use std::collections::BTreeMap;
use std::time::Duration;

pub use ads1115::Ads1115;
pub use at24::At24;
#[cfg(feature = "embedded-hal")]
pub use hal::HalNack;
pub use mcp4728::{Channel as Mcp4728Channel, Mcp4728};
pub use time::{Clock, ClockDomain, HostClock, SimTime, VirtualClock};

use crate::chip::Sim;

/// The target did not acknowledge (unknown address, busy, or rejected byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nack;

impl std::fmt::Display for Nack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no acknowledge")
    }
}

impl std::error::Error for Nack {}

/// An I2C target (peripheral) at byte level.
///
/// Before each call the bus calls [`advance`](Self::advance) with the time of that event, so the
/// other methods act at "now".
pub trait I2cTarget: Any + Send {
    /// Time has reached `now` (never earlier than a previous call). Finish whatever is due.
    fn advance(&mut self, now: SimTime) {
        let _ = now;
    }
    /// The next time the model changes state on its own (a conversion completes, a write cycle
    /// ends), if any. A scheduler can advance the model then, for example to raise an ALERT pin.
    fn next_event(&self) -> Option<SimTime> {
        None
    }
    /// A start or repeated start addressed to this target; `read` is the R/W bit.
    /// `Err(Nack)` leaves the address unacknowledged (for example while an EEPROM is busy); the
    /// transfer then ends without a [`stop`](Self::stop) call to this target.
    ///
    /// # Errors
    /// When the target does not acknowledge its address.
    fn start(&mut self, read: bool) -> Result<(), Nack> {
        let _ = read;
        Ok(())
    }
    /// One byte written by the controller. `Err(Nack)` rejects it and ends the transfer.
    ///
    /// # Errors
    /// When the target does not acknowledge the byte.
    fn write(&mut self, byte: u8) -> Result<(), Nack>;
    /// One byte read by the controller.
    fn read(&mut self) -> u8;
    /// A stop condition.
    fn stop(&mut self) {}
    /// A complete write to the general call address 0x00.
    fn general_call(&mut self, data: &[u8]) {
        let _ = data;
    }
    /// Whether this target is asserting an SMBus alert, with the status bit it sends in an
    /// alert response (the least significant bit after its address). `None` when not alerting.
    fn smbus_alert(&self) -> Option<bool> {
        None
    }
    /// This target won an SMBus alert response; it clears a latched alert.
    fn smbus_alert_answered(&mut self) {}
}

/// An SPI target at byte level. No SPI models exist yet; the trait fixes the shape for them.
pub trait SpiTarget: Any + Send {
    /// Time has reached `now`; see [`I2cTarget::advance`].
    fn advance(&mut self, now: SimTime) {
        let _ = now;
    }
    /// The next time the model changes state on its own; see [`I2cTarget::next_event`].
    fn next_event(&self) -> Option<SimTime> {
        None
    }
    /// Chip select asserted.
    fn select(&mut self) {}
    /// One byte clocked in on MOSI; returns the byte shifted out on MISO.
    fn transfer(&mut self, mosi: u8) -> u8;
    /// Chip select released.
    fn deselect(&mut self) {}
}

/// One message of a transfer.
#[derive(Debug)]
pub enum Message<'a> {
    /// Controller writes these bytes.
    Write(&'a [u8]),
    /// Controller reads into this buffer.
    Read(&'a mut [u8]),
}

/// Two targets on one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressInUse(pub u16);

/// The general call address.
pub const GENERAL_CALL: u16 = 0x00;

/// The SMBus alert response address. A read from it returns the address of the alerting target
/// with the lowest address (shifted left, status bit in bit 0), which then clears its alert.
pub const ALERT_RESPONSE: u16 = 0x0c;

/// Which part of a transfer was not acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nacked {
    /// The address (no target there, or the target is busy).
    Address,
    /// A data byte.
    Data,
}

/// Bits on the wire, in SCL periods: a (repeated) start plus the address byte with its ACK.
const START_AND_ADDRESS: u64 = 1 + 9;
/// One data byte with its ACK.
const BYTE: u64 = 9;
/// A stop condition.
const STOP: u64 = 1;

/// Default bus speed: standard mode, as on the Raspberry Pi's `i2c_arm` bus.
pub const DEFAULT_SPEED_HZ: u64 = 100_000;

/// An I2C bus with models attached by address.
///
/// Every transfer starts at the clock's current time (or when the previous transfer ended, if
/// that is later) and lasts as long as its bits at the bus speed: start and address 10 SCL
/// periods, each data byte 9, stop 1. Setup and hold times and clock stretching are not modelled.
/// At the end the bus moves the clock to the end of the transfer ([`Clock::advance_to`]).
pub struct I2cBus {
    targets: BTreeMap<u16, Box<dyn I2cTarget>>,
    clock: Box<dyn Clock>,
    scl: ClockDomain,
    last_end: SimTime,
}

impl std::fmt::Debug for I2cBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("I2cBus")
            .field("addresses", &self.targets.keys())
            .field("clock", &self.clock)
            .field("scl", &self.scl)
            .finish_non_exhaustive()
    }
}

impl Default for I2cBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Times events within one transfer: each event's time is computed from the transfer's start, so
/// rounding does not add up.
struct Timeline {
    start: SimTime,
    scl: ClockDomain,
    bits: u64,
}

impl Timeline {
    fn after(&mut self, bits: u64) -> SimTime {
        self.bits = self.bits.saturating_add(bits);
        self.now()
    }

    fn now(&self) -> SimTime {
        self.peek(0)
    }

    /// The time `bits` from now, without moving there.
    fn peek(&self, bits: u64) -> SimTime {
        self.start + self.scl.cycles(self.bits.saturating_add(bits))
    }
}

impl I2cBus {
    /// An empty bus with its own [`VirtualClock`] at 100 kHz.
    pub fn new() -> Self {
        Self::with_clock(VirtualClock::new())
    }

    /// An empty bus at 100 kHz that takes the time from `clock`. Pass a clone of a
    /// [`VirtualClock`] to share virtual time with a test or other buses.
    /// @relation(PRT-004, scope=function)
    pub fn with_clock(clock: impl Clock + 'static) -> Self {
        Self {
            targets: BTreeMap::new(),
            clock: Box::new(clock),
            scl: ClockDomain::nominal(DEFAULT_SPEED_HZ),
            last_end: SimTime::ZERO,
        }
    }

    /// The SCL clock.
    pub fn speed(&self) -> ClockDomain {
        self.scl
    }

    /// Sets the SCL clock, such as 400 kHz fast mode.
    pub fn set_speed(&mut self, scl: ClockDomain) {
        self.scl = scl;
    }

    /// The bus clock's current time.
    pub fn now(&self) -> SimTime {
        self.clock.now()
    }

    /// The earliest [`I2cTarget::next_event`] of all models.
    pub fn next_event(&self) -> Option<SimTime> {
        self.targets.values().filter_map(|t| t.next_event()).min()
    }

    /// Advances every model to the clock's current time, so state changes between transfers
    /// (a finished conversion) are visible through the models' own methods.
    pub fn sync(&mut self) {
        let now = self.clock.now().max(self.last_end);
        for target in self.targets.values_mut() {
            target.advance(now);
        }
    }

    /// Attaches a model at `address`.
    ///
    /// # Errors
    /// When another model already uses the address.
    pub fn attach(&mut self, address: u16, target: Box<dyn I2cTarget>) -> Result<(), AddressInUse> {
        if address == GENERAL_CALL
            || address == ALERT_RESPONSE
            || self.targets.contains_key(&address)
        {
            return Err(AddressInUse(address));
        }
        self.targets.insert(address, target);
        Ok(())
    }

    /// The model at `address`, if it is a `T`.
    pub fn target<T: I2cTarget>(&self, address: u16) -> Option<&T> {
        let target: &dyn Any = self.targets.get(&address)?.as_ref();
        target.downcast_ref()
    }

    /// The model at `address`, if it is a `T`.
    pub fn target_mut<T: I2cTarget>(&mut self, address: u16) -> Option<&mut T> {
        let target: &mut dyn Any = self.targets.get_mut(&address)?.as_mut();
        target.downcast_mut()
    }

    /// Runs a transfer with Linux `i2c_msg` semantics: every message begins with a (repeated)
    /// start, and a stop follows the last one. Writes to [`GENERAL_CALL`] go to every model; a
    /// one-byte read from [`ALERT_RESPONSE`] answers an SMBus alert.
    ///
    /// # Errors
    /// [`Nack`] when no model has the address or a model rejects its address or a byte.
    /// @relation(PRT-002, scope=function)
    pub fn transfer(&mut self, address: u16, messages: &mut [Message<'_>]) -> Result<(), Nack> {
        self.run(address, messages, false).map_err(|_| Nack)
    }

    /// Runs a transfer where adjacent messages of the same direction continue each other without
    /// a repeated start (embedded-hal `transaction` semantics).
    /// @relation(PRT-005, scope=function)
    pub(crate) fn run(
        &mut self,
        address: u16,
        messages: &mut [Message<'_>],
        merge_adjacent: bool,
    ) -> Result<(), Nacked> {
        let mut timeline = Timeline {
            start: self.clock.now().max(self.last_end),
            scl: self.scl,
            bits: 0,
        };
        let result = self.run_timed(&mut timeline, address, messages, merge_adjacent);
        let end = timeline.after(STOP);
        self.last_end = end;
        self.clock.advance_to(end);
        result
    }

    /// @relation(PRT-004, scope=function)
    fn run_timed(
        &mut self,
        timeline: &mut Timeline,
        address: u16,
        messages: &mut [Message<'_>],
        merge_adjacent: bool,
    ) -> Result<(), Nacked> {
        if address == ALERT_RESPONSE {
            return self.alert_response(timeline, messages);
        }
        if address == GENERAL_CALL {
            let mut data = Vec::new();
            for message in messages.iter() {
                match message {
                    Message::Write(bytes) => data.extend_from_slice(bytes),
                    Message::Read(_) => return Err(Nacked::Address),
                }
            }
            let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
            let at = timeline.after(START_AND_ADDRESS + bytes.saturating_mul(BYTE));
            for target in self.targets.values_mut() {
                target.advance(at);
                target.general_call(&data);
            }
            return Ok(());
        }
        let Some(target) = self.targets.get_mut(&address) else {
            timeline.after(START_AND_ADDRESS);
            return Err(Nacked::Address);
        };
        let mut previous_read: Option<bool> = None;
        let mut result = Ok(());
        'messages: for message in messages.iter_mut() {
            let read = matches!(message, Message::Read(_));
            if !(merge_adjacent && previous_read == Some(read)) {
                target.advance(timeline.after(START_AND_ADDRESS));
                // Not addressed: the transfer ends and the target sees no stop.
                target.start(read).map_err(|_| Nacked::Address)?;
            }
            previous_read = Some(read);
            match message {
                Message::Write(bytes) => {
                    for byte in *bytes {
                        // The target acts once the byte is in.
                        target.advance(timeline.after(BYTE));
                        if target.write(*byte).is_err() {
                            result = Err(Nacked::Data);
                            break 'messages;
                        }
                    }
                }
                Message::Read(buffer) => {
                    for byte in buffer.iter_mut() {
                        // The target drives the byte from its first bit.
                        target.advance(timeline.now());
                        *byte = target.read();
                        timeline.after(BYTE);
                    }
                }
            }
        }
        target.advance(timeline.peek(STOP));
        target.stop();
        result
    }

    /// SMBus alert response: one read message; every alerting target competes and the lowest
    /// address wins arbitration, as on the wire.
    /// @relation(PRT-002, scope=function)
    fn alert_response(
        &mut self,
        timeline: &mut Timeline,
        messages: &mut [Message<'_>],
    ) -> Result<(), Nacked> {
        let at = timeline.after(START_AND_ADDRESS);
        for target in self.targets.values_mut() {
            target.advance(at);
        }
        let winner = self
            .targets
            .iter()
            .find_map(|(address, t)| t.smbus_alert().map(|status| (*address, status)));
        let (Some((address, status)), [Message::Read(buffer)]) = (winner, messages) else {
            return Err(Nacked::Address);
        };
        let byte = u8::try_from(address << 1).unwrap_or(0xfe) | u8::from(status);
        for slot in buffer.iter_mut() {
            *slot = byte;
            timeline.after(BYTE);
        }
        if let Some(target) = self.targets.get_mut(&address) {
            target.smbus_alert_answered();
        }
        Ok(())
    }
}

/// Model names [`build`] knows.
pub const MODELS: &[&str] = &["ads1115", "at24", "mcp4728"];

/// Builds the model a chip file refers to.
///
/// # Errors
/// For an unknown model name or invalid parameters.
pub fn build(sim: &Sim) -> Result<Box<dyn I2cTarget>, String> {
    let param = |name: &str| {
        sim.params
            .get(name)
            .and_then(|v| usize::try_from(*v).ok())
            .ok_or_else(|| format!("sim model {} needs parameter `{name}`", sim.model))
    };
    match sim.model.as_str() {
        "ads1115" => Ok(Box::new(Ads1115::new())),
        "mcp4728" => Ok(Box::new(Mcp4728::new())),
        "at24" => {
            let (size, page) = (param("size")?, param("page")?);
            let mut eeprom = At24::new(size, page).ok_or_else(|| format!("at24: size {size} and page {page} must be powers of two, page <= size, size <= 64 KiB"))?;
            // Optional: write cycle time t_WR in microseconds (default 5 ms).
            if let Some(us) = sim.params.get("write_cycle_us") {
                eeprom.set_write_cycle(Duration::from_micros(*us));
            }
            Ok(Box::new(eeprom))
        }
        other => Err(format!(
            "unknown sim model `{other}` (known: {})",
            MODELS.join(", ")
        )),
    }
}
