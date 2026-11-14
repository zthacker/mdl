//! The faults the telemetry oracles plant (`spec/decoding.md` §2.3, §3 to
//! §5, §9): their kinds and routes, as damaged messages report them, and
//! how a message's faults are picked. `body/faults.rs` plants them in
//! binary packets, `json/faults.rs` in JSON ones.

use proptest::prelude::*;
use proptest::sample::Index;

/// A kind of fault, as faults are picked: a kind first, each as likely as
/// the others. The variants are in the order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FaultKind {
    /// A JSON message delivered as a binary payload.
    AsBinary,
    /// A binary message delivered as a JSON payload.
    AsJson,
    /// A checksum's value, or what it covers, not matching ([`Tamper`]).
    Checksum,
    /// A count field holding a count below 0, or its largest.
    Count,
    /// The payload cut short.
    Cut,
    /// A JSON document that isn't an object, by what it is ([`JsonKind`]).
    Document,
    /// A length field holding a length below 0, or its largest.
    Length,
    /// A `path` alias finding what it can't read ([`Unusable`]).
    Locator,
    /// A payload that isn't JSON MDL reads ([`Malformed`]).
    Malformed,
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
    /// A `path` alias reading a native id no binding names.
    Unbound,
    /// A JSON `kind` that no packet matches, in a set without a catch-all.
    Unmatched,
    /// A JSON value of another kind than its entry takes ([`Wanted`]).
    WrongType,
}

impl FaultKind {
    pub const ALL: [FaultKind; 17] = [
        FaultKind::AsBinary,
        FaultKind::AsJson,
        FaultKind::Checksum,
        FaultKind::Count,
        FaultKind::Cut,
        FaultKind::Document,
        FaultKind::Length,
        FaultKind::Locator,
        FaultKind::Malformed,
        FaultKind::Missing,
        FaultKind::More,
        FaultKind::OutOfRange,
        FaultKind::SharedCount,
        FaultKind::Text,
        FaultKind::Unbound,
        FaultKind::Unmatched,
        FaultKind::WrongType,
    ];

    /// Which packets faults of the kind are planted in.
    pub fn family(self) -> Family {
        match self {
            FaultKind::AsJson
            | FaultKind::Checksum
            | FaultKind::Count
            | FaultKind::Cut
            | FaultKind::Length
            | FaultKind::More
            | FaultKind::SharedCount
            | FaultKind::Text => Family::Binary,
            FaultKind::AsBinary
            | FaultKind::Document
            | FaultKind::Locator
            | FaultKind::Malformed
            | FaultKind::Missing
            | FaultKind::OutOfRange
            | FaultKind::Unbound
            | FaultKind::Unmatched
            | FaultKind::WrongType => Family::Json,
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
    /// The object its pointer reads `01` or `-` in made an array, which
    /// those tokens, not being indices, find nothing in (RFC 6901), with
    /// the value where a reader taking them for one would look.
    NotIndex,
    /// `null` in its place.
    Null,
    /// The object its pointer goes through removed.
    Parent,
    /// Removed; an array's element removed with those after it.
    Removed,
}

impl Gone {
    pub const ALL: [Gone; 4] = [Gone::NotIndex, Gone::Null, Gone::Parent, Gone::Removed];
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

/// A JSON value's kind (§2.3), as `wrong_type` reports what an entry
/// takes and what it found.
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

/// What a JSON entry takes (§5.3): a field's type, an array, or an object,
/// for a struct or a record. `wrong_type` reports its kind of value, and
/// `out_of_range` whether an integer field is signed. The variants are in
/// the order picking sorts them in.
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

/// What a locator or an identify key finds that it can't read (§3.1,
/// §4): nothing, or a value that's neither a string nor an integer
/// (§2.3). The variants are in the order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unusable {
    Array,
    Bool,
    /// A fraction: `1.5`.
    Fraction,
    /// Nothing there.
    Gone,
    /// Past ±(2^53 − 1), with an exponent: `1e20`.
    Large,
    Null,
    Object,
    /// An integer literal past 64 bits.
    Wide,
}

impl Unusable {
    pub const ALL: [Unusable; 8] = [
        Unusable::Array,
        Unusable::Bool,
        Unusable::Fraction,
        Unusable::Gone,
        Unusable::Large,
        Unusable::Null,
        Unusable::Object,
        Unusable::Wide,
    ];
}

/// Why a message's metadata `kind` matches no packet (§4, §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unmet {
    /// Not given.
    Missing,
    /// Given twice, so it can't be read.
    Twice,
    /// A value no packet has: a numbered kind's text with a leading zero,
    /// which isn't its integer's one spelling, or other text.
    Unknown,
}

impl Unmet {
    pub const ALL: [Unmet; 3] = [Unmet::Missing, Unmet::Twice, Unmet::Unknown];
}

/// How a payload stops being JSON MDL reads (§2.3). The variants are in
/// the order picking sorts them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Malformed {
    /// A byte order mark before the document.
    Bom,
    /// The text cut before the document ends.
    Cut,
    /// Arrays and objects nested 128 deep.
    Deep,
    /// A name twice in one object, at the top or deeper.
    Duplicate,
    /// No document: nothing, or only whitespace.
    Empty,
    /// A character JSON doesn't allow raw in a string, or an escape it
    /// doesn't have.
    Escape,
    /// A number past binary64.
    Huge,
    /// A number JSON doesn't allow: `01`, `+1`, `.5`, `NaN`.
    Number,
    /// Two names in one object that differ only in how they're escaped.
    Respelled,
    /// An escaped surrogate with no partner.
    Surrogate,
    /// A token JSON doesn't have, a missing one, or whitespace it doesn't
    /// allow.
    Token,
    /// A token after the document.
    Trailing,
    /// Bytes that aren't UTF-8.
    Utf8,
}

impl Malformed {
    pub const ALL: [Malformed; 13] = [
        Malformed::Bom,
        Malformed::Cut,
        Malformed::Deep,
        Malformed::Duplicate,
        Malformed::Empty,
        Malformed::Escape,
        Malformed::Huge,
        Malformed::Number,
        Malformed::Respelled,
        Malformed::Surrogate,
        Malformed::Token,
        Malformed::Trailing,
        Malformed::Utf8,
    ];
}

/// A fault's route: its kind, and what it reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Route {
    AsBinary,
    AsJson,
    Checksum(Tamper),
    Count(Held),
    /// A cut, by what decoding then reads short.
    Cut(Place),
    /// The document replaced by a value of this kind.
    Document(JsonKind),
    /// A numbered `kind` written as text, which a pointer key compares
    /// exactly (§3.2).
    KindAsText,
    Length(Held),
    /// What the alias's pointer finds.
    Locator(Unusable),
    Malformed(Malformed),
    Missing(Gone),
    /// One more element, by what in it decoding reads short.
    More(Place),
    OutOfRange(Unfit),
    SharedCount,
    Text(Garbling),
    Unbound,
    /// What `/kind` holds.
    Unmatched(Unusable),
    /// What's wrong with the metadata `kind`.
    Unmet(Unmet),
    /// A value of another kind, by what the entry takes.
    WrongType(Wanted),
}

impl Route {
    pub fn kind(self) -> FaultKind {
        match self {
            Route::AsBinary => FaultKind::AsBinary,
            Route::AsJson => FaultKind::AsJson,
            Route::Checksum(_) => FaultKind::Checksum,
            Route::Count(_) => FaultKind::Count,
            Route::Cut(_) => FaultKind::Cut,
            Route::Document(_) => FaultKind::Document,
            Route::KindAsText | Route::Unmatched(_) | Route::Unmet(_) => FaultKind::Unmatched,
            Route::Length(_) => FaultKind::Length,
            Route::Locator(_) => FaultKind::Locator,
            Route::Malformed(_) => FaultKind::Malformed,
            Route::Missing(_) => FaultKind::Missing,
            Route::More(_) => FaultKind::More,
            Route::OutOfRange(_) => FaultKind::OutOfRange,
            Route::SharedCount => FaultKind::SharedCount,
            Route::Text(_) => FaultKind::Text,
            Route::Unbound => FaultKind::Unbound,
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
