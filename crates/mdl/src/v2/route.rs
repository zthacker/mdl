//! Routing: which asset sent a message, and which set decodes it.
//!
//! [`FamilySets`] holds the sets of one family, loaded from artifacts and
//! checked together. For each message, [`FamilySets::route`]:
//!
//! 1. checks that the payload is in the family's encoding;
//! 2. reads the alias locator to get the native id;
//! 3. resolves the asset at the message's ground time;
//! 4. selects the set: by the version locator if the family has one,
//!    otherwise by the asset's binding.
//!
//! Routing reads only what the locators need. A JSON payload is parsed
//! only when a locator is a JSON Pointer, and a payload whose other parts
//! can't be decoded still routes; the decoder reports those. Each failure
//! is its own [`RouteError`], with a stable code for counting rejected
//! messages.
//!
//! # Rules
//!
//! These are the rules every implementation follows when routing.
//!
//! - **Order.** The checks run in this order, and the first failure is the
//!   error: the payload's encoding (`wrong_encoding`); the alias
//!   (`invalid_json`, `unreadable_locator`); the asset (`unknown_asset`);
//!   for a family with a version locator, a binding that names a set
//!   (`unexpected_set`), then the version (`invalid_json`,
//!   `unreadable_locator`, `unknown_version`); for a family without one, a
//!   binding that names no set (`missing_set`); then whether the set is
//!   loaded (`unknown_set`).
//! - **What a locator reads.** Bits read an unsigned integer. Metadata
//!   reads the link's value; a name given more than once is unreadable. A
//!   fixed locator reads its text. A JSON Pointer reads a string, or a
//!   number whose value is an integer that can be read exactly: integer
//!   literals within 64 bits, and doubles with no fraction within
//!   ±(2^53 - 1), so `42`, `42.0` and `4.2e1` are all 42. Other numbers,
//!   such as `1e20`, `1.5`, or an integer literal beyond 64 bits, and other
//!   JSON types, are unreadable.
//! - **Comparing.** Native ids compare by value: text in an integer's one
//!   decimal spelling is that integer ([`NativeId`]). Versions compare the
//!   way validation compares set versions: read by a JSON Pointer, exactly
//!   (`"2"` is not `2`); read from bits or metadata, by value (`"2"` is
//!   `2`, `"02"` is text).
//! - **JSON payloads** are parsed only when a JSON Pointer locator reads
//!   them, and refused (`invalid_json`) if they aren't valid JSON or have
//!   a duplicate key. A payload no locator reads isn't checked here; the
//!   decoder reports it.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;

use crate::canonical::{CanonicalError, MAX_SAFE_INTEGER, parse_without_duplicates};
use crate::v2::artifact::{AssembleError, assemble};
use crate::v2::bindings::{Binding, NativeId, Resolver};
use crate::v2::bits::{BitsError, read_bits};
use crate::v2::decode::Program;
use crate::v2::ir::{
    Artifact, Bundle, ByteOrder, Catalog, Digest, Encoding, Family, Locator, Scalar, Set,
};
use crate::v2::message::{Message, MetaValue, Payload};
use crate::v2::read::Loaded;
use crate::v2::time::Timestamp;
use crate::v2::validate::validate;

/// The sets of one family, ready to route its messages.
#[derive(Debug, Clone)]
pub struct FamilySets {
    family: Family,
    kind: Kind,
    sets: BTreeMap<String, LoadedSet>,
    by_version: BTreeMap<VersionKey, String>,
}

/// A set ready to decode: its definition, its digest and what decoding
/// precomputes from it.
#[derive(Debug, Clone)]
pub(super) struct LoadedSet {
    pub(super) set: Set,
    pub(super) def_version: Digest,
    /// The catalog the set pins, shared by sets that pin the same one.
    pub(super) catalog: Arc<Catalog>,
    pub(super) program: Program,
}

/// How a family's payloads are read.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Binary(ByteOrder),
    Json,
}

impl Kind {
    fn encoding(self) -> &'static str {
        match self {
            Kind::Binary(_) => "binary",
            Kind::Json => "json",
        }
    }
}

/// Where a message was sent from and which set decodes it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Route<'a> {
    /// What the family's alias locator read.
    pub native_id: NativeId,
    /// The asset's binding at the message's ground time.
    pub binding: Arc<Binding>,
    pub set: &'a Set,
    /// The set artifact's digest, to stamp on what's decoded.
    pub def_version: &'a Digest,
}

/// Why a message can't be routed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RouteError {
    #[error("family {family} is {expected}, but the message has a {given} payload")]
    WrongEncoding {
        family: String,
        expected: &'static str,
        given: &'static str,
    },
    #[error("family {family}'s payload isn't JSON MDL can read: {problem}")]
    InvalidJson {
        family: String,
        problem: CanonicalError,
    },
    /// A locator found nothing usable: missing metadata, a payload too
    /// short for its bits, nothing at its JSON Pointer.
    #[error("family {family}'s {locator} locator can't be read: {reason}")]
    UnreadableLocator {
        family: String,
        locator: &'static str,
        reason: String,
    },
    #[error(
        "native id {native_id} of family {family} is no asset at {at}; bind it in the asset \
         registry"
    )]
    UnknownAsset {
        family: String,
        native_id: NativeId,
        at: Timestamp,
    },
    #[error("family {family} has no set with version {version}")]
    UnknownVersion { family: String, version: String },
    #[error("asset {asset} is bound to set {set}, which isn't among family {family}'s loaded sets")]
    UnknownSet {
        family: String,
        asset: String,
        set: String,
    },
    #[error(
        "asset {asset}'s binding names no set, and family {family} has no version locator to \
         choose one; name the set in the binding"
    )]
    MissingSet { family: String, asset: String },
    #[error(
        "asset {asset}'s binding names set {set}, but family {family} chooses sets by each \
         message's version; remove the set from the binding"
    )]
    UnexpectedSet {
        family: String,
        asset: String,
        set: String,
    },
}

impl RouteError {
    /// A stable code for the error, for counting and dead-letter queues.
    pub fn code(&self) -> &'static str {
        match self {
            RouteError::WrongEncoding { .. } => "wrong_encoding",
            RouteError::InvalidJson { .. } => "invalid_json",
            RouteError::UnreadableLocator { .. } => "unreadable_locator",
            RouteError::UnknownAsset { .. } => "unknown_asset",
            RouteError::UnknownVersion { .. } => "unknown_version",
            RouteError::UnknownSet { .. } => "unknown_set",
            RouteError::MissingSet { .. } => "missing_set",
            RouteError::UnexpectedSet { .. } => "unexpected_set",
        }
    }
}

impl FamilySets {
    /// A family and its sets, read from storage. Each set is assembled
    /// with its family and the catalog it pins (found in `catalogs` by
    /// digest), so every set is validated again; then the sets are checked
    /// together, since they may come from different builds: names and
    /// versions are unique.
    pub fn new(
        family: &Loaded<Artifact>,
        sets: &[Loaded<Artifact>],
        catalogs: &[Loaded<Artifact>],
    ) -> Result<FamilySets, AssembleError> {
        let Artifact::Family { family: body, .. } = &family.value else {
            return Err(AssembleError::WrongKind { expected: "family" });
        };
        // The family on its own first, so its problems show with no sets.
        let mut alone = Bundle::new(Catalog::default());
        alone.families.push(body.clone());
        validate(alone).map_err(AssembleError::Invalid)?;
        let kind = match body.encoding {
            // Validation requires a byte order for binary families.
            Encoding::Binary => Kind::Binary(body.byte_order.unwrap_or(ByteOrder::Big)),
            Encoding::Json => Kind::Json,
        };

        let mut loaded = BTreeMap::new();
        let mut by_version: BTreeMap<VersionKey, String> = BTreeMap::new();
        let mut shared: BTreeMap<Digest, Arc<Catalog>> = BTreeMap::new();
        for set in sets {
            let Artifact::Set {
                catalog: pinned,
                set: set_body,
                ..
            } = &set.value
            else {
                return Err(AssembleError::WrongKind { expected: "set" });
            };
            if loaded.contains_key(&set_body.name) {
                return Err(AssembleError::Duplicate {
                    what: "set",
                    name: set_body.name.clone(),
                });
            }
            let Some(catalog) = catalogs.iter().find(|c| &c.digest == pinned) else {
                return Err(AssembleError::Missing {
                    set: set_body.name.clone(),
                    what: "catalog",
                    pinned: pinned.clone(),
                });
            };
            assemble(set, family, catalog)?;
            let Artifact::Catalog {
                catalog: catalog_body,
                ..
            } = &catalog.value
            else {
                return Err(AssembleError::WrongKind {
                    expected: "catalog",
                });
            };
            let pinned_catalog = Arc::clone(
                shared
                    .entry(catalog.digest.clone())
                    .or_insert_with(|| Arc::new(catalog_body.clone())),
            );
            if let (Some(locator), Some(version)) = (&body.version, &set_body.version) {
                let key = VersionKey::of_set(version, is_exact(locator));
                if let Some(first) = by_version.get(&key) {
                    return Err(AssembleError::DuplicateVersion {
                        family: body.name.clone(),
                        version: key.to_string(),
                        first: first.clone(),
                        second: set_body.name.clone(),
                    });
                }
                by_version.insert(key, set_body.name.clone());
            }
            loaded.insert(
                set_body.name.clone(),
                LoadedSet {
                    set: set_body.clone(),
                    def_version: set.digest.clone(),
                    catalog: pinned_catalog,
                    program: Program::compile(set_body, body),
                },
            );
        }
        Ok(FamilySets {
            family: body.clone(),
            kind,
            sets: loaded,
            by_version,
        })
    }

    pub fn family(&self) -> &Family {
        &self.family
    }

    /// Which asset sent `message` and which set decodes it.
    pub fn route<R: Resolver + ?Sized>(
        &self,
        message: &Message<'_>,
        resolver: &R,
    ) -> Result<Route<'_>, RouteError> {
        let readable = self.readable(message)?;
        self.route_parts(message, &readable, resolver)
            .map(|(route, _)| route)
    }

    /// The payload, checked against the family's encoding.
    pub(super) fn readable<'m>(&self, message: &Message<'m>) -> Result<Readable<'m>, RouteError> {
        match (self.kind, message.payload) {
            (Kind::Binary(order), Payload::Binary(bytes)) => Ok(Readable::Binary { bytes, order }),
            (Kind::Json, Payload::Json(bytes)) => Ok(Readable::Json {
                bytes,
                parsed: OnceCell::new(),
            }),
            (kind, payload) => Err(RouteError::WrongEncoding {
                family: self.family.name.clone(),
                expected: kind.encoding(),
                given: payload.encoding(),
            }),
        }
    }

    /// The route and the set it chose.
    pub(super) fn route_parts<R: Resolver + ?Sized>(
        &self,
        message: &Message<'_>,
        readable: &Readable<'_>,
        resolver: &R,
    ) -> Result<(Route<'_>, &LoadedSet), RouteError> {
        let family = &self.family.name;
        let native_id = self
            .read(&self.family.alias, "alias", readable, message)?
            .native_id();
        let Some(binding) = resolver.resolve(family, &native_id, message.ground_time) else {
            return Err(RouteError::UnknownAsset {
                family: family.clone(),
                native_id,
                at: message.ground_time,
            });
        };

        let set_name = match (&self.family.version, &binding.set) {
            (Some(_), Some(set)) => {
                return Err(RouteError::UnexpectedSet {
                    family: family.clone(),
                    asset: binding.asset.clone(),
                    set: set.clone(),
                });
            }
            (Some(locator), None) => {
                let read = self.read(locator, "version", readable, message)?;
                let key = read.version_key(is_exact(locator));
                match self.by_version.get(&key) {
                    Some(name) => name,
                    None => {
                        return Err(RouteError::UnknownVersion {
                            family: family.clone(),
                            version: key.to_string(),
                        });
                    }
                }
            }
            (None, Some(set)) => set,
            (None, None) => {
                return Err(RouteError::MissingSet {
                    family: family.clone(),
                    asset: binding.asset.clone(),
                });
            }
        };
        let Some(loaded) = self.sets.get(set_name) else {
            return Err(RouteError::UnknownSet {
                family: family.clone(),
                asset: binding.asset.clone(),
                set: set_name.clone(),
            });
        };
        let route = Route {
            native_id,
            binding,
            set: &loaded.set,
            def_version: &loaded.def_version,
        };
        Ok((route, loaded))
    }

    fn read(
        &self,
        locator: &Locator,
        which: &'static str,
        readable: &Readable<'_>,
        message: &Message<'_>,
    ) -> Result<Located, RouteError> {
        let unreadable = |reason: String| RouteError::UnreadableLocator {
            family: self.family.name.clone(),
            locator: which,
            reason,
        };
        match (locator, readable) {
            (Locator::Fixed { value }, _) => Ok(Located::Text(value.clone())),
            (Locator::Metadata { name }, _) => read_metadata(message, name).map_err(unreadable),
            (
                Locator::Bits {
                    offset_bits,
                    size_bits,
                },
                Readable::Binary { bytes, order },
            ) => read_bits(bytes, u64::from(*offset_bits), *size_bits, *order)
                .map(Located::Uint)
                .map_err(|err| unreadable(bits_problem(err))),
            (Locator::Path { pointer }, Readable::Json { .. }) => {
                let Some(document) = readable.json(&self.family.name)? else {
                    return Err(unreadable(
                        "a JSON Pointer into a binary payload".to_string(),
                    ));
                };
                let value = document
                    .pointer(pointer)
                    .ok_or_else(|| unreadable(format!("nothing at {pointer}")))?;
                read_json(value).map_err(unreadable)
            }
            // Validation keeps bits locators in binary families and JSON
            // Pointers in JSON ones.
            (Locator::Bits { .. }, Readable::Json { .. }) => {
                Err(unreadable("bits in a JSON payload".to_string()))
            }
            (Locator::Path { .. }, Readable::Binary { .. }) => Err(unreadable(
                "a JSON Pointer into a binary payload".to_string(),
            )),
        }
    }
}

/// A payload checked against its family's encoding. JSON is parsed on
/// first use, once, whether routing or decoding uses it first.
pub(super) enum Readable<'m> {
    Binary {
        bytes: &'m [u8],
        order: ByteOrder,
    },
    Json {
        bytes: &'m [u8],
        parsed: OnceCell<Result<Value, CanonicalError>>,
    },
}

impl Readable<'_> {
    /// The parsed JSON document; `None` for a binary payload.
    pub(super) fn json(&self, family: &str) -> Result<Option<&Value>, RouteError> {
        let Readable::Json { bytes, parsed } = self else {
            return Ok(None);
        };
        parsed
            .get_or_init(|| parse_without_duplicates(bytes))
            .as_ref()
            .map(Some)
            .map_err(|problem| RouteError::InvalidJson {
                family: family.to_string(),
                problem: problem.clone(),
            })
    }
}

/// What a locator read, before it's compared.
enum Located {
    Uint(u64),
    Int(i64),
    Text(String),
}

impl Located {
    fn native_id(&self) -> NativeId {
        match self {
            Located::Uint(n) => NativeId::from(*n),
            Located::Int(n) => NativeId::from(*n),
            Located::Text(text) => NativeId::from_text(text.as_str()),
        }
    }

    fn version_key(&self, exact: bool) -> VersionKey {
        match self {
            Located::Text(text) if exact => VersionKey::Text(text.clone()),
            other => VersionKey::by_value(&other.native_id()),
        }
    }
}

/// A version as its family's locator compares it. A JSON Pointer reads a
/// JSON value, where `"2"` and `2` differ; bits and metadata read
/// integers, and text that spells one is that integer. Validation compares
/// set versions the same way.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum VersionKey {
    Int(i128),
    Text(String),
}

impl VersionKey {
    fn of_set(version: &Scalar, exact: bool) -> VersionKey {
        match version {
            Scalar::Str(text) if exact => VersionKey::Text(text.clone()),
            other => VersionKey::by_value(&NativeId::from(other)),
        }
    }

    fn by_value(id: &NativeId) -> VersionKey {
        match id.as_int() {
            Some(n) => VersionKey::Int(n),
            None => VersionKey::Text(id.to_string()),
        }
    }
}

impl fmt::Display for VersionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionKey::Int(n) => write!(f, "{n}"),
            VersionKey::Text(text) => write!(f, "{text:?}"),
        }
    }
}

fn is_exact(locator: &Locator) -> bool {
    matches!(locator, Locator::Path { .. })
}

fn read_metadata(message: &Message<'_>, name: &str) -> Result<Located, String> {
    let mut values = message
        .metadata
        .iter()
        .filter(|(key, _)| *key == name)
        .map(|(_, value)| value);
    let Some(value) = values.next() else {
        return Err(format!("the message has no metadata {name}"));
    };
    if values.next().is_some() {
        return Err(format!("the message has metadata {name} more than once"));
    }
    Ok(match value {
        MetaValue::Uint(n) => Located::Uint(*n),
        MetaValue::Int(n) => Located::Int(*n),
        MetaValue::Text(text) => Located::Text((*text).to_string()),
    })
}

/// An integer or a string; a number counts if it's an integer JSON holds
/// exactly (`2.0` is 2).
fn read_json(value: &Value) -> Result<Located, String> {
    match value {
        Value::String(text) => Ok(Located::Text(text.clone())),
        Value::Number(number) => {
            let integer = json_integer(number);
            if let Some(n) = integer.and_then(|n| u64::try_from(n).ok()) {
                return Ok(Located::Uint(n));
            }
            match integer.and_then(|n| i64::try_from(n).ok()) {
                Some(n) => Ok(Located::Int(n)),
                None => Err(format!(
                    "{number} isn't an integer that can be read exactly"
                )),
            }
        }
        Value::Null => Err("found null, not an integer or a string".to_string()),
        Value::Bool(_) => Err("found a boolean, not an integer or a string".to_string()),
        Value::Array(_) => Err("found an array, not an integer or a string".to_string()),
        Value::Object(_) => Err("found an object, not an integer or a string".to_string()),
    }
}

/// The integer a JSON number holds exactly: an integer literal within 64
/// bits, or a double with no fraction within ±(2^53 - 1), so `42`, `42.0`
/// and `4.2e1` are all 42.
pub(super) fn json_integer(number: &serde_json::Number) -> Option<i128> {
    if let Some(n) = number.as_u64() {
        return Some(i128::from(n));
    }
    if let Some(n) = number.as_i64() {
        return Some(i128::from(n));
    }
    match number.as_f64() {
        // Within ±(2^53 - 1) every integer is a double, so the cast is
        // exact.
        Some(x) if x.fract() == 0.0 && x.abs() <= MAX_SAFE_INTEGER as f64 => Some(x as i128),
        _ => None,
    }
}

fn bits_problem(err: BitsError) -> String {
    match err {
        BitsError::PastEnd {
            end_bits,
            payload_bits,
        } => format!("it reads up to bit {end_bits}, but the payload has {payload_bits} bits"),
        BitsError::Misaligned => {
            "a little-endian value spanning bytes must be whole bytes".to_string()
        }
        BitsError::BadSize => "sizes are 1 to 64 bits".to_string(),
    }
}

#[cfg(test)]
mod tests;
