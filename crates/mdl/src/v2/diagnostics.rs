//! Problems found in a definition, all of them at once.
//!
//! Each diagnostic has a stable [`Code`], a JSON Pointer (RFC 6901) into the
//! bundle, a human context ("set vendor_x_v2, packet HK, field mode") and a
//! message.
//! Codes are the part other implementations must agree on: the conformance
//! suite lists the codes an invalid document produces, while the wording of
//! messages is free.

use std::fmt;

/// What kind of problem a diagnostic reports. Stable once released: a code
/// is never renamed or reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Code {
    /// A name doesn't follow its naming rule.
    InvalidName,
    /// Two things in the same scope share a name.
    DuplicateName,
    UnknownFamily,
    UnknownPacket,
    UnknownEntry,
    UnknownType,
    UnknownCanonical,
    /// Struct types that contain themselves.
    TypeCycle,
    /// `byte_order` missing for a binary family, or present for another.
    ByteOrder,
    /// A locator, time source or identify key that the encoding can't use.
    WrongEncoding,
    /// A bit size outside what the value can have.
    BitSize,
    InvalidPointer,
    /// A set `version` missing, unexpected, or repeated within its family.
    SetVersion,
    /// A `fixed` version locator, which would select one set for every
    /// message.
    FixedVersion,
    /// A CUC time code with an unsupported number of bytes.
    CucSize,
    InvalidEpoch,
    /// `time_field` on a packet whose set doesn't use boot time.
    TimeField,
    /// Two packets in a set that can match the same message.
    AmbiguousIdentify,
    /// A list or text that must not be empty.
    Empty,
    /// An integer outside ±(2^53 − 1).
    UnsafeInteger,
    NonFinite,
    /// Limits that decrease from red low to red high.
    LimitsOrder,
    /// Limits, states or a frame on a param type that can't have them.
    ParamShape,
    /// A sample interval that isn't positive.
    SampleInterval,
    /// A metadata name the family doesn't declare.
    UnknownMetadata,
    /// The same value listed twice where values must be distinct.
    DuplicateValue,
    /// Struct types or conditions nested deeper than the limit.
    NestingDepth,
    /// A time correlation that names one entry for both clocks.
    CorrelateFields,
    /// An optional value written out although it equals its default.
    ExplicitDefault,
    /// An integer written as a string although a JSON number holds it.
    IntegerAsString,
    /// An identify value its key can never read: the wrong type, or out of
    /// the field's range.
    IdentifyValue,
    /// A list that means a set, not written in sorted order.
    NotSorted,
    /// Two entries that read the same bits, or an entry placed where an
    /// earlier variable-size entry may reach.
    Overlap,
    /// An entry or time code that runs past the packet's exact length, or a
    /// locator a packet is too short for.
    OutOfBounds,
    /// A size or length that doesn't fit the entry's type.
    FieldSize,
    /// Bits that the byte order or type can't read where they are.
    Alignment,
    /// A reference to an entry decoded later.
    ForwardReference,
    /// A reference to an entry of the wrong kind: a count from a signed
    /// field, a condition on a float, a length from a conditional field.
    ReferenceType,
    /// A conversion the field's type can't take.
    ConversionType,
    /// A sentinel or state value the field can never hold.
    ValueRange,
    /// A field or array that can't produce what its canonical param holds.
    CanonicalType,
    /// A unit that differs from the canonical param's.
    UnitMismatch,
    UnknownUnit,
    /// A source frame consumers can't convert to the param's frame.
    FrameMismatch,
    /// A composite with the wrong members for its param.
    CompositeShape,
    /// A newer catalog without a param the older one had.
    RemovedParam,
    /// A newer catalog that changes what a param means.
    ChangedParam,
    /// An entry after one that reads to the end of its container, or a
    /// read-to-the-end inside a struct type (which has no end of its own).
    ReadsToEnd,
    /// Two entries of one packet mapping to the same canonical param.
    DuplicateMapping,
    /// A binding naming a set its family doesn't have.
    UnknownSet,
    /// Text that isn't MDL's one spelling of an instant.
    InvalidInstant,
    /// Two bindings rows for one native id from the same instant.
    DuplicateBinding,
    /// A bindings row that is neither a binding nor a retirement: no asset
    /// and not retired, or retired but naming an asset or set.
    BindingShape,
    /// A binding that names a set when its family reads a version from
    /// each message, or none when the family doesn't.
    BindingSet,
    /// A native id the family's alias locator can't produce.
    NativeId,
    /// A command packet for a command the catalog doesn't have.
    UnknownCommand,
    /// A field encoding an argument its command doesn't have.
    UnknownArgument,
    /// A command argument no field of the command packet encodes.
    UnmappedArgument,
    /// An argument default that doesn't fit the argument.
    DefaultValue,
    /// A command-only key (`value`, `argument`, `inverse`) in telemetry.
    CommandOnly,
    /// Something a command packet can't hold, or a field that gets its
    /// value from more than one place.
    CommandShape,
    /// A command field that gets no value: no constant, no argument, and
    /// no later entry takes a length or count from it.
    NoValue,
    /// A newer catalog without a command the older one had.
    RemovedCommand,
    /// A newer catalog that changes what a command means.
    ChangedCommand,
    /// A checksum that can't be computed as written: a width that isn't
    /// whole bytes from 8 to 64, a parameter that doesn't fit it, a range
    /// past the checksum, or a checksum inside a struct type.
    Checksum,
}

impl Code {
    /// The code as written in conformance vectors and tool output.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::InvalidName => "invalid_name",
            Code::DuplicateName => "duplicate_name",
            Code::UnknownFamily => "unknown_family",
            Code::UnknownPacket => "unknown_packet",
            Code::UnknownEntry => "unknown_entry",
            Code::UnknownType => "unknown_type",
            Code::UnknownCanonical => "unknown_canonical",
            Code::TypeCycle => "type_cycle",
            Code::ByteOrder => "byte_order",
            Code::WrongEncoding => "wrong_encoding",
            Code::BitSize => "bit_size",
            Code::InvalidPointer => "invalid_pointer",
            Code::SetVersion => "set_version",
            Code::FixedVersion => "fixed_version",
            Code::CucSize => "cuc_size",
            Code::InvalidEpoch => "invalid_epoch",
            Code::TimeField => "time_field",
            Code::AmbiguousIdentify => "ambiguous_identify",
            Code::Empty => "empty",
            Code::UnsafeInteger => "unsafe_integer",
            Code::NonFinite => "non_finite",
            Code::LimitsOrder => "limits_order",
            Code::ParamShape => "param_shape",
            Code::SampleInterval => "sample_interval",
            Code::UnknownMetadata => "unknown_metadata",
            Code::DuplicateValue => "duplicate_value",
            Code::NestingDepth => "nesting_depth",
            Code::CorrelateFields => "correlate_fields",
            Code::ExplicitDefault => "explicit_default",
            Code::IntegerAsString => "integer_as_string",
            Code::IdentifyValue => "identify_value",
            Code::NotSorted => "not_sorted",
            Code::Overlap => "overlap",
            Code::OutOfBounds => "out_of_bounds",
            Code::FieldSize => "field_size",
            Code::Alignment => "alignment",
            Code::ForwardReference => "forward_reference",
            Code::ReferenceType => "reference_type",
            Code::ConversionType => "conversion_type",
            Code::ValueRange => "value_range",
            Code::CanonicalType => "canonical_type",
            Code::UnitMismatch => "unit_mismatch",
            Code::UnknownUnit => "unknown_unit",
            Code::FrameMismatch => "frame_mismatch",
            Code::CompositeShape => "composite_shape",
            Code::RemovedParam => "removed_param",
            Code::ChangedParam => "changed_param",
            Code::ReadsToEnd => "reads_to_end",
            Code::DuplicateMapping => "duplicate_mapping",
            Code::UnknownSet => "unknown_set",
            Code::InvalidInstant => "invalid_instant",
            Code::DuplicateBinding => "duplicate_binding",
            Code::BindingShape => "binding_shape",
            Code::BindingSet => "binding_set",
            Code::NativeId => "native_id",
            Code::UnknownCommand => "unknown_command",
            Code::UnknownArgument => "unknown_argument",
            Code::UnmappedArgument => "unmapped_argument",
            Code::DefaultValue => "default_value",
            Code::CommandOnly => "command_only",
            Code::CommandShape => "command_shape",
            Code::NoValue => "no_value",
            Code::RemovedCommand => "removed_command",
            Code::ChangedCommand => "changed_command",
            Code::Checksum => "checksum",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One problem.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Diagnostic {
    pub code: Code,
    /// Where in the bundle, as a JSON Pointer (RFC 6901) like
    /// `/sets/0/packets/2/entries/5`, so tools (a frontend mapping back to
    /// its source file, an editor) can find it. The empty string is the
    /// whole bundle.
    pub path: String,
    /// Where, for people: "set vendor_x_v2, packet HK, entry mode".
    pub context: String,
    pub message: String,
    /// For something defined twice, where the first one is, as a JSON
    /// Pointer; the diagnostic's own path is the second.
    pub first: Option<String>,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if !self.context.is_empty() {
            write!(f, "{}: ", self.context)?;
        }
        write!(f, "{}", self.message)?;
        if let Some(first) = &self.first {
            write!(f, "; the first is at {first}")?;
        }
        write!(f, " [{}]", self.code)
    }
}

/// Every problem in a definition, sorted by where it is in the canonical
/// document (object keys in canonical order, list items by index), then by
/// code, so every implementation lists them the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostics(Vec<Diagnostic>);

impl Diagnostics {
    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.0
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type Item = &'a Diagnostic;
    type IntoIter = std::slice::Iter<'a, Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for diagnostic in &self.0 {
            writeln!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostics {}

/// One step of a JSON Pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Key(String),
    Index(usize),
}

/// Object keys in canonical (UTF-16) order, list items by index.
fn compare_paths(a: &[Segment], b: &[Segment]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (x, y) in a.iter().zip(b) {
        let order = match (x, y) {
            (Segment::Index(x), Segment::Index(y)) => x.cmp(y),
            (Segment::Key(x), Segment::Key(y)) => x.encode_utf16().cmp(y.encode_utf16()),
            (Segment::Index(_), Segment::Key(_)) => Ordering::Less,
            (Segment::Key(_), Segment::Index(_)) => Ordering::Greater,
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

fn pointer(segments: &[Segment]) -> String {
    let mut out = String::new();
    for segment in segments {
        out.push('/');
        match segment {
            Segment::Key(key) => out.push_str(&escape_pointer_token(key)),
            Segment::Index(index) => out.push_str(&index.to_string()),
        }
    }
    out
}

/// RFC 6901: `~` is written `~0` and `/` is written `~1` inside a token.
pub fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Where the validator is: a JSON Pointer and a human context, extended as
/// it walks down.
#[derive(Debug, Clone)]
pub(crate) struct Place {
    segments: Vec<Segment>,
    context: String,
}

impl Place {
    pub(crate) fn root(context: impl Into<String>) -> Self {
        Place {
            segments: Vec::new(),
            context: context.into(),
        }
    }

    /// A member of an object: `time`, `alias`.
    pub(crate) fn key(&self, key: &str) -> Place {
        let mut place = self.clone();
        place.segments.push(Segment::Key(key.to_string()));
        place
    }

    /// An item of a list that people don't name: `states/3`.
    pub(crate) fn at(&self, list: &str, index: usize) -> Place {
        let mut place = self.key(list);
        place.segments.push(Segment::Index(index));
        place
    }

    /// A section that people name too: `catalog`.
    pub(crate) fn section(&self, key: &str) -> Place {
        let mut place = self.key(key);
        place.push_context(key, "");
        place
    }

    /// An entry of a map keyed by name: `params/eps.mode`.
    pub(crate) fn map_item(&self, map: &str, key: &str, label: &str) -> Place {
        let mut place = self.key(map);
        place.segments.push(Segment::Key(key.to_string()));
        place.push_context(label, key);
        place
    }

    /// An item of a list, with its label and name for the context.
    pub(crate) fn item(&self, list: &str, index: usize, label: &str, name: &str) -> Place {
        let mut place = self.at(list, index);
        place.push_context(label, name);
        place
    }

    fn push_context(&mut self, label: &str, name: &str) {
        let part = if name.is_empty() {
            label.to_string()
        } else {
            format!("{label} {name}")
        };
        if self.context.is_empty() {
            self.context = part;
        } else {
            self.context = format!("{}, {part}", self.context);
        }
    }
}

/// Collects diagnostics during one validation.
#[derive(Debug, Default)]
pub(crate) struct Report {
    diagnostics: Vec<(Vec<Segment>, Diagnostic)>,
}

impl Report {
    pub(crate) fn error(&mut self, code: Code, place: &Place, message: impl Into<String>) {
        let diagnostic = Diagnostic {
            code,
            path: pointer(&place.segments),
            context: place.context.clone(),
            message: message.into(),
            first: None,
        };
        self.diagnostics.push((place.segments.clone(), diagnostic));
    }

    /// Something at `place` that was already defined at `first`.
    pub(crate) fn duplicate(
        &mut self,
        code: Code,
        place: &Place,
        first: &Place,
        message: impl Into<String>,
    ) {
        self.error(code, place, message);
        if let Some((_, diagnostic)) = self.diagnostics.last_mut() {
            diagnostic.first = Some(pointer(&first.segments));
        }
    }

    /// Whether anything at or under `place` was reported, so a later check
    /// can leave out a part whose problems are already known instead of
    /// echoing them.
    pub(crate) fn reported_under(&self, place: &Place) -> bool {
        self.diagnostics
            .iter()
            .any(|(segments, _)| segments.starts_with(&place.segments))
    }

    /// The value if nothing went wrong, otherwise every diagnostic in
    /// document order, then by code.
    pub(crate) fn finish<T>(mut self, value: T) -> Result<T, Diagnostics> {
        if self.diagnostics.is_empty() {
            return Ok(value);
        }
        self.diagnostics.sort_by(|a, b| {
            compare_paths(&a.0, &b.0).then_with(|| a.1.code.as_str().cmp(b.1.code.as_str()))
        });
        Err(Diagnostics(
            self.diagnostics.into_iter().map(|(_, d)| d).collect(),
        ))
    }
}
