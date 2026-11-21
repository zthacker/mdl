//! Valid command layouts and requests for them, with the payload each
//! encodes to and what decoding that payload gives back, worked out from
//! `spec/encoding.md` on this crate's own reading, not from `mdl`: the
//! oracle the encoder and the command decoder are checked against.
//!
//! A generated [`Commands`] is a binary family with one set of one to three
//! command packets, told apart by a constant first field (an integer, or a
//! tag in a fixed-length buffer) and maybe link metadata. A packet writes
//! constants; arguments of every type, through each way §3.1 turns a value
//! into a raw value (unconverted, linear, states, a table read backwards,
//! an explicit inverse), in units the field's may differ from; maybe the
//! target's native id; strings and bytes of every length kind (a length
//! field holding a computed length); up to two arrays (§3.6), each of a
//! list argument's values or of records whose fields take list arguments
//! or constants, counted by a fixed count, a count field (which two arrays
//! may share) or the rest of the payload; and up to two checksums of any
//! algorithm and parameters, the later covering the earlier when it
//! reaches back that far. Entries are laid out as the telemetry layouts
//! are. A catalog command may have arguments its packet leaves out (§2.4).
//!
//! Requests spell numbers every way §2.2 allows, leave out arguments that
//! have defaults, give lists of every length their bounds and counts allow
//! (one length for the lists that must agree), and lean toward the ends of
//! ranges and toward raw values halfway between two that a field holds,
//! where rounding decides (ties away from zero for integers, to even for
//! binary32).

use std::ops::RangeInclusive;

use proptest::collection::vec;
use proptest::prelude::*;
use serde_json::Value;

use crate::checksum::Algorithm;
use crate::layout::{Form, Numeric, Raw, Text};

use bundle::assemble;
use faults::{garbling, json_garbling, refusal};
use packets::{command_spec, key, lay_out};
use requests::request;

/// A family, its one set of command packets, and requests for them.
#[derive(Debug, Clone)]
pub struct Commands {
    /// The bundle: a catalog of commands, the family and the set.
    pub bundle: Value,
    pub family: String,
    pub set: String,
    pub requests: Vec<Request>,
    /// Requests with faults planted in them.
    pub refused: Vec<Refused>,
    /// Payloads with faults planted in them.
    pub garbled: Vec<Garbled>,
}

/// A request with one or two faults planted in it, and the error encoding
/// gives: the first problem `spec/encoding.md` reaches (the set and
/// command; §2.1's two passes; §3's entries in order, then length fields,
/// then counts).
#[derive(Debug, Clone)]
pub struct Refused {
    pub set: String,
    pub command: String,
    /// The arguments it gives, in the order it gives them.
    pub arguments: Vec<(String, Given)>,
    pub target: Option<Target>,
    pub code: &'static str,
    /// What the error names, if it names anything: an argument or field, a
    /// list's value (`a3[2]`), or an element and its record field
    /// (`f2[1].r0`).
    pub at: Option<String>,
    /// Every fault in it, planted or found by §2.1's first pass: the error
    /// is the one caught first.
    pub faults: Vec<Fault>,
}

/// A fault in a request: the kind it was planted as, the code it gives on
/// its own, where encoding catches it, the catalog argument it's about if
/// it's about one, and what the error names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    pub kind: RefusedKind,
    pub code: &'static str,
    pub stage: Stage,
    pub argument: Option<usize>,
    pub at: Option<String>,
}

/// A kind of fault in a request. Faults are picked by kind first, each by
/// its weight, however many of it the command has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefusedKind {
    /// A set the family doesn't have, or a command the set doesn't encode.
    Request,
    /// An argument the command doesn't have, or one given again.
    Name,
    /// An argument without a default, left out.
    Missing,
    /// A list argument's fault.
    ListValue,
    /// A value the catalog refuses for an argument that takes one.
    Value,
    /// A value whose length its length field can't hold.
    Length,
    /// A value its field can't hold.
    Field,
    /// A value an element's field can't hold.
    Element,
    /// An array's records given lists of two lengths.
    Records,
    /// A count field given a length its `multiply` doesn't divide.
    Multiply,
    /// Two arrays sharing a count field given two counts.
    Shared,
    /// The target replaced, or left out.
    Target,
}

impl RefusedKind {
    pub const ALL: [RefusedKind; 12] = [
        RefusedKind::Request,
        RefusedKind::Name,
        RefusedKind::Missing,
        RefusedKind::ListValue,
        RefusedKind::Value,
        RefusedKind::Length,
        RefusedKind::Field,
        RefusedKind::Element,
        RefusedKind::Records,
        RefusedKind::Multiply,
        RefusedKind::Shared,
        RefusedKind::Target,
    ];

    /// How likely the kind is to be picked, against the others: those
    /// any request can be given (an unknown set or command, a name, a
    /// missing argument) a quarter as likely as those that need what the
    /// layout has.
    fn weight(self) -> u32 {
        match self {
            RefusedKind::Request | RefusedKind::Name | RefusedKind::Missing => 1,
            RefusedKind::ListValue
            | RefusedKind::Value
            | RefusedKind::Length
            | RefusedKind::Field
            | RefusedKind::Element
            | RefusedKind::Records
            | RefusedKind::Multiply
            | RefusedKind::Shared
            | RefusedKind::Target => 4,
        }
    }

    /// Whether faults of the kind are caught before every argument and
    /// entry, so that planted with another, they'd hide it.
    fn hides_others(self) -> bool {
        match self {
            RefusedKind::Request | RefusedKind::Name => true,
            RefusedKind::Missing
            | RefusedKind::ListValue
            | RefusedKind::Value
            | RefusedKind::Length
            | RefusedKind::Field
            | RefusedKind::Element
            | RefusedKind::Records
            | RefusedKind::Multiply
            | RefusedKind::Shared
            | RefusedKind::Target => false,
        }
    }
}

/// Where encoding catches a fault, in the order it checks things: the
/// request (§1); a given name, by its place in the request; an argument,
/// by catalog index; an entry, by index; then a length field, by the index
/// of the entry it measures (§3.5); then an array's count, by the array's
/// index (§3.6). Don't reorder the variants: the derived order is the
/// check order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Request,
    Name(usize),
    Argument(usize),
    /// An entry. In an array, the lengths of its records' lists are
    /// compared first (`element: None`), then each element in order, and
    /// in a record each field in order: `Some((element, position))`.
    Field {
        entry: usize,
        element: Option<(usize, usize)>,
    },
    Length(usize),
    Count(usize),
}

/// A payload with a fault planted in it, and the code decoding it gives
/// (`spec/encoding.md` §7).
#[derive(Debug, Clone)]
pub struct Garbled {
    /// The set it's decoded with.
    pub set: String,
    pub payload: Vec<u8>,
    pub encoding: Encoding,
    /// The metadata the link frames it with.
    pub metadata: Vec<(String, i64)>,
    pub code: &'static str,
    /// The argument or entry the error names, if it names one.
    pub at: Option<String>,
    /// The kind of fault, as faults are picked.
    pub kind: GarbledKind,
    /// What was planted, for a failing case's report.
    pub planted: String,
}

/// A kind of fault planted in a payload. Faults are picked by kind first,
/// each by its weight; the variants are in the order picking sorts them
/// in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GarbledKind {
    /// A stored checksum with a bit flipped.
    Checksum,
    /// A constant that isn't a key, holding another value.
    Constant,
    /// The count field of an array holding a count below 0, or its
    /// largest.
    Count,
    /// The payload cut short inside an entry after the keys.
    Cut,
    /// The payload cut inside an array with a known count.
    CutInsideArray,
    /// A JSON payload whose document isn't an object.
    Document,
    /// An element holding a raw value decoding refuses.
    Element,
    /// A JSON payload for a binary family.
    Encoding,
    /// Lists past their bounds, through a payload packed with them.
    ListLength,
    /// A JSON payload's member gone, or `null`, where an entry reads a
    /// value.
    Missing,
    /// Part of one more element after an array that runs to the end.
    MoreBytes,
    /// A payload no packet matches.
    NoPacket,
    /// A JSON payload that isn't JSON MDL reads.
    NotJson,
    /// A `null` element of a JSON array that holds a list, which is no
    /// value.
    Null,
    /// An argument's field holding a raw value decoding refuses.
    Raw,
    /// A record's constant holding another value.
    RecordConstant,
    /// A set the family doesn't have.
    Set,
    /// A JSON number an integer field can't take.
    Unfit,
    /// A JSON value of another type than its entry or element takes.
    WrongType,
}

impl GarbledKind {
    pub const ALL: [GarbledKind; 19] = [
        GarbledKind::Checksum,
        GarbledKind::Constant,
        GarbledKind::Count,
        GarbledKind::Cut,
        GarbledKind::CutInsideArray,
        GarbledKind::Document,
        GarbledKind::Element,
        GarbledKind::Encoding,
        GarbledKind::ListLength,
        GarbledKind::Missing,
        GarbledKind::MoreBytes,
        GarbledKind::NoPacket,
        GarbledKind::NotJson,
        GarbledKind::Null,
        GarbledKind::Raw,
        GarbledKind::RecordConstant,
        GarbledKind::Set,
        GarbledKind::Unfit,
        GarbledKind::WrongType,
    ];

    /// Whether payloads in `encoding` are given faults of this kind.
    pub fn planted_in(self, encoding: Encoding) -> bool {
        let binary = matches!(
            self,
            GarbledKind::Checksum
                | GarbledKind::Count
                | GarbledKind::Cut
                | GarbledKind::CutInsideArray
                | GarbledKind::MoreBytes
        );
        let json = matches!(
            self,
            GarbledKind::Document
                | GarbledKind::Missing
                | GarbledKind::NotJson
                | GarbledKind::Null
                | GarbledKind::Unfit
                | GarbledKind::WrongType
        );
        match encoding {
            Encoding::Binary => !json,
            Encoding::Json => !binary,
        }
    }

    /// How likely the kind is to be picked, against the others: those
    /// any payload can be given (an unknown set, the wrong encoding, no
    /// packet, text that isn't JSON, a document that isn't an object) a
    /// quarter as likely as those that need what the layout has.
    fn weight(self) -> usize {
        match self {
            GarbledKind::Document
            | GarbledKind::Encoding
            | GarbledKind::NoPacket
            | GarbledKind::NotJson
            | GarbledKind::Set => 1,
            GarbledKind::Checksum
            | GarbledKind::Constant
            | GarbledKind::Count
            | GarbledKind::Cut
            | GarbledKind::CutInsideArray
            | GarbledKind::Element
            | GarbledKind::ListLength
            | GarbledKind::Missing
            | GarbledKind::MoreBytes
            | GarbledKind::Null
            | GarbledKind::Raw
            | GarbledKind::RecordConstant
            | GarbledKind::Unfit
            | GarbledKind::WrongType => 4,
        }
    }
}

/// How a payload is given to the decoder: as bytes, or as JSON text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Binary,
    Json,
}

/// A request, what it encodes to, and what decoding that gives back.
#[derive(Debug, Clone)]
pub struct Request {
    pub command: String,
    /// The arguments it gives, in the order it gives them.
    pub arguments: Vec<(String, Given)>,
    /// The target, spelled as the caller spells it.
    pub target: Option<Target>,
    /// The packet's metadata, which the link frames the payload with.
    pub metadata: Vec<(String, i64)>,
    pub payload: Vec<u8>,
    /// Each field's and checksum's raw value, in packet order.
    pub record: Vec<(String, Raw)>,
    /// The arguments it leaves out, which take their defaults, in catalog
    /// order.
    pub defaulted: Vec<String>,
    /// What decoding the payload gives.
    pub decoded: Decoded,
    /// The payload the decoded arguments and target encode to: this one,
    /// when every converted field's inverse undoes its conversion (§7.4).
    pub again: Vec<u8>,
    /// For a JSON packet, the payload written another way (`decoding.md`
    /// §2.3), with members no field reads (§7.3), which decodes as the
    /// payload does.
    pub respelled: Option<Vec<u8>>,
}

/// A decoded command (`spec/encoding.md` §7.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// Every argument of the command, in catalog order.
    pub arguments: Vec<(String, Arg)>,
    /// The arguments the set doesn't encode, which have their defaults.
    pub defaulted: Vec<String>,
    pub target: Option<Target>,
}

/// An argument's value as a request spells it. Numbers compare by value
/// (`spec/encoding.md` §2.2), so a u64 argument may be given as an `Int`,
/// and a whole f64 one as either.
#[derive(Debug, Clone, PartialEq)]
pub enum Given {
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// A list argument's values.
    List(Vec<Given>),
}

/// An argument's value, by its type: an enum's is its state's name, a list
/// argument's its values. Floats compare by their bits, so -0.0 isn't 0.0.
#[derive(Debug, Clone)]
pub enum Arg {
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    Text(String),
    Bytes(Vec<u8>),
    List(Vec<Arg>),
}

impl PartialEq for Arg {
    fn eq(&self, other: &Arg) -> bool {
        match (self, other) {
            (Arg::Bool(a), Arg::Bool(b)) => a == b,
            (Arg::I64(a), Arg::I64(b)) => a == b,
            (Arg::U64(a), Arg::U64(b)) => a == b,
            (Arg::F64(a), Arg::F64(b)) => a.to_bits() == b.to_bits(),
            (Arg::Text(a), Arg::Text(b)) => a == b,
            (Arg::Bytes(a), Arg::Bytes(b)) => a == b,
            (Arg::List(a), Arg::List(b)) => a == b,
            _ => false,
        }
    }
}

/// A native id (`spec/bindings.md` §3): an integer, or text. As a request
/// spells it, text may be an integer's decimal spelling, which is that
/// integer; as decoding gives it, it never is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Int(i128),
    Text(String),
}

/// Any command layout validation accepts, of the kinds this module builds,
/// with one to three requests for each packet.
pub fn commands() -> BoxedStrategy<Commands> {
    (any::<bool>(), any::<bool>())
        .prop_flat_map(|(little, metadata)| {
            (
                Just(little),
                Just(metadata),
                vec(command_spec(little), 1..=3),
            )
        })
        .prop_flat_map(|(little, metadata, specs)| {
            let count = specs.len();
            let apids = vec(0_i64..3, count).prop_map(move |apids| {
                apids
                    .into_iter()
                    .map(|apid| metadata.then_some(apid))
                    .collect::<Vec<_>>()
            });
            (Just(little), Just(metadata), Just(specs), key(count), apids)
        })
        .prop_flat_map(|(little, metadata, specs, (key, values), apids)| {
            let keys = values.clone();
            let layouts: Vec<Layout> = specs
                .into_iter()
                .zip(values)
                .zip(apids)
                .enumerate()
                .map(|(index, ((spec, value), apid))| {
                    lay_out(index, spec, (key.clone(), value), little, apid)
                })
                .collect();
            let requests: Vec<BoxedStrategy<Vec<Request>>> = layouts
                .iter()
                .map(|layout| vec(request(layout.clone()), 1..=3).boxed())
                .collect();
            let refused: Vec<BoxedStrategy<Vec<Refused>>> = layouts
                .iter()
                .map(|layout| vec(refusal(layout.clone()), 1..=3).boxed())
                .collect();
            let garbled: Vec<BoxedStrategy<Vec<Garbled>>> = layouts
                .iter()
                .map(|layout| vec(garbling(layout.clone(), keys.clone()), 1..=3).boxed())
                .collect();
            (
                Just(little),
                Just(metadata),
                Just(layouts),
                requests,
                refused,
                garbled,
            )
        })
        .prop_map(|(little, metadata, layouts, requests, refused, garbled)| {
            let mut case = assemble(Encoding::Binary, little, metadata, &layouts, requests);
            case.refused = refused.into_iter().flatten().collect();
            case.garbled = garbled.into_iter().flatten().collect();
            case
        })
        .boxed()
}

/// Any JSON command layout validation accepts, of the kinds `json.rs`
/// builds, keyed by `op`, by it and metadata, or by metadata alone, with
/// one to three requests for each packet, one to three refused requests,
/// and one to three payloads with faults planted in them
/// (`spec/encoding.md` §5, §7).
pub fn json_commands() -> BoxedStrategy<Commands> {
    (
        json::keying(),
        vec(json::command_spec(), 1..=3),
        json::place(),
    )
        .prop_flat_map(|(keying, specs, key_place)| {
            let count = specs.len();
            (
                Just(keying),
                Just(specs),
                Just(key_place),
                json::key(count),
                json::apids(keying, count),
            )
        })
        .prop_flat_map(|(keying, specs, key_place, (key, values), apids)| {
            let keys = if keying.op() {
                values.clone()
            } else {
                Vec::new()
            };
            let layouts: Vec<Layout> = specs
                .into_iter()
                .zip(values)
                .zip(apids)
                .enumerate()
                .map(|(index, ((spec, value), apid))| {
                    let key = keying.op().then(|| (key.clone(), value));
                    json::lay_out(index, spec, key, &key_place, apid)
                })
                .collect();
            let requests: Vec<BoxedStrategy<Vec<Request>>> = layouts
                .iter()
                .map(|layout| vec(request(layout.clone()), 1..=3).boxed())
                .collect();
            let refused: Vec<BoxedStrategy<Vec<Refused>>> = layouts
                .iter()
                .map(|layout| vec(refusal(layout.clone()), 1..=3).boxed())
                .collect();
            let garbled: Vec<BoxedStrategy<Vec<Garbled>>> = layouts
                .iter()
                .map(|layout| vec(json_garbling(layout.clone(), keys.clone()), 1..=3).boxed())
                .collect();
            (
                Just(keying.metadata()),
                Just(layouts),
                requests,
                refused,
                garbled,
            )
        })
        .prop_map(|(metadata, layouts, requests, refused, garbled)| {
            let mut case = assemble(Encoding::Json, false, metadata, &layouts, requests);
            case.refused = refused.into_iter().flatten().collect();
            case.garbled = garbled.into_iter().flatten().collect();
            case
        })
        .boxed()
}

/// An argument's type (`spec/encoding.md` §2.2).
#[derive(Debug, Clone, PartialEq)]
enum ArgType {
    Bool,
    U64,
    I64,
    F64,
    /// Its states, in catalog order.
    Enum(Vec<String>),
    Str,
    Bytes,
}

impl ArgType {
    fn name(&self) -> &'static str {
        match self {
            ArgType::Bool => "bool",
            ArgType::U64 => "u64",
            ArgType::I64 => "i64",
            ArgType::F64 => "f64",
            ArgType::Enum(_) => "enum",
            ArgType::Str => "str",
            ArgType::Bytes => "bytes",
        }
    }

    /// The integers a u64 or i64 argument can be.
    fn integers(&self) -> RangeInclusive<i128> {
        match self {
            ArgType::U64 => 0..=i128::from(u64::MAX),
            _ => i128::from(i64::MIN)..=i128::from(i64::MAX),
        }
    }
}

/// How many bytes a string or bytes value takes (`spec/encoding.md` §3.4).
#[derive(Debug, Clone, PartialEq)]
enum Len {
    Fixed(usize),
    /// Fixed, the value ending at a terminator, which pads the rest.
    Padded(usize, u8),
    Terminator(u8),
    /// From an earlier length field of `bits`, which holds
    /// `(n − add) / multiply`.
    Field {
        bits: u32,
        multiply: u32,
        add: i64,
    },
    Remaining,
}

impl Len {
    fn terminator(&self) -> Option<u8> {
        match self {
            Len::Padded(_, byte) | Len::Terminator(byte) => Some(*byte),
            _ => None,
        }
    }

    /// Byte lengths a value can have, the short ones.
    fn lengths(&self) -> BoxedStrategy<usize> {
        match self {
            Len::Fixed(bytes) => Just(*bytes).boxed(),
            Len::Padded(bytes, _) => (0..=*bytes).boxed(),
            Len::Terminator(_) | Len::Remaining => (0_usize..=10).boxed(),
            Len::Field { multiply, add, .. } => {
                let multiply = i64::from(*multiply);
                let add = *add;
                // The least raw value that gives a length of at least 0.
                let least = ((-add).max(0) + multiply - 1) / multiply;
                (least..=least + 5)
                    .prop_map(move |raw| (raw * multiply + add) as usize)
                    .boxed()
            }
        }
    }
}

/// A field's type and size.
#[derive(Debug, Clone, PartialEq)]
enum Shape {
    Uint(u32),
    Int(u32, Form),
    Float(u32),
    Bool,
    Text(Text, Len),
    Bytes(Len),
}

/// An int of one of `sizes`: two's complement, or now and then
/// sign-magnitude or ones' complement, which hold more than zero from 2
/// bits on.
fn int_shape(sizes: impl Strategy<Value = u32> + 'static) -> BoxedStrategy<Shape> {
    (sizes, 0_u8..4)
        .prop_map(|(bits, pick)| {
            let form = match pick {
                2 if bits >= 2 => Form::SignMagnitude,
                3 if bits >= 2 => Form::OnesComplement,
                _ => Form::TwosComplement,
            };
            Shape::Int(bits, form)
        })
        .boxed()
}

impl Shape {
    /// The bits it takes, unless its length varies.
    fn bits(&self) -> Option<u32> {
        match self {
            Shape::Uint(bits) | Shape::Int(bits, _) | Shape::Float(bits) => Some(*bits),
            Shape::Bool => Some(1),
            Shape::Text(_, len) | Shape::Bytes(len) => match len {
                Len::Fixed(bytes) | Len::Padded(bytes, _) => Some(*bytes as u32 * 8),
                _ => None,
            },
        }
    }

    fn len(&self) -> Option<&Len> {
        match self {
            Shape::Text(_, len) | Shape::Bytes(len) => Some(len),
            _ => None,
        }
    }

    /// Whether it has a byte order: only integers and floats do.
    fn ordered(&self) -> bool {
        matches!(self, Shape::Uint(_) | Shape::Int(..) | Shape::Float(_))
    }

    /// The integers an integer or bool field holds.
    fn integers(&self) -> Option<RangeInclusive<i128>> {
        match self {
            Shape::Uint(bits) => Some(0..=(1_i128 << bits) - 1),
            Shape::Int(bits, form) => Some(form.range(*bits)),
            Shape::Bool => Some(0..=1),
            _ => None,
        }
    }

    /// The `bits` bits an integer field writes for `n`: through its form
    /// for an int, else as `n`'s low bits.
    fn int_bits(&self, n: i128, bits: u32) -> u64 {
        match self {
            Shape::Int(_, form) => form.bits_of(n, bits),
            _ => crate::layout::fit(n as u64, bits),
        }
    }

    /// The same kind of integer, `bits` wide.
    fn widened(&self, bits: u32) -> Shape {
        match self {
            Shape::Uint(_) => Shape::Uint(bits),
            Shape::Int(_, form) => Shape::Int(bits, *form),
            other => other.clone(),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Shape::Uint(_) => "uint",
            Shape::Int(..) => "int",
            Shape::Float(_) => "float",
            Shape::Bool => "bool",
            Shape::Text(..) => "string",
            Shape::Bytes(_) => "bytes",
        }
    }
}

/// How an argument becomes its field's raw value (`spec/encoding.md`
/// §3.1).
#[derive(Debug, Clone, PartialEq)]
enum Through {
    Plain,
    /// An enum argument's raw value for each state.
    States(Vec<(i128, String)>),
    /// A conversion that inverts exactly: linear, or a linear table whose
    /// values only rise or only fall.
    Exact(Numeric),
    /// A conversion that doesn't, and the field's explicit inverse.
    Inverse {
        conversion: Numeric,
        inverse: Numeric,
    },
}

/// How many values a list argument takes (`list`), either end open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bounds {
    min: Option<u32>,
    max: Option<u32>,
}

/// A catalog argument, and a field that encodes it: for a list argument,
/// an array's element, each value of the list written as that field.
#[derive(Debug, Clone)]
struct Encoder {
    ty: ArgType,
    /// A list argument's bounds.
    list: Option<Bounds>,
    /// The argument's unit and the field's: the same code but for an f64
    /// argument.
    units: Option<(&'static str, &'static str)>,
    /// An f64 argument's range.
    min: Option<f64>,
    max: Option<f64>,
    /// A u64 or i64 argument's range, exactly: its ends past ±(2^53 − 1)
    /// are written as decimal strings.
    integer_range: (Option<i128>, Option<i128>),
    default: Option<Arg>,
    /// Where the range and default are written.
    limits: Limits,
    shape: Shape,
    through: Through,
    /// An enum argument's states the catalog lists and the packet doesn't
    /// accept (IR 2.3), in catalog order; `ty` has those it does, in the
    /// same order. The packet then lists its own in `states`, and a field
    /// through states gives these raw values too, so decoding one is
    /// refused.
    unaccepted: Vec<Unaccepted>,
    /// Whether the packet requires the argument (IR 2.3), so that every
    /// request sent with it gives the argument: `default` is then none,
    /// and the catalog's default, if it has one, is `unsent_default`.
    required: bool,
    unsent_default: Option<Arg>,
}

/// A state the catalog lists that the packet doesn't accept: its position
/// among the catalog's states, and for an argument through states, its raw
/// value in the field's conversion.
#[derive(Debug, Clone, PartialEq)]
struct Unaccepted {
    name: String,
    position: usize,
    raw: Option<i128>,
}

impl Encoder {
    /// Whether it has a range or a default to write.
    fn has_limits(&self) -> bool {
        self.min.is_some()
            || self.max.is_some()
            || self.integer_range != (None, None)
            || self.default.is_some()
    }
}

/// Where an argument's range and default are written.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Limits {
    /// On the catalog's argument.
    Catalog,
    /// On the packet that encodes it (IR 2.2), in the argument's unit, so
    /// encoding keeps to the packet's. The catalog's argument has no range
    /// or default, or with `wider` a range one further out at each end the
    /// type allows, and that range's low end as its default where the
    /// packet's replaces it.
    Packet { wider: bool },
}

fn encoder_of(ty: ArgType, shape: Shape) -> Encoder {
    Encoder {
        ty,
        list: None,
        units: None,
        min: None,
        max: None,
        integer_range: (None, None),
        default: None,
        limits: Limits::Catalog,
        shape,
        through: Through::Plain,
        unaccepted: Vec::new(),
        required: false,
        unsent_default: None,
    }
}

const TEXTS: &[Text] = &[Text::Utf8, Text::Ascii, Text::Latin1];

/// A constant field's value.
#[derive(Debug, Clone, PartialEq)]
enum Const {
    Int(i128),
    Bool(bool),
    Text(String),
}

/// Where a field's raw value comes from.
#[derive(Debug, Clone)]
enum Source {
    Constant(Const),
    /// The argument at this index of the command's.
    Argument(usize),
    Target,
    /// The byte length of the field with this name, which comes later.
    Length(String),
    /// The number of elements of the arrays that take their count from it.
    Count,
}

/// How an array's count is given (`spec/encoding.md` §3.6).
#[derive(Debug, Clone, PartialEq)]
enum Counted {
    Fixed(u32),
    /// The field of this name holds `(n − add) / multiply`.
    Field {
        name: String,
        multiply: u32,
        add: i64,
    },
    Remaining,
}

/// An array's elements.
#[derive(Debug, Clone)]
enum Elements {
    /// The values of the list argument at this index of the command's, each
    /// written as a field of this shape.
    Scalar {
        argument: usize,
        shape: Shape,
        little: bool,
        explicit: bool,
    },
    /// Records of the struct type of this name, its entries laid out from
    /// the record's start: a field that takes a list argument holds its
    /// value *i* in record *i*.
    Records { name: String, entries: Vec<Placed> },
}

/// How an array's length follows from the count it's given: `count ×
/// multiply + add` for a count field (§3.6), the count itself otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scale {
    multiply: u32,
    add: i64,
}

impl Scale {
    const ONE: Scale = Scale {
        multiply: 1,
        add: 0,
    };

    /// Whether the count gives a length of at least 0.
    fn reaches(self, count: i64) -> bool {
        count * i64::from(self.multiply) + self.add >= 0
    }

    /// The length a count gives; groups only hold counts that reach one.
    fn length(self, count: i64) -> usize {
        usize::try_from(count * i64::from(self.multiply) + self.add).unwrap_or(0)
    }
}

/// List arguments whose lengths go together (§2.5, §3.6): those that fill
/// one array's records have one length, and arrays that share a count field
/// give it one value, each array's length following from it by its own
/// `multiply` and `add`.
#[derive(Debug, Clone)]
struct Group {
    /// Each member, and how its length follows from the group's count.
    members: Vec<(usize, Scale)>,
    /// The counts a request may give them.
    counts: Vec<i64>,
    /// The count their defaults give, when any of them has one.
    default: Option<i64>,
}

/// An entry at its place.
#[derive(Debug, Clone)]
enum Placed {
    Field {
        name: String,
        shape: Shape,
        /// Whether it's written little-endian.
        little: bool,
        /// Whether its byte order is spelled.
        explicit: bool,
        source: Source,
        /// In a JSON packet, its pointer's tokens, relative to its object
        /// (RFC 6901 escapes undone); none in a binary one.
        pointer: Vec<String>,
    },
    Padding(u64),
    Checksum {
        name: String,
        algorithm: Algorithm,
        from_bits: u64,
    },
    Array {
        name: String,
        count: Counted,
        elements: Elements,
        /// As a field's.
        pointer: Vec<String>,
    },
}

#[derive(Debug, Clone)]
struct Layout {
    name: String,
    command: String,
    encoding: Encoding,
    little: bool,
    apid: Option<i64>,
    arguments: Vec<(Encoder, bool)>,
    entries: Vec<Placed>,
    /// Whether each entry starts where it does in every payload: a
    /// constant there is a key (`spec/encoding.md` §7.2).
    statics: Vec<bool>,
    groups: Vec<Group>,
}

impl Layout {
    fn target(&self) -> Option<&Shape> {
        self.entries.iter().find_map(|entry| match entry {
            Placed::Field {
                shape,
                source: Source::Target,
                ..
            } => Some(shape),
            _ => None,
        })
    }
}

mod bundle;
mod canonical;
mod encoders;
mod faults;
mod json;
mod packets;
mod requests;
mod values;

#[cfg(test)]
mod tests;
