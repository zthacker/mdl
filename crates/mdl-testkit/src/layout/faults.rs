//! The faults the telemetry oracles plant (`spec/decoding.md` §5, §9): their
//! kinds and routes, as damaged messages report them, and how a message's
//! faults are picked. `body/faults.rs` plants them in binary packets,
//! `json/faults.rs` in JSON ones.

use proptest::prelude::*;
use proptest::sample::Index;

/// A kind of fault, as faults are picked: a kind first, each as likely as
/// the others. The variants are in the order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FaultKind {
    /// A checksum's value, or what it covers, not matching ([`Tamper`]).
    Checksum,
    /// A count field holding a count below 0, or its largest.
    Count,
    /// The payload cut short.
    Cut,
    /// A length field holding a length below 0, or its largest.
    Length,
    /// A required JSON entry with nothing, or `null`, at its pointer
    /// ([`Gone`]).
    Missing,
    /// Part of one more element after an array that runs to the end.
    More,
    /// A number a JSON integer field can't take ([`Unfit`]).
    OutOfRange,
    /// A count field shared by arrays, holding a count the first can have
    /// and a later one can't.
    SharedCount,
    /// A string's bytes that aren't text in its encoding.
    Text,
    /// A JSON value of another kind than its entry takes ([`Wanted`]).
    WrongType,
}

impl FaultKind {
    pub const ALL: [FaultKind; 10] = [
        FaultKind::Checksum,
        FaultKind::Count,
        FaultKind::Cut,
        FaultKind::Length,
        FaultKind::Missing,
        FaultKind::More,
        FaultKind::OutOfRange,
        FaultKind::SharedCount,
        FaultKind::Text,
        FaultKind::WrongType,
    ];

    /// Which packets faults of the kind are planted in.
    pub fn family(self) -> Family {
        match self {
            FaultKind::Checksum
            | FaultKind::Count
            | FaultKind::Cut
            | FaultKind::Length
            | FaultKind::More
            | FaultKind::SharedCount
            | FaultKind::Text => Family::Binary,
            FaultKind::Missing | FaultKind::OutOfRange | FaultKind::WrongType => Family::Json,
        }
    }
}

/// The packets a fault is planted in: binary ones (`body/faults.rs`) or
/// JSON ones (`json/faults.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Binary,
    Json,
}

/// What a step reads, and so where a payload that ends too soon fails. The
/// variants are in the order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    /// An array's count, checked as `count × size` (§5.5).
    Array,
    /// An array's count, checked a bit per element.
    ArrayBitEach,
    /// A checksum's value.
    Checksum,
    /// A scalar element.
    Element,
    /// A field, at the top level or in a struct.
    Field,
    /// A header or time field.
    Header,
    /// The packet's `length`, which a shorter payload is `too_short` for,
    /// before any entry is read (§5.1).
    Length,
    /// Padding at a packet's top level.
    Padding,
    /// Padding in a struct entry.
    PaddingInStruct,
    /// Padding in an array's element.
    PaddingInElement,
    /// A value up to a terminator.
    Terminator,
    /// A CUC time code, padding to decoding.
    TimeCode,
}

impl Place {
    pub const ALL: [Place; 12] = [
        Place::Array,
        Place::ArrayBitEach,
        Place::Checksum,
        Place::Element,
        Place::Field,
        Place::Header,
        Place::Length,
        Place::Padding,
        Place::PaddingInStruct,
        Place::PaddingInElement,
        Place::Terminator,
        Place::TimeCode,
    ];
}

/// Bytes that aren't text in an encoding. The variants are in the order
/// picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Garbling {
    /// A UTF-8 sequence cut off by the value's end.
    CutOff,
    /// An ASCII byte of 0x80 or more.
    High,
    /// A byte UTF-8 never has.
    Never,
    /// An overlong UTF-8 form.
    Overlong,
    /// A UTF-8 code point past U+10FFFF.
    PastMax,
    /// A stray UTF-8 continuation byte.
    Stray,
    /// A UTF-8 surrogate.
    Surrogate,
}

impl Garbling {
    pub const ALL: [Garbling; 7] = [
        Garbling::CutOff,
        Garbling::High,
        Garbling::Never,
        Garbling::Overlong,
        Garbling::PastMax,
        Garbling::Stray,
        Garbling::Surrogate,
    ];
}

/// What a count or length field is made to hold: its largest raw value,
/// or 0 where that gives a count or length below 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Held {
    Largest,
    Negative,
}

impl Held {
    pub const ALL: [Held; 2] = [Held::Largest, Held::Negative];
}

/// What a checksum fault changes. The variants are in the order picking
/// sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tamper {
    /// A bit the checksum covers that nothing else depends on, flipped.
    Covered,
    /// A count, length or string changed as their faults change them,
    /// with each checksum at a static position holding the valid
    /// message's value: decoding verifies those first, so the error is
    /// `bad_checksum`, not the change's own (§5.6).
    Hidden,
    /// A bit of the checksum's value, flipped.
    Stored,
}

impl Tamper {
    pub const ALL: [Tamper; 3] = [Tamper::Covered, Tamper::Hidden, Tamper::Stored];
}

/// How a required member goes missing (§5.3). The variants are in the
/// order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Gone {
    /// `null` in its place.
    Null,
    /// The object its pointer goes through removed.
    Parent,
    /// Removed.
    Removed,
}

impl Gone {
    pub const ALL: [Gone; 3] = [Gone::Null, Gone::Parent, Gone::Removed];
}

/// A number a JSON integer field can't take (§2.3, §5.3). The variants
/// are in the order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unfit {
    /// A fraction: `1.5`.
    Fraction,
    /// Past ±(2^53 − 1), with an exponent: `1e20`.
    Large,
    /// An integer literal past the field's 64 bits.
    Literal,
    /// Below 0, for a `uint`.
    Negative,
    /// 2^53 with a fraction of zeros, past what a fraction keeps an
    /// integer: `9007199254740992.0`.
    PastSafe,
}

impl Unfit {
    pub const ALL: [Unfit; 5] = [
        Unfit::Fraction,
        Unfit::Large,
        Unfit::Literal,
        Unfit::Negative,
        Unfit::PastSafe,
    ];
}

/// A JSON value's kind (§2.3), as `wrong_type` reports what it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JsonKind {
    Array,
    Bool,
    Null,
    Number,
    Object,
    String,
}

impl JsonKind {
    pub const ALL: [JsonKind; 6] = [
        JsonKind::Array,
        JsonKind::Bool,
        JsonKind::Null,
        JsonKind::Number,
        JsonKind::Object,
        JsonKind::String,
    ];
}

/// What a JSON entry takes (§5.3), as `wrong_type` reports it: a field's
/// type, an array, or an object, for a struct or a record. The variants
/// are in the order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Wanted {
    Array,
    Bool,
    Float,
    Int,
    Object,
    String,
    Uint,
}

impl Wanted {
    pub const ALL: [Wanted; 7] = [
        Wanted::Array,
        Wanted::Bool,
        Wanted::Float,
        Wanted::Int,
        Wanted::Object,
        Wanted::String,
        Wanted::Uint,
    ];
}

/// A fault's route: its kind, and what it reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Route {
    Checksum(Tamper),
    Count(Held),
    /// A cut, by what decoding then reads short.
    Cut(Place),
    Length(Held),
    Missing(Gone),
    /// One more element, by what in it decoding reads short.
    More(Place),
    OutOfRange(Unfit),
    SharedCount,
    Text(Garbling),
    /// A value of another kind, by what the entry takes.
    WrongType(Wanted),
}

impl Route {
    pub fn kind(self) -> FaultKind {
        match self {
            Route::Checksum(_) => FaultKind::Checksum,
            Route::Count(_) => FaultKind::Count,
            Route::Cut(_) => FaultKind::Cut,
            Route::Length(_) => FaultKind::Length,
            Route::Missing(_) => FaultKind::Missing,
            Route::More(_) => FaultKind::More,
            Route::OutOfRange(_) => FaultKind::OutOfRange,
            Route::SharedCount => FaultKind::SharedCount,
            Route::Text(_) => FaultKind::Text,
            Route::WrongType(_) => FaultKind::WrongType,
        }
    }
}

/// What a damaged message's faults are picked by: a kind, a route of it
/// and one of the route's faults, for the first fault and for a second,
/// planted a third of the time; and the fault's details (where a garbled
/// value's bytes go, which bit is flipped, what a JSON value becomes).
#[derive(Debug, Clone)]
pub(in crate::layout) struct Picks {
    pub(in crate::layout) first: [Index; 3],
    pub(in crate::layout) second: Option<[Index; 3]>,
    pub(in crate::layout) detail: Index,
}

impl Picks {
    pub(in crate::layout) fn arbitrary() -> BoxedStrategy<Picks> {
        (
            any::<[Index; 3]>(),
            proptest::option::weighted(0.35, any::<[Index; 3]>()),
            any::<Index>(),
        )
            .prop_map(|(first, second, detail)| Picks {
                first,
                second,
                detail,
            })
            .boxed()
    }
}

/// A plant among those `allowed` by kind, each plant's route as `route`
/// gives it: a kind, a route of it and a plant of the route, by `picks`.
pub(in crate::layout) fn pick<'p, P>(
    plants: &'p [P],
    route: impl Fn(&P) -> Route,
    allowed: impl Fn(FaultKind) -> bool,
    picks: &[Index; 3],
) -> Option<&'p P> {
    let [kind, picked, which] = picks;
    let mut kinds: Vec<FaultKind> = plants
        .iter()
        .map(|plant| route(plant).kind())
        .filter(|kind| allowed(*kind))
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    let kind = *kinds.get(kind.index(kinds.len().max(1)))?;
    let mut routes: Vec<Route> = plants
        .iter()
        .map(&route)
        .filter(|route| route.kind() == kind)
        .collect();
    routes.sort_unstable();
    routes.dedup();
    let chosen = *routes.get(picked.index(routes.len().max(1)))?;
    let of_route: Vec<&P> = plants
        .iter()
        .filter(|plant| route(plant) == chosen)
        .collect();
    of_route.get(which.index(of_route.len().max(1))).copied()
}
