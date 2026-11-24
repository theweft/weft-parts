// SPDX-FileCopyrightText: 2026 The weft authors
// SPDX-License-Identifier: MIT

//! KiCad netlist importer: the S-expression netlist `kicad-cli sch export netlist` writes by
//! default (`--format kicadsexpr`, version "E", checked with KiCad 10.0.8).
//!
//! Reads every component (reference, value, all fields) and every net node (pin number, pin name,
//! pin type). The part number comes from the first non-empty field among the names given, since
//! projects keep it in different fields (`MPN`, `Manufacturer_Part_Number`, or a generic `Field6`
//! as on the Raspberry Pi CM5 IO Board).

use super::{Board, PinInfo};

/// Field names tried for the part number when the caller names none.
pub const MPN_FIELDS: &[&str] = &[
    "MPN",
    "Manufacturer Part Number",
    "Manufacturer_Part_Number",
    "Manufacturer P/N",
    "Mfr. No",
    "MFR_PN",
];

/// Maximum nesting depth of the S-expression (real netlists stay below 10).
const MAX_DEPTH: usize = 64;

/// A netlist that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// What is wrong.
    pub message: String,
    /// Byte offset in the input, where known.
    pub offset: Option<usize>,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.offset {
            Some(offset) => write!(f, "KiCad netlist, byte {offset}: {}", self.message),
            None => write!(f, "KiCad netlist: {}", self.message),
        }
    }
}

impl std::error::Error for Error {}

fn error(message: &str, offset: Option<usize>) -> Error {
    Error {
        message: message.to_owned(),
        offset,
    }
}

/// One S-expression: a list or an atom (bare or quoted).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sexp {
    List(Vec<Sexp>),
    Atom(String),
}

impl Sexp {
    /// The head atom of a list, such as `comp` in `(comp (ref "R1") …)`.
    fn head(&self) -> Option<&str> {
        match self {
            Self::List(items) => match items.first() {
                Some(Self::Atom(head)) => Some(head),
                _ => None,
            },
            Self::Atom(_) => None,
        }
    }

    /// Child lists with head `name`.
    fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Sexp> + 'a {
        let items: &[Sexp] = match self {
            Self::List(items) => items,
            Self::Atom(_) => &[],
        };
        items.iter().filter(move |s| s.head() == Some(name))
    }

    fn child<'a>(&'a self, name: &'a str) -> Option<&'a Sexp> {
        self.children(name).next()
    }

    /// The first atom after the head: `"R1"` in `(ref "R1")`.
    fn value(&self) -> Option<&str> {
        match self {
            Self::List(items) => items.iter().skip(1).find_map(|s| match s {
                Self::Atom(a) => Some(a.as_str()),
                Self::List(_) => None,
            }),
            Self::Atom(_) => None,
        }
    }

    /// The value of the child `name`: `(comp (ref "R1"))` -> `child_value("ref")` = `R1`.
    fn child_value<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        self.child(name)?.value()
    }
}

struct Reader<'a> {
    text: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn skip_space(&mut self) {
        while self.text.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn read(&mut self, depth: usize) -> Result<Sexp, Error> {
        self.skip_space();
        let start = self.at;
        match self.text.get(self.at) {
            None => Err(error("unexpected end of input", Some(start))),
            Some(b'(') => {
                if depth >= MAX_DEPTH {
                    return Err(error("nested too deeply", Some(start)));
                }
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    match self.text.get(self.at) {
                        None => return Err(error("unclosed list", Some(start))),
                        Some(b')') => {
                            self.at += 1;
                            return Ok(Sexp::List(items));
                        }
                        Some(_) => items.push(self.read(depth + 1)?),
                    }
                }
            }
            Some(b')') => Err(error("unexpected `)`", Some(start))),
            Some(b'"') => {
                self.at += 1;
                let mut out = Vec::new();
                loop {
                    match self.text.get(self.at) {
                        None => return Err(error("unclosed string", Some(start))),
                        Some(b'"') => {
                            self.at += 1;
                            break;
                        }
                        Some(b'\\') => {
                            let escaped = self
                                .text
                                .get(self.at + 1)
                                .ok_or_else(|| error("unclosed string", Some(start)))?;
                            out.push(match escaped {
                                b'n' => b'\n',
                                b't' => b'\t',
                                b'r' => b'\r',
                                other => *other,
                            });
                            self.at += 2;
                        }
                        Some(byte) => {
                            out.push(*byte);
                            self.at += 1;
                        }
                    }
                }
                String::from_utf8(out)
                    .map(Sexp::Atom)
                    .map_err(|_| error("string is not UTF-8", Some(start)))
            }
            Some(_) => {
                while self
                    .text
                    .get(self.at)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'(' | b')' | b'"'))
                {
                    self.at += 1;
                }
                let atom = self.text.get(start..self.at).unwrap_or_default();
                String::from_utf8(atom.to_vec())
                    .map(Sexp::Atom)
                    .map_err(|_| error("atom is not UTF-8", Some(start)))
            }
        }
    }
}

/// Reads a KiCad S-expression netlist into a [`Board`]. `mpn_fields` names the fields to take
/// the part number from, in order; empty means [`MPN_FIELDS`].
///
/// # Errors
/// When the text is not an S-expression netlist (`(export …)` with `components` and `nets`).
/// @relation(PRT-011, scope=function)
pub fn parse(text: &str, mpn_fields: &[&str]) -> Result<Board, Error> {
    let mut reader = Reader {
        text: text.as_bytes(),
        at: 0,
    };
    let root = reader.read(0)?;
    reader.skip_space();
    if reader.at != text.len() {
        return Err(error("text after the netlist", Some(reader.at)));
    }
    if root.head() != Some("export") {
        return Err(error("not a KiCad netlist: expected `(export …)`", None));
    }
    let mpn_fields = if mpn_fields.is_empty() {
        MPN_FIELDS
    } else {
        mpn_fields
    };

    let mut board = Board::new();
    let components = root
        .child("components")
        .ok_or_else(|| error("no `components` section", None))?;
    for comp in components.children("comp") {
        let refdes = comp
            .child_value("ref")
            .ok_or_else(|| error("component without `ref`", None))?;
        board.add_component(refdes);
        let Some(component) = board.components.get_mut(refdes) else {
            continue;
        };
        component.value = comp
            .child_value("value")
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        if let Some(fields) = comp.child("fields") {
            for field in fields.children("field") {
                let Some(name) = field.child_value("name") else {
                    continue;
                };
                // (field (name "Field6") "RK73H…"); a field without a value has only its name.
                let value = field.value().unwrap_or_default();
                component.fields.insert(name.to_owned(), value.to_owned());
            }
        }
        let mpn = mpn_fields
            .iter()
            .find_map(|f| component.fields.get(*f).filter(|v| !v.trim().is_empty()))
            .cloned();
        if let Some(mpn) = mpn {
            board.set_mpn(refdes, &mpn);
        }
    }

    let nets = root
        .child("nets")
        .ok_or_else(|| error("no `nets` section", None))?;
    for net in nets.children("net") {
        let name = net
            .child_value("name")
            .ok_or_else(|| error("net without `name`", None))?;
        for node in net.children("node") {
            let (Some(refdes), Some(pin)) = (node.child_value("ref"), node.child_value("pin"))
            else {
                return Err(error(
                    &format!("net {name}: node without `ref` or `pin`"),
                    None,
                ));
            };
            board.connect(name, refdes, pin);
            let info = PinInfo {
                name: node.child_value("pinfunction").map(str::to_owned),
                kind: node.child_value("pintype").map(str::to_owned),
            };
            if let Some(component) = board.components.get_mut(refdes) {
                component.pin_info.insert(pin.to_owned(), info);
            }
        }
    }
    Ok(board)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn strings_escapes_and_atoms() {
        let mut reader = Reader {
            text: br#"(a "b \"c\" \\ d" e1 (f))"#,
            at: 0,
        };
        assert_eq!(
            reader.read(0).unwrap(),
            Sexp::List(vec![
                Sexp::Atom("a".into()),
                Sexp::Atom(r#"b "c" \ d"#.into()),
                Sexp::Atom("e1".into()),
                Sexp::List(vec![Sexp::Atom("f".into())]),
            ])
        );
    }

    #[test]
    fn malformed_input_is_rejected() {
        for (text, message) in [
            ("(export", "unclosed list"),
            ("(export \"x)", "unclosed string"),
            (")", "unexpected `)`"),
            ("(export) x", "text after the netlist"),
            ("(design)", "not a KiCad netlist: expected `(export …)`"),
            ("(export (nets))", "no `components` section"),
        ] {
            assert_eq!(parse(text, &[]).unwrap_err().message, message, "{text}");
        }
        let deep = "(".repeat(100) + &")".repeat(100);
        assert_eq!(parse(&deep, &[]).unwrap_err().message, "nested too deeply");
    }
}
