use serde_json::{Value, json};

use super::validate;
use crate::v2::diagnostics::Code;
use crate::v2::ir::{Bundle, Conversion, Entry};

const FULL_BUNDLE: &str = include_str!("../../../tests/fixtures/v2/full_bundle.json");

fn fixture() -> Value {
    serde_json::from_str(FULL_BUNDLE).unwrap()
}

fn bundle(value: Value) -> Bundle {
    crate::v2::read::typed(&value).unwrap()
}

/// The diagnostics for the fixture after `edit`, as (code, path) pairs.
fn problems(edit: impl FnOnce(&mut Value)) -> Vec<(Code, String)> {
    let mut value = fixture();
    edit(&mut value);
    match validate(bundle(value)) {
        Ok(_) => Vec::new(),
        Err(diagnostics) => diagnostics
            .iter()
            .map(|d| (d.code, d.path.clone()))
            .collect(),
    }
}

/// Asserts the edit produces exactly one diagnostic, with this code and path.
fn assert_one(code: Code, path: &str, edit: impl FnOnce(&mut Value)) {
    assert_eq!(problems(edit), vec![(code, path.to_string())]);
}

fn set_x(v: &mut Value) -> &mut Value {
    &mut v["sets"][0]
}

fn hk(v: &mut Value) -> &mut Value {
    &mut v["sets"][0]["packets"][0]
}

#[test]
fn the_full_fixture_is_valid() {
    let validated = validate(bundle(fixture())).unwrap();
    assert_eq!(validated.bundle().sets.len(), 3);
}

#[test]
fn catalog_rules() {
    assert_one(Code::InvalidName, "/catalog/params/Eps.Mode", |v| {
        v["catalog"]["params"]["Eps.Mode"] = json!({ "type": "f64" });
    });
    assert_one(Code::InvalidName, "/catalog/params/eps", |v| {
        v["catalog"]["params"]["eps"] = json!({ "type": "f64" });
    });
    assert_one(Code::Empty, "/catalog/params/a.b/states", |v| {
        v["catalog"]["params"]["a.b"] = json!({ "type": "enum" });
    });
    assert_one(Code::ParamShape, "/catalog/params/a.b/states", |v| {
        v["catalog"]["params"]["a.b"] = json!({ "type": "f64", "states": ["X"] });
    });
    assert_one(Code::DuplicateName, "/catalog/params/a.b/states/1", |v| {
        v["catalog"]["params"]["a.b"] = json!({ "type": "enum", "states": ["X", "X"] });
    });
    assert_one(Code::ParamShape, "/catalog/params/a.b/frame", |v| {
        v["catalog"]["params"]["a.b"] = json!({ "type": "vec3", "unit": "m" });
    });
    assert_one(Code::ParamShape, "/catalog/params/a.b/frame", |v| {
        v["catalog"]["params"]["a.b"] = json!({ "type": "f64", "frame": "ECEF" });
    });
    assert_one(Code::ParamShape, "/catalog/params/a.b/frame", |v| {
        v["catalog"]["params"]["a.b"] = json!({ "type": "vec3", "frame": "GEODETIC" });
    });
    assert_one(Code::ParamShape, "/catalog/params/a.b/limits", |v| {
        v["catalog"]["params"]["a.b"] =
            json!({ "type": "enum", "states": ["X"], "limits": { "red_high": 1.0 } });
    });
    assert_one(
        Code::LimitsOrder,
        "/catalog/params/eps.bus_voltage/limits",
        |v| {
            v["catalog"]["params"]["eps.bus_voltage"]["limits"]["yellow_low"] = json!(9.0);
        },
    );
    assert_one(Code::Empty, "/catalog/params/a.b/deprecated", |v| {
        v["catalog"]["params"]["a.b"] = json!({ "type": "f64", "deprecated": "" });
    });
}

#[test]
fn family_rules() {
    assert_one(Code::InvalidName, "/families/0/name", |v| {
        v["families"][0]["name"] = json!("Vendor-X");
        set_x(v)["family"] = json!("Vendor-X");
    });
    assert_one(Code::DuplicateName, "/families/3", |v| {
        let copy = v["families"][1].clone();
        v["families"].as_array_mut().unwrap().push(copy);
    });
    assert_one(Code::ByteOrder, "/families/0/byte_order", |v| {
        v["families"][0]
            .as_object_mut()
            .unwrap()
            .remove("byte_order");
    });
    assert_one(Code::ByteOrder, "/families/1/byte_order", |v| {
        v["families"][1]["byte_order"] = json!("big");
    });
    assert_one(Code::WrongEncoding, "/families/1/alias", |v| {
        v["families"][1]["alias"] = json!({ "kind": "bits", "offset_bits": 0, "size_bits": 8 });
    });
    assert_one(Code::BitSize, "/families/0/alias", |v| {
        v["families"][0]["alias"]["size_bits"] = json!(65);
    });
    assert_one(Code::InvalidPointer, "/families/1/alias", |v| {
        v["families"][1]["alias"]["pointer"] = json!("sat");
    });
    assert_one(Code::UnknownMetadata, "/families/0/version", |v| {
        v["families"][0]["version"]["name"] = json!("format");
    });
    assert_one(Code::InvalidName, "/families/0/metadata/2", |v| {
        v["families"][0]["metadata"] = json!(["apid", "fmt", "x-y"]);
    });
    assert_one(Code::DuplicateValue, "/families/0/metadata/2", |v| {
        v["families"][0]["metadata"] = json!(["apid", "fmt", "apid"]);
    });
    assert_one(Code::FixedVersion, "/families/0/version", |v| {
        v["families"][0]["version"] = json!({ "kind": "fixed", "value": "2" });
    });
}

#[test]
fn set_names_and_versions() {
    assert_one(Code::UnknownFamily, "/sets/0/family", |v| {
        set_x(v)["family"] = json!("vendor_q");
    });
    assert_one(Code::DuplicateName, "/sets/3", |v| {
        let mut copy = v["sets"][1].clone();
        copy["family"] = json!("legacy");
        copy.as_object_mut().unwrap().remove("time");
        copy.as_object_mut().unwrap().remove("types");
        copy.as_object_mut().unwrap().remove("commands");
        copy["packets"].as_array_mut().unwrap().truncate(1);
        copy["packets"][0]
            .as_object_mut()
            .unwrap()
            .remove("identify");
        copy["packets"][0]["entries"] = json!([]);
        v["sets"].as_array_mut().unwrap().push(copy);
    });
    assert_one(Code::InvalidName, "/sets/0/name", |v| {
        set_x(v)["name"] = json!("X2");
    });
    assert_one(Code::SetVersion, "/sets/0/version", |v| {
        set_x(v).as_object_mut().unwrap().remove("version");
    });
    assert_one(Code::SetVersion, "/sets/1/version", |v| {
        v["sets"][1]["version"] = json!(1);
    });
    assert_one(Code::SetVersion, "/sets/3/version", |v| {
        let mut copy = v["sets"][0].clone();
        copy["name"] = json!("vendor_x_v2_copy");
        v["sets"].as_array_mut().unwrap().push(copy);
    });
    assert_one(Code::UnsafeInteger, "/sets/0/version", |v| {
        set_x(v)["version"] = json!(1_i64 << 53);
    });
}

#[test]
fn time_rules() {
    assert_one(Code::WrongEncoding, "/sets/1/time", |v| {
        v["sets"][1]["time"] = json!({ "kind": "cuc", "offset_bits": 0, "coarse_bytes": 4,
            "fine_bytes": 0, "epoch": "2000-01-01T00:00:00Z" });
    });
    assert_one(Code::CucSize, "/sets/0/time", |v| {
        set_x(v)["time"]["coarse_bytes"] = json!(5);
    });
    assert_one(Code::InvalidEpoch, "/sets/0/time/epoch", |v| {
        set_x(v)["time"]["epoch"] = json!("2000-01-01");
    });
    assert_one(Code::WrongEncoding, "/sets/0/time", |v| {
        set_x(v)["time"] = json!({ "kind": "iso8601", "pointer": "/t" });
    });
    assert_one(Code::InvalidPointer, "/sets/1/time/pointer", |v| {
        v["sets"][1]["time"]["pointer"] = json!("t");
    });
    assert_one(Code::UnknownPacket, "/sets/2/time/correlate/packet", |v| {
        v["sets"][2]["time"]["correlate"]["packet"] = json!("CLOCKS");
    });
    assert_one(
        Code::UnknownEntry,
        "/sets/2/time/correlate/boot_field",
        |v| {
            v["sets"][2]["time"]["correlate"]["boot_field"] = json!("uptime");
        },
    );
    assert_one(Code::TimeField, "/sets/0/packets/0/time_field", |v| {
        hk(v)["time_field"] = json!("bus_counts");
    });
    assert_one(Code::UnknownEntry, "/sets/2/packets/0/time_field", |v| {
        v["sets"][2]["packets"][0]["time_field"] = json!("uptime");
    });
}

#[test]
fn identification_rules() {
    assert_one(
        Code::WrongEncoding,
        "/sets/0/packets/0/identify/~1apid",
        |v| {
            hk(v)["identify"] = json!({ "/apid": 100 });
        },
    );
    assert_one(
        Code::UnknownMetadata,
        "/sets/1/packets/0/identify/Ch",
        |v| {
            v["sets"][1]["packets"][0]["identify"] = json!({ "Ch": "PWR.BUS.V" });
        },
    );
    assert_one(
        Code::UnsafeInteger,
        "/sets/0/packets/0/identify/apid",
        |v| {
            hk(v)["identify"]["apid"] = json!(1_i64 << 60);
        },
    );
    assert_one(Code::WrongEncoding, "/sets/1/packets/0/length", |v| {
        v["sets"][1]["packets"][0]["length"] = json!({ "kind": "exact", "bytes": 4 });
    });
    // Same identify: ambiguous.
    assert_one(Code::AmbiguousIdentify, "/sets/0/packets/1/identify", |v| {
        let mut copy = hk(v).clone();
        copy["name"] = json!("HK2");
        set_x(v)["packets"].as_array_mut().unwrap().push(copy);
    });
    // No shared key, neither contains the other: a message carrying both
    // keys matches both.
    assert_one(Code::AmbiguousIdentify, "/sets/0/packets/1/identify", |v| {
        v["families"][0]["metadata"] = json!(["apid", "fmt", "subtype"]);
        let mut copy = hk(v).clone();
        copy["name"] = json!("HK2");
        copy["identify"] = json!({ "subtype": 3 });
        set_x(v)["packets"].as_array_mut().unwrap().push(copy);
    });
    // A key with different values: exclusive, so valid.
    assert_eq!(
        problems(|v| {
            v["families"][0]["metadata"] = json!(["apid", "fmt", "subtype"]);
            let mut copy = hk(v).clone();
            copy["name"] = json!("HK2");
            copy["identify"] = json!({ "apid": 101, "subtype": 3 });
            set_x(v)["packets"].as_array_mut().unwrap().push(copy);
        }),
        vec![]
    );
}

#[test]
fn reference_rules() {
    assert_one(
        Code::UnknownCanonical,
        "/sets/0/packets/0/entries/0/canonical",
        |v| {
            hk(v)["entries"][0]["canonical"] = json!("eps.bus_volts");
        },
    );
    assert_one(
        Code::UnknownCanonical,
        "/sets/0/packets/0/entries/5/canonical",
        |v| {
            hk(v)["entries"][5]["canonical"] = json!("eps.nope");
        },
    );
    assert_one(
        Code::UnknownCanonical,
        "/sets/0/packets/0/composites/0/canonical",
        |v| {
            hk(v)["composites"][0]["canonical"] = json!("nav.pos_eci");
        },
    );
    assert_one(
        Code::UnknownType,
        "/sets/0/packets/0/entries/6/struct",
        |v| {
            hk(v)["entries"][6]["struct"] = json!("vec3_f32");
        },
    );
    assert_one(
        Code::UnknownType,
        "/sets/1/packets/0/entries/1/element",
        |v| {
            v["sets"][1]["packets"][0]["entries"][1]["element"]["name"] = json!("battery");
        },
    );
    assert_one(Code::TypeCycle, "/sets/0/types/a", |v| {
        set_x(v)["types"]["a"] =
            json!({ "entries": [{ "kind": "struct", "name": "b", "struct": "b" }] });
        set_x(v)["types"]["b"] = json!({ "entries": [{ "kind": "array", "name": "as",
            "count": { "kind": "fixed", "count": 1 }, "element": { "kind": "struct", "name": "a" } }] });
    });
    assert_one(Code::TypeCycle, "/sets/0/types/self", |v| {
        set_x(v)["types"]["self"] =
            json!({ "entries": [{ "kind": "struct", "name": "me", "struct": "self" }] });
    });
}

#[test]
fn entry_rules() {
    assert_one(Code::DuplicateName, "/sets/0/packets/0/entries/2", |v| {
        hk(v)["entries"][2]["name"] = json!("bus_counts");
    });
    assert_one(Code::InvalidName, "/sets/0/packets/0/entries/2/name", |v| {
        hk(v)["entries"][2]["name"] = json!("mo.de");
    });
    // Renaming a field breaks the length that names it, and only that.
    assert_eq!(
        problems(|v| hk(v)["entries"][7]["name"] = json!("label_size")),
        vec![(
            Code::UnknownEntry,
            "/sets/0/packets/0/entries/8/length/field".to_string()
        )]
    );
    assert_one(Code::Empty, "/sets/0/types/vec3_i32/entries", |v| {
        set_x(v)["types"]["vec3_i32"]["entries"] = json!([]);
    });
    assert_one(
        Code::Empty,
        "/sets/0/packets/0/entries/5/element/conversion/coefficients",
        |v| {
            hk(v)["entries"][5]["element"]["conversion"]["coefficients"] = json!([]);
        },
    );
    assert_one(
        Code::DuplicateValue,
        "/sets/0/packets/0/entries/2/conversion/states/1",
        |v| {
            hk(v)["entries"][2]["conversion"]["states"][1]["raw"] = json!(0);
        },
    );
    assert_one(
        Code::UnsafeInteger,
        "/sets/0/packets/0/entries/0/invalid/0",
        |v| {
            hk(v)["entries"][0]["invalid"][0] = json!(-(1_i64 << 54));
        },
    );
    assert_one(
        Code::Empty,
        "/sets/0/packets/0/entries/8/when/conditions",
        |v| {
            hk(v)["entries"][8]["when"] = json!({ "kind": "any", "conditions": [] });
        },
    );
    assert_one(
        Code::Empty,
        "/sets/0/packets/0/entries/8/when/conditions/0/values",
        |v| {
            hk(v)["entries"][8]["when"]["conditions"][0]["values"] = json!([]);
        },
    );
    assert_one(
        Code::SampleInterval,
        "/sets/0/packets/0/entries/5/sample_interval_ms",
        |v| {
            hk(v)["entries"][5]["sample_interval_ms"] = json!(-1.0);
        },
    );
    assert_one(
        Code::SampleInterval,
        "/sets/0/packets/0/entries/5/sample_interval_ms",
        |v| {
            hk(v)["entries"][5]
                .as_object_mut()
                .unwrap()
                .remove("canonical");
        },
    );
    assert_one(Code::Empty, "/sets/0/packets/0/composites/0/members", |v| {
        hk(v)["composites"][0]["members"] = json!([]);
    });
}

#[test]
fn non_finite_numbers_built_in_code_are_caught() {
    let mut bundle = bundle(fixture());
    let Entry::Field(field) = &mut bundle.sets[0].packets[0].entries[0] else {
        panic!("expected a field");
    };
    field.conversion = Some(Conversion::linear(f64::NAN, 0.0));
    let diagnostics = validate(bundle).unwrap_err();
    let codes: Vec<Code> = diagnostics.iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![Code::NonFinite]);
}

#[test]
fn every_problem_is_reported_in_document_order_with_its_context() {
    let mut value = fixture();
    value["catalog"]["params"]["eps.mode"]["states"] = json!([]);
    value["families"][1]["byte_order"] = json!("little");
    hk(&mut value)["entries"][0]["canonical"] = json!("eps.bus_volts");
    let diagnostics = validate(bundle(value)).unwrap_err();
    let lines: Vec<String> = diagnostics.iter().map(ToString::to_string).collect();
    assert_eq!(
        lines,
        [
            "catalog, param eps.mode: an enum param lists its states [empty]",
            "family vendor_j: only binary families have a byte_order [byte_order]",
            "set vendor_x_v2, packet HK, field bus_counts: param eps.bus_volts isn't in the \
             catalog [unknown_canonical]",
        ]
    );
}

/// A packet of 8-bit fields, each `(name, offset_bits)`.
fn binary_packet(name: &str, fields: &[(&str, u32)], identify: Value) -> Value {
    let entries: Vec<Value> = fields
        .iter()
        .map(|(field, offset)| {
            json!({ "kind": "field", "name": field, "type": "uint", "offset_bits": offset,
                    "size_bits": 8 })
        })
        .collect();
    json!({ "name": name, "identify": identify, "entries": entries })
}

/// The fixture with vendor_x_v2's packets replaced. The family declares
/// metadata `apid` and `fmt`.
fn with_packets(packets: Vec<Value>) -> impl FnOnce(&mut Value) {
    move |v| {
        set_x(v)["packets"] = Value::Array(packets);
        set_x(v).as_object_mut().unwrap().remove("time");
    }
}

#[test]
fn identify_keys_are_compared_by_what_they_read() {
    // Same name, different bits: both can match `01 02 ...`.
    assert_one(
        Code::AmbiguousIdentify,
        "/sets/0/packets/1/identify",
        with_packets(vec![
            binary_packet("A", &[("kind", 0)], json!({ "kind": 1 })),
            binary_packet("B", &[("pad", 0), ("kind", 8)], json!({ "kind": 2 })),
        ]),
    );
    // A field named like declared metadata is a clash, whatever other
    // packets do: which one identifies the packet would be unclear.
    assert_one(
        Code::DuplicateName,
        "/sets/0/packets/1/identify/apid",
        with_packets(vec![
            binary_packet("A", &[("x", 0)], json!({ "apid": 100 })),
            binary_packet("B", &[("x", 0), ("apid", 8)], json!({ "apid": 200 })),
        ]),
    );
    // Different names for the same bits, different values: exclusive.
    assert_eq!(
        problems(with_packets(vec![
            binary_packet("A", &[("type", 0)], json!({ "type": 1 })),
            binary_packet("B", &[("msg_type", 0)], json!({ "msg_type": 2 })),
        ])),
        vec![]
    );
    // An integer field's value has one spelling, and "1" equals 1.
    assert_eq!(
        problems(with_packets(vec![
            binary_packet("A", &[("kind", 0)], json!({ "kind": "1" })),
            binary_packet("B", &[("kind", 0)], json!({ "kind": 1 })),
        ])),
        vec![
            (
                Code::IntegerAsString,
                "/sets/0/packets/0/identify/kind".to_string()
            ),
            (
                Code::AmbiguousIdentify,
                "/sets/0/packets/1/identify".to_string()
            ),
        ]
    );
}

#[test]
fn the_more_specific_packet_wins() {
    // HK_EXT's keys strictly contain HK's and agree: HK_EXT wins.
    assert_eq!(
        problems(with_packets(vec![
            binary_packet("HK", &[("subtype", 0)], json!({ "apid": 100 })),
            binary_packet(
                "HK_EXT",
                &[("subtype", 0)],
                json!({ "apid": 100, "subtype": 3 })
            ),
        ])),
        vec![]
    );
    // An empty identify is the set's catch-all.
    assert_eq!(
        problems(with_packets(vec![
            binary_packet("ANY", &[("x", 0)], json!({})),
            binary_packet("HK", &[("x", 0)], json!({ "apid": 100 })),
        ])),
        vec![]
    );
    // Two specializations of one base that don't contain each other.
    assert_one(
        Code::AmbiguousIdentify,
        "/sets/0/packets/2/identify",
        with_packets(vec![
            binary_packet("HK", &[("subtype", 0), ("mode", 8)], json!({ "apid": 100 })),
            binary_packet(
                "HK_A",
                &[("subtype", 0), ("mode", 8)],
                json!({ "apid": 100, "subtype": 3 }),
            ),
            binary_packet(
                "HK_B",
                &[("subtype", 0), ("mode", 8)],
                json!({ "apid": 100, "mode": 1 }),
            ),
        ]),
    );
}

#[test]
fn identify_keys_must_resolve() {
    // A misspelled field is neither a field nor declared metadata.
    assert_one(
        Code::UnknownMetadata,
        "/sets/0/packets/0/identify/Kind",
        with_packets(vec![binary_packet(
            "A",
            &[("kind", 0)],
            json!({ "Kind": 1 }),
        )]),
    );
    // A JSON key without its "/" is not declared metadata either.
    assert_one(
        Code::UnknownMetadata,
        "/sets/1/packets/0/identify/ch",
        |v| {
            v["sets"][1]["packets"][0]["identify"] = json!({ "ch": "PWR.BUS.V" });
        },
    );
}

#[test]
fn ambiguity_is_reported_once_per_packet() {
    let packets: Vec<Value> = (0..300)
        .map(|i| binary_packet(&format!("P{i}"), &[("x", 0)], json!({})))
        .collect();
    assert_eq!(problems(with_packets(packets)).len(), 299);

    let mut value = fixture();
    with_packets(
        (0..6)
            .map(|i| binary_packet(&format!("P{i}"), &[("x", 0)], json!({})))
            .collect(),
    )(&mut value);
    let diagnostics = validate(bundle(value)).unwrap_err();
    let last = diagnostics.iter().last().unwrap().to_string();
    assert!(last.contains("as P0, P1, P2 and 2 others"), "{last}");
}

#[test]
fn set_versions_are_compared_by_value_and_fit_their_locator() {
    assert_eq!(
        problems(|v| {
            let mut copy = v["sets"][0].clone();
            copy["name"] = json!("vendor_x_v2_copy");
            copy["version"] = json!("2");
            v["sets"].as_array_mut().unwrap().push(copy);
        }),
        vec![
            (Code::IntegerAsString, "/sets/3/version".to_string()),
            (Code::SetVersion, "/sets/3/version".to_string()),
        ]
    );
    let bits = |v: &mut Value, version: Value| {
        v["families"][0]["version"] = json!({ "kind": "bits", "offset_bits": 0, "size_bits": 4 });
        set_x(v)["version"] = version;
    };
    assert_one(Code::SetVersion, "/sets/0/version", |v| bits(v, json!(16)));
    assert_one(Code::SetVersion, "/sets/0/version", |v| bits(v, json!(-1)));
    assert_eq!(problems(|v| bits(v, json!(15))), vec![]);
    assert_eq!(
        problems(|v| bits(v, json!("15"))),
        vec![(Code::IntegerAsString, "/sets/0/version".to_string())]
    );
}

#[test]
fn epochs_have_one_spelling() {
    for bad in [
        "2000-01-01 00:00:00Z",
        "2000-01-01T00:00:00z",
        "2000-01-01T23:59:60Z",
        "2000-01-01T00:00:00+00:00",
    ] {
        assert_one(Code::InvalidEpoch, "/sets/0/time/epoch", |v| {
            set_x(v)["time"]["epoch"] = json!(bad);
        });
    }
}

#[test]
fn correlation_needs_two_fields() {
    assert_one(Code::CorrelateFields, "/sets/2/time/correlate", |v| {
        v["sets"][2]["time"]["correlate"]["unix_field"] = json!("boot_ms");
    });
}

#[test]
fn one_spelling_per_meaning() {
    assert_one(Code::Empty, "/catalog/params/a.b/limits", |v| {
        v["catalog"]["params"]["a.b"] = json!({ "type": "f64", "limits": {} });
    });
    assert_one(
        Code::ExplicitDefault,
        "/sets/0/packets/0/entries/8/text",
        |v| {
            hk(v)["entries"][8]["text"] = json!("utf8");
        },
    );
    assert_eq!(
        problems(|v| {
            v["catalog"]["params"]["eps.3v3_current"] = json!({ "type": "f64", "unit": "A" });
        }),
        vec![]
    );
}

#[test]
fn nesting_is_limited_and_checked_without_recursion() {
    use super::MAX_NESTING;
    let chain = |v: &mut Value, length: usize| {
        let types = &mut set_x(v)["types"];
        for i in 0..length {
            let entry = if i + 1 < length {
                json!({ "kind": "struct", "name": "next", "struct": format!("t{}", i + 1) })
            } else {
                json!({ "kind": "field", "name": "x", "type": "uint", "size_bits": 8 })
            };
            types[format!("t{i}")] = json!({ "entries": [entry] });
        }
    };
    assert_eq!(problems(|v| chain(v, MAX_NESTING)), vec![]);
    assert_one(Code::NestingDepth, "/sets/0/types/t0", |v| {
        chain(v, MAX_NESTING + 1)
    });
    // Far past any recursion limit, on a test thread's small stack.
    assert_one(Code::NestingDepth, "/sets/0/types/t0", |v| chain(v, 20_000));

    let mut condition = json!({ "kind": "compare", "field": "flags", "op": "eq", "value": 1 });
    for _ in 0..MAX_NESTING {
        condition = json!({ "kind": "not", "condition": condition });
    }
    let found = problems(|v| hk(v)["entries"][8]["when"] = condition);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, Code::NestingDepth);
}

#[test]
fn diagnostics_come_in_document_order() {
    let found = problems(|v| {
        v["families"][0]["metadata"] = json!(["apid", "fmt", "subtype"]);
        let mut copy = hk(v).clone();
        copy["name"] = json!("HK2");
        let mut bad = hk(v).clone();
        bad["name"] = json!("3rd");
        bad["identify"] = json!({ "apid": 300 });
        let packets = set_x(v)["packets"].as_array_mut().unwrap();
        packets.push(copy);
        packets.push(bad);
    });
    assert_eq!(
        found,
        vec![
            (
                Code::AmbiguousIdentify,
                "/sets/0/packets/1/identify".to_string()
            ),
            (Code::InvalidName, "/sets/0/packets/2/name".to_string()),
        ]
    );
}

#[test]
fn huge_fixed_sizes_dont_overflow_offsets() {
    // Debug builds would panic on unchecked arithmetic here.
    let found = problems(with_packets(vec![json!({
        "name": "BIG",
        "identify": { "apid": 1 },
        "entries": [
            { "kind": "array", "name": "a", "count": { "kind": "fixed", "count": 4294967295_u32 },
              "element": { "kind": "scalar", "type": "bytes",
                           "length": { "kind": "fixed", "bytes": 4294967295_u32 } } },
            { "kind": "array", "name": "b", "count": { "kind": "fixed", "count": 4294967295_u32 },
              "element": { "kind": "scalar", "type": "uint", "size_bits": 4294967295_u32 } },
            { "kind": "field", "name": "after", "type": "uint", "size_bits": 8 }
        ]
    })]));
    // The 4294967295-bit integer is caught; nothing overflows on the way.
    assert_eq!(
        found,
        vec![(
            Code::FieldSize,
            "/sets/0/packets/0/entries/1/element/size_bits".to_string()
        )]
    );
}

#[test]
fn an_unknown_family_doesnt_cascade_into_ambiguity() {
    let found = problems(|v| {
        with_packets(
            (0..5)
                .map(|i| binary_packet(&format!("P{i}"), &[("x", 0)], json!({ "apid": i })))
                .collect(),
        )(v);
        set_x(v)["family"] = json!("vendor_xx");
    });
    assert_eq!(
        found,
        vec![(Code::UnknownFamily, "/sets/0/family".to_string())]
    );

    // An unresolved key next to a catch-all: only the real error.
    assert_eq!(
        problems(with_packets(vec![
            binary_packet("ANY", &[("x", 0)], json!({})),
            binary_packet("A", &[("x", 0)], json!({ "tpye": 1 })),
        ])),
        vec![(
            Code::UnknownMetadata,
            "/sets/0/packets/1/identify/tpye".to_string()
        )]
    );
}

#[test]
fn field_keys_take_values_the_field_can_hold() {
    let one = |value: Value| {
        problems(with_packets(vec![binary_packet(
            "A",
            &[("kind", 0)],
            json!({ "kind": value }),
        )]))
    };
    let path = "/sets/0/packets/0/identify/kind".to_string();
    assert_eq!(one(json!(255)), vec![]);
    for bad in [json!("SAFE"), json!(256), json!(-1)] {
        assert_eq!(
            one(bad.clone()),
            vec![(Code::IdentifyValue, path.clone())],
            "{bad}"
        );
    }
    let float_key = problems(with_packets(vec![json!({
        "name": "A", "identify": { "temp": 1 },
        "entries": [{ "kind": "field", "name": "temp", "type": "float", "size_bits": 32 }]
    })]));
    assert_eq!(
        float_key,
        vec![(
            Code::IdentifyValue,
            "/sets/0/packets/0/identify/temp".to_string()
        )]
    );
}

#[test]
fn a_field_may_not_shadow_declared_metadata() {
    assert_one(
        Code::DuplicateName,
        "/sets/0/packets/0/identify/apid",
        with_packets(vec![binary_packet(
            "A",
            &[("x", 0), ("apid", 16)],
            json!({ "apid": 100 }),
        )]),
    );
}

#[test]
fn values_compare_as_their_key_reads_them() {
    // Metadata reads integers: "100" is 100 (and must be spelled 100).
    assert_eq!(
        problems(with_packets(vec![
            binary_packet("A", &[("x", 0)], json!({ "apid": "100" })),
            binary_packet("B", &[("x", 0)], json!({ "apid": 100 })),
        ])),
        vec![
            (
                Code::IntegerAsString,
                "/sets/0/packets/0/identify/apid".to_string()
            ),
            (
                Code::AmbiguousIdentify,
                "/sets/0/packets/1/identify".to_string()
            ),
        ]
    );
    // A JSON Pointer reads JSON values: the string "2" and the number 2
    // differ, for identify keys and for path-located versions alike.
    let found = problems(|v| {
        v["families"][1]["version"] = json!({ "kind": "path", "pointer": "/fmt" });
        v["sets"][1]["version"] = json!(2);
        let mut v1 = v["sets"][1].clone();
        v1["name"] = json!("vendor_j_v2s");
        v1["version"] = json!("2");
        v["sets"].as_array_mut().unwrap().push(v1);
        let mut other = v["sets"][1]["packets"][0].clone();
        other["name"] = json!("BUS_S");
        other["identify"] = json!({ "/ch": "PWR.BUS.V", "/t": "2" });
        v["sets"][1]["packets"][0]["identify"] = json!({ "/ch": "PWR.BUS.V", "/t": 2 });
        v["sets"][1]["packets"].as_array_mut().unwrap().push(other);
    });
    assert_eq!(found, vec![]);
}

#[test]
fn metadata_is_listed_in_sorted_order() {
    assert_one(Code::NotSorted, "/families/0/metadata/1", |v| {
        v["families"][0]["metadata"] = json!(["fmt", "apid"]);
    });
}

#[test]
fn diagnostics_at_one_path_are_ordered_by_code() {
    let found = problems(|v| {
        v["families"][0]["version"] = json!({ "kind": "bits", "offset_bits": 0, "size_bits": 4 });
        set_x(v)["version"] = json!("16");
    });
    assert_eq!(
        found,
        vec![
            (Code::IntegerAsString, "/sets/0/version".to_string()),
            (Code::SetVersion, "/sets/0/version".to_string()),
        ]
    );
}

// ---- Part 2: layout and canonical mapping ----

/// vendor_x_v2 (binary, big-endian) with one packet of these entries.
fn layout(entries: Value) -> impl FnOnce(&mut Value) {
    with_packets(vec![
        json!({ "name": "P", "identify": { "apid": 1 }, "entries": entries }),
    ])
}

/// The same, with the packet's length set.
fn layout_with_length(entries: Value, length: Value) -> impl FnOnce(&mut Value) {
    with_packets(vec![
        json!({ "name": "P", "identify": { "apid": 1 }, "length": length, "entries": entries }),
    ])
}

fn uint(name: &str, bits: u32) -> Value {
    json!({ "kind": "field", "name": name, "type": "uint", "size_bits": bits })
}

fn at(mut entry: Value, offset: u32) -> Value {
    entry["offset_bits"] = json!(offset);
    entry
}

fn when(mut entry: Value, condition: Value) -> Value {
    entry["when"] = condition;
    entry
}

fn on(field: &str, op: &str, value: Value) -> Value {
    json!({ "kind": "compare", "field": field, "op": op, "value": value })
}

fn one(entries: Value) -> Vec<(Code, String)> {
    problems(layout(entries))
}

fn entry(index: usize, rest: &str) -> String {
    format!("/sets/0/packets/0/entries/{index}{rest}")
}

#[test]
fn positions_overlaps_and_bounds() {
    // Two unconditional fields reading the same bits.
    assert_eq!(
        one(json!([at(uint("a", 16), 0), at(uint("b", 8), 8)])),
        vec![(Code::Overlap, entry(1, ""))]
    );
    // A conditional alternative reading of the same bits is allowed.
    assert_eq!(
        one(json!([
            uint("flag", 8),
            at(uint("a", 16), 8),
            when(at(uint("b", 8), 8), on("flag", "eq", json!(1)))
        ])),
        vec![]
    );
    // An explicit offset after a variable-size entry can't be proven clear.
    assert_eq!(
        one(json!([
            uint("n", 8),
            { "kind": "field", "name": "s", "type": "string",
              "length": { "kind": "field", "field": "n" } },
            at(uint("late", 8), 8)
        ])),
        vec![(Code::Overlap, entry(2, ""))]
    );
    // Nothing comes after an entry that reads to the end.
    assert_eq!(
        one(json!([
            { "kind": "field", "name": "rest", "type": "bytes", "length": { "kind": "remaining" } },
            uint("x", 8)
        ])),
        vec![(Code::ReadsToEnd, entry(1, ""))]
    );
    // An exact length bounds every entry. (8 bytes also holds the family's
    // alias at bits 48 to 63.)
    assert_eq!(
        problems(layout_with_length(
            json!([uint("a", 8), at(uint("b", 16), 56)]),
            json!({ "kind": "exact", "bytes": 8 })
        )),
        vec![(Code::OutOfBounds, entry(1, ""))]
    );
    // The family's alias (bits 48 to 63) must fit every exact-length packet.
    assert_eq!(
        problems(layout_with_length(
            json!([uint("a", 8)]),
            json!({ "kind": "exact", "bytes": 4 })
        )),
        vec![(Code::OutOfBounds, "/sets/0/packets/0/length".to_string())]
    );
}

#[test]
fn sizes_suit_types() {
    let field = |ty: &str, extra: Value| {
        let mut f = json!({ "kind": "field", "name": "f", "type": ty });
        for (k, v) in extra.as_object().unwrap() {
            f[k] = v.clone();
        }
        one(json!([f]))
    };
    for (ty, extra, key) in [
        ("uint", json!({ "size_bits": 0 }), "/size_bits"),
        ("int", json!({ "size_bits": 65 }), "/size_bits"),
        ("float", json!({ "size_bits": 16 }), "/size_bits"),
        ("bool", json!({ "size_bits": 2 }), "/size_bits"),
        ("uint", json!({}), "/size_bits"),
        ("string", json!({}), "/length"),
        (
            "string",
            json!({ "size_bits": 8, "length": { "kind": "fixed", "bytes": 1 } }),
            "/size_bits",
        ),
        (
            "uint",
            json!({ "size_bits": 8, "length": { "kind": "fixed", "bytes": 1 } }),
            "/length",
        ),
        ("uint", json!({ "size_bits": 8, "text": "ascii" }), "/text"),
    ] {
        assert_eq!(
            field(ty, extra.clone()),
            vec![(Code::FieldSize, entry(0, key))],
            "{ty} {extra}"
        );
    }
    assert_eq!(
        one(json!([{ "kind": "padding", "size_bits": 0 }])),
        vec![(Code::FieldSize, entry(0, "/size_bits"))]
    );
    assert_eq!(
        one(
            json!([{ "kind": "field", "name": "f", "type": "uint", "size_bits": 8, "pointer": "/f" }])
        ),
        vec![(Code::WrongEncoding, entry(0, "/pointer"))]
    );
}

#[test]
fn array_rules() {
    let element = |e: Value, count: Value| {
        one(json!([uint("n", 8), { "kind": "array", "name": "a", "count": count, "element": e }]))
    };
    assert_eq!(
        one(json!([{ "kind": "array", "name": "a",
                     "element": { "kind": "scalar", "type": "uint", "size_bits": 8 } }])),
        vec![(Code::WrongEncoding, entry(0, "/count"))]
    );
    assert_eq!(
        element(
            json!({ "kind": "scalar", "type": "bytes", "length": { "kind": "fixed", "bytes": 0 } }),
            json!({ "kind": "remaining" })
        ),
        vec![(Code::FieldSize, entry(1, "/count"))]
    );
    assert_eq!(
        element(
            json!({ "kind": "scalar", "type": "string", "length": { "kind": "field", "field": "n" } }),
            json!({ "kind": "fixed", "count": 2 })
        ),
        vec![(Code::ReferenceType, entry(1, "/element/length"))]
    );
    assert_eq!(
        element(
            json!({ "kind": "scalar", "type": "bytes", "length": { "kind": "remaining" } }),
            json!({ "kind": "fixed", "count": 2 })
        ),
        vec![(Code::ReadsToEnd, entry(1, "/element/length"))]
    );
}

#[test]
fn little_endian_alignment() {
    // legacy_v1 is little-endian; replace its packet's entries.
    let legacy = |entries: Value| {
        problems(move |v| {
            v["sets"][2]["packets"][0]["entries"] = entries;
            v["sets"][2]["packets"][0]
                .as_object_mut()
                .unwrap()
                .remove("length");
            v["sets"][2].as_object_mut().unwrap().remove("time");
            v["sets"][2]["packets"][0]
                .as_object_mut()
                .unwrap()
                .remove("time_field");
        })
    };
    let path = |i: usize| format!("/sets/2/packets/0/entries/{i}");
    assert_eq!(
        legacy(json!([uint("a", 12)])),
        vec![(Code::Alignment, format!("{}/size_bits", path(0)))]
    );
    assert_eq!(
        legacy(json!([uint("pad", 4), uint("a", 16)])),
        vec![(Code::Alignment, path(1))]
    );
    assert_eq!(
        legacy(json!([uint("pad", 6), uint("a", 3)])),
        vec![(Code::Alignment, path(1))]
    );
    assert_eq!(legacy(json!([uint("pad", 5), uint("a", 3)])), vec![]);
    // Strings start on a byte in either byte order.
    assert_eq!(
        one(json!([uint("pad", 4),
                   { "kind": "field", "name": "s", "type": "string",
                     "length": { "kind": "fixed", "bytes": 2 } }])),
        vec![(Code::Alignment, entry(1, ""))]
    );
    // A CUC time code starts on a byte.
    assert_one(Code::Alignment, "/sets/0/time/offset_bits", |v| {
        set_x(v)["time"]["offset_bits"] = json!(4);
    });
}

#[test]
fn references_point_backwards_at_suitable_fields() {
    let counted = |count_field: &str, entries_before: Value| {
        let mut entries = entries_before.as_array().unwrap().clone();
        entries.push(json!({ "kind": "array", "name": "a",
            "count": { "kind": "field", "field": count_field },
            "element": { "kind": "scalar", "type": "uint", "size_bits": 8 } }));
        entries.push(uint("later", 8));
        one(Value::Array(entries))
    };
    let count = |i: usize| entry(i, "/count/field");
    assert_eq!(
        counted("later", json!([])),
        vec![(Code::ForwardReference, count(0))]
    );
    assert_eq!(
        counted("nope", json!([])),
        vec![(Code::UnknownEntry, count(0))]
    );
    assert_eq!(
        counted(
            "n",
            json!([{ "kind": "field", "name": "n", "type": "int", "size_bits": 8 }])
        ),
        vec![(Code::ReferenceType, count(1))]
    );
    assert_eq!(
        counted(
            "n",
            json!([
                uint("flag", 8),
                when(uint("n", 8), on("flag", "eq", json!(1)))
            ])
        ),
        vec![(Code::ReferenceType, count(2))]
    );

    let guarded = |condition: Value, before: Value| {
        let mut entries = before.as_array().unwrap().clone();
        entries.push(when(uint("x", 8), condition));
        entries.push(uint("later", 8));
        one(Value::Array(entries))
    };
    let field_ref = |i: usize| entry(i, "/when/field");
    assert_eq!(
        guarded(on("later", "eq", json!(1)), json!([])),
        vec![(Code::ForwardReference, field_ref(0))]
    );
    let float = json!([{ "kind": "field", "name": "f", "type": "float", "size_bits": 32 }]);
    assert_eq!(
        guarded(on("f", "eq", json!(1)), float),
        vec![(Code::ReferenceType, entry(1, "/when/op"))]
    );
    let int = json!([{ "kind": "field", "name": "i", "type": "int", "size_bits": 8 }]);
    assert_eq!(
        guarded(on("i", "mask_any", json!(1)), int),
        vec![(Code::ReferenceType, entry(1, "/when/op"))]
    );
    let flag = json!([{ "kind": "field", "name": "b", "type": "bool", "size_bits": 1 }]);
    assert_eq!(
        guarded(on("b", "gt", json!(0)), flag),
        vec![(Code::ReferenceType, entry(1, "/when/op"))]
    );
    let text = json!([{ "kind": "field", "name": "s", "type": "string",
                        "length": { "kind": "fixed", "bytes": 2 } }]);
    assert_eq!(
        guarded(on("s", "eq", json!(1)), text),
        vec![(Code::ReferenceType, entry(1, "/when/value"))]
    );
    assert_eq!(
        guarded(
            on("c", "eq", json!(1)),
            json!([
                uint("flag", 8),
                when(uint("c", 8), on("flag", "eq", json!(1)))
            ])
        ),
        vec![(Code::ReferenceType, entry(2, "/when/field"))]
    );
}

#[test]
fn conversions_and_sentinels_suit_the_field() {
    let field = |f: Value| one(json!([f]));
    let text = json!({ "kind": "field", "name": "s", "type": "string",
        "length": { "kind": "fixed", "bytes": 2 } });
    let mut linear_text = text.clone();
    linear_text["conversion"] = json!({ "kind": "linear", "scale": 1.0, "offset": 0.0 });
    assert_eq!(
        field(linear_text),
        vec![(Code::ConversionType, entry(0, "/conversion"))]
    );
    assert_eq!(
        field(
            json!({ "kind": "field", "name": "f", "type": "float", "size_bits": 32,
            "conversion": { "kind": "states", "states": [{ "raw": 0, "name": "A" }] } })
        ),
        vec![(Code::ConversionType, entry(0, "/conversion"))]
    );
    assert_eq!(
        field(
            json!({ "kind": "field", "name": "m", "type": "uint", "size_bits": 2,
            "conversion": { "kind": "states", "states": [{ "raw": 4, "name": "A" }] } })
        ),
        vec![(Code::ValueRange, entry(0, "/conversion/states/0"))]
    );
    let mut sentinel = uint("v", 16);
    sentinel["invalid"] = json!([70000]);
    assert_eq!(
        field(sentinel),
        vec![(Code::ValueRange, entry(0, "/invalid/0"))]
    );
    // Floats take integer sentinels like -9999, compared by value.
    assert_eq!(
        field(
            json!({ "kind": "field", "name": "f", "type": "float", "size_bits": 32,
                      "invalid": [-9999] })
        ),
        vec![]
    );
    assert_eq!(
        field(
            json!({ "kind": "field", "name": "f", "type": "float", "size_bits": 32,
                      "invalid": ["NaN"] })
        ),
        vec![(Code::ValueRange, entry(0, "/invalid/0"))]
    );
    let mut text_sentinel = text.clone();
    text_sentinel["invalid"] = json!(["NA"]);
    assert_eq!(field(text_sentinel), vec![]);
    let mut bad_text_sentinel = text;
    bad_text_sentinel["invalid"] = json!([5]);
    assert_eq!(
        field(bad_text_sentinel),
        vec![(Code::ValueRange, entry(0, "/invalid/0"))]
    );
}

#[test]
fn self_describing_layout_rules() {
    fn bus(v: &mut Value) -> &mut Value {
        &mut v["sets"][1]["packets"][0]["entries"][0]
    }
    let path = |rest: &str| format!("/sets/1/packets/0/entries/0{rest}");
    assert_one(Code::WrongEncoding, &path("/offset_bits"), |v| {
        bus(v)["offset_bits"] = json!(0);
    });
    assert_one(Code::WrongEncoding, &path("/pointer"), |v| {
        bus(v).as_object_mut().unwrap().remove("pointer");
    });
    assert_one(Code::WrongEncoding, "/sets/1/packets/0/entries/2", |v| {
        v["sets"][1]["packets"][0]["entries"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "kind": "padding", "size_bits": 8 }));
    });
    assert_one(
        Code::WrongEncoding,
        "/sets/1/packets/0/entries/1/count",
        |v| {
            v["sets"][1]["packets"][0]["entries"][1]["count"] =
                json!({ "kind": "fixed", "count": 2 });
        },
    );
}

#[test]
fn canonical_mapping_matches_the_catalog() {
    let mapped = |f: Value| one(json!([f]));
    let canonical = entry(0, "/canonical");
    let text = json!({ "kind": "field", "name": "s", "type": "string",
        "length": { "kind": "fixed", "bytes": 2 }, "canonical": "eps.bus_voltage", "unit": "V" });
    assert_eq!(mapped(text), vec![(Code::CanonicalType, canonical.clone())]);
    assert_eq!(
        mapped(
            json!({ "kind": "field", "name": "m", "type": "uint", "size_bits": 2,
            "conversion": { "kind": "states", "states": [{ "raw": 0, "name": "SAFE" }] },
            "canonical": "eps.bus_voltage", "unit": "V" })
        ),
        vec![(Code::CanonicalType, canonical.clone())]
    );
    assert_eq!(
        mapped(
            json!({ "kind": "field", "name": "v", "type": "float", "size_bits": 32,
            "canonical": "nav.pos_ecef", "unit": "m" })
        ),
        vec![(Code::CanonicalType, canonical.clone())]
    );
    assert_eq!(
        mapped(
            json!({ "kind": "field", "name": "v", "type": "uint", "size_bits": 16,
            "canonical": "eps.bus_voltage", "unit": "mV" })
        ),
        vec![(Code::UnitMismatch, entry(0, "/unit"))]
    );
    assert_eq!(
        mapped(
            json!({ "kind": "field", "name": "v", "type": "uint", "size_bits": 16,
            "canonical": "eps.bus_voltage" })
        ),
        vec![(Code::UnitMismatch, entry(0, "/unit"))]
    );
    assert_eq!(
        mapped(
            json!({ "kind": "field", "name": "m", "type": "uint", "size_bits": 2,
            "conversion": { "kind": "states", "states": [{ "raw": 0, "name": "SCIENCE" }] },
            "canonical": "eps.mode" })
        ),
        vec![(Code::CanonicalType, entry(0, "/conversion/states/0"))]
    );
    // i64 from a 64-bit uint could overflow.
    let wide = layout(json!([{ "kind": "field", "name": "c", "type": "uint",
        "size_bits": 64, "canonical": "a.count" }]));
    assert_eq!(
        problems(move |v| {
            v["catalog"]["params"]["a.count"] = json!({ "type": "i64" });
            wide(v);
        }),
        vec![(Code::CanonicalType, canonical)]
    );
}

#[test]
fn composites_resolve_and_convert() {
    let composite = |c: Value| problems(|v| hk(v)["composites"][0] = c);
    let path = |rest: &str| format!("/sets/0/packets/0/composites/0{rest}");
    let base = |frame: &str, members: Value| json!({ "canonical": "nav.pos_ecef", "frame": frame, "members": members });
    assert_eq!(
        composite(base("ECI_J2000", json!(["pos.x", "pos.y"]))),
        vec![(Code::CompositeShape, path("/members"))]
    );
    assert_eq!(
        composite(json!({ "canonical": "eps.bus_voltage", "frame": "ECEF",
            "members": ["pos.x", "pos.y", "pos.z"] })),
        vec![(Code::CompositeShape, path("/canonical"))]
    );
    assert_eq!(
        composite(base("BODY", json!(["pos.x", "pos.y", "pos.z"]))),
        vec![(Code::FrameMismatch, path("/frame"))]
    );
    assert_eq!(
        composite(base("ECI_J2000", json!(["pos.x", "pos.y", "pos.w"]))),
        vec![(Code::UnknownEntry, path("/members/2"))]
    );
    assert_eq!(
        composite(base("ECI_J2000", json!(["pos.x", "pos.x", "pos.z"]))),
        vec![(Code::DuplicateValue, path("/members/1"))]
    );
    assert_eq!(
        composite(base("ECI_J2000", json!(["pos.x", "pos.y", "label"]))),
        vec![(Code::CanonicalType, path("/members/2"))]
    );
    // Geodetic: latitude and longitude in degrees, then altitude in meters.
    assert_eq!(
        composite(base("GEODETIC", json!(["pos.x", "pos.y", "pos.z"]))),
        vec![
            (Code::UnitMismatch, path("/members/0")),
            (Code::UnitMismatch, path("/members/1")),
        ]
    );
    let geodetic = problems(|v| {
        set_x(v)["types"]["lla"] = json!({ "entries": [
            { "kind": "field", "name": "lat", "type": "int", "size_bits": 32, "unit": "deg" },
            { "kind": "field", "name": "lon", "type": "int", "size_bits": 32, "unit": "deg" },
            { "kind": "field", "name": "alt", "type": "int", "size_bits": 32, "unit": "m" }
        ] });
        hk(v)["entries"][6]["struct"] = json!("lla");
        hk(v)["composites"][0] = json!({ "canonical": "nav.pos_ecef", "frame": "GEODETIC",
            "members": ["pos.lat", "pos.lon", "pos.alt"] });
    });
    assert_eq!(geodetic, vec![]);
    // Members index into fixed-size arrays only.
    let indexed = problems(|v| {
        hk(v)["entries"][5]["count"] = json!({ "kind": "fixed", "count": 3 });
        hk(v)["entries"][5]["element"]["unit"] = json!("m");
        hk(v)["entries"][5]["element"]
            .as_object_mut()
            .unwrap()
            .remove("conversion");
        hk(v)["entries"][5]
            .as_object_mut()
            .unwrap()
            .remove("canonical");
        hk(v)["entries"][5]
            .as_object_mut()
            .unwrap()
            .remove("sample_interval_ms");
        hk(v)["composites"][0] = json!({ "canonical": "nav.pos_ecef", "frame": "ECEF",
            "members": ["samples[0]", "samples[1]", "samples[2]"] });
    });
    assert_eq!(indexed, vec![]);
    assert_eq!(
        composite(base(
            "ECEF",
            json!(["samples[0]", "samples[1]", "samples[2]"])
        )),
        vec![
            (Code::UnknownEntry, path("/members/0")),
            (Code::UnknownEntry, path("/members/1")),
            (Code::UnknownEntry, path("/members/2")),
        ]
    );
}

// ---- Part 2, review round 1 ----

fn string_from(name: &str, length_field: &str) -> Value {
    json!({ "kind": "field", "name": name, "type": "string",
            "length": { "kind": "field", "field": length_field } })
}

#[test]
fn overlap_detection_doesnt_depend_on_listing_order() {
    // An explicit entry listed before a variable one it can collide with.
    assert_eq!(
        one(json!([
            at(uint("y", 8), 8),
            at(uint("len", 8), 0),
            string_from("s", "len")
        ])),
        vec![(Code::Overlap, entry(2, ""))]
    );
    // The variable entry is itself explicit.
    assert_eq!(
        one(json!([
            at(uint("y", 8), 16),
            at(uint("len", 8), 0),
            at(string_from("s", "len"), 8)
        ])),
        vec![(Code::Overlap, entry(2, ""))]
    );
    // A sequential entry shifted by an optional one.
    assert_eq!(
        one(json!([
            at(uint("y", 8), 24),
            at(uint("flag", 8), 0),
            when(uint("opt", 16), on("flag", "eq", json!(1))),
            uint("x", 8)
        ])),
        vec![(Code::Overlap, entry(3, ""))]
    );
    // Out of order but disjoint: fine.
    assert_eq!(
        one(json!([at(uint("b", 8), 8), at(uint("a", 8), 0)])),
        vec![]
    );
}

#[test]
fn conditional_overlap_is_only_for_alternatives() {
    let base = |second: Value| {
        one(json!([
            at(uint("flag", 8), 0),
            at(uint("y", 16), 16),
            second
        ]))
    };
    // Half over an unconditional entry: a mistake.
    assert_eq!(
        base(when(at(uint("x", 16), 8), on("flag", "eq", json!(1)))),
        vec![(Code::Overlap, entry(2, ""))]
    );
    // A detail reading inside an unconditional entry: allowed.
    assert_eq!(
        base(when(at(uint("hi", 8), 16), on("flag", "eq", json!(1)))),
        vec![]
    );
    // Two alternatives at one place: allowed.
    assert_eq!(
        one(json!([
            at(uint("flag", 8), 0),
            when(at(uint("a", 16), 8), on("flag", "eq", json!(1))),
            when(at(uint("b", 8), 16), on("flag", "eq", json!(2)))
        ])),
        vec![]
    );
}

#[test]
fn exact_length_bounds_every_possible_end() {
    assert_eq!(
        problems(layout_with_length(
            json!([
                uint("x0", 64),
                uint("flag", 8),
                when(uint("opt", 16), on("flag", "eq", json!(1))),
                uint("x", 16)
            ]),
            json!({ "kind": "exact", "bytes": 12 })
        )),
        vec![(Code::OutOfBounds, entry(3, ""))]
    );
    // The same layout fits an at_least length.
    assert_eq!(
        problems(layout_with_length(
            json!([
                uint("x0", 64),
                uint("flag", 8),
                when(uint("opt", 16), on("flag", "eq", json!(1))),
                uint("x", 16)
            ]),
            json!({ "kind": "at_least", "bytes": 12 })
        )),
        vec![]
    );
}

#[test]
fn alignment_holds_for_every_possible_start() {
    let legacy = |types: Value, entries: Value| {
        problems(move |v| {
            v["sets"][2]["types"] = types;
            v["sets"][2]["packets"][0]["entries"] = entries;
            let packet = v["sets"][2]["packets"][0].as_object_mut().unwrap();
            packet.remove("length");
            packet.remove("time_field");
            v["sets"][2].as_object_mut().unwrap().remove("time");
        })
    };
    let path = |i: usize| format!("/sets/2/packets/0/entries/{i}");
    // A struct holding a little-endian u16, placed at bit 4.
    assert_eq!(
        legacy(
            json!({ "t": { "entries": [uint("a", 16)] } }),
            json!([uint("n", 4), { "kind": "struct", "name": "s", "struct": "t" }])
        ),
        vec![(Code::Alignment, path(1))]
    );
    // Three-bit little-endian elements: element 2 straddles a byte.
    assert_eq!(
        legacy(
            json!({}),
            json!([{ "kind": "array", "name": "a", "count": { "kind": "fixed", "count": 3 },
                     "element": { "kind": "scalar", "type": "uint", "size_bits": 3 } }])
        ),
        vec![(Code::Alignment, path(0))]
    );
    // A u16 that starts at bit 4 or 7 depending on an optional entry.
    assert_eq!(
        legacy(
            json!({}),
            json!([
                uint("flag", 4),
                when(uint("opt", 3), on("flag", "eq", json!(1))),
                uint("x", 16)
            ])
        ),
        vec![(Code::Alignment, path(2))]
    );
    // A big-endian struct holding a string, at bit 4.
    assert_eq!(
        problems(|v| {
            set_x(v)["types"]["text"] = json!({ "entries": [
                { "kind": "field", "name": "s", "type": "string",
                  "length": { "kind": "fixed", "bytes": 2 } }] });
            layout(json!([uint("n", 4), { "kind": "struct", "name": "s", "struct": "text" }]))(v);
        }),
        vec![(Code::Alignment, entry(1, ""))]
    );
}

#[test]
fn struct_types_dont_read_to_the_end() {
    assert_one(Code::ReadsToEnd, "/sets/0/types/blob/entries/1", |v| {
        set_x(v)["types"]["blob"] = json!({ "entries": [
            uint("len", 8),
            { "kind": "field", "name": "data", "type": "bytes", "length": { "kind": "remaining" } }
        ] });
    });
}

#[test]
fn reported_errors_dont_echo_onto_later_entries() {
    // Each mistake makes a size unknown; later explicit entries stay quiet.
    let followed = |broken: Value| one(json!([broken, at(uint("later", 8), 64)]));
    assert_eq!(
        followed(json!({ "kind": "struct", "name": "s", "struct": "nope" })),
        vec![(Code::UnknownType, entry(0, "/struct"))]
    );
    assert_eq!(
        followed(uint("wide", 65)),
        vec![(Code::FieldSize, entry(0, "/size_bits"))]
    );
    // A misspelled member is reported even when the param is wrong too.
    assert_eq!(
        problems(|v| {
            hk(v)["composites"][0] = json!({ "canonical": "eps.mode", "frame": "ECEF",
                "members": ["pos.x", "pos.yy", "pos.z"] });
        }),
        vec![
            (
                Code::CompositeShape,
                "/sets/0/packets/0/composites/0/canonical".to_string()
            ),
            (
                Code::UnknownEntry,
                "/sets/0/packets/0/composites/0/members/1".to_string()
            ),
        ]
    );
}

#[test]
fn condition_values_fit_the_field() {
    for (condition, path) in [
        (on("n", "eq", json!(300)), "/when/value"),
        (
            json!({ "kind": "in", "field": "n", "values": [1, 999] }),
            "/when/values/1",
        ),
    ] {
        assert_eq!(
            one(json!([uint("n", 8), when(uint("x", 8), condition)])),
            vec![(Code::ValueRange, entry(1, path))]
        );
    }
    let flag = json!({ "kind": "field", "name": "b", "type": "bool", "size_bits": 1 });
    assert_eq!(
        one(json!([
            flag,
            uint("pad", 7),
            when(uint("x", 8), on("b", "eq", json!(5)))
        ])),
        vec![(Code::ValueRange, entry(2, "/when/value"))]
    );
}

#[test]
fn lengths_and_counts_can_be_adjusted() {
    // CCSDS-style "octets minus one".
    let adjusted = json!({ "kind": "field", "name": "data", "type": "bytes",
        "length": { "kind": "field", "field": "len", "add": 1 } });
    assert_eq!(one(json!([uint("len", 8), adjusted])), vec![]);
    let with = |length: Value| {
        one(json!([uint("len", 8),
                   { "kind": "field", "name": "data", "type": "bytes", "length": length }]))
    };
    assert_eq!(
        with(json!({ "kind": "field", "field": "len", "multiply": 0 })),
        vec![(Code::FieldSize, entry(1, "/length/multiply"))]
    );
    assert_eq!(
        with(json!({ "kind": "field", "field": "len", "multiply": 1 })),
        vec![(Code::ExplicitDefault, entry(1, "/length/multiply"))]
    );
    assert_eq!(
        with(json!({ "kind": "field", "field": "len", "add": 0 })),
        vec![(Code::ExplicitDefault, entry(1, "/length/add"))]
    );
}

#[test]
fn mapping_rules_for_real_telemetry() {
    let mapped = |f: Value, param: Value| {
        problems(move |v| {
            v["catalog"]["params"]["a.p"] = param;
            layout(json!([f]))(v);
        })
    };
    // A byte-wide flag maps to a bool param.
    assert_eq!(
        mapped(
            json!({ "kind": "field", "name": "f", "type": "uint", "size_bits": 8,
                       "canonical": "a.p" }),
            json!({ "type": "bool" })
        ),
        vec![]
    );
    // Geodetic reaches ECI_J2000 through ECEF.
    assert_eq!(
        problems(|v| {
            v["catalog"]["params"]["nav.pos_eci"] =
                json!({ "type": "vec3", "unit": "m", "frame": "ECI_J2000" });
            set_x(v)["types"]["lla"] = json!({ "entries": [
                { "kind": "field", "name": "lat", "type": "int", "size_bits": 32, "unit": "deg" },
                { "kind": "field", "name": "lon", "type": "int", "size_bits": 32, "unit": "deg" },
                { "kind": "field", "name": "alt", "type": "int", "size_bits": 32, "unit": "m" }
            ] });
            hk(v)["entries"][6]["struct"] = json!("lla");
            hk(v)["composites"][0] = json!({ "canonical": "nav.pos_eci", "frame": "GEODETIC",
                "members": ["pos.lat", "pos.lon", "pos.alt"] });
        }),
        vec![]
    );
    // Two fields of one packet on one param.
    assert_eq!(
        problems(layout(json!([
            { "kind": "field", "name": "a", "type": "uint", "size_bits": 16, "unit": "V",
              "canonical": "eps.bus_voltage" },
            { "kind": "field", "name": "b", "type": "uint", "size_bits": 16, "unit": "V",
              "canonical": "eps.bus_voltage" }
        ]))),
        vec![(Code::DuplicateMapping, entry(1, "/canonical"))]
    );
    // One spelling for member indexes.
    assert_eq!(
        problems(|v| {
            hk(v)["entries"][5]["count"] = json!({ "kind": "fixed", "count": 3 });
            hk(v)["composites"][0] = json!({ "canonical": "nav.pos_ecef", "frame": "ECEF",
                "members": ["samples[0]", "samples[00]", "samples[+1]"] });
        })
        .into_iter()
        .filter(|(code, _)| *code == Code::UnknownEntry)
        .collect::<Vec<_>>(),
        vec![
            (
                Code::UnknownEntry,
                "/sets/0/packets/0/composites/0/members/1".to_string()
            ),
            (
                Code::UnknownEntry,
                "/sets/0/packets/0/composites/0/members/2".to_string()
            ),
        ]
    );
}

#[test]
fn little_endian_offsets_near_the_limit_dont_overflow() {
    let found = problems(|v| {
        v["sets"][2]["packets"][0]["entries"] = json!([
            { "kind": "array", "name": "a", "count": { "kind": "fixed", "count": 2147483648_u32 },
              "element": { "kind": "scalar", "type": "bytes",
                           "length": { "kind": "fixed", "bytes": 1073741823_u32 } } },
            { "kind": "padding", "size_bits": 4294967294_u32 },
            { "kind": "padding", "size_bits": 4294967294_u32 },
            { "kind": "padding", "size_bits": 4294967294_u32 },
            { "kind": "padding", "size_bits": 4294967294_u32 },
            uint("x", 16)
        ]);
        let packet = v["sets"][2]["packets"][0].as_object_mut().unwrap();
        packet.remove("length");
        packet.remove("time_field");
        v["sets"][2].as_object_mut().unwrap().remove("time");
    });
    assert!(
        found.iter().all(|(code, _)| *code == Code::Alignment),
        "{found:?}"
    );
}

// ---- Part 2, review round 2 ----

fn flags_array(count: u32, bits: u32, ty: &str) -> Value {
    json!({ "kind": "array", "name": "flags", "count": { "kind": "fixed", "count": count },
            "element": { "kind": "scalar", "type": ty, "size_bits": bits } })
}

#[test]
fn whole_byte_arrays_of_sub_byte_elements_keep_alignment() {
    // Eight one-bit flags then a string: the string starts on a byte.
    assert_eq!(
        one(json!([flags_array(8, 1, "bool"),
                   { "kind": "field", "name": "s", "type": "string",
                     "length": { "kind": "fixed", "bytes": 2 } }])),
        vec![]
    );
    let legacy = |entries: Value| {
        problems(move |v| {
            v["sets"][2]["packets"][0]["entries"] = entries;
            let packet = v["sets"][2]["packets"][0].as_object_mut().unwrap();
            packet.remove("length");
            packet.remove("time_field");
            v["sets"][2].as_object_mut().unwrap().remove("time");
        })
    };
    // Four 2-bit little-endian elements, then a little-endian u16.
    assert_eq!(
        legacy(json!([flags_array(4, 2, "uint"), uint("x", 16)])),
        vec![]
    );
    // Two 3-bit little-endian elements both fit inside byte 0.
    assert_eq!(legacy(json!([flags_array(2, 3, "uint")])), vec![]);
}

#[test]
fn struct_types_with_explicit_offsets_keep_alignment() {
    assert_eq!(
        problems(|v| {
            set_x(v)["types"]["rec"] = json!({ "entries": [
                at(uint("a", 8), 0),
                at(uint("n", 8), 8),
                at(string_from("s", "n"), 16)
            ] });
            layout(json!([{ "kind": "struct", "name": "r", "struct": "rec" },
                          { "kind": "field", "name": "t", "type": "string",
                            "length": { "kind": "fixed", "bytes": 2 } }]))(v);
        }),
        vec![]
    );
}

#[test]
fn containment_only_excuses_overlap_when_it_is_certain() {
    // c sits at bits 8..24 or 16..32; at the first placement it half
    // covers w at bits 20..28.
    assert_eq!(
        one(json!([
            at(uint("flag", 8), 0),
            when(uint("opt", 8), on("flag", "eq", json!(1))),
            when(uint("c", 16), on("flag", "eq", json!(2))),
            at(uint("w", 8), 20)
        ])),
        vec![(Code::Overlap, entry(3, ""))]
    );
}

#[test]
fn an_error_doesnt_hide_an_earlier_overlap() {
    assert_eq!(
        one(json!([
            at(uint("a", 16), 0),
            at(uint("b", 8), 8),
            uint("c", 65)
        ])),
        vec![
            (Code::Overlap, entry(1, "")),
            (Code::FieldSize, entry(2, "/size_bits")),
        ]
    );
}

#[test]
fn exact_length_flags_entries_without_a_bound() {
    assert_eq!(
        problems(layout_with_length(
            json!([uint("x0", 64),
                   { "kind": "field", "name": "t", "type": "string",
                     "length": { "kind": "terminator", "byte": 0 } },
                   uint("x", 32)]),
            json!({ "kind": "exact", "bytes": 12 })
        )),
        vec![(Code::OutOfBounds, entry(1, ""))],
        "x is unbounded only because t is, so only t is reported"
    );
    // A second entry with its own unbounded size is its own problem.
    assert_eq!(
        problems(layout_with_length(
            json!([uint("x0", 64),
                   { "kind": "field", "name": "t", "type": "string",
                     "length": { "kind": "terminator", "byte": 0 } },
                   { "kind": "field", "name": "u", "type": "string",
                     "length": { "kind": "terminator", "byte": 0 } }]),
            json!({ "kind": "exact", "bytes": 12 })
        )),
        vec![
            (Code::OutOfBounds, entry(1, "")),
            (Code::OutOfBounds, entry(2, ""))
        ]
    );
    // Reading to the end is bounded by the payload: fine.
    assert_eq!(
        problems(layout_with_length(
            json!([uint("x0", 64),
                   { "kind": "field", "name": "rest", "type": "bytes",
                     "length": { "kind": "remaining" } }]),
            json!({ "kind": "exact", "bytes": 12 })
        )),
        vec![]
    );
}

#[test]
fn duplicate_mappings_count_fields_inside_structs() {
    let found = problems(|v| {
        set_x(v)["types"]["batt"] = json!({ "entries": [
            { "kind": "field", "name": "volts", "type": "uint", "size_bits": 16, "unit": "V",
              "canonical": "eps.bus_voltage" }] });
        layout(
            json!([{ "kind": "struct", "name": "batt1", "struct": "batt" },
                      { "kind": "struct", "name": "batt2", "struct": "batt" }]),
        )(v);
    });
    assert_eq!(found, vec![(Code::DuplicateMapping, entry(1, ""))]);
    // Arrays of structs carry an index per sample, so they don't count.
    let indexed = problems(|v| {
        set_x(v)["types"]["batt"] = json!({ "entries": [
            { "kind": "field", "name": "volts", "type": "uint", "size_bits": 16, "unit": "V",
              "canonical": "eps.bus_voltage" }] });
        layout(
            json!([{ "kind": "array", "name": "batts", "count": { "kind": "fixed", "count": 2 },
                        "element": { "kind": "struct", "name": "batt" } }]),
        )(v);
    });
    assert_eq!(indexed, vec![]);
}

#[test]
fn duplicate_mappings_in_a_type_and_its_packet_are_separate_problems() {
    let found = problems(|v| {
        set_x(v)["types"]["batt"] = json!({ "entries": [
            { "kind": "field", "name": "a", "type": "uint", "size_bits": 16, "unit": "V",
              "canonical": "eps.bus_voltage" },
            { "kind": "field", "name": "b", "type": "uint", "size_bits": 16, "unit": "V",
              "canonical": "eps.bus_voltage" }] });
        layout(json!([
            { "kind": "field", "name": "volts", "type": "uint", "size_bits": 16, "unit": "V",
              "canonical": "eps.bus_voltage" },
            { "kind": "struct", "name": "batt1", "struct": "batt" }]))(v);
    });
    assert_eq!(
        found,
        vec![
            (Code::DuplicateMapping, entry(1, "")),
            (
                Code::DuplicateMapping,
                "/sets/0/types/batt/entries/1/canonical".to_string()
            ),
        ]
    );
}

#[test]
fn a_duplicate_inside_a_struct_type_is_described_as_one() {
    let mut value = fixture();
    set_x(&mut value)["types"]["batt"] = json!({ "entries": [
        { "kind": "field", "name": "a", "type": "uint", "size_bits": 16, "unit": "V",
          "canonical": "eps.bus_voltage" },
        { "kind": "field", "name": "b", "type": "uint", "size_bits": 16, "unit": "V",
          "canonical": "eps.bus_voltage" }] });
    let diagnostics = validate(bundle(value)).unwrap_err();
    let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].contains("of this struct type"),
        "{}",
        messages[0]
    );
}

#[test]
fn duplicate_mappings_are_counted_once_per_type() {
    use super::MAX_NESTING;
    // Each type holds two of the one below it: 2^31 paths down to t0's
    // field, which a walk into every struct entry would follow one by one.
    let found = problems(|v| {
        let types = &mut set_x(v)["types"];
        types["t0"] = json!({ "entries": [
            { "kind": "field", "name": "volts", "type": "uint", "size_bits": 16, "unit": "V",
              "canonical": "eps.bus_voltage" }] });
        for i in 1..MAX_NESTING {
            let below = format!("t{}", i - 1);
            types[format!("t{i}")] = json!({ "entries": [
                { "kind": "struct", "name": "a", "struct": below },
                { "kind": "struct", "name": "b", "struct": below }] });
        }
        let top = format!("t{}", MAX_NESTING - 1);
        layout(json!([{ "kind": "struct", "name": "top", "struct": top }]))(v);
    });
    // Document order puts t10..t19 between t1 and t2.
    let mut expected: Vec<(Code, String)> = (1..MAX_NESTING)
        .map(|i| {
            (
                Code::DuplicateMapping,
                format!("/sets/0/types/t{i}/entries/1"),
            )
        })
        .collect();
    expected.sort();
    assert_eq!(found, expected);
}

#[test]
fn optional_is_for_self_describing_entries_and_spelled_true() {
    assert_one(Code::WrongEncoding, &entry(0, "/optional"), |v| {
        hk(v)["entries"][0]["optional"] = json!(true);
    });
    fn bus(v: &mut Value) -> &mut Value {
        &mut v["sets"][1]["packets"][0]
    }
    assert_one(
        Code::ExplicitDefault,
        "/sets/1/packets/0/entries/0/optional",
        |v| bus(v)["entries"][0]["optional"] = json!(false),
    );
    assert_eq!(
        problems(|v| bus(v)["entries"][1]["optional"] = json!(true)),
        vec![]
    );
    // A condition can't use a field that may be missing.
    assert_one(
        Code::ReferenceType,
        "/sets/1/packets/0/entries/3/when/field",
        |v| {
            let entries = bus(v)["entries"].as_array_mut().unwrap();
            entries.push(json!({ "kind": "field", "name": "mode", "type": "string",
                                 "pointer": "/mode", "optional": true }));
            entries.push(json!({ "kind": "field", "name": "extra", "type": "int",
                                 "pointer": "/x",
                                 "when": { "kind": "compare", "field": "mode", "op": "eq",
                                           "value": "on" } }));
        },
    );
}

#[test]
fn clock_fields_are_unconditional_uints() {
    fn time_packet(v: &mut Value) -> &mut Value {
        &mut v["sets"][2]["packets"][0]
    }
    // boot_ms is both the packet's time_field and the correlation's
    // boot_field: two references, each needing a uint.
    assert_eq!(
        problems(|v| time_packet(v)["entries"][1]["type"] = json!("int")),
        vec![
            (
                Code::ReferenceType,
                "/sets/2/packets/0/time_field".to_string()
            ),
            (
                Code::ReferenceType,
                "/sets/2/time/correlate/boot_field".to_string()
            ),
        ]
    );
    assert_one(
        Code::ReferenceType,
        "/sets/2/time/correlate/unix_field",
        |v| {
            time_packet(v)["entries"][0]["when"] =
                json!({ "kind": "compare", "field": "boot_ms", "op": "gt", "value": 0 });
            // The condition reads a later field; move boot_ms first.
            let entries = time_packet(v)["entries"].as_array_mut().unwrap();
            entries.swap(0, 1);
        },
    );
}

// ---- Commands ----

fn command<'v>(v: &'v mut Value, name: &str) -> &'v mut Value {
    &mut v["catalog"]["commands"][name]
}

fn set_mode_packet(v: &mut Value) -> &mut Value {
    &mut set_x(v)["commands"][1]
}

#[test]
fn catalog_commands_are_well_formed() {
    assert_one(
        Code::ParamShape,
        "/catalog/commands/nav.point/arguments/0/type",
        |v| {
            *command(v, "nav.point") =
                json!({ "arguments": [{ "name": "target", "type": "vec3" }] });
        },
    );
    assert_one(Code::InvalidName, "/catalog/commands/Point", |v| {
        *command(v, "Point") = json!({});
    });
    assert_one(Code::DuplicateName, "/catalog/commands/eps.mode", |v| {
        *command(v, "eps.mode") = json!({});
    });
    assert_one(
        Code::DuplicateName,
        "/catalog/commands/eps.load_table/arguments/2/name",
        |v| {
            command(v, "eps.load_table")["arguments"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "name": "slot", "type": "u64", "default": 1 }));
        },
    );
}

#[test]
fn argument_ranges_and_defaults_fit_their_argument() {
    let delay = "/catalog/commands/eps.set_mode/arguments/1";
    let range = |v: &mut Value, r: Value| command(v, "eps.set_mode")["arguments"][1]["range"] = r;
    assert_one(Code::LimitsOrder, &format!("{delay}/range"), |v| {
        range(v, json!({ "min": 10, "max": 1 }))
    });
    assert_one(Code::Empty, &format!("{delay}/range"), |v| {
        range(v, json!({}))
    });
    assert_one(
        Code::ValueRange,
        "/catalog/commands/eps.load_table/arguments/1/range/max",
        |v| command(v, "eps.load_table")["arguments"][1]["range"]["max"] = json!(7.5),
    );
    assert_one(
        Code::ParamShape,
        "/catalog/commands/eps.set_mode/arguments/0/range",
        |v| command(v, "eps.set_mode")["arguments"][0]["range"] = json!({ "max": 1 }),
    );

    let default = |v: &mut Value, argument: usize, value: Value| {
        command(v, "eps.set_mode")["arguments"][argument]["default"] = value;
    };
    for bad in [json!("fast"), json!(5000), json!(true)] {
        assert_one(Code::DefaultValue, &format!("{delay}/default"), |v| {
            default(v, 1, bad.clone())
        });
    }
    assert_one(
        Code::DefaultValue,
        "/catalog/commands/eps.set_mode/arguments/0/default",
        |v| default(v, 0, json!("OFF")),
    );
    assert_eq!(problems(|v| default(v, 0, json!("SAFE"))), vec![]);
    assert_eq!(problems(|v| default(v, 1, json!(0.5))), vec![]);
    let slot = "/catalog/commands/eps.load_table/arguments/1/default";
    assert_one(Code::DefaultValue, slot, |v| {
        command(v, "eps.load_table")["arguments"][1]["default"] = json!(-1)
    });
    let table = "/catalog/commands/eps.load_table/arguments/0/default";
    assert_one(Code::DefaultValue, table, |v| {
        command(v, "eps.load_table")["arguments"][0]["default"] = json!("0G")
    });
    assert_eq!(
        problems(|v| command(v, "eps.load_table")["arguments"][0]["default"] = json!("0a1b")),
        vec![]
    );
}

#[test]
fn every_command_field_gets_its_value_from_one_place() {
    let field = |index: usize| format!("/sets/0/commands/1/entries/{index}");
    assert_one(Code::NoValue, &field(0), |v| {
        set_mode_packet(v)["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("value");
    });
    assert_one(Code::CommandShape, &field(2), |v| {
        set_mode_packet(v)["entries"][2]["value"] = json!(5);
    });
    assert_one(
        Code::UnknownArgument,
        &format!("{}/argument", field(1)),
        |v| {
            set_mode_packet(v)["entries"][1]["argument"] = json!("nope");
            set_mode_packet(v)["entries"].as_array_mut().unwrap().push(
                json!({ "kind": "field", "name": "mode2", "type": "uint", "size_bits": 8,
                          "argument": "mode",
                          "conversion": { "kind": "states",
                                          "states": [{ "raw": 0, "name": "SAFE" },
                                                     { "raw": 1, "name": "NOMINAL" }] } }),
            );
        },
    );
    assert_one(
        Code::DuplicateMapping,
        // After the packet's checksum, at 3.
        &format!("{}/argument", field(4)),
        |v| {
            set_mode_packet(v)["entries"].as_array_mut().unwrap().push(
                json!({ "kind": "field", "name": "mode2", "type": "uint", "size_bits": 8,
                          "argument": "mode",
                          "conversion": { "kind": "states",
                                          "states": [{ "raw": 0, "name": "SAFE" },
                                                     { "raw": 1, "name": "NOMINAL" }] } }),
            );
        },
    );
    assert_one(
        Code::UnmappedArgument,
        "/sets/0/commands/1/canonical",
        |v| {
            set_mode_packet(v)["entries"]
                .as_array_mut()
                .unwrap()
                .remove(1);
        },
    );
    // An argument with a default may be left out, as by a layout from
    // before the argument existed; the encoder then sends only its default.
    assert_eq!(
        problems(|v| {
            set_mode_packet(v)["entries"].as_array_mut().unwrap().pop();
        }),
        vec![]
    );
    assert_one(Code::UnknownCommand, "/sets/0/commands/1/canonical", |v| {
        set_mode_packet(v)["canonical"] = json!("eps.reboot");
    });
    assert_one(Code::ValueRange, &format!("{}/value", field(0)), |v| {
        set_mode_packet(v)["entries"][0]["value"] = json!(300);
    });
    assert_one(
        Code::ReferenceType,
        "/sets/1/commands/0/entries/0/value",
        |v| v["sets"][1]["commands"][0]["entries"][0]["value"] = json!(1),
    );
    assert_one(
        Code::UnknownMetadata,
        "/sets/0/commands/1/metadata/sysid",
        |v| {
            set_mode_packet(v)["metadata"]["sysid"] = json!(1);
        },
    );
}

#[test]
fn an_argument_fits_the_field_that_encodes_it() {
    let delay = "/sets/0/commands/1/entries/2";
    assert_one(Code::ConversionType, &format!("{delay}/conversion"), |v| {
        set_mode_packet(v)["entries"][2]
            .as_object_mut()
            .unwrap()
            .remove("inverse");
    });
    assert_one(Code::ConversionType, &format!("{delay}/inverse"), |v| {
        set_mode_packet(v)["entries"][2]["conversion"] =
            json!({ "kind": "linear", "scale": 0.1, "offset": 0.0 });
    });
    assert_one(Code::UnitMismatch, &format!("{delay}/unit"), |v| {
        set_mode_packet(v)["entries"][2]["unit"] = json!("ms");
    });
    assert_one(
        Code::CanonicalType,
        "/sets/0/commands/1/entries/1/argument",
        |v| {
            let mode = &mut set_mode_packet(v)["entries"][1];
            mode["type"] = json!("float");
            mode["size_bits"] = json!(32);
            mode.as_object_mut().unwrap().remove("conversion");
        },
    );
    assert_one(
        Code::CanonicalType,
        "/sets/0/commands/1/entries/1/conversion/states/1",
        |v| {
            set_mode_packet(v)["entries"][1]["conversion"]["states"][1]["name"] = json!("ON");
        },
    );
}

#[test]
fn command_packets_and_telemetry_keep_to_their_own_keys() {
    assert_one(Code::CommandShape, "/sets/0/commands/1/entries/4", |v| {
        set_mode_packet(v)["entries"].as_array_mut().unwrap().push(
            json!({ "kind": "array", "name": "pad",
                          "count": { "kind": "fixed", "count": 1 },
                          "element": { "kind": "scalar", "type": "uint", "size_bits": 8 } }),
        );
    });
    // Each is one mistake with one diagnostic, though the shared field
    // checks would otherwise look at these keys too.
    for (key, value) in [
        ("canonical", json!("eps.mode")),
        ("canonical", json!("no.such")),
        ("invalid", json!([255])),
        ("invalid", json!([999])),
        ("optional", json!(true)),
    ] {
        assert_one(
            Code::CommandShape,
            &format!("/sets/0/commands/1/entries/0/{key}"),
            |v| set_mode_packet(v)["entries"][0][key] = value.clone(),
        );
    }
    assert_one(Code::CommandOnly, &entry(0, "/value"), |v| {
        hk(v)["entries"][0]["value"] = json!(1);
    });
    assert_one(Code::CommandOnly, &entry(0, "/argument"), |v| {
        hk(v)["entries"][0]["argument"] = json!("mode");
    });
}

fn load_table_packet(v: &mut Value) -> &mut Value {
    &mut set_x(v)["commands"][0]
}

#[test]
fn every_command_packet_encodes_one_way() {
    assert_one(
        Code::DuplicateMapping,
        "/sets/0/commands/2/canonical",
        |v| {
            let mut copy = set_mode_packet(v).clone();
            copy["name"] = json!("SET_MODE_AGAIN");
            set_x(v)["commands"].as_array_mut().unwrap().push(copy);
        },
    );
    assert_one(
        Code::CommandShape,
        "/sets/0/commands/1/entries/1/when",
        |v| {
            set_mode_packet(v)["entries"][1]["when"] =
                json!({ "kind": "compare", "field": "opcode", "op": "eq", "value": 26 });
        },
    );
    assert_one(
        Code::ConversionType,
        "/sets/0/commands/1/entries/2/conversion/scale",
        |v| {
            let delay = &mut set_mode_packet(v)["entries"][2];
            delay["conversion"] = json!({ "kind": "linear", "scale": 0.0, "offset": 0.0 });
            delay.as_object_mut().unwrap().remove("inverse");
        },
    );
    assert_one(
        Code::DuplicateValue,
        "/sets/0/commands/1/entries/1/conversion/states/2",
        |v| {
            set_mode_packet(v)["entries"][1]["conversion"]["states"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "raw": 2, "name": "SAFE" }));
        },
    );
    // Two data fields can't share one length field.
    assert_one(
        Code::CommandShape,
        "/sets/0/commands/0/entries/5/length/field",
        |v| {
            command(v, "eps.load_table")["arguments"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "name": "extra", "type": "bytes", "default": "" }));
            load_table_packet(v)["entries"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "kind": "field", "name": "extra", "type": "bytes",
                              "length": { "kind": "field", "field": "len" },
                              "argument": "extra" }));
        },
    );
}

#[test]
fn a_set_encodes_every_value_the_catalog_allows() {
    // Every set that encodes the command is told.
    assert_eq!(
        problems(|v| {
            command(v, "eps.set_mode")["arguments"][0]["states"] =
                json!(["SAFE", "NOMINAL", "SCIENCE"]);
        }),
        vec![
            (
                Code::CanonicalType,
                "/sets/0/commands/1/entries/1/conversion".to_string()
            ),
            (
                Code::CanonicalType,
                "/sets/2/commands/0/entries/0/conversion".to_string()
            ),
        ]
    );
    let slot = "/sets/0/commands/0/entries/1/argument";
    assert_one(Code::ValueRange, slot, |v| {
        command(v, "eps.load_table")["arguments"][1]["range"]["max"] = json!(1000);
    });
    // Without a range an argument promises every value of its type.
    assert_one(Code::ValueRange, slot, |v| {
        command(v, "eps.load_table")["arguments"][1]
            .as_object_mut()
            .unwrap()
            .remove("range");
    });
    // A linear conversion maps the range: 0 to 7 at 0.5 per count is raw
    // 0 to 14, which fits.
    assert_eq!(
        problems(|v| {
            load_table_packet(v)["entries"][1]["conversion"] =
                json!({ "kind": "linear", "scale": 0.5, "offset": 0.0 });
            let slot = &mut command(v, "eps.load_table")["arguments"][1];
            slot["type"] = json!("f64");
            slot["range"]["min"] = json!(0);
        }),
        vec![]
    );
}

#[test]
fn a_new_argument_with_a_default_keeps_sets_valid() {
    assert_eq!(
        problems(|v| {
            command(v, "eps.load_table")["arguments"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "name": "verify", "type": "bool", "default": false }));
        }),
        vec![]
    );
}

fn legacy_set_mode(v: &mut Value) -> &mut Value {
    &mut v["sets"][2]["commands"][0]
}

fn target_field(ty: &str) -> Value {
    let mut field = json!({ "kind": "field", "name": "target", "type": ty, "target": true });
    match ty {
        "string" => {}
        "float" => field["size_bits"] = json!(32),
        _ => field["size_bits"] = json!(8),
    }
    field
}

/// The legacy SET_MODE's target field (entries/1), replaced.
fn insert_target(v: &mut Value, field: Value) {
    legacy_set_mode(v)["entries"][1] = field;
}

#[test]
fn a_command_field_may_hold_the_targets_native_id() {
    assert_eq!(problems(|v| insert_target(v, target_field("uint"))), vec![]);
    // In a JSON payload too, as text.
    assert_eq!(
        problems(|v| {
            v["sets"][1]["commands"][0]["entries"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "kind": "field", "name": "device", "type": "string",
                              "pointer": "/device", "target": true }));
        }),
        vec![]
    );
}

#[test]
fn a_target_field_holds_the_id_as_it_is() {
    let at = |rest: &str| format!("/sets/2/commands/0/entries/1{rest}");
    assert_one(Code::CommandShape, &at("/target"), |v| {
        insert_target(v, target_field("float"));
    });
    assert_one(Code::CommandShape, &at("/conversion"), |v| {
        let mut field = target_field("uint");
        field["conversion"] = json!({ "kind": "linear", "scale": 2.0, "offset": 0.0 });
        insert_target(v, field);
    });
    assert_one(Code::CommandShape, &at("/unit"), |v| {
        let mut field = target_field("uint");
        field["unit"] = json!("count");
        insert_target(v, field);
    });
    assert_one(Code::CommandShape, &at(""), |v| {
        let mut field = target_field("uint");
        field["value"] = json!(1);
        insert_target(v, field);
    });
    // false is the default, and the field still needs a value.
    assert_eq!(
        problems(|v| {
            let mut field = target_field("uint");
            field["target"] = json!(false);
            insert_target(v, field);
        }),
        vec![
            (Code::NoValue, at("")),
            (Code::ExplicitDefault, at("/target")),
        ]
    );
    assert_one(
        Code::CommandOnly,
        "/sets/2/packets/0/entries/0/target",
        |v| {
            v["sets"][2]["packets"][0]["entries"][0]["target"] = json!(true);
        },
    );
}

#[test]
fn a_command_packet_holds_the_target_once() {
    // A second id could disagree with the first, and decoding would have
    // to pick one.
    assert_one(
        Code::CommandShape,
        "/sets/2/commands/0/entries/2/target",
        |v| {
            legacy_set_mode(v)["entries"]
                .as_array_mut()
                .unwrap()
                .insert(0, target_field("uint"));
        },
    );
}

#[test]
fn a_sets_command_packets_can_be_told_apart() {
    // LOAD_TABLE's opcode is 3, and both have apid 300.
    assert_one(Code::AmbiguousIdentify, "/sets/0/commands/1", |v| {
        set_mode_packet(v)["entries"][0]["value"] = json!(3);
    });
    // Constants compare as the bits they write: int8 -1 is 0xFF, which
    // differs from 3 and equals uint8 255.
    let opcode_minus_one = |v: &mut Value| {
        set_mode_packet(v)["entries"][0]["type"] = json!("int");
        set_mode_packet(v)["entries"][0]["value"] = json!(-1);
    };
    assert_eq!(problems(opcode_minus_one), vec![]);
    assert_one(Code::AmbiguousIdentify, "/sets/0/commands/1", |v| {
        opcode_minus_one(v);
        load_table_packet(v)["entries"][0]["value"] = json!(255);
    });
    // Metadata tells them apart as well as a field does.
    assert_eq!(
        problems(|v| {
            set_mode_packet(v)["entries"][0]["value"] = json!(3);
            set_mode_packet(v)["metadata"]["apid"] = json!(301);
        }),
        vec![]
    );
    // In a JSON payload, by a constant at the same pointer.
    let reset = |op: &'static str| {
        move |v: &mut Value| {
            v["catalog"]["commands"]["eps.reset"] = json!({});
            v["sets"][1]["commands"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "name": "RESET",
                    "canonical": "eps.reset",
                    "entries": [{ "kind": "field", "name": "op", "type": "string",
                                  "pointer": "/op", "value": op }]
                }));
        }
    };
    assert_one(
        Code::AmbiguousIdentify,
        "/sets/1/commands/1",
        reset("set_mode"),
    );
    assert_eq!(problems(reset("reset")), vec![]);
}

#[test]
fn each_field_of_a_json_command_is_written_at_its_own_place() {
    let push = |v: &mut Value, pointer: &str| {
        v["sets"][1]["commands"][0]["entries"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "kind": "field", "name": "extra", "type": "string",
                          "pointer": pointer, "value": "x" }));
    };
    let at = "/sets/1/commands/0/entries/3/pointer";
    assert_one(Code::Overlap, at, |v| push(v, "/args"));
    assert_one(Code::Overlap, at, |v| push(v, "/op/more"));
    assert_one(Code::Overlap, at, |v| push(v, "/op"));
    // Tokens compare unescaped: "/a~1b" is the key "a/b", not a path.
    assert_eq!(problems(|v| push(v, "/args~1mode")), vec![]);
    assert_eq!(problems(|v| push(v, "/opx")), vec![]);
    // The payload is an object, so no field is the whole document.
    assert_one(Code::CommandShape, at, |v| push(v, ""));
}

#[test]
fn coverage_compares_raw_values_exactly_at_the_top_of_64_bits() {
    // u64::MAX as a double is 2^64, so a double comparison would let raw
    // 2^64 through.
    assert_one(
        Code::ValueRange,
        "/sets/0/commands/1/entries/2/argument",
        |v| {
            command(v, "eps.set_mode")["arguments"][1]["range"]["max"] = json!(1);
            set_mode_packet(v)["entries"][2] = json!({
                "kind": "field", "name": "delay", "type": "uint", "size_bits": 64, "unit": "s",
                "conversion": { "kind": "linear", "scale": 5.421_010_862_427_522e-20, "offset": 0.0 },
                "argument": "delay_s"
            });
        },
    );
}

#[test]
fn a_string_constant_fits_its_field() {
    let tag = |v: &mut Value, text: &str, length: Value| {
        set_mode_packet(v)["entries"]
            .as_array_mut()
            .unwrap()
            .insert(
                1,
                json!({ "kind": "field", "name": "tag", "type": "string",
                               "text": "ascii", "length": length, "value": text }),
            );
    };
    let at = "/sets/0/commands/1/entries/1/value";
    assert_one(Code::ValueRange, at, |v| {
        tag(v, "PIN", json!({ "kind": "fixed", "bytes": 4 }));
    });
    assert_one(Code::ValueRange, at, |v| {
        tag(v, "A;B", json!({ "kind": "terminator", "byte": 59 }));
    });
    assert_one(Code::ReferenceType, at, |v| {
        tag(v, "PÏN", json!({ "kind": "fixed", "bytes": 4 }));
    });
}

#[test]
fn text_tags_tell_command_packets_apart_by_their_bytes() {
    // Both packets start with a 4-byte tag in place of an opcode.
    let tagged = |v: &mut Value, load: &str, set: &str| {
        for (index, text) in [(0, load), (1, set)] {
            v["sets"][0]["commands"][index]["entries"][0] = json!({
                "kind": "field", "name": "opcode", "type": "string", "text": "ascii",
                "length": { "kind": "fixed", "bytes": 4 }, "value": text });
        }
    };
    assert_eq!(problems(|v| tagged(v, "LOAD", "MODE")), vec![]);
    assert_one(Code::AmbiguousIdentify, "/sets/0/commands/1", |v| {
        tagged(v, "MODE", "MODE");
    });
}

#[test]
fn a_polynomial_inverse_is_checked_over_its_arguments_range() {
    let delay = "/sets/0/commands/1/entries/2";
    // The range bounds the check, so it needs both ends.
    assert_one(Code::ValueRange, &format!("{delay}/argument"), |v| {
        command(v, "eps.set_mode")["arguments"][1]["range"]
            .as_object_mut()
            .unwrap()
            .remove("max");
    });
    // 10x - 0.01x^3 peaks near 18.3 s, inside 0 to 3600 s: a raw value
    // could come from two arguments.
    assert_one(Code::ConversionType, &format!("{delay}/inverse"), |v| {
        set_mode_packet(v)["entries"][2]["inverse"] =
            json!({ "kind": "poly", "coefficients": [0, 10, 0, -0.01] });
    });
    // Through the inverse, 3600 s is 0.7 + 36000 counts: a u16 holds it,
    // but 6553.5 s is raw 65535.7, written 65536, which doesn't fit.
    assert_eq!(
        problems(|v| {
            set_mode_packet(v)["entries"][2]["inverse"] =
                json!({ "kind": "poly", "coefficients": [0.7, 10] });
        }),
        vec![]
    );
    assert_one(Code::ValueRange, &format!("{delay}/argument"), |v| {
        set_mode_packet(v)["entries"][2]["inverse"] =
            json!({ "kind": "poly", "coefficients": [0.7, 10] });
        command(v, "eps.set_mode")["arguments"][1]["range"]["max"] = json!(6553.5);
    });
}

fn thermal(v: &mut Value) -> &mut Value {
    &mut v["sets"][1]["packets"][1]
}

#[test]
fn tables_and_splines_have_sorted_finite_points() {
    let heater = "/sets/1/packets/1/entries/0/conversion";
    assert_one(Code::Empty, &format!("{heater}/points"), |v| {
        thermal(v)["entries"][0]["conversion"]["points"] = json!([{ "raw": 0, "value": 1 }]);
    });
    assert_one(Code::NotSorted, &format!("{heater}/points/2/raw"), |v| {
        thermal(v)["entries"][0]["conversion"]["points"][2]["raw"] = json!(100);
    });
    assert_one(
        Code::ExplicitDefault,
        &format!("{heater}/interpolation"),
        |v| {
            thermal(v)["entries"][0]["conversion"]["interpolation"] = json!("linear");
        },
    );
    assert_one(
        Code::ExplicitDefault,
        &format!("{heater}/extrapolate"),
        |v| {
            thermal(v)["entries"][0]["conversion"]["extrapolate"] = json!(false);
        },
    );
    assert_one(Code::ConversionType, heater, |v| {
        thermal(v)["entries"][0]["type"] = json!("string");
    });
    assert_one(
        Code::NotSorted,
        "/sets/1/packets/1/entries/1/conversion/points/1/raw",
        |v| {
            thermal(v)["entries"][1]["conversion"]["points"][1]["raw"] = json!(0);
        },
    );
}

#[test]
fn an_argument_goes_through_a_table_or_spline_by_an_exact_or_explicit_inverse() {
    let delay = "/sets/0/commands/1/entries/2";
    let points = |pairs: &[(f64, f64)]| {
        Value::Array(
            pairs
                .iter()
                .map(|(raw, value)| json!({ "raw": raw, "value": value }))
                .collect(),
        )
    };
    // A linear table whose values only rise reads backwards exactly.
    let table = |v: &mut Value, pairs: &[(f64, f64)]| {
        let field = &mut set_mode_packet(v)["entries"][2];
        field["conversion"] = json!({ "kind": "table", "points": points(pairs) });
        field.as_object_mut().unwrap().remove("inverse");
    };
    let rising = [(0.0, 0.0), (36000.0, 3600.0)];
    assert_eq!(problems(|v| table(v, &rising)), vec![]);
    assert_one(Code::ConversionType, &format!("{delay}/inverse"), |v| {
        table(v, &rising);
        set_mode_packet(v)["entries"][2]["inverse"] =
            json!({ "kind": "poly", "coefficients": [0, 10] });
    });
    // The range must stay inside the points (or the table extrapolate).
    assert_one(Code::ValueRange, &format!("{delay}/argument"), |v| {
        table(v, &[(0.0, 0.0), (10000.0, 1000.0)]);
    });
    assert_eq!(
        problems(|v| {
            table(v, &[(0.0, 0.0), (10000.0, 1000.0)]);
            set_mode_packet(v)["entries"][2]["conversion"]["extrapolate"] = json!(true);
        }),
        vec![]
    );
    // A step table doesn't read backwards: it needs an inverse.
    assert_one(Code::ConversionType, &format!("{delay}/conversion"), |v| {
        table(v, &rising);
        set_mode_packet(v)["entries"][2]["conversion"]["interpolation"] = json!("step");
    });
    // A spline with an explicit inverse table, which must only rise or
    // only fall over the range.
    let spline = |v: &mut Value, inverse: &[(f64, f64)]| {
        let field = &mut set_mode_packet(v)["entries"][2];
        field["conversion"] = json!({ "kind": "spline",
            "points": points(&[(0.0, 0.0), (18000.0, 2000.0), (36000.0, 3600.0)]) });
        field["inverse"] = json!({ "kind": "table", "points": points(inverse) });
    };
    assert_eq!(
        problems(|v| spline(v, &[(0.0, 0.0), (2000.0, 18000.0), (3600.0, 36000.0)])),
        vec![]
    );
    assert_one(Code::ConversionType, &format!("{delay}/inverse"), |v| {
        spline(v, &[(0.0, 0.0), (2000.0, 30000.0), (3600.0, 20000.0)]);
    });
    assert_one(Code::ConversionType, &format!("{delay}/inverse"), |v| {
        spline(v, &[(0.0, 0.0), (3600.0, 36000.0)]);
        set_mode_packet(v)["entries"][2]["inverse"] =
            json!({ "kind": "states", "states": [{ "raw": 0, "name": "SAFE" }] });
    });
}

#[test]
fn the_raw_values_a_range_encodes_to_lie_inside_the_points() {
    // Each would write a raw value just outside the points at a range end,
    // which decoding couldn't read back.
    let argument = "/sets/0/commands/1/entries/2/argument";
    let delay = |v: &mut Value, field: Value, max: f64| {
        set_mode_packet(v)["entries"][2] = field;
        command(v, "eps.set_mode")["arguments"][1]["range"]["max"] = json!(max);
    };
    let points = |pairs: &[(f64, f64)]| {
        Value::Array(
            pairs
                .iter()
                .map(|(raw, value)| json!({ "raw": raw, "value": value }))
                .collect(),
        )
    };
    // An approximate inverse: 0 s is raw -5, below the spline's first point.
    assert_one(Code::ValueRange, argument, |v| {
        let field = json!({ "kind": "field", "name": "delay", "type": "int", "size_bits": 16,
            "unit": "s", "argument": "delay_s",
            "conversion": { "kind": "spline",
                "points": points(&[(0.0, 0.0), (50.0, 5.0), (100.0, 10.0)]) },
            "inverse": { "kind": "linear", "scale": 10.5, "offset": -5.0 } });
        delay(v, field, 10.0);
    });
    // Rounding: 100 s is raw 100.5, written 101, past the last point.
    assert_one(Code::ValueRange, argument, |v| {
        let field = json!({ "kind": "field", "name": "delay", "type": "uint", "size_bits": 16,
            "unit": "s", "argument": "delay_s",
            "conversion": { "kind": "table",
                "points": points(&[(0.5, 0.0), (100.5, 100.0)]) } });
        delay(v, field, 100.0);
    });
    // Binary32: 0 s is raw 0.7, written as 0.699999988, below 0.7.
    assert_one(Code::ValueRange, argument, |v| {
        let field = json!({ "kind": "field", "name": "delay", "type": "float", "size_bits": 32,
            "unit": "s", "argument": "delay_s",
            "conversion": { "kind": "table",
                "points": points(&[(0.7, 0.0), (2.0, 10.0)]) } });
        delay(v, field, 10.0);
    });
    // Extrapolating, the raw values may go past the points.
    assert_eq!(
        problems(|v| {
            let field = json!({ "kind": "field", "name": "delay", "type": "float",
                "size_bits": 32, "unit": "s", "argument": "delay_s",
                "conversion": { "kind": "table", "extrapolate": true,
                    "points": points(&[(0.7, 0.0), (2.0, 10.0)]) } });
            delay(v, field, 10.0);
        }),
        vec![]
    );
}

#[test]
fn an_inverse_table_is_checked_at_every_point_not_by_sampling() {
    // 150 is a typo for 99.992, between two of a polynomial's samples
    // (9.99756 and 10): a table is checked at its points, so it's caught.
    let points = |pairs: &[(f64, f64)]| {
        Value::Array(
            pairs
                .iter()
                .map(|(raw, value)| json!({ "raw": raw, "value": value }))
                .collect(),
        )
    };
    let pump = |v: &mut Value, inverse: &[(f64, f64)]| {
        set_mode_packet(v)["entries"][2] = json!({ "kind": "field", "name": "delay",
            "type": "uint", "size_bits": 8, "unit": "s", "argument": "delay_s",
            "conversion": { "kind": "spline",
                "points": points(&[(0.0, 0.0), (100.0, 10.0), (200.0, 20.0)]) },
            "inverse": { "kind": "table", "points": points(inverse) } });
        command(v, "eps.set_mode")["arguments"][1]["range"]["max"] = json!(10);
    };
    let typo = [
        (0.0, 0.0),
        (9.9991, 99.991),
        (9.9992, 150.0),
        (9.9993, 99.993),
        (10.0, 100.0),
    ];
    assert_one(
        Code::ConversionType,
        "/sets/0/commands/1/entries/2/inverse",
        |v| pump(v, &typo),
    );
    let fixed = [
        (0.0, 0.0),
        (9.9991, 99.991),
        (9.9992, 99.992),
        (9.9993, 99.993),
        (10.0, 100.0),
    ];
    assert_eq!(problems(|v| pump(v, &fixed)), vec![]);
}

#[test]
fn a_spline_with_an_infinite_slope_is_refused() {
    assert_one(
        Code::NonFinite,
        "/sets/1/packets/1/entries/1/conversion/points",
        |v| {
            thermal(v)["entries"][1]["conversion"]["points"] = json!([
                { "raw": 0, "value": 0 }, { "raw": 1e-300, "value": 1e9 }, { "raw": 2, "value": 0 }
            ]);
        },
    );
}

#[test]
fn command_identities_are_constants_a_decoder_reads_directly() {
    use super::command::command_identities;
    use super::{KeyId, KeyValue};
    let mut value = fixture();
    // A constant after the variable-length table has no static position.
    load_table_packet(&mut value)["entries"]
        .as_array_mut()
        .unwrap()
        .insert(
            4,
            json!({ "kind": "field", "name": "marker", "type": "uint",
                           "size_bits": 8, "value": 255 }),
        );
    let validated = validate(bundle(value)).unwrap();
    let bundle = validated.bundle();
    let identities = command_identities(&bundle.sets[0], &bundle.families[0]);
    let opcode = KeyId::Bits {
        offset: 0,
        size: 8,
        signed: false,
    };
    let apid = KeyId::Metadata("apid".to_string());
    assert_eq!(
        identities,
        vec![
            [
                (apid.clone(), KeyValue::Int(300)),
                (opcode.clone(), KeyValue::Int(3))
            ]
            .into(),
            [(apid, KeyValue::Int(300)), (opcode, KeyValue::Int(26))].into(),
        ]
    );
}

// ---- Checksums ----

fn crc16() -> Value {
    json!({ "kind": "checksum", "name": "crc",
            "algorithm": { "kind": "crc", "width": 16, "poly": 4129, "init": 65535,
                           "reflect_in": false, "reflect_out": false, "xor_out": 0 } })
}

#[test]
fn checksums_are_computable_as_written() {
    let crc_at = |v: &mut Value| -> usize {
        let entries = load_table_packet(v)["entries"].as_array().unwrap();
        entries.len() - 1
    };
    let algorithm = |v: &mut Value, key: &str, value: Value| {
        let at = crc_at(v);
        load_table_packet(v)["entries"][at]["algorithm"][key] = value;
    };
    let path = |rest: &str| format!("/sets/0/commands/0/entries/4{rest}");
    assert_one(Code::Checksum, &path("/algorithm/width"), |v| {
        algorithm(v, "width", json!(12))
    });
    assert_one(Code::Checksum, &path("/algorithm/poly"), |v| {
        algorithm(v, "poly", json!(65536))
    });
    assert_one(Code::IntegerAsString, &path("/algorithm/init"), |v| {
        algorithm(v, "init", json!("65535"))
    });
    assert_one(Code::Alignment, &path("/from_bits"), |v| {
        let at = crc_at(v);
        load_table_packet(v)["entries"][at]["from_bits"] = json!(4);
    });
    // A checksum covers bytes before its own start.
    assert_one(
        Code::Checksum,
        "/sets/2/packets/0/entries/2/from_bits",
        |v| {
            let legacy = &mut v["sets"][2]["packets"][0];
            legacy["entries"].as_array_mut().unwrap().push(json!(
            { "kind": "checksum", "name": "xor", "offset_bits": 96, "from_bits": 104,
              "algorithm": { "kind": "xor" } }));
            legacy["length"]["bytes"] = json!(13);
        },
    );
}

#[test]
fn checksums_sit_at_a_binary_packets_top_level() {
    assert_one(Code::Checksum, "/sets/0/types/vec3_i32/entries/3", |v| {
        set_x(v)["types"]["vec3_i32"]["entries"]
            .as_array_mut()
            .unwrap()
            .push(crc16());
    });
    assert_one(Code::WrongEncoding, "/sets/1/packets/0/entries/2", |v| {
        v["sets"][1]["packets"][0]["entries"]
            .as_array_mut()
            .unwrap()
            .push(crc16());
    });
    // Telemetry may end with one too.
    assert_eq!(
        problems(|v| {
            let legacy = &mut v["sets"][2]["packets"][0];
            legacy["entries"].as_array_mut().unwrap().push(json!(
                { "kind": "checksum", "name": "xor", "algorithm": { "kind": "xor" } }));
            legacy["length"]["bytes"] = json!(13);
        }),
        vec![]
    );
}
