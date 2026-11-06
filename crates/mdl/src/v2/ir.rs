//! The MDL IR, version 2: the data contract between frontends, the
//! compiler and every implementation that reads artifacts.
//!
//! This module only defines shapes. Whether a document *means* something
//! (names resolve, fields don't overlap, units match the catalog) is the
//! validator's job; it then compiles a valid document into a typed program.
//! So the IR favours a flat, explicit JSON form that any language can
//! produce, over Rust types that make every bad combination
//! unrepresentable.
//!
//! # The JSON contract
//!
//! - Every enum with data is tagged with `"kind"` (`{"kind": "bits", ...}`);
//!   artifacts are tagged with `"artifact"`.
//! - Names are snake_case.
//! - **Strict reading:** unknown fields are errors, everywhere. A reader that
//!   skipped a field it didn't understand (a `when` condition, say) would
//!   silently decode wrong values, and dropping it would change the
//!   artifact's canonical bytes and therefore its identity.
//! - **Typed reading is invariant under canonicalization:** a document and
//!   its canonical form (RFC 8785) are read identically. `16.0` in an
//!   integer field is read as `16`, because that is its canonical text.
//! - **Normal form:** an absent optional value is omitted (never `null`),
//!   and an empty optional list or map is omitted. **Artifacts must be in
//!   normal form**, so an artifact's identity is a function of its value:
//!   two spellings of the same value can't get two digests. Bundles, which
//!   aren't content-addressed, may also use `null` or empty collections.
//!   Lists documented as *required* must be non-empty in a valid document,
//!   so `[]` never appears in one.
//! - **Versions are `"major.minor"`.** Each document records the lowest IR
//!   version that can read it. A minor version adds optional features, and
//!   a reader refuses a document whose minor version is newer than its own,
//!   instead of misreading it.
//! - Integers the IR means as integers are JSON numbers within ±(2^53 − 1).
//!   Wider constants are decimal strings ([`Scalar`]).
//! - Floats are finite. Writing NaN or infinity is an error, not `null`.
//!
//! **Read artifacts with [`crate::v2::read`].** Plain `serde_json` on these
//! types skips the duplicate-key, integer, version and canonical-form checks
//! the contract requires, and loses error locations.
//!
//! # Rust API stability
//!
//! Structs are `#[non_exhaustive]` and enum variants that carry data are
//! too, so a future optional field is a non-breaking addition. Build
//! structs with `new(...)` and set optional fields afterwards; build
//! variants with their constructor functions:
//!
//! ```
//! use mdl::v2::ir::{Conversion, Field, ScalarType};
//! let mut field = Field::new("bus_mv", ScalarType::Uint);
//! field.size_bits = Some(16);
//! field.conversion = Some(Conversion::linear(0.001, 0.0));
//! field.unit = Some("V".into());
//! ```
//!
//! Struct literals don't compile outside this crate, which is what keeps
//! adding a field non-breaking:
//!
//! ```compile_fail,E0639
//! use mdl::v2::ir::Conversion;
//! let scale = Conversion::Linear { scale: 0.001, offset: 0.0 };
//! ```
//!
//! ```compile_fail,E0639
//! use mdl::v2::ir::Count;
//! let count = Count::Remaining {};
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use sha2::Sha256;

// ---- Versions ------------------------------------------------------------------

/// An IR version, written `"major.minor"` in JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IrVersion {
    pub major: u16,
    pub minor: u16,
}

impl IrVersion {
    /// The newest version this build reads and writes.
    pub const CURRENT: IrVersion = IrVersion { major: 2, minor: 0 };

    pub const fn new(major: u16, minor: u16) -> Self {
        IrVersion { major, minor }
    }

    /// Can a reader at version `self` read a document that needs `needed`?
    /// Same major version, and no newer minor version.
    pub fn can_read(self, needed: IrVersion) -> bool {
        needed.major == self.major && needed.minor <= self.minor
    }
}

impl fmt::Display for IrVersion {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl FromStr for IrVersion {
    type Err = String;

    /// Exactly `major.minor`: decimal digits, no sign, no leading zeros,
    /// so every version has one spelling (and one canonical form).
    fn from_str(text: &str) -> Result<Self, String> {
        let bad = || format!("{text:?} is not an IR version like \"2.0\"");
        let (major, minor) = text.split_once('.').ok_or_else(bad)?;
        let part = |p: &str| -> Result<u16, String> {
            let digits_only = !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
            let no_leading_zero = p == "0" || !p.starts_with('0');
            if digits_only && no_leading_zero {
                p.parse().map_err(|_| bad())
            } else {
                Err(bad())
            }
        };
        Ok(IrVersion::new(part(major)?, part(minor)?))
    }
}

impl Serialize for IrVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for IrVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(de::Error::custom)
    }
}

// ---- Scalars ---------------------------------------------------------------------

/// A constant in a definition: an identification value, a sentinel, a set
/// version, a condition operand. An integer is a JSON number within
/// ±(2^53 − 1) (validation enforces the range); anything else, including a
/// wider integer such as a `u64` sentinel, is a string that validation
/// interprets in context.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scalar {
    Int(i64),
    Str(String),
}

impl Serialize for Scalar {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Scalar::Int(n) => serializer.serialize_i64(*n),
            Scalar::Str(s) => serializer.serialize_str(s),
        }
    }
}

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ScalarVisitor)
    }
}

struct ScalarVisitor;

impl Visitor<'_> for ScalarVisitor {
    type Value = Scalar;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an integer or a string")
    }

    fn visit_i64<E>(self, n: i64) -> Result<Scalar, E> {
        Ok(Scalar::Int(n))
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<Scalar, E> {
        i64::try_from(n).map(Scalar::Int).map_err(|_| {
            E::custom(format!(
                "integer {n} is too large for a JSON number here; write it as the string \"{n}\""
            ))
        })
    }

    fn visit_f64<E: de::Error>(self, n: f64) -> Result<Scalar, E> {
        // An integral value arrives here when it's beyond the 64-bit range
        // (canonical JSON reads such integer text as a double).
        if n.fract() == 0.0 {
            Err(E::custom(format!(
                "integer {n} is outside the 64-bit range; write it as a string"
            )))
        } else {
            Err(E::custom(format!(
                "expected an integer or a string, got a number with a fraction ({n})"
            )))
        }
    }

    fn visit_str<E>(self, s: &str) -> Result<Scalar, E> {
        Ok(Scalar::Str(s.to_string()))
    }

    fn visit_string<E>(self, s: String) -> Result<Scalar, E> {
        Ok(Scalar::Str(s))
    }
}

impl From<i64> for Scalar {
    fn from(n: i64) -> Self {
        Scalar::Int(n)
    }
}

impl From<&str> for Scalar {
    fn from(s: &str) -> Self {
        Scalar::Str(s.to_string())
    }
}

/// A typed constant: a command argument's default. Validation checks it
/// against the argument's type. A number with no fraction reads as an
/// integer (`1.0` is `1`), as canonical JSON writes it; integers beyond
/// ±(2^53 − 1) and bytes (lowercase hex) are strings. Literals compare by
/// value: `Float(2.0)` equals `Int(2)`, as their canonical JSON does.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Literal {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl PartialEq for Literal {
    fn eq(&self, other: &Literal) -> bool {
        match (self, other) {
            (Literal::Bool(a), Literal::Bool(b)) => a == b,
            (Literal::Int(a), Literal::Int(b)) => a == b,
            (Literal::Float(a), Literal::Float(b)) => a == b,
            (Literal::Int(n), Literal::Float(x)) | (Literal::Float(x), Literal::Int(n)) => {
                // Exact both ways: `x` has no fraction and converts back to `n`.
                x.fract() == 0.0 && i64_from_integral(*x) == Some(*n)
            }
            (Literal::Str(a), Literal::Str(b)) => a == b,
            _ => false,
        }
    }
}

/// The i64 an integral double holds exactly, if it's in range.
fn i64_from_integral(x: f64) -> Option<i64> {
    // -2^63 is exactly representable; 2^63 is not an i64.
    ((-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&x)).then_some(x as i64)
}

impl Serialize for Literal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Literal::Bool(b) => serializer.serialize_bool(*b),
            Literal::Int(n) => serializer.serialize_i64(*n),
            Literal::Float(x) if x.is_finite() => serializer.serialize_f64(*x),
            Literal::Float(x) => Err(serde::ser::Error::custom(format!(
                "{x} isn't a finite number, which JSON can't hold"
            ))),
            Literal::Str(s) => serializer.serialize_str(s),
        }
    }
}

impl<'de> Deserialize<'de> for Literal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(LiteralVisitor)
    }
}

struct LiteralVisitor;

impl Visitor<'_> for LiteralVisitor {
    type Value = Literal;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a bool, a number or a string")
    }

    fn visit_bool<E>(self, b: bool) -> Result<Literal, E> {
        Ok(Literal::Bool(b))
    }

    fn visit_i64<E>(self, n: i64) -> Result<Literal, E> {
        Ok(Literal::Int(n))
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<Literal, E> {
        i64::try_from(n).map(Literal::Int).map_err(|_| {
            E::custom(format!(
                "integer {n} is too large for a JSON number here; write it as the string \"{n}\""
            ))
        })
    }

    fn visit_f64<E: de::Error>(self, x: f64) -> Result<Literal, E> {
        if x.fract() != 0.0 {
            return Ok(Literal::Float(x));
        }
        i64_from_integral(x).map(Literal::Int).ok_or_else(|| {
            E::custom(format!(
                "integer {x} is outside the 64-bit range; write it as a string"
            ))
        })
    }

    fn visit_str<E>(self, s: &str) -> Result<Literal, E> {
        Ok(Literal::Str(s.to_string()))
    }

    fn visit_string<E>(self, s: String) -> Result<Literal, E> {
        Ok(Literal::Str(s))
    }
}

// ---- Serde helpers: finite floats, null as empty ------------------------------------

/// Serialize-side checks that keep the "never null" rule: serde_json would
/// otherwise write NaN and infinity as `null`, producing a document that
/// either can't be read back or silently loses the value.
mod finite {
    use serde::{Serializer, ser::Error};

    fn check<E: Error>(n: f64) -> Result<f64, E> {
        if n.is_finite() {
            Ok(n)
        } else {
            Err(E::custom(format!(
                "{n} can't be written: MDL numbers must be finite"
            )))
        }
    }

    pub fn one<S: Serializer>(n: &f64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(check(*n)?)
    }

    pub fn option<S: Serializer>(n: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
        match n {
            Some(n) => s.serialize_some(&check::<S::Error>(*n)?),
            None => s.serialize_none(),
        }
    }

    pub fn list<S: Serializer>(ns: &[f64], s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(ns.len()))?;
        for n in ns {
            seq.serialize_element(&check::<S::Error>(*n)?)?;
        }
        seq.end()
    }
}

/// Reads `null` as an empty collection, so writers whose languages encode
/// an empty list as `null` (Go without `omitempty`) are read the same as
/// writers that omit it.
fn null_as_empty<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

// ---- Artifacts ---------------------------------------------------------------------

/// The SHA-256 of an artifact's canonical JSON: 64 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    /// The digest of canonical JSON text (see [`crate::canonical`]).
    pub fn of_canonical(canonical: &str) -> Digest {
        use sha2::Digest as _;
        let hash = Sha256::digest(canonical.as_bytes());
        Digest(hash.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Digest {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let hex = text.len() == 64
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if hex {
            Ok(Digest(text.to_string()))
        } else {
            Err(format!(
                "{text:?} is not a SHA-256 digest (64 lowercase hex digits)"
            ))
        }
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(de::Error::custom)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One artifact: a standalone, content-addressed document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "artifact", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Artifact {
    #[non_exhaustive]
    Catalog {
        mdl_version: IrVersion,
        catalog: Catalog,
    },
    #[non_exhaustive]
    Family {
        mdl_version: IrVersion,
        family: Family,
    },
    /// A set names the exact catalog and family it was validated against.
    /// Its own digest is the `def_version` consumers stamp on output.
    #[non_exhaustive]
    Set {
        mdl_version: IrVersion,
        catalog: Digest,
        family: Digest,
        set: Set,
    },
}

impl Artifact {
    pub fn catalog(mdl_version: IrVersion, catalog: Catalog) -> Self {
        Artifact::Catalog {
            mdl_version,
            catalog,
        }
    }

    pub fn family(mdl_version: IrVersion, family: Family) -> Self {
        Artifact::Family {
            mdl_version,
            family,
        }
    }

    pub fn set(mdl_version: IrVersion, catalog: Digest, family: Digest, set: Set) -> Self {
        Artifact::Set {
            mdl_version,
            catalog,
            family,
            set,
        }
    }

    /// The lowest IR version that can read this artifact.
    pub fn mdl_version(&self) -> IrVersion {
        match self {
            Artifact::Catalog { mdl_version, .. }
            | Artifact::Family { mdl_version, .. }
            | Artifact::Set { mdl_version, .. } => *mdl_version,
        }
    }
}

/// Everything a frontend produces in one document. `morphc build`
/// validates a bundle and writes its artifacts. Families and sets are lists
/// (names unique, checked by validation) so each name is written once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Bundle {
    pub mdl_version: IrVersion,
    pub catalog: Catalog,
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub families: Vec<Family>,
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub sets: Vec<Set>,
}

impl Bundle {
    pub fn new(catalog: Catalog) -> Self {
        Bundle {
            mdl_version: IrVersion::CURRENT,
            catalog,
            families: Vec::new(),
            sets: Vec::new(),
        }
    }
}

// ---- Catalog ---------------------------------------------------------------------

/// The canonical model: every name a set may map its fields to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Catalog {
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub params: BTreeMap<String, Param>,
    /// Canonical commands: what can be asked of an asset, whatever its
    /// vendor.
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub commands: BTreeMap<String, Command>,
}

/// One canonical command. Its name is its key in [`Catalog::commands`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Command {
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub arguments: Vec<Argument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Set when the command is retired; the text says what replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
}

impl Command {
    pub fn new(arguments: Vec<Argument>) -> Self {
        Command {
            arguments,
            description: None,
            deprecated: None,
        }
    }
}

/// One argument of a canonical command: a scalar of a param type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Argument {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ParamType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Enum arguments: the allowed states.
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub states: Vec<String>,
    /// Numeric arguments: the values a caller may give, inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ArgumentRange>,
    /// The value used when a caller leaves the argument out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Literal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Argument {
    pub fn new(name: impl Into<String>, ty: ParamType) -> Self {
        Argument {
            name: name.into(),
            ty,
            unit: None,
            states: Vec::new(),
            range: None,
            default: None,
            description: None,
        }
    }
}

/// The values a numeric argument may take, inclusive; either end may be
/// open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ArgumentRange {
    #[serde(
        default,
        serialize_with = "finite::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub min: Option<f64>,
    #[serde(
        default,
        serialize_with = "finite::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max: Option<f64>,
}

impl ArgumentRange {
    pub fn new(min: Option<f64>, max: Option<f64>) -> Self {
        ArgumentRange { min, max }
    }
}

/// One canonical parameter. Its name is its key in [`Catalog::params`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Param {
    #[serde(rename = "type")]
    pub ty: ParamType,
    /// A UCUM unit string; absent for unitless params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Enum params: the allowed states, in order. Required (non-empty) for
    /// enums, absent otherwise.
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub states: Vec<String>,
    /// Vec3 and quat params: the frame consumers receive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<Frame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Set when the name is retired; the text says what replaces it.
    /// Deprecated names stay valid forever (catalog names are a schema).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
}

impl Param {
    pub fn new(ty: ParamType) -> Self {
        Param {
            ty,
            unit: None,
            states: Vec::new(),
            frame: None,
            limits: None,
            description: None,
            deprecated: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ParamType {
    F64,
    I64,
    U64,
    Bool,
    Str,
    Bytes,
    Enum,
    Vec3,
    Quat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum Frame {
    Ecef,
    EciJ2000,
    Body,
    Ned,
    Enu,
    /// Latitude, longitude, altitude. Source side only: consumers convert it.
    Geodetic,
}

/// Default limits. Any of them may be omitted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Limits {
    #[serde(
        default,
        serialize_with = "finite::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub red_low: Option<f64>,
    #[serde(
        default,
        serialize_with = "finite::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub yellow_low: Option<f64>,
    #[serde(
        default,
        serialize_with = "finite::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub yellow_high: Option<f64>,
    #[serde(
        default,
        serialize_with = "finite::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub red_high: Option<f64>,
}

// ---- Families ---------------------------------------------------------------------

/// What every layout version of one vendor's data has in common: how its
/// payloads are encoded, and where the asset id and (optionally) the layout
/// version are, readable before the set is known.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Family {
    pub name: String,
    pub encoding: Encoding,
    /// Binary families only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_order: Option<ByteOrder>,
    /// Where the vendor's own id for the asset is.
    pub alias: Locator,
    /// Where the payload names its layout version, if it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Locator>,
    /// The metadata names the family's link provides (`apid`, `sysid`).
    /// Identify keys and metadata locators may only use these, so a typo is
    /// a build error, and a consumer can check its link provides them all.
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub metadata: Vec<String>,
}

impl Family {
    pub fn new(name: impl Into<String>, encoding: Encoding, alias: Locator) -> Self {
        Family {
            name: name.into(),
            encoding,
            byte_order: None,
            alias,
            version: None,
            metadata: Vec::new(),
        }
    }
}

/// How a payload is written, once its link has removed the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Encoding {
    /// Entries at bit positions.
    Binary,
    /// One JSON document; entries at JSON Pointers.
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteOrder {
    Big,
    Little,
}

/// Where a value is found before the set is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Locator {
    /// Bits in the payload, as an unsigned integer.
    #[non_exhaustive]
    Bits { offset_bits: u32, size_bits: u32 },
    /// A value the link's protocol stack provides (`sysid`, `apid`).
    #[non_exhaustive]
    Metadata { name: String },
    /// A JSON Pointer into the payload.
    #[non_exhaustive]
    Path { pointer: String },
    /// The same value for every message: a single-asset family.
    #[non_exhaustive]
    Fixed { value: String },
}

impl Locator {
    pub fn bits(offset_bits: u32, size_bits: u32) -> Self {
        Locator::Bits {
            offset_bits,
            size_bits,
        }
    }

    pub fn metadata(name: impl Into<String>) -> Self {
        Locator::Metadata { name: name.into() }
    }

    pub fn path(pointer: impl Into<String>) -> Self {
        Locator::Path {
            pointer: pointer.into(),
        }
    }

    pub fn fixed(value: impl Into<String>) -> Self {
        Locator::Fixed {
            value: value.into(),
        }
    }
}

// ---- Sets ---------------------------------------------------------------------

/// One layout version of one family.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Set {
    pub name: String,
    pub family: String,
    /// Matched against the family's version locator. Present exactly when
    /// the family has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Scalar>,
    /// Absent: messages carry ground time only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<Time>,
    /// Reusable struct types, by name.
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub types: BTreeMap<String, StructType>,
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub packets: Vec<Packet>,
    /// How this layout version encodes canonical commands.
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub commands: Vec<CommandPacket>,
}

impl Set {
    pub fn new(name: impl Into<String>, family: impl Into<String>) -> Self {
        Set {
            name: name.into(),
            family: family.into(),
            version: None,
            time: None,
            types: BTreeMap::new(),
            packets: Vec::new(),
            commands: Vec::new(),
        }
    }
}

/// How event time is found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Time {
    /// CCSDS unsegmented time code: seconds and fractions since `epoch`.
    #[non_exhaustive]
    Cuc {
        offset_bits: u32,
        coarse_bytes: u8,
        fine_bytes: u8,
        /// RFC 3339, e.g. `"1958-01-01T00:00:00Z"`, read on `scale`.
        epoch: String,
        scale: TimeScale,
    },
    /// An ISO 8601 / RFC 3339 string in a self-describing payload.
    #[non_exhaustive]
    Iso8601 { pointer: String },
    /// Time since boot, in each packet's `time_field`, turned into absolute
    /// time by a packet that carries both clocks. A packet without a
    /// `time_field` carries ground time only.
    #[non_exhaustive]
    Boot {
        unit: TimeUnit,
        correlate: Correlate,
    },
    /// A count of `unit`s since `epoch`, on `scale`, in each packet's
    /// `time_field` (an integer, or a float with a fraction). Unix time is
    /// seconds since 1970-01-01T00:00:00Z on the utc scale. A packet
    /// without a `time_field` carries ground time only.
    #[non_exhaustive]
    Elapsed {
        unit: TimeUnit,
        /// RFC 3339, read on `scale`.
        epoch: String,
        scale: TimeScale,
    },
    /// GPS time: the week in each timed packet's `week_field`, and the time
    /// of week, in `unit`s, in its `time_field`, from 1980-01-06T00:00:00 on
    /// the gps scale. A week of `week_bits` bits (10 in older receivers)
    /// rolls over; the full week is the one nearest the message's ground
    /// time.
    #[non_exhaustive]
    Gps {
        unit: TimeUnit,
        week_field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        week_bits: Option<u8>,
    },
}

/// How a time source counts seconds. `utc` counts every day as 86,400
/// seconds, as Unix time does (leap seconds aren't counted); `tai` counts
/// every second; `gps` is TAI 19 s behind and `tt` (Terrestrial Time)
/// 32.184 s ahead. Converting the others to UTC uses the leap-second table
/// in `spec/decoding.md` §8.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TimeScale {
    Utc,
    Tai,
    Gps,
    Tt,
}

impl Time {
    pub fn cuc(
        offset_bits: u32,
        coarse_bytes: u8,
        fine_bytes: u8,
        epoch: impl Into<String>,
        scale: TimeScale,
    ) -> Self {
        Time::Cuc {
            offset_bits,
            coarse_bytes,
            fine_bytes,
            epoch: epoch.into(),
            scale,
        }
    }

    pub fn elapsed(unit: TimeUnit, epoch: impl Into<String>, scale: TimeScale) -> Self {
        Time::Elapsed {
            unit,
            epoch: epoch.into(),
            scale,
        }
    }

    pub fn gps(unit: TimeUnit, week_field: impl Into<String>) -> Self {
        Time::Gps {
            unit,
            week_field: week_field.into(),
            week_bits: None,
        }
    }

    pub fn iso8601(pointer: impl Into<String>) -> Self {
        Time::Iso8601 {
            pointer: pointer.into(),
        }
    }

    pub fn boot(unit: TimeUnit, correlate: Correlate) -> Self {
        Time::Boot { unit, correlate }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TimeUnit {
    S,
    Ms,
    Us,
    Ns,
}

/// The packet that carries both clocks, and its two entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Correlate {
    pub packet: String,
    /// The entry holding absolute time: a count of `clock_unit`s since
    /// `epoch` on `scale`, as an `elapsed` time source reads it.
    pub clock_field: String,
    pub clock_unit: TimeUnit,
    /// RFC 3339, read on `scale`; Unix time is `1970-01-01T00:00:00Z` on
    /// `utc`.
    pub epoch: String,
    pub scale: TimeScale,
    /// The entry holding time since boot, in the set's boot unit.
    pub boot_field: String,
}

impl Correlate {
    /// A correlation whose absolute clock is Unix time in `clock_unit`s.
    pub fn new(
        packet: impl Into<String>,
        clock_field: impl Into<String>,
        clock_unit: TimeUnit,
        boot_field: impl Into<String>,
    ) -> Self {
        Correlate {
            packet: packet.into(),
            clock_field: clock_field.into(),
            clock_unit,
            epoch: "1970-01-01T00:00:00Z".to_string(),
            scale: TimeScale::Utc,
            boot_field: boot_field.into(),
        }
    }

    /// The absolute clock's epoch and scale.
    pub fn with_epoch(mut self, epoch: impl Into<String>, scale: TimeScale) -> Self {
        self.epoch = epoch.into();
        self.scale = scale;
        self
    }
}

// ---- Packets and layout -------------------------------------------------------

/// How one canonical command is encoded: a payload layout whose fields get
/// their values from the command's arguments, from constants, or from the
/// data (a count or length another entry takes from them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CommandPacket {
    pub name: String,
    /// The catalog command this encodes.
    pub canonical: String,
    /// Values the link frames the command with (`apid`), by names the
    /// family declares.
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub metadata: BTreeMap<String, Scalar>,
    /// The payload; absent for a command the link frames with no payload.
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub entries: Vec<Entry>,
}

impl CommandPacket {
    pub fn new(name: impl Into<String>, canonical: impl Into<String>, entries: Vec<Entry>) -> Self {
        CommandPacket {
            name: name.into(),
            canonical: canonical.into(),
            metadata: BTreeMap::new(),
            entries,
        }
    }
}

/// One message layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Packet {
    pub name: String,
    /// All must match. Keys are link metadata names, the packet's own
    /// top-level integer fields, or JSON Pointers.
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub identify: BTreeMap<String, Scalar>,
    /// Binary payloads: how long the payload must be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<Length>,
    /// `boot`, `elapsed` and `gps` time: the entry holding the packet's
    /// clock (time since boot, the count since the epoch, the time of
    /// week).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_field: Option<String>,
    /// May be empty: a packet whose arrival is the information (a
    /// heartbeat) has no entries.
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub entries: Vec<Entry>,
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub composites: Vec<Composite>,
}

impl Packet {
    pub fn new(name: impl Into<String>) -> Self {
        Packet {
            name: name.into(),
            identify: BTreeMap::new(),
            length: None,
            time_field: None,
            entries: Vec::new(),
            composites: Vec::new(),
        }
    }
}

/// A binary payload's length. `exact` means every entry fits within that
/// many bytes in every message; use `at_least` for layouts whose size
/// varies beyond it. Longer payloads are accepted either way (a vendor
/// appending fields mustn't break readers); shorter ones are an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Length {
    #[non_exhaustive]
    Exact { bytes: u32 },
    #[non_exhaustive]
    AtLeast { bytes: u32 },
}

impl Length {
    pub fn exact(bytes: u32) -> Self {
        Length::Exact { bytes }
    }

    pub fn at_least(bytes: u32) -> Self {
        Length::AtLeast { bytes }
    }
}

/// A reusable struct type. `entries` is required and non-empty: unlike a
/// packet, an empty struct carries nothing, not even arrival.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StructType {
    pub entries: Vec<Entry>,
}

impl StructType {
    pub fn new(entries: Vec<Entry>) -> Self {
        StructType { entries }
    }
}

/// One item in a packet or struct type.
///
/// Binary position: an entry with `offset_bits` sits at that bit from the
/// start of its container; one without starts where the previous entry
/// ended. Bits are numbered from the most significant bit of the first
/// byte, in both byte orders: bit 0 is the top bit of byte 0, bit 8 the top
/// bit of byte 1. The byte order decides how the bytes of a multi-byte
/// value combine. Self-describing payloads use `pointer`, relative to the
/// container.
///
/// Rules a decoder follows:
///
/// - An absent conditional entry ends where it would have started, so an
///   absent entry with `offset_bits` still moves the position of the next
///   sequential entry to that offset.
/// - A struct's size in a message, for the entries after it, is the
///   furthest end of its entries present in that message.
/// - Conditions, counts, lengths and identify values use raw values, before
///   any conversion. A count or length taken from a field is
///   `raw * multiply + add` ([`FromField`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Entry {
    Field(Field),
    Array(ArrayEntry),
    Struct(StructEntry),
    /// Skipped bits (binary only).
    Padding(Padding),
    /// A checksum over earlier bytes of the packet (binary only).
    Checksum(ChecksumEntry),
}

/// One scalar value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Field {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ScalarType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_bits: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// Binary numbers and booleans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bits: Option<u32>,
    /// Binary strings and bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<ByteLength>,
    /// Strings: how the bytes are text. Defaults to UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversion: Option<Conversion>,
    /// The unit after conversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// The catalog param this field maps to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    /// Raw values that mean "no reading".
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub invalid: Vec<Scalar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Condition>,
    /// Self-describing payloads only: `Some(true)` makes the entry absent
    /// when its pointer finds nothing or `null`; without it, that rejects
    /// the message. `Some(false)` spells out the default, which validation
    /// rejects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
    /// Command packets: the raw value this field always has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Scalar>,
    /// Command packets: the command argument this field encodes, through
    /// the inverse of `conversion`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument: Option<String>,
    /// Command packets: engineering value to raw value, for an argument
    /// whose `conversion` is a polynomial (linear and states conversions
    /// invert exactly).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverse: Option<Conversion>,
    /// Command packets: `Some(true)` writes the native id of the asset the
    /// command is sent to, for links that carry it in the payload
    /// (MAVLink's `target_system`). `Some(false)` spells out the default,
    /// which validation rejects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<bool>,
}

impl Field {
    pub fn new(name: impl Into<String>, ty: ScalarType) -> Self {
        Field {
            name: name.into(),
            ty,
            offset_bits: None,
            pointer: None,
            size_bits: None,
            length: None,
            text: None,
            conversion: None,
            unit: None,
            canonical: None,
            invalid: Vec::new(),
            when: None,
            optional: None,
            value: None,
            argument: None,
            inverse: None,
            target: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ScalarType {
    Uint,
    Int,
    /// IEEE 754, 32 or 64 bits.
    Float,
    /// One bit.
    Bool,
    String,
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TextEncoding {
    Utf8,
    Ascii,
    Latin1,
}

/// How many bytes a binary string or bytes value has.
///
/// `Remaining {}` is an empty *struct* variant on purpose: serde reads a
/// unit variant of an internally tagged enum while ignoring every other
/// key, so `{"kind": "remaining", "byte": 0}` would be silently accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum ByteLength {
    #[non_exhaustive]
    Fixed { bytes: u32 },
    /// An earlier unsigned integer entry holds the length in bytes (after
    /// its adjustment).
    Field(FromField),
    /// Up to (not including) this byte, which is consumed.
    #[non_exhaustive]
    Terminator { byte: u8 },
    /// Everything to the end of the container.
    #[non_exhaustive]
    Remaining {},
}

impl ByteLength {
    pub fn fixed(bytes: u32) -> Self {
        ByteLength::Fixed { bytes }
    }

    pub fn field(field: impl Into<String>) -> Self {
        ByteLength::Field(FromField::new(field))
    }

    pub fn terminator(byte: u8) -> Self {
        ByteLength::Terminator { byte }
    }

    pub fn remaining() -> Self {
        ByteLength::Remaining {}
    }
}

/// A repeated value: scalars or structs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ArrayEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_bits: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// Binary arrays: how many elements. Self-describing arrays take their
    /// length from the document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<Count>,
    pub element: Element,
    /// Numeric scalar elements: the catalog param each element maps to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    /// With `canonical`: element *i* is a sample at packet time + *i* ×
    /// this interval (a high-rate block). Without it, every element is a
    /// sample at packet time, with its index recorded.
    #[serde(
        default,
        serialize_with = "finite::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub sample_interval_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Condition>,
    /// Self-describing payloads only: `Some(true)` makes the entry absent
    /// when its pointer finds nothing or `null`; without it, that rejects
    /// the message. `Some(false)` spells out the default, which validation
    /// rejects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
}

impl ArrayEntry {
    pub fn new(name: impl Into<String>, element: Element) -> Self {
        ArrayEntry {
            name: name.into(),
            offset_bits: None,
            pointer: None,
            count: None,
            element,
            canonical: None,
            sample_interval_ms: None,
            when: None,
            optional: None,
        }
    }
}

/// How many elements a binary array has. `Remaining {}` is an empty struct
/// variant for the reason given on [`ByteLength`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Count {
    #[non_exhaustive]
    Fixed { count: u32 },
    /// An earlier unsigned integer entry holds the count (after its
    /// adjustment).
    Field(FromField),
    /// As many elements as fit before the container ends.
    #[non_exhaustive]
    Remaining {},
}

impl Count {
    pub fn fixed(count: u32) -> Self {
        Count::Fixed { count }
    }

    pub fn field(field: impl Into<String>) -> Self {
        Count::Field(FromField::new(field))
    }

    pub fn remaining() -> Self {
        Count::Remaining {}
    }
}

/// A count or length read from an earlier field, as `raw * multiply + add`.
/// The adjustment covers encodings such as CCSDS's packet length ("octets
/// minus one": `add: 1`) or lengths in 16-bit words (`multiply: 2`). A
/// result below zero means the message is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FromField {
    pub field: String,
    /// Defaults to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiply: Option<u32>,
    /// Defaults to 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add: Option<i64>,
}

impl FromField {
    pub fn new(field: impl Into<String>) -> Self {
        FromField {
            field: field.into(),
            multiply: None,
            add: None,
        }
    }

    /// The largest value this can produce when the field holds at most
    /// `max_raw`, or `None` if it would overflow.
    pub fn max_value(&self, max_raw: u64) -> Option<u64> {
        let value = i128::from(max_raw) * i128::from(self.multiply.unwrap_or(1))
            + i128::from(self.add.unwrap_or(0));
        u64::try_from(value.max(0)).ok()
    }
}

/// What an array holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Element {
    Scalar(ScalarElement),
    /// A struct type from [`Set::types`], by name.
    #[non_exhaustive]
    Struct {
        name: String,
    },
}

impl Element {
    pub fn struct_type(name: impl Into<String>) -> Self {
        Element::Struct { name: name.into() }
    }
}

/// A scalar array element: a field without a name or position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ScalarElement {
    #[serde(rename = "type")]
    pub ty: ScalarType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bits: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<ByteLength>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversion: Option<Conversion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(
        default,
        deserialize_with = "null_as_empty",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub invalid: Vec<Scalar>,
}

impl ScalarElement {
    pub fn new(ty: ScalarType) -> Self {
        ScalarElement {
            ty,
            size_bits: None,
            length: None,
            text: None,
            conversion: None,
            unit: None,
            invalid: Vec::new(),
        }
    }
}

/// A nested struct, of a type from [`Set::types`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StructEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_bits: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// The struct type's name.
    #[serde(rename = "struct")]
    pub struct_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Condition>,
    /// Self-describing payloads only: `Some(true)` makes the entry absent
    /// when its pointer finds nothing or `null`; without it, that rejects
    /// the message. `Some(false)` spells out the default, which validation
    /// rejects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
}

impl StructEntry {
    pub fn new(name: impl Into<String>, struct_type: impl Into<String>) -> Self {
        StructEntry {
            name: name.into(),
            offset_bits: None,
            pointer: None,
            struct_type: struct_type.into(),
            when: None,
            optional: None,
        }
    }
}

/// A checksum over the bytes of a packet from `from_bits` (a byte
/// boundary, default 0) up to the checksum's own start, stored as an
/// unsigned integer of the algorithm's width in the family's byte order
/// (`iso8473` fixes its own). Decoding verifies it; encoding writes it.
/// Only at a packet's top level, and always present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ChecksumEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_bits: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_bits: Option<u32>,
    pub algorithm: Checksum,
}

impl ChecksumEntry {
    pub fn new(name: impl Into<String>, algorithm: Checksum) -> Self {
        ChecksumEntry {
            name: name.into(),
            offset_bits: None,
            from_bits: None,
            algorithm,
        }
    }
}

/// How a checksum is computed over bytes.
///
/// `Crc` is the parametric model of the CRC catalogue (Rocksoft): every
/// parameter is required, so each CRC has one spelling; values past
/// ±(2^53 − 1), such as a CRC-64 polynomial, are decimal strings.
/// [`crate::v2::checksum::preset`] gives the parameters of named CRCs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Checksum {
    /// A cyclic redundancy check, 8 to 64 bits in whole bytes.
    #[non_exhaustive]
    Crc {
        width: u8,
        poly: Scalar,
        init: Scalar,
        reflect_in: bool,
        reflect_out: bool,
        xor_out: Scalar,
    },
    /// The bytes added, modulo 2^width: 8 to 64 bits in whole bytes.
    #[non_exhaustive]
    Sum { width: u8 },
    /// The bytes exclusive-ored together: 8 bits.
    #[non_exhaustive]
    Xor {},
    /// The ISO checksum of ISO 8473-1 (Fletcher's arithmetic checksum,
    /// modulo 255) that ECSS PUS uses for packet error control: two check
    /// bytes, stored first then second whatever the family's byte order.
    #[non_exhaustive]
    Iso8473 {},
}

impl Checksum {
    pub fn crc(
        width: u8,
        poly: impl Into<Scalar>,
        init: impl Into<Scalar>,
        reflect_in: bool,
        reflect_out: bool,
        xor_out: impl Into<Scalar>,
    ) -> Self {
        Checksum::Crc {
            width,
            poly: poly.into(),
            init: init.into(),
            reflect_in,
            reflect_out,
            xor_out: xor_out.into(),
        }
    }

    pub fn sum(width: u8) -> Self {
        Checksum::Sum { width }
    }

    pub fn xor() -> Self {
        Checksum::Xor {}
    }

    pub fn iso8473() -> Self {
        Checksum::Iso8473 {}
    }

    /// Bits the checksum's value takes in the packet.
    pub fn width_bits(&self) -> u32 {
        match self {
            Checksum::Crc { width, .. } | Checksum::Sum { width } => u32::from(*width),
            Checksum::Xor {} => 8,
            Checksum::Iso8473 {} => 16,
        }
    }
}

/// Skipped bits in a binary layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Padding {
    pub size_bits: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_bits: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Condition>,
}

impl Padding {
    pub fn new(size_bits: u32) -> Self {
        Padding {
            size_bits,
            offset_bits: None,
            when: None,
        }
    }
}

/// When an entry is present. Conditions compare entries decoded before the
/// one they guard, so they can always be evaluated in one pass. The lists in
/// `In`, `All` and `Any` are required and non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Condition {
    #[non_exhaustive]
    Compare {
        field: String,
        op: CompareOp,
        value: Scalar,
    },
    #[non_exhaustive]
    In { field: String, values: Vec<Scalar> },
    #[non_exhaustive]
    All { conditions: Vec<Condition> },
    #[non_exhaustive]
    Any { conditions: Vec<Condition> },
    #[non_exhaustive]
    Not { condition: Box<Condition> },
}

impl Condition {
    pub fn compare(field: impl Into<String>, op: CompareOp, value: impl Into<Scalar>) -> Self {
        Condition::Compare {
            field: field.into(),
            op,
            value: value.into(),
        }
    }

    pub fn is_in(field: impl Into<String>, values: Vec<Scalar>) -> Self {
        Condition::In {
            field: field.into(),
            values,
        }
    }

    pub fn all(conditions: Vec<Condition>) -> Self {
        Condition::All { conditions }
    }

    pub fn any(conditions: Vec<Condition>) -> Self {
        Condition::Any { conditions }
    }

    pub fn negate(condition: Condition) -> Self {
        Condition::Not {
            condition: Box::new(condition),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `field & value != 0`
    MaskAny,
    /// `field & value == value`
    MaskAll,
}

/// Raw value to engineering value. `coefficients`, `states` and `points`
/// are required and non-empty (a table or spline has at least two points).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Conversion {
    /// `raw * scale + offset`
    #[non_exhaustive]
    Linear {
        #[serde(serialize_with = "finite::one")]
        scale: f64,
        #[serde(serialize_with = "finite::one")]
        offset: f64,
    },
    /// `c0 + c1*x + c2*x^2 + ...`
    #[non_exhaustive]
    Poly {
        #[serde(serialize_with = "finite::list")]
        coefficients: Vec<f64>,
    },
    /// Integer to state name.
    #[non_exhaustive]
    States { states: Vec<State> },
    /// Through points, in increasing raw order: straight lines between
    /// them, or with `step` interpolation each point's value until the
    /// next. A raw value outside the points has no engineering value unless
    /// `extrapolate` is true, which extends the end segments (holds the end
    /// values, for steps).
    #[non_exhaustive]
    Table {
        points: Vec<Point>,
        /// Defaults to `linear`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interpolation: Option<Interpolation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extrapolate: Option<bool>,
    },
    /// The natural cubic spline through points, in increasing raw order.
    /// Outside them, as for a table: no value unless `extrapolate`, which
    /// continues the spline's end slopes in straight lines.
    #[non_exhaustive]
    Spline {
        points: Vec<Point>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extrapolate: Option<bool>,
    },
}

/// A raw value and the engineering value it stands for.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Point {
    #[serde(serialize_with = "finite::one")]
    pub raw: f64,
    #[serde(serialize_with = "finite::one")]
    pub value: f64,
}

impl Point {
    pub fn new(raw: f64, value: f64) -> Self {
        Point { raw, value }
    }
}

/// How a table goes from one point to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Interpolation {
    Linear,
    Step,
}

impl Conversion {
    pub fn linear(scale: f64, offset: f64) -> Self {
        Conversion::Linear { scale, offset }
    }

    pub fn poly(coefficients: Vec<f64>) -> Self {
        Conversion::Poly { coefficients }
    }

    pub fn states(states: Vec<State>) -> Self {
        Conversion::States { states }
    }

    pub fn table(points: Vec<Point>, interpolation: Interpolation) -> Self {
        Conversion::Table {
            points,
            interpolation: (interpolation != Interpolation::Linear).then_some(interpolation),
            extrapolate: None,
        }
    }

    pub fn spline(points: Vec<Point>) -> Self {
        Conversion::Spline {
            points,
            extrapolate: None,
        }
    }
}

/// One enum state. A list of these rather than a map, so the raw values
/// stay JSON numbers (object keys can only be strings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct State {
    pub raw: i64,
    pub name: String,
}

impl State {
    pub fn new(raw: i64, name: impl Into<String>) -> Self {
        State {
            raw,
            name: name.into(),
        }
    }
}

/// Several numeric entries that become one vec3 or quat sample. Members are
/// entry paths (`pos.x`), in x, y, z (or w, x, y, z) order, required and
/// non-empty. `frame` is the source's frame; it must be one consumers can
/// convert to the catalog param's frame (validation checks).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Composite {
    pub canonical: String,
    pub frame: Frame,
    pub members: Vec<String>,
}

impl Composite {
    pub fn new(canonical: impl Into<String>, frame: Frame, members: Vec<String>) -> Self {
        Composite {
            canonical: canonical.into(),
            frame,
            members,
        }
    }
}

#[cfg(test)]
mod tests;
