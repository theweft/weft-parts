// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! 24Cxx serial EEPROM model with a 2-byte memory address.
//!
//! A write sends the address (MSB first), then data that wraps within the page. Reads continue
//! from the internal address counter and wrap at the end of memory. Erased cells read 0xFF.

use super::{I2cTarget, Nack};

/// EEPROM state.
#[derive(Debug, Clone)]
pub struct At24 {
    memory: Vec<u8>,
    page: usize,
    pointer: usize,
    write_index: usize,
}

impl At24 {
    /// An erased EEPROM. `size` and `page` must be powers of two with `page <= size <= 65536`.
    pub fn new(size: usize, page: usize) -> Option<Self> {
        let valid =
            size.is_power_of_two() && page.is_power_of_two() && page <= size && size <= 65_536;
        valid.then(|| Self {
            memory: vec![0xff; size],
            page,
            pointer: 0,
            write_index: 0,
        })
    }

    /// Memory contents.
    pub fn memory(&self) -> &[u8] {
        &self.memory
    }

    /// Memory contents, for preloading (such as a HAT ID image).
    pub fn memory_mut(&mut self) -> &mut [u8] {
        &mut self.memory
    }

    fn mask(&self) -> usize {
        self.memory.len() - 1
    }
}

impl I2cTarget for At24 {
    fn start(&mut self, read: bool) {
        if !read {
            self.write_index = 0;
        }
    }

    fn write(&mut self, byte: u8) -> Result<(), Nack> {
        match self.write_index {
            0 => self.pointer = (usize::from(byte) << 8) & self.mask(),
            1 => self.pointer = (self.pointer | usize::from(byte)) & self.mask(),
            _ => {
                if let Some(cell) = self.memory.get_mut(self.pointer) {
                    *cell = byte;
                }
                let base = self.pointer & !(self.page - 1);
                self.pointer = base | ((self.pointer + 1) & (self.page - 1));
            }
        }
        self.write_index = self.write_index.saturating_add(1);
        Ok(())
    }

    fn read(&mut self) -> u8 {
        let byte = self.memory.get(self.pointer).copied().unwrap_or(0xff);
        self.pointer = (self.pointer + 1) & self.mask();
        byte
    }
}
