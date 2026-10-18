// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Behavioural chip models: what a chip does with the bytes on its bus.
//!
//! A model implements [`I2cTarget`] (or [`SpiTarget`]) at byte level. An [`I2cBus`] routes
//! transfers to models by address, with the message semantics of Linux `i2c_msg` lists, so the
//! same models can sit behind a virtio-i2c backend (Linux drivers in QEMU), behind the
//! embedded-hal `I2c` trait (Rust MCU drivers in host tests), or in plain unit tests.
//!
//! Analog values and pins are set and observed through each model's own methods
//! (for example [`Ads1115::set_input`] or [`Mcp4728::output_voltage`]).

mod ads1115;
mod at24;
#[cfg(feature = "embedded-hal")]
mod hal;
mod mcp4728;

use std::any::Any;
use std::collections::BTreeMap;

pub use ads1115::Ads1115;
pub use at24::At24;
pub use mcp4728::{Channel as Mcp4728Channel, Mcp4728};

use crate::chip::Sim;

/// The target did not acknowledge (unknown address or rejected byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nack;

impl std::fmt::Display for Nack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no acknowledge")
    }
}

impl std::error::Error for Nack {}

/// An I2C target (peripheral) at byte level.
pub trait I2cTarget: Any + Send {
    /// A start or repeated start addressed to this target; `read` is the R/W bit.
    fn start(&mut self, read: bool) {
        let _ = read;
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
}

/// An SPI target at byte level. No SPI models exist yet; the trait fixes the shape for them.
pub trait SpiTarget: Any + Send {
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

/// An I2C bus with models attached by address.
#[derive(Default)]
pub struct I2cBus {
    targets: BTreeMap<u16, Box<dyn I2cTarget>>,
}

impl std::fmt::Debug for I2cBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("I2cBus")
            .field("addresses", &self.targets.keys())
            .finish()
    }
}

impl I2cBus {
    /// An empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches a model at `address`.
    ///
    /// # Errors
    /// When another model already uses the address.
    pub fn attach(&mut self, address: u16, target: Box<dyn I2cTarget>) -> Result<(), AddressInUse> {
        if address == GENERAL_CALL || self.targets.contains_key(&address) {
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
    /// start, and a stop follows the last one. Writes to [`GENERAL_CALL`] go to every model.
    ///
    /// # Errors
    /// [`Nack`] when no model has the address or a model rejects a byte.
    pub fn transfer(&mut self, address: u16, messages: &mut [Message<'_>]) -> Result<(), Nack> {
        self.run(address, messages, false)
    }

    /// Runs a transfer where adjacent messages of the same direction continue each other without
    /// a repeated start (embedded-hal `transaction` semantics).
    pub(crate) fn run(
        &mut self,
        address: u16,
        messages: &mut [Message<'_>],
        merge_adjacent: bool,
    ) -> Result<(), Nack> {
        if address == GENERAL_CALL {
            let mut data = Vec::new();
            for message in messages.iter() {
                match message {
                    Message::Write(bytes) => data.extend_from_slice(bytes),
                    Message::Read(_) => return Err(Nack),
                }
            }
            for target in self.targets.values_mut() {
                target.general_call(&data);
            }
            return Ok(());
        }
        let target = self.targets.get_mut(&address).ok_or(Nack)?;
        let mut previous_read: Option<bool> = None;
        let mut result = Ok(());
        'messages: for message in messages.iter_mut() {
            let read = matches!(message, Message::Read(_));
            if !(merge_adjacent && previous_read == Some(read)) {
                target.start(read);
            }
            previous_read = Some(read);
            match message {
                Message::Write(bytes) => {
                    for byte in *bytes {
                        if let Err(nack) = target.write(*byte) {
                            result = Err(nack);
                            break 'messages;
                        }
                    }
                }
                Message::Read(buffer) => {
                    for byte in buffer.iter_mut() {
                        *byte = target.read();
                    }
                }
            }
        }
        target.stop();
        result
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
            At24::new(size, page)
                .map(|m| Box::new(m) as Box<dyn I2cTarget>)
                .ok_or_else(|| format!("at24: size {size} and page {page} must be powers of two, page <= size, size <= 64 KiB"))
        }
        other => Err(format!(
            "unknown sim model `{other}` (known: {})",
            MODELS.join(", ")
        )),
    }
}
