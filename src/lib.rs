// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! Machine-readable chip data and behavioural models.
//!
//! One TOML file per chip under `chips/` records what tools need to know about the chip:
//!
//! 1. **Board checks**: packages, part numbers, pinouts and how the I2C address is strapped
//!    (used by `weft-boardchecker` against IPC-2581 netlists).
//! 2. **Device tree checks**: compatible strings, the Linux binding and driver, and which pin each
//!    GPIO property refers to.
//! 3. **Simulation**: which model in [`sim`] implements the chip's bus behaviour, and what it
//!    covers. The models run behind Linux-style transfers or the embedded-hal `I2c` trait.
//!
//! See [`Library`] to load and look up chips, and [`sim::I2cBus`] to run models.
#![doc(html_logo_url = "https://theweft.org/brand/repos/weft-parts.svg")]
#![doc(html_favicon_url = "https://theweft.org/favicon.svg")]

pub mod chip;
pub mod library;
pub mod sim;

pub use chip::{Address, BusKind, Chip, DeviceTree, Package};
pub use library::{Error, Library, Part};
