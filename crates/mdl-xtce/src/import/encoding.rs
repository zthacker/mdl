//! A container's own BinaryEncoding: the size it gives the container, as
//! padding after its entries, and the checks over the packet's bytes
//! (XTCE 1.3's ErrorDetectCorrect), as checksum entries.
//!
//! The importer places an inherited container's entries from the start of
//! the packet, as it reads `containerStart`, so a container's size and a
//! check's `bitsFromReference` count from there too; an embedded
//! container's size counts from its own start, as Yamcs reads it. Yamcs
//! reads no checks, so what XTCE leaves open about one (where its bytes
//! end, what `direction` does beside the reflections, what `sum16` adds)
//! is refused rather than guessed.

use std::collections::BTreeSet;

use mdl::source::Span;
use serde_json::{Value, json};

use super::commands::Command;
use super::{ChecksumField, Element, Entry, Import, Packet, ParamKey, TypeId};
use crate::Code;
use crate::out::integer_json;
use crate::space::Kind;
use crate::types::{IntegerValue, Length, Order, Scalar, integer_value};
use crate::xml::{self, El, Expect};

/// What a container's BinaryEncoding says.
#[derive(Clone, Default)]
pub(super) struct ContainerEncoding<'d> {
    /// The bits from where the container's entries are placed from (the
    /// packet's start, or an embedded container's) to its end.
    pub(super) size: Option<(u32, Span)>,
    pub(super) checks: Vec<Check<'d>>,
}

/// A check over a packet's bytes, and the parameter holding its value.
#[derive(Clone)]
pub(super) struct Check<'d> {
    /// The CRC, Checksum or XOR element.
    pub(super) span: Span,
    /// The parameter, by SpaceSystem and name.
    pub(super) parameter: (usize, &'d str),
    /// Its parameterRef attribute.
    pub(super) reference: Span,
    /// MDL's algorithm, as the IR writes it.
    algorithm: Value,
    /// The bits its value takes.
    width: u32,
    /// Where its bytes start, from the start of the packet, if not there.
    from_bits: Option<(u32, Span)>,
}

impl<'d> Import<'d, '_> {
    /// A container's BinaryEncoding, or none. An embedded container holds
    /// no checks: an MDL checksum covers a packet's bytes.
    pub(super) fn container_encoding(
        &mut self,
        space: usize,
        container: El<'d>,
        embedded: bool,
    ) -> Option<ContainerEncoding<'d>> {
        let Some(encoding) = container.child("BinaryEncoding") else {
            return Some(ContainerEncoding::default());
        };
        // XTCE 1.0 to 1.2 give a container a value's BinaryDataEncoding:
        // its size is required, and its bit and byte orders mean nothing
        // for a container but their defaults. 1.3 gives it a type of its
        // own, with neither order.
        let in_1_3 = encoding.in_1_3();
        let attrs: &[&str] = if in_1_3 {
            &[]
        } else {
            &["bitOrder", "byteOrder"]
        };
        encoding.check(
            &Expect {
                attrs,
                children: &[
                    "ErrorDetectCorrect",
                    "SizeInBits",
                    "FromBinaryTransformAlgorithm",
                    "ToBinaryTransformAlgorithm",
                ],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let mut complete = true;
        for name in ["FromBinaryTransformAlgorithm", "ToBinaryTransformAlgorithm"] {
            if let Some(algorithm) = encoding.child(name) {
                self.report.error(
                    Code::Unsupported,
                    algorithm.span(),
                    format!("a container's {name} isn't supported: MDL runs no algorithms"),
                );
                complete = false;
            }
        }
        if !in_1_3 {
            complete &= self.container_orders(encoding).is_some();
        }
        let size = match encoding.child("SizeInBits") {
            Some(size) => self.container_size(space, size),
            None if in_1_3 => Some(None),
            None => {
                self.report.error(
                    Code::Invalid,
                    encoding.span(),
                    "before XTCE 1.3, a container's BinaryEncoding has a SizeInBits",
                );
                None
            }
        };
        let checks = match encoding.child("ErrorDetectCorrect") {
            Some(list) => self.checks(space, list, embedded),
            None => Some(Vec::new()),
        };
        match (size, checks) {
            (Some(size), Some(checks)) if complete => Some(ContainerEncoding { size, checks }),
            _ => None,
        }
    }

    /// An XTCE 1.0 to 1.2 container's bit and byte order, which only their
    /// defaults leave as they are: each value's encoding gives its own.
    fn container_orders(&mut self, encoding: El<'d>) -> Option<()> {
        let mut complete = true;
        for (name, default, other) in [
            (
                "bitOrder",
                "mostSignificantBitFirst",
                "leastSignificantBitFirst",
            ),
            (
                "byteOrder",
                "mostSignificantByteFirst",
                "leastSignificantByteFirst",
            ),
        ] {
            let Some(attr) = encoding.attr(name) else {
                continue;
            };
            let value = attr.value.trim();
            if value == default {
                continue;
            }
            complete = false;
            if value == other {
                self.report.error(
                    Code::Unsupported,
                    attr.span,
                    format!(
                        "a container's {name} of {other} isn't supported: each value's encoding \
                         gives its own"
                    ),
                );
            } else {
                self.report.error(
                    Code::Invalid,
                    attr.span,
                    format!("{name} is {value:?}, not {default} or {other}"),
                );
            }
        }
        complete.then_some(())
    }

    /// A container's SizeInBits: a fixed number of bits.
    fn container_size(&mut self, space: usize, size: El<'d>) -> Option<Option<(u32, Span)>> {
        match integer_value(size, space, self.report)? {
            IntegerValue::Fixed(bits, span) => {
                if bits <= 0 {
                    self.report.error(
                        Code::Invalid,
                        span,
                        format!("a container's SizeInBits is {bits}, not a number of bits"),
                    );
                    return None;
                }
                match u32::try_from(bits) {
                    Ok(bits) => Some(Some((bits, span))),
                    Err(_) => {
                        self.report.error(
                            Code::Unsupported,
                            span,
                            format!(
                                "a container of {bits} bits isn't supported: MDL places entries \
                                 within 2^32 bits"
                            ),
                        );
                        None
                    }
                }
            }
            IntegerValue::Dynamic(dynamic) => {
                self.report.error(
                    Code::Unsupported,
                    dynamic.span,
                    "a container's size another value gives isn't supported: MDL pads a \
                     container to a fixed size",
                );
                None
            }
        }
    }

    /// An ErrorDetectCorrect's checks, each that names the parameter
    /// holding its value as MDL's; one that names none is left out.
    fn checks(&mut self, space: usize, list: El<'d>, embedded: bool) -> Option<Vec<Check<'d>>> {
        list.check(
            &Expect {
                attrs: &[],
                children: &["Checksum", "CRC", "XOR", "Parity"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let mut checks = Vec::new();
        let mut complete = true;
        for el in list.children() {
            if !matches!(el.name(), "Checksum" | "CRC" | "XOR" | "Parity") {
                continue;
            }
            match self.check(space, el, embedded) {
                Some(Some(check)) => checks.push(check),
                Some(None) => {}
                None => complete = false,
            }
        }
        complete.then_some(checks)
    }

    /// One check; `Some(None)` for one left out, which names no parameter
    /// holding its value, so there's nothing to check it against.
    fn check(&mut self, space: usize, el: El<'d>, embedded: bool) -> Option<Option<Check<'d>>> {
        let in_1_3 = el.in_1_3();
        let (own, children): (&[&str], &[&str]) = match el.name() {
            "CRC" if in_1_3 => (
                &["width", "reflectData", "reflectRemainder", "direction"],
                &["Polynomial", "InitRemainder", "FinalXOR"],
            ),
            "CRC" => (
                &["width", "reflectData", "reflectRemainder"],
                &["Polynomial", "InitRemainder", "FinalXOR"],
            ),
            "Checksum" => (&["name", "hashSizeInBits"], &["InputAlgorithm"]),
            "Parity" => (&["type"], &[]),
            _ => (&[], &[]),
        };
        let mut attrs = own.to_vec();
        attrs.extend(["bitsFromReference", "reference"]);
        // XTCE 1.3 names the parameter holding a check's value.
        if in_1_3 {
            attrs.push("parameterRef");
        }
        el.check(
            &Expect {
                attrs: &attrs,
                children,
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let what = match el.name() {
            "CRC" => "a CRC",
            "Checksum" => "a checksum",
            "XOR" => "an XOR",
            _ => "a parity check",
        };
        let Some(reference) = el.attr("parameterRef").filter(|_| in_1_3) else {
            self.report.warning(
                Code::Ignored,
                el.span(),
                format!(
                    "{what} that names no parameter holding its value (parameterRef, which XTCE \
                     1.3 adds) isn't imported: there's nothing to check it against"
                ),
            );
            return Some(None);
        };
        if embedded {
            self.report.error(
                Code::Unsupported,
                el.span(),
                format!(
                    "{what} over an embedded container isn't supported: an MDL checksum covers a \
                     packet's bytes"
                ),
            );
            return None;
        }
        let (algorithm, width) = match el.name() {
            "CRC" => self.crc(el)?,
            "Checksum" => self.checksum(el)?,
            "XOR" => (json!({ "kind": "xor" }), 8),
            _ => {
                self.report.error(
                    Code::Unsupported,
                    el.span(),
                    "a parity check isn't supported: it's one bit, and MDL checksums are whole \
                     bytes",
                );
                return None;
            }
        };
        let from_bits = self.check_start(el)?;
        let found = match self.tree.resolve(space, reference.value, Kind::Parameter) {
            Ok(found) => found,
            Err(unresolved) => {
                self.report
                    .error(unresolved.code, reference.span, unresolved.message);
                return None;
            }
        };
        let name = found.el.item_name()?;
        if !found.members.is_empty() {
            self.report.error(
                Code::Unsupported,
                reference.span,
                format!(
                    "{what} held by a member of an aggregate isn't supported: an MDL checksum is \
                     a field of its own"
                ),
            );
            return None;
        }
        Some(Some(Check {
            span: el.span(),
            parameter: (found.space, name),
            reference: reference.span,
            algorithm,
            width,
            from_bits,
        }))
    }

    /// Where a check's bytes start: `bitsFromReference` bits from the
    /// start of the packet, whole bytes.
    fn check_start(&mut self, el: El<'d>) -> Option<Option<(u32, Span)>> {
        if let Some(reference) = el.attr("reference") {
            match reference.value.trim() {
                "start" => {}
                "end" => {
                    self.report.error(
                        Code::Unsupported,
                        reference.span,
                        "a check over bits counted from the container's end isn't supported: an \
                         MDL checksum covers the bytes from a start up to its own",
                    );
                    return None;
                }
                other => {
                    self.report.error(
                        Code::Invalid,
                        reference.span,
                        format!("reference is {other:?}, not start or end"),
                    );
                    return None;
                }
            }
        }
        let Some(attr) = el.attr("bitsFromReference") else {
            return Some(None);
        };
        let bits = xml::long(attr, self.report)?;
        if bits < 0 {
            self.report.error(
                Code::Invalid,
                attr.span,
                format!("bitsFromReference is {bits}, not a number of bits"),
            );
            return None;
        }
        match u32::try_from(bits) {
            Ok(0) => Some(None),
            Ok(bits) if bits.is_multiple_of(8) => Some(Some((bits, attr.span))),
            _ => {
                self.report.error(
                    Code::Unsupported,
                    attr.span,
                    format!(
                        "a check from bit {bits} isn't supported: MDL checksums cover whole bytes \
                         of a packet"
                    ),
                );
                None
            }
        }
    }

    /// A CRC as MDL's. XTCE's names are those of Michael Barr's CRC code,
    /// whose model is the CRC catalogue's that MDL uses: the register
    /// starts at InitRemainder as written, reflectData reflects each byte
    /// in, and reflectRemainder the result, before FinalXOR.
    fn crc(&mut self, el: El<'d>) -> Option<(Value, u32)> {
        let attr = el.required("width", self.report)?;
        let bits = xml::long(attr, self.report)?;
        let width = match u8::try_from(bits) {
            Ok(width) if (8..=64).contains(&width) && width.is_multiple_of(8) => width,
            _ => {
                self.report.error(
                    Code::Unsupported,
                    attr.span,
                    format!(
                        "a CRC of {bits} bits isn't supported: MDL's are whole bytes from 8 to 64 \
                         bits"
                    ),
                );
                return None;
            }
        };
        if let Some(direction) = el.attr("direction") {
            match direction.value.trim() {
                "mostSignificantBitFirst" => {}
                "leastSignificantBitFirst" => {
                    self.report.error(
                        Code::Unsupported,
                        direction.span,
                        "a CRC worked least significant bit first isn't supported: XTCE doesn't \
                         say how that differs from reflectData and reflectRemainder",
                    );
                    return None;
                }
                other => {
                    self.report.error(
                        Code::Invalid,
                        direction.span,
                        format!("direction is {other:?}, not a bit order"),
                    );
                    return None;
                }
            }
        }
        let reflect_in = match el.attr("reflectData") {
            Some(attr) => xml::boolean(attr, self.report)?,
            None => false,
        };
        let reflect_out = match el.attr("reflectRemainder") {
            Some(attr) => xml::boolean(attr, self.report)?,
            None => false,
        };
        let Some(polynomial) = el.child("Polynomial") else {
            self.report
                .error(Code::Invalid, el.span(), "a CRC has a Polynomial");
            return None;
        };
        let poly = self.crc_value(polynomial, width)?;
        let init = match el.child("InitRemainder") {
            Some(init) => self.crc_value(init, width)?,
            None => 0,
        };
        let xor_out = match el.child("FinalXOR") {
            Some(xor) => self.crc_value(xor, width)?,
            None => 0,
        };
        let algorithm = json!({
            "kind": "crc",
            "width": width,
            "poly": integer_json(i128::from(poly)),
            "init": integer_json(i128::from(init)),
            "reflect_in": reflect_in,
            "reflect_out": reflect_out,
            "xor_out": integer_json(i128::from(xor_out)),
        });
        Some((algorithm, u32::from(width)))
    }

    /// A CRC's Polynomial, InitRemainder or FinalXOR: hexadecimal that
    /// fits the CRC's width.
    fn crc_value(&mut self, el: El<'d>, width: u8) -> Option<u64> {
        el.check(
            &Expect {
                attrs: &[],
                children: &[],
                ignored: &[],
                text: true,
            },
            self.report,
        );
        let name = el.name();
        let (text, span) = el.text();
        let digits = text.trim();
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            // XTCE's own example writes a polynomial 0x04C11DB7, but the
            // schema's hexBinary is the digits alone.
            let prefixed = digits.starts_with("0x") || digits.starts_with("0X");
            let why = if prefixed {
                "hexBinary is hexadecimal digits without 0x"
            } else {
                "it isn't hexadecimal digits"
            };
            self.report
                .error(Code::Invalid, span, format!("{name} is {digits:?}: {why}"));
            return None;
        }
        let significant = digits.trim_start_matches('0');
        let value = match significant {
            "" => Some(0),
            _ if significant.len() > 16 => None,
            _ => u64::from_str_radix(significant, 16).ok(),
        };
        let mask = u64::MAX >> (64 - u32::from(width));
        match value {
            Some(value) if value <= mask => Some(value),
            _ => {
                let top = if name == "Polynomial" {
                    ": XTCE leaves out a polynomial's top bit"
                } else {
                    ""
                };
                self.report.error(
                    Code::Invalid,
                    span,
                    format!("{name} {digits} is wider than the CRC's {width} bits{top}"),
                );
                None
            }
        }
    }

    /// A Checksum as MDL's: `sum8` is MDL's 8-bit sum. XTCE doesn't say
    /// whether `sum16` to `sum32` add bytes or words, and MDL computes none
    /// of the others.
    fn checksum(&mut self, el: El<'d>) -> Option<(Value, u32)> {
        if let Some(input) = el.child("InputAlgorithm") {
            self.report.error(
                Code::Unsupported,
                input.span(),
                "a checksum's InputAlgorithm isn't supported: MDL runs no algorithms",
            );
            return None;
        }
        let attr = el.required("name", self.report)?;
        match attr.value.trim() {
            "sum8" => {}
            name @ ("sum16" | "sum24" | "sum32") => {
                self.report.error(
                    Code::Unsupported,
                    attr.span,
                    format!(
                        "checksum {name} isn't supported: XTCE doesn't say whether it adds bytes \
                         or words; write the checksum in MDL's own definitions instead"
                    ),
                );
                return None;
            }
            name @ ("unix_sum" | "fletcher4" | "fletcher8" | "fletcher16" | "fletcher32"
            | "adler32" | "luhn" | "verhoeff" | "damm" | "custom") => {
                self.report.error(
                    Code::Unsupported,
                    attr.span,
                    format!(
                        "checksum {name} isn't supported: MDL computes CRCs, sums of bytes, XOR \
                         and the checksum of ISO 8473-1"
                    ),
                );
                return None;
            }
            other => {
                self.report.error(
                    Code::Invalid,
                    attr.span,
                    format!("checksum name {other:?} isn't one XTCE names"),
                );
                return None;
            }
        }
        if let Some(hash) = el.attr("hashSizeInBits") {
            let bits = xml::long(hash, self.report)?;
            if bits != 8 {
                self.report.error(
                    Code::Unsupported,
                    hash.span,
                    format!(
                        "a sum8 worked out in {bits} bits isn't supported: XTCE doesn't say which \
                         of them its parameter holds"
                    ),
                );
                return None;
            }
        }
        Some((json!({ "kind": "sum", "width": 8 }), 8))
    }

    /// Padding after a container's entries, up to its size from where they
    /// are placed from. The padding follows the last entry listed, as MDL
    /// places an entry, so that one reaches furthest. `embedded` says
    /// whether Yamcs reads the size too: it reads one only where another
    /// container embeds the container.
    pub(super) fn pad_to(
        &mut self,
        entries: &mut Vec<Entry<'d>>,
        size: Option<(u32, Span)>,
        embedded: bool,
    ) -> Option<()> {
        let Some((size, span)) = size else {
            return Some(());
        };
        let Some(places) = self.static_places(entries) else {
            self.report.error(
                Code::Unsupported,
                span,
                "a container's size after entries whose end varies (one included on a \
                 condition, or sized by another value) isn't supported: MDL pads to a fixed \
                 place",
            );
            return None;
        };
        let end = places.iter().map(|(_, end)| *end).max().unwrap_or(0);
        let last = places.last().map_or(0, |(_, end)| *end);
        if last != end {
            self.report.error(
                Code::Unsupported,
                span,
                format!(
                    "a container's size after entries placed out of order isn't supported: the \
                     last one listed ends at bit {last} and another at {end}, and MDL places what \
                     follows after the last one listed"
                ),
            );
            return None;
        }
        match u32::try_from(end).ok().filter(|end| *end <= size) {
            Some(end) if end == size => {}
            Some(end) => {
                let bits = size - end;
                entries.push(Entry::Padding {
                    bits,
                    span,
                    when: None,
                });
                if !embedded {
                    self.report.warning(
                        Code::Differs,
                        span,
                        format!(
                            "this SizeInBits adds {bits} bits of padding after the container's \
                             entries, which Yamcs leaves out: it reads a container's size only \
                             where another container embeds it"
                        ),
                    );
                }
            }
            None => {
                self.report.error(
                    Code::Invalid,
                    span,
                    format!(
                        "the entries up to this container's end take {end} bits, more than its \
                         SizeInBits of {size}"
                    ),
                );
                return None;
            }
        }
        Some(())
    }

    /// Where each entry starts and ends, from where entries are placed
    /// from, if no message can change it: one included on a condition, or
    /// whose size another value gives, can.
    fn static_places(&self, entries: &[Entry<'d>]) -> Option<Vec<(u64, u64)>> {
        let mut at = 0_u64;
        let mut places = Vec::with_capacity(entries.len());
        for entry in entries {
            if entry.conditional() {
                return None;
            }
            let start = entry.offset().map_or(at, |(bits, _)| u64::from(bits));
            at = start.checked_add(self.static_size(entry)?)?;
            places.push((start, at));
        }
        Some(places)
    }

    /// Where entries end, as far as any reaches, if no message can change
    /// it.
    fn static_end(&self, entries: &[Entry<'d>]) -> Option<u64> {
        let places = self.static_places(entries)?;
        Some(places.iter().map(|(_, end)| *end).max().unwrap_or(0))
    }

    /// An entry's size, if it never varies.
    fn static_size(&self, entry: &Entry<'d>) -> Option<u64> {
        match entry {
            Entry::Padding { bits, .. } => Some(u64::from(*bits)),
            Entry::Field(field) => scalar_bits(
                &field.scalar,
                field.length.as_ref().map(|(length, _)| length),
            ),
            Entry::Checksum(checksum) => scalar_bits(&checksum.scalar, None),
            Entry::Struct(inner) => self.struct_bits(&inner.ty),
            Entry::Array(array) => {
                let count = match array.count.0.get("kind").and_then(Value::as_str) {
                    Some("fixed") => array.count.0.get("count").and_then(Value::as_u64)?,
                    _ => return None,
                };
                let element = match &array.element {
                    Element::Scalar(scalar) => {
                        let length = match &scalar.length {
                            Some(Length::Written(length, _)) => Some(length),
                            _ => None,
                        };
                        scalar_bits(scalar, length)?
                    }
                    Element::Struct(ty) => self.struct_bits(ty)?,
                };
                count.checked_mul(element)
            }
        }
    }

    /// A struct type's size, if it never varies: as far as its entries
    /// reach.
    fn struct_bits(&self, ty: &TypeId<'d>) -> Option<u64> {
        self.structs
            .get(ty)
            .and_then(|ty| self.static_end(&ty.entries))
    }

    /// The fields of a packet (or command packet) holding its checks, made
    /// checksum entries. Each check's parameter is a field of the packet
    /// that only other checks follow, in bits: XTCE doesn't say where a
    /// check's bytes end, and an MDL checksum covers those up to its own
    /// start, so a check followed by anything else could mean either. A
    /// packet that doesn't hold a check's parameter has nothing to check it
    /// against, so it's left out of that packet.
    pub(super) fn apply_checks(
        &mut self,
        entries: &mut [Entry<'d>],
        checks: &[Check<'d>],
        packet: &str,
    ) -> Option<()> {
        let mut complete = true;
        for check in checks {
            let key: ParamKey<'d> = (check.parameter.0, check.parameter.1, Vec::new());
            let shown = self.describe(&key);
            let at = entries.iter().position(|entry| match entry {
                Entry::Field(field) => field.key.as_ref() == Some(&key),
                Entry::Checksum(checksum) => checksum.key == key,
                _ => false,
            });
            let Some(at) = at else {
                if entries.iter().any(|entry| self.holds(entry, &key)) {
                    self.report.error(
                        Code::Unsupported,
                        check.reference,
                        format!(
                            "{shown} holds this check's value in {packet}, as an aggregate's \
                             members, an array or in an embedded container: an MDL checksum is a \
                             field at a packet's top level"
                        ),
                    );
                    complete = false;
                } else {
                    self.report.warning(
                        Code::Ignored,
                        check.reference,
                        format!(
                            "{packet} doesn't hold {shown}, which holds this check's value, so \
                             it's imported without the check: there's nothing to check it against"
                        ),
                    );
                }
                continue;
            };
            let field = match &entries[at] {
                Entry::Field(field) => field,
                Entry::Checksum(first) => {
                    self.report.duplicate(
                        Code::Invalid,
                        check.span,
                        first.check,
                        format!("two checks are held by {shown}"),
                    );
                    complete = false;
                    continue;
                }
                _ => continue,
            };
            if let Some(why) = unfit(field, check.width) {
                self.report.error(
                    Code::Unsupported,
                    check.reference,
                    format!("{shown} can't hold this check's value as an MDL checksum: {why}"),
                );
                complete = false;
                continue;
            }
            let checksum = ChecksumField {
                name: field.name.clone(),
                key: key.clone(),
                span: field.span,
                scalar: field.scalar.clone(),
                offset: field.offset,
                check: check.span,
                algorithm: check.algorithm.clone(),
                from_bits: check.from_bits,
            };
            entries[at] = Entry::Checksum(Box::new(checksum));
            self.checked.insert(key);
        }
        if !complete {
            return None;
        }
        self.checks_last(entries)
    }

    /// Only checks follow a packet's first check, in bits. Without
    /// locations from the start of the packet, entries are placed in the
    /// order they're listed; with them, where each is has to be fixed to
    /// tell.
    fn checks_last(&mut self, entries: &[Entry<'d>]) -> Option<()> {
        let is_check = |entry: &Entry<'_>| matches!(entry, Entry::Checksum(_));
        let Some(first) = entries.iter().position(is_check) else {
            return Some(());
        };
        let (checksum, after) = if entries.iter().any(|entry| entry.offset().is_some()) {
            let Some(places) = self.static_places(entries) else {
                if let Entry::Checksum(checksum) = &entries[first] {
                    self.report.error(
                        Code::Unsupported,
                        checksum.check,
                        format!(
                            "{} holds this check's value in a packet whose entries are placed \
                             from its start and not all at fixed places, which isn't supported: \
                             MDL can't tell whether only other checks follow it",
                            self.describe(&checksum.key)
                        ),
                    );
                }
                return None;
            };
            let placed: Vec<(&Entry<'d>, (u64, u64))> = entries.iter().zip(places).collect();
            // The earliest check, and an entry that isn't one ending after
            // it starts.
            let earliest = placed
                .iter()
                .filter(|(entry, _)| is_check(entry))
                .min_by_key(|(_, (start, _))| *start);
            let Some((check, (start, _))) = earliest else {
                return Some(());
            };
            let after = placed
                .iter()
                .find(|(entry, (_, end))| !is_check(entry) && end > start)
                .map(|(entry, _)| *entry);
            (*check, after)
        } else {
            let after = entries[first..].iter().find(|entry| !is_check(entry));
            (&entries[first], after)
        };
        if let (Entry::Checksum(checksum), Some(after)) = (checksum, after) {
            let next = match after.name() {
                Some(name) => format!("entry {name}"),
                None => "padding".to_string(),
            };
            self.report.error(
                Code::Unsupported,
                checksum.check,
                format!(
                    "{} holds this check's value, and {next} follows it: XTCE doesn't say whether \
                     a check covers what follows its value, and an MDL checksum covers the bytes \
                     before it, so only other checks may follow",
                    self.describe(&checksum.key)
                ),
            );
            return None;
        }
        Some(())
    }

    /// Whether an entry reads the parameter anywhere: in an aggregate's
    /// members, an array, or a struct type's entries.
    fn holds(&self, entry: &Entry<'d>, key: &ParamKey<'d>) -> bool {
        let same = |other: &Option<ParamKey<'d>>| {
            other
                .as_ref()
                .is_some_and(|other| other.0 == key.0 && other.1 == key.1)
        };
        let within = |ty: &TypeId<'d>| {
            self.structs
                .get(ty)
                .is_some_and(|ty| ty.entries.iter().any(|entry| self.holds(entry, key)))
        };
        match entry {
            Entry::Field(field) => same(&field.key),
            Entry::Array(array) => {
                same(&array.key)
                    || match &array.element {
                        Element::Struct(ty) => within(ty),
                        Element::Scalar(_) => false,
                    }
            }
            Entry::Struct(inner) => within(&inner.ty),
            Entry::Checksum(_) | Entry::Padding { .. } => false,
        }
    }

    /// A checksum wider than a byte is in its family's byte order, as MDL
    /// writes one.
    pub(super) fn check_orders(
        &mut self,
        order: Order,
        packets: &[Packet<'d>],
        commands: &[Command<'d>],
    ) -> Option<()> {
        let entries = packets
            .iter()
            .flat_map(|packet| &packet.entries)
            .chain(commands.iter().flat_map(|command| &command.entries));
        let mut complete = true;
        for entry in entries {
            let Entry::Checksum(checksum) = entry else {
                continue;
            };
            let Some((own, span)) = checksum.scalar.order else {
                continue;
            };
            if own != order {
                self.report.error(
                    Code::Unsupported,
                    span,
                    format!(
                        "{} holds a check's value {}-endian, and an MDL checksum is in its \
                         family's byte order, {}-endian",
                        self.describe(&checksum.key),
                        own.as_str(),
                        order.as_str()
                    ),
                );
                complete = false;
            }
        }
        complete.then_some(())
    }

    /// The params of parameters that hold checks, where no field maps them
    /// too: a checksum is a value of the packet's, not of the catalog.
    pub(super) fn drop_check_params(&mut self, packets: &[Packet<'d>], commands: &[Command<'d>]) {
        let mut mapped = BTreeSet::new();
        let entries = packets
            .iter()
            .flat_map(|packet| &packet.entries)
            .chain(self.structs.values().flat_map(|ty| &ty.entries))
            .chain(commands.iter().flat_map(|command| &command.entries));
        for entry in entries {
            let canonical = match entry {
                Entry::Field(field) => &field.canonical,
                Entry::Array(array) => &array.canonical,
                _ => continue,
            };
            if let Some((name, _)) = canonical {
                mapped.insert(name.clone());
            }
        }
        for key in &self.checked {
            if let Some(Some((name, _))) = self.names.get(key)
                && !mapped.contains(name)
            {
                self.params.remove(name);
            }
        }
    }
}

/// Why a field can't hold a check's value of `width` bits as an MDL
/// checksum: an unsigned integer of that size, always present, whose raw
/// value is its value.
fn unfit(field: &super::Field<'_>, width: u32) -> Option<String> {
    let scalar = &field.scalar;
    if field.when.is_some() {
        return Some("it's included on a condition, and a checksum is always there".to_string());
    }
    if scalar.ty.0 != "uint" {
        return Some(format!(
            "it's {}, and a checksum is an unsigned integer",
            match scalar.ty.0 {
                "int" => "a signed integer",
                "float" => "a float",
                "bool" => "a boolean",
                "string" => "text",
                _ => "bytes",
            }
        ));
    }
    // An integer type with a calibrator is refused where it's read, so an
    // unsigned integer's conversion gives its states.
    if !scalar.states.is_empty() {
        return Some("its values are labels, and a checksum's value is a number".to_string());
    }
    if scalar.conversion.is_some() {
        return Some("it's converted, and a checksum's value is its raw value".to_string());
    }
    match scalar.size_bits {
        Some((bits, _)) if bits == width => None,
        Some((bits, _)) => Some(format!(
            "it's {bits} bits, and the check's value is {width}"
        )),
        None => Some(format!("the check's value is {width} bits")),
    }
}

/// A scalar's size, if it never varies: its bits, or its fixed length in
/// bytes.
fn scalar_bits(scalar: &Scalar<'_>, length: Option<&Value>) -> Option<u64> {
    if let Some((bits, _)) = scalar.size_bits {
        return Some(u64::from(bits));
    }
    let length = length?;
    match length.get("kind").and_then(Value::as_str) {
        Some("fixed") => length.get("bytes")?.as_u64()?.checked_mul(8),
        _ => None,
    }
}
