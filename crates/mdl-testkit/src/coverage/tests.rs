use super::features;

#[test]
fn features_are_named_by_what_holds_them() {
    let value = serde_json::json!({
        "catalog": { "params": { "a": { "type": "i64", "frame": "NED", "deprecated": "x",
                                         "limits": { "yellow_low": 1.0 } } } },
        "families": [{ "alias": { "kind": "metadata", "name": "id" }, "byte_order": "big",
                       "metadata": ["id"] }],
        "sets": [{ "time": { "kind": "elapsed", "unit": "ns", "scale": "tt" },
                   "packets": [{ "entries": [
                       { "kind": "field", "type": "uint", "byte_order": "little",
                         "invalid": ["9007199254740992"],
                         "length": { "kind": "fixed", "bytes": 2, "terminator": 0 },
                         "when": { "kind": "any", "conditions": [
                             { "kind": "compare", "op": "mask_all" }] } },
                       { "kind": "checksum", "algorithm": { "kind": "crc", "reflect_in": true } }
                   ] }] }]
    });
    let found = features(&value);
    for feature in [
        "param_type:i64",
        "frame:NED",
        "key:param.deprecated",
        "key:limits.yellow_low",
        "locator:metadata",
        "key:family.metadata",
        "byte_order:big",
        "time:elapsed",
        "time_unit:ns",
        "scale:tt",
        "entry:field",
        "field_type:uint",
        "key:entry.byte_order",
        "scalar:string_integer",
        "length:fixed/terminator",
        "condition:any",
        "condition:compare",
        "op:mask_all",
        "checksum:crc/reflected",
    ] {
        assert!(found.contains(feature), "{feature}: {found:?}");
    }
    assert!(!found.contains("param_type:uint"), "{found:?}");
}

/// Keys that also appear outside entries, kinds that also appear as
/// inverses, empty collections and names people choose don't count.
#[test]
fn look_alikes_are_not_features() {
    let value = serde_json::json!({
        "catalog": { "params": { "value": { "type": "f64" } } },
        "families": [
            { "alias": { "kind": "fixed", "value": "x" } },
            { "alias": { "kind": "bits", "offset_bits": 0 } },
            { "alias": { "kind": "path", "pointer": "/id" } }
        ],
        "sets": [{
            "types": {},
            "time": { "kind": "cuc", "offset_bits": 0 },
            "commands": [{ "metadata": { "target": 1 }, "canonical": "a.b", "entries": [] }],
            "packets": [{
                "identify": { "range": 1, "list": 2 },
                "composites": [],
                "entries": [
                    { "kind": "field", "invalid": [], "optional": false,
                      "inverse": { "kind": "poly" },
                      "when": { "kind": "compare", "value": "9007199254740991" } }
                ]
            }]
        }]
    });
    let found = features(&value);
    for feature in [
        "key:entry.value",
        "key:entry.offset_bits",
        "key:entry.pointer",
        "key:entry.canonical",
        "key:entry.target",
        "key:entry.invalid",
        "key:entry.optional",
        "key:argument.range",
        "key:argument.list",
        "key:set.types",
        "key:packet.composites",
        "conversion:poly",
        "scalar:string_integer",
    ] {
        assert!(!found.contains(feature), "{feature}: {found:?}");
    }
    assert!(found.contains("inverse:poly"), "{found:?}");
    // A param, an identify key and a metadata name called after IR keys
    // name nothing; `value` is only where the IR holds it.
    let named = |suffix: &str| -> Vec<&str> {
        found
            .iter()
            .map(String::as_str)
            .filter(|f| f.ends_with(suffix))
            .collect()
    };
    assert_eq!(
        named(".value"),
        ["key:condition.value", "key:locator.value"]
    );
    for suffix in [".range", ".list", ".target"] {
        assert_eq!(named(suffix), Vec::<&str>::new(), "{suffix}");
    }
}
