//! Layout: where entries are, how big they are, and whether they fit.
//!
//! A binary container (a packet or a struct type) is walked once, in order.
//! Each entry gets a *start range* (the earliest and latest bit it can start
//! at, the latest unbounded when it can't be known) and size bounds, plus
//! the set of bit positions modulo 8 it can start at. From these:
//!
//! - **Overlap** (decoding.md §5.2). Entries placed one after another
//!   never overlap, so each explicit `offset_bits` starts a new *run*, and
//!   the possible extents of entries in different runs are compared,
//!   whatever order they're listed in. Two unconditional entries that can
//!   read the same bits are an error. A conditional entry may overlap
//!   another only as an alternative reading: when both are conditional, or
//!   when one region contains the other.
//! - **Exact length.** In a packet of exact length, every entry's furthest
//!   possible end fits. Layouts whose size varies beyond that use
//!   `at_least`.
//! - **Alignment.** Strings, bytes, little-endian values wider than a byte
//!   and struct types containing any of those start on a byte for every
//!   possible start; a little-endian value narrower than a byte fits inside
//!   one byte. A value is little-endian by its own `byte_order`, else its
//!   family's. A `byte_order` on a value that lies inside one byte wherever
//!   it can start changes nothing, so it's refused. A struct type holding
//!   a value whose rules depend on where it starts within a byte starts on
//!   a byte itself, so positions checked inside the type are the ones it's
//!   read at.
//! - **References.** Counts, lengths and conditions use fields decoded
//!   before them, always present, of a type that suits the use.
//!
//! A struct's size, for the entries that follow it, is the furthest end of
//! its entries.
//!
//! An entry whose size can't be known because of an error reported for it
//! (an undefined struct type, an invalid size) stops position checks for
//! the rest of its container, so the error isn't echoed onto every later
//! entry. Self-describing containers get the shape and reference checks
//! that apply to them.

use std::collections::{BTreeMap, BTreeSet};

use super::MAX_NESTING;
use super::rules::{MAX_SAFE_INTEGER, check_int, check_integer_spelling, integer_value};
use crate::diagnostics::{Code, Place, Report};
use crate::ir::{
    ArrayEntry, ByteLength, ByteOrder, CompareOp, Condition, Conversion, Count, Element, Entry,
    Field, FromField, Length, Scalar, ScalarElement, ScalarType, StructType, TextEncoding,
};
use crate::layout::Placement;

/// Size bounds in bits. `max` is `None` when unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Size {
    pub(super) min: u64,
    pub(super) max: Option<u64>,
}

impl Size {
    fn fixed(bits: u64) -> Size {
        Size {
            min: bits,
            max: Some(bits),
        }
    }

    fn exact(self) -> Option<u64> {
        (self.max == Some(self.min)).then_some(self.min)
    }

    /// Present or absent: between nothing and the full size.
    fn optional(self) -> Size {
        Size {
            min: 0,
            max: self.max,
        }
    }
}

/// A set of bit positions modulo 8, one bit per residue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Residues(u8);

impl Residues {
    const ANY: Residues = Residues(0xff);
    const ALIGNED: Residues = Residues(1);

    fn at(bit: u64) -> Residues {
        Residues(1 << (bit % 8))
    }

    fn aligned(self) -> bool {
        self == Residues::ALIGNED
    }

    fn iter(self) -> impl Iterator<Item = u64> {
        (0..8).filter(move |r| self.0 & (1 << r) != 0)
    }

    /// Every `a + b` modulo 8.
    fn plus(self, other: Residues) -> Residues {
        let mut out = 0_u8;
        for a in self.iter() {
            for b in other.iter() {
                out |= 1 << ((a + b) % 8);
            }
        }
        Residues(out)
    }

    fn union(self, other: Residues) -> Residues {
        Residues(self.0 | other.0)
    }

    /// Every `k * step` modulo 8 for k from 0 up to `max_count`.
    fn multiples(step: u64, max_count: Option<u64>) -> Residues {
        let limit = max_count.map_or(8, |n| n.min(8));
        let mut out = Residues::ALIGNED;
        for k in 1..=limit {
            out = out.union(Residues::at(k * (step % 8)));
        }
        out
    }
}

/// What a container's users need to know about it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Shape {
    pub(super) size: Size,
    /// The container's size modulo 8, over every possible size.
    size_residues: Residues,
    /// It holds something that must start on a byte, so it does too.
    needs_byte: bool,
    /// It holds a value narrower than a byte whose place within a byte
    /// matters, so it starts on a byte: positions checked inside it are
    /// then the positions it's read at.
    holds_narrow: bool,
}

/// What every container in one set shares.
pub(super) struct Layout<'a> {
    pub(super) binary: bool,
    pub(super) byte_order: Option<ByteOrder>,
    /// Struct types checked so far; a type is checked before anything that
    /// uses it. A type missing here had an error reported.
    pub(super) types: BTreeMap<&'a str, Shape>,
}

/// One entry's size facts.
#[derive(Debug, Clone, Copy)]
struct Facts {
    /// Its size where the container's layout is concerned: from nothing
    /// for a conditional entry.
    size: Size,
    /// Its size when present.
    present: Size,
    size_residues: Residues,
    /// Must start on a byte.
    needs_byte: bool,
    /// A struct (or an array of them) whose type holds narrow values, so it
    /// starts on a byte.
    holds_narrow: bool,
    /// A little-endian value narrower than a byte: must fit inside one.
    narrow_le: Option<u64>,
    /// A value of at most 8 bits that names its own byte order: the order
    /// only matters if it can straddle a byte boundary.
    own_narrow: Option<u64>,
    /// An array that reads to the end of the packet, which ends on a byte,
    /// so it starts on one.
    to_end: bool,
    /// For an array: what each element needs where it starts.
    element: Option<ElementFacts>,
}

#[derive(Debug, Clone, Copy)]
struct ElementFacts {
    size: Size,
    /// The element size modulo 8: element k starts k steps after the first.
    step: Residues,
    needs_byte: bool,
    holds_narrow: bool,
    narrow_le: Option<u64>,
    own_narrow: Option<u64>,
    /// At most this many elements; `None` when unbounded.
    max_count: Option<u64>,
}

impl Facts {
    fn plain(size: Size, size_residues: Residues) -> Facts {
        Facts {
            size,
            present: size,
            size_residues,
            needs_byte: false,
            holds_narrow: false,
            narrow_le: None,
            own_narrow: None,
            to_end: false,
            element: None,
        }
    }

    /// Facts of a struct entry or element, from its type's shape.
    fn of_struct(shape: &Shape) -> Facts {
        Facts {
            needs_byte: shape.needs_byte,
            holds_narrow: shape.holds_narrow,
            ..Facts::plain(shape.size, shape.size_residues)
        }
    }

    /// Whether this entry is, or holds, a value narrower than a byte whose
    /// place within a byte matters: a struct holding it starts on a byte.
    fn is_narrow(&self) -> bool {
        self.holds_narrow
            || self.narrow_le.is_some()
            || self.own_narrow.is_some()
            || self
                .element
                .is_some_and(|e| e.holds_narrow || e.narrow_le.is_some() || e.own_narrow.is_some())
    }
}

/// An earlier field, for references.
struct Earlier<'a> {
    field: &'a Field,
    conditional: bool,
}

/// What an entry may refer to while its container is walked.
struct Scope<'a> {
    earlier: BTreeMap<&'a str, Earlier<'a>>,
    all: BTreeSet<&'a str>,
    /// A command packet, whose fields have no sentinels (command.rs
    /// reports any).
    command: bool,
}

impl Scope<'_> {
    /// A field's sentinels, as the value checks should see them.
    fn sentinels<'s>(&self, invalid: &'s [Scalar]) -> &'s [Scalar] {
        if self.command { &[] } else { invalid }
    }
}

/// What kind of container entries are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Container {
    Packet,
    StructType,
    /// A command packet: telemetry-only keys are reported by command.rs,
    /// so the shared checks leave them alone.
    Command,
}

/// Where an entry can be, for the overlap check.
struct Extent {
    /// Earliest possible start.
    start: u64,
    /// Latest possible start; `None` when unbounded.
    latest_start: Option<u64>,
    /// Smallest size when present.
    min_size: u64,
    /// Furthest possible end; `None` when unbounded.
    end: Option<u64>,
    /// Always exactly these bits (fixed start and size).
    exact: bool,
    conditional: bool,
    run: usize,
    index: usize,
}

impl Extent {
    fn end(&self) -> u64 {
        self.end.unwrap_or(u64::MAX)
    }

    fn overlaps(&self, other: &Extent) -> bool {
        self.start < other.end() && other.start < self.end()
    }

    /// Every placement of `other` lies within bits `self` covers in every
    /// placement where it's present: from its latest start to its earliest
    /// start plus its smallest size.
    fn surely_contains(&self, other: &Extent) -> bool {
        self.latest_start
            .is_some_and(|latest| latest <= other.start)
            && other.end() <= self.start.saturating_add(self.min_size)
    }
}

impl<'a> Layout<'a> {
    /// Check one container; return its shape, or `None` when an error
    /// reported for it makes its size unknown. `length` is a packet's
    /// declared length.
    pub(super) fn check_container(
        &self,
        entries: &'a [Entry],
        length: Option<&Length>,
        container: Container,
        place: &Place,
        report: &mut Report,
    ) -> Option<Shape> {
        self.place_container(entries, length, container, place, report)
            .0
    }

    /// [`check_container`](Self::check_container), also giving where each
    /// entry of a binary container can be: `None` for one whose size an
    /// error makes unknown, and for every entry after it. The static
    /// position is left for the caller.
    pub(super) fn place_container(
        &self,
        entries: &'a [Entry],
        length: Option<&Length>,
        container: Container,
        place: &Place,
        report: &mut Report,
    ) -> (Option<Shape>, Vec<Option<Placement>>) {
        let mut placements = Vec::with_capacity(entries.len());
        let in_struct = container == Container::StructType;
        let mut scope = Scope {
            earlier: BTreeMap::new(),
            all: entries.iter().filter_map(entry_name).collect(),
            command: container == Container::Command,
        };
        for (index, entry) in entries.iter().enumerate().filter(|_| !scope.command) {
            check_optional(
                entry,
                self.binary,
                &entry_place(place, index, entry),
                report,
            );
        }
        if !self.binary {
            for (index, entry) in entries.iter().enumerate() {
                let entry_place = entry_place(place, index, entry);
                self.check_json_entry(entry, &scope, &entry_place, report);
                remember(entry, &mut scope);
            }
            return (None, placements);
        }

        // Where the next sequential entry starts: earliest, latest.
        let mut cursor = (0_u64, Some(0_u64));
        let mut residues = Residues::ALIGNED;
        let mut run = 0;
        let mut explicit_jumps = false;
        // Where any entry can end, modulo 8: a struct with explicit offsets
        // ends at the furthest of these.
        let mut end_residues = Residues(0);
        let mut min_end = 0_u64;
        let mut known = true;
        let mut needs_byte = false;
        let mut holds_narrow = false;
        let mut to_the_end: Option<&str> = None;
        let mut extents: Vec<Extent> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let entry_place = entry_place(place, index, entry);
            let name = entry_name(entry).unwrap_or("padding");
            if let Some(earlier) = to_the_end {
                report.error(
                    Code::ReadsToEnd,
                    &entry_place,
                    format!(
                        "{name} comes after {earlier}, which reads to the end of the container"
                    ),
                );
            }
            if in_struct && reads_to_end(entry) {
                report.error(
                    Code::ReadsToEnd,
                    &entry_place,
                    "a struct type has no end of its own to read to; read to the end only at \
                     the top level of a packet",
                );
            }
            let (start, start_residues) = match explicit_offset(entry) {
                Some(offset) => {
                    run = index + 1;
                    explicit_jumps = true;
                    ((offset, Some(offset)), Residues::at(offset))
                }
                None => (cursor, residues),
            };
            let facts = self.entry_facts(entry, &scope, &entry_place, report);
            remember(entry, &mut scope);
            let Some(facts) = facts else {
                known = false;
                placements.push(None);
                continue;
            };
            if !known {
                placements.push(None);
                continue;
            }
            let conditional = entry_condition(entry).is_some();
            placements.push(Some(Placement {
                static_offset_bits: None,
                start_min_bits: start.0,
                start_max_bits: start.1,
                size_min_bits: facts.present.min,
                size_max_bits: facts.present.max,
                conditional,
            }));
            needs_byte |= facts.needs_byte;
            holds_narrow |= facts.is_narrow();
            self.check_alignment(name, &facts, start_residues, &entry_place, report);
            let end = start
                .1
                .zip(facts.size.max)
                .and_then(|(s, m)| s.checked_add(m));
            if let Some(Length::Exact { bytes }) = length {
                let limit = u64::from(*bytes) * 8;
                let past = match end {
                    Some(end) => (end > limit).then(|| format!("can reach bit {end}")),
                    // Reading to the end is bounded by the payload itself,
                    // and an entry unbounded only because an earlier one is
                    // has that one reported already.
                    None if facts.size.max.is_none() && !reads_to_end(entry) => {
                        Some("has no bound on where it ends".into())
                    }
                    None => None,
                };
                if let Some(past) = past {
                    report.error(
                        Code::OutOfBounds,
                        &entry_place,
                        format!(
                            "{name} {past}, past the packet's exact length of {bytes} bytes \
                             ({limit} bits); use an at_least length if the size varies"
                        ),
                    );
                }
            }
            if !conditional {
                min_end = min_end.max(start.0.saturating_add(facts.size.min));
            }
            if facts.size.max != Some(0) {
                extents.push(Extent {
                    start: start.0,
                    latest_start: start.1,
                    min_size: facts.size.min,
                    end,
                    exact: start.1 == Some(start.0) && facts.size.exact().is_some(),
                    conditional,
                    run,
                    index,
                });
            }
            cursor = (
                start.0.saturating_add(facts.size.min),
                start
                    .1
                    .zip(facts.size.max)
                    .and_then(|(s, m)| s.checked_add(m)),
            );
            residues = start_residues.plus(facts.size_residues);
            end_residues = end_residues.union(residues);
            if reads_to_end(entry) {
                to_the_end = Some(name);
            }
        }
        // Overlaps among the entries placed before any error are still real.
        report_overlaps(entries, &extents, place, report);
        if !known {
            return (None, placements);
        }
        // The furthest any entry can reach; unbounded if any can be.
        let max = cursor.1.and_then(|end| {
            extents
                .iter()
                .try_fold(end, |far, e| e.end.map(|x| far.max(x)))
        });
        let size = Size { min: min_end, max };
        let size_residues = match size.exact() {
            Some(bits) => Residues::at(bits),
            None if explicit_jumps => end_residues.union(residues),
            None => residues,
        };
        let shape = Shape {
            size,
            size_residues,
            needs_byte,
            holds_narrow,
        };
        (Some(shape), placements)
    }

    /// Size facts of one binary entry, checking its shape and references.
    /// `None` when an error reported here makes its size unknown.
    fn entry_facts(
        &self,
        entry: &Entry,
        scope: &Scope,
        place: &Place,
        report: &mut Report,
    ) -> Option<Facts> {
        let facts = match entry {
            Entry::Field(field) => {
                if field.pointer.is_some() {
                    report.error(
                        Code::WrongEncoding,
                        &place.key("pointer"),
                        "binary entries are located with offset_bits, not pointer",
                    );
                }
                check_values(
                    field.ty,
                    field.size_bits,
                    field.conversion.as_ref(),
                    scope.sentinels(&field.invalid),
                    place,
                    report,
                );
                self.scalar_facts(ScalarShape::of_field(field), Some(scope), place, report)?
            }
            Entry::Padding(padding) => {
                if padding.size_bits == 0 {
                    report.error(
                        Code::FieldSize,
                        &place.key("size_bits"),
                        "padding skips at least one bit",
                    );
                    return None;
                }
                let bits = u64::from(padding.size_bits);
                Facts::plain(Size::fixed(bits), Residues::at(bits))
            }
            Entry::Checksum(checksum) => {
                let bits = u64::from(checksum.algorithm.width_bits());
                let mut facts = Facts::plain(Size::fixed(bits), Residues::at(bits));
                facts.needs_byte = true;
                facts
            }
            Entry::Array(array) => self.array_facts(array, scope, place, report)?,
            Entry::Struct(entry) => {
                if entry.pointer.is_some() {
                    report.error(
                        Code::WrongEncoding,
                        &place.key("pointer"),
                        "binary entries are located with offset_bits, not pointer",
                    );
                }
                Facts::of_struct(self.types.get(entry.struct_type.as_str())?)
            }
        };
        Some(match entry_condition(entry) {
            Some(when) => {
                check_condition_refs(when, scope, &place.key("when"), 1, report);
                Facts {
                    size: facts.size.optional(),
                    size_residues: facts.size_residues.union(Residues::ALIGNED),
                    ..facts
                }
            }
            None => facts,
        })
    }

    fn array_facts(
        &self,
        array: &ArrayEntry,
        scope: &Scope,
        place: &Place,
        report: &mut Report,
    ) -> Option<Facts> {
        if array.pointer.is_some() {
            report.error(
                Code::WrongEncoding,
                &place.key("pointer"),
                "binary entries are located with offset_bits, not pointer",
            );
        }
        let element = match &array.element {
            Element::Scalar(element) => {
                let element_place = place.key("element");
                check_values(
                    element.ty,
                    element.size_bits,
                    element.conversion.as_ref(),
                    &element.invalid,
                    &element_place,
                    report,
                );
                self.element_facts(element, &element_place, report)?
            }
            Element::Struct { name } => Facts::of_struct(self.types.get(name.as_str())?),
        };
        if element.size.max == Some(0) {
            report.error(
                Code::FieldSize,
                &place.key("element"),
                "an array element takes at least one bit; decoding refuses an empty one \
                 (empty_element), so an array of them could never be read",
            );
            return None;
        }
        let count_place = place.key("count");
        let (size, max_count) = match &array.count {
            None => {
                report.error(
                    Code::WrongEncoding,
                    &count_place,
                    "binary arrays say how many elements they have: count",
                );
                return None;
            }
            Some(Count::Fixed { count }) => {
                let n = u64::from(*count);
                // No elements read no bits, however many an element can take.
                let max = match n {
                    0 => Some(0),
                    _ => element.size.max.and_then(|m| m.checked_mul(n)),
                };
                (
                    Size {
                        min: element.size.min.saturating_mul(n),
                        max,
                    },
                    Some(n),
                )
            }
            Some(Count::Field(from)) => {
                let max_count = check_from_field(from, "count", scope, &count_place, report)?;
                (
                    Size {
                        min: 0,
                        max: element.size.max.and_then(|m| m.checked_mul(max_count)),
                    },
                    Some(max_count),
                )
            }
            Some(Count::Remaining { .. }) => {
                if element.size.min == 0 {
                    report.error(
                        Code::FieldSize,
                        &count_place,
                        "an array that runs to the end of its container needs elements that are \
                         never empty, or it would never end",
                    );
                    return None;
                }
                // A packet ends on a byte, so bits after the last element,
                // short of a byte, would be read as the start of another.
                if !element.size_residues.aligned() {
                    report.error(
                        Code::Alignment,
                        &place.key("element"),
                        "an array that runs to the end of the packet has elements of whole \
                         bytes: the packet ends on a byte, so bits left after the last element \
                         would be read as another",
                    );
                    return None;
                }
                (Size { min: 0, max: None }, None)
            }
        };
        let step = element.size_residues;
        let size_residues = match (&array.count, element.size.exact()) {
            _ if step.aligned() => Residues::ALIGNED,
            (Some(Count::Fixed { count }), Some(bits)) => bits
                .checked_mul(u64::from(*count))
                .map_or(Residues::ANY, Residues::at),
            (_, Some(bits)) => Residues::multiples(bits, max_count),
            (_, None) => Residues::ANY,
        };
        Some(Facts {
            size,
            present: size,
            size_residues,
            needs_byte: element.needs_byte,
            holds_narrow: false,
            narrow_le: None,
            own_narrow: None,
            to_end: matches!(array.count, Some(Count::Remaining { .. })),
            element: Some(ElementFacts {
                size: element.size,
                step,
                needs_byte: element.needs_byte,
                holds_narrow: element.holds_narrow,
                narrow_le: element.narrow_le,
                own_narrow: element.own_narrow,
                max_count,
            }),
        })
    }

    fn element_facts(
        &self,
        element: &ScalarElement,
        place: &Place,
        report: &mut Report,
    ) -> Option<Facts> {
        match &element.length {
            Some(ByteLength::Field(_)) => {
                report.error(
                    Code::ReferenceType,
                    &place.key("length"),
                    "an array element has no fields of its own to take its length from; use a \
                     struct element",
                );
                None
            }
            Some(ByteLength::Remaining { .. }) => {
                report.error(
                    Code::ReadsToEnd,
                    &place.key("length"),
                    "an array element can't read to the end of the container",
                );
                None
            }
            _ => self.scalar_facts(ScalarShape::of_element(element), None, place, report),
        }
    }

    /// Facts of a binary scalar, checking that its size or length suits its
    /// type. `scope` resolves a length taken from an earlier field.
    fn scalar_facts(
        &self,
        shape: ScalarShape,
        scope: Option<&Scope>,
        place: &Place,
        report: &mut Report,
    ) -> Option<Facts> {
        let ScalarShape {
            ty,
            size_bits,
            byte_order,
            length,
            text,
        } = shape;
        // The value's own order, when it validly names one.
        let mut own_order = None;
        if let Some(order) = byte_order {
            let order_place = place.key("byte_order");
            if !matches!(ty, ScalarType::Uint | ScalarType::Int | ScalarType::Float) {
                report.error(
                    Code::ByteOrder,
                    &order_place,
                    format!(
                        "only uint, int and float values have a byte order; this one is {}",
                        type_name(ty)
                    ),
                );
            } else if Some(order) == self.byte_order {
                report.error(
                    Code::ExplicitDefault,
                    &order_place,
                    format!(
                        "{} is the family's byte order, the default; omit byte_order",
                        order_name(order)
                    ),
                );
            } else {
                own_order = Some(order);
            }
        }
        if text.is_some() && ty != ScalarType::String {
            report.error(
                Code::FieldSize,
                &place.key("text"),
                "only string fields have a text encoding",
            );
        }
        if let ScalarType::String | ScalarType::Bytes = ty {
            if size_bits.is_some() {
                report.error(
                    Code::FieldSize,
                    &place.key("size_bits"),
                    "strings and bytes are sized by length in bytes, not size_bits",
                );
            }
            let length_place = place.key("length");
            let size = match length {
                None => {
                    report.error(
                        Code::FieldSize,
                        &length_place,
                        "strings and bytes say their length",
                    );
                    return None;
                }
                Some(ByteLength::Fixed { bytes, terminator }) => {
                    if *bytes == 0 && terminator.is_some() {
                        report.error(
                            Code::FieldSize,
                            &length_place.key("terminator"),
                            "an empty buffer holds no value to end; leave out its terminator",
                        );
                    }
                    Size::fixed(u64::from(*bytes) * 8)
                }
                Some(ByteLength::Field(from)) => {
                    let scope = scope?;
                    let max = check_from_field(from, "length", scope, &length_place, report)?;
                    Size {
                        min: 0,
                        max: max.checked_mul(8),
                    }
                }
                Some(ByteLength::Terminator { .. }) => Size { min: 8, max: None },
                Some(ByteLength::Remaining { .. }) => Size { min: 0, max: None },
            };
            return Some(Facts {
                needs_byte: true,
                ..Facts::plain(size, Residues::ALIGNED)
            });
        }
        if length.is_some() {
            report.error(
                Code::FieldSize,
                &place.key("length"),
                "only strings and bytes have a length; numbers have size_bits",
            );
        }
        let (allowed, rule) = match ty {
            ScalarType::Float => (matches!(size_bits, Some(32 | 64)), "32 or 64"),
            ScalarType::Bool => (size_bits == Some(1), "1"),
            _ => (size_bits.is_some_and(|s| (1..=64).contains(&s)), "1 to 64"),
        };
        let bits = match size_bits {
            Some(bits) if allowed => u64::from(bits),
            _ => {
                let shown = size_bits.map_or("no size".to_string(), |s| format!("{s} bits"));
                report.error(
                    Code::FieldSize,
                    &place.key("size_bits"),
                    format!("a {} field has {rule} bits, not {shown}", type_name(ty)),
                );
                return None;
            }
        };
        // Only numbers have a byte order (decoding.md §2.1): a bool reads
        // the same wherever it sits.
        let little =
            ty != ScalarType::Bool && byte_order.or(self.byte_order) == Some(ByteOrder::Little);
        let whole_bytes = bits % 8 == 0;
        if little && bits > 8 && !whole_bytes {
            report.error(
                Code::Alignment,
                &place.key("size_bits"),
                format!(
                    "a little-endian value wider than a byte is whole bytes; {bits} bits isn't"
                ),
            );
        }
        Some(Facts {
            needs_byte: little && whole_bytes,
            narrow_le: (little && bits < 8).then_some(bits),
            own_narrow: (own_order.is_some() && bits <= 8).then_some(bits),
            ..Facts::plain(Size::fixed(bits), Residues::at(bits))
        })
    }

    /// Every possible start must suit what the entry holds.
    fn check_alignment(
        &self,
        name: &str,
        facts: &Facts,
        start: Residues,
        place: &Place,
        report: &mut Report,
    ) {
        let not_on_byte = |what: &str| {
            format!(
                "{name} holds {what}, which start on a byte, but it can start at bit {} of a byte",
                start.iter().find(|r| *r != 0).unwrap_or(0)
            )
        };
        if facts.needs_byte && !start.aligned() {
            report.error(
                Code::Alignment,
                place,
                not_on_byte("strings, bytes, whole-byte little-endian values or structs of them"),
            );
            return;
        }
        if facts.to_end && !start.aligned() {
            report.error(
                Code::Alignment,
                place,
                format!(
                    "{name} runs to the end of the packet, which ends on a byte, so it starts on \
                     one, but it can start at bit {} of a byte",
                    start.iter().find(|r| *r != 0).unwrap_or(0)
                ),
            );
            return;
        }
        if facts.holds_narrow && !start.aligned() {
            report.error(
                Code::Alignment,
                place,
                format!(
                    "{name} holds a value narrower than a byte whose place within a byte matters \
                     (a little-endian one, or one with its own byte_order), so it starts on a \
                     byte, but it can start at bit {} of a byte",
                    start.iter().find(|r| *r != 0).unwrap_or(0)
                ),
            );
            return;
        }
        if let Some(bits) = facts.narrow_le
            && let Some(r) = start.iter().find(|r| r + bits > 8)
        {
            report.error(
                Code::Alignment,
                place,
                format!(
                    "a little-endian value narrower than a byte fits inside one byte; {name} \
                     ({bits} bits) can start at bit {r} of a byte"
                ),
            );
            return;
        }
        if let Some(bits) = facts.own_narrow
            && start.iter().all(|r| r + bits <= 8)
        {
            report.error(
                Code::ByteOrder,
                &place.key("byte_order"),
                format!(
                    "{name} always fits inside one byte, where byte order doesn't apply; omit \
                     byte_order"
                ),
            );
            return;
        }
        if let Some(element) = facts.element {
            // Element k starts at the array's start plus k element sizes,
            // for k below the count, and element 0 even for a count of 0
            // (decoding.md §5.2; eight steps reach every residue).
            let steps = element.max_count.map_or(7, |n| n.saturating_sub(1).min(7));
            let mut element_starts = start;
            for _ in 0..steps {
                element_starts = element_starts.union(element_starts.plus(element.step));
            }
            let size = element.size;
            if (element.needs_byte || element.holds_narrow) && !element_starts.aligned() {
                report.error(
                    Code::Alignment,
                    place,
                    format!(
                        "{name}'s elements must each start on a byte, so the element size must \
                         be whole bytes (it is {} to {} bits)",
                        size.min,
                        size.max.map_or("unbounded".to_string(), |m| m.to_string())
                    ),
                );
            } else if let Some(bits) = element.narrow_le
                && let Some(r) = element_starts.iter().find(|r| r + bits > 8)
            {
                report.error(
                    Code::Alignment,
                    place,
                    format!(
                        "{name}'s little-endian {bits}-bit elements must each fit inside one \
                         byte, but one can start at bit {r} of a byte"
                    ),
                );
            } else if let Some(bits) = element.own_narrow
                && element_starts.iter().all(|r| r + bits <= 8)
            {
                report.error(
                    Code::ByteOrder,
                    &place.key("element").key("byte_order"),
                    format!(
                        "{name}'s elements always fit inside one byte, where byte order doesn't \
                         apply; omit byte_order"
                    ),
                );
            }
        }
    }

    fn check_json_entry(&self, entry: &Entry, scope: &Scope, place: &Place, report: &mut Report) {
        let wrong = |report: &mut Report, key: &str, message: &str| {
            report.error(Code::WrongEncoding, &place.key(key), message.to_string());
        };
        let binary_only = "self-describing entries are located with pointer; offset_bits, \
                           size_bits, byte_order, length and count only apply to binary payloads";
        let needs_pointer = "self-describing entries say where they are: pointer";
        match entry {
            Entry::Padding(_) => report.error(
                Code::WrongEncoding,
                place,
                "padding only applies to binary payloads",
            ),
            Entry::Checksum(_) => report.error(
                Code::WrongEncoding,
                place,
                "checksums only apply to binary payloads",
            ),
            Entry::Field(field) => {
                if field.pointer.is_none() {
                    wrong(report, "pointer", needs_pointer);
                }
                for (present, key) in [
                    (field.offset_bits.is_some(), "offset_bits"),
                    (field.size_bits.is_some(), "size_bits"),
                    (field.byte_order.is_some(), "byte_order"),
                    (field.length.is_some(), "length"),
                ] {
                    if present {
                        wrong(report, key, binary_only);
                    }
                }
                if field.text.is_some() {
                    wrong(
                        report,
                        "text",
                        "text encodings only apply to binary strings",
                    );
                }
                if field.ty == ScalarType::Bytes {
                    wrong(report, "type", "bytes fields only apply to binary payloads");
                }
                check_values(
                    field.ty,
                    None,
                    field.conversion.as_ref(),
                    scope.sentinels(&field.invalid),
                    place,
                    report,
                );
            }
            Entry::Array(array) => {
                if array.pointer.is_none() {
                    wrong(report, "pointer", needs_pointer);
                }
                if array.offset_bits.is_some() {
                    wrong(report, "offset_bits", binary_only);
                }
                if array.count.is_some() {
                    wrong(report, "count", binary_only);
                }
                if let Element::Scalar(element) = &array.element {
                    let element_place = place.key("element");
                    if element.size_bits.is_some()
                        || element.byte_order.is_some()
                        || element.length.is_some()
                    {
                        report.error(Code::WrongEncoding, &element_place, binary_only);
                    }
                    if element.ty == ScalarType::Bytes {
                        report.error(
                            Code::WrongEncoding,
                            &element_place.key("type"),
                            "bytes only apply to binary payloads",
                        );
                    }
                    check_values(
                        element.ty,
                        None,
                        element.conversion.as_ref(),
                        &element.invalid,
                        &element_place,
                        report,
                    );
                }
            }
            Entry::Struct(entry) => {
                if entry.pointer.is_none() {
                    wrong(report, "pointer", needs_pointer);
                }
                if entry.offset_bits.is_some() {
                    wrong(report, "offset_bits", binary_only);
                }
            }
        }
        if let Some(when) = entry_condition(entry) {
            check_condition_refs(when, scope, &place.key("when"), 1, report);
        }
    }
}

/// A scalar's type and how it's sized, from a field or an array element.
#[derive(Clone, Copy)]
struct ScalarShape<'a> {
    ty: ScalarType,
    size_bits: Option<u32>,
    byte_order: Option<ByteOrder>,
    length: Option<&'a ByteLength>,
    text: Option<TextEncoding>,
}

impl<'a> ScalarShape<'a> {
    fn of_field(field: &'a Field) -> Self {
        ScalarShape {
            ty: field.ty,
            size_bits: field.size_bits,
            byte_order: field.byte_order,
            length: field.length.as_ref(),
            text: field.text,
        }
    }

    fn of_element(element: &'a ScalarElement) -> Self {
        ScalarShape {
            ty: element.ty,
            size_bits: element.size_bits,
            byte_order: element.byte_order,
            length: element.length.as_ref(),
            text: element.text,
        }
    }
}

fn remember<'a>(entry: &'a Entry, scope: &mut Scope<'a>) {
    if let Entry::Field(field) = entry {
        scope.earlier.insert(
            &field.name,
            Earlier {
                field,
                // An optional field may be missing, so references treat it
                // like a conditional one.
                conditional: field.when.is_some() || field.optional == Some(true),
            },
        );
    }
}

/// `optional` is for self-describing payloads, whose members can be
/// missing, and is only ever spelled `true`.
fn check_optional(entry: &Entry, binary: bool, place: &Place, report: &mut Report) {
    let optional = match entry {
        Entry::Field(field) => field.optional,
        Entry::Array(array) => array.optional,
        Entry::Struct(inner) => inner.optional,
        Entry::Padding(_) | Entry::Checksum(_) => None,
    };
    match optional {
        Some(false) => report.error(
            Code::ExplicitDefault,
            &place.key("optional"),
            "optional: false is the default; omit it",
        ),
        Some(true) if binary => report.error(
            Code::WrongEncoding,
            &place.key("optional"),
            "a binary entry is always there unless a condition says otherwise; optional only \
             applies to self-describing payloads, whose members can be missing",
        ),
        _ => {}
    }
}

fn entry_place(container: &Place, index: usize, entry: &Entry) -> Place {
    let (label, name) = match entry {
        Entry::Field(field) => ("field", field.name.as_str()),
        Entry::Array(array) => ("array", array.name.as_str()),
        Entry::Struct(entry) => ("struct", entry.name.as_str()),
        Entry::Checksum(checksum) => ("checksum", checksum.name.as_str()),
        Entry::Padding(_) => ("padding", ""),
    };
    container.item("entries", index, label, name)
}

fn entry_name(entry: &Entry) -> Option<&str> {
    match entry {
        Entry::Field(field) => Some(&field.name),
        Entry::Array(array) => Some(&array.name),
        Entry::Struct(entry) => Some(&entry.name),
        Entry::Checksum(checksum) => Some(&checksum.name),
        Entry::Padding(_) => None,
    }
}

fn entry_condition(entry: &Entry) -> Option<&Condition> {
    match entry {
        Entry::Field(field) => field.when.as_ref(),
        Entry::Array(array) => array.when.as_ref(),
        Entry::Struct(entry) => entry.when.as_ref(),
        Entry::Padding(padding) => padding.when.as_ref(),
        Entry::Checksum(_) => None,
    }
}

fn explicit_offset(entry: &Entry) -> Option<u64> {
    match entry {
        Entry::Field(field) => field.offset_bits,
        Entry::Array(array) => array.offset_bits,
        Entry::Struct(entry) => entry.offset_bits,
        Entry::Padding(padding) => padding.offset_bits,
        Entry::Checksum(checksum) => checksum.offset_bits,
    }
    .map(u64::from)
}

fn reads_to_end(entry: &Entry) -> bool {
    match entry {
        Entry::Field(field) => matches!(field.length, Some(ByteLength::Remaining { .. })),
        Entry::Array(array) => matches!(array.count, Some(Count::Remaining { .. })),
        Entry::Struct(_) | Entry::Padding(_) | Entry::Checksum(_) => false,
    }
}

pub(super) fn type_name(ty: ScalarType) -> &'static str {
    match ty {
        ScalarType::Uint => "uint",
        ScalarType::Int => "int",
        ScalarType::Float => "float",
        ScalarType::Bool => "bool",
        ScalarType::String => "string",
        ScalarType::Bytes => "bytes",
    }
}

fn order_name(order: ByteOrder) -> &'static str {
    match order {
        ByteOrder::Big => "big",
        ByteOrder::Little => "little",
    }
}

/// Unconditional pairs from different runs must not overlap; a pair with a
/// conditional entry may overlap only if both are conditional or one region
/// contains the other. Each entry is reported once, at the later-listed
/// entry of its first conflict.
fn report_overlaps(entries: &[Entry], extents: &[Extent], container: &Place, report: &mut Report) {
    let mut conflicts: BTreeMap<usize, usize> = BTreeMap::new();
    let mut record = |a: &Extent, b: &Extent| {
        let (earlier, later) = if a.index < b.index { (a, b) } else { (b, a) };
        conflicts.entry(later.index).or_insert(earlier.index);
    };

    // Unconditional pairs, in one sweep by start: an extent overlaps some
    // earlier-starting extent of another run exactly when the furthest end
    // among those runs passes its start. Keep the furthest end overall and
    // the furthest from any other run.
    let mut unconditional: Vec<&Extent> = extents.iter().filter(|e| !e.conditional).collect();
    unconditional.sort_by_key(|e| (e.start, e.index));
    let mut best: Option<&Extent> = None;
    let mut second: Option<&Extent> = None;
    for &extent in &unconditional {
        let rival = match best {
            Some(b) if b.run != extent.run => Some(b),
            _ => second,
        };
        if let Some(rival) = rival
            && rival.end() > extent.start
        {
            record(rival, extent);
        }
        match best {
            Some(b) if b.end() >= extent.end() => {
                if b.run != extent.run && second.is_none_or(|s| s.end() < extent.end()) {
                    second = Some(extent);
                }
            }
            _ => {
                if let Some(b) = best
                    && b.run != extent.run
                {
                    second = Some(b);
                }
                best = Some(extent);
            }
        }
    }

    // Pairs with a conditional entry: few in practice, checked directly.
    for conditional in extents.iter().filter(|e| e.conditional) {
        for other in extents {
            if other.index == conditional.index || other.run == conditional.run {
                continue;
            }
            let allowed = other.conditional
                || conditional.surely_contains(other)
                || other.surely_contains(conditional);
            if !allowed && conditional.overlaps(other) {
                record(conditional, other);
            }
        }
    }

    let by_index: BTreeMap<usize, &Extent> = extents.iter().map(|e| (e.index, e)).collect();
    for (later, earlier) in conflicts {
        let (a, b) = (by_index[&earlier], by_index[&later]);
        let verb = if a.exact && b.exact {
            "overlaps"
        } else {
            "can overlap"
        };
        let earlier_name = entry_name(&entries[earlier]).unwrap_or("padding");
        let later_name = entry_name(&entries[later]).unwrap_or("padding");
        report.error(
            Code::Overlap,
            &entry_place(container, later, &entries[later]),
            format!("{later_name} {verb} {earlier_name}: they can read the same bits"),
        );
    }
}

/// A count or length taken from an earlier field: it must be an
/// unconditional unsigned integer decoded before this entry. Returns the
/// largest value it can produce, or `None` after reporting why not.
fn check_from_field(
    from: &FromField,
    what: &str,
    scope: &Scope,
    place: &Place,
    report: &mut Report,
) -> Option<u64> {
    match from.multiply {
        Some(0) => {
            report.error(
                Code::FieldSize,
                &place.key("multiply"),
                "multiply is at least 1",
            );
            return None;
        }
        Some(1) => report.error(
            Code::ExplicitDefault,
            &place.key("multiply"),
            "1 is the default multiply; omit it",
        ),
        _ => {}
    }
    match from.add {
        Some(0) => report.error(
            Code::ExplicitDefault,
            &place.key("add"),
            "0 is the default add; omit it",
        ),
        Some(add) => check_int(add, "add", &place.key("add"), report),
        None => {}
    }
    let field_place = place.key("field");
    let earlier = lookup(&from.field, scope, &field_place, report)?;
    let field = earlier.field;
    if field.ty != ScalarType::Uint || earlier.conditional {
        report.error(
            Code::ReferenceType,
            &field_place,
            format!(
                "a {what} comes from an unconditional unsigned integer field; {} is {}",
                field.name,
                if earlier.conditional {
                    "conditional, so it may be absent".to_string()
                } else {
                    format!("a {} field", type_name(field.ty))
                }
            ),
        );
        return None;
    }
    let bits = field.size_bits.filter(|s| (1..=64).contains(s))?;
    let max_raw = if bits == 64 {
        u64::MAX
    } else {
        (1_u64 << bits) - 1
    };
    from.max_value(max_raw)
}

fn lookup<'s>(
    name: &str,
    scope: &'s Scope,
    place: &Place,
    report: &mut Report,
) -> Option<&'s Earlier<'s>> {
    match scope.earlier.get(name) {
        Some(earlier) => Some(earlier),
        None if scope.all.contains(name) => {
            report.error(
                Code::ForwardReference,
                place,
                format!(
                    "{name} is decoded after this entry; a reference can only use entries \
                     decoded before it"
                ),
            );
            None
        }
        None => {
            report.error(
                Code::UnknownEntry,
                place,
                format!("there is no field {name} in this container"),
            );
            None
        }
    }
}

/// References in a condition: earlier, unconditional fields of a type the
/// comparison suits, with values the field can hold.
fn check_condition_refs(
    condition: &Condition,
    scope: &Scope,
    place: &Place,
    depth: usize,
    report: &mut Report,
) {
    if depth > MAX_NESTING {
        return;
    }
    let field_ref = |name: &str, report: &mut Report| -> Option<&Field> {
        let earlier = lookup(name, scope, &place.key("field"), report)?;
        if earlier.conditional {
            report.error(
                Code::ReferenceType,
                &place.key("field"),
                format!(
                    "{name} is conditional, so it may be absent; conditions use fields that are \
                     always there (a conditional struct with unconditional fields inside works)"
                ),
            );
            return None;
        }
        Some(earlier.field)
    };
    match condition {
        Condition::Compare { field, op, value } => {
            let Some(field) = field_ref(field, report) else {
                return;
            };
            let ordering = !matches!(op, CompareOp::Eq | CompareOp::Ne);
            let mask = matches!(op, CompareOp::MaskAny | CompareOp::MaskAll);
            let suits = match field.ty {
                ScalarType::Uint => true,
                ScalarType::Int => !mask,
                ScalarType::Bool | ScalarType::String => !ordering,
                ScalarType::Float | ScalarType::Bytes => false,
            };
            if !suits {
                report.error(
                    Code::ReferenceType,
                    &place.key("op"),
                    format!(
                        "this comparison doesn't apply to {} field {}: conditions compare integer, \
                         bool or string fields, masks need unsigned integers, and ordering needs \
                         numbers",
                        type_name(field.ty),
                        field.name
                    ),
                );
                return;
            }
            check_condition_value(field, value, &place.key("value"), report);
        }
        Condition::In { field, values } => {
            let Some(field) = field_ref(field, report) else {
                return;
            };
            if matches!(field.ty, ScalarType::Float | ScalarType::Bytes) {
                report.error(
                    Code::ReferenceType,
                    &place.key("field"),
                    format!(
                        "conditions compare integer, bool or string fields; {} is a {} field",
                        field.name,
                        type_name(field.ty)
                    ),
                );
                return;
            }
            for (index, value) in values.iter().enumerate() {
                check_condition_value(field, value, &place.at("values", index), report);
            }
        }
        Condition::All { conditions } | Condition::Any { conditions } => {
            for (index, inner) in conditions.iter().enumerate() {
                check_condition_refs(
                    inner,
                    scope,
                    &place.at("conditions", index),
                    depth + 1,
                    report,
                );
            }
        }
        Condition::Not { condition } => {
            check_condition_refs(condition, scope, &place.key("condition"), depth + 1, report)
        }
    }
}

fn check_condition_value(field: &Field, value: &Scalar, place: &Place, report: &mut Report) {
    match (field.ty, value) {
        (ScalarType::String, Scalar::Str(_)) => {}
        (ScalarType::String, Scalar::Int(_)) => report.error(
            Code::ReferenceType,
            place,
            format!(
                "{} is a string field, so it compares with strings",
                field.name
            ),
        ),
        _ => {
            check_integer_spelling(value, "condition value", place, report);
            match (
                integer_value(value),
                integer_range(field.ty, field.size_bits),
            ) {
                (None, _) => report.error(
                    Code::ReferenceType,
                    place,
                    format!(
                        "{} holds integers, so it compares with integers",
                        field.name
                    ),
                ),
                (Some(n), Some((low, high))) if !(low..=high).contains(&n) => report.error(
                    Code::ValueRange,
                    place,
                    format!(
                        "{} holds {low} to {high}, so it can never be {n}",
                        field.name
                    ),
                ),
                _ => {}
            }
        }
    }
}

/// Conversions and sentinel values that suit a field's type and range.
fn check_values(
    ty: ScalarType,
    size_bits: Option<u32>,
    conversion: Option<&Conversion>,
    invalid: &[Scalar],
    place: &Place,
    report: &mut Report,
) {
    let numeric = matches!(ty, ScalarType::Uint | ScalarType::Int | ScalarType::Float);
    let integer = matches!(ty, ScalarType::Uint | ScalarType::Int);
    let range = integer_range(ty, size_bits);
    if let Some(conversion) = conversion {
        let conversion_place = place.key("conversion");
        match conversion {
            Conversion::Linear { .. }
            | Conversion::Poly { .. }
            | Conversion::Table { .. }
            | Conversion::Spline { .. }
                if !numeric =>
            {
                report.error(
                    Code::ConversionType,
                    &conversion_place,
                    format!(
                        "linear, poly, table and spline conversions need a number, not a {} field",
                        type_name(ty)
                    ),
                )
            }
            Conversion::States { .. } if !integer => report.error(
                Code::ConversionType,
                &conversion_place,
                format!(
                    "a states conversion needs an integer field, not a {} field",
                    type_name(ty)
                ),
            ),
            Conversion::States { states } => {
                if let Some((low, high)) = range {
                    for (index, state) in states.iter().enumerate() {
                        // A raw value that isn't an integer is reported
                        // with the set's conversions.
                        let Some(raw) = integer_value(&state.raw) else {
                            continue;
                        };
                        if !(low..=high).contains(&raw) {
                            report.error(
                                Code::ValueRange,
                                &conversion_place.at("states", index),
                                format!(
                                    "raw value {raw} can never come from this field ({low} to \
                                     {high})"
                                ),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (index, value) in invalid.iter().enumerate() {
        let value_place = place.at("invalid", index);
        // A number beyond ±(2^53 - 1) is already reported as unsafe.
        if let Scalar::Int(n) = value
            && n.unsigned_abs() > MAX_SAFE_INTEGER as u64
        {
            continue;
        }
        match ty {
            ScalarType::String => {
                if matches!(value, Scalar::Int(_)) {
                    report.error(
                        Code::ValueRange,
                        &value_place,
                        "a string field's invalid values are strings",
                    );
                }
            }
            ScalarType::Uint | ScalarType::Int | ScalarType::Float => {
                check_integer_spelling(value, "invalid value", &value_place, report);
                match (integer_value(value), range) {
                    (None, _) => report.error(
                        Code::ValueRange,
                        &value_place,
                        "a numeric field's invalid values are integers",
                    ),
                    (Some(n), Some((low, high))) if !(low..=high).contains(&n) => report.error(
                        Code::ValueRange,
                        &value_place,
                        format!(
                            "invalid value {n} can never come from this field ({low} to {high})"
                        ),
                    ),
                    _ => {}
                }
            }
            ScalarType::Bool | ScalarType::Bytes => {
                report.error(
                    Code::ValueRange,
                    &value_place,
                    format!(
                        "invalid values apply to numeric and string fields, not {}",
                        type_name(ty)
                    ),
                );
                break;
            }
        }
    }
}

/// The raw values an integer or bool field can hold, when its size is known.
fn integer_range(ty: ScalarType, size_bits: Option<u32>) -> Option<(i128, i128)> {
    if ty == ScalarType::Bool {
        return Some((0, 1));
    }
    let bits = size_bits.filter(|s| (1..=64).contains(s))?;
    match ty {
        ScalarType::Uint => Some((0, (1_i128 << bits) - 1)),
        ScalarType::Int => Some((-(1_i128 << (bits - 1)), (1_i128 << (bits - 1)) - 1)),
        _ => None,
    }
}

/// The bit offset of each entry where it is *static*, as
/// `spec/decoding.md` §5.2 defines it: an explicit `offset_bits`, the first
/// entry, or an entry after one with a static offset and an exact size
/// ([`exact_size`], with struct types' sizes in `type_sizes`).
/// Identification compares field keys by this position, and decoding
/// verifies checksums at one first, so every implementation must compute
/// it the same way.
pub(crate) fn static_offsets(
    entries: &[Entry],
    type_sizes: &BTreeMap<&str, u64>,
) -> Vec<Option<u64>> {
    let mut cursor = Some(0_u64);
    entries
        .iter()
        .map(|entry| {
            let offset = explicit_offset(entry).or(cursor);
            // An offset that doesn't fit in 64 bits is unknown, not wrapped.
            cursor = offset
                .zip(exact_size(entry, type_sizes))
                .and_then(|(o, size)| o.checked_add(size));
            offset
        })
        .collect()
}

/// Exact sizes of struct types, smallest nesting first, for
/// [`static_offsets`]. A type has an exact size when every entry has a
/// static offset and an exact size; its size is then its furthest end.
pub(super) fn exact_type_sizes<'a>(
    types: &'a BTreeMap<String, StructType>,
    heights: &BTreeMap<&str, usize>,
) -> BTreeMap<&'a str, u64> {
    let mut order: Vec<(&String, &StructType)> = types.iter().collect();
    order.sort_by_key(|(name, _)| heights.get(name.as_str()).copied().unwrap_or(0));
    let mut sizes: BTreeMap<&str, u64> = BTreeMap::new();
    for (name, struct_type) in order {
        let offsets = static_offsets(&struct_type.entries, &sizes);
        let ends: Option<Vec<u64>> = struct_type
            .entries
            .iter()
            .zip(offsets)
            .map(|(entry, offset)| {
                offset
                    .zip(exact_size(entry, &sizes))
                    .and_then(|(o, s)| o.checked_add(s))
            })
            .collect();
        if let Some(end) = ends.and_then(|ends| ends.into_iter().max()) {
            sizes.insert(name, end);
        }
    }
    sizes
}

/// Size in bits when it can't vary between messages. A conditional entry
/// may be absent, so its size varies.
fn exact_size(entry: &Entry, type_sizes: &BTreeMap<&str, u64>) -> Option<u64> {
    match entry {
        Entry::Field(field) if field.when.is_none() => {
            simple_size(field.ty, field.size_bits, field.length.as_ref())
        }
        Entry::Padding(padding) if padding.when.is_none() => Some(u64::from(padding.size_bits)),
        Entry::Array(array) if array.when.is_none() => match (&array.count, &array.element) {
            (Some(Count::Fixed { count }), Element::Scalar(element)) => {
                simple_size(element.ty, element.size_bits, element.length.as_ref())
                    .and_then(|size| size.checked_mul(u64::from(*count)))
            }
            (Some(Count::Fixed { count }), Element::Struct { name }) => type_sizes
                .get(name.as_str())
                .and_then(|size| size.checked_mul(u64::from(*count))),
            _ => None,
        },
        Entry::Struct(entry) if entry.when.is_none() => {
            type_sizes.get(entry.struct_type.as_str()).copied()
        }
        Entry::Checksum(checksum) => Some(u64::from(checksum.algorithm.width_bits())),
        _ => None,
    }
}

fn simple_size(ty: ScalarType, size_bits: Option<u32>, length: Option<&ByteLength>) -> Option<u64> {
    match ty {
        ScalarType::String | ScalarType::Bytes => match length? {
            ByteLength::Fixed { bytes, .. } => Some(u64::from(*bytes) * 8),
            _ => None,
        },
        _ => size_bits.map(u64::from),
    }
}
