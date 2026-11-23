use mdl::artifact::{Written, build};
use mdl::bindings::Bindings;
use mdl::decode::Value as Raw;
use mdl::encode::CommandRequest;
use mdl::message::{Message, Payload};
use mdl::read::read_artifact;
use mdl::route::FamilySets;
use mdl::source::{Parsed, Severity, SourceDiagnostic, Sources};
use mdl::validate::validate;
use proptest::prelude::*;
use serde_json::{Value, json};

use crate::{CatalogRole, Code, Context, Options, read, read_with};

const XTCE: &str = "http://www.omg.org/spec/XTCE/20180204";

/// A root SpaceSystem `Sat` holding `body`.
fn doc(body: &str) -> String {
    format!(r#"<SpaceSystem name="Sat" xmlns="{XTCE}">{body}</SpaceSystem>"#)
}

/// A TelemetryMetaData with these types, parameters and containers.
fn telemetry(types: &str, params: &str, containers: &str) -> String {
    format!(
        "<TelemetryMetaData><ParameterTypeSet>{types}</ParameterTypeSet>\
         <ParameterSet>{params}</ParameterSet><ContainerSet>{containers}</ContainerSet>\
         </TelemetryMetaData>"
    )
}

const U8: &str = r#"<IntegerParameterType name="U8" signed="false"><IntegerDataEncoding sizeInBits="8"/></IntegerParameterType>"#;
const U16: &str = r#"<IntegerParameterType name="U16" signed="false"><IntegerDataEncoding sizeInBits="16"/></IntegerParameterType>"#;

fn param(name: &str, ty: &str) -> String {
    format!(r#"<Parameter name="{name}" parameterTypeRef="{ty}"/>"#)
}

fn container(name: &str, entries: &str) -> String {
    format!(
        r#"<SequenceContainer name="{name}"><EntryList>{entries}</EntryList></SequenceContainer>"#
    )
}

fn entry(param: &str) -> String {
    format!(r#"<ParameterRefEntry parameterRef="{param}"/>"#)
}

/// An entry `bits` from the start of the packet.
fn entry_at(param: &str, bits: u32) -> String {
    format!(
        r#"<ParameterRefEntry parameterRef="{param}"><LocationInContainerInBits referenceLocation="containerStart"><FixedValue>{bits}</FixedValue></LocationInContainerInBits></ParameterRefEntry>"#
    )
}

fn import_all(files: &[(&str, &str)]) -> (Sources, Result<Parsed, Vec<SourceDiagnostic>>) {
    let mut sources = Sources::new();
    for (name, text) in files {
        sources.add(*name, *text);
    }
    let result = read(sources.iter(), &Options::default());
    (sources, result)
}

/// Each diagnostic as (severity, code, line:column, message).
fn shown(
    sources: &Sources,
    diagnostics: &[SourceDiagnostic],
) -> Vec<(Severity, String, String, String)> {
    diagnostics
        .iter()
        .map(|d| {
            let at = d
                .span
                .and_then(|span| sources.location(span))
                .map_or_else(String::new, |l| format!("{}:{}", l.line, l.column));
            (d.severity, d.code.to_string(), at, d.message.clone())
        })
        .collect()
}

/// The problems importing `text` reports, as (code, line:column).
fn problems(text: &str) -> Vec<(String, String)> {
    let (sources, result) = import_all(&[("sat.xml", text)]);
    let diagnostics = match result {
        Ok(parsed) => parsed.warnings,
        Err(diagnostics) => diagnostics,
    };
    shown(&sources, &diagnostics)
        .into_iter()
        .map(|(_, code, at, _)| (code, at))
        .collect()
}

/// The one problem importing `text` reports: its code and message.
fn one_problem(text: &str) -> (String, String) {
    let (sources, result) = import_all(&[("sat.xml", text)]);
    let diagnostics = match result {
        Ok(parsed) => parsed.warnings,
        Err(diagnostics) => diagnostics,
    };
    let shown = shown(&sources, &diagnostics);
    assert_eq!(shown.len(), 1, "{shown:#?}");
    let (_, code, _, message) = shown.into_iter().next().unwrap();
    (code, message)
}

/// The one error importing `text` reports, ignoring warnings.
fn one_error(text: &str) -> (String, String) {
    let (sources, result) = import_all(&[("sat.xml", text)]);
    let errors: Vec<_> = shown(&sources, &result.unwrap_err())
        .into_iter()
        .filter(|(severity, ..)| *severity == Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    let (_, code, _, message) = errors.into_iter().next().unwrap();
    (code, message)
}

/// The bundle, as JSON, from a document that imports without a warning.
fn bundle(text: &str) -> Value {
    let (sources, result) = import_all(&[("sat.xml", text)]);
    match result {
        Ok(parsed) => {
            assert!(
                parsed.warnings.is_empty(),
                "{:#?}",
                shown(&sources, &parsed.warnings)
            );
            serde_json::to_value(&parsed.bundle).unwrap()
        }
        Err(diagnostics) => panic!("{:#?}", shown(&sources, &diagnostics)),
    }
}

fn first_packet(text: &str) -> Value {
    bundle(text)["sets"][0]["packets"][0].clone()
}

/// The bundle, as JSON, from a document that imports, and its warnings as
/// (code, message).
fn warned(text: &str) -> (Value, Vec<(String, String)>) {
    let (sources, result) = import_all(&[("sat.xml", text)]);
    match result {
        Ok(parsed) => {
            let warnings = shown(&sources, &parsed.warnings)
                .into_iter()
                .map(|(_, code, _, message)| (code, message))
                .collect();
            (serde_json::to_value(&parsed.bundle).unwrap(), warnings)
        }
        Err(diagnostics) => panic!("{:#?}", shown(&sources, &diagnostics)),
    }
}

/// Each warning's code, and whether its message holds `wanted`.
fn codes(warnings: &[(String, String)], wanted: &str) -> Vec<(String, bool)> {
    warnings
        .iter()
        .map(|(code, message)| (code.clone(), message.contains(wanted)))
        .collect()
}

#[test]
fn syntax_errors_point_where_parsing_stopped() {
    assert_eq!(
        problems(&format!(r#"<SpaceSystem name="Sat" xmlns="{XTCE}">"#)),
        [("xtce.syntax".into(), "1:71".into())]
    );
    assert_eq!(
        problems(&format!(
            "<SpaceSystem name=\"Sat\" xmlns=\"{XTCE}\">\n  <Header></Footer>\n</SpaceSystem>"
        )),
        [("xtce.syntax".into(), "2:11".into())]
    );
    let (code, message) = one_problem(&format!(
        "<!DOCTYPE x [<!ENTITY a \"b\">]>\n<SpaceSystem name=\"Sat\" xmlns=\"{XTCE}\"/>"
    ));
    assert_eq!(code, "xtce.syntax");
    assert!(message.contains("DTD"), "{message}");
}

#[test]
fn only_xtce_documents_are_read() {
    assert_eq!(
        one_problem("<Catalog/>"),
        (
            "xtce.document".into(),
            "this isn't an XTCE document: its root is Catalog in no namespace, not a \
             SpaceSystem in http://www.omg.org/spec/XTCE/20250214 (XTCE 1.3), \
             http://www.omg.org/spec/XTCE/20180204 (XTCE 1.2) or http://www.omg.org/space/xtce \
             (XTCE 1.0 and 1.1)"
                .into()
        )
    );
    let older = r#"<xtce:SpaceSystem name="Sat" xmlns:xtce="http://www.omg.org/space/xtce"/>"#;
    assert_eq!(bundle(older)["families"][0]["name"], "sat");
    // XTCE 1.3's namespace reads as 1.2's does, with no warning.
    let body = telemetry(U8, &param("A", "U8"), &container("P", &entry("A")));
    let newer = doc(&body).replace(XTCE, "http://www.omg.org/spec/XTCE/20250214");
    let (_, result) = import_all(&[("sat.xml", &newer)]);
    let parsed = result.unwrap();
    assert!(parsed.warnings.is_empty(), "{:#?}", parsed.warnings);
    assert_eq!(
        serde_json::to_value(&parsed.bundle).unwrap(),
        bundle(&doc(&body))
    );
    let (_, result) = import_all(&[]);
    assert_eq!(result.unwrap_err()[0].code, "xtce.document");
}

/// A SpaceSystem in no namespace, or in a misspelling of XTCE 1.2's or
/// 1.3's that documents written for Yamcs use, is read as XTCE with a
/// warning, Yamcs
/// reading elements by their local names. Every element still keeps to
/// the root's namespace, and any other namespace isn't XTCE.
#[test]
fn documents_yamcs_reads_whatever_their_namespace_are_read_with_a_warning() {
    let body = telemetry(U8, &param("A", "U8"), &container("P", &entry("A")));
    let expected = bundle(&doc(&body));
    let fix = "its namespace should be http://www.omg.org/spec/XTCE/20250214 (XTCE 1.3), \
               http://www.omg.org/spec/XTCE/20180204 (XTCE 1.2) or http://www.omg.org/space/xtce \
               (XTCE 1.0 and 1.1)";
    for namespace in [
        None,
        // `xmlns=""` is no namespace too.
        Some(""),
        Some("https://www.omg.org/spec/XTCE/20180204"),
        Some("http://www.omg.org/spec/XTCE/20180204/SpaceSystem.xsd"),
        Some("https://www.omg.org/spec/XTCE/20180204/SpaceSystem.xsd"),
        Some("https://www.omg.org/spec/XTCE/20250214"),
        Some("http://www.omg.org/spec/XTCE/20250214/SpaceSystem.xsd"),
        Some("https://www.omg.org/spec/XTCE/20250214/SpaceSystem.xsd"),
    ] {
        let text = match namespace {
            None => format!(r#"<SpaceSystem name="Sat">{body}</SpaceSystem>"#),
            Some(namespace) => {
                format!(r#"<SpaceSystem name="Sat" xmlns="{namespace}">{body}</SpaceSystem>"#)
            }
        };
        let (sources, result) = import_all(&[("sat.xml", &text)]);
        let parsed = result.unwrap();
        let found = shown(&sources, &parsed.warnings);
        assert_eq!(found.len(), 1, "{namespace:?}: {found:#?}");
        let (_, code, at, message) = &found[0];
        assert_eq!((code.as_str(), at.as_str()), ("xtce.namespace", "1:1"));
        let root = match namespace {
            None | Some("") => "no namespace".to_string(),
            Some(namespace) if namespace.contains("20250214") => {
                format!("{namespace}, which isn't XTCE 1.3's namespace")
            }
            Some(namespace) => format!("{namespace}, which isn't XTCE 1.2's namespace"),
        };
        assert_eq!(
            *message,
            format!(
                "this document's root is in {root}; MDL reads it as XTCE, as Yamcs does, but {fix}"
            )
        );
        assert_eq!(serde_json::to_value(&parsed.bundle).unwrap(), expected);
    }

    // An element outside the root's namespace: in none under XTCE, and in
    // XTCE under none.
    let none_under_xtce = format!(
        r#"<SpaceSystem name="Sat" xmlns="{XTCE}"><TelemetryMetaData xmlns=""/></SpaceSystem>"#
    );
    assert_eq!(
        one_problem(&none_under_xtce),
        (
            "xtce.unsupported".to_string(),
            "TelemetryMetaData is in no namespace, and SpaceSystem Sat in namespace \
             http://www.omg.org/spec/XTCE/20180204; an XTCE document keeps every element in its \
             root's namespace"
                .to_string()
        )
    );
    let xtce_under_none =
        format!(r#"<SpaceSystem name="Sat"><TelemetryMetaData xmlns="{XTCE}"/></SpaceSystem>"#);
    let (sources, result) = import_all(&[("sat.xml", &xtce_under_none)]);
    let found = shown(&sources, &result.unwrap_err());
    let found: Vec<(&str, &str)> = found
        .iter()
        .map(|(_, code, _, message)| (code.as_str(), message.as_str()))
        .collect();
    assert_eq!(found.len(), 2, "{found:#?}");
    assert_eq!(found[0].0, "xtce.namespace");
    assert_eq!(
        found[1],
        (
            "xtce.invalid",
            "TelemetryMetaData is in namespace http://www.omg.org/spec/XTCE/20180204, and \
             SpaceSystem Sat in no namespace; an XTCE document keeps every element in its root's \
             namespace"
        )
    );

    // Any other namespace isn't XTCE.
    let other = r#"<SpaceSystem name="Sat" xmlns="http://example.com/xtce"/>"#;
    assert_eq!(one_problem(other).0, "xtce.document");
}

/// A prefix bound to no namespace (`xmlns:p=""`) isn't namespace-well-formed
/// XML, though roxmltree reads it, as an empty namespace. It's refused
/// where it's declared, on the root or below.
#[test]
fn a_prefix_bound_to_no_namespace_is_refused() {
    let message = "xmlns:p=\"\" binds a prefix to no namespace, which XML namespaces don't allow";
    for text in [
        format!(r#"<SpaceSystem name="Sat" xmlns="{XTCE}" xmlns:p="" p:note="x"/>"#),
        r#"<p:SpaceSystem name="Sat" xmlns:p=""/>"#.to_string(),
        format!(
            r#"<SpaceSystem name="Sat" xmlns="{XTCE}"><TelemetryMetaData xmlns:p=""><ParameterSet/></TelemetryMetaData></SpaceSystem>"#
        ),
    ] {
        let (sources, result) = import_all(&[("sat.xml", &text)]);
        let found = shown(&sources, &result.unwrap_err());
        assert_eq!(found.len(), 1, "{text}: {found:#?}");
        assert_eq!(
            (found[0].1.as_str(), found[0].3.as_str()),
            ("xtce.syntax", message)
        );
    }
}

#[test]
fn references_resolve_as_yamcs_resolves_them() {
    // The header is in the root; Sub's packet names it several ways.
    let header_types = r#"<IntegerParameterType name="U3" signed="false"><IntegerDataEncoding sizeInBits="3"/></IntegerParameterType>
        <IntegerParameterType name="U13" signed="false"><IntegerDataEncoding sizeInBits="13"/></IntegerParameterType>
        <AggregateParameterType name="IdType"><MemberList>
          <Member name="Version" typeRef="U3"/><Member name="Apid" typeRef="U13"/>
        </MemberList></AggregateParameterType>"#;
    let root = telemetry(
        header_types,
        &param("Id", "IdType"),
        r#"<SequenceContainer name="Header" abstract="true"><EntryList><ParameterRefEntry parameterRef="Id"/></EntryList></SequenceContainer>"#,
    );
    let sub = |base: &str, criteria: &str, entry_ref: &str| {
        format!(
            r#"<SpaceSystem name="Sub"><TelemetryMetaData>
              <ParameterTypeSet>{U8}</ParameterTypeSet>
              <ParameterSet>{}</ParameterSet>
              <ContainerSet><SequenceContainer name="P">
                <EntryList><ParameterRefEntry parameterRef="{entry_ref}"/></EntryList>
                <BaseContainer containerRef="{base}"><RestrictionCriteria><ComparisonList>{criteria}</ComparisonList></RestrictionCriteria></BaseContainer>
              </SequenceContainer></ContainerSet>
            </TelemetryMetaData></SpaceSystem>"#,
            param("Value", "U8")
        )
    };
    let criteria = r#"<Comparison parameterRef="Id.Version" value="0"/><Comparison parameterRef="/Sat/Id/Apid" value="9"/>"#;
    for base in ["Header", "/Sat/Header", "../Header"] {
        let packet = first_packet(&doc(&format!("{root}{}", sub(base, criteria, "./Value"))));
        assert_eq!(
            packet["identify"],
            json!({ "Id_Apid": 9, "Id_Version": 0 }),
            "{base}"
        );
        assert_eq!(packet["entries"][2]["name"], "Value");
    }
    // Names that resolve to nothing are errors at the reference.
    let (code, message) = one_problem(&doc(&format!(
        "{root}{}",
        sub("Missing", criteria, "Value")
    )));
    assert_eq!(code, "xtce.reference");
    assert_eq!(message, "Missing names no container that /Sat/Sub can see");
    let (code, message) = one_problem(&doc(&format!(
        "{root}{}",
        sub("Header", criteria, "../Value")
    )));
    assert_eq!(code, "xtce.reference");
    assert_eq!(message, "../Value names no parameter that /Sat/Sub can see");
}

#[test]
fn catalog_names_come_from_aliases_or_paths() {
    let params = r#"<Parameter name="Bus-Voltage" parameterTypeRef="U8"/>
        <Parameter name="Temp" parameterTypeRef="U8"><AliasSet><Alias nameSpace="MDL" alias="eps.temp"/></AliasSet></Parameter>"#;
    let text = doc(&telemetry(
        U8,
        params,
        &container("P", &(entry("Bus-Voltage") + &entry("Temp"))),
    ));
    let (sources, result) = import_all(&[("sat.xml", &text)]);
    let parsed = result.unwrap();
    let names: Vec<&String> = parsed.bundle.catalog.params.keys().collect();
    assert_eq!(names, ["eps.temp", "sat.bus_voltage"]);
    let packet = serde_json::to_value(&parsed.bundle.sets[0].packets[0]).unwrap();
    assert_eq!(packet["entries"][0]["name"], "Bus_Voltage");
    assert_eq!(packet["entries"][1]["name"], "Temp");
    let warnings = shown(&sources, &parsed.warnings);
    assert_eq!(warnings.len(), 1, "{warnings:#?}");
    assert_eq!(
        (warnings[0].1.as_str(), warnings[0].3.as_str()),
        (
            "xtce.renamed",
            "parameter /Sat/Bus-Voltage is field Bus_Voltage: MDL names are letters, digits \
             and '_'"
        )
    );
    // Two parameters that come out with one catalog name.
    let params = format!("{}{}", param("BusVoltage", "U8"), param("busvoltage", "U8"));
    let text = doc(&telemetry(
        U8,
        &params,
        &container("P", &(entry("BusVoltage") + &entry("busvoltage"))),
    ));
    let (sources, result) = import_all(&[("sat.xml", &text)]);
    let errors = result.unwrap_err();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, "xtce.name");
    assert_eq!(
        errors[0].message,
        "/Sat/busvoltage and /Sat/BusVoltage both have catalog name sat.busvoltage"
    );
    let at = sources.location(errors[0].span.unwrap()).unwrap();
    let first = sources.location(errors[0].first.unwrap()).unwrap();
    assert!(first.column < at.column, "{first:?} {at:?}");
}

#[test]
fn the_family_is_little_endian_only_when_every_number_is() {
    let little = r#"<IntegerParameterType name="L16" signed="false"><IntegerDataEncoding sizeInBits="16" byteOrder="leastSignificantByteFirst"/></IntegerParameterType>"#;
    let types = format!("{little}{U8}{U16}");
    let all_little = bundle(&doc(&telemetry(
        &types,
        &format!("{}{}", param("A", "L16"), param("B", "U8")),
        &container("P", &(entry("A") + &entry("B"))),
    )));
    let params = format!(
        "{}{}{}",
        param("A", "L16"),
        param("B", "U8"),
        param("C", "U16")
    );
    assert_eq!(all_little["families"][0]["byte_order"], "little");
    assert_eq!(
        all_little["sets"][0]["packets"][0]["entries"][0].get("byte_order"),
        None
    );
    let mixed = bundle(&doc(&telemetry(
        &types,
        &params,
        &container("P", &(entry("C") + &entry("A") + &entry("B"))),
    )));
    assert_eq!(mixed["families"][0]["byte_order"], "big");
    let entries = &mixed["sets"][0]["packets"][0]["entries"];
    assert_eq!(entries[0].get("byte_order"), None);
    assert_eq!(entries[1]["byte_order"], "little");
    assert_eq!(entries[2].get("byte_order"), None);
}

#[test]
fn locations_place_entries_from_the_packet_start() {
    let at = |reference: &str, bits: i64| {
        format!(
            r#"<ParameterRefEntry parameterRef="B"><LocationInContainerInBits referenceLocation="{reference}"><FixedValue>{bits}</FixedValue></LocationInContainerInBits></ParameterRefEntry>"#
        )
    };
    let params = format!("{}{}", param("A", "U8"), param("B", "U8"));
    let text = |entries: String| doc(&telemetry(U8, &params, &container("P", &entries)));
    let start = first_packet(&text(entry("A") + &at("containerStart", 16)));
    assert_eq!(start["entries"][1]["offset_bits"], 16);
    // Comments around a value are skipped.
    let commented = at("containerStart", 16).replace(
        "<FixedValue>16",
        "<FixedValue><!-- after A and a spare byte -->16",
    );
    let start = first_packet(&text(entry("A") + &commented));
    assert_eq!(start["entries"][1]["offset_bits"], 16);
    let after = first_packet(&text(entry("A") + &at("previousEntry", 4)));
    assert_eq!(
        after["entries"][1],
        json!({ "kind": "padding", "size_bits": 4 })
    );
    assert_eq!(
        one_problem(&text(entry("A") + &at("previousEntry", -4))),
        (
            "xtce.unsupported".into(),
            "an entry placed before the end of the previous one isn't supported".into()
        )
    );
    assert_eq!(
        one_problem(&text(entry("A") + &at("containerEnd", 8))).0,
        "xtce.unsupported"
    );
}

#[test]
fn an_absent_entry_at_a_fixed_place_is_followed_only_by_another() {
    let params = format!(
        "{}{}{}",
        param("A", "U8"),
        param("B", "U8"),
        param("C", "U8")
    );
    let conditional = r#"<ParameterRefEntry parameterRef="B">
        <LocationInContainerInBits referenceLocation="containerStart"><FixedValue>16</FixedValue></LocationInContainerInBits>
        <IncludeCondition><Comparison parameterRef="A" value="1"/></IncludeCondition></ParameterRefEntry>"#;
    let (code, message) = one_problem(&doc(&telemetry(
        U8,
        &params,
        &container("P", &format!("{}{conditional}{}", entry("A"), entry("C"))),
    )));
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with(
            "B is included on a condition at a fixed location, and the next entry follows it"
        ),
        "{message}"
    );
    // An aggregate's members come and go together, so only what follows
    // the aggregate counts; padding before the next entry counts too.
    let pair = r#"<AggregateParameterType name="Pair"><MemberList><Member name="a" typeRef="U8"/><Member name="b" typeRef="U8"/></MemberList></AggregateParameterType>"#;
    let conditional_pair = r#"<ParameterRefEntry parameterRef="B">
        <LocationInContainerInBits referenceLocation="containerStart"><FixedValue>16</FixedValue></LocationInContainerInBits>
        <IncludeCondition><Comparison parameterRef="A" value="1"/></IncludeCondition></ParameterRefEntry>"#;
    let padded = r#"<ParameterRefEntry parameterRef="C"><LocationInContainerInBits><FixedValue>8</FixedValue></LocationInContainerInBits></ParameterRefEntry>"#;
    let params = format!(
        "{}{}{}",
        param("A", "U8"),
        param("B", "Pair"),
        param("C", "U8")
    );
    let text = |after: &str| {
        doc(&telemetry(
            &format!("{U8}{pair}"),
            &params,
            &container("P", &format!("{}{conditional_pair}{after}", entry("A"))),
        ))
    };
    assert_eq!(one_problem(&text(padded)).0, "xtce.unsupported");
    let alone = bundle(&text(&entry_at("C", 40)));
    assert_eq!(
        alone["sets"][0]["packets"][0]["entries"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn conditions_compare_earlier_fields_by_raw_value() {
    let mode = r#"<EnumeratedParameterType name="Mode"><IntegerDataEncoding sizeInBits="8"/><EnumerationList>
        <Enumeration value="0" label="OFF"/><Enumeration value="5" label="ON"/></EnumerationList></EnumeratedParameterType>"#;
    let types = format!("{mode}{U8}");
    let params = format!(
        "{}{}{}",
        param("M", "Mode"),
        param("N", "U8"),
        param("X", "U8")
    );
    let text = |condition: &str| {
        let conditional = format!(
            r#"<ParameterRefEntry parameterRef="X"><IncludeCondition>{condition}</IncludeCondition></ParameterRefEntry>"#
        );
        doc(&telemetry(
            &types,
            &params,
            &container("P", &format!("{}{}{conditional}", entry("M"), entry("N"))),
        ))
    };
    let when = |condition: &str| first_packet(&text(condition))["entries"][2]["when"].clone();
    assert_eq!(
        when(r#"<Comparison parameterRef="M" value="ON"/>"#),
        json!({ "kind": "compare", "field": "M", "op": "eq", "value": 5 })
    );
    assert_eq!(
        when(
            r#"<ComparisonList><Comparison parameterRef="M" value="5" useCalibratedValue="false"/>
               <Comparison parameterRef="N" comparisonOperator="&gt;=" value="0x10"/></ComparisonList>"#
        ),
        json!({ "kind": "all", "conditions": [
            { "kind": "compare", "field": "M", "op": "eq", "value": 5 },
            { "kind": "compare", "field": "N", "op": "ge", "value": 16 }
        ] })
    );
    assert_eq!(
        one_problem(&text(r#"<Comparison parameterRef="M" value="DIM"/>"#)),
        (
            "xtce.unsupported".into(),
            "this comparison of M can't be imported: \"DIM\" isn't one of its values (OFF, ON)"
                .into()
        )
    );
    assert_eq!(
        one_problem(&text(r#"<Comparison parameterRef="X" value="1"/>"#)),
        (
            "xtce.unsupported".into(),
            "X isn't a field decoded earlier in the packet, and MDL conditions compare only those"
                .into()
        )
    );
    assert_eq!(
        one_problem(&text("<BooleanExpression/>")),
        (
            "xtce.invalid".into(),
            "BooleanExpression holds conditions".into()
        )
    );
}

#[test]
fn restriction_criteria_become_identify_keys() {
    let params = format!("{}{}", param("Kind", "U8"), param("Sub", "U8"));
    let derived = |name: &str, base: &str, criteria: &str| {
        format!(
            r#"<SequenceContainer name="{name}"><EntryList/><BaseContainer containerRef="{base}"><RestrictionCriteria>{criteria}</RestrictionCriteria></BaseContainer></SequenceContainer>"#
        )
    };
    let base = r#"<SequenceContainer name="Base" abstract="true"><EntryList><ParameterRefEntry parameterRef="Kind"/><ParameterRefEntry parameterRef="Sub"/></EntryList></SequenceContainer>"#;
    let text = |containers: &str| doc(&telemetry(U8, &params, &format!("{base}{containers}")));
    let kind = |value: u8| format!(r#"<Comparison parameterRef="Kind" value="{value}"/>"#);
    let packets = bundle(&text(&format!(
        "{}{}",
        derived("A", "Base", &kind(1)),
        derived("B", "A", r#"<Comparison parameterRef="Sub" value="2"/>"#)
    )))["sets"][0]["packets"]
        .clone();
    assert_eq!(packets[0]["identify"], json!({ "Kind": 1 }));
    assert_eq!(packets[1]["identify"], json!({ "Kind": 1, "Sub": 2 }));
    // A derived container that contradicts its base never matches.
    assert_eq!(
        one_problem(&text(&format!(
            "{}{}",
            derived("A", "Base", &kind(1)),
            derived("B", "A", &kind(2))
        ))),
        (
            "xtce.invalid".into(),
            "this requires Kind to be 2, but a container above requires 1, so no packet matches"
                .into()
        )
    );
    assert_eq!(
        one_problem(&text(&derived(
            "A",
            "Base",
            r#"<Comparison parameterRef="Kind" comparisonOperator="!=" value="1"/>"#
        )))
        .1,
        "MDL identifies packets by equal values; this comparison isn't one"
    );
    assert_eq!(
        one_problem(&text(&derived("A", "A", &kind(1)))),
        (
            "xtce.invalid".into(),
            "container A inherits from itself".into()
        )
    );
}

#[test]
fn what_mdl_cant_represent_is_an_error() {
    let cases = [
        (
            r#"<AbsoluteTimeParameterType name="T"><Encoding><IntegerDataEncoding sizeInBits="32"/></Encoding><ReferenceTime><Epoch>TAI</Epoch></ReferenceTime></AbsoluteTimeParameterType>"#,
            "absolute time parameters aren't supported: an MDL field holds a number, not an \
             instant, so the decoded value would change",
        ),
        (
            r#"<IntegerParameterType name="T"><IntegerDataEncoding sizeInBits="8" encoding="BCD"/></IntegerParameterType>"#,
            "BCD integers aren't supported: MDL reads unsigned, two's complement, sign-magnitude \
             and ones' complement",
        ),
        (
            r#"<FloatParameterType name="T"><FloatDataEncoding sizeInBits="32" encoding="MILSTD_1750A"/></FloatParameterType>"#,
            "MILSTD_1750A floats aren't supported: MDL reads IEEE 754",
        ),
        (
            r#"<IntegerParameterType name="T"><IntegerDataEncoding sizeInBits="8" bitOrder="leastSignificantBitFirst"/></IntegerParameterType>"#,
            "least significant bit first isn't supported: MDL numbers bits from the most \
             significant",
        ),
        (
            r#"<FloatParameterType name="T"><IntegerDataEncoding sizeInBits="8"><ContextCalibratorList/></IntegerDataEncoding></FloatParameterType>"#,
            "context calibrators (a calibration chosen by a condition) aren't supported",
        ),
        (
            r#"<FloatParameterType name="T"><IntegerDataEncoding sizeInBits="8"><DefaultCalibrator><MathOperationCalibrator/></DefaultCalibrator></IntegerDataEncoding></FloatParameterType>"#,
            "math operation calibrators aren't supported",
        ),
        (
            r#"<IntegerParameterType name="T"><IntegerDataEncoding sizeInBits="8"><DefaultCalibrator><PolynomialCalibrator><Term coefficient="2" exponent="1"/></PolynomialCalibrator></DefaultCalibrator></IntegerDataEncoding></IntegerParameterType>"#,
            "an integer type with a calibrator: MDL's calibrations give floats, so the value \
             would change; make it a FloatParameterType",
        ),
        (
            r#"<FloatParameterType name="T"><IntegerDataEncoding sizeInBits="8"><DefaultCalibrator><SplineCalibrator order="2"><SplinePoint raw="0" calibrated="0"/><SplinePoint raw="1" calibrated="1"/></SplineCalibrator></DefaultCalibrator></IntegerDataEncoding></FloatParameterType>"#,
            "order 2 splines aren't supported: MDL interpolates flat (0) or linearly (1)",
        ),
        (
            r#"<StringParameterType name="T"><StringDataEncoding encoding="UTF-16"><SizeInBits><Fixed><FixedValue>64</FixedValue></Fixed></SizeInBits></StringDataEncoding></StringParameterType>"#,
            "UTF-16 text isn't supported: MDL reads UTF-8, US-ASCII and ISO-8859-1",
        ),
        (
            r#"<IntegerParameterType name="T" baseType="U8"><IntegerDataEncoding sizeInBits="8"/></IntegerParameterType>"#,
            "type inheritance (baseType) isn't supported; write the type out whole",
        ),
        (
            r#"<IntegerParameterType name="T" flavour="x"><IntegerDataEncoding sizeInBits="8"/></IntegerParameterType>"#,
            "IntegerParameterType T's attribute flavour isn't supported, so this can't be \
             imported faithfully",
        ),
    ];
    for (ty, message) in cases {
        let text = doc(&telemetry(
            &format!("{U8}{ty}"),
            &param("X", "T"),
            &container("P", &entry("X")),
        ));
        assert_eq!(
            one_problem(&text),
            ("xtce.unsupported".into(), message.into()),
            "{ty}"
        );
    }
    let entries = [(
        r#"<xi:include xmlns:xi="http://www.w3.org/2001/XInclude" href="more.xml"/>"#,
        "XInclude isn't supported; give the importer the included files instead",
    )];
    for (entry, message) in entries {
        let text = doc(&telemetry(U8, &param("X", "U8"), &container("P", entry)));
        assert_eq!(
            one_problem(&text),
            ("xtce.unsupported".into(), message.into()),
            "{entry}"
        );
    }
}

#[test]
fn what_doesnt_change_values_is_a_warning() {
    let types = format!(
        r#"{U8}<FloatParameterType name="Temp"><UnitSet><Unit>units:Becquerel</Unit></UnitSet>
             <IntegerDataEncoding sizeInBits="8" changeThreshold="1"/>
             <DefaultAlarm><StaticAlarmRanges><WarningRange maxInclusive="5"/></StaticAlarmRanges></DefaultAlarm></FloatParameterType>
           <BooleanParameterType name="Flag" oneStringValue="ON" zeroStringValue="OFF"><IntegerDataEncoding sizeInBits="8"/></BooleanParameterType>"#
    );
    let params = format!(
        r#"{}{}{}<Parameter name="Spare" parameterTypeRef="U8"><AliasSet><Alias nameSpace="ops" alias="SPARE"/></AliasSet></Parameter>"#,
        param("T", "Temp"),
        param("F", "Flag"),
        param("Unused", "U8")
    );
    let telemetry = telemetry(
        &types,
        &params,
        &container("P", &(entry("T") + &entry("F") + &entry("Spare"))),
    )
    .replace(
        "</TelemetryMetaData>",
        "<AlgorithmSet/></TelemetryMetaData>",
    );
    let text = doc(&format!(
        "<Header validationStatus=\"Working\"/>{telemetry}\
         <CommandMetaData><CommandContainerSet/></CommandMetaData>"
    ));
    let (sources, result) = import_all(&[("sat.xml", &text)]);
    let parsed = result.unwrap();
    let messages: Vec<String> = shown(&sources, &parsed.warnings)
        .into_iter()
        .map(|(severity, code, _, message)| {
            assert_eq!(
                (severity, code.as_str()),
                (Severity::Warning, "xtce.ignored")
            );
            message
        })
        .collect();
    assert_eq!(
        messages,
        [
            "SpaceSystem Sat's Header isn't imported: MDL keeps no document header",
            "unit \"units:Becquerel\" isn't a UCUM code MDL knows, so it isn't imported; the \
             value is unchanged",
            "IntegerDataEncoding's changeThreshold isn't imported: MDL has no change thresholds",
            "type Flag's labels (ON, OFF) aren't imported: MDL booleans are true and false",
            "parameter /Sat/Unused is in no packet, so it isn't imported",
            "aliases in ops aren't imported: MDL reads only the MDL namespace",
            "TelemetryMetaData's AlgorithmSet isn't imported: MDL decodes what packets carry, \
             not what algorithms compute",
            "CommandMetaData's CommandContainerSet isn't imported: command containers are \
             read inside meta-commands, as Yamcs reads them",
        ]
    );
}

#[test]
fn strings_and_bytes_keep_their_lengths_and_text_encodings() {
    let types = format!(
        r#"{U8}<StringParameterType name="Name"><StringDataEncoding encoding="US-ASCII"><SizeInBits><Fixed><FixedValue>32</FixedValue></Fixed></SizeInBits></StringDataEncoding></StringParameterType>
           <StringParameterType name="Note"><StringDataEncoding encoding="ISO-8859-1"><SizeInBits><TerminationChar>0A</TerminationChar></SizeInBits></StringDataEncoding></StringParameterType>
           <StringParameterType name="Buffer"><StringDataEncoding><SizeInBits><Fixed><FixedValue>64</FixedValue></Fixed><TerminationChar>00</TerminationChar></SizeInBits></StringDataEncoding></StringParameterType>
           <BinaryParameterType name="Blob"><BinaryDataEncoding><SizeInBits><FixedValue>24</FixedValue></SizeInBits></BinaryDataEncoding></BinaryParameterType>"#
    );
    let params = format!(
        "{}{}{}{}",
        param("N", "Name"),
        param("M", "Note"),
        param("F", "Buffer"),
        param("B", "Blob")
    );
    let packet = first_packet(&doc(&telemetry(
        &types,
        &params,
        &container("P", &(entry("N") + &entry("M") + &entry("F") + &entry("B"))),
    )));
    assert_eq!(
        packet["entries"],
        json!([
            { "kind": "field", "name": "N", "type": "string", "text": "ascii",
              "length": { "kind": "fixed", "bytes": 4 }, "canonical": "sat.n" },
            { "kind": "field", "name": "M", "type": "string", "text": "latin1",
              "length": { "kind": "terminator", "byte": 10 }, "canonical": "sat.m" },
            { "kind": "field", "name": "F", "type": "string",
              "length": { "kind": "fixed", "bytes": 8, "terminator": 0 }, "canonical": "sat.f" },
            { "kind": "field", "name": "B", "type": "bytes",
              "length": { "kind": "fixed", "bytes": 3 }, "canonical": "sat.b" }
        ])
    );
}

#[test]
fn validation_problems_point_into_the_xtce() {
    // B at bit 4 overlaps A: the import is fine, and validation's overlap
    // is reported at B's entry.
    let params = format!("{}{}", param("A", "U8"), param("B", "U8"));
    let text = doc(&telemetry(
        U8,
        &params,
        &container("P", &format!("{}{}", entry("A"), entry_at("B", 4))),
    ));
    let (sources, result) = import_all(&[("sat.xml", &text)]);
    let parsed = result.unwrap();
    let diagnostics = parsed.map.locate_all(&validate(parsed.bundle).unwrap_err());
    let shown = shown(&sources, &diagnostics);
    assert_eq!(shown.len(), 1, "{shown:#?}");
    let span = diagnostics[0].span.unwrap();
    assert!(
        text[span.start..].starts_with(r#"<ParameterRefEntry parameterRef="B">"#),
        "{shown:#?}"
    );
}

#[test]
fn a_problem_in_a_shared_definition_is_reported_once() {
    let base = r#"<SequenceContainer name="Base" abstract="true"><EntryList><ParameterRefEntry parameterRef="Missing"/></EntryList></SequenceContainer>"#;
    let derived = |name: &str| {
        format!(
            r#"<SequenceContainer name="{name}"><EntryList/><BaseContainer containerRef="Base"/></SequenceContainer>"#
        )
    };
    let text = doc(&telemetry(
        U8,
        "",
        &format!("{base}{}{}", derived("A"), derived("B")),
    ));
    let found = problems(&text);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "xtce.reference");
}

/// A container built on one that couldn't be imported reports nothing
/// about the missing container's fields: they're there in the XTCE, and
/// the real problem was reported (Yamcs' PUS example: `pus-tm` restricted
/// by `apid != 0`, its packets by its own `type`).
#[test]
fn a_restriction_on_a_failed_containers_field_isnt_blamed() {
    let params = format!("{}{}", param("Kind", "U8"), param("Sub", "U8"));
    let containers = format!(
        "{}{}{}",
        r#"<SequenceContainer name="Base" abstract="true"><EntryList><ParameterRefEntry parameterRef="Kind"/></EntryList></SequenceContainer>"#,
        r#"<SequenceContainer name="A" abstract="true"><EntryList><ParameterRefEntry parameterRef="Sub"/></EntryList><BaseContainer containerRef="Base"><RestrictionCriteria><Comparison parameterRef="Kind" comparisonOperator="!=" value="0"/></RestrictionCriteria></BaseContainer></SequenceContainer>"#,
        r#"<SequenceContainer name="B"><EntryList/><BaseContainer containerRef="A"><RestrictionCriteria><Comparison parameterRef="Sub" value="2"/></RestrictionCriteria></BaseContainer></SequenceContainer>"#,
    );
    assert_eq!(
        one_problem(&doc(&telemetry(U8, &params, &containers))).1,
        "MDL identifies packets by equal values; this comparison isn't one"
    );
}

/// Only the parameters a failed container holds go unblamed, in
/// restrictions, conditions and sizes alike; a reference to anything else
/// that isn't a field is reported, and so is one after a conflict, which
/// loses no fields.
#[test]
fn only_what_a_failed_container_holds_goes_unblamed() {
    let messages = |containers: &str, types: &str| {
        let params = ["Kind", "Sub", "Other", "X"]
            .map(|name| param(name, "U8"))
            .concat()
            + &param("Arr", "Some");
        let text = doc(&telemetry(types, &params, containers));
        let (sources, result) = import_all(&[("sat.xml", &text)]);
        let diagnostics = result.err().unwrap_or_default();
        shown(&sources, &diagnostics)
            .into_iter()
            .map(|(.., message)| message)
            .collect::<Vec<_>>()
    };
    let types = format!(
        "{U8}{}",
        array_type(
            "Some",
            "U8",
            r#"<DynamicValue><ParameterInstanceRef parameterRef="Sub"/><LinearAdjustment intercept="-1"/></DynamicValue>"#
        )
    );
    let base = r#"<SequenceContainer name="Base" abstract="true"><EntryList><ParameterRefEntry parameterRef="Kind"/></EntryList></SequenceContainer>"#;
    let sub = |name: &str, on: &str, criteria: &str, entries: &str| {
        let restriction = match criteria {
            "" => format!(r#"<BaseContainer containerRef="{on}"/>"#),
            criteria => format!(
                r#"<BaseContainer containerRef="{on}"><RestrictionCriteria>{criteria}</RestrictionCriteria></BaseContainer>"#
            ),
        };
        format!(
            r#"<SequenceContainer name="{name}"><EntryList>{entries}</EntryList>{restriction}</SequenceContainer>"#
        )
    };
    let is = |field: &str, op: &str, value: u8| {
        format!(r#"<Comparison parameterRef="{field}" comparisonOperator="{op}" value="{value}"/>"#)
    };
    let fails = sub("A", "Base", &is("Kind", "!=", 0), &entry("Sub")).replace(
        "<SequenceContainer ",
        r#"<SequenceContainer abstract="true" "#,
    );
    let refused = "MDL identifies packets by equal values; this comparison isn't one";
    let not_a_field = "Other isn't a field of the containers this one inherits from, and MDL \
                       identifies a packet by its own fields";

    // B1's key is A's field; B2's is in no container.
    let leaves = format!(
        "{base}{fails}{}{}",
        sub("B1", "A", &is("Sub", "==", 1), ""),
        sub("B2", "A", &is("Other", "==", 2), "")
    );
    assert_eq!(messages(&leaves, &types), [refused, not_a_field]);
    // A condition and a size read from A's field.
    let condition = r#"<ParameterRefEntry parameterRef="X"><IncludeCondition><Comparison parameterRef="Sub" value="1"/></IncludeCondition></ParameterRefEntry>"#;
    let both = format!(
        "{base}{fails}{}{}",
        sub("C", "A", "", condition),
        sub("D", "A", "", &entry("Arr"))
    );
    assert_eq!(messages(&both, &types), [refused]);
    // A conflict loses nothing, so E's key in no container is reported.
    let conflict = format!(
        "{base}{}{}{}",
        sub("A", "Base", &is("Kind", "==", 1), &entry("Sub")).replace(
            "<SequenceContainer ",
            r#"<SequenceContainer abstract="true" "#
        ),
        sub("B", "A", &is("Kind", "==", 2), "").replace(
            "<SequenceContainer ",
            r#"<SequenceContainer abstract="true" "#
        ),
        sub("E", "B", &is("Other", "==", 3), "")
    );
    assert_eq!(
        messages(&conflict, &types),
        [
            "this requires Kind to be 2, but a container above requires 1, so no packet matches",
            not_a_field
        ]
    );
}

/// A second SpaceSystem of a name, a root in another file or a sibling, is
/// reported and left out, like a second parameter of a name: imported too,
/// its contents would be reported colliding with the first's.
#[test]
fn a_second_spacesystem_of_a_name_is_left_out() {
    let body = telemetry(U8, &param("A", "U8"), &container("P", &entry("A")));
    let sat = doc(&body);
    let (sources, result) = import_all(&[("one.xml", &sat), ("two.xml", &sat)]);
    let found = shown(&sources, &result.unwrap_err());
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].1, "xtce.duplicate");
    assert_eq!(found[0].3, "two files have a root SpaceSystem named Sat");

    let sub = format!(r#"<SpaceSystem name="Sub">{body}</SpaceSystem>"#);
    assert_eq!(
        one_problem(&doc(&format!("{sub}{sub}"))),
        (
            "xtce.duplicate".to_string(),
            "/Sat has two SpaceSystems named Sub".to_string()
        )
    );
}

#[test]
fn names_shared_across_spacesystems_are_qualified() {
    // EPS and ADCS each have an HK container and a Status parameter.
    let sub = |name: &str| {
        format!(
            r#"<SpaceSystem name="{name}">{}</SpaceSystem>"#,
            telemetry(
                U8,
                &param("Status", "U8"),
                &container("HK", &entry("Status"))
            )
        )
    };
    let text = doc(&format!("{}{}", sub("EPS"), sub("ADCS")));
    let (sources, result) = import_all(&[("sat.xml", &text)]);
    let parsed = result.unwrap();
    let packets = serde_json::to_value(&parsed.bundle.sets[0].packets).unwrap();
    assert_eq!(packets[0]["name"], "EPS_HK");
    assert_eq!(packets[0]["entries"][0]["name"], "EPS_Status");
    assert_eq!(packets[1]["name"], "ADCS_HK");
    assert_eq!(packets[1]["entries"][0]["name"], "ADCS_Status");
    let messages: Vec<String> = shown(&sources, &parsed.warnings)
        .into_iter()
        .map(|(_, code, _, message)| format!("{code} {message}"))
        .collect();
    assert_eq!(
        messages,
        [
            "xtce.renamed parameter /Sat/EPS/Status is field EPS_Status: another parameter is \
             also named Status",
            "xtce.renamed container /Sat/EPS/HK is packet EPS_HK: another container is also \
             named HK",
            "xtce.renamed parameter /Sat/ADCS/Status is field ADCS_Status: another parameter is \
             also named Status",
            "xtce.renamed container /Sat/ADCS/HK is packet ADCS_HK: another container is also \
             named HK",
        ]
    );
    // Validation finds no duplicate names (the two catch-all packets are
    // ambiguous, which is this document's own problem).
    let codes: Vec<_> = validate(parsed.bundle)
        .unwrap_err()
        .iter()
        .map(|d| d.code)
        .collect();
    assert_eq!(codes, [mdl::diagnostics::Code::AmbiguousIdentify]);
    // A qualified name another container already has is still an error.
    let taken = doc(&format!(
        "{}{}{}",
        telemetry(U8, &param("X", "U8"), &container("EPS_HK", &entry("X"))),
        sub("EPS"),
        sub("ADCS")
    ));
    let (code, message) = one_error(&taken);
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("xtce.name", "two packets are named EPS_HK")
    );
}

#[test]
fn a_parameter_is_in_a_packet_once() {
    let text = doc(&telemetry(
        U8,
        &param("A", "U8"),
        &container("P", &(entry("A") + &entry("A"))),
    ));
    assert_eq!(
        one_problem(&text),
        (
            "xtce.unsupported".into(),
            "/Sat/A is in this packet twice, and MDL maps a param from one entry per packet".into()
        )
    );
}

#[test]
fn a_wide_boolean_is_true_for_any_value_but_zero() {
    let types = format!(
        r#"{U8}<BooleanParameterType name="Wide"><IntegerDataEncoding sizeInBits="8"/></BooleanParameterType>
           <BooleanParameterType name="Bit"><IntegerDataEncoding sizeInBits="1"/></BooleanParameterType>"#
    );
    let params = format!(
        "{}{}{}",
        param("W", "Wide"),
        param("B", "Bit"),
        param("X", "U8")
    );
    let text = |condition: &str| {
        let conditional = format!(
            r#"<ParameterRefEntry parameterRef="X"><IncludeCondition>{condition}</IncludeCondition></ParameterRefEntry>"#
        );
        doc(&telemetry(
            &types,
            &params,
            &container(
                "P",
                &format!(
                    "{}{}<ParameterRefEntry parameterRef=\"B\"><LocationInContainerInBits><FixedValue>7</FixedValue></LocationInContainerInBits></ParameterRefEntry>{conditional}",
                    entry("W"),
                    ""
                ),
            ),
        ))
    };
    let when = |condition: &str| first_packet(&text(condition))["entries"][3]["when"].clone();
    let compare = |field: &str, op: &str, value: i64| json!({ "kind": "compare", "field": field, "op": op, "value": value });
    assert_eq!(
        when(r#"<Comparison parameterRef="W" value="True"/>"#),
        compare("W", "ne", 0)
    );
    assert_eq!(
        when(r#"<Comparison parameterRef="W" comparisonOperator="!=" value="true"/>"#),
        compare("W", "eq", 0)
    );
    assert_eq!(
        when(r#"<Comparison parameterRef="W" value="False"/>"#),
        compare("W", "eq", 0)
    );
    assert_eq!(
        when(r#"<Comparison parameterRef="B" value="True"/>"#),
        compare("B", "eq", 1)
    );
    // Identification needs one value.
    let identify = doc(&telemetry(
        &types,
        &param("W", "Wide"),
        &format!(
            r#"<SequenceContainer name="Base" abstract="true"><EntryList>{}</EntryList></SequenceContainer>
               <SequenceContainer name="P"><EntryList/><BaseContainer containerRef="Base"><RestrictionCriteria>
               <Comparison parameterRef="W" value="True"/></RestrictionCriteria></BaseContainer></SequenceContainer>"#,
            entry("W")
        ),
    ));
    assert_eq!(
        one_problem(&identify).1,
        "W is a boolean wider than a bit, true for any value but 0, and MDL identifies a packet \
         by one value of each key"
    );
}

#[test]
fn a_label_on_several_values_means_any_of_them() {
    let mode = r#"<EnumeratedParameterType name="Mode"><IntegerDataEncoding sizeInBits="8"/><EnumerationList>
        <Enumeration value="0" label="OFF"/><Enumeration value="1" label="ON"/><Enumeration value="2" label="ON"/>
        </EnumerationList></EnumeratedParameterType>"#;
    let types = format!("{mode}{U8}");
    let params = format!("{}{}", param("M", "Mode"), param("X", "U8"));
    let packet = |op: &str| {
        let conditional = format!(
            r#"<ParameterRefEntry parameterRef="X"><IncludeCondition><Comparison parameterRef="M" comparisonOperator="{op}" value="ON"/></IncludeCondition></ParameterRefEntry>"#
        );
        first_packet(&doc(&telemetry(
            &types,
            &params,
            &container("P", &format!("{}{conditional}", entry("M"))),
        )))
    };
    let compare =
        |op: &str, value: i64| json!({ "kind": "compare", "field": "M", "op": op, "value": value });
    assert_eq!(
        packet("==")["entries"][1]["when"],
        json!({ "kind": "any", "conditions": [compare("eq", 1), compare("eq", 2)] })
    );
    assert_eq!(
        packet("!=")["entries"][1]["when"],
        json!({ "kind": "all", "conditions": [compare("ne", 1), compare("ne", 2)] })
    );
}

#[test]
fn time_associations_are_errors_and_validity_conditions_warnings() {
    let associated = r#"<ParameterRefEntry parameterRef="A"><TimeAssociation parameterRef="T"/></ParameterRefEntry>"#;
    assert_eq!(
        one_problem(&doc(&telemetry(
            U8,
            &param("A", "U8"),
            &container("P", associated)
        ))),
        (
            "xtce.unsupported".into(),
            "a time association isn't supported: it times the value by another parameter, and an \
             MDL sample takes its packet's time"
                .into()
        )
    );
    let properties = |inner: &str| {
        format!(
            r#"<Parameter name="A" parameterTypeRef="U8"><ParameterProperties>{inner}</ParameterProperties></Parameter>"#
        )
    };
    let text = |inner: &str| {
        doc(&telemetry(
            U8,
            &properties(inner),
            &container("P", &entry("A")),
        ))
    };
    assert_eq!(
        one_problem(&text(r#"<TimeAssociation parameterRef="T"/>"#)).0,
        "xtce.unsupported"
    );
    assert_eq!(
        one_problem(&text(
            r#"<ValidityCondition><Comparison parameterRef="A" value="1"/></ValidityCondition>"#
        )),
        (
            "xtce.ignored".into(),
            "ParameterProperties's ValidityCondition isn't imported: MDL has no validity \
             conditions, which set a value's quality, not the value"
                .into()
        )
    );
}

#[test]
fn units_and_byte_orders_are_read_exactly() {
    let float = |unit: &str, encoding: &str| {
        format!(
            r#"<FloatParameterType name="T"><UnitSet>{unit}</UnitSet>{encoding}</FloatParameterType>"#
        )
    };
    let text = |ty: String| {
        doc(&telemetry(
            &ty,
            &param("X", "T"),
            &container("P", &entry("X")),
        ))
    };
    let sixteen = r#"<IntegerDataEncoding sizeInBits="16"/>"#;
    // Text split by a comment is one unit; plain unit attributes are fine.
    let unit = first_packet(&text(float(
        r#"<Unit power="1" factor="1" form="calibrated">m<!-- per -->/s</Unit>"#,
        sixteen,
    )))["entries"][0]["unit"]
        .clone();
    assert_eq!(unit, "m/s");
    let (code, message) = one_problem(&text(float("<Unit>C</Unit>", sixteen)));
    assert_eq!(code, "xtce.ignored");
    assert!(
        message.starts_with("unit \"C\" isn't imported: UCUM reads it as the coulomb"),
        "{message}"
    );
    assert_eq!(
        one_problem(&text(float(
            "",
            r#"<IntegerDataEncoding sizeInBits="12" byteOrder="leastSignificantByteFirst"/>"#
        ))),
        (
            "xtce.unsupported".into(),
            "a little-endian value of 12 bits isn't supported: MDL reads little-endian values \
             wider than a byte as whole bytes"
                .into()
        )
    );
    assert_eq!(
        one_problem(&text(float(
            "",
            r#"<IntegerDataEncoding sizeInBits="32"><ByteOrderList><Byte byteSignificance="0"/><Byte byteSignificance="1"/></ByteOrderList></IntegerDataEncoding>"#
        ))),
        (
            "xtce.invalid".into(),
            "a ByteOrderList names each byte of the value, and this one names 2 for 32 bits".into()
        )
    );
}

#[test]
fn deep_nesting_is_refused_not_overflowed() {
    // SpaceSystems inside each other, far deeper than the limit.
    let depth = 5_000;
    let mut text = format!(r#"<SpaceSystem name="S0" xmlns="{XTCE}">"#);
    for level in 1..depth {
        text.push_str(&format!(r#"<SpaceSystem name="S{level}">"#));
    }
    text.push_str(&"</SpaceSystem>".repeat(depth));
    let (code, message) = one_problem(&text);
    assert_eq!(code, "xtce.unsupported");
    assert_eq!(
        message,
        "SpaceSystems nested more than 64 deep aren't supported"
    );
    // An aggregate holding an aggregate holding an aggregate, each its own
    // type, as deep.
    let mut types = String::from(U8);
    for level in 0..depth {
        let member = if level == 0 {
            "U8".to_string()
        } else {
            format!("A{}", level - 1)
        };
        types.push_str(&format!(
            r#"<AggregateParameterType name="A{level}"><MemberList><Member name="m" typeRef="{member}"/></MemberList></AggregateParameterType>"#
        ));
    }
    let using = |outer: usize| {
        doc(&telemetry(
            &types,
            &param("X", &format!("A{outer}")),
            &container("P", &entry("X")),
        ))
    };
    let (code, message) = one_problem(&using(depth - 1));
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with("aggregates and arrays nested more than 32 deep"),
        "{message}"
    );
    // 32 aggregates around a scalar are as deep as MDL goes; 33 aren't.
    let (sources, result) = import_all(&[("sat.xml", &using(31))]);
    let parsed = result.unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    let field = serde_json::to_value(&parsed.bundle.sets[0].packets[0].entries[0]).unwrap();
    assert_eq!(field["name"], format!("X{}", "_m".repeat(32)));
    assert_eq!(one_problem(&using(32)).0, "xtce.unsupported");
}

#[test]
fn options_name_the_family_and_set() {
    let text = doc(&telemetry(
        U8,
        &param("A", "U8"),
        &container("P", &entry("A")),
    ));
    let mut sources = Sources::new();
    sources.add("sat.xml", text);
    let options = Options::new()
        .with_family("vendor_x")
        .with_set("vendor_x_v1")
        .with_alias(mdl::ir::Locator::metadata("scid"));
    let parsed = read(sources.iter(), &options).unwrap();
    let bundle = serde_json::to_value(&parsed.bundle).unwrap();
    assert_eq!(bundle["families"][0]["name"], "vendor_x");
    assert_eq!(
        bundle["families"][0]["alias"],
        json!({ "kind": "metadata", "name": "scid" })
    );
    assert_eq!(bundle["sets"][0]["name"], "vendor_x_v1");
    // The family declares the metadata its alias reads, so it validates.
    assert_eq!(bundle["families"][0]["metadata"], json!(["scid"]));
    assert!(validate(parsed.bundle).is_ok());
    // Another alias declares none.
    let fixed = bundle_of(&sources, &Options::new());
    assert_eq!(
        fixed["families"][0]["alias"],
        json!({ "kind": "fixed", "value": "Sat" })
    );
    assert!(fixed["families"][0].get("metadata").is_none(), "{fixed:#}");
}

fn bundle_of(sources: &Sources, options: &Options) -> Value {
    serde_json::to_value(read(sources.iter(), options).unwrap().bundle).unwrap()
}

/// The packets of a document that imports without a warning and
/// validates, with the set's struct types.
fn valid(text: &str) -> (Value, Value) {
    let (sources, result) = import_all(&[("sat.xml", text)]);
    let parsed = result.unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    assert!(
        parsed.warnings.is_empty(),
        "{:#?}",
        shown(&sources, &parsed.warnings)
    );
    let json = serde_json::to_value(&parsed.bundle).unwrap();
    if let Err(diagnostics) = validate(parsed.bundle) {
        panic!(
            "{:#?}",
            shown(&sources, &parsed.map.locate_all(&diagnostics))
        );
    }
    (
        json["sets"][0]["packets"].clone(),
        json["sets"][0]["types"].clone(),
    )
}

const N: &str = r#"<Parameter name="N" parameterTypeRef="U8"/>"#;

fn array_type(name: &str, element: &str, ending: &str) -> String {
    format!(
        r#"<ArrayParameterType name="{name}" arrayTypeRef="{element}"><DimensionList><Dimension>
           <StartingIndex><FixedValue>0</FixedValue></StartingIndex>
           <EndingIndex>{ending}</EndingIndex></Dimension></DimensionList></ArrayParameterType>"#
    )
}

const BY_N: &str = r#"<DynamicValue><ParameterInstanceRef parameterRef="N"/><LinearAdjustment intercept="-1"/></DynamicValue>"#;

#[test]
fn arrays_count_their_elements_fixed_or_from_a_field() {
    let types = format!(
        "{U8}{U16}{}{}",
        array_type("Four", "U16", "<FixedValue>3</FixedValue>"),
        array_type("Some", "U16", BY_N)
    );
    let params = format!("{N}{}{}", param("A", "Four"), param("B", "Some"));
    let (packets, _) = valid(&doc(&telemetry(
        &types,
        &params,
        &container("P", &(entry("N") + &entry("A") + &entry("B"))),
    )));
    let element = json!({ "kind": "scalar", "type": "uint", "size_bits": 16 });
    assert_eq!(
        packets[0]["entries"][1],
        json!({ "kind": "array", "name": "A", "count": { "kind": "fixed", "count": 4 },
                "element": element, "canonical": "sat.a" })
    );
    // EndingIndex N - 1 from StartingIndex 0: N elements.
    assert_eq!(
        packets[0]["entries"][2]["count"],
        json!({ "kind": "field", "field": "N" })
    );
}

#[test]
fn an_array_of_aggregates_has_a_struct_type_for_its_elements() {
    let types = format!(
        r#"{U8}{U16}<AggregateParameterType name="Pair"><MemberList>
             <Member name="id" typeRef="U8"/><Member name="range" typeRef="U16"/>
           </MemberList></AggregateParameterType>{}"#,
        array_type("Targets", "Pair", BY_N)
    );
    let params = format!("{N}{}", param("T", "Targets"));
    let (packets, types) = valid(&doc(&telemetry(
        &types,
        &params,
        &container("P", &(entry("N") + &entry("T"))),
    )));
    assert_eq!(
        packets[0]["entries"][1]["element"],
        json!({ "kind": "struct", "name": "T_element" })
    );
    assert_eq!(
        types["T_element"]["entries"],
        json!([
            { "kind": "field", "name": "id", "type": "uint", "size_bits": 8,
              "canonical": "sat.t.id" },
            { "kind": "field", "name": "range", "type": "uint", "size_bits": 16,
              "canonical": "sat.t.range" }
        ])
    );
}

#[test]
fn an_xtce_1_1_array_takes_its_size_from_its_entry() {
    let types =
        format!(r#"{U8}<ArrayParameterType name="Old" arrayTypeRef="U8" numberOfDimensions="1"/>"#);
    let dimensioned = r#"<ArrayParameterRefEntry parameterRef="A"><DimensionList><Dimension>
        <StartingIndex><FixedValue>0</FixedValue></StartingIndex>
        <EndingIndex><FixedValue>1</FixedValue></EndingIndex></Dimension></DimensionList>
        </ArrayParameterRefEntry>"#;
    let text = |entries: &str| {
        doc(&telemetry(
            &types,
            &param("A", "Old"),
            &container("P", entries),
        ))
    };
    let (packets, _) = valid(&text(dimensioned));
    assert_eq!(
        packets[0]["entries"][0]["count"],
        json!({ "kind": "fixed", "count": 2 })
    );
    assert_eq!(one_problem(&text(&entry("A"))).0, "xtce.invalid");
    // Two dimensions, and a slice of an array, aren't MDL arrays.
    let square = format!(
        r#"{U8}<ArrayParameterType name="Square" arrayTypeRef="U8"><DimensionList>
           <Dimension><StartingIndex><FixedValue>0</FixedValue></StartingIndex><EndingIndex><FixedValue>1</FixedValue></EndingIndex></Dimension>
           <Dimension><StartingIndex><FixedValue>0</FixedValue></StartingIndex><EndingIndex><FixedValue>1</FixedValue></EndingIndex></Dimension>
           </DimensionList></ArrayParameterType>"#
    );
    let (code, message) = one_problem(&doc(&telemetry(
        &square,
        &param("A", "Square"),
        &container("P", &entry("A")),
    )));
    assert_eq!(code, "xtce.unsupported");
    assert!(message.starts_with("an array of 2 dimensions"), "{message}");
}

#[test]
fn a_repeated_parameter_is_an_array_of_its_samples() {
    let repeat = |count: &str, offset: &str| {
        format!(
            r#"<ParameterRefEntry parameterRef="V"><RepeatEntry><Count>{count}</Count>{offset}</RepeatEntry></ParameterRefEntry>"#
        )
    };
    let params = format!("{N}{}", param("V", "U16"));
    let text = |entry: String| {
        doc(&telemetry(
            &format!("{U8}{U16}"),
            &params,
            &container("P", &(super::tests::entry("N") + &entry)),
        ))
    };
    let (packets, _) = valid(&text(repeat("<FixedValue>3</FixedValue>", "")));
    assert_eq!(
        packets[0]["entries"][1],
        json!({ "kind": "array", "name": "V", "count": { "kind": "fixed", "count": 3 },
                "element": { "kind": "scalar", "type": "uint", "size_bits": 16 },
                "canonical": "sat.v" })
    );
    let dynamic = r#"<DynamicValue><ParameterInstanceRef parameterRef="N"/></DynamicValue>"#;
    let offset = "<Offset><FixedValue>8</FixedValue></Offset>";
    let (packets, types) = valid(&text(repeat(dynamic, offset)));
    assert_eq!(
        packets[0]["entries"][1]["count"],
        json!({ "kind": "field", "field": "N" })
    );
    // The gap after each sample is padding in a struct element, named for
    // its gap.
    assert_eq!(
        types["V_element_gap8"]["entries"],
        json!([
            { "kind": "field", "name": "value", "type": "uint", "size_bits": 16,
              "canonical": "sat.v" },
            { "kind": "padding", "size_bits": 8 }
        ])
    );
}

#[test]
fn an_embedded_container_is_a_struct_of_its_type() {
    // A secondary header present when the flag says so, as the CCSDS green
    // book writes one.
    let containers = format!(
        r#"<SequenceContainer name="Secondary" abstract="true"><EntryList>{}</EntryList></SequenceContainer>
           <SequenceContainer name="P"><EntryList>{}
             <ContainerRefEntry containerRef="Secondary">
               <IncludeCondition><Comparison parameterRef="Flag" value="1"/></IncludeCondition>
             </ContainerRefEntry>{}</EntryList></SequenceContainer>"#,
        entry("Time"),
        entry("Flag"),
        entry("Data")
    );
    let params = format!(
        "{}{}{}",
        param("Flag", "U8"),
        param("Time", "U16"),
        param("Data", "U8")
    );
    let (packets, types) = valid(&doc(&telemetry(
        &format!("{U8}{U16}"),
        &params,
        &containers,
    )));
    let p = packets
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "P")
        .unwrap();
    assert_eq!(
        p["entries"][1],
        json!({ "kind": "struct", "name": "Secondary", "struct": "Secondary",
                "when": { "kind": "compare", "field": "Flag", "op": "eq", "value": 1 } })
    );
    assert_eq!(types["Secondary"]["entries"][0]["canonical"], "sat.time");
}

#[test]
fn an_embedded_container_that_isnt_abstract_is_still_not_a_packet() {
    let text = doc(&telemetry(
        U8,
        &param("A", "U8"),
        &format!(
            r#"<SequenceContainer name="Inner">{}</SequenceContainer>
               <SequenceContainer name="P"><EntryList><ContainerRefEntry containerRef="Inner"/></EntryList></SequenceContainer>"#,
            format_args!("<EntryList>{}</EntryList>", entry("A"))
        ),
    ));
    assert_eq!(
        one_problem(&text),
        (
            "xtce.ignored".into(),
            "container /Sat/Inner isn't a packet: /Sat/P embeds it, so it's a structure; mark it \
             abstract to say so"
                .into()
        )
    );
}

#[test]
fn containers_embed_up_to_mdls_struct_nesting() {
    // C0 embeds C1 embeds ... embeds the last, which holds a field.
    let nested = |depth: usize| {
        let mut containers = String::new();
        for level in 0..depth {
            let inner = if level + 1 == depth {
                entry("A")
            } else {
                format!(r#"<ContainerRefEntry containerRef="C{}"/>"#, level + 1)
            };
            containers.push_str(&format!(
                r#"<SequenceContainer name="C{level}" abstract="true"><EntryList>{inner}</EntryList></SequenceContainer>"#
            ));
        }
        containers.push_str(
            r#"<SequenceContainer name="P"><EntryList><ContainerRefEntry containerRef="C0"/></EntryList></SequenceContainer>"#,
        );
        doc(&telemetry(U8, &param("A", "U8"), &containers))
    };
    valid(&nested(32));
    let (code, message) = one_problem(&nested(33));
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with("containers embedded more than 32 deep"),
        "{message}"
    );
}

#[test]
fn a_parameter_repeated_with_different_gaps_has_a_type_for_each() {
    let repeat = |gap: u32| {
        format!(
            r#"<ParameterRefEntry parameterRef="V"><RepeatEntry><Count><FixedValue>2</FixedValue></Count>
               <Offset><FixedValue>{gap}</FixedValue></Offset></RepeatEntry></ParameterRefEntry>"#
        )
    };
    let containers = format!(
        r#"<SequenceContainer name="Base" abstract="true"><EntryList>{}</EntryList></SequenceContainer>
           <SequenceContainer name="A"><EntryList>{}</EntryList><BaseContainer containerRef="Base"><RestrictionCriteria><Comparison parameterRef="Kind" value="1"/></RestrictionCriteria></BaseContainer></SequenceContainer>
           <SequenceContainer name="B"><EntryList>{}</EntryList><BaseContainer containerRef="Base"><RestrictionCriteria><Comparison parameterRef="Kind" value="2"/></RestrictionCriteria></BaseContainer></SequenceContainer>"#,
        entry("Kind"),
        repeat(8),
        repeat(16)
    );
    let params = format!("{}{}", param("Kind", "U8"), param("V", "U8"));
    let (packets, types) = valid(&doc(&telemetry(U8, &params, &containers)));
    assert_eq!(
        packets[0]["entries"][1]["element"]["name"],
        "V_element_gap8"
    );
    assert_eq!(
        packets[1]["entries"][1]["element"]["name"],
        "V_element_gap16"
    );
    assert_eq!(types["V_element_gap16"]["entries"][1]["size_bits"], 16);
}

#[test]
fn an_embedded_container_keeps_to_its_own_fields() {
    let text = |inner: &str, outer: &str| {
        doc(&telemetry(
            U8,
            &format!("{}{}", param("A", "U8"), param("B", "U8")),
            &format!(
                r#"<SequenceContainer name="Inner" abstract="true"><EntryList>{inner}</EntryList></SequenceContainer>
                   <SequenceContainer name="Base" abstract="true"><EntryList/></SequenceContainer>
                   <SequenceContainer name="P"><EntryList>{outer}</EntryList></SequenceContainer>"#
            ),
        ))
    };
    let embed = r#"<ContainerRefEntry containerRef="Inner"/>"#;
    // A condition inside on a field outside.
    let conditional = r#"<ParameterRefEntry parameterRef="B"><IncludeCondition><Comparison parameterRef="A" value="1"/></IncludeCondition></ParameterRefEntry>"#;
    let (code, message) = one_problem(&text(conditional, &format!("{}{embed}", entry("A"))));
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with("A isn't a field decoded earlier"),
        "{message}"
    );
    // The same container embedded twice maps its parameters twice.
    let (code, message) = one_problem(&text(&entry("A"), &format!("{embed}{embed}")));
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with("/Sat/A is in this packet twice"),
        "{message}"
    );
}

#[test]
fn sizes_come_from_earlier_fields_in_whole_bytes() {
    let binary = |adjustment: &str| {
        format!(
            r#"<BinaryParameterType name="Blob"><BinaryDataEncoding><SizeInBits><DynamicValue>
               <ParameterInstanceRef parameterRef="N"/>{adjustment}</DynamicValue></SizeInBits>
               </BinaryDataEncoding></BinaryParameterType>"#
        )
    };
    let text = |ty: String, entries: String| {
        doc(&telemetry(
            &format!("{U8}{ty}"),
            &format!("{N}{}", param("B", "Blob")),
            &container("P", &entries),
        ))
    };
    // N counts bytes: eight bits each.
    let (packets, _) = valid(&text(
        binary(r#"<LinearAdjustment slope="8"/>"#),
        entry("N") + &entry("B"),
    ));
    assert_eq!(
        packets[0]["entries"][1]["length"],
        json!({ "kind": "field", "field": "N" })
    );
    // N counting bits can't be a whole number of bytes.
    let (code, message) = one_problem(&text(binary(""), entry("N") + &entry("B")));
    assert_eq!(code, "xtce.unsupported");
    assert!(message.contains("isn't whole bytes"), "{message}");
    // The size must come first.
    let (code, message) = one_problem(&text(
        binary(r#"<LinearAdjustment slope="8"/>"#),
        entry("B") + &entry("N"),
    ));
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with("N isn't a field decoded earlier"),
        "{message}"
    );
}

#[test]
fn a_pascal_string_has_a_size_tag_before_it() {
    let string = |variable: &str| {
        format!(
            r#"<StringParameterType name="Text"><StringDataEncoding><Variable maxSizeInBits="2048">{variable}</Variable></StringDataEncoding></StringParameterType>"#
        )
    };
    let text = |variable: &str| {
        doc(&telemetry(
            &format!("{U8}{}", string(variable)),
            &param("S", "Text"),
            &container("P", &entry("S")),
        ))
    };
    let leading = r#"<DynamicValue><ParameterInstanceRef parameterRef="_yamcs_ignore"/></DynamicValue><LeadingSize sizeInBitsOfSizeTag="8"/>"#;
    let (sources, result) = import_all(&[("sat.xml", &text(leading))]);
    let parsed = result.unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    let packet = serde_json::to_value(&parsed.bundle.sets[0].packets[0]).unwrap();
    assert_eq!(
        packet["entries"],
        json!([
            { "kind": "field", "name": "S_size", "type": "uint", "size_bits": 8 },
            { "kind": "field", "name": "S", "type": "string",
              "length": { "kind": "field", "field": "S_size" }, "canonical": "sat.s" }
        ])
    );
    validate(parsed.bundle).unwrap();
    // _yamcs_ignore with a terminator is a terminated string.
    let terminated = r#"<DynamicValue><ParameterInstanceRef parameterRef="_yamcs_ignore"/></DynamicValue><TerminationChar>00</TerminationChar>"#;
    let (sources, result) = import_all(&[("sat.xml", &text(terminated))]);
    let parsed = result.unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    let packet = serde_json::to_value(&parsed.bundle.sets[0].packets[0]).unwrap();
    assert_eq!(
        packet["entries"][0]["length"],
        json!({ "kind": "terminator", "byte": 0 })
    );
}

#[test]
fn qualified_naming_names_everything_by_its_spacesystem() {
    let sub = format!(
        r#"<SpaceSystem name="EPS">{}</SpaceSystem>"#,
        telemetry(
            U8,
            &param("Status", "U8"),
            &container("HK", &entry("Status"))
        )
    );
    let mut sources = Sources::new();
    sources.add("sat.xml", doc(&sub));
    let options = Options::new().with_naming(crate::Naming::Qualified);
    let parsed = read(sources.iter(), &options).unwrap();
    assert!(
        parsed.warnings.is_empty(),
        "{:#?}",
        shown(&sources, &parsed.warnings)
    );
    let packet = serde_json::to_value(&parsed.bundle.sets[0].packets[0]).unwrap();
    assert_eq!(packet["name"], "EPS_HK");
    assert_eq!(packet["entries"][0]["name"], "EPS_Status");
}

/// An XTCE Condition: a parameter, an operator and a value.
fn condition(parameter: &str, op: &str, value: &str) -> String {
    format!(
        r#"<Condition><ParameterInstanceRef parameterRef="{parameter}"/><ComparisonOperator>{op}</ComparisonOperator><Value>{value}</Value></Condition>"#
    )
}

#[test]
fn boolean_expressions_are_nested_conditions() {
    let mode = r#"<EnumeratedParameterType name="Mode"><IntegerDataEncoding sizeInBits="8"/><EnumerationList>
        <Enumeration value="0" label="OFF"/><Enumeration value="5" label="ON"/></EnumerationList></EnumeratedParameterType>"#;
    let params = format!(
        "{}{}{}",
        param("M", "Mode"),
        param("N", "U8"),
        param("X", "U8")
    );
    let text = |expression: &str| {
        let conditional = format!(
            r#"<ParameterRefEntry parameterRef="X"><IncludeCondition><BooleanExpression>{expression}</BooleanExpression></IncludeCondition></ParameterRefEntry>"#
        );
        doc(&telemetry(
            &format!("{mode}{U8}"),
            &params,
            &container("P", &format!("{}{}{conditional}", entry("M"), entry("N"))),
        ))
    };
    let expression = format!(
        "<ANDedConditions>{}<ORedConditions>{}{}</ORedConditions></ANDedConditions>",
        condition("M", "==", "ON"),
        condition("N", "&gt;=", "16"),
        condition("N", "==", "1")
    );
    let compare = |field: &str, op: &str, value: i64| json!({ "kind": "compare", "field": field, "op": op, "value": value });
    let (packets, _) = valid(&text(&expression));
    assert_eq!(
        packets[0]["entries"][2]["when"],
        json!({ "kind": "all", "conditions": [
            compare("M", "eq", 5),
            { "kind": "any", "conditions": [compare("N", "ge", 16), compare("N", "eq", 1)] }
        ] })
    );
    // MDL compares a field with a value, not with another field.
    let two = r#"<Condition><ParameterInstanceRef parameterRef="N"/><ComparisonOperator>==</ComparisonOperator><ParameterInstanceRef parameterRef="M"/></Condition>"#;
    assert_eq!(
        one_problem(&text(two)).1,
        "a comparison of two parameters isn't supported: MDL compares a field with a value"
    );
}

#[test]
fn a_boolean_expression_identifies_by_what_it_ands() {
    let params = format!("{}{}", param("Kind", "U8"), param("Sub", "U8"));
    let text = |expression: &str| {
        doc(&telemetry(
            U8,
            &params,
            &format!(
                r#"<SequenceContainer name="Base" abstract="true"><EntryList>{}{}</EntryList></SequenceContainer>
                   <SequenceContainer name="P"><EntryList/><BaseContainer containerRef="Base"><RestrictionCriteria>
                   <BooleanExpression>{expression}</BooleanExpression></RestrictionCriteria></BaseContainer></SequenceContainer>"#,
                entry("Kind"),
                entry("Sub")
            ),
        ))
    };
    let anded = format!(
        "<ANDedConditions>{}{}</ANDedConditions>",
        condition("Kind", "==", "3"),
        condition("Sub", "==", "4")
    );
    let (packets, _) = valid(&text(&anded));
    assert_eq!(packets[0]["identify"], json!({ "Kind": 3, "Sub": 4 }));
    let ored = format!(
        "<ORedConditions>{}{}</ORedConditions>",
        condition("Kind", "==", "3"),
        condition("Kind", "==", "4")
    );
    assert_eq!(
        one_problem(&text(&ored)),
        (
            "xtce.unsupported".into(),
            "identification by an OR isn't supported: MDL identifies a packet by values that \
             must all match"
                .into()
        )
    );
}

#[test]
fn criteria_hold_one_comparison_list_or_expression() {
    let params = format!("{}{}", param("K", "U8"), param("J", "U8"));
    let identified = |criteria: &str| {
        doc(&telemetry(
            U8,
            &params,
            &format!(
                r#"<SequenceContainer name="Head" abstract="true"><EntryList>{}{}</EntryList></SequenceContainer>
                   <SequenceContainer name="P"><EntryList/><BaseContainer containerRef="Head"><RestrictionCriteria>
                   {criteria}</RestrictionCriteria></BaseContainer></SequenceContainer>"#,
                entry("K"),
                entry("J")
            ),
        ))
    };
    let k = r#"<Comparison parameterRef="K" value="1"/>"#;
    let j = r#"<Comparison parameterRef="J" value="2"/>"#;
    let one = (
        "xtce.invalid".to_string(),
        "RestrictionCriteria holds one Comparison, ComparisonList or BooleanExpression".to_string(),
    );
    assert_eq!(one_problem(&identified(&format!("{k}{j}"))), one);
    assert_eq!(
        one_problem(&identified(&format!(
            "{k}<ComparisonList>{j}</ComparisonList>"
        ))),
        one
    );
    // An empty expression or list would identify by nothing.
    let invalid = |message: &str| ("xtce.invalid".to_string(), message.to_string());
    assert_eq!(
        one_problem(&identified("<BooleanExpression/>")),
        invalid("BooleanExpression holds conditions")
    );
    assert_eq!(
        one_problem(&identified(
            "<BooleanExpression><ANDedConditions/></BooleanExpression>"
        )),
        invalid("ANDedConditions holds conditions")
    );
    assert_eq!(
        one_problem(&identified("<ComparisonList/>")),
        invalid("a ComparisonList holds comparisons")
    );
    let beside = format!(
        "<BooleanExpression>{}<ANDedConditions>{}</ANDedConditions></BooleanExpression>",
        condition("K", "==", "1"),
        condition("J", "==", "2")
    );
    assert_eq!(
        one_problem(&identified(&beside)),
        invalid("a BooleanExpression holds one Condition, ANDedConditions or ORedConditions")
    );
    // An OR of one condition is that condition.
    let lone = format!(
        "<BooleanExpression><ANDedConditions>{}<ORedConditions>{}</ORedConditions></ANDedConditions></BooleanExpression>",
        condition("K", "==", "1"),
        condition("J", "==", "2")
    );
    let (packets, _) = valid(&identified(&lone));
    assert_eq!(packets[0]["identify"], json!({ "K": 1, "J": 2 }));
    // An IncludeCondition is read the same way.
    let included = |criteria: &str| {
        let conditional = format!(
            r#"<ParameterRefEntry parameterRef="X"><IncludeCondition>{criteria}</IncludeCondition></ParameterRefEntry>"#
        );
        doc(&telemetry(
            U8,
            &format!("{params}{}", param("X", "U8")),
            &container("P", &format!("{}{}{conditional}", entry("K"), entry("J"))),
        ))
    };
    assert_eq!(
        one_problem(&included(&format!("{k}{j}"))),
        invalid("IncludeCondition holds one Comparison, ComparisonList or BooleanExpression")
    );
    assert_eq!(
        one_problem(&included("<ComparisonList/>")),
        invalid("a ComparisonList holds comparisons")
    );
}

#[test]
fn alarm_ranges_are_limits() {
    let float = |alarm: &str| {
        format!(
            r#"<FloatParameterType name="T"><UnitSet><Unit>V</Unit></UnitSet><IntegerDataEncoding sizeInBits="16"/>
               <DefaultAlarm>{alarm}</DefaultAlarm></FloatParameterType>"#
        )
    };
    let text = |alarm: &str| {
        doc(&telemetry(
            &float(alarm),
            &param("X", "T"),
            &container("P", &entry("X")),
        ))
    };
    let ranges = r#"<StaticAlarmRanges>
        <WarningRange minInclusive="24" maxInclusive="32"/>
        <CriticalRange minInclusive="20" maxInclusive="36"/>
      </StaticAlarmRanges>"#;
    let (sources, result) = import_all(&[("sat.xml", &text(ranges))]);
    let parsed = result.unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    let catalog = serde_json::to_value(&parsed.bundle.catalog).unwrap();
    assert_eq!(
        catalog["params"]["sat.x"]["limits"],
        json!({ "red_low": 20.0, "yellow_low": 24.0, "yellow_high": 32.0, "red_high": 36.0 })
    );
    validate(parsed.bundle).unwrap();
    // What MDL's two levels and thresholds don't hold is a warning.
    let messages = |alarm: &str| -> Vec<String> {
        let (sources, result) = import_all(&[("sat.xml", &text(alarm))]);
        let parsed = result.unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
        shown(&sources, &parsed.warnings)
            .into_iter()
            .map(|(_, _, _, message)| message)
            .collect()
    };
    assert_eq!(
        messages(
            r#"<StaticAlarmRanges><WatchRange maxInclusive="30"/><WarningRange maxExclusive="32"/></StaticAlarmRanges>"#
        ),
        [
            "StaticAlarmRanges's WatchRange isn't imported: MDL limits have two levels, yellow and \
             red (XTCE's warning and critical)",
            "maxExclusive is imported as a limit, but not as exclusive: MDL limits don't say \
             whether a value at the limit is in alarm",
        ]
    );
    assert_eq!(
        messages(
            r#"<StaticAlarmRanges rangeForm="inside"><WarningRange maxInclusive="32"/></StaticAlarmRanges>"#
        ),
        [
            "alarm ranges that are in alarm inside them aren't imported: MDL limits are thresholds a \
          value goes beyond"
        ]
    );
}

#[test]
fn a_relative_time_is_a_number_of_its_unit() {
    let relative = |encoding: &str, units: &str| {
        format!(
            r#"<RelativeTimeParameterType name="T"><Encoding {units}>{encoding}</Encoding></RelativeTimeParameterType>"#
        )
    };
    let text = |ty: String| {
        doc(&telemetry(
            &ty,
            &param("X", "T"),
            &container("P", &entry("X")),
        ))
    };
    let sixteen = r#"<IntegerDataEncoding sizeInBits="16"/>"#;
    let (packets, _) = valid(&text(relative(sixteen, r#"scale="0.001""#)));
    assert_eq!(
        packets[0]["entries"][0],
        json!({ "kind": "field", "name": "X", "type": "uint", "size_bits": 16,
                "conversion": { "kind": "linear", "scale": 0.001, "offset": 0.0 },
                "unit": "s", "canonical": "sat.x" })
    );
    let (packets, _) = valid(&text(relative(sixteen, r#"units="days""#)));
    assert_eq!(packets[0]["entries"][0]["unit"], "d");
    assert_eq!(packets[0]["entries"][0].get("conversion"), None);
    let (code, message) = one_problem(&text(relative(sixteen, r#"units="months""#)));
    assert_eq!(code, "xtce.ignored");
    assert!(
        message.starts_with("a duration in months isn't imported with a unit"),
        "{message}"
    );
    let referenced = r#"<RelativeTimeParameterType name="T"><Encoding><IntegerDataEncoding sizeInBits="16"/></Encoding>
        <ReferenceTime><Epoch>TAI</Epoch></ReferenceTime></RelativeTimeParameterType>"#;
    assert_eq!(
        one_problem(&text(referenced.to_string())).0,
        "xtce.unsupported"
    );
}

#[test]
fn deep_boolean_expressions_are_refused_not_overflowed() {
    // ANDs inside ORs inside ANDs, thousands deep.
    let depth = 5_000;
    let mut expression = condition("A", "==", "1");
    for level in 0..depth {
        let (kind, other) = if level % 2 == 0 {
            ("ANDedConditions", condition("A", "==", "2"))
        } else {
            ("ORedConditions", condition("A", "==", "3"))
        };
        expression = format!("<{kind}>{expression}{other}</{kind}>");
    }
    let conditional = format!(
        r#"<ParameterRefEntry parameterRef="B"><IncludeCondition><BooleanExpression>{expression}</BooleanExpression></IncludeCondition></ParameterRefEntry>"#
    );
    let params = format!("{}{}", param("A", "U8"), param("B", "U8"));
    let text = doc(&telemetry(
        U8,
        &params,
        &container("P", &(entry("A") + &conditional)),
    ));
    let (code, message) = one_problem(&text);
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with("conditions nested more than 32 deep"),
        "{message}"
    );
    // ANDs only, as identification reads them.
    let mut anded = condition("A", "==", "1");
    for _ in 0..depth {
        anded = format!(
            "<ANDedConditions>{anded}{}</ANDedConditions>",
            condition("A", "==", "1")
        );
    }
    let identify = doc(&telemetry(
        U8,
        &param("A", "U8"),
        &format!(
            r#"<SequenceContainer name="Base" abstract="true"><EntryList>{}</EntryList></SequenceContainer>
               <SequenceContainer name="P"><EntryList/><BaseContainer containerRef="Base"><RestrictionCriteria>
               <BooleanExpression>{anded}</BooleanExpression></RestrictionCriteria></BaseContainer></SequenceContainer>"#,
            entry("A")
        ),
    ));
    let (code, message) = one_problem(&identify);
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with("conditions nested more than 32 deep"),
        "{message}"
    );
}

#[test]
fn every_code_is_listed_once_under_the_importers_name() {
    let mut seen = std::collections::BTreeSet::new();
    for code in Code::ALL {
        assert!(code.as_str().starts_with("xtce."), "{code:?}");
        assert!(seen.insert(code.as_str()), "{code:?}");
    }
}

/// A CommandMetaData with these argument types and meta-commands.
fn commands(types: &str, metas: &str) -> String {
    format!(
        "<CommandMetaData><ArgumentTypeSet>{types}</ArgumentTypeSet>\
         <MetaCommandSet>{metas}</MetaCommandSet></CommandMetaData>"
    )
}

const ARG_U8: &str = r#"<IntegerArgumentType name="U8" signed="false"><IntegerDataEncoding sizeInBits="8"/></IntegerArgumentType>"#;

/// An abstract meta-command `Base` writing a sync word and an opcode
/// argument, and a concrete one, `Go`, built on it with opcode 7, holding
/// `arguments` and `entries`.
fn go(arguments: &str, entries: &str) -> String {
    format!(
        r#"<MetaCommand name="Base" abstract="true">
             <ArgumentList><Argument name="Opcode" argumentTypeRef="U8"/></ArgumentList>
             <CommandContainer name="Base"><EntryList>
               <FixedValueEntry name="Sync" binaryValue="EB90" sizeInBits="16"/>
               <ArgumentRefEntry argumentRef="Opcode"/>
             </EntryList></CommandContainer>
           </MetaCommand>
           <MetaCommand name="Go">
             <BaseMetaCommand metaCommandRef="Base"><ArgumentAssignmentList>
               <ArgumentAssignment argumentName="Opcode" argumentValue="7"/>
             </ArgumentAssignmentList></BaseMetaCommand>
             <ArgumentList>{arguments}</ArgumentList>
             <CommandContainer name="Go"><EntryList>{entries}</EntryList></CommandContainer>
           </MetaCommand>"#
    )
}

fn argument(name: &str, ty: &str) -> String {
    format!(r#"<Argument name="{name}" argumentTypeRef="{ty}"/>"#)
}

fn argument_entry(name: &str) -> String {
    format!(r#"<ArgumentRefEntry argumentRef="{name}"/>"#)
}

/// `Go` with one argument `A` of a type `types` defines as `ty`.
fn go_with(types: &str, ty: &str) -> String {
    doc(&commands(
        &format!("{ARG_U8}{types}"),
        &go(&argument("A", ty), &argument_entry("A")),
    ))
}

#[test]
fn meta_commands_are_catalog_commands_and_command_packets() {
    let types = format!(
        r#"{ARG_U8}
           <EnumeratedArgumentType name="Mode"><IntegerDataEncoding sizeInBits="8"/><EnumerationList><Enumeration value="1" label="SAFE"/><Enumeration value="2" label="ON"/></EnumerationList></EnumeratedArgumentType>
           <IntegerArgumentType name="Delay" signed="false" initialValue="0x0A"><UnitSet><Unit>s</Unit></UnitSet><IntegerDataEncoding sizeInBits="16"/><ValidRangeSet><ValidRange minInclusive="1" maxInclusive="600"/></ValidRangeSet></IntegerArgumentType>"#
    );
    let arguments = r#"<Argument name="Mode" argumentTypeRef="Mode" shortDescription="Where to go"/>
        <Argument name="DelayTime" argumentTypeRef="Delay"/>"#;
    let entries = format!(
        r#"{}<ArgumentRefEntry argumentRef="DelayTime"><LocationInContainerInBits><FixedValue>4</FixedValue></LocationInContainerInBits></ArgumentRefEntry>"#,
        argument_entry("Mode")
    );
    let text = doc(&commands(&types, &go(arguments, &entries)).replace(
        r#"<MetaCommand name="Go">"#,
        r#"<MetaCommand name="Go" shortDescription="Start">"#,
    ));
    let bundle = bundle(&text);
    assert_eq!(
        bundle["catalog"]["commands"],
        json!({
            "sat.go": {
                "description": "Start",
                "arguments": [
                    {"name": "mode", "type": "enum", "states": ["SAFE", "ON"], "description": "Where to go"},
                    {"name": "delaytime", "type": "u64", "unit": "s", "range": {"min": 1, "max": 600}, "default": 10}
                ]
            }
        })
    );
    assert_eq!(
        bundle["sets"][0]["commands"],
        json!([{
            "name": "Go",
            "canonical": "sat.go",
            "entries": [
                {"kind": "field", "name": "Sync", "type": "uint", "size_bits": 16, "value": 60304},
                {"kind": "field", "name": "Opcode", "type": "uint", "size_bits": 8, "value": 7},
                {"kind": "field", "name": "Mode", "type": "uint", "size_bits": 8,
                 "conversion": {"kind": "states", "states": [{"raw": 1, "name": "SAFE"}, {"raw": 2, "name": "ON"}]},
                 "argument": "mode"},
                {"kind": "padding", "size_bits": 4},
                {"kind": "field", "name": "DelayTime", "type": "uint", "size_bits": 16, "unit": "s", "argument": "delaytime"}
            ]
        }])
    );
    let (_, result) = import_all(&[("sat.xml", &text)]);
    validate(result.unwrap().bundle).unwrap();
}

#[test]
fn a_referring_import_maps_onto_a_catalog_it_doesnt_define() {
    let alias =
        |name: &str| format!(r#"<AliasSet><Alias nameSpace="MDL" alias="{name}"/></AliasSet>"#);
    let params = format!(
        r#"{}<Parameter name="Temp" parameterTypeRef="U8">{}</Parameter><Parameter name="TempB" parameterTypeRef="U8">{}</Parameter>{}"#,
        param("Kind", "U8"),
        alias("eps.battery_tmp"),
        alias("eps.battery_tmp"),
        param("Volts", "U8"),
    );
    // P and Q, told apart by Kind.
    let derived = |name: &str, kind: u8, entries: &str| {
        format!(
            r#"<SequenceContainer name="{name}"><EntryList>{entries}</EntryList><BaseContainer containerRef="Base"><RestrictionCriteria><Comparison parameterRef="Kind" value="{kind}"/></RestrictionCriteria></BaseContainer></SequenceContainer>"#
        )
    };
    let containers = format!(
        r#"<SequenceContainer name="Base" abstract="true"><EntryList>{}</EntryList></SequenceContainer>{}{}"#,
        entry("Kind"),
        derived("P", 1, &(entry("Temp") + &entry("Volts"))),
        derived("Q", 2, &entry("TempB")),
    );
    let stop = r#"<MetaCommand name="Stop"><CommandContainer name="Stop"><EntryList>
        <FixedValueEntry name="Sync" binaryValue="EB90" sizeInBits="16"/>
        </EntryList></CommandContainer></MetaCommand>"#;
    let metas = go(
        &(argument("A", "Delay") + &argument("B", "U8") + &argument("C", "Gain")),
        &(argument_entry("A") + &argument_entry("B") + &argument_entry("C")),
    )
    .replace(
        r#"<MetaCommand name="Go">"#,
        &format!(r#"<MetaCommand name="Go">{}"#, alias("sat.go")),
    ) + stop;
    let delay = r#"<IntegerArgumentType name="Delay" signed="false" initialValue="5"><UnitSet><Unit>s</Unit></UnitSet><IntegerDataEncoding sizeInBits="8"/><ValidRangeSet><ValidRange minInclusive="0" maxInclusive="10"/></ValidRangeSet></IntegerArgumentType>"#;
    // A float written as an 8-bit integer: its valid range states a
    // minimum, and its maximum is worked out from the encoding.
    let gain = r#"<FloatArgumentType name="Gain"><IntegerDataEncoding sizeInBits="8"/><ValidRangeSet><ValidRange minInclusive="1"/></ValidRangeSet></FloatArgumentType>"#;
    let text = doc(&(telemetry(U8, &params, &containers)
        + &commands(&format!("{ARG_U8}{delay}{gain}"), &metas)));
    let mut sources = Sources::new();
    sources.add("sat.xml", text.as_str());
    let options = Options::new().with_catalog(CatalogRole::Refer);
    // The owner's. Its command allows A more than A's 8-bit field holds,
    // which the vendor's range keeps it to, and B any u64.
    let owners = catalog_of(json!({
        "params": {"eps.battery_temp": {"type": "u64"}},
        "commands": {"sat.go": {"arguments": [
            {"name": "a", "type": "u64", "unit": "s", "range": {"min": 0, "max": 1000}},
            {"name": "b", "type": "u64"},
            {"name": "c", "type": "f64", "range": {"min": 0, "max": 10}}
        ]}}
    }));
    let parsed = read_with(
        sources.iter(),
        &options,
        Context::new().with_catalog(&owners),
    )
    .unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    // Where an offset in the document is, as `shown` puts it.
    let at = |offset: Option<usize>| {
        let location = sources.iter().next().unwrap().location(offset.unwrap());
        format!("{}:{}", location.line, location.column)
    };
    // A meta-command without an alias is left out, named. (Gain's type
    // gets the warning any import of it gets: Yamcs drops the fraction MDL
    // rounds.)
    let warnings = shown(&sources, &parsed.warnings);
    assert_eq!(warnings.len(), 2, "{warnings:#?}");
    assert_eq!(warnings[0].1, "xtce.differs");
    assert_eq!(
        warnings[1],
        (
            Severity::Warning,
            "xtce.ignored".to_string(),
            at(text.find(r#"<MetaCommand name="Stop">"#)),
            "meta-command /Sat/Stop isn't imported: the catalog is another input's, and without \
             an alias in the MDL namespace it names no command there"
                .to_string()
        )
    );
    // Nothing is added to the catalog. Fields in two packets map to one
    // name; a parameter without an alias maps to none.
    let mut bundle = parsed.bundle;
    let json = serde_json::to_value(&bundle).unwrap();
    assert_eq!(json["catalog"], json!({}));
    let packets = &json["sets"][0]["packets"];
    let canonical =
        |packet: usize, entry: usize| packets[packet]["entries"][entry].get("canonical");
    assert_eq!(canonical(0, 0), None, "{packets:#}");
    assert_eq!(
        canonical(0, 1),
        Some(&json!("eps.battery_tmp")),
        "{packets:#}"
    );
    assert_eq!(canonical(0, 2), None, "{packets:#}");
    assert_eq!(
        canonical(1, 1),
        Some(&json!("eps.battery_tmp")),
        "{packets:#}"
    );
    let commands = &json["sets"][0]["commands"];
    assert_eq!(commands.as_array().map(Vec::len), Some(1), "{commands:#}");
    assert_eq!(commands[0]["canonical"], "sat.go");
    assert_eq!(commands[0]["entries"][2]["argument"], "a");
    // The vendor's range and initial value are the packet's own (IR 2.2),
    // since the owner's command sets the catalog's, in A's unit, which is
    // the catalog's, so it isn't written. B's type states neither, and C's
    // only a minimum: what an encoding holds isn't a limit the vendor
    // stated.
    assert_eq!(
        commands[0]["arguments"],
        json!({
            "a": { "range": { "min": 0, "max": 10 }, "default": 5 },
            "c": { "range": { "min": 1 } }
        })
    );
    assert_eq!(json["mdl_version"], "2.2");
    // Against the owner's catalog, the misspelt alias is a problem,
    // reported where each parameter has it, and so is B, which nothing
    // here narrows to what its field holds.
    bundle.catalog = owners;
    let diagnostics = parsed.map.locate_all(&validate(bundle).unwrap_err());
    let typo = |offset| {
        (
            Severity::Error,
            "unknown_canonical".to_string(),
            at(offset),
            "param eps.battery_tmp isn't in the catalog; did you mean eps.battery_temp?"
                .to_string(),
        )
    };
    assert_eq!(
        shown(&sources, &diagnostics),
        [
            (
                Severity::Error,
                "value_range".to_string(),
                at(text
                    .find(r#"argumentRef="B""#)
                    .map(|start| start + r#"argumentRef=""#.len())),
                "argument b has no range, so it can be any u64, but field B holds raw 0 to \
                 255; give the argument, or this packet's arguments, a range it can hold"
                    .to_string()
            ),
            typo(text.find("eps.battery_tmp")),
            typo(text.rfind("eps.battery_tmp")),
        ]
    );
}

#[test]
fn a_command_inherits_arguments_and_entries_root_first() {
    // Three levels: the root declares the APID and the opcode, the middle
    // assigns the APID and adds a flag, the leaf assigns the opcode and
    // writes its own argument.
    let metas = r#"
      <MetaCommand name="Root" abstract="true">
        <ArgumentList><Argument name="Apid" argumentTypeRef="U8"/><Argument name="Op" argumentTypeRef="U8"/></ArgumentList>
        <CommandContainer name="Root"><EntryList>
          <ArgumentRefEntry argumentRef="Apid"/><ArgumentRefEntry argumentRef="Op"/>
        </EntryList></CommandContainer>
      </MetaCommand>
      <MetaCommand name="Mid" abstract="true">
        <BaseMetaCommand metaCommandRef="Root"><ArgumentAssignmentList>
          <ArgumentAssignment argumentName="Apid" argumentValue="0x2A"/>
        </ArgumentAssignmentList></BaseMetaCommand>
        <ArgumentList><Argument name="Flag" argumentTypeRef="U8"/></ArgumentList>
        <CommandContainer name="Mid"><EntryList><ArgumentRefEntry argumentRef="Flag"/></EntryList>
          <BaseContainer containerRef="Root"/></CommandContainer>
      </MetaCommand>
      <MetaCommand name="Leaf">
        <BaseMetaCommand metaCommandRef="Mid"><ArgumentAssignmentList>
          <ArgumentAssignment argumentName="Op" argumentValue="3"/>
        </ArgumentAssignmentList></BaseMetaCommand>
        <ArgumentList><Argument name="Level" argumentTypeRef="U8"/></ArgumentList>
        <CommandContainer name="Leaf"><EntryList><ArgumentRefEntry argumentRef="Level"/></EntryList>
          <BaseContainer containerRef="Mid"/></CommandContainer>
      </MetaCommand>
      <MetaCommand name="Other">
        <BaseMetaCommand metaCommandRef="Root"/>
      </MetaCommand>"#;
    let bundle = bundle(&doc(&commands(ARG_U8, metas)));
    let arguments = |command: &str| -> Vec<String> {
        bundle["catalog"]["commands"][command]["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(arguments("sat.leaf"), ["flag", "level"]);
    assert_eq!(arguments("sat.other"), ["apid", "op"]);
    let leaf = &bundle["sets"][0]["commands"][0];
    let entries: Vec<(&str, &Value)> = leaf["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap(),
                e.get("value").unwrap_or(&e["argument"]),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            ("Apid", &json!(42)),
            ("Op", &json!(3)),
            ("Flag", &json!("flag")),
            ("Level", &json!("level"))
        ]
    );
}

#[test]
fn fixed_values_are_right_aligned_bit_patterns() {
    let entries = r#"
        <FixedValueEntry binaryValue="5" sizeInBits="3"/>
        <FixedValueEntry name="Word" binaryValue="0102"/>
        <FixedValueEntry name="Wide" binaryValue="00890123456789ABCDEF" sizeInBits="72"/>
        <FixedValueEntry binaryValue="00" sizeInBits="5"/>"#;
    let text = doc(&commands(ARG_U8, &go("", entries)));
    let packet = &bundle(&text)["sets"][0]["commands"][0];
    let fields: Vec<(&str, u64, &Value, Option<&str>)> = packet["entries"]
        .as_array()
        .unwrap()
        .iter()
        .skip(2)
        .map(|e| {
            (
                e["name"].as_str().unwrap(),
                e["size_bits"].as_u64().unwrap(),
                &e["value"],
                e["byte_order"].as_str(),
            )
        })
        .collect();
    // The family is big-endian (no number says otherwise), so no field
    // names its order; the 72 bits are 8, then 64.
    assert_eq!(
        fields,
        [
            ("fixed1", 3, &json!(5), None),
            ("Word", 16, &json!(258), None),
            ("Wide_1", 8, &json!(0x89), None),
            ("Wide_2", 64, &json!("81985529216486895"), None),
            ("fixed2", 5, &json!(0), None),
        ]
    );
}

/// Sign-magnitude and ones' complement integers import as MDL's `signed`
/// forms, and the bundle says the IR version that has them. An argument
/// with no valid range takes what its field holds, one negative value
/// fewer than two's complement.
#[test]
fn sign_magnitude_and_ones_complement_integers_are_signed_forms() {
    let types = r#"<IntegerParameterType name="SM" signed="true"><IntegerDataEncoding sizeInBits="12" encoding="signMagnitude"/></IntegerParameterType>
        <IntegerParameterType name="OC" signed="true"><IntegerDataEncoding sizeInBits="8" encoding="onesComplement"/></IntegerParameterType>"#;
    let params = format!("{}{}", param("Trim", "SM"), param("Bias", "OC"));
    let text = doc(&telemetry(
        types,
        &params,
        &container("P", &format!("{}{}", entry("Trim"), entry("Bias"))),
    ));
    let imported = bundle(&text);
    assert_eq!(imported["mdl_version"], "2.1");
    let entries = &imported["sets"][0]["packets"][0]["entries"];
    assert_eq!(
        (
            &entries[0]["type"],
            &entries[0]["signed"],
            &entries[0]["size_bits"]
        ),
        (&json!("int"), &json!("sign_magnitude"), &json!(12))
    );
    assert_eq!(entries[1]["signed"], "ones_complement");

    let argument_type = r#"<IntegerArgumentType name="SM" signed="true" sizeInBits="8"><IntegerDataEncoding sizeInBits="8" encoding="signMagnitude"/></IntegerArgumentType>"#;
    let text = doc(&commands(
        &format!("{ARG_U8}{argument_type}"),
        &go(&argument("A", "SM"), &argument_entry("A")),
    ));
    let commanded = bundle(&text);
    let argument = &commanded["catalog"]["commands"]["sat.go"]["arguments"][0];
    assert_eq!(
        argument["range"],
        json!({ "min": -127, "max": 127 }),
        "{argument:#}"
    );

    // Without either, the bundle stays at 2.0.
    let plain = doc(&telemetry(
        U8,
        &param("A", "U8"),
        &container("P", &entry("A")),
    ));
    assert_eq!(bundle(&plain)["mdl_version"], "2.0");
}

#[test]
fn fixed_values_in_a_little_endian_family_name_their_order() {
    let types = r#"<IntegerArgumentType name="LE" signed="false"><IntegerDataEncoding sizeInBits="16" byteOrder="leastSignificantByteFirst"/></IntegerArgumentType>"#;
    let text = doc(&commands(
        &format!("{ARG_U8}{types}"),
        &go(&argument("A", "LE"), &argument_entry("A")),
    ));
    let bundle = bundle(&text);
    assert_eq!(bundle["families"][0]["byte_order"], "little");
    let sync = &bundle["sets"][0]["commands"][0]["entries"][0];
    assert_eq!(
        (&sync["name"], &sync["byte_order"]),
        (&json!("Sync"), &json!("big"))
    );
}

#[test]
fn a_fixed_value_needs_its_bits_and_drops_higher_ones() {
    let text = |entry: &str| doc(&commands(ARG_U8, &go("", entry)));
    assert_eq!(
        one_error(&text(
            r#"<FixedValueEntry binaryValue="01" sizeInBits="12"/>"#
        )),
        (
            "xtce.invalid".into(),
            "binaryValue 01 has 8 bits, fewer than the entry's 12".into()
        )
    );
    assert_eq!(
        one_error(&text(r#"<FixedValueEntry binaryValue="0xFF"/>"#)),
        (
            "xtce.invalid".into(),
            "binaryValue \"0xFF\" isn't hexadecimal".into()
        )
    );
    assert_eq!(
        one_problem(&text(
            r#"<FixedValueEntry binaryValue="FF" sizeInBits="3"/>"#
        )),
        (
            "xtce.ignored".into(),
            "binaryValue FF's bits above its low 3 aren't imported: the entry is 3 bits, and \
             Yamcs drops them too"
                .into()
        )
    );
}

#[test]
fn string_and_binary_arguments_pad_as_yamcs_pads_them() {
    let length = |ty: &str| {
        let text = go_with(ty, "A");
        bundle(&text)["sets"][0]["commands"][0]["entries"][2]["length"].clone()
    };
    let string = |size: &str| {
        format!(
            r#"<StringArgumentType name="A"><StringDataEncoding><{size}</StringDataEncoding></StringArgumentType>"#
        )
    };
    assert_eq!(
        length(&string(
            "SizeInBits><Fixed><FixedValue>32</FixedValue></Fixed></SizeInBits>"
        )),
        json!({"kind": "fixed", "bytes": 4, "terminator": 0})
    );
    assert_eq!(
        length(&string(
            "SizeInBits><Fixed><FixedValue>32</FixedValue></Fixed><TerminationChar>00</TerminationChar></SizeInBits>"
        )),
        json!({"kind": "fixed", "bytes": 4, "terminator": 0})
    );
    assert_eq!(
        length(&string(
            "Variable><TerminationChar>0A</TerminationChar></Variable>"
        )),
        json!({"kind": "terminator", "byte": 10})
    );
    assert_eq!(
        length(
            r#"<BinaryArgumentType name="A"><BinaryDataEncoding><SizeInBits><FixedValue>16</FixedValue></SizeInBits></BinaryDataEncoding></BinaryArgumentType>"#
        ),
        json!({"kind": "fixed", "bytes": 2})
    );
    assert_eq!(
        one_error(&go_with(
            &string("SizeInBits><Fixed><FixedValue>32</FixedValue></Fixed><TerminationChar>20</TerminationChar></SizeInBits>"),
            "A"
        ))
        .0,
        "xtce.unsupported"
    );
    // A Pascal string: a size tag MDL works out, then the text.
    let text = go_with(
        &string("SizeInBits><LeadingSize sizeInBitsOfSizeTag=\"8\"/></SizeInBits>"),
        "A",
    );
    let entries = bundle(&text)["sets"][0]["commands"][0]["entries"].clone();
    assert_eq!(
        (&entries[2]["name"], entries[2].get("argument")),
        (&json!("A_size"), None)
    );
    assert_eq!(
        entries[3]["length"],
        json!({"kind": "field", "field": "A_size"})
    );
}

#[test]
fn eight_bit_characters_are_text_and_wider_ones_unsupported() {
    let string = |width: &str| {
        format!(
            r#"<StringParameterType name="Name" characterWidth="{width}"><StringDataEncoding><SizeInBits><Fixed><FixedValue>32</FixedValue></Fixed></SizeInBits></StringDataEncoding></StringParameterType>"#
        )
    };
    let text = |width: &str| {
        doc(&telemetry(
            &string(width),
            &param("N", "Name"),
            &container("P", &entry("N")),
        ))
    };
    assert_eq!(
        one_problem(&text("8")),
        (
            "xtce.ignored".into(),
            "StringParameterType Name's characterWidth isn't imported: MDL reads text by its \
             encoding"
                .into()
        )
    );
    assert_eq!(
        one_error(&text("16")),
        (
            "xtce.unsupported".into(),
            "characters of 16 bits aren't supported: MDL reads text as UTF-8, US-ASCII or \
             ISO-8859-1 bytes"
                .into()
        )
    );
}

#[test]
fn an_enumeration_label_on_two_values_is_encoded_as_the_first() {
    let ty = r#"<EnumeratedArgumentType name="A"><IntegerDataEncoding sizeInBits="8"/><EnumerationList>
        <Enumeration value="1" label="ON"/><Enumeration value="2" label="OFF"/><Enumeration value="3" label="ON"/>
      </EnumerationList></EnumeratedArgumentType>"#;
    let text = go_with(ty, "A");
    assert_eq!(
        one_problem(&text),
        (
            "xtce.ignored".into(),
            "label ON stands for 3 too, which isn't imported: an argument is encoded as its \
             label's first value, as Yamcs encodes it"
                .into()
        )
    );
    let (_, result) = import_all(&[("sat.xml", &text)]);
    let bundle = serde_json::to_value(result.unwrap().bundle).unwrap();
    assert_eq!(
        bundle["sets"][0]["commands"][0]["entries"][2]["conversion"]["states"],
        json!([{"raw": 1, "name": "ON"}, {"raw": 2, "name": "OFF"}])
    );
}

#[test]
fn an_enumeration_value_past_2_to_the_53_is_a_decimal_string() {
    let ty = r#"<EnumeratedArgumentType name="A"><IntegerDataEncoding sizeInBits="64"/><EnumerationList>
        <Enumeration value="1" label="ON"/><Enumeration value="9007199254740993" label="OFF"/>
      </EnumerationList></EnumeratedArgumentType>"#;
    let (_, result) = import_all(&[("sat.xml", &go_with(ty, "A"))]);
    let imported = result.unwrap().bundle;
    let bundle = serde_json::to_value(&imported).unwrap();
    assert_eq!(
        bundle["sets"][0]["commands"][0]["entries"][2]["conversion"]["states"],
        json!([{"raw": 1, "name": "ON"}, {"raw": "9007199254740993", "name": "OFF"}])
    );
    assert!(validate(imported).is_ok());
}

#[test]
fn where_yamcs_writes_a_float_argument_otherwise_is_a_warning() {
    let float = |attrs: &str, encoding: &str| {
        format!(r#"<FloatArgumentType name="A"{attrs}>{encoding}</FloatArgumentType>"#)
    };
    let (code, message) = one_problem(&go_with(
        &float("", r#"<IntegerDataEncoding sizeInBits="16"/>"#),
        "A",
    ));
    assert_eq!(code, "xtce.differs");
    assert!(message.starts_with("Yamcs drops the fraction"), "{message}");
    let (code, message) = one_problem(&go_with(
        &float("", r#"<FloatDataEncoding sizeInBits="64"/>"#),
        "A",
    ));
    assert_eq!(code, "xtce.differs");
    assert!(message.starts_with("Yamcs narrows a value"), "{message}");
    bundle(&go_with(
        &float(
            r#" sizeInBits="64""#,
            r#"<FloatDataEncoding sizeInBits="64"/>"#,
        ),
        "A",
    ));
    bundle(&go_with(
        &float("", r#"<FloatDataEncoding sizeInBits="32"/>"#),
        "A",
    ));
}

#[test]
fn valid_ranges_are_inclusive_ranges() {
    let range = |ty: &str| {
        bundle(&go_with(ty, "A"))["catalog"]["commands"]["sat.go"]["arguments"][0]["range"].clone()
    };
    let float = |range: &str| {
        format!(
            r#"<FloatArgumentType name="A" sizeInBits="64"><FloatDataEncoding sizeInBits="64"/>{range}</FloatArgumentType>"#
        )
    };
    assert_eq!(
        range(&float(
            r#"<ValidRangeSet><ValidRange minExclusive="0" maxExclusive="1"/></ValidRangeSet>"#
        )),
        json!({"min": 0.0_f64.next_up(), "max": 1.0_f64.next_down()})
    );
    // XTCE 1.0 and 1.1 put the range on the type.
    assert_eq!(
        range(&float(r#"<ValidRange maxInclusive="2.5"/>"#)),
        json!({"max": 2.5})
    );
    let integer = |range: &str| {
        format!(
            r#"<IntegerArgumentType name="A"><IntegerDataEncoding sizeInBits="64" encoding="twosComplement"/>{range}</IntegerArgumentType>"#
        )
    };
    assert_eq!(
        range(&integer(
            r#"<ValidRangeSet><ValidRange minInclusive="-5" maxInclusive="9007199254740991"/></ValidRangeSet>"#
        )),
        json!({"min": -5, "max": 9_007_199_254_740_991_i64})
    );
    assert_eq!(
        one_problem(&go_with(
            &float(
                r#"<ValidRangeSet><ValidRange minInclusive="0" minExclusive="0"/></ValidRangeSet>"#
            ),
            "A"
        )),
        (
            "xtce.ignored".into(),
            "minExclusive isn't imported: the range has minInclusive too, which Yamcs takes".into()
        )
    );
    // Past ±(2^53 - 1), an integer end is exact, written as a decimal
    // string.
    assert_eq!(
        range(&integer(
            r#"<ValidRangeSet><ValidRange minInclusive="-9223372036854775807" maxInclusive="9007199254740993"/></ValidRangeSet>"#
        )),
        json!({"min": "-9223372036854775807", "max": "9007199254740993"})
    );
    for (range, code) in [
        (
            r#"<ValidRangeSet><ValidRange minInclusive="0"/><ValidRange maxInclusive="9"/></ValidRangeSet>"#,
            "xtce.unsupported",
        ),
        (
            r#"<ValidRangeSet><ValidRange minInclusive="0"/></ValidRangeSet><ValidRange minInclusive="0"/>"#,
            "xtce.invalid",
        ),
    ] {
        assert_eq!(one_error(&go_with(&integer(range), "A")).0, code, "{range}");
    }
}

/// The catalog range of `Go`'s one argument `A`, from a document that
/// imports, validates, and warns only as `warnings` says.
fn validated_range(ty: &str, warnings: &[&str]) -> Value {
    let text = go_with(ty, "A");
    let (sources, result) = import_all(&[("sat.xml", &text)]);
    let parsed = result.unwrap_or_else(|d| panic!("{:#?}", shown(&sources, &d)));
    let codes: Vec<String> = parsed.warnings.iter().map(|w| w.code.to_string()).collect();
    assert_eq!(codes, warnings, "{:#?}", shown(&sources, &parsed.warnings));
    let bundle = serde_json::to_value(&parsed.bundle).unwrap();
    validate(parsed.bundle).unwrap();
    bundle["catalog"]["commands"]["sat.go"]["arguments"][0]
        .get("range")
        .cloned()
        .unwrap_or(Value::Null)
}

#[test]
fn an_integer_argument_without_a_valid_range_takes_what_its_field_holds() {
    let integer = |attrs: &str, encoding: &str| {
        format!(r#"<IntegerArgumentType name="A"{attrs}>{encoding}</IntegerArgumentType>"#)
    };
    // What the encoding holds (a u64 starts at 0 already)...
    assert_eq!(
        validated_range(
            &integer(
                r#" signed="false""#,
                r#"<IntegerDataEncoding sizeInBits="16"/>"#
            ),
            &[]
        ),
        json!({"max": 65535})
    );
    assert_eq!(
        validated_range(
            &integer(
                "",
                r#"<IntegerDataEncoding sizeInBits="12" encoding="twosComplement"/>"#
            ),
            &[]
        ),
        json!({"min": -2048, "max": 2047})
    );
    // An unsigned type over a two's complement encoding is an i64 kept at 0
    // and above, since a command writes a u64 to a uint field only.
    let unsigned_int = integer(
        r#" signed="false""#,
        r#"<IntegerDataEncoding sizeInBits="8" encoding="twosComplement"/>"#,
    );
    assert_eq!(
        validated_range(&unsigned_int, &[]),
        json!({"min": 0, "max": 127})
    );
    assert_eq!(
        bundle(&go_with(&unsigned_int, "A"))["catalog"]["commands"]["sat.go"]["arguments"][0]["type"],
        "i64"
    );
    // ...and the type: 32 signed bits unless it says otherwise, which
    // Yamcs checks a value against.
    assert_eq!(
        validated_range(
            &integer("", r#"<IntegerDataEncoding sizeInBits="64"/>"#),
            &[]
        ),
        json!({"min": 0, "max": 2_147_483_647})
    );
    // Where those are the catalog type's own, no range is needed.
    assert_eq!(
        validated_range(
            &integer(
                r#" signed="false" sizeInBits="64""#,
                r#"<IntegerDataEncoding sizeInBits="64"/>"#
            ),
            &[]
        ),
        Value::Null
    );
    assert_eq!(
        validated_range(
            &integer(
                r#" sizeInBits="64""#,
                r#"<IntegerDataEncoding sizeInBits="64" encoding="twosComplement"/>"#
            ),
            &[]
        ),
        Value::Null
    );
    // A field wider than 53 bits keeps its top values: the range's end is
    // exact, a decimal string.
    assert_eq!(
        validated_range(
            &integer(
                r#" signed="false" sizeInBits="64""#,
                r#"<IntegerDataEncoding sizeInBits="60"/>"#
            ),
            &[]
        ),
        json!({"max": "1152921504606846975"})
    );
}

#[test]
fn a_float_written_as_an_integer_takes_what_rounds_into_its_field() {
    let float = |range: &str| {
        format!(
            r#"<FloatArgumentType name="A"><IntegerDataEncoding sizeInBits="8"/>{range}</FloatArgumentType>"#
        )
    };
    let differs = ["xtce.differs"];
    // From just above -0.5 to just below 255.5.
    assert_eq!(
        validated_range(&float(""), &differs),
        json!({"min": (-0.5_f64).next_up(), "max": 255.5_f64.next_down()})
    );
    // Yamcs keeps 255.9 in the byte, dropping the fraction; MDL rounds it
    // to 256, so the range is narrowed to what MDL keeps.
    let (sources, result) = import_all(&[(
        "sat.xml",
        &go_with(
            &float(r#"<ValidRange minInclusive="0" maxExclusive="256"/>"#),
            "A",
        ),
    )]);
    let messages: Vec<String> = shown(&sources, &result.unwrap().warnings)
        .into_iter()
        .map(|(_, _, _, message)| message)
        .collect();
    assert_eq!(
        messages[1],
        "the valid range's maximum, 255.99999999999997, is narrowed to 255.49999999999997: \
         MDL rounds the values past it to 256, which the encoding can't hold, where Yamcs \
         drops their fraction"
    );
    assert_eq!(
        validated_range(
            &float(r#"<ValidRange minInclusive="0" maxExclusive="256"/>"#),
            &["xtce.differs", "xtce.differs"]
        ),
        json!({"min": 0, "max": 255.5_f64.next_down()})
    );
    // A range the byte can't hold either way is the document's to fix.
    let text = go_with(&float(r#"<ValidRange maxInclusive="300"/>"#), "A");
    let (_, result) = import_all(&[("sat.xml", &text)]);
    assert!(validate(result.unwrap().bundle).is_err());
}

#[test]
fn initial_values_are_defaults_read_as_yamcs_reads_them() {
    let default = |ty: &str, argument: &str| {
        let text = doc(&commands(
            &format!("{ARG_U8}{ty}"),
            &go(argument, &argument_entry("A")),
        ));
        bundle(&text)["catalog"]["commands"]["sat.go"]["arguments"][0]["default"].clone()
    };
    let int = r#"<IntegerArgumentType name="T" initialValue="0b1_0000"><IntegerDataEncoding sizeInBits="16"/></IntegerArgumentType>"#;
    assert_eq!(default(int, &argument("A", "T")), json!(16));
    // The argument's initial value is used over its type's.
    assert_eq!(
        default(
            int,
            r#"<Argument name="A" argumentTypeRef="T" initialValue="-3"/>"#
        ),
        json!(-3)
    );
    let boolean = r#"<BooleanArgumentType name="T" oneStringValue="ON" zeroStringValue="OFF"><IntegerDataEncoding sizeInBits="8"/></BooleanArgumentType>"#;
    let text = doc(&commands(
        &format!("{ARG_U8}{boolean}"),
        &go(
            r#"<Argument name="A" argumentTypeRef="T" initialValue="OFF"/>"#,
            &argument_entry("A"),
        ),
    ));
    let (_, result) = import_all(&[("sat.xml", &text)]);
    let bundle = serde_json::to_value(result.unwrap().bundle).unwrap();
    assert_eq!(
        bundle["catalog"]["commands"]["sat.go"]["arguments"][0]["default"],
        json!(false)
    );
    let binary = r#"<BinaryArgumentType name="T" initialValue="ABC"><BinaryDataEncoding><SizeInBits><FixedValue>16</FixedValue></SizeInBits></BinaryDataEncoding></BinaryArgumentType>"#;
    assert_eq!(default(binary, &argument("A", "T")), json!("0abc"));
    let float = r#"<FloatArgumentType name="T" sizeInBits="64" initialValue="2.5"><FloatDataEncoding sizeInBits="64"/></FloatArgumentType>"#;
    assert_eq!(default(float, &argument("A", "T")), json!(2.5));
    // A whole number past i64 is a double like any other.
    let wide = float.replace("2.5", "1e21");
    assert_eq!(default(&wide, &argument("A", "T")), json!(1e21));
    let enumerated = r#"<EnumeratedArgumentType name="T" initialValue="Off"><IntegerDataEncoding sizeInBits="8"/><EnumerationList><Enumeration value="0" label="OFF"/></EnumerationList></EnumeratedArgumentType>"#;
    assert_eq!(
        one_error(&doc(&commands(
            &format!("{ARG_U8}{enumerated}"),
            &go(&argument("A", "T"), &argument_entry("A")),
        ))),
        (
            "xtce.invalid".into(),
            "argument A's initial value: \"Off\" isn't one of its labels (OFF)".into()
        )
    );
}

#[test]
fn an_argument_no_entry_writes_is_left_out() {
    let text = doc(&commands(
        ARG_U8,
        &go(
            &(argument("A", "U8") + &argument("Note", "U8")),
            &argument_entry("A"),
        ),
    ));
    assert_eq!(
        one_problem(&text),
        (
            "xtce.ignored".into(),
            "argument Note of meta-command /Sat/Go is in no entry, so it isn't imported: it \
             changes no bytes"
                .into()
        )
    );
}

#[test]
fn assignments_name_inherited_arguments_once() {
    let assigning = |assignments: &str| {
        doc(&commands(
            ARG_U8,
            &go("", "").replace(
                r#"<ArgumentAssignment argumentName="Opcode" argumentValue="7"/>"#,
                assignments,
            ),
        ))
    };
    assert_eq!(
        one_error(&assigning(
            r#"<ArgumentAssignment argumentName="Op" argumentValue="7"/>"#
        )),
        (
            "xtce.reference".into(),
            "Op names no argument that meta-command /Sat/Go inherits".into()
        )
    );
    assert_eq!(
        one_error(&assigning(
            r#"<ArgumentAssignment argumentName="Opcode" argumentValue="7"/><ArgumentAssignment argumentName="Opcode" argumentValue="8"/>"#
        )),
        (
            "xtce.invalid".into(),
            "argument Opcode is given a value already".into()
        )
    );
    assert_eq!(
        one_error(&assigning(
            r#"<ArgumentAssignment argumentName="Opcode" argumentValue="seven"/>"#
        )),
        (
            "xtce.invalid".into(),
            "the value assigned to argument Opcode: \"seven\" isn't an integer".into()
        )
    );
    // A float's assigned value is unsupported, however it's spelled.
    let float = |value: &str| {
        doc(&commands(
            r#"<FloatArgumentType name="U8"><IntegerDataEncoding sizeInBits="8"/></FloatArgumentType>"#,
            &go("", "").replace(
                r#"argumentValue="7""#,
                &format!("argumentValue=\"{value}\""),
            ),
        ))
    };
    for value in ["2.5", "3"] {
        assert_eq!(
            one_error(&float(value)),
            (
                "xtce.unsupported".into(),
                "a value assigned to argument Opcode isn't supported: MDL constants are \
                 integers and text, and the argument is a float"
                    .into()
            ),
            "{value}"
        );
    }
    // Integers read as defaults are, and checked against the valid range.
    let ranged = |value: &str| {
        doc(&commands(
            r#"<IntegerArgumentType name="U8" signed="false"><IntegerDataEncoding sizeInBits="8"/><ValidRangeSet><ValidRange minInclusive="1" maxInclusive="9"/></ValidRangeSet></IntegerArgumentType>"#,
            &go("", "").replace(
                r#"argumentValue="7""#,
                &format!("argumentValue=\"{value}\""),
            ),
        ))
    };
    let packet = &bundle(&ranged("0b1_000"))["sets"][0]["commands"][0];
    assert_eq!(packet["entries"][1]["value"], json!(8));
    assert_eq!(
        one_error(&ranged("10")),
        (
            "xtce.invalid".into(),
            "the value assigned to argument Opcode, 10, is above its maximum, 9, so Yamcs \
             refuses the command"
                .into()
        )
    );
}

#[test]
fn an_argument_entry_names_its_own_meta_commands_argument() {
    let text = doc(&commands(ARG_U8, &go("", &argument_entry("Opcode"))));
    assert_eq!(
        one_error(&text),
        (
            "xtce.reference".into(),
            "Opcode names no argument of meta-command /Sat/Go; an ArgumentRefEntry names an \
             argument its own meta-command declares, as Yamcs reads it"
                .into()
        )
    );
    let twice = doc(&commands(
        ARG_U8,
        &go(
            &argument("A", "U8"),
            &(argument_entry("A") + &argument_entry("A")),
        ),
    ));
    assert_eq!(one_error(&twice).0, "xtce.unsupported");
}

#[test]
fn a_command_container_is_built_on_its_inherited_meta_commands() {
    // Base is the meta-command Go inherits; Other is a meta-command it
    // doesn't, and Tm a telemetry container.
    let other = r#"<MetaCommand name="Other" abstract="true"><CommandContainer name="Other"><EntryList/></CommandContainer></MetaCommand>"#;
    let based = |base: &str| {
        let metas = go("", "").replace(
            "<EntryList></EntryList></CommandContainer>",
            &format!("<EntryList></EntryList>{base}</CommandContainer>"),
        );
        doc(&format!(
            "{}{}",
            telemetry(U8, &param("P", "U8"), &container("Tm", &entry("P"))),
            commands(ARG_U8, &format!("{metas}{other}"))
        ))
    };
    bundle(&based(r#"<BaseContainer containerRef="Base"/>"#));
    bundle(&based(r#"<BaseContainer containerRef="/Sat/Base"/>"#));
    let (code, message) = one_error(&based(r#"<BaseContainer containerRef="Other"/>"#));
    assert_eq!(code, "xtce.unsupported");
    assert!(message.contains("build it on Base"), "{message}");
    // A reference resolves before it's compared, so a path naming nothing
    // isn't taken for the container its last part names.
    assert_eq!(
        one_error(&based(r#"<BaseContainer containerRef="/Nowhere/Base"/>"#)).0,
        "xtce.reference"
    );
    assert_eq!(
        one_error(&based(r#"<BaseContainer containerRef="Tm"/>"#)),
        (
            "xtce.unsupported".into(),
            "a command container built on a telemetry container isn't supported".into()
        )
    );
    let rootless = doc(&commands(
        ARG_U8,
        &format!(
            r#"{other}<MetaCommand name="Go"><CommandContainer name="Go"><EntryList/><BaseContainer containerRef="Other"/></CommandContainer></MetaCommand>"#
        ),
    ));
    let (code, message) = one_error(&rootless);
    assert_eq!(code, "xtce.unsupported");
    assert!(message.contains("supported only when"), "{message}");
}

#[test]
fn what_a_command_packet_cant_hold_is_unsupported() {
    for (entry, message) in [
        (
            r#"<ContainerRefEntry containerRef="C"/>"#,
            "ContainerRefEntry in a command isn't supported: an MDL command packet holds \
             arguments and constants",
        ),
        (
            r#"<FixedValueEntry binaryValue="01"><IncludeCondition><Comparison parameterRef="P" value="1"/></IncludeCondition></FixedValueEntry>"#,
            "an entry of a command on a condition isn't supported: every entry of an MDL \
             command packet is written",
        ),
        (
            r#"<FixedValueEntry binaryValue="01"><RepeatEntry><Count><FixedValue>2</FixedValue></Count></RepeatEntry></FixedValueEntry>"#,
            "a repeated entry in a command isn't supported: an MDL list is an argument of its own",
        ),
    ] {
        let text = doc(&commands(ARG_U8, &go("", entry)));
        assert_eq!(
            one_error(&text),
            ("xtce.unsupported".into(), message.into()),
            "{entry}"
        );
    }
    assert_eq!(
        one_error(&go_with(r#"<AbsoluteTimeArgumentType name="A"/>"#, "A")).0,
        "xtce.unsupported"
    );
}

#[test]
fn command_and_argument_names() {
    let text = doc(&commands(
        ARG_U8,
        &go(
            r#"<Argument name="Heater-Id" argumentTypeRef="U8"/><Argument name="X" argumentTypeRef="U8"><AliasSet><Alias nameSpace="MDL" alias="level"/></AliasSet></Argument>"#,
            &(argument_entry("Heater-Id") + &argument_entry("X")),
        ),
    ));
    let (sources, result) = import_all(&[("sat.xml", &text)]);
    let parsed = result.unwrap();
    assert_eq!(
        shown(&sources, &parsed.warnings)
            .into_iter()
            .map(|(_, code, _, message)| (code, message))
            .collect::<Vec<_>>(),
        [(
            "xtce.renamed".to_string(),
            "argument Heater-Id of meta-command /Sat/Go is field Heater_Id: MDL names are \
             letters, digits and '_'"
                .to_string()
        )]
    );
    let bundle = serde_json::to_value(parsed.bundle).unwrap();
    let names: Vec<&str> = bundle["catalog"]["commands"]["sat.go"]["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["heater_id", "level"]);

    // A command's catalog name is the catalog's, which params share.
    let clash = doc(&format!(
        "{}{}",
        telemetry(U8, &param("Go", "U8"), &container("P", &entry("Go"))),
        commands(ARG_U8, &go("", ""))
    ));
    assert_eq!(
        one_error(&clash),
        (
            "xtce.name".into(),
            "meta-command /Sat/Go and parameter /Sat/Go both have catalog name sat.go".into()
        )
    );
    let looped = doc(&commands(
        ARG_U8,
        r#"<MetaCommand name="A"><BaseMetaCommand metaCommandRef="B"/></MetaCommand>
           <MetaCommand name="B" abstract="true"><BaseMetaCommand metaCommandRef="A"/></MetaCommand>"#,
    ));
    assert_eq!(
        one_error(&looped),
        (
            "xtce.invalid".into(),
            "meta-command A inherits from itself".into()
        )
    );
}

/// The catalog argument of a one-argument command `Go`, its field, and the
/// codes of the warnings, from a document that validates.
fn validated_argument(ty: &str) -> (Value, Value, Vec<String>) {
    let text = go_with(ty, "A");
    let (sources, result) = import_all(&[("sat.xml", &text)]);
    let parsed = result.unwrap_or_else(|d| panic!("{:#?}", shown(&sources, &d)));
    let codes = parsed.warnings.iter().map(|w| w.code.to_string()).collect();
    let bundle = serde_json::to_value(&parsed.bundle).unwrap();
    if let Err(d) = validate(parsed.bundle) {
        panic!("{:#?}", shown(&sources, &parsed.map.locate_all(&d)));
    }
    (
        bundle["catalog"]["commands"]["sat.go"]["arguments"][0].clone(),
        bundle["sets"][0]["commands"][0]["entries"][2].clone(),
        codes,
    )
}

/// A float argument type `A` written in 16 unsigned bits through
/// `calibrator`, with `rest` after the encoding.
fn calibrated(calibrator: &str, rest: &str) -> String {
    format!(
        r#"<FloatArgumentType name="A"><IntegerDataEncoding sizeInBits="16"><DefaultCalibrator>{calibrator}</DefaultCalibrator></IntegerDataEncoding>{rest}</FloatArgumentType>"#
    )
}

const CENTI: &str = r#"<PolynomialCalibrator><Term coefficient="0" exponent="0"/><Term coefficient="100" exponent="1"/></PolynomialCalibrator>"#;

#[test]
fn an_argument_calibrator_is_the_inverse_and_its_inverse_the_conversion() {
    // Yamcs writes 100 v: that's the field's inverse as written, and the
    // conversion is its inverse, for decoding a command.
    let (argument, field, warnings) = validated_argument(&calibrated(
        CENTI,
        r#"<ValidRangeSet><ValidRange minInclusive="0" maxInclusive="5"/></ValidRangeSet>"#,
    ));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(argument["range"], json!({"min": 0, "max": 5}));
    assert_eq!(
        field["conversion"],
        json!({"kind": "poly", "coefficients": [0.0, 0.01]})
    );
    assert_eq!(
        field["inverse"],
        json!({"kind": "linear", "scale": 100.0, "offset": 0.0})
    );
    // Without a range, what the field holds: raw 0 to 65535, rounded.
    let (argument, _, _) = validated_argument(&calibrated(CENTI, ""));
    let (min, max) = (
        argument["range"]["min"].as_f64().unwrap(),
        argument["range"]["max"].as_f64().unwrap(),
    );
    assert_eq!(
        ((100.0 * min).round(), (100.0 * max).round()),
        (0.0, 65535.0)
    );
    assert!((100.0 * min.next_down()).round() < 0.0);
    assert!((100.0 * max.next_up()).round() > 65535.0);
}

#[test]
fn a_spline_that_only_rises_or_falls_is_a_table_read_backwards() {
    // Engineering 0 to 10 is raw 1000 down to 0, extrapolating.
    let falling = r#"<SplineCalibrator extrapolate="true"><SplinePoint raw="0" calibrated="1000"/><SplinePoint raw="10" calibrated="0"/></SplineCalibrator>"#;
    let (argument, field, warnings) = validated_argument(&calibrated(falling, ""));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        field["conversion"],
        json!({"kind": "table", "points": [{"raw": 0.0, "value": 10.0}, {"raw": 1000.0, "value": 0.0}], "extrapolate": true})
    );
    assert_eq!(field.get("inverse"), None);
    let max = argument["range"]["max"].as_f64().unwrap();
    assert!(max > 10.0, "extrapolated down to raw 0: {max}");
    // Yamcs extrapolates whatever the spline says; MDL keeps to the points.
    let kept = falling.replace(r#" extrapolate="true""#, "");
    let (argument, _, warnings) = validated_argument(&calibrated(&kept, ""));
    assert_eq!(warnings, ["xtce.differs"]);
    assert_eq!(argument["range"], json!({"min": 0, "max": 10}));
    for (spline, message) in [
        (
            r#"<SplineCalibrator order="0"><SplinePoint raw="0" calibrated="0"/><SplinePoint raw="1" calibrated="5"/></SplineCalibrator>"#,
            "an order 0 spline on an argument",
        ),
        (
            r#"<SplineCalibrator><SplinePoint raw="0" calibrated="0"/><SplinePoint raw="1" calibrated="5"/><SplinePoint raw="2" calibrated="1"/></SplineCalibrator>"#,
            "an argument spline that doesn't only rise or only fall",
        ),
        (
            r#"<PolynomialCalibrator><Term coefficient="1" exponent="2"/></PolynomialCalibrator>"#,
            "an argument calibrator of degree 2 or more",
        ),
        (
            r#"<PolynomialCalibrator><Term coefficient="3" exponent="0"/></PolynomialCalibrator>"#,
            "a constant argument calibrator",
        ),
    ] {
        let (code, text) = one_error(&go_with(&calibrated(spline, ""), "A"));
        assert_eq!(code, "xtce.unsupported", "{spline}");
        assert!(text.starts_with(message), "{text}");
    }
}

#[test]
fn a_range_on_raw_values_is_the_engineering_values_that_give_them() {
    // Raw 0 to 1000 through 100 v is engineering 0 to 10, give or take
    // the rounding at each end.
    let (argument, _, warnings) = validated_argument(&calibrated(
        CENTI,
        r#"<ValidRangeSet validRangeAppliesToCalibrated="false"><ValidRange minInclusive="0" maxInclusive="1000"/></ValidRangeSet>"#,
    ));
    assert_eq!(warnings, ["xtce.differs"]);
    let (min, max) = (
        argument["range"]["min"].as_f64().unwrap(),
        argument["range"]["max"].as_f64().unwrap(),
    );
    assert_eq!(
        ((100.0 * min).round(), (100.0 * max).round()),
        (0.0, 1000.0)
    );
    assert!((100.0 * max.next_up()).round() > 1000.0);
}

/// The limits a referring import gives the packet of `Go`, whose argument
/// `A` is of type `ty`.
fn referred_limits(ty: &str) -> Value {
    let text = go_with(ty, "A").replace(
        r#"<MetaCommand name="Go">"#,
        r#"<MetaCommand name="Go"><AliasSet><Alias nameSpace="MDL" alias="sat.go"/></AliasSet>"#,
    );
    let mut sources = Sources::new();
    sources.add("sat.xml", text.as_str());
    let options = Options::new().with_catalog(CatalogRole::Refer);
    let catalog = catalog_of(json!({
        "commands": {"sat.go": {"arguments": [{"name": "a", "type": "f64"}]}}
    }));
    let parsed = read_with(
        sources.iter(),
        &options,
        Context::new().with_catalog(&catalog),
    )
    .unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    serde_json::to_value(&parsed.bundle).unwrap()["sets"][0]["commands"][0]["arguments"].clone()
}

fn catalog_of(value: Value) -> mdl::ir::Catalog {
    serde_json::from_value(value).unwrap()
}

#[test]
fn a_referring_import_limits_an_argument_to_the_ends_its_type_states() {
    let polynomial = |slope: i32| {
        format!(
            r#"<PolynomialCalibrator><Term coefficient="0" exponent="0"/><Term coefficient="{slope}" exponent="1"/></PolynomialCalibrator>"#
        )
    };
    let raw = |ends: &str| {
        format!(
            r#"<ValidRangeSet validRangeAppliesToCalibrated="false"><ValidRange {ends}/></ValidRangeSet>"#
        )
    };
    let ends = |limits: &Value| {
        let range = &limits["a"]["range"];
        (range["min"].as_f64(), range["max"].as_f64())
    };
    // Raw 10 and up: engineering 9.5 and up where raw rises with it, and
    // -9.5 and down where it falls. The other end is what the encoding
    // holds, which isn't stated.
    let rising = referred_limits(&calibrated(&polynomial(1), &raw(r#"minInclusive="10""#)));
    assert_eq!(ends(&rising), (Some(9.5), None), "{rising}");
    let falling = referred_limits(&calibrated(&polynomial(-1), &raw(r#"minInclusive="10""#)));
    assert_eq!(ends(&falling), (None, Some(-9.5)), "{falling}");
    // Both raw ends state both engineering ends, whichever way.
    let both = referred_limits(&calibrated(
        &polynomial(-1),
        &raw(r#"minInclusive="10" maxInclusive="20""#),
    ));
    // (Raw 20.5 would round away from zero to 21, so the low end is just
    // inside -20.5.)
    let (min, max) = ends(&both);
    assert!(
        min.is_some_and(|min| (-20.5..-20.49).contains(&min)),
        "{both}"
    );
    assert_eq!(max, Some(-9.5), "{both}");
    // A type that states no range gives no limit.
    assert_eq!(
        referred_limits(&calibrated(&polynomial(1), "")),
        Value::Null
    );
}

#[test]
fn a_falling_spline_takes_a_range_on_raw_values_backwards() {
    // Raw 1000 down to 0 over engineering 0 to 10: raw 200 to 800 is
    // engineering 2 to 8, give or take the rounding at each end.
    let falling = r#"<SplineCalibrator extrapolate="true"><SplinePoint raw="0" calibrated="1000"/><SplinePoint raw="10" calibrated="0"/></SplineCalibrator>"#;
    let (argument, _, warnings) = validated_argument(&calibrated(
        falling,
        r#"<ValidRangeSet validRangeAppliesToCalibrated="false"><ValidRange minInclusive="200" maxInclusive="800"/></ValidRangeSet>"#,
    ));
    assert_eq!(warnings, ["xtce.differs"]);
    let (min, max) = (
        argument["range"]["min"].as_f64().unwrap(),
        argument["range"]["max"].as_f64().unwrap(),
    );
    assert!(
        (1.99..2.0).contains(&min) && (8.0..8.01).contains(&max),
        "{min} {max}"
    );
    // A given end past everything the encoding holds.
    let (code, message) = one_error(&go_with(
        &calibrated(
            CENTI,
            r#"<ValidRangeSet><ValidRange minInclusive="700"/></ValidRangeSet>"#,
        ),
        "A",
    ));
    assert_eq!(code, "xtce.invalid");
    assert!(
        message.starts_with("the valid range's end, 700, lies past every value"),
        "{message}"
    );
}

#[test]
fn a_relative_time_argument_is_a_duration_with_a_linear_conversion() {
    let ty = r#"<RelativeTimeAgumentType name="A" initialValue="PT1.5S"><Encoding units="seconds" scale="0.001"><IntegerDataEncoding sizeInBits="32"/></Encoding></RelativeTimeAgumentType>"#;
    let (argument, field, warnings) = validated_argument(ty);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        (&argument["type"], &argument["unit"], &argument["default"]),
        (&json!("f64"), &json!("s"), &json!(1.5))
    );
    assert_eq!(
        field["conversion"],
        json!({"kind": "linear", "scale": 0.001, "offset": 0.0})
    );
    assert!(argument["range"]["max"].as_f64().unwrap() > 4_294_967.0);
    let bad = ty.replace("PT1.5S", "P1M");
    let (code, message) = one_error(&go_with(&bad, "A"));
    assert_eq!(code, "xtce.invalid");
    assert!(message.contains("isn't a duration"), "{message}");
}

/// A command `Go` built on `Base`, whose container writes the parameter
/// `Id` (an aggregate of an 8-bit `Apid` and a 1-bit enumeration `Kind`)
/// and an 8-bit `Seq`, with `criteria` on `Go`'s container.
fn with_parameters(criteria: &str) -> String {
    let types = format!(
        r#"{U8}<EnumeratedParameterType name="KindType"><IntegerDataEncoding sizeInBits="1"/><EnumerationList><Enumeration value="0" label="TM"/><Enumeration value="1" label="TC"/></EnumerationList></EnumeratedParameterType>
           <AggregateParameterType name="IdType"><MemberList><Member name="Apid" typeRef="U8"/><Member name="Kind" typeRef="KindType"/></MemberList></AggregateParameterType>"#
    );
    let params = format!("{}{}", param("Id", "IdType"), param("Seq", "U8"));
    let metas = format!(
        r#"<MetaCommand name="Base" abstract="true"><CommandContainer name="Base"><EntryList>
             <ParameterRefEntry parameterRef="Id"/><ParameterRefEntry parameterRef="Seq"/>
           </EntryList></CommandContainer></MetaCommand>
           <MetaCommand name="Go"><BaseMetaCommand metaCommandRef="Base"/>
             <CommandContainer name="Go"><EntryList/><BaseContainer containerRef="Base"><RestrictionCriteria>{criteria}</RestrictionCriteria></BaseContainer></CommandContainer>
           </MetaCommand>"#
    );
    doc(&format!(
        "<TelemetryMetaData><ParameterTypeSet>{types}</ParameterTypeSet><ParameterSet>{params}</ParameterSet></TelemetryMetaData>{}",
        commands("", &metas)
    ))
}

#[test]
fn a_parameter_entry_holds_what_the_sent_commands_comparison_list_gives() {
    let list = r#"<ComparisonList>
        <Comparison parameterRef="Id.Apid" value="0x2A"/>
        <Comparison parameterRef="Id/Kind" value="TC"/>
        <Comparison parameterRef="Seq" value="7"/>
      </ComparisonList>"#;
    let text = with_parameters(list);
    let packet = &bundle(&text)["sets"][0]["commands"][0];
    let fields: Vec<(&str, &Value)> = packet["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), &e["value"]))
        .collect();
    assert_eq!(
        fields,
        [
            ("Id_Apid", &json!(42)),
            ("Id_Kind", &json!(1)),
            ("Seq", &json!(7))
        ]
    );
    let (_, result) = import_all(&[("sat.xml", &text)]);
    validate(result.unwrap().bundle).unwrap();

    // Without a value for every member, nothing to write.
    let partial = with_parameters(
        r#"<ComparisonList><Comparison parameterRef="Id.Apid" value="1"/><Comparison parameterRef="Seq" value="0"/></ComparisonList>"#,
    );
    let (code, message) = one_error(&partial);
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with("the sent command's criteria give /Sat/Id.Kind no value"),
        "{message}"
    );
    // Yamcs reads only a ComparisonList, and only its == comparisons:
    // with nothing else to give them values, the entries are unsupported.
    let messages = |criteria: &str| -> Vec<(String, String)> {
        let (sources, result) = import_all(&[("sat.xml", &with_parameters(criteria))]);
        shown(&sources, &result.unwrap_err())
            .into_iter()
            .map(|(_, code, _, message)| (code, message))
            .collect()
    };
    let single = messages(r#"<Comparison parameterRef="Seq" value="0"/>"#);
    assert_eq!(
        single
            .iter()
            .map(|(code, _)| code.as_str())
            .collect::<Vec<_>>(),
        ["xtce.unsupported", "xtce.unsupported", "xtce.ignored"]
    );
    assert_eq!(
        single[2].1,
        "a command container's Comparison isn't read: Yamcs fills parameter entries from a \
         ComparisonList only"
    );
    let unequal = messages(
        r#"<ComparisonList><Comparison parameterRef="Id.Apid" value="1"/><Comparison parameterRef="Id.Kind" value="TC"/><Comparison parameterRef="Seq" value="0" comparisonOperator="!="/></ComparisonList>"#,
    );
    assert_eq!(
        unequal
            .iter()
            .map(|(code, _)| code.as_str())
            .collect::<Vec<_>>(),
        ["xtce.unsupported", "xtce.ignored"]
    );
    // Yamcs reads a criteria value as an engineering value whatever
    // useCalibratedValue says; MDL takes XTCE's raw value, and says so.
    let raw = with_parameters(&list.replace(
        r#"value="TC"/>"#,
        r#"value="1" useCalibratedValue="false"/>"#,
    ));
    let (sources, result) = import_all(&[("sat.xml", &raw)]);
    let parsed = result.unwrap();
    assert_eq!(
        shown(&sources, &parsed.warnings)
            .into_iter()
            .map(|(_, code, _, _)| code)
            .collect::<Vec<_>>(),
        ["xtce.differs"]
    );
    let bundle = serde_json::to_value(parsed.bundle).unwrap();
    assert_eq!(bundle["sets"][0]["commands"][0]["entries"][1]["value"], 1);
    // Two values for one parameter.
    assert_eq!(
        one_error(&with_parameters(&list.replace(
            "</ComparisonList>",
            r#"<Comparison parameterRef="Seq" value="8"/></ComparisonList>"#
        )))
        .0,
        "xtce.invalid"
    );
}

/// A document whose `Go` holds `arguments` and `entries`, with `types`
/// besides U8, imported and validated: the bundle and the warnings' codes.
fn validated_go(types: &str, arguments: &str, entries: &str) -> (Value, Vec<String>) {
    validated_doc(&doc(&commands(
        &format!("{ARG_U8}{types}"),
        &go(arguments, entries),
    )))
}

fn validated_doc(text: &str) -> (Value, Vec<String>) {
    let (sources, result) = import_all(&[("sat.xml", text)]);
    let parsed = result.unwrap_or_else(|d| panic!("{:#?}", shown(&sources, &d)));
    let codes = parsed.warnings.iter().map(|w| w.code.to_string()).collect();
    let bundle = serde_json::to_value(&parsed.bundle).unwrap();
    if let Err(d) = validate(parsed.bundle) {
        panic!("{:#?}", shown(&sources, &parsed.map.locate_all(&d)));
    }
    (bundle, codes)
}

/// A one-dimensional array argument type over `element`, its last index
/// `ending` (an IntegerValue's content).
fn argument_array_type(name: &str, element: &str, ending: &str, rest: &str) -> String {
    format!(
        r#"<ArrayArgumentType name="{name}" arrayTypeRef="{element}"{rest}><DimensionList><Dimension><StartingIndex><FixedValue>0</FixedValue></StartingIndex><EndingIndex>{ending}</EndingIndex></Dimension></DimensionList></ArrayArgumentType>"#
    )
}

const BY_ARGUMENT_N: &str = r#"<DynamicValue><ArgumentInstanceRef argumentRef="N"/><LinearAdjustment intercept="-1"/></DynamicValue>"#;

#[test]
fn an_aggregate_argument_is_an_argument_per_member() {
    let types = r#"<AggregateArgumentType name="Pos"><MemberList>
        <Member name="X" typeRef="U8"/><Member name="Y" typeRef="U8" initialValue="5"/>
      </MemberList></AggregateArgumentType>"#;
    let (bundle, warnings) = validated_go(
        types,
        r#"<Argument name="Target" argumentTypeRef="Pos" initialValue='{"X": 3}'/>"#,
        &argument_entry("Target"),
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    let arguments = &bundle["catalog"]["commands"]["sat.go"]["arguments"];
    assert_eq!(
        arguments,
        &json!([
            {"name": "target_x", "type": "u64", "range": {"max": 255}, "default": 3},
            {"name": "target_y", "type": "u64", "range": {"max": 255}, "default": 5}
        ])
    );
    let entries = &bundle["sets"][0]["commands"][0]["entries"];
    assert_eq!(
        (&entries[2]["name"], &entries[2]["argument"]),
        (&json!("Target_X"), &json!("target_x"))
    );
    assert_eq!(
        (&entries[3]["name"], &entries[3]["argument"]),
        (&json!("Target_Y"), &json!("target_y"))
    );

    // Assigned, it's a constant per member: the JSON's, else the member's
    // initial value.
    let metas = format!(
        r#"<MetaCommand name="Base" abstract="true">
             <ArgumentList><Argument name="Target" argumentTypeRef="Pos"/></ArgumentList>
             <CommandContainer name="Base"><EntryList>{}</EntryList></CommandContainer>
           </MetaCommand>
           <MetaCommand name="Go"><BaseMetaCommand metaCommandRef="Base"><ArgumentAssignmentList>
             <ArgumentAssignment argumentName="Target" argumentValue='{{"X": 1}}'/>
           </ArgumentAssignmentList></BaseMetaCommand></MetaCommand>"#,
        argument_entry("Target")
    );
    let (bundle, _) = validated_doc(&doc(&commands(&format!("{ARG_U8}{types}"), &metas)));
    let values: Vec<&Value> = bundle["sets"][0]["commands"][0]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["value"])
        .collect();
    assert_eq!(values, [&json!(1), &json!(5)]);
    let bad = metas.replace(r#"{"X": 1}"#, r#"{"Z": 1}"#);
    assert_eq!(
        one_error(&doc(&commands(&format!("{ARG_U8}{types}"), &bad))),
        (
            "xtce.invalid".into(),
            "argument Target has no member Z".into()
        )
    );
}

#[test]
fn an_array_argument_is_a_list_argument() {
    let types = argument_array_type(
        "Three",
        "U8",
        "<FixedValue>2</FixedValue>",
        r#" initialValue="[1, 2, 3]""#,
    );
    let (bundle, warnings) = validated_go(
        &types,
        &argument("Levels", "Three"),
        &argument_entry("Levels"),
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    // Yamcs skips an ArrayArgumentRefEntry; MDL writes the array, as XTCE
    // says, and warns.
    let (same, warnings) = validated_go(
        &types,
        &argument("Levels", "Three"),
        r#"<ArrayArgumentRefEntry argumentRef="Levels" lastEntryForThisArrayInstance="true"/>"#,
    );
    assert_eq!(warnings, ["xtce.differs"]);
    assert_eq!(
        same["sets"][0]["commands"][0]["entries"][2],
        bundle["sets"][0]["commands"][0]["entries"][2]
    );
    assert_eq!(
        bundle["catalog"]["commands"]["sat.go"]["arguments"],
        json!([{"name": "levels", "type": "u64", "range": {"max": 255},
                "list": {"min": 3, "max": 3}, "default": [1, 2, 3]}])
    );
    assert_eq!(
        bundle["sets"][0]["commands"][0]["entries"][2],
        json!({"kind": "array", "name": "Levels", "count": {"kind": "fixed", "count": 3},
               "element": {"kind": "scalar", "type": "uint", "size_bits": 8},
               "argument": "levels"})
    );
    // A list is written whole, by one entry.
    let subset = r#"<ArrayArgumentRefEntry argumentRef="Levels"><DimensionList><Dimension><StartingIndex><FixedValue>0</FixedValue></StartingIndex><EndingIndex><FixedValue>1</FixedValue></EndingIndex></Dimension></DimensionList></ArrayArgumentRefEntry>"#;
    let text = doc(&commands(
        &format!("{ARG_U8}{types}"),
        &go(&argument("Levels", "Three"), subset),
    ));
    assert_eq!(one_error(&text).0, "xtce.unsupported");
}

#[test]
fn an_argument_giving_a_count_is_the_count_field() {
    let types = argument_array_type("Some", "U8", BY_ARGUMENT_N, "");
    let arguments = argument("N", "U8") + &argument("Values", "Some");
    let (bundle, warnings) = validated_go(
        &types,
        &arguments,
        &(argument_entry("N") + &argument_entry("Values")),
    );
    assert_eq!(warnings, ["xtce.ignored"]);
    assert_eq!(
        bundle["catalog"]["commands"]["sat.go"]["arguments"],
        json!([{"name": "values", "type": "u64", "range": {"max": 255}, "list": {"max": 255}}])
    );
    let entries = &bundle["sets"][0]["commands"][0]["entries"];
    assert_eq!(
        entries[2],
        json!({"kind": "field", "name": "N", "type": "uint", "size_bits": 8})
    );
    assert_eq!(entries[3]["count"], json!({"kind": "field", "field": "N"}));

    // The count field comes first.
    let (code, message) = one_error(&doc(&commands(
        &format!("{ARG_U8}{types}"),
        &go(
            &arguments,
            &(argument_entry("Values") + &argument_entry("N")),
        ),
    )));
    assert_eq!(code, "xtce.unsupported");
    assert!(message.contains("isn't written before it"), "{message}");

    // Assigned, the size is a constant and the count fixed.
    let metas = format!(
        r#"<MetaCommand name="Base" abstract="true"><ArgumentList>{arguments}</ArgumentList>
             <CommandContainer name="Base"><EntryList>{}{}</EntryList></CommandContainer></MetaCommand>
           <MetaCommand name="Go"><BaseMetaCommand metaCommandRef="Base"><ArgumentAssignmentList>
             <ArgumentAssignment argumentName="N" argumentValue="2"/>
           </ArgumentAssignmentList></BaseMetaCommand></MetaCommand>"#,
        argument_entry("N"),
        argument_entry("Values")
    );
    let (bundle, _) = validated_doc(&doc(&commands(&format!("{ARG_U8}{types}"), &metas)));
    let entries = &bundle["sets"][0]["commands"][0]["entries"];
    assert_eq!(entries[0]["value"], 2);
    assert_eq!(entries[1]["count"], json!({"kind": "fixed", "count": 2}));
    assert_eq!(
        bundle["catalog"]["commands"]["sat.go"]["arguments"][0]["list"],
        json!({"min": 2, "max": 2})
    );
}

#[test]
fn an_argument_giving_a_length_is_the_length_field() {
    let types = r#"<StringArgumentType name="Text"><StringDataEncoding><Variable maxSizeInBits="256"><DynamicValue><ArgumentInstanceRef argumentRef="L"/><LinearAdjustment slope="8"/></DynamicValue></Variable></StringDataEncoding></StringArgumentType>"#;
    let (bundle, warnings) = validated_go(
        &format!(
            "{types}<IntegerArgumentType name=\"I8\"><IntegerDataEncoding sizeInBits=\"8\" encoding=\"twosComplement\"/></IntegerArgumentType>"
        ),
        &(argument("L", "U8") + &argument("Name", "Text")),
        &(argument_entry("L") + &argument_entry("Name")),
    );
    assert_eq!(warnings, ["xtce.ignored", "xtce.ignored"]);
    let entries = &bundle["sets"][0]["commands"][0]["entries"];
    assert_eq!(entries[3]["length"], json!({"kind": "field", "field": "L"}));
    assert_eq!(
        bundle["catalog"]["commands"]["sat.go"]["arguments"],
        json!([{"name": "name", "type": "str"}])
    );
    // A size is an unsigned integer, and names an argument of the command.
    let signed = doc(&commands(
        &format!(
            "{ARG_U8}{types}<IntegerArgumentType name=\"I8\"><IntegerDataEncoding sizeInBits=\"8\" encoding=\"twosComplement\"/></IntegerArgumentType>"
        ),
        &go(
            &(argument("L", "I8") + &argument("Name", "Text")),
            &(argument_entry("L") + &argument_entry("Name")),
        ),
    ));
    assert_eq!(one_error(&signed).0, "xtce.unsupported");
    let nameless = doc(&commands(
        &format!("{ARG_U8}{types}"),
        &go(&argument("Name", "Text"), &argument_entry("Name")),
    ));
    assert_eq!(
        one_error(&nameless),
        (
            "xtce.reference".into(),
            "L names no argument of this command".into()
        )
    );
}

#[test]
fn an_array_of_aggregates_is_records_of_one_list_per_member() {
    let types = format!(
        r#"<IntegerArgumentType name="U16" signed="false"><IntegerDataEncoding sizeInBits="16"/></IntegerArgumentType>
           <AggregateArgumentType name="Target"><MemberList><Member name="Id" typeRef="U8"/><Member name="Range" typeRef="U16"/></MemberList></AggregateArgumentType>
           {}"#,
        argument_array_type(
            "Targets",
            "Target",
            "<FixedValue>1</FixedValue>",
            r#" initialValue='[{"Id": 1, "Range": 100}, {"Id": 2, "Range": 200}]'"#
        )
    );
    let (bundle, warnings) = validated_go(
        &types,
        &argument("Targets", "Targets"),
        &argument_entry("Targets"),
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    let arguments = &bundle["catalog"]["commands"]["sat.go"]["arguments"];
    assert_eq!(
        (
            &arguments[0]["name"],
            &arguments[0]["default"],
            &arguments[0]["list"]
        ),
        (
            &json!("targets_id"),
            &json!([1, 2]),
            &json!({"min": 2, "max": 2})
        )
    );
    assert_eq!(
        (&arguments[1]["name"], &arguments[1]["default"]),
        (&json!("targets_range"), &json!([100, 200]))
    );
    let set = &bundle["sets"][0];
    assert_eq!(
        set["commands"][0]["entries"][2]["element"],
        json!({"kind": "struct", "name": "Go_Targets_element"})
    );
    assert_eq!(
        set["types"]["Go_Targets_element"]["entries"],
        json!([
            {"kind": "field", "name": "Id", "type": "uint", "size_bits": 8, "argument": "targets_id"},
            {"kind": "field", "name": "Range", "type": "uint", "size_bits": 16, "argument": "targets_range"}
        ])
    );
}

#[test]
fn a_nested_aggregate_is_completed_as_yamcs_completes_it() {
    // pos's own initial value gives x; y's gives y; the argument's gives z.
    let types = r#"<AggregateArgumentType name="Inner"><MemberList>
          <Member name="x" typeRef="U8"/><Member name="y" typeRef="U8" initialValue="7"/>
        </MemberList></AggregateArgumentType>
        <AggregateArgumentType name="Outer"><MemberList>
          <Member name="pos" typeRef="Inner" initialValue='{"x": 4}'/><Member name="z" typeRef="U8"/>
        </MemberList></AggregateArgumentType>"#;
    let defaults = |initial: &str| {
        let (bundle, _) = validated_go(
            types,
            &format!(r#"<Argument name="T" argumentTypeRef="Outer" initialValue='{initial}'/>"#),
            &argument_entry("T"),
        );
        bundle["catalog"]["commands"]["sat.go"]["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["name"].as_str().unwrap().to_string(),
                    a["default"].clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        defaults(r#"{"z": 1}"#),
        [
            ("t_pos_x".to_string(), json!(4)),
            ("t_pos_y".to_string(), json!(7)),
            ("t_z".to_string(), json!(1))
        ]
    );
    // Given pos, its members come from it, else their own initial values:
    // x has none, so no default.
    assert_eq!(
        defaults(r#"{"pos": {"y": 2}, "z": 1}"#),
        [
            ("t_pos_x".to_string(), Value::Null),
            ("t_pos_y".to_string(), json!(2)),
            ("t_z".to_string(), json!(1))
        ]
    );
    // pos is an aggregate, so its value is an object.
    let text = doc(&commands(
        &format!("{ARG_U8}{types}"),
        &go(
            r#"<Argument name="T" argumentTypeRef="Outer" initialValue='{"pos": 5}'/>"#,
            &argument_entry("T"),
        ),
    ));
    let (code, message) = one_error(&text);
    assert_eq!(code, "xtce.invalid");
    assert!(
        message.starts_with("member pos of argument T is an aggregate"),
        "{message}"
    );
    // A member's initial value names only its members.
    let bad = types.replace(r#"{"x": 4}"#, r#"{"w": 4}"#);
    let text = doc(&commands(
        &format!("{ARG_U8}{bad}"),
        &go(&argument("T", "Outer"), &argument_entry("T")),
    ));
    assert_eq!(
        one_error(&text),
        (
            "xtce.invalid".into(),
            "member pos of argument T has no member w".into()
        )
    );
}

#[test]
fn records_of_two_commands_are_named_for_their_meta_commands() {
    let types = argument_array_type("Points", "Pair", "<FixedValue>1</FixedValue>", "")
        + r#"<AggregateArgumentType name="Pair"><MemberList><Member name="a" typeRef="U8"/><Member name="b" typeRef="U8"/></MemberList></AggregateArgumentType>"#;
    let meta = |name: &str, op: &str| {
        format!(
            r#"<MetaCommand name="{name}"><ArgumentList>{}</ArgumentList><CommandContainer name="{name}"><EntryList><FixedValueEntry name="Op" binaryValue="{op}"/>{}</EntryList></CommandContainer></MetaCommand>"#,
            argument("Points", "Points"),
            argument_entry("Points")
        )
    };
    let (bundle, warnings) = validated_doc(&doc(&commands(
        &format!("{ARG_U8}{types}"),
        &(meta("A", "01") + &meta("B", "02")),
    )));
    assert!(warnings.is_empty(), "{warnings:?}");
    let names: Vec<&String> = bundle["sets"][0]["types"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(names, ["A_Points_element", "B_Points_element"]);
}

#[test]
fn a_record_type_another_struct_type_names_is_told_apart() {
    let pair = r#"<AggregateArgumentType name="Pair"><MemberList><Member name="a" typeRef="U8"/><Member name="b" typeRef="U8"/></MemberList></AggregateArgumentType>"#.to_string()
        + &argument_array_type("Points", "Pair", "<FixedValue>1</FixedValue>", "");
    let meta = |name: &str, field: &str, op: &str| {
        format!(
            r#"<MetaCommand name="{name}"><ArgumentList>{}</ArgumentList><CommandContainer name="{name}"><EntryList><FixedValueEntry name="Op" binaryValue="{op}"/>{}</EntryList></CommandContainer></MetaCommand>"#,
            argument(field, "Points"),
            argument_entry(field)
        )
    };
    let names = |bundle: &Value| -> Vec<String> {
        bundle["sets"][0]["types"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    // A_B's Points and A's B_Points both join to A_B_Points.
    let (bundle, _) = validated_doc(&doc(&commands(
        &format!("{ARG_U8}{pair}"),
        &(meta("A_B", "Points", "01") + &meta("A", "B_Points", "02")),
    )));
    assert_eq!(
        names(&bundle),
        ["Sat_A_B_Points_element", "Sat_A_B__Points_element"]
    );
    // A telemetry array of aggregates A_Points has A_Points_element.
    let telemetry_types = format!(
        r#"{U8}<AggregateParameterType name="PairT"><MemberList><Member name="a" typeRef="U8"/></MemberList></AggregateParameterType>
           <ArrayParameterType name="PointsT" arrayTypeRef="PairT"><DimensionList><Dimension><StartingIndex><FixedValue>0</FixedValue></StartingIndex><EndingIndex><FixedValue>1</FixedValue></EndingIndex></Dimension></DimensionList></ArrayParameterType>"#
    );
    let text = doc(&format!(
        "{}{}",
        telemetry(
            &telemetry_types,
            &param("A_Points", "PointsT"),
            &container("P", &entry("A_Points"))
        ),
        commands(&format!("{ARG_U8}{pair}"), &meta("A", "Points", "01"))
    ));
    let (bundle, warnings) = validated_doc(&text);
    assert_eq!(warnings, ["xtce.renamed"]);
    assert_eq!(names(&bundle), ["A_Points_element", "Sat_A_Points_element"]);
}

#[test]
fn an_unwritten_value_shares_a_length_and_member_initial_values_are_checked() {
    // Name and Note share L, but only Name is written.
    let text_type = r#"<StringArgumentType name="Text"><StringDataEncoding><Variable maxSizeInBits="256"><DynamicValue><ArgumentInstanceRef argumentRef="L"/><LinearAdjustment slope="8"/></DynamicValue></Variable></StringDataEncoding></StringArgumentType>"#;
    validated_go(
        text_type,
        &(argument("L", "U8") + &argument("Name", "Text") + &argument("Note", "Text")),
        &(argument_entry("L") + &argument_entry("Name")),
    );
    // pos's initial value is checked though the argument gives pos whole.
    let types = r#"<AggregateArgumentType name="Inner"><MemberList><Member name="x" typeRef="U8"/></MemberList></AggregateArgumentType>
        <AggregateArgumentType name="Outer"><MemberList><Member name="pos" typeRef="Inner" initialValue='{"w": 4}'/></MemberList></AggregateArgumentType>"#;
    let text = doc(&commands(
        &format!("{ARG_U8}{types}"),
        &go(
            r#"<Argument name="T" argumentTypeRef="Outer" initialValue='{"pos": {"x": 1}}'/>"#,
            &argument_entry("T"),
        ),
    ));
    assert_eq!(
        one_error(&text),
        (
            "xtce.invalid".into(),
            "member pos of argument T has no member w".into()
        )
    );
    // An ArrayArgumentRefEntry names an array argument.
    let text = doc(&commands(
        ARG_U8,
        &go(
            &argument("A", "U8"),
            r#"<ArrayArgumentRefEntry argumentRef="A"/>"#,
        ),
    ));
    assert_eq!(
        one_error(&text),
        (
            "xtce.invalid".into(),
            "an ArrayArgumentRefEntry names an array argument".into()
        )
    );
}

#[test]
fn sizes_are_bounded_and_fixed_as_their_arguments_say() {
    // A 32-bit count whose last index is N counts up to 2^32 values; the
    // list stops at the most a u32 holds.
    let types = r#"<IntegerArgumentType name="U32" signed="false"><IntegerDataEncoding sizeInBits="32"/></IntegerArgumentType>"#.to_string()
        + &argument_array_type(
            "Some",
            "U8",
            r#"<DynamicValue><ArgumentInstanceRef argumentRef="N"/></DynamicValue>"#,
            "",
        );
    let (bundle, _) = validated_go(
        &types,
        &(argument("N", "U32") + &argument("Values", "Some")),
        &(argument_entry("N") + &argument_entry("Values")),
    );
    assert_eq!(
        bundle["catalog"]["commands"]["sat.go"]["arguments"][0]["list"],
        json!({"min": 1, "max": 4_294_967_295_u32})
    );
    assert_eq!(
        bundle["sets"][0]["commands"][0]["entries"][3]["count"],
        json!({"kind": "field", "field": "N", "add": 1})
    );

    // An assigned length is a fixed, NUL-padded buffer.
    let text_type = r#"<StringArgumentType name="Text"><StringDataEncoding><Variable maxSizeInBits="256"><DynamicValue><ArgumentInstanceRef argumentRef="L"/><LinearAdjustment slope="8"/></DynamicValue></Variable></StringDataEncoding></StringArgumentType>"#;
    let metas = format!(
        r#"<MetaCommand name="Base" abstract="true"><ArgumentList>{}{}</ArgumentList>
             <CommandContainer name="Base"><EntryList>{}{}</EntryList></CommandContainer></MetaCommand>
           <MetaCommand name="Go"><BaseMetaCommand metaCommandRef="Base"><ArgumentAssignmentList>
             <ArgumentAssignment argumentName="L" argumentValue="4"/>
           </ArgumentAssignmentList></BaseMetaCommand></MetaCommand>"#,
        argument("L", "U8"),
        argument("Name", "Text"),
        argument_entry("L"),
        argument_entry("Name")
    );
    let (bundle, _) = validated_doc(&doc(&commands(&format!("{ARG_U8}{text_type}"), &metas)));
    assert_eq!(
        bundle["sets"][0]["commands"][0]["entries"][1]["length"],
        json!({"kind": "fixed", "bytes": 4, "terminator": 0})
    );
}

#[test]
fn sizes_mdl_cant_take_are_unsupported() {
    let text = r#"<StringArgumentType name="Text"><StringDataEncoding><Variable maxSizeInBits="256"><DynamicValue><ArgumentInstanceRef argumentRef="L"/><LinearAdjustment slope="8"/></DynamicValue></Variable></StringDataEncoding></StringArgumentType>"#;
    let some = argument_array_type("Some", "U8", BY_ARGUMENT_N, "");
    let pair = r#"<AggregateArgumentType name="Pair"><MemberList><Member name="a" typeRef="U8"/><Member name="b" typeRef="U8"/></MemberList></AggregateArgumentType>"#;
    let ranged = r#"<IntegerArgumentType name="Big" signed="false"><IntegerDataEncoding sizeInBits="8"/><ValidRangeSet><ValidRange minInclusive="300" maxInclusive="400"/></ValidRangeSet></IntegerArgumentType>"#;
    let by_param = argument_array_type(
        "ByParam",
        "U8",
        r#"<DynamicValue><ParameterInstanceRef parameterRef="P"/></DynamicValue>"#,
        "",
    );
    let flat = |arguments: &str, entries: &str| {
        doc(&commands(
            &format!("{ARG_U8}{text}{some}{pair}{ranged}{by_param}"),
            &go(arguments, entries),
        ))
    };
    let cases = [
        // One argument as both a count and a length.
        (
            flat(
                &(argument("N", "U8") + &argument("Values", "Some") + &argument("Name", "Text")),
                &(argument_entry("N") + &argument_entry("Values") + &argument_entry("Name")),
            )
            .replace(r#"argumentRef="L""#, r#"argumentRef="N""#),
            "gives both a count and a length",
        ),
        // A length for two values.
        (
            flat(
                &(argument("L", "U8") + &argument("Name", "Text") + &argument("Note", "Text")),
                &(argument_entry("L") + &argument_entry("Name") + &argument_entry("Note")),
            ),
            "gives the length of two values",
        ),
        // An aggregate as a size.
        (
            flat(
                &(argument("N", "Pair") + &argument("Values", "Some")),
                &(argument_entry("N") + &argument_entry("Values")),
            ),
            "not an aggregate's or a list's",
        ),
        // A size of nothing written.
        (
            flat(
                &(argument("N", "U8") + &argument("Values", "Some")),
                &argument_entry("N"),
            ),
            "gives the size of an argument no entry writes",
        ),
        // A size whose range its field can't hold.
        (
            flat(
                &(argument("N", "Big") + &argument("Values", "Some")),
                &(argument_entry("N") + &argument_entry("Values")),
            ),
            "its valid range lies outside the raw values its field holds",
        ),
        // A size a parameter gives.
        (
            flat(&argument("Values", "ByParam"), &argument_entry("Values")),
            "a size a parameter gives isn't supported in a command",
        ),
    ];
    for (text, expected) in cases {
        let errors: Vec<String> = {
            let (sources, result) = import_all(&[("sat.xml", &text)]);
            shown(&sources, &result.unwrap_err())
                .into_iter()
                .filter(|(severity, ..)| *severity == Severity::Error)
                .map(|(_, _, _, message)| message)
                .collect()
        };
        assert!(
            errors.len() == 1 && errors[0].contains(expected),
            "{expected}: {errors:#?}"
        );
    }
}

#[test]
fn array_argument_shapes_mdl_cant_write_are_unsupported() {
    let pair = r#"<AggregateArgumentType name="Pair"><MemberList><Member name="a" typeRef="U8"/><Member name="b" typeRef="U8"/></MemberList></AggregateArgumentType>"#;
    let cases = [
        (
            argument_array_type("Three", "U8", "<FixedValue>2</FixedValue>", "")
                + r#"<AggregateArgumentType name="A"><MemberList><Member name="list" typeRef="Three"/></MemberList></AggregateArgumentType>"#,
            "an array inside an aggregate argument",
        ),
        (
            r#"<ArrayArgumentType name="A" arrayTypeRef="U8"/>"#.to_string(),
            "has no DimensionList",
        ),
        (
            format!(
                "{pair}{}",
                argument_array_type(
                    "A",
                    "Pair",
                    "<FixedValue>1</FixedValue>",
                    r#" initialValue='[1, 2]'"#
                )
            ),
            "which isn't an object of its elements' members",
        ),
        (
            format!(
                "{pair}{}",
                argument_array_type(
                    "A",
                    "Pair",
                    "<FixedValue>1</FixedValue>",
                    r#" initialValue='[{"a": 1}, {"a": 2}]'"#
                )
            ),
            "gives value 0 no b, and the member has no initial value",
        ),
        (
            argument_array_type(
                "A",
                "U8",
                "<FixedValue>1</FixedValue>",
                r#" initialValue='{"a": 1}'"#,
            ),
            "isn't a JSON array of argument A's values",
        ),
    ];
    for (types, expected) in cases {
        let (_, message) = one_error(&go_with(&types, "A"));
        assert!(message.contains(expected), "{expected}: {message}");
    }
}

proptest! {
    #[test]
    fn any_text_never_panics(text in "\\PC{0,200}") {
        let _ = import_all(&[("any.xml", &text)]);
    }

    #[test]
    fn any_cut_of_a_real_document_never_panics(cut in 0_usize..15000, length in 0_usize..400) {
        let text = include_str!("../../../examples/xtce/demosat.xml");
        let mut start = cut.min(text.len());
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (start + length).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let damaged = format!("{}{}", &text[..start], &text[end..]);
        let _ = import_all(&[("demosat.xml", &damaged)]);
    }
}

/// `Go`'s arguments for the referring tests: an enumerated mode listing
/// NOMINAL before SAFE, a delay in seconds with an initial value, a pulse
/// in milliseconds without one, and a wait in seconds.
const REFERRED_TYPES: &str = r#"<EnumeratedArgumentType name="Mode"><IntegerDataEncoding sizeInBits="8"/><EnumerationList><Enumeration value="1" label="NOMINAL"/><Enumeration value="0" label="SAFE"/></EnumerationList></EnumeratedArgumentType>
<IntegerArgumentType name="Delay" signed="false" initialValue="5"><UnitSet><Unit>s</Unit></UnitSet><IntegerDataEncoding sizeInBits="8"/><ValidRangeSet><ValidRange minInclusive="0" maxInclusive="10"/></ValidRangeSet></IntegerArgumentType>
<IntegerArgumentType name="Pulse" signed="false"><UnitSet><Unit>ms</Unit></UnitSet><IntegerDataEncoding sizeInBits="16"/><ValidRangeSet><ValidRange minInclusive="50" maxInclusive="1000"/></ValidRangeSet></IntegerArgumentType>
<FloatArgumentType name="Wait"><UnitSet><Unit>s</Unit></UnitSet><FloatDataEncoding sizeInBits="32"/><ValidRangeSet><ValidRange minInclusive="0" maxInclusive="60"/></ValidRangeSet></FloatArgumentType>"#;

/// A referring import of `Go` with [`REFERRED_TYPES`], against `catalog`
/// (sat.go's arguments): its text, its packet's arguments, and the bundle
/// with that catalog, as a build would validate it.
fn referred(arguments: Value) -> (Sources, String, Value, mdl::ir::Bundle) {
    let names = ["Mode", "Delay", "Pulse", "Wait"];
    let declared: String = names.iter().map(|name| argument(name, name)).collect();
    let written: String = names.iter().map(|name| argument_entry(name)).collect();
    let text = doc(&commands(
        &format!("{ARG_U8}{REFERRED_TYPES}"),
        &go(&declared, &written).replace(
            r#"<MetaCommand name="Go">"#,
            r#"<MetaCommand name="Go"><AliasSet><Alias nameSpace="MDL" alias="sat.go"/></AliasSet>"#,
        ),
    ));
    let mut sources = Sources::new();
    sources.add("sat.xml", text.as_str());
    let catalog = catalog_of(json!({ "commands": { "sat.go": { "arguments": arguments } } }));
    let options = Options::new().with_catalog(CatalogRole::Refer);
    let parsed = read_with(
        sources.iter(),
        &options,
        Context::new().with_catalog(&catalog),
    )
    .unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    let json = serde_json::to_value(&parsed.bundle).unwrap();
    let limits = json["sets"][0]["commands"][0]["arguments"].clone();
    assert_eq!(json["catalog"], json!({}));
    let mut bundle = parsed.bundle;
    bundle.catalog = catalog;
    (sources, text, limits, bundle)
}

#[test]
fn a_referring_import_writes_what_the_vendor_says_beyond_the_catalog() {
    let arguments = |mode_states: Value| {
        json!([
            { "name": "mode", "type": "enum", "states": mode_states },
            { "name": "delay", "type": "u64", "unit": "s" },
            { "name": "pulse", "type": "u64", "unit": "ms", "default": 200 },
            { "name": "wait", "type": "f64", "unit": "min" }
        ])
    };
    // The vendor's mode lacks DETUMBLE, so the packet accepts the others,
    // in the catalog's order. Limits in the catalog argument's unit don't
    // write it, and the wait's, in another, do. A pulse without an initial
    // value is required, since the catalog's default is the other
    // vendors'; the delay's initial value is the packet's default instead.
    let (_, _, limits, bundle) = referred(arguments(json!(["SAFE", "NOMINAL", "DETUMBLE"])));
    assert_eq!(
        limits,
        json!({
            "mode": { "states": ["SAFE", "NOMINAL"] },
            "delay": { "range": { "min": 0, "max": 10 }, "default": 5 },
            "pulse": { "range": { "min": 50, "max": 1000 }, "required": true },
            "wait": { "unit": "s", "range": { "min": 0, "max": 60 } }
        })
    );
    assert_eq!(bundle.mdl_version.to_string(), "2.3");
    assert!(validate(bundle).is_ok());
    // Listing every state, the packet accepts every state, as without a
    // list.
    let (_, _, limits, bundle) = referred(arguments(json!(["SAFE", "NOMINAL"])));
    assert_eq!(limits.get("mode"), None, "{limits}");
    assert!(validate(bundle).is_ok());
    // Listing one the catalog lacks, the field's own states say so, where
    // the vendor lists it.
    let (_, text, limits, bundle) = referred(arguments(json!(["SAFE", "DETUMBLE"])));
    assert_eq!(limits.get("mode"), None, "{limits}");
    let parsed_map = {
        let mut again = Sources::new();
        again.add("sat.xml", text.as_str());
        let catalog = bundle.catalog.clone();
        read_with(
            again.iter(),
            &Options::new().with_catalog(CatalogRole::Refer),
            Context::new().with_catalog(&catalog),
        )
        .unwrap()
        .map
    };
    let located = parsed_map.locate_all(&validate(bundle).unwrap_err());
    let codes: Vec<&str> = located.iter().map(|d| d.code).collect();
    assert_eq!(codes, ["canonical_type"]);
    let span = located[0].span.unwrap();
    assert_eq!(
        Some(span.start),
        text.find(r#"<Enumeration value="1" label="NOMINAL"/>"#)
    );
    // A catalog default the vendor gives no initial value requires the
    // argument, beside the states it accepts; one the vendor gives an
    // initial value is the packet's default instead.
    let mut defaulted = arguments(json!(["SAFE", "NOMINAL", "DETUMBLE"]));
    defaulted[0]["default"] = json!("SAFE");
    defaulted[1]["default"] = json!(3);
    let (_, _, limits, bundle) = referred(defaulted);
    assert_eq!(
        limits["mode"],
        json!({ "states": ["SAFE", "NOMINAL"], "required": true })
    );
    assert_eq!(
        limits["delay"],
        json!({ "range": { "min": 0, "max": 10 }, "default": 5 })
    );
    assert!(validate(bundle).is_ok());
    // Either IR 2.3 key alone states 2.3.
    let mut states_only = arguments(json!(["SAFE", "NOMINAL", "DETUMBLE"]));
    states_only[2].as_object_mut().unwrap().remove("default");
    let (_, _, limits, bundle) = referred(states_only);
    assert_eq!(
        limits["pulse"],
        json!({ "range": { "min": 50, "max": 1000 } })
    );
    assert_eq!(bundle.mdl_version.to_string(), "2.3");
    let (_, _, limits, bundle) = referred(arguments(json!(["SAFE", "NOMINAL"])));
    assert_eq!(limits["pulse"]["required"], true);
    assert_eq!(bundle.mdl_version.to_string(), "2.3");
}

#[test]
fn a_referring_import_is_given_the_catalog() {
    let text = go_with("", "U8").replace(
        r#"<MetaCommand name="Go">"#,
        r#"<MetaCommand name="Go"><AliasSet><Alias nameSpace="MDL" alias="sat.go"/></AliasSet>"#,
    );
    let mut sources = Sources::new();
    sources.add("sat.xml", text.as_str());
    let options = Options::new().with_catalog(CatalogRole::Refer);
    let errors = shown(&sources, &read(sources.iter(), &options).unwrap_err());
    assert_eq!(
        errors,
        [(
            Severity::Error,
            "xtce.options".to_string(),
            "1:1".to_string(),
            "the import refers to another input's catalog, but isn't given it".to_string()
        )]
    );
}

/// A housekeeping packet of one big-endian 16-bit voltage.
fn housekeeping() -> String {
    doc(&telemetry(
        U16,
        &param("Volts", "U16"),
        &container("HK", &entry("Volts")),
    ))
}

/// Importing `text` with `options`, beside `families`: the bundle as JSON,
/// or each problem's message.
fn joining(text: &str, options: &Options, families: Value) -> Result<Value, Vec<String>> {
    let families: Vec<mdl::ir::Family> = serde_json::from_value(families).unwrap();
    let mut sources = Sources::new();
    sources.add("sat.xml", text);
    match read_with(
        sources.iter(),
        options,
        Context::new().with_families(&families),
    ) {
        Ok(parsed) => Ok(serde_json::to_value(&parsed.bundle).unwrap()),
        Err(diagnostics) => Err(shown(&sources, &diagnostics)
            .into_iter()
            .map(|(_, code, at, message)| {
                // At the import's root SpaceSystem, which says which
                // import it is.
                assert_eq!((code.as_str(), at.as_str()), ("xtce.options", "1:1"));
                message
            })
            .collect()),
    }
}

fn vendor_x(byte_order: &str) -> Value {
    json!([{
        "name": "vendor_x", "encoding": "binary", "byte_order": byte_order,
        "alias": { "kind": "fixed", "value": "X-1" }
    }])
}

#[test]
fn an_import_joins_a_family_another_input_defines() {
    let text = housekeeping();
    let options = Options::new().with_join("vendor_x").with_set("vendor_x_v2");
    // The set is the family's, which the import doesn't write, and its
    // fields take the family's byte order: a big-endian one in a
    // little-endian family says so, and in a big-endian one needn't.
    let little = joining(&text, &options, vendor_x("little")).unwrap();
    assert_eq!(little["families"], json!(null), "{little}");
    assert_eq!(little["sets"][0]["name"], "vendor_x_v2");
    assert_eq!(little["sets"][0]["family"], "vendor_x");
    let volts = &little["sets"][0]["packets"][0]["entries"][0];
    assert_eq!(volts["byte_order"], "big", "{volts}");
    let big = joining(&text, &options, vendor_x("big")).unwrap();
    let volts = &big["sets"][0]["packets"][0]["entries"][0];
    assert_eq!(volts.get("byte_order"), None, "{volts}");
    // The family's alias may be restated.
    let restated = options.clone().with_alias(mdl::ir::Locator::fixed("X-1"));
    assert!(joining(&text, &restated, vendor_x("big")).is_ok());
    // A family that reads each payload's layout version chooses the set
    // by its version.
    let mut versioned = vendor_x("big");
    versioned[0]["version"] = json!({ "kind": "bits", "offset_bits": 0, "size_bits": 8 });
    let with_version = options.clone().with_set_version(mdl::ir::Scalar::Int(2));
    let joined = joining(&text, &with_version, versioned.clone()).unwrap();
    assert_eq!(joined["sets"][0]["version"], 2, "{joined}");
    // An import that defines its family is as before.
    let own = joining(&text, &Options::new(), json!([])).unwrap();
    assert_eq!(own["families"][0]["name"], "sat");
}

#[test]
fn a_join_the_import_cant_follow_is_refused_with_every_reason() {
    let text = housekeeping();
    let options = || Options::new().with_join("vendor_x").with_set("vendor_x_v2");
    let refused =
        |options: Options, families: Value| joining(&text, &options, families).unwrap_err();
    assert_eq!(
        refused(
            Options::new().with_join("vendor_xx").with_set("v2"),
            vendor_x("big")
        ),
        ["set v2 joins family vendor_xx, which no other input defines; did you mean vendor_x?"]
    );
    assert_eq!(
        refused(Options::new().with_join("vendor_x"), vendor_x("big")),
        ["the import joins family vendor_x, so it names the set it adds to it"]
    );
    assert_eq!(
        refused(options().with_family("other"), vendor_x("big")),
        ["the import names family other and joins family vendor_x; give one"]
    );
    let mut json_family = vendor_x("big");
    json_family[0]["encoding"] = json!("json");
    json_family[0].as_object_mut().unwrap().remove("byte_order");
    assert_eq!(
        refused(options(), json_family),
        [
            "set vendor_x_v2 can't join family vendor_x: the family is JSON, and an XTCE import is binary"
        ]
    );
    assert_eq!(
        refused(
            options().with_alias(mdl::ir::Locator::fixed("X-2")),
            vendor_x("big")
        ),
        ["set vendor_x_v2 joins family vendor_x, whose alias is \
             {\"kind\":\"fixed\",\"value\":\"X-1\"}, but the alias option is \
             {\"kind\":\"fixed\",\"value\":\"X-2\"}; leave it out to take the family's"]
    );
    let mut versioned = vendor_x("big");
    versioned[0]["version"] = json!({ "kind": "bits", "offset_bits": 0, "size_bits": 8 });
    assert_eq!(
        refused(options(), versioned),
        [
            "family vendor_x reads each payload's layout version, so set vendor_x_v2, joining it, \
             gives set_version"
        ]
    );
    assert_eq!(
        refused(
            options().with_set_version(mdl::ir::Scalar::Int(2)),
            vendor_x("big")
        ),
        [
            "family vendor_x has no version locator, so set vendor_x_v2, joining it, gives no \
             set_version"
        ]
    );
    assert_eq!(
        refused(
            Options::new().with_set_version(mdl::ir::Scalar::Int(2)),
            json!([])
        ),
        [
            "set_version is for a set that joins a family with a version locator, and the \
             import joins none"
        ]
    );
    // Every reason at once.
    assert_eq!(
        refused(
            Options::new()
                .with_join("vendor_x")
                .with_family("other")
                .with_alias(mdl::ir::Locator::fixed("X-2")),
            vendor_x("big")
        )
        .len(),
        3
    );
}

const XTCE_1_3: &str = "http://www.omg.org/spec/XTCE/20250214";

/// One of these tests' documents, written in XTCE 1.2's namespace, as an
/// XTCE 1.3 document.
fn in_1_3(text: &str) -> String {
    text.replace(XTCE, XTCE_1_3)
}

#[test]
fn time_units_are_mdl_units_in_either_spelling() {
    let relative = |units: &str| {
        doc(&telemetry(
            &format!(
                r#"<RelativeTimeParameterType name="T"><Encoding units="{units}"><IntegerDataEncoding sizeInBits="16"/></Encoding></RelativeTimeParameterType>"#
            ),
            &param("X", "T"),
            &container("P", &entry("X")),
        ))
    };
    // XTCE 1.3's units, and 1.2's spelling of picoseconds.
    for (xtce, mdl) in [
        ("seconds", "s"),
        ("milliseconds", "ms"),
        ("microseconds", "us"),
        ("nanoseconds", "ns"),
        ("picoseconds", "ps"),
        ("picoSeconds", "ps"),
        ("minutes", "min"),
        ("hours", "h"),
        ("days", "d"),
    ] {
        let (packets, _) = valid(&in_1_3(&relative(xtce)));
        assert_eq!(packets[0]["entries"][0]["unit"], mdl, "{xtce}");
    }
    let (code, _) = one_problem(&in_1_3(&relative("years")));
    assert_eq!(code, "xtce.ignored");
    assert_eq!(
        one_error(&relative("fortnights")),
        (
            "xtce.invalid".to_string(),
            "units is \"fortnights\", not an XTCE time unit".to_string()
        )
    );
}

#[test]
fn a_disabled_alarm_gives_no_limits() {
    let text = |disabled: &str| {
        in_1_3(&doc(&telemetry(
            &format!(
                r#"<FloatParameterType name="T"><UnitSet><Unit>V</Unit></UnitSet><IntegerDataEncoding sizeInBits="16"/><DefaultAlarm disabled="{disabled}"><StaticAlarmRanges><WarningRange maxInclusive="32"/></StaticAlarmRanges></DefaultAlarm></FloatParameterType>"#
            ),
            &param("X", "T"),
            &container("P", &entry("X")),
        )))
    };
    let disabled = text("true");
    let (sources, result) = import_all(&[("sat.xml", &disabled)]);
    let parsed = result.unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    let catalog = serde_json::to_value(&parsed.bundle.catalog).unwrap();
    assert_eq!(catalog["params"]["sat.x"].get("limits"), None, "{catalog}");
    let at = sources
        .iter()
        .next()
        .unwrap()
        .location(disabled.find("\"true\"").unwrap() + 1);
    assert_eq!(
        shown(&sources, &parsed.warnings),
        [(
            Severity::Warning,
            "xtce.ignored".to_string(),
            format!("{}:{}", at.line, at.column),
            "a disabled alarm isn't imported as limits: no alarm applies to the value".to_string()
        )]
    );
    // An alarm that isn't disabled is as one without the attribute.
    let (sources, result) = import_all(&[("sat.xml", &text("false"))]);
    let parsed = result.unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    assert!(parsed.warnings.is_empty());
    let catalog = serde_json::to_value(&parsed.bundle.catalog).unwrap();
    assert_eq!(
        catalog["params"]["sat.x"]["limits"],
        json!({ "yellow_high": 32.0 })
    );
}

#[test]
fn a_space_systems_type_is_left_out() {
    let text = in_1_3(&doc(&telemetry(
        U8,
        &param("A", "U8"),
        &container("P", &entry("A")),
    )))
    .replacen(
        r#"<SpaceSystem name="Sat""#,
        r#"<SpaceSystem name="Sat" systemType="asset" assetType="spacecraft""#,
        1,
    );
    let (sources, result) = import_all(&[("sat.xml", &text)]);
    let parsed = result.unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    let found = shown(&sources, &parsed.warnings);
    assert_eq!(found.len(), 2, "{found:#?}");
    for ((_, code, _, message), attribute) in found.iter().zip(["systemType", "assetType"]) {
        assert_eq!(code, "xtce.ignored");
        assert!(
            message.contains(attribute)
                && message.ends_with("it describes the system, and decodes nothing"),
            "{message}"
        );
    }
}

#[test]
fn a_reference_into_an_array_or_an_arguments_member_is_unsupported() {
    let element = doc(&telemetry(
        U8,
        &param("A", "U8"),
        &container("P", r#"<ParameterRefEntry parameterRef="A[2]"/>"#),
    ));
    let (code, message) = one_error(&in_1_3(&element));
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with("A[2] names an array's element, which isn't supported"),
        "{message}"
    );
    let member = doc(&commands(
        ARG_U8,
        &go(&argument("A", "U8"), &argument_entry("A.x")),
    ));
    let (code, message) = one_error(&in_1_3(&member));
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.starts_with("A.x names a member or element of an argument, which isn't supported"),
        "{message}"
    );
    // A name that's simply missing is still one that names nothing.
    let missing = doc(&commands(
        ARG_U8,
        &go(&argument("A", "U8"), &argument_entry("B")),
    ));
    assert_eq!(one_error(&missing).0, "xtce.reference");
}

/// An aggregate argument type in XTCE 1.3, whose own initial value gives
/// its members defaults beneath their own and the argument's.
#[test]
fn an_aggregate_argument_types_initial_value_gives_its_members_defaults() {
    let types = |initial: &str| {
        format!(
            r#"<AggregateArgumentType name="Pos" initialValue='{initial}'><MemberList>
              <Member name="x" typeRef="U8"/><Member name="y" typeRef="U8" initialValue="5"/><Member name="z" typeRef="U8"/>
            </MemberList></AggregateArgumentType>"#
        )
    };
    let text = |types: &str, argument: &str| {
        doc(&commands(
            &format!("{ARG_U8}{types}"),
            &go(
                &format!(r#"<Argument name="T" argumentTypeRef="Pos"{argument}/>"#),
                &argument_entry("T"),
            ),
        ))
    };
    let defaults = |text: &str| {
        let (bundle, codes) = validated_doc(text);
        let defaults: Vec<(String, Value)> = bundle["catalog"]["commands"]["sat.go"]["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["name"].as_str().unwrap().to_string(),
                    a["default"].clone(),
                )
            })
            .collect();
        (defaults, codes)
    };
    let named = |values: [Value; 3]| -> Vec<(String, Value)> {
        ["t_x", "t_y", "t_z"]
            .iter()
            .map(|name| name.to_string())
            .zip(values)
            .collect()
    };
    let spread = types(r#"{"x": 1, "y": 2, "z": 3}"#);
    // The type gives x and z; y's own initial value takes precedence.
    let (found, codes) = defaults(&in_1_3(&text(&spread, "")));
    assert_eq!(found, named([json!(1), json!(5), json!(3)]));
    assert!(codes.is_empty(), "{codes:?}");
    // The argument's own initial value takes precedence over both.
    let (found, _) = defaults(&in_1_3(&text(
        &spread,
        r#" initialValue='{"y": 9, "z": 8}'"#,
    )));
    assert_eq!(found, named([json!(1), json!(9), json!(8)]));
    // Before XTCE 1.3 there's none, as Yamcs reads it: left out, named.
    let (found, codes) = defaults(&text(&spread, ""));
    assert_eq!(found, named([Value::Null, json!(5), Value::Null]));
    assert_eq!(codes, ["xtce.ignored"]);
    // A nested aggregate's type gives its members theirs.
    let nested = r#"<AggregateArgumentType name="Inner" initialValue='{"a": 4}'><MemberList>
          <Member name="a" typeRef="U8"/></MemberList></AggregateArgumentType>
        <AggregateArgumentType name="Pos"><MemberList><Member name="inner" typeRef="Inner"/></MemberList></AggregateArgumentType>"#;
    let (bundle, _) = validated_doc(&in_1_3(&text(nested, "")));
    assert_eq!(
        bundle["catalog"]["commands"]["sat.go"]["arguments"][0]["default"],
        4
    );
    // What isn't an object of its members, names a member it lacks, or
    // holds what isn't a value of its member's type is refused, at the
    // type's attribute; a value outside the member's range is validation's
    // to refuse, as any default is.
    for (initial, expected) in [
        ("[1]", "isn't a JSON object of"),
        (r#"{"w": 1}"#, "has no member w"),
        (r#"{"x": "many"}"#, "many"),
        // Even where the member's own initial value overrides it.
        (r#"{"y": "many"}"#, "many"),
    ] {
        let (code, message) = one_error(&in_1_3(&text(&types(initial), "")));
        assert_eq!(code, "xtce.invalid", "{initial}: {message}");
        assert!(message.contains(expected), "{initial}: {message}");
    }
}

/// A referring import's member that its aggregate type gives a default
/// isn't required: the vendor gave it a value.
#[test]
fn a_member_its_type_gives_a_default_isnt_required() {
    let text = in_1_3(&doc(&commands(
        &format!(
            r#"{ARG_U8}<AggregateArgumentType name="Pos" initialValue='{{"x": 1}}'><MemberList>
              <Member name="x" typeRef="U8"/><Member name="y" typeRef="U8"/>
            </MemberList></AggregateArgumentType>"#
        ),
        &go(
            r#"<Argument name="T" argumentTypeRef="Pos"/>"#,
            &argument_entry("T"),
        )
        .replace(
            r#"<MetaCommand name="Go">"#,
            r#"<MetaCommand name="Go"><AliasSet><Alias nameSpace="MDL" alias="sat.go"/></AliasSet>"#,
        ),
    )));
    let catalog = catalog_of(json!({ "commands": { "sat.go": { "arguments": [
        { "name": "t_x", "type": "u64", "default": 0 },
        { "name": "t_y", "type": "u64", "default": 0 }
    ] } } }));
    let mut sources = Sources::new();
    sources.add("sat.xml", text.as_str());
    let parsed = read_with(
        sources.iter(),
        &Options::new().with_catalog(CatalogRole::Refer),
        Context::new().with_catalog(&catalog),
    )
    .unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    let json = serde_json::to_value(&parsed.bundle).unwrap();
    assert_eq!(
        json["sets"][0]["commands"][0]["arguments"],
        json!({ "t_x": { "default": 1 }, "t_y": { "required": true } })
    );
}

/// A binary type's context alarms are left out under either name: XTCE 1.0
/// to 1.2's BinaryContextAlarmList and 1.3's ContextAlarmList.
#[test]
fn a_binary_types_context_alarms_are_left_out_in_either_version() {
    let text = |list: &str| {
        doc(&telemetry(
            &format!(
                r#"<BinaryParameterType name="Blob"><BinaryDataEncoding><SizeInBits><FixedValue>24</FixedValue></SizeInBits></BinaryDataEncoding><{list}/></BinaryParameterType>"#
            ),
            &param("B", "Blob"),
            &container("P", &entry("B")),
        ))
    };
    for (list, text) in [
        ("BinaryContextAlarmList", text("BinaryContextAlarmList")),
        ("ContextAlarmList", in_1_3(&text("ContextAlarmList"))),
    ] {
        let (code, message) = one_problem(&text);
        assert_eq!(code, "xtce.ignored", "{list}: {message}");
        assert!(
            message.contains(list) && message.ends_with("alarms aren't imported"),
            "{message}"
        );
    }
}

/// A sequence container with a BinaryEncoding holding `encoding` (none
/// when it's empty), these entries, and a BaseContainer (or nothing).
fn encoded(name: &str, encoding: &str, entries: &str, base: &str) -> String {
    let encoding = if encoding.is_empty() {
        String::new()
    } else {
        format!("<BinaryEncoding>{encoding}</BinaryEncoding>")
    };
    format!(
        r#"<SequenceContainer name="{name}">{encoding}<EntryList>{entries}</EntryList>{base}</SequenceContainer>"#
    )
}

fn size(bits: i64) -> String {
    format!("<SizeInBits><FixedValue>{bits}</FixedValue></SizeInBits>")
}

/// An abstract header of an 8-bit `Id`, sized `header` bits, and a packet
/// `Hk` built on it holding a 16-bit `Value`, sized `packet` bits.
fn sized_packet(header: &str, packet: &str) -> String {
    let header = encoded("Header", header, &entry("Id"), "")
        .replace(r#"name="Header""#, r#"name="Header" abstract="true""#);
    let hk = encoded(
        "Hk",
        packet,
        &entry("Value"),
        r#"<BaseContainer containerRef="Header"/>"#,
    );
    doc(&telemetry(
        &format!("{U8}{U16}"),
        &format!("{}{}", param("Id", "U8"), param("Value", "U16")),
        &format!("{header}{hk}"),
    ))
}

/// A container's size counts from the start of the packet, as its
/// entries' locations do, so a header's says where the next container's
/// entries start and a packet's is the whole packet. XTCE 1.0 to 1.2 give
/// a container a BinaryDataEncoding, and 1.3 a type of its own.
#[test]
fn a_containers_size_pads_it_from_the_start_of_the_packet() {
    let text = sized_packet(&size(16), &size(48));
    let padded = json!([
        { "kind": "field", "name": "Id", "type": "uint", "size_bits": 8, "canonical": "sat.id" },
        { "kind": "padding", "size_bits": 8 },
        { "kind": "field", "name": "Value", "type": "uint", "size_bits": 16, "canonical": "sat.value" },
        { "kind": "padding", "size_bits": 16 },
    ]);
    // Yamcs reads a size only where a container is embedded, so padding
    // in an inheritance chain differs from what it reads.
    for text in [text.clone(), in_1_3(&text)] {
        let (json, warnings) = warned(&text);
        assert_eq!(json["sets"][0]["packets"][0]["entries"], padded);
        assert_eq!(
            codes(&warnings, "which Yamcs leaves out"),
            [
                ("xtce.differs".to_string(), true),
                ("xtce.differs".to_string(), true)
            ]
        );
    }
    // A size the entries fill adds nothing.
    let filled = sized_packet(&size(8), &size(24));
    assert_eq!(
        first_packet(&filled)["entries"].as_array().map(Vec::len),
        Some(2)
    );
    // Entries placed from the start count where they end.
    let placed = doc(&telemetry(
        U8,
        &param("Id", "U8"),
        &encoded("P", &size(32), &entry_at("Id", 16), ""),
    ));
    let (json, warnings) = warned(&placed);
    assert_eq!(
        json["sets"][0]["packets"][0]["entries"][1],
        json!({ "kind": "padding", "size_bits": 8 })
    );
    assert_eq!(warnings.len(), 1);
}

/// An embedded container's size counts from its own start, as Yamcs
/// reads it, so its struct type ends with the padding.
#[test]
fn an_embedded_containers_size_pads_its_struct_type() {
    let inner = encoded("Inner", &size(24), &entry("Id"), "")
        .replace(r#"name="Inner""#, r#"name="Inner" abstract="true""#);
    let text = doc(&telemetry(
        &format!("{U8}{U16}"),
        &format!("{}{}", param("Id", "U8"), param("Value", "U16")),
        &format!(
            "{inner}{}",
            container(
                "P",
                &format!(
                    r#"{}<ContainerRefEntry containerRef="Inner"/>"#,
                    entry("Value")
                )
            )
        ),
    ));
    let json = bundle(&text);
    assert_eq!(
        json["sets"][0]["types"]["Inner"]["entries"],
        json!([
            { "kind": "field", "name": "Id", "type": "uint", "size_bits": 8, "canonical": "sat.id" },
            { "kind": "padding", "size_bits": 16 },
        ])
    );
}

/// A command container's size pads the command packet, which encoding
/// writes as zeros.
#[test]
fn a_command_containers_size_pads_the_command() {
    let text = doc(&commands(
        ARG_U8,
        &format!(
            r#"<MetaCommand name="Go"><ArgumentList>{}</ArgumentList>
                 <CommandContainer name="Go"><BinaryEncoding>{}</BinaryEncoding><EntryList>
                   <FixedValueEntry name="Op" binaryValue="07" sizeInBits="8"/>{}
                 </EntryList></CommandContainer></MetaCommand>"#,
            argument("A", "U8"),
            size(32),
            argument_entry("A")
        ),
    ));
    let (sets, warnings) = family_sets_warned(&text);
    let encoded = sets
        .encode(&CommandRequest::new("sat", "sat.go").with_argument("a", 5_u64))
        .unwrap();
    assert_eq!(encoded.payload, [7, 5, 0, 0]);
    // Yamcs sizes a command by its entries.
    assert_eq!(
        codes(&warnings, "adds 16 bits of padding"),
        [("xtce.differs".to_string(), true)]
    );
}

#[test]
fn container_sizes_mdl_cant_pad_to_are_refused() {
    let refused = |header: &str, packet: &str| one_error(&sized_packet(header, packet));
    let (code, message) = refused(&size(4), "");
    assert_eq!(code, "xtce.invalid", "{message}");
    assert!(
        message.contains("take 8 bits, more than its SizeInBits of 4"),
        "{message}"
    );
    let (code, message) = refused(&size(0), "");
    assert_eq!(code, "xtce.invalid", "{message}");
    // XTCE 1.0 to 1.2 require a size.
    let (code, message) = one_error(&sized_packet(&size(16), "").replace(&size(16), ""));
    assert_eq!(code, "xtce.invalid", "{message}");
    assert!(message.contains("has a SizeInBits"), "{message}");
    for (encoding, wanted) in [
        (
            r#"<SizeInBits><DynamicValue><ParameterInstanceRef parameterRef="Id"/></DynamicValue></SizeInBits>"#,
            "another value gives",
        ),
        (
            r#"<SizeInBits><DiscreteLookupList><DiscreteLookup value="16"><Comparison parameterRef="Id" value="1"/></DiscreteLookup></DiscreteLookupList></SizeInBits>"#,
            "lookup list",
        ),
        (
            &format!(
                r#"{}<FromBinaryTransformAlgorithm name="X"><AlgorithmText language="java">x</AlgorithmText></FromBinaryTransformAlgorithm>"#,
                size(16)
            ),
            "FromBinaryTransformAlgorithm",
        ),
    ] {
        let (code, message) = refused(encoding, "");
        assert_eq!(code, "xtce.unsupported", "{message}");
        assert!(message.contains(wanted), "{message}");
    }
    // Before 1.3 a container's encoding has a value's byte and bit order,
    // which mean nothing but their defaults; 1.3's has neither.
    let ordered = |order: &str| {
        sized_packet(&size(16), "").replace(
            "<BinaryEncoding><SizeInBits><FixedValue>16",
            &format!("<BinaryEncoding {order}><SizeInBits><FixedValue>16"),
        )
    };
    let (json, _) = warned(&ordered(
        r#"byteOrder="mostSignificantByteFirst" bitOrder="mostSignificantBitFirst""#,
    ));
    assert_eq!(
        json["sets"][0]["packets"][0]["entries"][1],
        json!({ "kind": "padding", "size_bits": 8 })
    );
    let (code, message) = one_error(&ordered(r#"byteOrder="leastSignificantByteFirst""#));
    assert_eq!(code, "xtce.unsupported", "{message}");
    let (code, _) = one_error(&ordered(r#"bitOrder="sideways""#));
    assert_eq!(code, "xtce.invalid");
    let (code, message) = one_error(&in_1_3(&ordered(r#"byteOrder="mostSignificantByteFirst""#)));
    assert_eq!(code, "xtce.unsupported", "{message}");
    assert!(message.contains("attribute byteOrder"), "{message}");
    // A size after an entry a condition can leave out has no fixed place.
    let conditional = doc(&telemetry(
        &format!("{U8}{U16}"),
        &format!("{}{}", param("Id", "U8"), param("Value", "U16")),
        &encoded(
            "P",
            &size(32),
            &format!(
                r#"{}<ParameterRefEntry parameterRef="Value"><IncludeCondition><Comparison parameterRef="Id" value="1"/></IncludeCondition></ParameterRefEntry>"#,
                entry("Id")
            ),
            "",
        ),
    ));
    let (code, message) = one_error(&conditional);
    assert_eq!(code, "xtce.unsupported", "{message}");
    assert!(message.contains("whose end varies"), "{message}");
}

const U32: &str = r#"<IntegerParameterType name="U32" signed="false"><IntegerDataEncoding sizeInBits="32"/></IntegerParameterType>"#;
const NINE: &str = r#"<BinaryParameterType name="Nine"><BinaryDataEncoding><SizeInBits><FixedValue>72</FixedValue></SizeInBits></BinaryDataEncoding></BinaryParameterType>"#;

/// An XTCE 1.3 packet `P` of nine bytes of `Data` and then `held`, whose
/// container's ErrorDetectCorrect holds `checks`; the parameters are those
/// of `held`, each of type `U8`, `U16` or `U32` as its name ends.
fn checked(checks: &str, held: &[&str]) -> String {
    let params: String = held
        .iter()
        .map(|name| {
            let ty = if name.ends_with("32") {
                "U32"
            } else if name.ends_with("16") {
                "U16"
            } else {
                "U8"
            };
            param(name, ty)
        })
        .collect();
    let entries: String = held.iter().map(|name| entry(name)).collect();
    in_1_3(&doc(&telemetry(
        &format!("{U8}{U16}{U32}{NINE}"),
        &format!("{}{params}", param("Data", "Nine")),
        &encoded(
            "P",
            &format!("<ErrorDetectCorrect>{checks}</ErrorDetectCorrect>"),
            &format!("{}{entries}", entry("Data")),
            "",
        ),
    )))
}

/// A document imported without a warning, validated and built, its one
/// family's sets ready to decode and encode with.
fn family_sets(text: &str) -> FamilySets {
    let (sets, warnings) = family_sets_warned(text);
    assert!(warnings.is_empty(), "{warnings:#?}");
    sets
}

/// A document imported, validated and built, and its warnings as (code,
/// message).
fn family_sets_warned(text: &str) -> (FamilySets, Vec<(String, String)>) {
    let (sources, result) = import_all(&[("sat.xml", text)]);
    let parsed = result.unwrap_or_else(|d| panic!("{:#?}", shown(&sources, &d)));
    let warnings = shown(&sources, &parsed.warnings)
        .into_iter()
        .map(|(_, code, _, message)| (code, message))
        .collect();
    let validated = validate(parsed.bundle)
        .unwrap_or_else(|d| panic!("{:#?}", shown(&sources, &parsed.map.locate_all(&d))));
    let artifacts = build(&validated).unwrap();
    let load = |written: &Written| read_artifact(written.canonical.as_bytes()).unwrap();
    let sets: Vec<_> = artifacts.sets.values().map(load).collect();
    let family = artifacts.families.values().next().unwrap();
    let sets = FamilySets::new(&load(family), &sets, &[load(&artifacts.catalog)]).unwrap();
    (sets, warnings)
}

/// Decoding `payload` from the document's one asset: each entry's raw
/// unsigned value by name, or the error's code.
fn raw_values(sets: &FamilySets, payload: &[u8]) -> Result<Vec<(String, u64)>, &'static str> {
    let bindings = Bindings::from_toml(
        r#"
mdl_version = "2.0"

[[binding]]
family = "sat"
native_id = "Sat"
asset = "SAT-1"
set = "sat"
effective_from = "2026-01-01T00:00:00Z"
"#,
    )
    .unwrap();
    let ground = "2026-06-01T12:00:00Z".parse().unwrap();
    let decoded = sets
        .decode(&Message::new(Payload::Binary(payload), ground), &bindings)
        .map_err(|e| e.code())?;
    Ok(decoded
        .record
        .iter()
        .filter_map(|(name, value)| match value {
            Raw::Uint(n) => Some((name.to_string(), *n)),
            _ => None,
        })
        .collect())
}

/// The CRC catalogue's check input.
const CHECK: &[u8] = b"123456789";

/// `CHECK` and then each value, big-endian in `bytes` bytes.
fn check_payload(values: &[(u64, usize)]) -> Vec<u8> {
    let mut payload = CHECK.to_vec();
    for (value, bytes) in values {
        payload.extend_from_slice(&value.to_be_bytes()[8 - bytes..]);
    }
    payload
}

/// A check naming the parameter holding its value (XTCE 1.3) makes that
/// field a checksum entry, verified on decoding, with MDL's parameters for
/// it; the parameter leaves the catalog, since no field maps it. Each CRC
/// gives the CRC catalogue's check value for its input.
#[test]
fn a_check_makes_the_parameter_holding_it_a_checksum() {
    // CRC-16/IBM-3740: unreflected, from 0xFFFF.
    let text = checked(
        r#"<CRC width="16" parameterRef="Crc16"><Polynomial>1021</Polynomial><InitRemainder>FFFF</InitRemainder></CRC>"#,
        &["Crc16"],
    );
    let json = bundle(&text);
    assert_eq!(
        json["sets"][0]["packets"][0]["entries"][1],
        json!({
            "kind": "checksum",
            "name": "Crc16",
            "algorithm": {
                "kind": "crc", "width": 16, "poly": 4129, "init": 65535,
                "reflect_in": false, "reflect_out": false, "xor_out": 0
            }
        })
    );
    assert_eq!(
        json["catalog"]["params"]
            .as_object()
            .map(|params| params.keys().cloned().collect::<Vec<_>>()),
        Some(vec!["sat.data".to_string()])
    );
    let sets = family_sets(&text);
    assert_eq!(
        raw_values(&sets, &check_payload(&[(0x29B1, 2)])),
        Ok(vec![("Crc16".to_string(), 0x29B1)])
    );
    let mut damaged = check_payload(&[(0x29B1, 2)]);
    damaged[4] ^= 0x10;
    assert_eq!(raw_values(&sets, &damaged), Err("bad_checksum"));
    // Each reflection is its own.
    let one_way = bundle(&checked(
        r#"<CRC width="16" reflectData="true" parameterRef="Crc16"><Polynomial>1021</Polynomial></CRC>"#,
        &["Crc16"],
    ));
    let algorithm = &one_way["sets"][0]["packets"][0]["entries"][1]["algorithm"];
    assert_eq!(
        (&algorithm["reflect_in"], &algorithm["reflect_out"]),
        (&json!(true), &json!(false))
    );
    // A little-endian check counts toward the family's byte order, which
    // it's then in.
    let little = r#"<IntegerParameterType name="L16" signed="false"><IntegerDataEncoding sizeInBits="16" byteOrder="leastSignificantByteFirst"/></IntegerParameterType>"#;
    let text = in_1_3(&doc(&telemetry(
        &format!("{NINE}{little}"),
        &format!("{}{}", param("Data", "Nine"), param("Crc", "L16")),
        &encoded(
            "P",
            r#"<ErrorDetectCorrect><CRC width="16" parameterRef="Crc"><Polynomial>1021</Polynomial><InitRemainder>FFFF</InitRemainder></CRC></ErrorDetectCorrect>"#,
            &format!("{}{}", entry("Data"), entry("Crc")),
            "",
        ),
    )));
    assert_eq!(bundle(&text)["families"][0]["byte_order"], json!("little"));
    let mut payload = CHECK.to_vec();
    payload.extend_from_slice(&0x29B1_u16.to_le_bytes());
    assert_eq!(
        raw_values(&family_sets(&text), &payload),
        Ok(vec![("Crc".to_string(), 0x29B1)])
    );

    // CRC-16/ARC reflects each byte in and the result; CRC-32/ISO-HDLC
    // also ends with a FinalXOR.
    for (check, width, value) in [
        (
            r#"<CRC width="16" reflectData="true" reflectRemainder="true" parameterRef="Crc16"><Polynomial>8005</Polynomial></CRC>"#,
            "Crc16",
            0xBB3D,
        ),
        (
            r#"<CRC width="32" reflectData="true" reflectRemainder="true" parameterRef="Crc32"><Polynomial>04C11DB7</Polynomial><InitRemainder>FFFFFFFF</InitRemainder><FinalXOR>FFFFFFFF</FinalXOR></CRC>"#,
            "Crc32",
            0xCBF4_3926,
        ),
    ] {
        let bytes = if width == "Crc32" { 4 } else { 2 };
        let sets = family_sets(&checked(check, &[width]));
        assert_eq!(
            raw_values(&sets, &check_payload(&[(value, bytes)])),
            Ok(vec![(width.to_string(), value)]),
            "{check}"
        );
    }
}

/// `sum8` adds the bytes modulo 256 and XOR exclusive-ors them; a check
/// covers the bytes from `bitsFromReference` up to its own value, and two
/// checks may end a packet, the second covering the first.
#[test]
fn sums_xors_and_two_checks_end_a_packet() {
    let sum: u64 = CHECK.iter().map(|b| u64::from(*b)).sum::<u64>() % 256;
    let xor = CHECK.iter().fold(0_u8, |x, b| x ^ b);
    assert_eq!((sum, xor), (0xDD, 0x31));
    let text = checked(
        r#"<Checksum name="sum8" parameterRef="Sum8"/><XOR bitsFromReference="8" parameterRef="Xor8"/>"#,
        &["Sum8", "Xor8"],
    );
    let entries = &bundle(&text)["sets"][0]["packets"][0]["entries"];
    assert_eq!(
        entries[1]["algorithm"],
        json!({ "kind": "sum", "width": 8 })
    );
    assert_eq!(entries[2]["algorithm"], json!({ "kind": "xor" }));
    assert_eq!(entries[2]["from_bits"], json!(8));
    let sets = family_sets(&text);
    // The XOR covers the bytes after the first, and the sum before it.
    let xor_after = CHECK[1..].iter().fold(sum as u8, |x, b| x ^ b);
    assert_eq!(
        raw_values(
            &sets,
            &check_payload(&[(sum, 1), (u64::from(xor_after), 1)])
        ),
        Ok(vec![
            ("Sum8".to_string(), sum),
            ("Xor8".to_string(), u64::from(xor_after))
        ])
    );
    assert_eq!(
        raw_values(&sets, &check_payload(&[(sum, 1), (u64::from(xor), 1)])),
        Err("bad_checksum")
    );
}

/// A command container's check is worked out when encoding.
#[test]
fn a_command_containers_check_is_written_when_encoding() {
    let text = in_1_3(&doc(&format!(
        "{}{}",
        telemetry(U16, &param("Crc16", "U16"), ""),
        commands(
            ARG_U8,
            &format!(
                r#"<MetaCommand name="Go"><ArgumentList>{}</ArgumentList>
                     <CommandContainer name="Go"><BinaryEncoding><ErrorDetectCorrect>
                       <CRC width="16" parameterRef="Crc16"><Polynomial>1021</Polynomial><InitRemainder>FFFF</InitRemainder></CRC>
                     </ErrorDetectCorrect></BinaryEncoding><EntryList>
                       <FixedValueEntry name="Op" binaryValue="3132333435363738" sizeInBits="64"/>{}
                       <ParameterRefEntry parameterRef="Crc16"/>
                     </EntryList></CommandContainer></MetaCommand>"#,
                argument("A", "U8"),
                argument_entry("A")
            ),
        )
    )));
    let sets = family_sets(&text);
    let encoded = sets
        .encode(&CommandRequest::new("sat", "sat.go").with_argument("a", 0x39_u64))
        .unwrap();
    assert_eq!(encoded.payload, check_payload(&[(0x29B1, 2)]));
}

/// A check that names no parameter holding its value has nothing to be
/// checked against, so it's left out: always so before XTCE 1.3.
#[test]
fn a_check_naming_no_parameter_is_left_out() {
    let crc = r#"<CRC width="16"><Polynomial>1021</Polynomial></CRC>"#;
    let older = sized_packet(
        &format!("<ErrorDetectCorrect>{crc}</ErrorDetectCorrect>{}", size(8)),
        "",
    );
    let newer = in_1_3(&sized_packet(
        &format!("<ErrorDetectCorrect>{crc}</ErrorDetectCorrect>"),
        "",
    ));
    for text in [older, newer] {
        let (code, message) = one_problem(&text);
        assert_eq!(code, "xtce.ignored", "{message}");
        assert!(message.contains("names no parameter"), "{message}");
    }
}

#[test]
fn checks_mdl_cant_compute_are_refused() {
    let crc = |attrs: &str, poly: &str| {
        format!(r#"<CRC {attrs} parameterRef="Crc16"><Polynomial>{poly}</Polynomial></CRC>"#)
    };
    for (check, code, wanted) in [
        (
            r#"<Parity type="Even" parameterRef="Crc16"/>"#.to_string(),
            "xtce.unsupported",
            "one bit",
        ),
        (
            r#"<Checksum name="sum16" parameterRef="Crc16"/>"#.to_string(),
            "xtce.unsupported",
            "bytes or words",
        ),
        (
            r#"<Checksum name="fletcher16" parameterRef="Crc16"/>"#.to_string(),
            "xtce.unsupported",
            "ISO 8473-1",
        ),
        (
            r#"<Checksum name="sum8" hashSizeInBits="16" parameterRef="Crc16"/>"#.to_string(),
            "xtce.unsupported",
            "which of them",
        ),
        (
            r#"<Checksum name="custom" parameterRef="Crc16"><InputAlgorithm name="X"/></Checksum>"#
                .to_string(),
            "xtce.unsupported",
            "InputAlgorithm",
        ),
        (
            r#"<Checksum name="sum9" parameterRef="Crc16"/>"#.to_string(),
            "xtce.invalid",
            "isn't one XTCE names",
        ),
        (crc(r#"width="12""#, "80F"), "xtce.unsupported", "12 bits"),
        (
            crc(r#"width="16" direction="leastSignificantBitFirst""#, "1021"),
            "xtce.unsupported",
            "least significant bit first",
        ),
        (
            crc(r#"width="16" reference="end""#, "1021"),
            "xtce.unsupported",
            "container's end",
        ),
        (
            crc(r#"width="16" bitsFromReference="4""#, "1021"),
            "xtce.unsupported",
            "whole bytes",
        ),
        (crc(r#"width="16""#, "011021"), "xtce.invalid", "top bit"),
        (
            crc(r#"width="16""#, "10G1"),
            "xtce.invalid",
            "isn't hexadecimal digits",
        ),
        (
            crc(r#"width="16""#, "0x1021"),
            "xtce.invalid",
            "hexadecimal digits without 0x",
        ),
        (
            crc(r#"width="16""#, "1021").replace("Crc16", "Nothing"),
            "xtce.reference",
            "Nothing",
        ),
    ] {
        let (got, message) = one_error(&checked(&check, &["Crc16"]));
        assert_eq!(got, code, "{check}: {message}");
        assert!(message.contains(wanted), "{check}: {message}");
    }
    // An embedded container's bytes aren't a packet's.
    let inner = encoded(
        "Inner",
        r#"<ErrorDetectCorrect><XOR parameterRef="X"/></ErrorDetectCorrect>"#,
        &entry("X"),
        "",
    )
    .replace(r#"name="Inner""#, r#"name="Inner" abstract="true""#);
    let text = in_1_3(&doc(&telemetry(
        U8,
        &param("X", "U8"),
        &format!(
            r#"{inner}{}"#,
            container("P", r#"<ContainerRefEntry containerRef="Inner"/>"#)
        ),
    )));
    let (code, message) = one_error(&text);
    assert_eq!(code, "xtce.unsupported", "{message}");
    assert!(message.contains("embedded container"), "{message}");
}

/// The parameter holding a check is a field at the packet's top level, an
/// unsigned integer of the check's width whose raw value is its value, in
/// the family's byte order, and only other checks follow it.
#[test]
fn a_checks_parameter_is_a_field_mdl_can_check() {
    let xor = r#"<XOR parameterRef="X"/>"#;
    let refused = |text: &str, code: &str, wanted: &str| {
        let (got, message) = one_error(text);
        assert_eq!(got, code, "{message}");
        assert!(message.contains(wanted), "{message}");
    };
    let with = |types: &str, params: &str, entries: &str| {
        in_1_3(&doc(&telemetry(
            &format!("{U8}{U16}{types}"),
            &format!("{}{params}", param("Id", "U8")),
            &encoded(
                "P",
                &format!("<ErrorDetectCorrect>{xor}</ErrorDetectCorrect>"),
                entries,
                "",
            ),
        )))
    };
    // Something other than a check follows it.
    refused(
        &with(
            "",
            &param("X", "U8"),
            &format!("{}{}", entry("X"), entry("Id")),
        ),
        "xtce.unsupported",
        "entry Id follows it",
    );
    // Too wide, signed, enumerated, or on a condition.
    refused(
        &with(
            "",
            &param("X", "U16"),
            &format!("{}{}", entry("Id"), entry("X")),
        ),
        "xtce.unsupported",
        "it's 16 bits, and the check's value is 8",
    );
    let signed = r#"<IntegerParameterType name="S8" signed="true"><IntegerDataEncoding sizeInBits="8" encoding="twosComplement"/></IntegerParameterType>"#;
    refused(
        &with(
            signed,
            &param("X", "S8"),
            &format!("{}{}", entry("Id"), entry("X")),
        ),
        "xtce.unsupported",
        "a signed integer",
    );
    let labeled = r#"<EnumeratedParameterType name="Labeled"><IntegerDataEncoding sizeInBits="8"/><EnumerationList><Enumeration value="0" label="OFF"/><Enumeration value="1" label="ON"/></EnumerationList></EnumeratedParameterType>"#;
    refused(
        &with(
            labeled,
            &param("X", "Labeled"),
            &format!("{}{}", entry("Id"), entry("X")),
        ),
        "xtce.unsupported",
        "its values are labels",
    );
    refused(
        &with(
            "",
            &param("X", "U8"),
            &format!(
                r#"{}<ParameterRefEntry parameterRef="X"><IncludeCondition><Comparison parameterRef="Id" value="1"/></IncludeCondition></ParameterRefEntry>"#,
                entry("Id")
            ),
        ),
        "xtce.unsupported",
        "on a condition",
    );
    // A little-endian check in a big-endian family.
    let little = r#"<IntegerParameterType name="L16" signed="false"><IntegerDataEncoding sizeInBits="16" byteOrder="leastSignificantByteFirst"/></IntegerParameterType>"#;
    let text = in_1_3(&doc(&telemetry(
        &format!("{U16}{little}"),
        &format!("{}{}", param("Id", "U16"), param("C", "L16")),
        &encoded(
            "P",
            r#"<ErrorDetectCorrect><CRC width="16" parameterRef="C"><Polynomial>1021</Polynomial></CRC></ErrorDetectCorrect>"#,
            &format!("{}{}", entry("Id"), entry("C")),
            "",
        ),
    )));
    refused(&text, "xtce.unsupported", "little-endian");
    // Two checks held by one parameter.
    refused(
        &with(
            "",
            &param("X", "U8"),
            &format!("{}{}", entry("Id"), entry("X")),
        )
        .replace(xor, &format!("{xor}{xor}")),
        "xtce.invalid",
        "two checks are held by",
    );
}

/// A check on a container covers each packet built on it; one that
/// doesn't hold the check's parameter has nothing to check it against, so
/// it's imported without the check.
#[test]
fn a_packet_without_the_checks_parameter_is_imported_without_the_check() {
    let base = encoded(
        "Base",
        r#"<ErrorDetectCorrect><XOR parameterRef="X"/></ErrorDetectCorrect>"#,
        &entry("Id"),
        "",
    );
    let child = container("Child", &entry("X")).replace(
        "</SequenceContainer>",
        r#"<BaseContainer containerRef="Base"><RestrictionCriteria><Comparison parameterRef="Id" value="1"/></RestrictionCriteria></BaseContainer></SequenceContainer>"#,
    );
    let text = in_1_3(&doc(&telemetry(
        U8,
        &format!("{}{}", param("Id", "U8"), param("X", "U8")),
        &format!("{base}{child}"),
    )));
    let (json, warnings) = warned(&text);
    let packets = &json["sets"][0]["packets"];
    assert_eq!(packets[0]["name"], json!("Base"));
    assert_eq!(packets[0]["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(packets[1]["entries"][1]["kind"], json!("checksum"));
    assert_eq!(
        codes(&warnings, "container Base doesn't hold /Sat/X"),
        [("xtce.ignored".to_string(), true)]
    );
}

/// Where entries are placed from the start of the packet, only other
/// checks follow a check in bits, whatever order they're listed in.
#[test]
fn a_check_is_last_by_where_entries_are_placed() {
    let text = |entries: &str| {
        in_1_3(&doc(&telemetry(
            U8,
            &format!(
                "{}{}{}",
                param("A", "U8"),
                param("B", "U8"),
                param("X", "U8")
            ),
            &encoded(
                "P",
                r#"<ErrorDetectCorrect><XOR parameterRef="X"/></ErrorDetectCorrect>"#,
                entries,
                "",
            ),
        )))
    };
    // Listed last, but placed first: it would cover nothing.
    let (code, message) = one_error(&text(&format!(
        "{}{}{}",
        entry_at("A", 8),
        entry_at("B", 16),
        entry_at("X", 0)
    )));
    assert_eq!(code, "xtce.unsupported", "{message}");
    assert!(message.contains("entry A follows it"), "{message}");
    // Listed first, but placed last: it covers the bytes before it.
    let sets = family_sets(&text(&format!(
        "{}{}{}",
        entry_at("X", 16),
        entry_at("A", 0),
        entry_at("B", 8)
    )));
    assert_eq!(
        raw_values(&sets, &[0x0F, 0xF0, 0xFF]),
        Ok(vec![
            ("X".to_string(), 0xFF),
            ("A".to_string(), 0x0F),
            ("B".to_string(), 0xF0)
        ])
    );
    assert_eq!(raw_values(&sets, &[0x0F, 0xF0, 0x00]), Err("bad_checksum"));
    // Placed from the start, but not all at fixed places.
    let (code, message) = one_error(&text(&format!(
        r#"{}<ParameterRefEntry parameterRef="B"><IncludeCondition><Comparison parameterRef="A" value="1"/></IncludeCondition></ParameterRefEntry>{}"#,
        entry_at("A", 0),
        entry("X")
    )));
    assert_eq!(code, "xtce.unsupported", "{message}");
    assert!(message.contains("not all at fixed places"), "{message}");
}

/// A size follows the last entry listed, as MDL places it, so that one
/// reaches furthest.
#[test]
fn a_size_after_entries_placed_out_of_order_is_refused() {
    for (a, size) in [(40, 64), (56, 64)] {
        let text = doc(&telemetry(
            U8,
            &format!("{}{}", param("A", "U8"), param("B", "U8")),
            &encoded(
                "P",
                &self::size(size),
                &format!("{}{}", entry_at("A", a), entry_at("B", 0)),
                "",
            ),
        ));
        let (code, message) = one_error(&text);
        assert_eq!(code, "xtce.unsupported", "{message}");
        assert!(message.contains("placed out of order"), "{message}");
    }
}

/// XTCE 1.3 deprecates error detection in a value's encoding for a
/// container's, and doesn't say what the old one covers.
#[test]
fn error_detection_in_a_values_encoding_is_refused() {
    let text = doc(&telemetry(
        r#"<IntegerParameterType name="C" signed="false"><IntegerDataEncoding sizeInBits="16"><ErrorDetectCorrect><CRC width="16"><Polynomial>1021</Polynomial></CRC></ErrorDetectCorrect></IntegerDataEncoding></IntegerParameterType>"#,
        &param("X", "C"),
        &container("P", &entry("X")),
    ));
    assert_eq!(
        one_error(&text),
        (
            "xtce.unsupported".to_string(),
            "error detection in a value's encoding isn't supported: XTCE doesn't say whether the \
             value holds the check or is covered by it, and 1.3 deprecates it for a container's \
             ErrorDetectCorrect naming the parameter that holds it (parameterRef), which MDL reads"
                .to_string()
        )
    );
}

/// A check counted from where its own value starts covers no bytes. The
/// import writes it as the document says, and validation refuses it at
/// bitsFromReference, as it would a checksum from any frontend.
#[test]
fn a_check_over_no_bytes_is_refused_where_its_start_is_given() {
    let text = checked(
        r#"<CRC width="16" bitsFromReference="72" parameterRef="Crc16"><Polynomial>1021</Polynomial></CRC>"#,
        &["Crc16"],
    );
    let (sources, result) = import_all(&[("sat.xml", &text)]);
    let parsed = result.unwrap_or_else(|d| panic!("{:#?}", shown(&sources, &d)));
    let diagnostics = parsed.map.locate_all(&validate(parsed.bundle).unwrap_err());
    let shown = shown(&sources, &diagnostics);
    assert_eq!(shown.len(), 1, "{shown:#?}");
    assert_eq!(
        (shown[0].1.as_str(), shown[0].3.as_str()),
        (
            "checksum",
            "a checksum covers at least one byte before its own start (bit 72), so from_bits \
             can't be 72"
        )
    );
    let span = diagnostics[0].span.unwrap();
    assert_eq!(&text[span.start..span.end], "72");
    assert!(text[..span.start].ends_with(r#"<CRC width="16" bitsFromReference=""#));
}

// ---- Composites (§4.1) ----

const RATE: &str = r#"<FloatParameterType name="Rate"><UnitSet><Unit>rad/s</Unit></UnitSet><FloatDataEncoding sizeInBits="32"/></FloatParameterType>"#;

/// An AncillaryDataSet of (name, text).
fn ancillary(items: &[(&str, &str)]) -> String {
    let data: String = items
        .iter()
        .map(|(name, text)| format!(r#"<AncillaryData name="{name}">{text}</AncillaryData>"#))
        .collect();
    format!("<AncillaryDataSet>{data}</AncillaryDataSet>")
}

/// An aggregate parameter type of (member, type), with `extra` children
/// (its ancillary data) before its members.
fn aggregate(name: &str, members: &[(&str, &str)], extra: &str) -> String {
    let members: String = members
        .iter()
        .map(|(member, ty)| format!(r#"<Member name="{member}" typeRef="{ty}"/>"#))
        .collect();
    format!(
        r#"<AggregateParameterType name="{name}">{extra}<MemberList>{members}</MemberList></AggregateParameterType>"#
    )
}

/// A parameter of `ty` with `children` (an alias, ancillary data).
fn param_with(name: &str, ty: &str, children: &str) -> String {
    format!(r#"<Parameter name="{name}" parameterTypeRef="{ty}">{children}</Parameter>"#)
}

/// A document whose one packet, ADCS, holds parameter `Rate` of the
/// three-member rate vector `Vec`, given `children`.
fn rate_vector(children: &str) -> String {
    let types =
        RATE.to_string() + &aggregate("Vec", &[("x", "Rate"), ("y", "Rate"), ("z", "Rate")], "");
    let params = param_with("Rate", "Vec", children);
    doc(&telemetry(
        &types,
        &params,
        &container("ADCS", &entry("Rate")),
    ))
}

#[test]
fn an_aggregate_marked_with_a_frame_is_one_composite_sample() {
    let text = rate_vector(&ancillary(&[("MDL.frame", "BODY")]));
    let (packets, _) = valid(&text);
    let packet = &packets[0];
    // The members are fields in document order, each mapped to no param of
    // its own: the composite is the one sample.
    let entries = packet["entries"].as_array().unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Rate_x", "Rate_y", "Rate_z"]);
    assert!(
        entries.iter().all(|e| e.get("canonical").is_none()),
        "{packet:#}"
    );
    assert_eq!(
        packet["composites"],
        json!([{ "canonical": "sat.rate", "frame": "BODY",
                 "members": ["Rate_x", "Rate_y", "Rate_z"] }])
    );
    // An import that owns its catalog defines the param, in the source's
    // frame and the members' unit, and none for the members.
    assert_eq!(
        bundle(&text)["catalog"]["params"],
        json!({ "sat.rate": { "type": "vec3", "unit": "rad/s", "frame": "BODY" } })
    );
}

#[test]
fn mdl_members_orders_a_scalar_last_quaternion() {
    // The type orders its members as MDL's quaternion is, scalar first;
    // the parameter gives the frame, over the type's none.
    let q =
        r#"<FloatParameterType name="Q"><FloatDataEncoding sizeInBits="32"/></FloatParameterType>"#;
    let quat = aggregate(
        "Quat",
        &[("q1", "Q"), ("q2", "Q"), ("q3", "Q"), ("q4", "Q")],
        &ancillary(&[("MDL.members", "q4 q1 q2 q3")]),
    );
    let params = param_with("Att", "Quat", &ancillary(&[("MDL.frame", "ECI_J2000")]));
    let text = doc(&telemetry(
        &(q.to_string() + &quat),
        &params,
        &container("ADCS", &entry("Att")),
    ));
    let (packets, _) = valid(&text);
    let names: Vec<&str> = packets[0]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Att_q1", "Att_q2", "Att_q3", "Att_q4"]);
    assert_eq!(
        packets[0]["composites"],
        json!([{ "canonical": "sat.att", "frame": "ECI_J2000",
                 "members": ["Att_q4", "Att_q1", "Att_q2", "Att_q3"] }])
    );
    assert_eq!(
        bundle(&text)["catalog"]["params"],
        json!({ "sat.att": { "type": "quat", "frame": "ECI_J2000" } })
    );
}

#[test]
fn a_member_aggregate_whose_type_is_marked_is_a_composite_of_its_own() {
    let floats = |name: &str, unit: &str| {
        format!(
            r#"<FloatParameterType name="{name}"><UnitSet><Unit>{unit}</Unit></UnitSet><FloatDataEncoding sizeInBits="32"/></FloatParameterType>"#
        )
    };
    let ecef = ancillary(&[("MDL.frame", "ECEF")]);
    let types = floats("Pos", "m")
        + &floats("Speed", "m/s")
        + &aggregate("PosVec", &[("x", "Pos"), ("y", "Pos"), ("z", "Pos")], &ecef)
        + &aggregate(
            "VelVec",
            &[("x", "Speed"), ("y", "Speed"), ("z", "Speed")],
            &ecef,
        )
        + &aggregate("State", &[("pos", "PosVec"), ("vel", "VelVec")], "");
    let text = doc(&telemetry(
        &types,
        &param("State", "State"),
        &container("NAV", &entry("State")),
    ));
    let (packets, _) = valid(&text);
    assert_eq!(
        packets[0]["composites"],
        json!([
            { "canonical": "sat.state.pos", "frame": "ECEF",
              "members": ["State_pos_x", "State_pos_y", "State_pos_z"] },
            { "canonical": "sat.state.vel", "frame": "ECEF",
              "members": ["State_vel_x", "State_vel_y", "State_vel_z"] }
        ])
    );
    assert_eq!(
        bundle(&text)["catalog"]["params"],
        json!({
            "sat.state.pos": { "type": "vec3", "unit": "m", "frame": "ECEF" },
            "sat.state.vel": { "type": "vec3", "unit": "m/s", "frame": "ECEF" }
        })
    );
}

#[test]
fn a_referring_import_maps_a_composite_onto_the_catalogs_vector_by_its_alias() {
    // GEODETIC positions, latitude and longitude in degrees and altitude in
    // metres, into the owner's ECI_J2000 position.
    let floats = |name: &str, unit: &str| {
        format!(
            r#"<FloatParameterType name="{name}"><UnitSet><Unit>{unit}</Unit></UnitSet><FloatDataEncoding sizeInBits="64"/></FloatParameterType>"#
        )
    };
    let types = floats("Angle", "deg")
        + &floats("Height", "m")
        + &aggregate(
            "Geo",
            &[("lat", "Angle"), ("lon", "Angle"), ("alt", "Height")],
            "",
        );
    let children = format!(
        r#"<AliasSet><Alias nameSpace="MDL" alias="nav.position"/></AliasSet>{}"#,
        ancillary(&[("MDL.frame", "GEODETIC")])
    );
    let text = doc(&telemetry(
        &types,
        &param_with("Where", "Geo", &children),
        &container("NAV", &entry("Where")),
    ));
    let mut sources = Sources::new();
    sources.add("sat.xml", text.as_str());
    let owners = catalog_of(json!({
        "params": { "nav.position": { "type": "vec3", "unit": "m", "frame": "ECI_J2000" } }
    }));
    let parsed = read_with(
        sources.iter(),
        &Options::new().with_catalog(CatalogRole::Refer),
        Context::new().with_catalog(&owners),
    )
    .unwrap_or_else(|e| panic!("{:#?}", shown(&sources, &e)));
    let mut bundle = parsed.bundle;
    let json = serde_json::to_value(&bundle).unwrap();
    assert_eq!(json["catalog"], json!({}));
    assert_eq!(
        json["sets"][0]["packets"][0]["composites"],
        json!([{ "canonical": "nav.position", "frame": "GEODETIC",
                 "members": ["Where_lat", "Where_lon", "Where_alt"] }])
    );
    bundle.catalog = owners;
    if let Err(diagnostics) = validate(bundle) {
        panic!(
            "{:#?}",
            shown(&sources, &parsed.map.locate_all(&diagnostics))
        );
    }
}

#[test]
fn a_composite_is_refused_where_it_cant_be_one() {
    let frame = |name: &str| ancillary(&[("MDL.frame", name)]);
    let cases: Vec<(String, &str, &str)> = vec![
        (
            rate_vector(&ancillary(&[("MDL.frame", "SIDEWAYS")])),
            "xtce.invalid",
            "MDL.frame \"SIDEWAYS\" isn't a frame",
        ),
        (
            rate_vector(&ancillary(&[("MDL.members", "x y z")])),
            "xtce.invalid",
            "MDL.members orders a composite's members, and /Sat/Rate is no composite",
        ),
        (
            rate_vector(&ancillary(&[
                ("MDL.frame", "BODY"),
                ("MDL.members", "x y w"),
            ])),
            "xtce.invalid",
            "MDL.members names w, which /Sat/Rate has no member of",
        ),
        (
            rate_vector(&ancillary(&[
                ("MDL.frame", "BODY"),
                ("MDL.members", "x x y"),
            ])),
            "xtce.invalid",
            "MDL.members names x twice",
        ),
        (
            rate_vector(&ancillary(&[("MDL.frame", "BODY"), ("MDL.members", "z y")])),
            "xtce.invalid",
            "MDL.members names each of /Sat/Rate's members once, and leaves out x",
        ),
        (
            rate_vector(&ancillary(&[("MDL.frame", "BODY"), ("MDL.frame", "ECEF")])),
            "xtce.invalid",
            "gives MDL.frame once",
        ),
        (
            rate_vector(&ancillary(&[("MDL.Frame", "BODY")])),
            "xtce.invalid",
            "MDL.Frame isn't MDL ancillary data, which is MDL.frame or MDL.members",
        ),
        (
            rate_vector(&frame("GEODETIC")),
            "xtce.unsupported",
            "/Sat/Rate is a composite of GEODETIC positions, whose param is in ECEF or \
             ECI_J2000, so an import that owns its catalog can't define its param",
        ),
    ];
    for (text, code, message) in cases {
        let (got_code, got) = one_error(&text);
        assert_eq!(got_code, code, "{got}");
        assert!(got.contains(message), "{got}");
    }

    // Two members, a member that isn't a number, members in two units.
    let two = doc(&telemetry(
        &(RATE.to_string() + &aggregate("Pair", &[("x", "Rate"), ("y", "Rate")], "")),
        &param_with("P", "Pair", &frame("BODY")),
        &container("C", &entry("P")),
    ));
    let (code, message) = one_error(&two);
    assert_eq!(code, "xtce.invalid");
    assert!(
        message.contains("a vec3 of 3 members or a quat of 4, and it has 2"),
        "{message}"
    );
    let flag = r#"<BooleanParameterType name="Flag"><IntegerDataEncoding sizeInBits="8"/></BooleanParameterType>"#;
    let mixed_kinds = doc(&telemetry(
        &(RATE.to_string()
            + flag
            + &aggregate("V", &[("x", "Rate"), ("y", "Rate"), ("ok", "Flag")], "")),
        &param_with("P", "V", &frame("BODY")),
        &container("C", &entry("P")),
    ));
    let (code, message) = one_error(&mixed_kinds);
    assert_eq!(code, "xtce.invalid");
    assert!(
        message.contains("member ok of /Sat/P isn't a number"),
        "{message}"
    );
    let metres = r#"<FloatParameterType name="Len"><UnitSet><Unit>m</Unit></UnitSet><FloatDataEncoding sizeInBits="32"/></FloatParameterType>"#;
    let mixed_units = doc(&telemetry(
        &(RATE.to_string()
            + metres
            + &aggregate("V", &[("x", "Rate"), ("y", "Rate"), ("z", "Len")], "")),
        &param_with("P", "V", &frame("BODY")),
        &container("C", &entry("P")),
    ));
    let (code, message) = one_error(&mixed_units);
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.contains("members are in different units (rad/s, rad/s, m)"),
        "{message}"
    );

    // A scalar parameter, and an array parameter, marked.
    let scalar = doc(&telemetry(
        RATE,
        &param_with("R", "Rate", &frame("BODY")),
        &container("C", &entry("R")),
    ));
    let (code, message) = one_error(&scalar);
    assert_eq!(code, "xtce.invalid");
    assert!(message.contains("isn't an aggregate"), "{message}");
    let array = doc(&telemetry(
        &(RATE.to_string() + &array_type("Rates", "Rate", "<FixedValue>2</FixedValue>")),
        &param_with("R", "Rates", &frame("BODY")),
        &container("C", &entry("R")),
    ));
    let (code, message) = one_error(&array);
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.contains("is an array, which can't be a composite yet"),
        "{message}"
    );

    // A referring import's composite needs its alias.
    let mut sources = Sources::new();
    let referred = rate_vector(&frame("BODY"));
    sources.add("sat.xml", referred.as_str());
    let owners = catalog_of(json!({
        "params": { "sat.rate": { "type": "vec3", "unit": "rad/s", "frame": "BODY" } }
    }));
    let errors = read_with(
        sources.iter(),
        &Options::new().with_catalog(CatalogRole::Refer),
        Context::new().with_catalog(&owners),
    )
    .map(|_| ())
    .unwrap_err();
    let shown = shown(&sources, &errors);
    assert_eq!(shown.len(), 1, "{shown:#?}");
    assert_eq!(shown[0].1, "xtce.invalid");
    assert!(
        shown[0].3.contains("give parameter Rate one"),
        "{}",
        shown[0].3
    );
}

#[test]
fn a_composite_in_a_repeat_or_an_embedded_container_isnt_supported() {
    let marked = param_with("Rate", "Vec", &ancillary(&[("MDL.frame", "BODY")]));
    let types =
        RATE.to_string() + &aggregate("Vec", &[("x", "Rate"), ("y", "Rate"), ("z", "Rate")], "");
    let repeated = doc(&telemetry(
        &types,
        &marked,
        &container(
            "C",
            r#"<ParameterRefEntry parameterRef="Rate"><RepeatEntry><Count><FixedValue>2</FixedValue></Count></RepeatEntry></ParameterRefEntry>"#,
        ),
    ));
    let (code, message) = one_error(&repeated);
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.contains("is a composite in a repeated entry"),
        "{message}"
    );
    let embedded = doc(&telemetry(
        &types,
        &marked,
        &(container("Inner", &entry("Rate"))
            + &container("Outer", r#"<ContainerRefEntry containerRef="Inner"/>"#)),
    ));
    let (code, message) = one_error(&embedded);
    assert_eq!(code, "xtce.unsupported");
    assert!(
        message.contains("is in embedded container Inner"),
        "{message}"
    );
    // MDL.members without a frame is refused on a repeated entry too.
    let ordered = doc(&telemetry(
        &types,
        &param_with("Rate", "Vec", &ancillary(&[("MDL.members", "z y x")])),
        &container(
            "C",
            r#"<ParameterRefEntry parameterRef="Rate"><RepeatEntry><Count><FixedValue>2</FixedValue></Count></RepeatEntry></ParameterRefEntry>"#,
        ),
    ));
    let (code, message) = one_error(&ordered);
    assert_eq!(code, "xtce.invalid");
    assert!(
        message.contains("MDL.members orders a composite's members, and /Sat/Rate is no composite"),
        "{message}"
    );
}

#[test]
fn ancillary_data_outside_the_mdl_namespace_is_left_out_with_a_warning() {
    let text = rate_vector(&ancillary(&[
        ("MDL.frame", "BODY"),
        ("Vendor.colour", "red"),
    ]));
    let (json, warnings) = warned(&text);
    assert_eq!(
        warnings,
        [(
            "xtce.ignored".to_string(),
            "Parameter Rate's ancillary data outside the MDL namespace isn't imported".to_string()
        )]
    );
    assert_eq!(
        json["sets"][0]["packets"][0]["composites"][0]["canonical"],
        "sat.rate"
    );
}
