// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! TI ADS1115 model: register file, conversion modes and timing, input multiplexer and gain.
//!
//! Protocol: a write sets the 2-bit address pointer; two more bytes write that 16-bit register
//! (MSB first). A read returns the register at the pointer, MSB first. Reads past the second byte
//! return 0xFF (model choice, not from the datasheet).
//!
//! Timing: a conversion takes 1/DR (data rate bits 7:5, 8 to 860 samples per second), optionally
//! off by a deviation (the datasheet allows ±10 %). Single-shot: writing OS = 1 in power-down
//! starts a conversion; OS reads 0 until it completes, then the result appears and the chip powers
//! down. Continuous: conversions run back to back from the config write; the conversion register
//! holds the last completed one. The input is sampled when a conversion completes (the model only
//! knows the input as last set). Model choices: every config write restarts continuous conversion,
//! OS reads 1 in continuous mode (the datasheet defines it for single-shot), and the wake-up time
//! from power-down is not added.

use std::time::Duration;

use super::{ClockDomain, I2cTarget, Nack, SimTime};

const CONVERSION: u8 = 0;
const CONFIG: u8 = 1;
const LO_THRESH: u8 = 2;
const HI_THRESH: u8 = 3;

/// Config register at power-up.
pub const CONFIG_RESET: u16 = 0x8583;
const OS: u16 = 1 << 15;
const MODE_SINGLE: u16 = 1 << 8;

/// Samples per second per DR setting (bits 7:5).
const DATA_RATES: [u64; 8] = [8, 16, 32, 64, 128, 250, 475, 860];

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
    now: SimTime,
    /// Rate deviation in ppm, applied to conversions started after it is set.
    deviation_ppm: i32,
    state: State,
}

/// What the converter is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Single-shot mode, powered down.
    Idle,
    /// Single-shot conversion that completes at the given time.
    Single(SimTime),
    /// Continuous conversion: conversions complete at `origin + rate.cycles(n)`, `done` so far.
    Continuous {
        origin: SimTime,
        rate: ClockDomain,
        done: u64,
    },
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
            now: SimTime::ZERO,
            deviation_ppm: 0,
            state: State::Idle,
        }
    }

    /// Makes conversions run `ppm` parts per million fast (positive) or slow; the datasheet allows
    /// ±10 % (±100 000 ppm). Applies to conversions started afterwards.
    pub fn set_rate_deviation_ppm(&mut self, ppm: i32) {
        self.deviation_ppm = ppm;
    }

    /// The conversion clock for the current DR setting and deviation.
    fn rate(&self) -> ClockDomain {
        let hz = DATA_RATES
            .get(usize::from((self.config >> 5) & 7))
            .copied()
            .unwrap_or(128);
        ClockDomain::nominal(hz).with_deviation_ppm(self.deviation_ppm)
    }

    /// Time one conversion takes with the current settings.
    pub fn conversion_time(&self) -> Duration {
        self.rate().cycles(1)
    }

    /// Sets the voltage on AIN`channel` (0..=3); other channels are ignored.
    pub fn set_input(&mut self, channel: usize, volts: f64) {
        if let Some(input) = self.inputs.get_mut(channel) {
            *input = volts;
        }
    }

    /// The config register as the controller would read it: OS is 0 while a single-shot
    /// conversion runs.
    pub fn config(&self) -> u16 {
        match self.state {
            State::Single(_) => self.config,
            State::Idle | State::Continuous { .. } => self.config | OS,
        }
    }

    /// The last conversion result.
    pub fn conversion(&self) -> i16 {
        i16::from_be_bytes(self.conversion.to_be_bytes())
    }

    fn continuous(&self) -> bool {
        self.config & MODE_SINGLE == 0
    }

    /// Completes conversions due by `now`.
    fn catch_up(&mut self, now: SimTime) {
        self.now = self.now.max(now);
        match self.state {
            State::Idle => {}
            State::Single(done_at) => {
                if done_at <= self.now {
                    self.convert();
                    self.state = State::Idle;
                }
            }
            State::Continuous { origin, rate, done } => {
                let total = rate.cycles_in(self.now.saturating_duration_since(origin));
                if total > done {
                    self.convert();
                    self.state = State::Continuous {
                        origin,
                        rate,
                        done: total,
                    };
                }
            }
        }
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
            CONVERSION => self.conversion,
            CONFIG => self.config(),
            LO_THRESH => self.lo_thresh,
            _ => self.hi_thresh,
        }
    }

    fn set_register(&mut self, pointer: u8, value: u16) {
        match pointer {
            CONFIG => {
                let busy = matches!(self.state, State::Single(_));
                self.config = value & !OS;
                self.state = if self.continuous() {
                    State::Continuous {
                        origin: self.now,
                        rate: self.rate(),
                        done: 0,
                    }
                } else if busy {
                    // OS = 1 has no effect during a conversion.
                    self.state
                } else if value & OS != 0 {
                    State::Single(self.now + self.conversion_time())
                } else {
                    State::Idle
                };
            }
            LO_THRESH => self.lo_thresh = value,
            HI_THRESH => self.hi_thresh = value,
            _ => {} // the conversion register is read-only
        }
    }
}

impl I2cTarget for Ads1115 {
    fn advance(&mut self, now: SimTime) {
        self.catch_up(now);
    }

    fn next_event(&self) -> Option<SimTime> {
        match self.state {
            State::Idle => None,
            State::Single(done_at) => Some(done_at),
            State::Continuous { origin, rate, done } => {
                Some(origin + rate.cycles(done.saturating_add(1)))
            }
        }
    }

    fn start(&mut self, read: bool) -> Result<(), Nack> {
        if read {
            self.read_index = 0;
            self.read_value = self.register(self.pointer);
        } else {
            self.write_index = 0;
        }
        Ok(())
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
