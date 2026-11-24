// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Microchip MCP4728 model: four 12-bit channels, each with an input register, an output
//! (analog) register and EEPROM.
//!
//! Output updates follow the datasheet: after a write when UDAC is 0 or LDAC is low, on a falling
//! LDAC edge, or on a general call software update. Factory EEPROM is assumed all zeros (VDD
//! reference, gain 1, normal mode, code 0). Reads past the 24th byte return 0xFF (model choice).
//! Unsupported commands (address bit write and read) are not acknowledged.
//!
//! EEPROM timing (datasheet DS22187E §5.6.3, §5.6.4): a single write starts programming the
//! EEPROM at the acknowledge of its last byte, a sequential write after the last channel (D, or
//! the last one sent if the stop comes earlier: model choice). Programming takes `T_WRITE`
//! (50 ms by default, the datasheet maximum; 25 ms typical). Meanwhile the RDY/BSY bit and the
//! open-drain RDY/BSY pin are low and every new command, general calls included, is acknowledged
//! but ignored; reads still work. The EEPROM holds the new values once programming ends.

use std::time::Duration;

use super::{I2cTarget, Nack, SimTime};

/// Default EEPROM write time `T_WRITE`: the datasheet maximum.
const DEFAULT_EEPROM_WRITE: Duration = Duration::from_millis(50);

const INTERNAL_REF: f64 = 2.048;

/// One channel's settings, as stored in each register.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Channel {
    /// Internal 2.048 V reference (VREF bit) instead of VDD.
    pub internal_ref: bool,
    /// Power-down bits: 0 normal, 1 = 1 kΩ, 2 = 100 kΩ, 3 = 500 kΩ to ground.
    pub power_down: u8,
    /// Gain 2 (only with the internal reference).
    pub gain2: bool,
    /// 12-bit code.
    pub value: u16,
}

impl Channel {
    fn from_bytes(high: u8, low: u8) -> Self {
        Self {
            internal_ref: high & 0x80 != 0,
            power_down: (high >> 5) & 0x03,
            gain2: high & 0x10 != 0,
            value: (u16::from(high & 0x0f) << 8) | u16::from(low),
        }
    }

    fn high_byte(self) -> u8 {
        let [value_high, _] = self.value.to_be_bytes();
        (u8::from(self.internal_ref) << 7)
            | (self.power_down << 5)
            | (u8::from(self.gain2) << 4)
            | (value_high & 0x0f)
    }
}

/// MCP4728 state.
#[derive(Debug, Clone)]
pub struct Mcp4728 {
    input: [Channel; 4],
    output: [Channel; 4],
    eeprom: [Channel; 4],
    address_bits: u8,
    vdd: f64,
    ldac_low: bool,
    command: Vec<u8>,
    read_index: usize,
    now: SimTime,
    eeprom_write: Duration,
    /// End of EEPROM programming, if it runs.
    busy_until: Option<SimTime>,
    /// EEPROM values of the current command, programmed once it ends.
    eeprom_pending: Vec<(usize, Channel)>,
    /// Commands arriving while busy are acknowledged and dropped.
    ignoring: bool,
}

impl Default for Mcp4728 {
    fn default() -> Self {
        Self::new()
    }
}

impl Mcp4728 {
    /// A chip after power-up with factory EEPROM, VDD = 3.3 V and LDAC tied low.
    pub fn new() -> Self {
        let eeprom = [Channel::default(); 4];
        Self {
            input: eeprom,
            output: eeprom,
            eeprom,
            address_bits: 0,
            vdd: 3.3,
            ldac_low: true,
            command: Vec::new(),
            read_index: 0,
            now: SimTime::ZERO,
            eeprom_write: DEFAULT_EEPROM_WRITE,
            busy_until: None,
            eeprom_pending: Vec::new(),
            ignoring: false,
        }
    }

    /// Sets the EEPROM write time `T_WRITE`.
    pub fn set_eeprom_write_time(&mut self, t_write: Duration) {
        self.eeprom_write = t_write;
    }

    /// Whether the EEPROM is being programmed (RDY/BSY low).
    pub fn busy(&self) -> bool {
        self.busy_until.is_some()
    }

    /// Whether the open-drain RDY/BSY pin pulls its net low (while programming).
    pub fn rdy_pulls_low(&self) -> bool {
        self.busy()
    }

    /// Starts programming the EEPROM with the pending values.
    /// @relation(PRT-010, scope=function)
    fn begin_eeprom_write(&mut self) {
        if !self.eeprom_pending.is_empty() && self.busy_until.is_none() {
            self.busy_until = Some(self.now + self.eeprom_write);
        }
    }

    /// Sets the supply voltage.
    pub fn set_vdd(&mut self, volts: f64) {
        self.vdd = volts;
    }

    /// Drives the LDAC pin. A falling edge copies all input registers to the outputs.
    pub fn set_ldac(&mut self, low: bool) {
        if low && !self.ldac_low {
            self.output = self.input;
        }
        self.ldac_low = low;
    }

    /// Input register of a channel (0..=3).
    pub fn input(&self, channel: usize) -> Option<Channel> {
        self.input.get(channel).copied()
    }

    /// EEPROM of a channel (0..=3).
    pub fn eeprom(&self, channel: usize) -> Option<Channel> {
        self.eeprom.get(channel).copied()
    }

    /// Analog output in volts, or `None` when the channel is powered down or out of range.
    pub fn output_voltage(&self, channel: usize) -> Option<f64> {
        let c = self.output.get(channel)?;
        if c.power_down != 0 {
            return None;
        }
        let code = f64::from(c.value) / 4096.0;
        let volts = if c.internal_ref {
            INTERNAL_REF * code * if c.gain2 { 2.0 } else { 1.0 }
        } else {
            self.vdd * code
        };
        Some(volts.min(self.vdd))
    }

    fn store(&mut self, channel: usize, value: Channel, eeprom: bool, update: bool) {
        if let Some(slot) = self.input.get_mut(channel) {
            *slot = value;
        }
        if eeprom && channel < 4 {
            self.eeprom_pending.push((channel, value));
        }
        if (update || self.ldac_low)
            && let Some(slot) = self.output.get_mut(channel)
        {
            *slot = value;
        }
    }

    /// Processes the command buffer after a byte arrived; acts when a unit is complete.
    fn on_byte(&mut self) -> Result<(), Nack> {
        let len = self.command.len();
        let Some(&first) = self.command.first() else {
            return Ok(());
        };
        let last2 = |c: &[u8]| -> Option<(u8, u8)> {
            let n = c.len();
            Some((*c.get(n.checked_sub(2)?)?, *c.get(n - 1)?))
        };
        match first >> 3 {
            // Fast write: pairs for channels A..D, setting power-down and code only.
            0b00000..=0b00111 => {
                if len.is_multiple_of(2) && len <= 8 {
                    let (high, low) = last2(&self.command).ok_or(Nack)?;
                    let channel = len / 2 - 1;
                    let mut value = self.input.get(channel).copied().unwrap_or_default();
                    value.power_down = (high >> 4) & 0x03;
                    value.value = (u16::from(high & 0x0f) << 8) | u16::from(low);
                    self.store(channel, value, false, false);
                }
            }
            // Multi-write: [command, high, low] per channel, each with its own command byte.
            0b01000 => {
                if len.is_multiple_of(3) {
                    let unit = self.command.get(len - 3..).ok_or(Nack)?;
                    let &[cmd, high, low] = <&[u8; 3]>::try_from(unit).map_err(|_| Nack)?;
                    if cmd >> 3 != 0b01000 {
                        return Err(Nack);
                    }
                    let channel = usize::from((cmd >> 1) & 0x03);
                    self.store(channel, Channel::from_bytes(high, low), false, cmd & 1 == 0);
                }
            }
            // Sequential write from the selected channel to D, with EEPROM.
            0b01010 => {
                if len >= 3 && !len.is_multiple_of(2) {
                    let (high, low) = last2(&self.command).ok_or(Nack)?;
                    let channel = usize::from((first >> 1) & 0x03) + (len - 3) / 2;
                    if channel <= 3 {
                        self.store(
                            channel,
                            Channel::from_bytes(high, low),
                            true,
                            first & 1 == 0,
                        );
                    }
                    if channel == 3 {
                        self.begin_eeprom_write();
                    }
                }
            }
            // Single write to one channel, with EEPROM.
            0b01011 => {
                if len == 3 {
                    let (high, low) = last2(&self.command).ok_or(Nack)?;
                    let channel = usize::from((first >> 1) & 0x03);
                    self.store(
                        channel,
                        Channel::from_bytes(high, low),
                        true,
                        first & 1 == 0,
                    );
                    self.begin_eeprom_write();
                }
            }
            // Select VREF / gain for all channels (bit 3 = A); applies immediately.
            0b10000 | 0b10001 | 0b11000 | 0b11001 => {
                if len == 1 {
                    for channel in 0..4 {
                        let bit = first & (0x08 >> channel) != 0;
                        for bank in [&mut self.input, &mut self.output] {
                            if let Some(c) = bank.get_mut(channel) {
                                if first >> 5 == 0b100 {
                                    c.internal_ref = bit;
                                } else {
                                    c.gain2 = bit;
                                }
                            }
                        }
                    }
                }
            }
            // Select power-down: A, B in byte 1; C, D in byte 2.
            0b10100 | 0b10101 => {
                if len == 2 {
                    let second = self.command.get(1).copied().unwrap_or(0);
                    let modes = [
                        (first >> 2) & 3,
                        first & 3,
                        (second >> 6) & 3,
                        (second >> 4) & 3,
                    ];
                    for (channel, mode) in modes.into_iter().enumerate() {
                        for bank in [&mut self.input, &mut self.output] {
                            if let Some(c) = bank.get_mut(channel) {
                                c.power_down = mode;
                            }
                        }
                    }
                }
            }
            _ => return Err(Nack),
        }
        Ok(())
    }
}

impl I2cTarget for Mcp4728 {
    /// @relation(PRT-010, scope=function)
    fn advance(&mut self, now: SimTime) {
        self.now = self.now.max(now);
        if self.busy_until.is_some_and(|end| end <= self.now) {
            self.busy_until = None;
            for (channel, value) in std::mem::take(&mut self.eeprom_pending) {
                if let Some(slot) = self.eeprom.get_mut(channel) {
                    *slot = value;
                }
            }
        }
    }

    fn next_event(&self) -> Option<SimTime> {
        self.busy_until
    }

    fn start(&mut self, read: bool) -> Result<(), Nack> {
        if read {
            self.read_index = 0;
        } else {
            self.command.clear();
            self.ignoring = self.busy();
        }
        Ok(())
    }

    fn write(&mut self, byte: u8) -> Result<(), Nack> {
        if self.ignoring {
            return Ok(());
        }
        self.command.push(byte);
        self.on_byte()
    }

    fn stop(&mut self) {
        // A sequential write that ended before channel D programs what it got.
        self.begin_eeprom_write();
    }

    fn read(&mut self) -> u8 {
        let index = self.read_index;
        self.read_index = self.read_index.saturating_add(1);
        if index >= 24 {
            return 0xff;
        }
        let channel = index / 6;
        let bank = if index % 6 < 3 {
            &self.input
        } else {
            &self.eeprom
        };
        let c = bank.get(channel).copied().unwrap_or_default();
        let [_, value_low] = c.value.to_be_bytes();
        match index % 3 {
            // RDY/BSY, POR (powered), channel, address bits.
            0 => {
                let ready = if self.busy() { 0 } else { 0x80 };
                ready | 0x40 | (u8::try_from(channel).unwrap_or(0) << 4) | self.address_bits
            }
            1 => c.high_byte(),
            _ => value_low,
        }
    }

    fn general_call(&mut self, data: &[u8]) {
        if self.busy() {
            return;
        }
        match data.first() {
            // Reset: load EEPROM into input and output registers.
            Some(0x06) => {
                self.input = self.eeprom;
                self.output = self.eeprom;
            }
            // Wake-up: clear power-down bits.
            Some(0x09) => {
                for c in self.input.iter_mut().chain(self.output.iter_mut()) {
                    c.power_down = 0;
                }
            }
            // Software update: copy input registers to outputs.
            Some(0x08) => self.output = self.input,
            _ => {}
        }
    }
}
