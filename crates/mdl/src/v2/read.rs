//! Reading MDL documents the way the contract requires.
//!
//! [`read_artifact`] and [`read_bundle`] run every check the IR contract
//! needs, in this order:
//!
//! 1. **Strict parse** ([`canonical::parse_strict`]): no duplicate keys, no
//!    invalid UTF-8 or lone surrogates, numbers that are finite doubles, and
//!    integers that are their own canonical text.
//! 2. **Canonical form** (RFC 8785), which is what the digest covers.
//! 3. **Version check**, before the typed read: a document from a newer
//!    minor version gets "needs MDL IR 2.1" instead of a confusing
//!    "unknown field" about the feature it added.
//! 4. **Typed read from the canonical form**, so a document and its
//!    canonical form always read identically (`16.0` reads as `16`), with
//!    errors that name the JSON path.
//! 5. **Artifacts only: normal form.** The typed value, written back, must
//!    give exactly the canonical text that was read, so an artifact's
//!    identity is a function of its value. Bundles skip this check.
//! 6. **Digest**: the SHA-256 of the canonical text.
//!
//! Deserializing the IR types with plain `serde_json` skips steps 1 to 3
//! and loses error locations.

use serde::Deserialize;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::{Number, Value};
use thiserror::Error;

use crate::canonical::{self, CanonicalError};
use crate::v2::diagnostics::escape_pointer_token;
use crate::v2::ir::{Artifact, Bundle, Catalog, Digest, Family, IrVersion, Set};

/// A document that passed every check, with its identity.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Loaded<T> {
    pub value: T,
    /// The SHA-256 of `canonical`.
    pub digest: Digest,
    /// The document's RFC 8785 canonical text.
    pub canonical: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ReadError {
    /// The bytes break an input rule of the canonical form.
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error("an MDL document is a JSON object, not {found}")]
    NotAnObject { found: &'static str },
    #[error("the document has no \"mdl_version\"; MDL documents start with one, like \"2.0\"")]
    MissingVersion,
    #[error("mdl_version: {message}")]
    BadVersion { message: String },
    #[error("{}", unsupported(*needed, *reader))]
    UnsupportedVersion {
        needed: IrVersion,
        reader: IrVersion,
    },
    /// The document doesn't have the IR's shape. `path` is a JSON Pointer
    /// (RFC 6901) like `/sets/0/packets/2/entries/5`, empty for the whole
    /// document. Paths stop at the nearest tagged object (an entry, a
    /// condition): the message then names the field inside it.
    #[error("{}", at(path, message))]
    Shape { path: String, message: String },
    /// An artifact spelled differently from its normal form (a `null`, an
    /// empty optional list): its digest would differ from the same value
    /// written normally. `path` is the first place that differs.
    #[error(
        "{}",
        at(path, &format!("{detail} (artifacts are written in normal form: absent values and \
                            empty optional lists are omitted)"))
    )]
    NotNormalForm { path: String, detail: String },
}

fn at(path: &str, message: &str) -> String {
    if path.is_empty() {
        message.to_string()
    } else {
        format!("{path}: {message}")
    }
}

fn unsupported(needed: IrVersion, reader: IrVersion) -> String {
    if needed.major == reader.major {
        format!(
            "this document needs MDL IR {needed}, but this build reads up to {reader}; \
             upgrade to read it"
        )
    } else {
        format!(
            "this document is MDL IR {needed}, but this build reads only IR {}.x",
            reader.major
        )
    }
}

/// Read one artifact (catalog, family or set).
pub fn read_artifact(bytes: &[u8]) -> Result<Loaded<Artifact>, ReadError> {
    let (value, canonical) = parse(bytes)?;
    // Type each kind as a plain struct rather than through the tagged
    // `Artifact` enum: serde buffers a tagged enum before typing it, which
    // would lose every error's path.
    let artifact = match value.get("artifact").and_then(Value::as_str) {
        Some("catalog") => {
            let doc: CatalogDoc = typed(&value)?;
            Artifact::catalog(doc.mdl_version, doc.catalog)
        }
        Some("family") => {
            let doc: FamilyDoc = typed(&value)?;
            Artifact::family(doc.mdl_version, doc.family)
        }
        Some("set") => {
            let doc: SetDoc = typed(&value)?;
            Artifact::set(doc.mdl_version, doc.catalog, doc.family, doc.set)
        }
        other => {
            return Err(ReadError::Shape {
                path: "/artifact".to_string(),
                message: match other {
                    Some(kind) => format!(
                        "unknown artifact kind {kind:?}, expected \"catalog\", \"family\" or \"set\""
                    ),
                    None => "missing; an artifact says which kind it is: \"catalog\", \"family\" \
                             or \"set\""
                        .to_string(),
                },
            });
        }
    };
    let normal = serde_json::to_value(&artifact).map_err(|err| ReadError::Shape {
        path: String::new(),
        message: err.to_string(),
    })?;
    if let Some((path, detail)) = first_difference(&value, &normal, String::new()) {
        return Err(ReadError::NotNormalForm { path, detail });
    }
    if let Artifact::Set { set, .. } = &artifact
        && let Some((path, detail)) = out_of_order(set)
    {
        return Err(ReadError::NotNormalForm { path, detail });
    }
    Ok(Loaded {
        value: artifact,
        digest: Digest::of_canonical(&canonical),
        canonical,
    })
}

/// Read a bundle, the document frontends produce.
pub fn read_bundle(bytes: &[u8]) -> Result<Loaded<Bundle>, ReadError> {
    let (value, canonical) = parse(bytes)?;
    let bundle = typed(&value)?;
    Ok(Loaded {
        value: bundle,
        digest: Digest::of_canonical(&canonical),
        canonical,
    })
}

// The artifact kinds as plain structs, for reading with paths.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogDoc {
    #[serde(rename = "artifact")]
    _artifact: IgnoredAny,
    mdl_version: IrVersion,
    catalog: Catalog,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FamilyDoc {
    #[serde(rename = "artifact")]
    _artifact: IgnoredAny,
    mdl_version: IrVersion,
    family: Family,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetDoc {
    #[serde(rename = "artifact")]
    _artifact: IgnoredAny,
    mdl_version: IrVersion,
    catalog: Digest,
    family: Digest,
    set: Set,
}

/// Steps 1 to 3: strict parse, canonical form, the canonical text read back
/// as a value, and the version check.
fn parse(bytes: &[u8]) -> Result<(Value, String), ReadError> {
    let parsed = canonical::parse_strict(bytes)?;
    let canonical = canonical::to_canonical_string(&parsed)?;
    let value = value_from_canonical(&canonical)?;
    check_version(&value)?;
    Ok((value, canonical))
}

/// Canonical text read back as a value, so typing sees exactly what the
/// digest covers: `16.0` arrives as the integer 16.
pub(crate) fn value_from_canonical(canonical: &str) -> Result<Value, ReadError> {
    let mut value: Value =
        serde_json::from_str(canonical).map_err(|err| CanonicalError::InvalidJson {
            detail: err.to_string(),
        })?;
    doubles_beyond_64_bits(&mut value);
    Ok(value)
}

/// Canonical text writes an integral double below 1e21 as plain digits
/// (`1e20` is `100000000000000000000`). Beyond the 64-bit range that text
/// is a double (spec rule 7), and serde_json already reads it as one, unless
/// its `arbitrary_precision` feature is on somewhere in the build, in which
/// case it arrives as a 128-bit integer that serde can't buffer. Make the
/// value say "double" either way.
fn doubles_beyond_64_bits(value: &mut Value) {
    match value {
        Value::Number(n) if n.as_u64().is_none() && n.as_i64().is_none() => {
            if let Some(double) = n.as_f64().and_then(Number::from_f64) {
                *n = double;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(doubles_beyond_64_bits),
        Value::Object(members) => members.values_mut().for_each(doubles_beyond_64_bits),
        _ => {}
    }
}

/// Step 4: type a value, naming the JSON Pointer of any error.
pub(crate) fn typed<T: DeserializeOwned>(value: &Value) -> Result<T, ReadError> {
    serde_path_to_error::deserialize(value).map_err(|err| ReadError::Shape {
        path: json_pointer(err.path()),
        message: err.inner().to_string(),
    })
}

fn json_pointer(path: &serde_path_to_error::Path) -> String {
    use serde_path_to_error::Segment;
    let mut out = String::new();
    for segment in path.iter() {
        match segment {
            Segment::Seq { index } => out.push_str(&format!("/{index}")),
            Segment::Map { key } => {
                out.push('/');
                out.push_str(&escape_pointer_token(key));
            }
            Segment::Enum { variant } => {
                out.push('/');
                out.push_str(&escape_pointer_token(variant));
            }
            Segment::Unknown => break,
        }
    }
    out
}

/// Packets and composites have no meaningful order, so a set artifact
/// lists them by name; any other order would be a second spelling with a
/// second digest.
fn out_of_order(set: &crate::v2::ir::Set) -> Option<(String, String)> {
    let packets = set.packets.windows(2).position(|w| w[0].name >= w[1].name);
    if let Some(index) = packets {
        return Some((
            format!("/set/packets/{}", index + 1),
            "packets are listed in name order, each name once".to_string(),
        ));
    }
    for (index, packet) in set.packets.iter().enumerate() {
        let composites = packet
            .composites
            .windows(2)
            .position(|w| w[0].canonical >= w[1].canonical);
        if let Some(at) = composites {
            return Some((
                format!("/set/packets/{index}/composites/{}", at + 1),
                "composites are listed in canonical name order, each name once".to_string(),
            ));
        }
    }
    None
}

/// Where the document as read and its normal form first differ, and how.
/// Both are walked in canonical key order, so the answer is the first
/// difference in the canonical text.
fn first_difference(read: &Value, normal: &Value, path: String) -> Option<(String, String)> {
    match (read, normal) {
        (Value::Object(read), Value::Object(normal)) => {
            let mut keys: Vec<&String> = read.keys().chain(normal.keys()).collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            keys.dedup();
            keys.into_iter().find_map(|key| {
                let child = format!("{path}/{}", escape_pointer_token(key));
                match (read.get(key), normal.get(key)) {
                    (Some(Value::Null), None) => Some((child, "null must be omitted".to_string())),
                    (Some(Value::Array(a)), None) if a.is_empty() => {
                        Some((child, "an empty list must be omitted".to_string()))
                    }
                    (Some(Value::Object(o)), None) if o.is_empty() => {
                        Some((child, "an empty object must be omitted".to_string()))
                    }
                    (Some(_), None) => Some((child, "this value must be omitted".to_string())),
                    (None, Some(_)) => Some((child, "this value is missing".to_string())),
                    (Some(a), Some(b)) => first_difference(a, b, child),
                    (None, None) => None,
                }
            })
        }
        (Value::Array(read), Value::Array(normal)) if read.len() == normal.len() => read
            .iter()
            .zip(normal)
            .enumerate()
            .find_map(|(index, (a, b))| first_difference(a, b, format!("{path}/{index}"))),
        _ if canonical_equal(read, normal) => None,
        _ => Some((path, "written differently from its normal form".to_string())),
    }
}

fn canonical_equal(a: &Value, b: &Value) -> bool {
    match (
        canonical::to_canonical_string(a),
        canonical::to_canonical_string(b),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn check_version(value: &Value) -> Result<(), ReadError> {
    let Value::Object(members) = value else {
        return Err(ReadError::NotAnObject {
            found: json_type(value),
        });
    };
    let needed = match members.get("mdl_version") {
        None => return Err(ReadError::MissingVersion),
        Some(Value::String(text)) => text
            .parse::<IrVersion>()
            .map_err(|message| ReadError::BadVersion { message })?,
        Some(other) => {
            return Err(ReadError::BadVersion {
                message: format!("expected a string like \"2.0\", got {}", json_type(other)),
            });
        }
    };
    if IrVersion::CURRENT.can_read(needed) {
        Ok(())
    } else {
        Err(ReadError::UnsupportedVersion {
            needed,
            reader: IrVersion::CURRENT,
        })
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::ir::{Entry, Param, ParamType};

    const FULL_BUNDLE: &str = include_str!("../../tests/fixtures/v2/full_bundle.json");

    fn bundle_with(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
        let mut value: Value = serde_json::from_str(FULL_BUNDLE).unwrap();
        edit(&mut value);
        serde_json::to_vec(&value).unwrap()
    }

    #[test]
    fn the_full_bundle_reads_and_its_digest_covers_the_canonical_form() {
        let loaded = read_bundle(FULL_BUNDLE.as_bytes()).unwrap();
        assert_eq!(loaded.value.sets.len(), 3);
        assert_eq!(loaded.digest, Digest::of_canonical(&loaded.canonical));
        // Formatting doesn't change identity: compact input, same digest.
        let compact = bundle_with(|_| {});
        assert_eq!(read_bundle(&compact).unwrap().digest, loaded.digest);
    }

    /// Cross-checked outside Rust: the canonical text below, hashed with
    /// Node's `crypto.createHash("sha256")`, gives the same digest.
    #[test]
    fn digests_match_an_independent_implementation() {
        let artifact = br#"{ "artifact": "catalog", "mdl_version": "2.0",
            "catalog": { "params": { "eps.bus_voltage": { "type": "f64", "unit": "V" } } } }"#;
        let loaded = read_artifact(artifact).unwrap();
        assert_eq!(
            loaded.canonical,
            r#"{"artifact":"catalog","catalog":{"params":{"eps.bus_voltage":{"type":"f64","unit":"V"}}},"mdl_version":"2.0"}"#
        );
        assert_eq!(
            loaded.digest.as_str(),
            "c60ec942a7cbe27fb25d7498d65b89b0c48a10d53089fa8e335b4f6d12d9cfec"
        );
        let Artifact::Catalog { catalog, .. } = loaded.value else {
            panic!("expected a catalog");
        };
        let mut expected = Param::new(ParamType::F64);
        expected.unit = Some("V".into());
        assert_eq!(catalog.params["eps.bus_voltage"], expected);
    }

    #[test]
    fn a_document_and_its_canonical_form_read_identically() {
        let bytes = bundle_with(|v| {
            let entry = &mut v["sets"][0]["packets"][0]["entries"][0];
            entry["size_bits"] = serde_json::json!(16.0);
            entry["invalid"] = serde_json::json!([65535.0]);
        });
        let loaded = read_bundle(&bytes).unwrap();
        let Entry::Field(field) = &loaded.value.sets[0].packets[0].entries[0] else {
            panic!("expected a field");
        };
        assert_eq!(field.size_bits, Some(16));
        assert_eq!(field.invalid, vec![65535.into()]);
    }

    #[test]
    fn input_rules_are_enforced() {
        let duplicate = br#"{"mdl_version":"2.0","catalog":{"params":{"a.b":{"type":"f64"},"a.b":{"type":"i64"}}}}"#;
        assert!(matches!(
            read_bundle(duplicate),
            Err(ReadError::Canonical(CanonicalError::DuplicateKey { .. }))
        ));
        let inexact = br#"{"mdl_version":"2.0","catalog":{},"sets":[{"name":"s","family":"f","version":9007199254740993}]}"#;
        assert!(matches!(
            read_bundle(inexact),
            Err(ReadError::Canonical(CanonicalError::InexactInteger { .. }))
        ));
    }

    #[test]
    fn versions_are_checked_before_the_shape() {
        // A 2.1 document using a field 2.0 doesn't know: the error is about
        // the version, not the field.
        let newer = br#"{"mdl_version":"2.1","catalog":{"commands":{}}}"#;
        assert_eq!(
            read_bundle(newer).unwrap_err().to_string(),
            "this document needs MDL IR 2.1, but this build reads up to 2.0; upgrade to read it"
        );
        let major = br#"{"mdl_version":"3.0","catalog":{}}"#;
        assert_eq!(
            read_bundle(major).unwrap_err().to_string(),
            "this document is MDL IR 3.0, but this build reads only IR 2.x"
        );
        assert!(matches!(
            read_bundle(br#"{"catalog":{}}"#),
            Err(ReadError::MissingVersion)
        ));
        assert_eq!(
            read_bundle(br#"{"mdl_version":2,"catalog":{}}"#)
                .unwrap_err()
                .to_string(),
            "mdl_version: expected a string like \"2.0\", got a number"
        );
        assert_eq!(
            read_bundle(br#"[1]"#).unwrap_err().to_string(),
            "an MDL document is a JSON object, not an array"
        );
    }

    #[test]
    fn artifacts_must_be_in_normal_form() {
        let family = |extra: &str| {
            format!(
                r#"{{"artifact":"family","mdl_version":"2.0","family":{{"name":"f","encoding":"json",
                "alias":{{"kind":"path","pointer":"/a"}}{extra}}}}}"#
            )
        };
        assert!(read_artifact(family("").as_bytes()).is_ok());
        let error = read_artifact(family(r#","version":null"#).as_bytes()).unwrap_err();
        assert!(matches!(error, ReadError::NotNormalForm { .. }), "{error}");
        assert!(
            error
                .to_string()
                .starts_with("/family/version: null must be omitted"),
            "{error}"
        );
        let error = read_artifact(family(r#","metadata":[]"#).as_bytes()).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("/family/metadata: an empty list must be omitted"),
            "{error}"
        );
        // Bundles aren't content-addressed and may use either spelling.
        let lenient = br#"{"mdl_version":"2.0","catalog":{"params":{}},"families":null}"#;
        assert!(read_bundle(lenient).is_ok());
    }

    #[test]
    fn artifact_errors_name_the_path_too() {
        let digest = "a".repeat(64);
        let set = format!(
            r#"{{"artifact":"set","mdl_version":"2.0","catalog":"{digest}","family":"{digest}",
            "set":{{"name":"s","family":"f","packets":[{{"name":"p","length_bytes":4}}]}}}}"#
        );
        let error = read_artifact(set.as_bytes()).unwrap_err().to_string();
        assert!(
            error.starts_with("/set/packets/0/length_bytes: unknown field `length_bytes`"),
            "{error}"
        );
        let family = br#"{"artifact":"family","mdl_version":"2.0","family":{"name":"f",
            "encoding":"xml","alias":{"kind":"path","pointer":"/a"}}}"#;
        let error = read_artifact(family).unwrap_err().to_string();
        assert!(
            error.starts_with("/family/encoding: unknown variant `xml`"),
            "{error}"
        );
        let error = read_artifact(br#"{"artifact":"plan","mdl_version":"2.0"}"#)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "/artifact: unknown artifact kind \"plan\", expected \"catalog\", \"family\" or \"set\""
        );
    }

    #[test]
    fn integral_doubles_beyond_64_bits_read_as_doubles() {
        // 1e20 canonicalizes to plain digits; it must still read as an f64.
        let catalog = br#"{"artifact":"catalog","mdl_version":"2.0","catalog":{"params":
            {"a.b":{"type":"f64","limits":{"red_high":1e20}}}}}"#;
        let loaded = read_artifact(catalog).unwrap();
        assert!(
            loaded
                .canonical
                .contains(r#""red_high":100000000000000000000"#)
        );
        let Artifact::Catalog { catalog, .. } = loaded.value else {
            panic!("expected a catalog");
        };
        let limits = catalog.params["a.b"].limits.unwrap();
        assert_eq!(limits.red_high, Some(1e20));
        // ...but not as an integer constant, with an error that says why.
        let bundle = br#"{"mdl_version":"2.0","catalog":{},"sets":[{"name":"s","family":"f","version":1e20}]}"#;
        assert_eq!(
            read_bundle(bundle).unwrap_err().to_string(),
            "/sets/0/version: integer 100000000000000000000 is outside the 64-bit range; write \
             it as a string"
        );
    }

    #[test]
    fn shape_errors_name_the_path() {
        let bytes = bundle_with(|v| {
            v["sets"][0]["packets"][0]["entries"][1]["fill"] = serde_json::json!(0);
        });
        let error = read_bundle(&bytes).unwrap_err().to_string();
        assert!(
            error.starts_with("/sets/0/packets/0/entries/1: unknown field `fill`"),
            "{error}"
        );
        let bytes = bundle_with(|v| {
            v["families"][1]["encoding"] = serde_json::json!("xml");
        });
        let error = read_bundle(&bytes).unwrap_err().to_string();
        assert!(
            error.starts_with("/families/1/encoding: unknown variant `xml`"),
            "{error}"
        );
    }
}
