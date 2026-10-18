// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! The set of chips: built-in data plus project-local chip files.

use std::collections::BTreeMap;
use std::fmt;

use crate::chip::{Chip, Package};

/// Built-in chip files: (path, contents). `tests/data.rs` checks that every file under `chips/`
/// is listed.
pub const BUILTIN: &[(&str, &str)] = &[
    (
        "chips/eeprom/24c32.toml",
        include_str!("../chips/eeprom/24c32.toml"),
    ),
    (
        "chips/microchip/mcp4728.toml",
        include_str!("../chips/microchip/mcp4728.toml"),
    ),
    (
        "chips/ti/ads1115.toml",
        include_str!("../chips/ti/ads1115.toml"),
    ),
];

/// A chip file that failed to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Where the data came from.
    pub origin: String,
    /// What is wrong, one sentence per problem.
    pub problems: Vec<String>,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.origin, self.problems.join("; "))
    }
}

impl std::error::Error for Error {}

/// A chip in one package: what a part number resolves to.
#[derive(Debug, Clone, Copy)]
pub struct Part<'a> {
    /// The chip.
    pub chip: &'a Chip,
    /// The package the part number selects.
    pub package: &'a Package,
}

impl Part<'_> {
    /// "TI ADS1115, VSSOP-10".
    pub fn name(&self) -> String {
        format!("{}, {}", self.chip.name, self.package.name)
    }

    /// Pin number of a role in this package.
    pub fn pin(&self, role: &str) -> Option<&str> {
        self.package.pins.get(role).map(String::as_str)
    }
}

/// A set of chips. Later additions take precedence on equal part number prefixes.
#[derive(Debug, Clone, Default)]
pub struct Library {
    chips: Vec<(String, Chip)>,
}

impl Library {
    /// The built-in chips.
    ///
    /// # Errors
    /// Only if the embedded data is invalid, which `tests/data.rs` rules out.
    pub fn builtin() -> Result<Self, Error> {
        let mut library = Self::default();
        for (path, text) in BUILTIN {
            library.add_toml(text, path)?;
        }
        Ok(library)
    }

    /// Parses, validates and adds one chip file.
    ///
    /// # Errors
    /// Fails on invalid TOML, unknown fields or consistency problems.
    pub fn add_toml(&mut self, text: &str, origin: &str) -> Result<(), Error> {
        let fail = |problems| Error {
            origin: origin.to_owned(),
            problems,
        };
        let chip: Chip = toml::from_str(text).map_err(|e| fail(vec![e.to_string()]))?;
        let problems = chip.problems();
        if !problems.is_empty() {
            return Err(fail(problems));
        }
        self.chips.push((origin.to_owned(), chip));
        Ok(())
    }

    /// All chips with their origin.
    pub fn chips(&self) -> impl Iterator<Item = (&str, &Chip)> {
        self.chips
            .iter()
            .map(|(origin, chip)| (origin.as_str(), chip))
    }

    /// Part number prefixes claimed by more than one package, with their claimants.
    pub fn overlaps(&self) -> BTreeMap<String, Vec<String>> {
        let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (_, chip) in &self.chips {
            for package in &chip.package {
                for mpn in &package.mpn {
                    seen.entry(mpn.to_ascii_uppercase())
                        .or_default()
                        .push(format!("{}, {}", chip.name, package.name));
                }
            }
        }
        seen.retain(|_, who| who.len() > 1);
        seen
    }

    /// The package whose prefix matches `mpn` longest (case-insensitive). On equal length, the
    /// most recently added chip wins, so project files override built-in data.
    pub fn lookup(&self, mpn: &str) -> Option<Part<'_>> {
        let mpn = mpn.trim().to_ascii_uppercase();
        let mut best: Option<(Part<'_>, usize)> = None;
        for (_, chip) in &self.chips {
            for package in &chip.package {
                for prefix in &package.mpn {
                    let len = prefix.len();
                    if mpn.starts_with(&prefix.to_ascii_uppercase())
                        && best.is_none_or(|(_, best_len)| len >= best_len)
                    {
                        best = Some((Part { chip, package }, len));
                    }
                }
            }
        }
        best.map(|(part, _)| part)
    }
}
