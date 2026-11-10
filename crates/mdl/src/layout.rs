//! Where each entry of a binary container sits: the positions and sizes
//! validation works out, for tools that show or check layouts (an
//! interface document, an editor's hover, `morphc inspect`).
//!
//! Positions count bits from the start of the entry's container: a
//! packet, a command packet, or a struct type wherever it's read. An
//! entry without `offset_bits` follows the one before, so one after an
//! entry whose size the data decides can start anywhere in a range.
//!
//! Two answers to "where does it start?" are given, because they answer
//! different questions:
//!
//! - [`Placement::static_offset_bits`] is the static position of
//!   spec/decoding.md §5.2, a rule on how entries are written that
//!   identification and checksums depend on, so every decoder computes
//!   it the same way;
//! - [`Placement::start_min_bits`] and [`Placement::start_max_bits`] are
//!   the earliest and latest bits the entry can start at, worked out from
//!   every size the entries before it can have.
//!
//! An entry with a static position has exactly one possible start. The
//! converse doesn't hold: after a conditional entry that is always empty
//! (an array of no elements, say), the next entry has one possible start
//! but no static position.
//!
//! ```
//! use mdl::{layout, read::read_bundle, validate::validate};
//!
//! let bundle = read_bundle(br#"{
//!   "mdl_version": "2.0",
//!   "catalog": { "params": { "eps.v": { "type": "u64" } } },
//!   "families": [{ "name": "sat", "encoding": "binary", "byte_order": "big",
//!                  "alias": { "kind": "fixed", "value": "SAT1" } }],
//!   "sets": [{ "name": "sat", "family": "sat", "packets": [{ "name": "HK", "entries": [
//!     { "kind": "field", "name": "label", "type": "string", "length": { "kind": "terminator", "byte": 0 } },
//!     { "kind": "field", "name": "v", "type": "uint", "size_bits": 16, "canonical": "eps.v" }
//!   ] }] }]
//! }"#).unwrap().value;
//! let validated = validate(bundle).unwrap();
//! let set = layout::of_set(&validated, "sat").unwrap();
//! let hk = set.packet("HK").unwrap();
//! assert_eq!(hk.entries[0].static_offset_bits, Some(0));
//! // After a string of any length: from bit 8 (an empty string's
//! // terminator) on, with no bound.
//! let v = &hk.entries[1];
//! assert_eq!(v.static_offset_bits, None);
//! assert_eq!((v.start_min_bits, v.start_max_bits), (8, None));
//! assert_eq!(v.size_bits(), Some(16));
//! ```

use std::collections::BTreeMap;

use thiserror::Error;

use crate::ir::{Encoding, Entry};
use crate::validate::{Validated, layout_of};

/// Where one entry can be, in bits from its container's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Placement {
    /// Its static position (spec/decoding.md §5.2), when no message can
    /// move it. An entry can have one possible start and still no static
    /// position (see the [module docs](self)).
    pub static_offset_bits: Option<u64>,
    /// The earliest bit it can start at.
    pub start_min_bits: u64,
    /// The latest bit it can start at; none when earlier data can move it
    /// without bound.
    pub start_max_bits: Option<u64>,
    /// Its smallest size when present.
    pub size_min_bits: u64,
    /// Its largest size when present; none when unbounded.
    pub size_max_bits: Option<u64>,
    /// Whether a condition can leave it out. What follows it then moves
    /// as if its size ran from nothing to `size_max_bits`.
    pub conditional: bool,
}

impl Placement {
    /// Its size when present, if that never varies.
    pub fn size_bits(&self) -> Option<u64> {
        (self.size_max_bits == Some(self.size_min_bits)).then_some(self.size_min_bits)
    }
}

/// One container: where each of its entries can be, and how far they
/// reach.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContainerLayout {
    /// The packet's, command packet's or struct type's name.
    pub name: String,
    /// One per entry of the container in the IR, in the same order,
    /// padding and checksums included. [`with_entries`](Self::with_entries)
    /// pairs them up.
    pub entries: Vec<Placement>,
    /// The fewest bits its entries can take: how far the entries that
    /// are always present reach.
    pub size_min_bits: u64,
    /// The most bits its entries can take; none when unbounded. A
    /// packet's declared `length` is apart from this: an `exact` length
    /// may be longer than its entries reach.
    pub size_max_bits: Option<u64>,
}

impl ContainerLayout {
    /// Each of `entries` (the container's entries in the IR) with its
    /// placement.
    pub fn with_entries<'a>(
        &'a self,
        entries: &'a [Entry],
    ) -> impl Iterator<Item = (&'a Entry, &'a Placement)> + 'a {
        entries.iter().zip(&self.entries)
    }
}

/// Every container of one binary set.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SetLayout {
    /// The set's packets, in its order.
    pub packets: Vec<ContainerLayout>,
    /// Its command packets, in its order: where the encoder writes each
    /// entry, by the same rules (a command packet has no declared
    /// length).
    pub commands: Vec<ContainerLayout>,
    /// Its struct types by name, positions counting from the struct's own
    /// start wherever it's read. A struct entry's placement in its
    /// container takes the type's size bounds.
    pub types: BTreeMap<String, ContainerLayout>,
}

impl SetLayout {
    pub fn packet(&self, name: &str) -> Option<&ContainerLayout> {
        self.packets.iter().find(|p| p.name == name)
    }

    pub fn command(&self, name: &str) -> Option<&ContainerLayout> {
        self.commands.iter().find(|c| c.name == name)
    }
}

/// Why a set has no layout.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum LayoutError {
    #[error("there's no set {set}")]
    UnknownSet { set: String },
    /// A self-describing (JSON) family's entries are found by pointer,
    /// not by position.
    #[error("set {set}'s family is self-describing, so its entries have no positions")]
    SelfDescribing { set: String },
    /// Validation passed a set its own layout walk can't place: a bug in
    /// mdl.
    #[error("set {set} validated, but its layout can't be worked out; please report this")]
    Unplaced { set: String },
}

/// The layout of the set named `set`.
pub fn of_set(validated: &Validated, set: &str) -> Result<SetLayout, LayoutError> {
    let bundle = validated.bundle();
    let unknown = || LayoutError::UnknownSet {
        set: set.to_string(),
    };
    let found = bundle
        .sets
        .iter()
        .find(|s| s.name == set)
        .ok_or_else(unknown)?;
    // Validation guarantees the family exists.
    let family = bundle
        .families
        .iter()
        .find(|f| f.name == found.family)
        .ok_or_else(unknown)?;
    if family.encoding != Encoding::Binary {
        return Err(LayoutError::SelfDescribing {
            set: set.to_string(),
        });
    }
    layout_of(found, family).ok_or_else(|| LayoutError::Unplaced {
        set: set.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::read::read_bundle_value;
    use crate::validate::validate;

    const FULL_BUNDLE: &str = include_str!("../tests/fixtures/full_bundle.json");

    fn validated(value: &Value) -> Validated {
        validate(read_bundle_value(value).unwrap().value).unwrap()
    }

    fn fixture() -> Validated {
        validated(&serde_json::from_str(FULL_BUNDLE).unwrap())
    }

    /// A one-set bundle of a binary family, with these packets and types.
    fn binary(packets: Value, types: Value) -> Value {
        json!({
            "mdl_version": "2.0",
            "catalog": { "params": { "a.v": { "type": "u64" } } },
            "families": [{ "name": "f", "encoding": "binary", "byte_order": "big",
                           "alias": { "kind": "fixed", "value": "X" } }],
            "sets": [{ "name": "s", "family": "f", "packets": packets, "types": types }]
        })
    }

    fn layout(packets: Value, types: Value) -> SetLayout {
        of_set(&validated(&binary(packets, types)), "s").unwrap()
    }

    fn uint(name: &str, bits: u32) -> Value {
        json!({ "kind": "field", "name": name, "type": "uint", "size_bits": bits })
    }

    fn when_zero(field: &str) -> Value {
        json!({ "kind": "compare", "field": field, "op": "eq", "value": 0 })
    }

    /// Each entry as (static offset, start min, start max, size min, size
    /// max).
    type Shown = (Option<u64>, u64, Option<u64>, u64, Option<u64>);

    fn shown(container: &ContainerLayout) -> Vec<Shown> {
        container
            .entries
            .iter()
            .map(|p| {
                (
                    p.static_offset_bits,
                    p.start_min_bits,
                    p.start_max_bits,
                    p.size_min_bits,
                    p.size_max_bits,
                )
            })
            .collect()
    }

    #[test]
    fn fixed_entries_have_static_positions() {
        let layout = layout(
            json!([{ "name": "P", "length": { "kind": "exact", "bytes": 5 }, "entries": [
                uint("a", 3), uint("b", 13),
                { "kind": "padding", "size_bits": 8 },
                { "kind": "field", "name": "c", "type": "uint", "size_bits": 8, "offset_bits": 24 }
            ] }]),
            json!({}),
        );
        let p = layout.packet("P").unwrap();
        assert_eq!(
            shown(p),
            [
                (Some(0), 0, Some(0), 3, Some(3)),
                (Some(3), 3, Some(3), 13, Some(13)),
                (Some(16), 16, Some(16), 8, Some(8)),
                (Some(24), 24, Some(24), 8, Some(8)),
            ]
        );
        // How far the entries reach, not the declared five bytes.
        assert_eq!((p.size_min_bits, p.size_max_bits), (32, Some(32)));
    }

    #[test]
    fn data_sized_and_conditional_entries_move_what_follows() {
        let layout = layout(
            json!([{ "name": "P", "length": { "kind": "at_least", "bytes": 2 }, "entries": [
                uint("n", 8),
                { "kind": "array", "name": "xs", "count": { "kind": "field", "field": "n" },
                  "element": { "kind": "scalar", "type": "uint", "size_bits": 8 } },
                { "kind": "field", "name": "flag", "type": "uint", "size_bits": 8,
                  "when": when_zero("n") },
                uint("tail", 8),
                { "kind": "field", "name": "rest", "type": "bytes",
                  "length": { "kind": "remaining" } }
            ] }]),
            json!({}),
        );
        let p = &layout.packets[0];
        assert_eq!(
            shown(p),
            [
                (Some(0), 0, Some(0), 8, Some(8)),
                // An 8-bit count: up to 255 bytes, so whatever follows
                // moves by up to 2040 bits.
                (Some(8), 8, Some(8), 0, Some(2040)),
                (None, 8, Some(2048), 8, Some(8)),
                (None, 8, Some(2056), 8, Some(8)),
                (None, 16, Some(2064), 0, None),
            ]
        );
        assert!(p.entries[2].conditional);
        assert!(!p.entries[3].conditional);
        assert_eq!(p.entries[2].size_bits(), Some(8));
        assert_eq!((p.size_min_bits, p.size_max_bits), (16, None));
    }

    #[test]
    fn struct_types_are_laid_out_from_their_own_start() {
        let layout = layout(
            json!([{ "name": "P", "entries": [
                uint("a", 8),
                { "kind": "struct", "name": "pos", "struct": "xy" },
                { "kind": "array", "name": "path", "count": { "kind": "fixed", "count": 2 },
                  "element": { "kind": "struct", "name": "xy" } },
                uint("b", 8)
            ] }]),
            json!({
                "xy": { "entries": [uint("x", 16), { "kind": "struct", "name": "z", "struct": "z" }] },
                "z": { "entries": [uint("z", 8)] }
            }),
        );
        assert_eq!(
            layout.types.keys().map(String::as_str).collect::<Vec<_>>(),
            ["xy", "z"]
        );
        assert_eq!(
            shown(&layout.types["xy"]),
            [
                (Some(0), 0, Some(0), 16, Some(16)),
                (Some(16), 16, Some(16), 8, Some(8))
            ]
        );
        assert_eq!(
            shown(layout.packet("P").unwrap()),
            [
                (Some(0), 0, Some(0), 8, Some(8)),
                (Some(8), 8, Some(8), 24, Some(24)),
                (Some(32), 32, Some(32), 48, Some(48)),
                (Some(80), 80, Some(80), 8, Some(8)),
            ]
        );
    }

    #[test]
    fn a_single_possible_start_isnt_always_a_static_position() {
        // Conditional entries that are always empty, and a struct whose
        // size bounds meet although §5.2 gives it no exact size.
        let empty_array = json!({ "kind": "array", "name": "none", "when": when_zero("n"),
            "count": { "kind": "fixed", "count": 0 },
            "element": { "kind": "scalar", "type": "uint", "size_bits": 8 } });
        let empty_string = json!({ "kind": "field", "name": "none", "type": "string",
            "when": when_zero("n"), "length": { "kind": "fixed", "bytes": 0 } });
        let empty_struct =
            json!({ "kind": "struct", "name": "none", "struct": "empty", "when": when_zero("n") });
        let types = json!({
            "empty": { "entries": [{ "kind": "array", "name": "xs",
                "count": { "kind": "fixed", "count": 0 },
                "element": { "kind": "scalar", "type": "uint", "size_bits": 8 } }] },
            "jump": { "entries": [
                uint("m", 8),
                { "kind": "array", "name": "xs", "count": { "kind": "field", "field": "m" },
                  "element": { "kind": "scalar", "type": "uint", "size_bits": 8 } },
                { "kind": "field", "name": "c", "type": "uint", "size_bits": 8,
                  "offset_bits": 2048 }
            ] }
        });
        for empty in [empty_array, empty_string, empty_struct] {
            let layout = layout(
                json!([{ "name": "P", "entries": [uint("n", 8), empty, uint("after", 8)] }]),
                types.clone(),
            );
            let after = layout.packets[0].entries[2];
            assert_eq!((after.start_min_bits, after.start_max_bits), (8, Some(8)));
            assert_eq!(after.static_offset_bits, None);
        }
        let layout = layout(
            json!([{ "name": "P", "entries": [
                { "kind": "struct", "name": "j", "struct": "jump" }, uint("d", 8)
            ] }]),
            types,
        );
        let jump = &layout.types["jump"];
        assert_eq!(jump.entries[2].static_offset_bits, Some(2048));
        assert_eq!((jump.size_min_bits, jump.size_max_bits), (2056, Some(2056)));
        let d = layout.packets[0].entries[1];
        assert_eq!((d.start_min_bits, d.start_max_bits), (2056, Some(2056)));
        assert_eq!(d.static_offset_bits, None);
    }

    #[test]
    fn command_packets_are_laid_out_as_the_encoder_writes_them() {
        let validated = fixture();
        let layout = of_set(&validated, "vendor_x_v2").unwrap();
        let load = layout.command("LOAD_TABLE").unwrap();
        let set = validated
            .bundle()
            .sets
            .iter()
            .find(|s| s.name == "vendor_x_v2")
            .unwrap();
        let packet = set
            .commands
            .iter()
            .find(|c| c.name == "LOAD_TABLE")
            .unwrap();
        let named: Vec<(String, Shown)> = load
            .with_entries(&packet.entries)
            .map(|(entry, p)| {
                let name = match entry {
                    Entry::Field(field) => field.name.clone(),
                    Entry::Checksum(checksum) => checksum.name.clone(),
                    _ => "?".to_string(),
                };
                let shown = (
                    p.static_offset_bits,
                    p.start_min_bits,
                    p.start_max_bits,
                    p.size_min_bits,
                    p.size_max_bits,
                );
                (name, shown)
            })
            .collect();
        let table_max = 65_535 * 8;
        assert_eq!(
            named,
            [
                ("opcode".to_string(), (Some(0), 0, Some(0), 8, Some(8))),
                ("slot".to_string(), (Some(8), 8, Some(8), 8, Some(8))),
                ("len".to_string(), (Some(16), 16, Some(16), 16, Some(16))),
                (
                    "table".to_string(),
                    (Some(32), 32, Some(32), 0, Some(table_max))
                ),
                (
                    "crc".to_string(),
                    (None, 32, Some(32 + table_max), 16, Some(16))
                ),
            ]
        );
    }

    #[test]
    fn every_binary_set_of_the_fixture_is_placed_entry_for_entry() {
        let validated = fixture();
        let bundle = validated.bundle();
        let mut checked = 0;
        for set in &bundle.sets {
            let family = bundle
                .families
                .iter()
                .find(|f| f.name == set.family)
                .unwrap();
            let layout = match of_set(&validated, &set.name) {
                Err(LayoutError::SelfDescribing { .. }) => {
                    assert_eq!(family.encoding, Encoding::Json, "{}", set.name);
                    continue;
                }
                other => other.unwrap(),
            };
            let pairs = set
                .packets
                .iter()
                .map(|p| (&p.entries, layout.packet(&p.name).unwrap()))
                .chain(
                    set.commands
                        .iter()
                        .map(|c| (&c.entries, layout.command(&c.name).unwrap())),
                )
                .chain(
                    set.types
                        .iter()
                        .map(|(name, t)| (&t.entries, &layout.types[name])),
                );
            for (entries, container) in pairs {
                assert_eq!(entries.len(), container.entries.len(), "{}", container.name);
                for p in &container.entries {
                    if let Some(offset) = p.static_offset_bits {
                        assert_eq!((p.start_min_bits, p.start_max_bits), (offset, Some(offset)));
                        checked += 1;
                    }
                    assert!(p.start_max_bits.is_none_or(|max| p.start_min_bits <= max));
                    assert!(p.size_max_bits.is_none_or(|max| p.size_min_bits <= max));
                }
            }
        }
        assert!(checked > 10, "{checked}");
        assert_eq!(
            of_set(&validated, "nope"),
            Err(LayoutError::UnknownSet {
                set: "nope".to_string()
            })
        );
    }
}
