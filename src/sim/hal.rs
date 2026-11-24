// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! embedded-hal 1.0 `I2c` for [`I2cBus`]: Rust drivers written for MCUs run against the models.
//! [`VirtualClock`] implements `DelayNs`, so a driver's delays move the same virtual time the
//! bus and the models use.

use std::time::Duration;

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::{
    self, ErrorKind, ErrorType, NoAcknowledgeSource, Operation, SevenBitAddress,
};

use super::{I2cBus, Message, Nacked, VirtualClock};

impl DelayNs for VirtualClock {
    fn delay_ns(&mut self, ns: u32) {
        self.advance(Duration::from_nanos(u64::from(ns)));
    }
}

/// The embedded-hal error of [`I2cBus`]: says whether the address or a data byte was not
/// acknowledged (a busy EEPROM leaves its address unacknowledged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HalNack(Nacked);

impl HalNack {
    /// Whether the address was not acknowledged (no target, or the target is busy).
    pub fn address(&self) -> bool {
        self.0 == Nacked::Address
    }
}

impl std::fmt::Display for HalNack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Nacked::Address => f.write_str("address not acknowledged"),
            Nacked::Data => f.write_str("data byte not acknowledged"),
        }
    }
}

impl std::error::Error for HalNack {}

impl i2c::Error for HalNack {
    /// @relation(PRT-003, scope=function)
    fn kind(&self) -> ErrorKind {
        ErrorKind::NoAcknowledge(match self.0 {
            Nacked::Address => NoAcknowledgeSource::Address,
            Nacked::Data => NoAcknowledgeSource::Data,
        })
    }
}

impl ErrorType for I2cBus {
    type Error = HalNack;
}

impl i2c::I2c<SevenBitAddress> for I2cBus {
    /// @relation(PRT-002, scope=function)
    fn transaction(
        &mut self,
        address: SevenBitAddress,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        let mut messages: Vec<Message<'_>> = operations
            .iter_mut()
            .map(|op| match op {
                Operation::Write(bytes) => Message::Write(bytes),
                Operation::Read(buffer) => Message::Read(buffer),
            })
            .collect();
        // embedded-hal: adjacent operations of the same kind have no repeated start between them.
        self.run(u16::from(address), &mut messages, true)
            .map_err(HalNack)
    }
}
