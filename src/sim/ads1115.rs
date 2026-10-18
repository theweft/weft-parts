// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! TI ADS1115 model: register file, conversion modes, input multiplexer and gain.
//!
//! Protocol: a write sets the 2-bit address pointer; two more bytes write that 16-bit register
//! (MSB first). A read returns the register at the pointer, MSB first. Conversions complete
//! instantly. Reads past the second byte return 0xFF (model choice, not from the datasheet).

use super::{I2cTarget, Nack};

const CONVERSION: u8 = 0;
const CONFIG: u8 = 1;
const LO_THRESH: u8 = 2;
const HI_THRESH: u8 = 3;

/// Config register at power-up.
pub const CONFIG_RESET: u16 = 0x8583;
const OS: u16 = 1 << 15;
const MODE_SINGLE: u16 = 1 << 8;

/// Full-scale range in volts per PGA setting (bits 11:9).
const FSR: [f64; 8] = [6.144, 4.096, 2.048, 1.024, 0.512, 0.256, 0.256, 0.256];

/// ADS1115 state.
#[derive(Debug, Clone)]
pub struct Ads1115 {
    pointer: u8,
    config: u16,
    conversion: u16,
    lo_thresh: u16,
    hi_thresh: u16,
    /// AIN0..AIN3 in volts, relative to GND.
    inputs: [f64; 4],
    write_index: usize,
    write_high: u8,
    read_index: usize,
    read_value: u16,
}

impl Default for Ads1115 {
    fn default() -> Self {
        Self::new()
    }
}

impl Ads1115 {
    /// A chip after power-up: all inputs at 0 V.
    pub fn new() -> Self {
        Self {
            pointer: 0,
            config: CONFIG_RESET,
            conversion: 0,
            lo_thresh: 0x8000,
            hi_thresh: 0x7fff,
            inputs: [0.0; 4],
            write_index: 0,
            write_high: 0,
            read_index: 0,
            read_value: 0,
        }
    }

    /// Sets the voltage on AIN`channel` (0..=3); other channels are ignored.
    pub fn set_input(&mut self, channel: usize, volts: f64) {
        if let Some(input) = self.inputs.get_mut(channel) {
            *input = volts;
        }
    }

    /// The config register as the controller would read it.
    pub fn config(&self) -> u16 {
        // Conversions are instant, so OS always reads 1 (not converting).
        self.config | OS
    }

    /// The last conversion result.
    pub fn conversion(&self) -> i16 {
        i16::from_be_bytes(self.conversion.to_be_bytes())
    }

    fn continuous(&self) -> bool {
        self.config & MODE_SINGLE == 0
    }

    /// Converts the currently selected input with the current gain.
    #[allow(clippy::cast_possible_truncation)] // clamped to the i16 range first
    fn convert(&mut self) {
        let [a0, a1, a2, a3] = self.inputs;
        let volts = match (self.config >> 12) & 7 {
            0 => a0 - a1,
            1 => a0 - a3,
            2 => a1 - a3,
            3 => a2 - a3,
            4 => a0,
            5 => a1,
            6 => a2,
            _ => a3,
        };
        let fsr = FSR
            .get(usize::from((self.config >> 9) & 7))
            .copied()
            .unwrap_or(2.048);
        let code = (volts / fsr * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        self.conversion = u16::from_be_bytes(code.to_be_bytes());
    }

    fn register(&mut self, pointer: u8) -> u16 {
        match pointer {
            CONVERSION => {
                if self.continuous() {
                    self.convert();
                }
                self.conversion
            }
            CONFIG => self.config(),
            LO_THRESH => self.lo_thresh,
            _ => self.hi_thresh,
        }
    }

    fn set_register(&mut self, pointer: u8, value: u16) {
        match pointer {
            CONFIG => {
                self.config = value & !OS;
                if self.continuous() || value & OS != 0 {
                    self.convert();
                }
            }
            LO_THRESH => self.lo_thresh = value,
            HI_THRESH => self.hi_thresh = value,
            _ => {} // the conversion register is read-only
        }
    }
}

impl I2cTarget for Ads1115 {
    fn start(&mut self, read: bool) {
        if read {
            self.read_index = 0;
            self.read_value = self.register(self.pointer);
        } else {
            self.write_index = 0;
        }
    }

    fn write(&mut self, byte: u8) -> Result<(), Nack> {
        match self.write_index {
            0 => self.pointer = byte & 0x03,
            1 => self.write_high = byte,
            2 => self.set_register(self.pointer, u16::from_be_bytes([self.write_high, byte])),
            _ => {}
        }
        self.write_index = self.write_index.saturating_add(1);
        Ok(())
    }

    fn read(&mut self) -> u8 {
        let [high, low] = self.read_value.to_be_bytes();
        let byte = match self.read_index {
            0 => high,
            1 => low,
            _ => 0xff,
        };
        self.read_index = self.read_index.saturating_add(1);
        byte
    }
}
