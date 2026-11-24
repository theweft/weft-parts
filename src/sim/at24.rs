// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! 24Cxx serial EEPROM model with a 2-byte memory address.
//!
//! A write sends the address (MSB first), then data that wraps within the page. Reads continue
//! from the internal address counter and wrap at the end of memory. Erased cells read 0xFF.
//!
//! Timing: a write with at least one data byte starts the internal write cycle at the stop
//! condition. Until it ends (`t_WR`, 5 ms by default: the 24LC32A datasheet maximum),
//! the chip does not acknowledge its address, which is what drivers poll for. An address-only
//! write (the first half of a random read) starts no cycle. The model always takes the full `t_WR`.

use std::time::Duration;

use super::{I2cTarget, Nack, SimTime};

/// Default write cycle time `t_WR`.
const DEFAULT_WRITE_CYCLE: Duration = Duration::from_millis(5);

/// EEPROM state.
#[derive(Debug, Clone)]
pub struct At24 {
    memory: Vec<u8>,
    page: usize,
    pointer: usize,
    write_index: usize,
    write_cycle: Duration,
    now: SimTime,
    /// End of the running write cycle, if one runs.
    busy_until: Option<SimTime>,
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
            write_cycle: DEFAULT_WRITE_CYCLE,
            now: SimTime::ZERO,
            busy_until: None,
        })
    }

    /// Sets the write cycle time `t_WR`.
    pub fn set_write_cycle(&mut self, t_wr: Duration) {
        self.write_cycle = t_wr;
    }

    /// Whether an internal write cycle is running.
    pub fn busy(&self) -> bool {
        self.busy_until.is_some()
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
    fn advance(&mut self, now: SimTime) {
        self.now = self.now.max(now);
        if self.busy_until.is_some_and(|end| end <= self.now) {
            self.busy_until = None;
        }
    }

    fn next_event(&self) -> Option<SimTime> {
        self.busy_until
    }

    fn start(&mut self, read: bool) -> Result<(), Nack> {
        if self.busy() {
            return Err(Nack);
        }
        if !read {
            self.write_index = 0;
        }
        Ok(())
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

    fn stop(&mut self) {
        if self.write_index > 2 {
            self.busy_until = Some(self.now + self.write_cycle);
        }
        self.write_index = 0;
    }
}
