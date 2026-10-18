// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! embedded-hal 1.0 `I2c` for [`I2cBus`]: Rust drivers written for MCUs run against the models.

use embedded_hal::i2c::{
    self, ErrorKind, ErrorType, NoAcknowledgeSource, Operation, SevenBitAddress,
};

use super::{I2cBus, Message, Nack};

impl i2c::Error for Nack {
    fn kind(&self) -> ErrorKind {
        ErrorKind::NoAcknowledge(NoAcknowledgeSource::Unknown)
    }
}

impl ErrorType for I2cBus {
    type Error = Nack;
}

impl i2c::I2c<SevenBitAddress> for I2cBus {
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
    }
}
