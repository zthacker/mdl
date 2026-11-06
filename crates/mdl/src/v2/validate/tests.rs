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
    assert_one(Code::InvalidName, "/sets/0/packets/0/entries/0/name", |v| {
        hk(v)["entries"][0]["name"] = json!("bus.counts");
    });
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
    assert_one(Code::NestingDepth, "/sets/0/types/t0", |v| chain(v, 50_000));

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
    assert_eq!(found, vec![]);
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
