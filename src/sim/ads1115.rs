// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! TI ADS1115 model: register file, conversion modes and timing, input multiplexer and gain,
//! comparator and the ALERT/RDY pin.
//!
//! Protocol: a write sets the 2-bit address pointer; two more bytes write that 16-bit register
//! (MSB first). A read returns the register at the pointer, MSB first. Reads past the second byte
//! return 0xFF (model choice, not from the datasheet).
//!
//! Timing (datasheet SBAS444E §7.4.2): a conversion takes 1/DR (data rate bits 7:5, 8 to 860
//! samples per second), optionally off by a deviation (the datasheet allows ±10 %).
//!
//! - Single-shot: writing OS = 1 in power-down wakes the chip (about 25 µs) and starts one
//!   conversion; then the chip powers down again. Writing OS = 1 during a conversion has no
//!   effect.
//! - Continuous: conversions run back to back. A config write lets the running conversion finish
//!   with the old settings; the new settings apply from the next one. Leaving power-down for
//!   continuous mode also takes the wake-up time (model choice: the datasheet states it for
//!   single-shot).
//! - OS reads 0 while the chip performs a conversion, so always in continuous mode.
//! - The conversion register holds the last completed conversion (0 after power-up).
//! - Inputs are sampled when a conversion completes, as the model knows them then: call
//!   [`I2cBus::sync`](super::I2cBus::sync) before changing an input so that earlier conversions
//!   see the old value.
//!
//! Comparator (§7.3.7 to §7.3.9): traditional (asserts above `Hi_thresh`, deasserts below
//! `Lo_thresh`) or window (asserts outside the thresholds), after 1, 2 or 4 successive conversions
//! beyond the thresholds (`COMP_QUE`), optionally latching until the conversion register is read or
//! an SMBus alert response picks this chip. `COMP_QUE` = 11 disables the comparator and leaves
//! ALERT/RDY high-impedance. With the `Hi_thresh` MSB set and the `Lo_thresh` MSB clear, ALERT/RDY is
//! a conversion-ready pin: an 8 µs pulse after each continuous conversion, or asserted after a
//! single-shot conversion until the next one starts (model choice for the end). The pin is open
//! drain; `COMP_POL` selects active low (default) or active high.

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
const COMP_MODE_WINDOW: u16 = 1 << 4;
const COMP_POL_HIGH: u16 = 1 << 3;
const COMP_LAT: u16 = 1 << 2;
const COMP_QUE: u16 = 0b11;

/// Samples per second per DR setting (bits 7:5).
const DATA_RATES: [u64; 8] = [8, 16, 32, 64, 128, 250, 475, 860];

/// Full-scale range in volts per PGA setting (bits 11:9).
const FSR: [f64; 8] = [6.144, 4.096, 2.048, 1.024, 0.512, 0.256, 0.256, 0.256];

/// Wake-up from power-down before a conversion starts.
const WAKE_UP: Duration = Duration::from_micros(25);
/// Width of the conversion-ready pulse in continuous mode.
const READY_PULSE: Duration = Duration::from_micros(8);

/// What the converter is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Powered down (single-shot mode, idle).
    Idle,
    /// One conversion with the active settings, completing at the given time; then power-down
    /// (or the pending settings).
    Single(SimTime),
    /// Conversions with the active settings complete at `origin + rate.cycles(n)`, `done` so far.
    Continuous {
        origin: SimTime,
        rate: ClockDomain,
        done: u64,
    },
}

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
    /// Config the running conversion uses (MUX, PGA, DR).
    active: u16,
    /// A config write arrived during a conversion; it applies when that conversion completes.
    pending: bool,
    /// Comparator: successive conversions beyond the thresholds.
    beyond_count: u8,
    /// Comparator output (logical, before polarity).
    alert: bool,
    /// Whether the last alert came from exceeding the high threshold (SMBus status bit).
    alert_high: bool,
    /// Completion time of the last conversion, for the conversion-ready pulse.
    last_done: Option<SimTime>,
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
            active: CONFIG_RESET,
            pending: false,
            beyond_count: 0,
            alert: false,
            alert_high: false,
            last_done: None,
        }
    }

    /// Sets the voltage on AIN`channel` (0..=3); other channels are ignored.
    pub fn set_input(&mut self, channel: usize, volts: f64) {
        if let Some(input) = self.inputs.get_mut(channel) {
            *input = volts;
        }
    }

    /// Makes conversions run `ppm` parts per million fast (positive) or slow; the datasheet allows
    /// ±10 % (±100 000 ppm). Applies to conversions started afterwards.
    pub fn set_rate_deviation_ppm(&mut self, ppm: i32) {
        self.deviation_ppm = ppm;
    }

    /// The conversion clock for the DR setting in `config` and the deviation.
    fn rate_of(&self, config: u16) -> ClockDomain {
        let hz = DATA_RATES
            .get(usize::from((config >> 5) & 7))
            .copied()
            .unwrap_or(128);
        ClockDomain::nominal(hz).with_deviation_ppm(self.deviation_ppm)
    }

    /// Time one conversion takes with the settings in the config register.
    pub fn conversion_time(&self) -> Duration {
        self.rate_of(self.config).cycles(1)
    }

    /// The config register as the controller would read it: OS is 0 while a conversion runs.
    pub fn config(&self) -> u16 {
        match self.state {
            State::Idle => self.config | OS,
            State::Single(_) | State::Continuous { .. } => self.config & !OS,
        }
    }

    /// The last conversion result.
    pub fn conversion(&self) -> i16 {
        i16::from_be_bytes(self.conversion.to_be_bytes())
    }

    /// Whether the comparator or conversion-ready output is asserted (before polarity).
    pub fn alert_asserted(&self) -> bool {
        if self.config & COMP_QUE == COMP_QUE {
            return false;
        }
        if self.ready_mode() {
            return match (self.state, self.last_done) {
                (State::Continuous { .. }, Some(t)) => self.now < t + READY_PULSE,
                (State::Idle, Some(_)) => true,
                _ => false,
            };
        }
        self.alert
    }

    /// Whether the open-drain ALERT/RDY pin pulls its net low now. False when disabled
    /// (high-impedance).
    /// @relation(PRT-008, scope=function)
    pub fn alert_pulls_low(&self) -> bool {
        if self.config & COMP_QUE == COMP_QUE {
            return false;
        }
        self.alert_asserted() != (self.config & COMP_POL_HIGH != 0)
    }

    fn ready_mode(&self) -> bool {
        self.hi_thresh & 0x8000 != 0 && self.lo_thresh & 0x8000 == 0
    }

    /// Converts the selected input with the active settings.
    #[allow(clippy::cast_possible_truncation)] // clamped to the i16 range first
    fn convert(&self) -> u16 {
        let [a0, a1, a2, a3] = self.inputs;
        let volts = match (self.active >> 12) & 7 {
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
            .get(usize::from((self.active >> 9) & 7))
            .copied()
            .unwrap_or(2.048);
        let code = (volts / fsr * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        u16::from_be_bytes(code.to_be_bytes())
    }

    /// A conversion completes at `t`: store the result and update the comparator.
    fn complete(&mut self, t: SimTime) {
        self.conversion = self.convert();
        self.last_done = Some(t);
        self.compare();
    }

    /// @relation(PRT-008, scope=function)
    fn compare(&mut self) {
        let que = self.config & COMP_QUE;
        if que == COMP_QUE || self.ready_mode() {
            self.beyond_count = 0;
            return;
        }
        let value = i16::from_be_bytes(self.conversion.to_be_bytes());
        let hi = i16::from_be_bytes(self.hi_thresh.to_be_bytes());
        let lo = i16::from_be_bytes(self.lo_thresh.to_be_bytes());
        let window = self.config & COMP_MODE_WINDOW != 0;
        let (beyond, high) = if value > hi {
            (true, true)
        } else if window && value < lo {
            (true, false)
        } else {
            (false, false)
        };
        let needed = match que {
            0 => 1,
            1 => 2,
            _ => 4,
        };
        if beyond {
            self.beyond_count = self.beyond_count.saturating_add(1);
            if self.beyond_count >= needed {
                self.alert = true;
                self.alert_high = high;
            }
        } else {
            self.beyond_count = 0;
            let latched = self.config & COMP_LAT != 0;
            // Traditional mode deasserts only below Lo_thresh; window mode inside the window.
            let clear = if window { true } else { value < lo };
            if !latched && clear {
                self.alert = false;
            }
        }
    }

    /// Applies the config register to the converter at `t` (after a write, or when a running
    /// conversion completes with a write pending).
    fn apply_config(&mut self, t: SimTime, wake: bool) {
        self.pending = false;
        self.active = self.config;
        let wake = if wake { WAKE_UP } else { Duration::ZERO };
        self.state = if self.config & MODE_SINGLE == 0 {
            State::Continuous {
                origin: t + wake,
                rate: self.rate_of(self.config),
                done: 0,
            }
        } else {
            State::Idle
        };
    }

    /// Completes conversions due by `now`.
    /// @relation(PRT-007, scope=function)
    fn catch_up(&mut self, now: SimTime) {
        self.now = self.now.max(now);
        loop {
            match self.state {
                State::Idle => return,
                State::Single(done_at) => {
                    if done_at > self.now {
                        return;
                    }
                    self.complete(done_at);
                    if self.pending {
                        // A continuous config written during the conversion starts now.
                        self.apply_config(done_at, false);
                    } else {
                        self.state = State::Idle;
                    }
                }
                State::Continuous { origin, rate, done } => {
                    let total = rate.cycles_in(self.now.saturating_duration_since(origin));
                    if total <= done {
                        return;
                    }
                    if self.pending {
                        let at = origin + rate.cycles(done + 1);
                        self.complete(at);
                        self.apply_config(at, false);
                        continue;
                    }
                    // Inputs are constant between events, so only the last few conversions can
                    // change the comparator (its queue is at most 4 deep); skip the rest.
                    let first = total.saturating_sub(4).max(done);
                    for n in first + 1..=total {
                        self.complete(origin + rate.cycles(n));
                    }
                    self.state = State::Continuous {
                        origin,
                        rate,
                        done: total,
                    };
                    return;
                }
            }
        }
    }

    fn register(&mut self, pointer: u8) -> u16 {
        match pointer {
            CONVERSION => {
                // Reading conversion data clears a latched alert.
                if self.config & COMP_LAT != 0 {
                    self.alert = false;
                }
                self.conversion
            }
            CONFIG => self.config(),
            LO_THRESH => self.lo_thresh,
            _ => self.hi_thresh,
        }
    }

    /// @relation(PRT-007, scope=function)
    fn set_register(&mut self, pointer: u8, value: u16) {
        match pointer {
            CONFIG => {
                self.config = value & !OS;
                match self.state {
                    State::Idle => {
                        if self.config & MODE_SINGLE == 0 {
                            self.apply_config(self.now, true);
                        } else if value & OS != 0 {
                            self.active = self.config;
                            self.state = State::Single(
                                self.now + WAKE_UP + self.rate_of(self.config).cycles(1),
                            );
                        }
                    }
                    // OS = 1 has no effect during a conversion; the rest applies after it.
                    State::Single(_) | State::Continuous { .. } => self.pending = true,
                }
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
        let pulse_end = match (self.state, self.last_done) {
            (State::Continuous { .. }, Some(t))
                if self.ready_mode() && self.now < t + READY_PULSE =>
            {
                Some(t + READY_PULSE)
            }
            _ => None,
        };
        let conversion = match self.state {
            State::Idle => None,
            State::Single(done_at) => Some(done_at),
            State::Continuous { origin, rate, done } => {
                Some(origin + rate.cycles(done.saturating_add(1)))
            }
        };
        [pulse_end, conversion].into_iter().flatten().min()
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

    fn smbus_alert(&self) -> Option<bool> {
        let comparator = self.config & COMP_QUE != COMP_QUE && !self.ready_mode();
        (comparator && self.alert).then_some(self.alert_high)
    }

    fn smbus_alert_answered(&mut self) {
        if self.config & COMP_LAT != 0 {
            self.alert = false;
        }
    }
}
