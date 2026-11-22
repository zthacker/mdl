//! The language-neutral decoding and encoding cases in
//! `conformance/decoding/` and `conformance/encoding/`
//! (`spec/conformance.md` §2 and §3), run through the runtime API as a
//! binding calls it. No expected result there is taken from MDL's output,
//! so a case that fails is wrong itself or has found a bug in MDL, and the
//! spec says which.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use mdl::artifact::build;
use mdl::bindings::NativeId;
use mdl::encode::{ArgumentValue, CommandRequest};
use mdl::message::{Message, MetaValue};
use mdl::read::read_bundle_value;
use mdl::runtime::{ApiError, DecodedMessage, EncodedCommand, Runtime, Value};
use mdl::store;
use mdl::time::Timestamp;
use mdl::validate::validate;
use serde_json::{Map, Value as Json};

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn every_decoding_case_gives_the_result_the_spec_does() {
    let dir = repository().join("conformance/decoding");
    let mut paths = Vec::new();
    case_files(&dir, &mut paths);
    paths.sort();
    assert!(!paths.is_empty(), "no case files in {dir:?}");
    let mut sections = Sections::default();
    let mut failures = Vec::new();
    let mut ran = 0;
    for path in &paths {
        // A file's name is its path under conformance/decoding.
        let name = path
            .strip_prefix(&dir)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let mut file = Failures::new(&name);
        ran += run_file(path, &name, &mut sections, &mut file);
        failures.extend(file.list);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(ran > 0, "no cases in {dir:?}");
}

#[test]
fn every_encoding_case_gives_the_result_the_spec_does() {
    let dir = repository().join("conformance/encoding");
    let mut paths = Vec::new();
    case_files(&dir, &mut paths);
    paths.sort();
    assert!(!paths.is_empty(), "no case files in {dir:?}");
    let mut sections = Sections::default();
    let mut failures = Vec::new();
    let mut ran = 0;
    for path in &paths {
        // A file's name is its path under conformance/encoding.
        let name = path
            .strip_prefix(&dir)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let mut file = Failures::new(&name);
        ran += run_encoding_file(path, &name, &mut sections, &mut file);
        failures.extend(file.list);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(ran > 0, "no cases in {dir:?}");
}

/// A request's values are read as `spec/conformance.md` §3 spells them,
/// and any other spelling is a malformed case, not a value.
#[test]
fn a_tagged_value_has_one_spelling() {
    use serde_json::json;
    let read = |value: Json| tagged(&value);
    assert_eq!(read(json!({ "integer": 7 })), Ok(ArgumentValue::Uint(7)));
    assert_eq!(read(json!({ "integer": -7 })), Ok(ArgumentValue::Int(-7)));
    assert_eq!(
        read(json!({ "integer": "9007199254740992" })),
        Ok(ArgumentValue::Uint(1 << 53))
    );
    assert_eq!(
        read(json!({ "list": [{ "text": "a" }, { "bytes": "c0ff" }] })),
        Ok(ArgumentValue::List(vec![
            ArgumentValue::Text("a".to_string()),
            ArgumentValue::Bytes(vec![0xc0, 0xff]),
        ]))
    );
    for malformed in [
        json!({ "integer": "7" }),
        json!({ "integer": "09007199254740992" }),
        json!({ "integer": 7.0 }),
        json!({ "float": 1 }),
        json!({ "bytes": "c0 ff" }),
        json!({ "bytes": "C0FF" }),
        json!({ "list": [{ "list": [] }] }),
        json!({ "text": "a", "bytes": "00" }),
        json!({ "number": 1 }),
    ] {
        assert!(read(malformed.clone()).is_err(), "{malformed}");
    }
}

/// Metadata strings read as `spec/api.md` §1 reads them, which cases
/// that route by metadata past 2^53 depend on.
#[test]
fn a_metadata_string_is_an_integer_only_in_its_one_spelling() {
    let readings = [
        ("0", Some(Meta::Uint(0))),
        ("42", Some(Meta::Uint(42))),
        ("-5", Some(Meta::Int(-5))),
        ("18446744073709551615", Some(Meta::Uint(u64::MAX))),
        ("-9223372036854775808", Some(Meta::Int(i64::MIN))),
        ("18446744073709551616", None),
        ("-9223372036854775809", None),
        ("-0", None),
        ("042", None),
        ("+1", None),
        ("", None),
        ("-", None),
        ("1e3", None),
        ("PX-9", None),
    ];
    for (text, reading) in readings {
        assert_eq!(integer_spelled(text), reading, "{text:?}");
    }
}

/// What failed in one file, each labelled with the file and case.
struct Failures {
    file: String,
    list: Vec<String>,
}

impl Failures {
    fn new(file: &str) -> Self {
        Failures {
            file: file.to_string(),
            list: Vec::new(),
        }
    }

    fn add(&mut self, case: &str, problem: impl Into<String>) {
        let place = if case.is_empty() {
            self.file.clone()
        } else {
            format!("{}, case {case}", self.file)
        };
        self.list.push(format!("{place}: {}", problem.into()));
    }
}

/// Every case file under `dir`, in its subdirectories too.
fn case_files(dir: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("{dir:?}: {e}")) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            case_files(&path, paths);
        } else if path.extension().is_some_and(|ext| ext == "json") {
            paths.push(path);
        }
    }
}

/// Runs one file's cases, singly and with `decode_many`, and returns how
/// many it ran. `name` is its path under conformance/decoding.
fn run_file(path: &Path, name: &str, sections: &mut Sections, failures: &mut Failures) -> usize {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let file: Json = match serde_json::from_str(&text) {
        Ok(file) => file,
        Err(error) => {
            failures.add("", format!("not JSON: {error}"));
            return 0;
        }
    };
    let cases = match read_file(&file) {
        Ok(cases) => cases,
        Err(problem) => {
            failures.add("", problem);
            return 0;
        }
    };
    let warnings = match expected_warnings(&file) {
        Ok(warnings) => warnings,
        Err(problem) => {
            failures.add("", problem);
            return 0;
        }
    };
    let mut names = BTreeSet::new();
    for case in &cases {
        if !names.insert(case.name.as_str()) {
            failures.add(&case.name, "a second case of this name");
        }
        for citation in &case.spec {
            if let Err(problem) = sections.check(citation) {
                failures.add(&case.name, problem);
            }
        }
    }
    // A store of the file's own, named by its path.
    let stem = name.trim_end_matches(".json").replace(['/', '\\'], "-");
    let runtime = match open(&file, &stem) {
        Ok(runtime) => runtime,
        Err(problem) => {
            failures.add("", problem);
            return 0;
        }
    };
    let mut given: Vec<(String, String)> = runtime
        .warnings()
        .iter()
        .map(|warning| (warning.code.to_string(), warning.path.clone()))
        .collect();
    given.sort();
    if given != warnings {
        failures.add(
            "",
            format!("the open gave the warnings {given:?}, not {warnings:?}"),
        );
    }
    let versions = match def_versions(&runtime) {
        Ok(versions) => versions,
        Err(problem) => {
            failures.add("", problem);
            return 0;
        }
    };

    for case in &cases {
        let metadata = case.message.metadata();
        let result = runtime
            .payload(&case.family, &case.message.payload)
            .and_then(|payload| {
                let message =
                    Message::new(payload, case.message.ground_time).with_metadata(&metadata);
                runtime.decode(&case.family, &message)
            });
        if let Err(problem) = compare(&case.expected, &result, DecodedMessage::to_value, &versions)
        {
            failures.add(&case.name, problem);
        }
    }

    // Each family's messages again, in the file's order, in one call.
    let mut families: Vec<&str> = Vec::new();
    for case in &cases {
        if !families.contains(&case.family.as_str()) {
            families.push(&case.family);
        }
    }
    for family in families {
        let of_family: Vec<&Case> = cases.iter().filter(|case| case.family == family).collect();
        let metadata: Vec<Vec<(&str, MetaValue<'_>)>> = of_family
            .iter()
            .map(|case| case.message.metadata())
            .collect();
        let messages: Result<Vec<Message<'_>>, ApiError> = of_family
            .iter()
            .zip(&metadata)
            .map(|(case, metadata)| {
                runtime
                    .payload(family, &case.message.payload)
                    .map(|payload| {
                        Message::new(payload, case.message.ground_time).with_metadata(metadata)
                    })
            })
            .collect();
        let results = match messages {
            Ok(messages) => runtime.decode_many(family, &messages),
            Err(error) => vec![Err(error); of_family.len()],
        };
        if results.len() != of_family.len() {
            failures.add(
                "",
                format!(
                    "decode_many gave {} results for {} messages of {family}",
                    results.len(),
                    of_family.len()
                ),
            );
            continue;
        }
        for (case, result) in of_family.iter().zip(&results) {
            if let Err(problem) =
                compare(&case.expected, result, DecodedMessage::to_value, &versions)
            {
                failures.add(&case.name, format!("with decode_many: {problem}"));
            }
        }
    }
    cases.len()
}

/// One case (`spec/conformance.md` §2).
struct Case {
    name: String,
    spec: Vec<String>,
    family: String,
    message: Input,
    expected: Expected,
}

enum Expected {
    /// The result's JSON, a decoded message or an encoded command, without
    /// `def_version`.
    Result(Json),
    /// The error's code.
    Error(String),
}

/// A case's message, read.
struct Input {
    payload: Vec<u8>,
    metadata: Vec<(String, Meta)>,
    ground_time: Timestamp,
}

#[derive(Debug, PartialEq)]
enum Meta {
    Uint(u64),
    Int(i64),
    Text(String),
}

impl Input {
    fn metadata(&self) -> Vec<(&str, MetaValue<'_>)> {
        self.metadata
            .iter()
            .map(|(name, value)| {
                let value = match value {
                    Meta::Uint(n) => MetaValue::Uint(*n),
                    Meta::Int(n) => MetaValue::Int(*n),
                    Meta::Text(text) => MetaValue::Text(text),
                };
                (name.as_str(), value)
            })
            .collect()
    }
}

/// The file's cases, after checking that it and they have the keys the
/// format lists and no others, so a misspelt key fails, not skips.
fn read_file(file: &Json) -> Result<Vec<Case>, String> {
    let file = object(file, "the file")?;
    keys(
        file,
        "the file",
        &["about", "definitions", "cases"],
        &["bindings", "warnings"],
    )?;
    text(&file["about"], "about")?;
    let cases = file["cases"]
        .as_array()
        .ok_or("cases isn't a list")?
        .iter()
        .enumerate()
        .map(|(index, case)| read_case(case).map_err(|problem| format!("case {index}: {problem}")))
        .collect::<Result<Vec<_>, _>>()?;
    if cases.is_empty() {
        return Err("no cases".to_string());
    }
    Ok(cases)
}

/// The warnings a file says the open gives, as codes and paths, sorted;
/// none when it has no `warnings` (`spec/conformance.md` §2.1).
fn expected_warnings(file: &Json) -> Result<Vec<(String, String)>, String> {
    let Some(warnings) = file.get("warnings") else {
        return Ok(Vec::new());
    };
    let mut warnings = warnings
        .as_array()
        .ok_or("warnings isn't a list")?
        .iter()
        .map(|warning| {
            let warning = object(warning, "a warning")?;
            keys(warning, "a warning", &["code", "path"], &[])?;
            Ok((
                text(&warning["code"], "a warning's code")?,
                text(&warning["path"], "a warning's path")?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    warnings.sort();
    Ok(warnings)
}

fn read_case(case: &Json) -> Result<Case, String> {
    let case = object(case, "a case")?;
    keys(
        case,
        "a case",
        &["name", "spec", "note", "family", "message"],
        &["decoded", "error"],
    )?;
    let name = text(&case["name"], "name")?;
    let spec = case["spec"]
        .as_array()
        .filter(|citations| !citations.is_empty())
        .ok_or_else(|| format!("{name}: spec isn't a list of sections"))?
        .iter()
        .map(|citation| text(citation, "a citation"))
        .collect::<Result<Vec<_>, _>>()?;
    text(&case["note"], "note")?;
    let expected = match (case.get("decoded"), case.get("error")) {
        (Some(decoded), None) => {
            let decoded = object(decoded, "decoded")?;
            if decoded.contains_key("def_version") {
                return Err(format!(
                    "{name}: decoded gives def_version, which describe gives"
                ));
            }
            Expected::Result(Json::Object(decoded.clone()))
        }
        (None, Some(error)) => Expected::Error(text(error, "error")?),
        _ => return Err(format!("{name}: a case has decoded or error, and not both")),
    };
    Ok(Case {
        family: text(&case["family"], "family")?,
        message: read_message(&case["message"]).map_err(|problem| format!("{name}: {problem}"))?,
        name,
        spec,
        expected,
    })
}

fn read_message(message: &Json) -> Result<Input, String> {
    let message = object(message, "message")?;
    keys(
        message,
        "message",
        &["metadata", "ground_time"],
        &["hex", "json"],
    )?;
    let payload = match (message.get("hex"), message.get("json")) {
        (Some(hex), None) => from_hex(&text(hex, "hex")?)?,
        (None, Some(json)) => text(json, "json")?.into_bytes(),
        _ => return Err("a message has hex or json, and not both".to_string()),
    };
    let metadata = object(&message["metadata"], "metadata")?
        .iter()
        .map(|(name, value)| {
            let value = match value {
                Json::String(text) => {
                    integer_spelled(text).unwrap_or_else(|| Meta::Text(text.clone()))
                }
                Json::Number(n) => match number(n) {
                    Some(Number::Integer(n)) if n.unsigned_abs() <= MAX_SAFE_INTEGER => {
                        integer_meta(n).ok_or_else(|| format!("metadata {name}: {n}"))?
                    }
                    _ => {
                        return Err(format!(
                            "metadata {name}: {n} isn't an integer within 2^53 - 1, as api.md §1 \
                             writes one as a number"
                        ));
                    }
                },
                other => return Err(format!("metadata {name}: {other} isn't an integer or text")),
            };
            Ok((name.clone(), value))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let ground_time = text(&message["ground_time"], "ground_time")?;
    let ground_time = Timestamp::from_rfc3339(&ground_time)
        .map_err(|e| format!("ground_time {ground_time:?}: {e}"))?;
    Ok(Input {
        payload,
        metadata,
        ground_time,
    })
}

/// The largest integer JSON writes as a number (`spec/api.md` §1).
const MAX_SAFE_INTEGER: u128 = (1 << 53) - 1;

/// A metadata string as `spec/api.md` §1 reads it: an integer's one
/// decimal spelling (no sign but a leading `-`, no leading zeros, not
/// `-0`) within −2^63 to 2^64 − 1 is that integer; any other is text.
fn integer_spelled(text: &str) -> Option<Meta> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let one_spelling = !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
        && text != "-0";
    if !one_spelling {
        return None;
    }
    integer_meta(text.parse().ok()?)
}

fn integer_meta(n: i128) -> Option<Meta> {
    u64::try_from(n)
        .map(Meta::Uint)
        .ok()
        .or_else(|| i64::try_from(n).map(Meta::Int).ok())
}

fn object<'a>(value: &'a Json, what: &str) -> Result<&'a Map<String, Json>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{what} isn't an object"))
}

fn text(value: &Json, what: &str) -> Result<String, String> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{what} isn't text"))
}

fn keys(
    object: &Map<String, Json>,
    what: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<(), String> {
    if let Some(key) = object
        .keys()
        .find(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err(format!("{what} has a key the format doesn't list: {key:?}"));
    }
    if let Some(key) = required.iter().find(|key| !object.contains_key(**key)) {
        return Err(format!("{what} has no {key:?}"));
    }
    Ok(())
}

/// Lowercase hex, two digits a byte, spaces between bytes allowed.
fn from_hex(hex: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    for group in hex.split(' ').filter(|group| !group.is_empty()) {
        if group.len() % 2 != 0
            || !group
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        {
            return Err(format!("hex {hex:?} isn't lowercase hex in whole bytes"));
        }
        for pair in group.as_bytes().chunks(2) {
            let pair = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
            bytes.push(u8::from_str_radix(pair, 16).map_err(|e| e.to_string())?);
        }
    }
    Ok(bytes)
}

/// The spec files' headings, read once each, for checking citations.
#[derive(Default)]
struct Sections {
    files: BTreeMap<String, String>,
}

impl Sections {
    /// A citation names a spec file and one of its section numbers, which
    /// is a heading there (`## 5. Extraction`, `### 5.6 Checksums`).
    fn check(&mut self, citation: &str) -> Result<(), String> {
        let (file, section) = citation
            .split_once(" §")
            .ok_or_else(|| format!("citation {citation:?} isn't a file and a section"))?;
        if !file.ends_with(".md") || file.contains(['/', '\\']) {
            return Err(format!(
                "citation {citation:?} doesn't name a file of spec/"
            ));
        }
        if !self.files.contains_key(file) {
            let path = repository().join("spec").join(file);
            let text = fs::read_to_string(&path)
                .map_err(|e| format!("citation {citation:?}: {path:?}: {e}"))?;
            self.files.insert(file.to_string(), text);
        }
        let headed = self.files[file].lines().any(|line| {
            let heading = line.trim_start_matches('#');
            heading.len() < line.len()
                && heading
                    .strip_prefix(' ')
                    .and_then(|rest| rest.strip_prefix(section))
                    .is_some_and(|after| after.starts_with(". ") || after.starts_with(' '))
        });
        if headed {
            Ok(())
        } else {
            Err(format!(
                "citation {citation:?}: {file} has no section {section}"
            ))
        }
    }
}

/// The definitions built into a store of this file's, and opened with
/// its bindings, as morphc builds and a binding opens one.
fn open(file: &Json, stem: &str) -> Result<Runtime, String> {
    let bundle = read_bundle_value(&file["definitions"])
        .map_err(|e| format!("the definitions don't read: {e}"))?
        .value;
    let validated = validate(bundle).map_err(|diagnostics| {
        let lines: Vec<String> = diagnostics
            .iter()
            .map(|d| format!("{} at {:?}: {}", d.code.as_str(), d.path, d.message))
            .collect();
        format!("the definitions don't validate:\n{}", lines.join("\n"))
    })?;
    let artifacts = build(&validated).map_err(|e| format!("the definitions don't build: {e}"))?;
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("conformance")
        .join(stem);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("{dir:?}: {e}"))?;
    }
    store::write(&dir, &artifacts, None).map_err(|e| format!("the store doesn't write: {e}"))?;
    let bindings = match file.get("bindings") {
        None => None,
        Some(lines) => {
            let lines = lines
                .as_array()
                .ok_or("bindings isn't a list of lines")?
                .iter()
                .map(|line| text(line, "a line of the bindings"))
                .collect::<Result<Vec<_>, _>>()?;
            Some(lines.join("\n") + "\n")
        }
    };
    Runtime::open(&dir, bindings.as_deref())
        .map_err(|e| format!("the store doesn't open: {}: {e}", e.code))
}

/// Each set's digest, as `describe` gives it.
fn def_versions(runtime: &Runtime) -> Result<BTreeMap<String, String>, String> {
    let description = runtime
        .describe()
        .and_then(|description| description.to_value())
        .map_err(|e| format!("describe: {}: {e}", e.code))?;
    let description = serde_json::to_value(&description).map_err(|e| e.to_string())?;
    description["sets"]
        .as_array()
        .ok_or("describe gave no sets")?
        .iter()
        .map(
            |set| match (set["name"].as_str(), set["def_version"].as_str()) {
                (Some(name), Some(digest)) => Ok((name.to_string(), digest.to_string())),
                _ => Err(format!(
                    "describe gave a set without a name or digest: {set}"
                )),
            },
        )
        .collect()
}

/// A result against a case's (`spec/conformance.md` §2.1, §3.1): `value`
/// gives a decoded message's or an encoded command's JSON.
fn compare<T>(
    expected: &Expected,
    result: &Result<T, ApiError>,
    value: impl Fn(&T) -> Value,
    versions: &BTreeMap<String, String>,
) -> Result<(), String> {
    match (expected, result) {
        (Expected::Result(expected), Ok(result)) => {
            let mut actual = serde_json::to_value(value(result)).map_err(|e| e.to_string())?;
            let fields = actual
                .as_object_mut()
                .ok_or("a result that isn't an object")?;
            let def_version = fields.remove("def_version");
            let set = fields.get("set").and_then(Json::as_str).unwrap_or_default();
            let digest = versions.get(set);
            match (def_version.as_ref().and_then(Json::as_str), digest) {
                (Some(given), Some(digest)) if given == digest => {}
                _ => {
                    return Err(format!(
                        "def_version {def_version:?} isn't set {set:?}'s digest {digest:?}"
                    ));
                }
            }
            match difference(expected, &actual, "") {
                None => Ok(()),
                Some(difference) => Err(format!("{difference}\nthe whole result: {actual}")),
            }
        }
        (Expected::Result(_), Err(error)) => Err(format!(
            "expected a result, got the error {}: {error}",
            error.code
        )),
        (Expected::Error(code), Err(error)) => {
            if error.code != code {
                Err(format!(
                    "expected the error {code}, got {}: {error}",
                    error.code
                ))
            } else if !error.problems.is_empty() {
                Err(format!(
                    "the error {code} lists problems: {:?}",
                    error.problems
                ))
            } else {
                Ok(())
            }
        }
        (Expected::Error(code), Ok(result)) => {
            let actual = serde_json::to_value(value(result)).map_err(|e| e.to_string())?;
            Err(format!("expected the error {code}, got {actual}"))
        }
    }
}

/// Where two results first differ, as a JSON Pointer and both values, or
/// `None` when they compare equal (`spec/conformance.md` §2.2).
fn difference(expected: &Json, actual: &Json, at: &str) -> Option<String> {
    let differ = || Some(format!("at {at:?}: expected {expected}, got {actual}"));
    match (expected, actual) {
        (Json::Object(expected), Json::Object(actual)) => {
            let keys: BTreeSet<&String> = expected.keys().chain(actual.keys()).collect();
            keys.into_iter().find_map(|key| {
                let here = format!("{at}/{}", key.replace('~', "~0").replace('/', "~1"));
                match (expected.get(key), actual.get(key)) {
                    (Some(e), Some(a)) => difference(e, a, &here),
                    (Some(e), None) => Some(format!("at {here:?}: expected {e}, got no key")),
                    (None, a) => Some(format!("at {here:?}: expected no key, got {a:?}")),
                }
            })
        }
        (Json::Array(e), Json::Array(a)) if e.len() == a.len() => e
            .iter()
            .zip(a)
            .enumerate()
            .find_map(|(index, (e, a))| difference(e, a, &format!("{at}/{index}"))),
        (Json::Number(e), Json::Number(a)) => match (number(e), number(a)) {
            (Some(e), Some(a)) if e == a => None,
            _ => differ(),
        },
        (Json::String(e), Json::String(a)) if e == a => None,
        (Json::Bool(e), Json::Bool(a)) if e == a => None,
        (Json::Null, Json::Null) => None,
        _ => differ(),
    }
}

#[derive(Debug, PartialEq)]
enum Number {
    Integer(i128),
    /// A binary64's bits, so `-0.0` isn't `0.0`.
    Float(u64),
}

/// A number by its spelling: a fraction or an exponent makes it a float.
/// Read from its text, which serde_json keeps as written with
/// `arbitrary_precision` and otherwise writes with a fraction or an
/// exponent for every float.
fn number(n: &serde_json::Number) -> Option<Number> {
    let text = n.to_string();
    if text.contains(['.', 'e', 'E']) {
        text.parse::<f64>().ok().map(|x| Number::Float(x.to_bits()))
    } else {
        text.parse::<i128>().ok().map(Number::Integer)
    }
}

// ---- Encoding cases (`spec/conformance.md` §3) ------------------------------

/// One encoding case.
struct EncodingCase {
    name: String,
    spec: Vec<String>,
    family: String,
    request: CommandRequest,
    expected: Expected,
}

/// Runs one encoding file's cases and returns how many it ran. `name` is
/// its path under conformance/encoding.
fn run_encoding_file(
    path: &Path,
    name: &str,
    sections: &mut Sections,
    failures: &mut Failures,
) -> usize {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let file: Json = match serde_json::from_str(&text) {
        Ok(file) => file,
        Err(error) => {
            failures.add("", format!("not JSON: {error}"));
            return 0;
        }
    };
    let cases = match read_encoding_file(&file) {
        Ok(cases) => cases,
        Err(problem) => {
            failures.add("", problem);
            return 0;
        }
    };
    let mut names = BTreeSet::new();
    for case in &cases {
        if !names.insert(case.name.as_str()) {
            failures.add(&case.name, "a second case of this name");
        }
        for citation in &case.spec {
            if let Err(problem) = sections.check(citation) {
                failures.add(&case.name, problem);
            }
        }
    }
    // A store of the file's own, apart from the decoding files' stores.
    let stem = format!(
        "encoding-{}",
        name.trim_end_matches(".json").replace(['/', '\\'], "-")
    );
    let runtime = match open(&file, &stem) {
        Ok(runtime) => runtime,
        Err(problem) => {
            failures.add("", problem);
            return 0;
        }
    };
    let versions = match def_versions(&runtime) {
        Ok(versions) => versions,
        Err(problem) => {
            failures.add("", problem);
            return 0;
        }
    };
    for case in &cases {
        let result = runtime.encode(&case.family, &case.request);
        if let Err(problem) = compare(&case.expected, &result, EncodedCommand::to_value, &versions)
        {
            failures.add(&case.name, problem);
        }
    }
    cases.len()
}

fn read_encoding_file(file: &Json) -> Result<Vec<EncodingCase>, String> {
    let file = object(file, "the file")?;
    keys(file, "the file", &["about", "definitions", "cases"], &[])?;
    text(&file["about"], "about")?;
    let cases = file["cases"]
        .as_array()
        .ok_or("cases isn't a list")?
        .iter()
        .enumerate()
        .map(|(index, case)| {
            read_encoding_case(case).map_err(|problem| format!("case {index}: {problem}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if cases.is_empty() {
        return Err("no cases".to_string());
    }
    Ok(cases)
}

fn read_encoding_case(case: &Json) -> Result<EncodingCase, String> {
    let case = object(case, "a case")?;
    keys(
        case,
        "a case",
        &["name", "spec", "note", "family", "request"],
        &["encoded", "error"],
    )?;
    let name = text(&case["name"], "name")?;
    let spec = case["spec"]
        .as_array()
        .filter(|citations| !citations.is_empty())
        .ok_or_else(|| format!("{name}: spec isn't a list of sections"))?
        .iter()
        .map(|citation| text(citation, "a citation"))
        .collect::<Result<Vec<_>, _>>()?;
    text(&case["note"], "note")?;
    let expected = match (case.get("encoded"), case.get("error")) {
        (Some(encoded), None) => {
            let encoded = object(encoded, "encoded")?;
            if encoded.contains_key("def_version") {
                return Err(format!(
                    "{name}: encoded gives def_version, which describe gives"
                ));
            }
            Expected::Result(Json::Object(encoded.clone()))
        }
        (None, Some(error)) => Expected::Error(text(error, "error")?),
        _ => return Err(format!("{name}: a case has encoded or error, and not both")),
    };
    Ok(EncodingCase {
        family: text(&case["family"], "family")?,
        request: read_request(&case["request"]).map_err(|problem| format!("{name}: {problem}"))?,
        name,
        spec,
        expected,
    })
}

/// A request (`spec/conformance.md` §3): its set and command, its tagged
/// arguments, and its target.
fn read_request(request: &Json) -> Result<CommandRequest, String> {
    let request = object(request, "request")?;
    keys(
        request,
        "request",
        &["set", "command", "arguments"],
        &["native_id"],
    )?;
    let mut built = CommandRequest::new(
        text(&request["set"], "set")?,
        text(&request["command"], "command")?,
    );
    for (name, value) in object(&request["arguments"], "arguments")? {
        let value = tagged(value).map_err(|problem| format!("argument {name}: {problem}"))?;
        built = built.with_argument(name.clone(), value);
    }
    if let Some(native_id) = request.get("native_id") {
        built = built.with_target(target(native_id)?);
    }
    Ok(built)
}

/// A tagged value: an object whose one key names its kind.
fn tagged(value: &Json) -> Result<ArgumentValue, String> {
    let value = object(value, "a value")?;
    let mut entries = value.iter();
    let (Some((kind, inner)), None) = (entries.next(), entries.next()) else {
        return Err(format!("a value has one key, its kind: {value:?}"));
    };
    match kind.as_str() {
        "integer" => match integer(inner)? {
            n if n >= 0 => u64::try_from(n).map(ArgumentValue::Uint),
            n => i64::try_from(n).map(ArgumentValue::Int),
        }
        .map_err(|_| format!("{inner} is past -2^63 to 2^64 - 1")),
        "float" => float(inner).map(ArgumentValue::Float),
        "boolean" => inner
            .as_bool()
            .map(ArgumentValue::Bool)
            .ok_or_else(|| format!("{inner} isn't a boolean")),
        "text" => text(inner, "text").map(ArgumentValue::Text),
        "bytes" => {
            let hex = text(inner, "bytes")?;
            if hex.contains(' ') {
                return Err(format!("bytes {hex:?} has a space"));
            }
            from_hex(&hex).map(ArgumentValue::Bytes)
        }
        "list" => {
            let values = inner
                .as_array()
                .ok_or_else(|| format!("{inner} isn't a list"))?
                .iter()
                .map(tagged)
                .collect::<Result<Vec<_>, _>>()?;
            // A list argument's values are scalars (`spec/api.md` §2.4).
            if values
                .iter()
                .any(|value| matches!(value, ArgumentValue::List(_)))
            {
                return Err(format!("a list's values aren't lists: {inner}"));
            }
            Ok(ArgumentValue::List(values))
        }
        other => Err(format!("{other:?} isn't a kind of value")),
    }
}

/// An integer in `spec/api.md` §1's one spelling: a number within
/// ±(2^53 - 1), or a decimal string past it.
fn integer(value: &Json) -> Result<i128, String> {
    match value {
        Json::Number(n) => match number(n) {
            Some(Number::Integer(n)) if n.unsigned_abs() <= MAX_SAFE_INTEGER => Ok(n),
            _ => Err(format!("{n} isn't an integer within 2^53 - 1")),
        },
        Json::String(text) => match integer_spelled(text) {
            Some(Meta::Uint(n)) if u128::from(n) > MAX_SAFE_INTEGER => Ok(i128::from(n)),
            Some(Meta::Int(n)) if n.unsigned_abs() > MAX_SAFE_INTEGER as u64 => Ok(i128::from(n)),
            _ => Err(format!(
                "{text:?} isn't an integer's decimal spelling past 2^53 - 1"
            )),
        },
        other => Err(format!("{other} isn't an integer")),
    }
}

/// A float in `spec/api.md` §1's spelling: a number with a fraction or an
/// exponent, or NaN or an infinity as its `non_finite` object.
fn float(value: &Json) -> Result<f64, String> {
    match value {
        Json::Number(n) => match number(n) {
            Some(Number::Float(bits)) => Ok(f64::from_bits(bits)),
            _ => Err(format!("{n} isn't written as a float")),
        },
        Json::Object(map) if map.len() == 1 => match map.get("non_finite").and_then(Json::as_str) {
            Some("NaN") => Ok(f64::NAN),
            Some("inf") => Ok(f64::INFINITY),
            Some("-inf") => Ok(f64::NEG_INFINITY),
            _ => Err(format!("{value} isn't a non_finite object")),
        },
        other => Err(format!("{other} isn't a float")),
    }
}

/// A target: an integer or text, read as `spec/api.md` §1 reads a native id.
fn target(value: &Json) -> Result<NativeId, String> {
    match value {
        Json::String(text) => Ok(NativeId::from_text(text.clone())),
        number => match integer(number)? {
            n if n >= 0 => u64::try_from(n).map(NativeId::from),
            n => i64::try_from(n).map(NativeId::from),
        }
        .map_err(|_| format!("{number} is past -2^63 to 2^64 - 1")),
    }
}
