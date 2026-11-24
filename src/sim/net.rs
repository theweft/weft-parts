// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Digital nets: how pins drive a net and what level the net settles to.
//!
//! Wired logic (weft-design docs/21 §1): a strong low wins over released pins and pulls; a push-pull
//! high and a low at the same time is contention; with nothing driving, pull-ups and pull-downs
//! decide, and without them the net floats. Pulls come from resistors to a rail (resistor
//! networks only, docs/21 §8); opposite pulls on one net leave its digital level undefined.

/// How one pin acts on its net.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Drive {
    /// High-impedance: an input, a released open-drain output, a disabled pin.
    #[default]
    Released,
    /// Pulled low by a push-pull or open-drain output.
    Low,
    /// Driven high by a push-pull output.
    High,
}

/// A resistor from the net to a rail, or none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pull {
    /// No pull.
    #[default]
    None,
    /// Pulled up to a supply.
    Up,
    /// Pulled down to ground.
    Down,
    /// Pulled both ways: a divider, whose digital level the model does not decide.
    Both,
}

impl Pull {
    /// Adds another pull resistor on the same net.
    #[must_use]
    pub fn with(self, other: Self) -> Self {
        match (self, other) {
            (Self::None, p) | (p, Self::None) => p,
            (a, b) if a == b => a,
            _ => Self::Both,
        }
    }
}

/// The level a net settles to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetState {
    /// Low.
    Low,
    /// High.
    High,
    /// Nothing drives or pulls the net (or pulls disagree): inputs on it are undefined.
    Floating,
    /// A push-pull high fights a low: a board or firmware fault.
    Contention,
}

impl NetState {
    /// `Some(true)` for high, `Some(false)` for low, `None` when the level is undefined.
    pub fn level(self) -> Option<bool> {
        match self {
            Self::Low => Some(false),
            Self::High => Some(true),
            Self::Floating | Self::Contention => None,
        }
    }
}

/// Resolves the drives on one net and its pull.
/// @relation(PRT-012, scope=function)
pub fn resolve(drives: impl IntoIterator<Item = Drive>, pull: Pull) -> NetState {
    let (mut low, mut high) = (false, false);
    for drive in drives {
        match drive {
            Drive::Low => low = true,
            Drive::High => high = true,
            Drive::Released => {}
        }
    }
    match (low, high, pull) {
        (true, true, _) => NetState::Contention,
        (true, false, _) | (false, false, Pull::Down) => NetState::Low,
        (false, true, _) | (false, false, Pull::Up) => NetState::High,
        (false, false, Pull::None | Pull::Both) => NetState::Floating,
    }
}

/// Whether a model pin reads its net or drives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDir {
    /// The model reads the net ([`I2cTarget::pin_input`](super::I2cTarget::pin_input)).
    Input,
    /// The model drives the net ([`I2cTarget::pin_drive`](super::I2cTarget::pin_drive)).
    Output,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wired_logic() {
        use Drive::{High, Low, Released};
        assert_eq!(resolve([Released, Released], Pull::Up), NetState::High);
        assert_eq!(
            resolve([Released, Low], Pull::Up),
            NetState::Low,
            "open drain"
        );
        assert_eq!(resolve([Low, Low], Pull::Up), NetState::Low, "wired AND");
        assert_eq!(resolve([High, Low], Pull::Up), NetState::Contention);
        assert_eq!(resolve([High], Pull::Down), NetState::High);
        assert_eq!(resolve([Released], Pull::None), NetState::Floating);
        assert_eq!(Pull::Up.with(Pull::Down), Pull::Both);
        assert_eq!(resolve([], Pull::Up.with(Pull::Down)), NetState::Floating);
        assert_eq!(Pull::None.with(Pull::Up).with(Pull::Up), Pull::Up);
    }
}
