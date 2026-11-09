use mdl::v2::source::{Parsed, Severity, SourceDiagnostic, Sources};
use mdl::v2::validate::validate;
use proptest::prelude::*;
use serde_json::{Value, json};

use crate::{Code, Options, read};

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
             SpaceSystem in http://www.omg.org/space/xtce (XTCE 1.0 and 1.1) or \
             http://www.omg.org/spec/XTCE/20180204 (XTCE 1.2)"
                .into()
        )
    );
    let older = r#"<xtce:SpaceSystem name="Sat" xmlns:xtce="http://www.omg.org/space/xtce"/>"#;
    assert_eq!(bundle(older)["families"][0]["name"], "sat");
    let (_, result) = import_all(&[]);
    assert_eq!(result.unwrap_err()[0].code, "xtce.document");
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
            r#"<IntegerParameterType name="T"><IntegerDataEncoding sizeInBits="8" encoding="signMagnitude"/></IntegerParameterType>"#,
            "signMagnitude integers aren't supported: MDL reads unsigned and two's complement",
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
    assert_eq!(codes, [mdl::v2::diagnostics::Code::AmbiguousIdentify]);
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
        .with_alias(mdl::v2::ir::Locator::metadata("scid"));
    let bundle = serde_json::to_value(read(sources.iter(), &options).unwrap().bundle).unwrap();
    assert_eq!(bundle["families"][0]["name"], "vendor_x");
    assert_eq!(
        bundle["families"][0]["alias"],
        json!({ "kind": "metadata", "name": "scid" })
    );
    assert_eq!(bundle["sets"][0]["name"], "vendor_x_v1");
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
                    {"name": "delaytime", "type": "u64", "unit": "s", "range": {"min": 1.0, "max": 600.0}, "default": 10}
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
    for size in [
        "SizeInBits><LeadingSize sizeInBitsOfSizeTag=\"8\"/></SizeInBits>",
        "Variable maxSizeInBits=\"64\"><DynamicValue><ArgumentInstanceRef argumentRef=\"N\"/></DynamicValue></Variable>",
    ] {
        assert_eq!(
            one_error(&go_with(&string(size), "A")),
            (
                "xtce.unsupported".into(),
                "a string argument whose size another argument or a leading size gives isn't \
                 supported yet"
                    .into()
            ),
            "{size}"
        );
    }
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
        json!({"min": -5.0, "max": 9_007_199_254_740_991.0})
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
    for (range, code) in [
        (
            r#"<ValidRangeSet><ValidRange minInclusive="0"/><ValidRange maxInclusive="9"/></ValidRangeSet>"#,
            "xtce.unsupported",
        ),
        (
            r#"<ValidRangeSet><ValidRange minInclusive="0"/></ValidRangeSet><ValidRange minInclusive="0"/>"#,
            "xtce.invalid",
        ),
        (
            r#"<ValidRangeSet><ValidRange maxInclusive="9007199254740992"/></ValidRangeSet>"#,
            "xtce.unsupported",
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
        json!({"max": 65535.0})
    );
    assert_eq!(
        validated_range(
            &integer(
                "",
                r#"<IntegerDataEncoding sizeInBits="12" encoding="twosComplement"/>"#
            ),
            &[]
        ),
        json!({"min": -2048.0, "max": 2047.0})
    );
    // An unsigned type over a two's complement encoding is an i64 kept at 0
    // and above, since a command writes a u64 to a uint field only.
    let unsigned_int = integer(
        r#" signed="false""#,
        r#"<IntegerDataEncoding sizeInBits="8" encoding="twosComplement"/>"#,
    );
    assert_eq!(
        validated_range(&unsigned_int, &[]),
        json!({"min": 0.0, "max": 127.0})
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
        json!({"min": 0.0, "max": 2_147_483_647.0})
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
    // An MDL range ends within ±(2^53 - 1), so a wider field's top values
    // are refused.
    assert_eq!(
        validated_range(
            &integer(
                r#" signed="false" sizeInBits="64""#,
                r#"<IntegerDataEncoding sizeInBits="60"/>"#
            ),
            &["xtce.differs"]
        ),
        json!({"max": 9_007_199_254_740_991.0})
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
        json!({"min": 0.0, "max": 255.5_f64.next_down()})
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
    let (code, message) = one_error(&based(
        r#"<BaseContainer containerRef="Base"><RestrictionCriteria><Comparison parameterRef="P" value="1"/></RestrictionCriteria></BaseContainer>"#,
    ));
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "xtce.unsupported",
            "restriction criteria on a command container aren't supported yet"
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
            r#"<ParameterRefEntry parameterRef="P"/>"#,
            "a parameter's value in a command isn't supported yet",
        ),
        (
            r#"<ContainerRefEntry containerRef="C"/>"#,
            "ContainerRefEntry in a command isn't supported: an MDL command packet holds \
             arguments and constants",
        ),
        (
            r#"<ArrayArgumentRefEntry argumentRef="A"/>"#,
            "array arguments aren't supported yet",
        ),
        (
            r#"<FixedValueEntry binaryValue="01"><IncludeCondition><Comparison parameterRef="P" value="1"/></IncludeCondition></FixedValueEntry>"#,
            "an entry of a command on a condition isn't supported: every entry of an MDL \
             command packet is written",
        ),
        (
            r#"<FixedValueEntry binaryValue="01"><RepeatEntry><Count><FixedValue>2</FixedValue></Count></RepeatEntry></FixedValueEntry>"#,
            "a repeated entry in a command isn't supported yet",
        ),
    ] {
        let text = doc(&commands(ARG_U8, &go("", entry)));
        assert_eq!(
            one_error(&text),
            ("xtce.unsupported".into(), message.into()),
            "{entry}"
        );
    }
    for ty in [
        r#"<FloatArgumentType name="A"><IntegerDataEncoding sizeInBits="8"><DefaultCalibrator><PolynomialCalibrator><Term coefficient="2" exponent="1"/></PolynomialCalibrator></DefaultCalibrator></IntegerDataEncoding></FloatArgumentType>"#,
        r#"<AggregateArgumentType name="A"><MemberList><Member name="x" typeRef="U8"/></MemberList></AggregateArgumentType>"#,
        r#"<AbsoluteTimeArgumentType name="A"/>"#,
    ] {
        assert_eq!(one_error(&go_with(ty, "A")).0, "xtce.unsupported", "{ty}");
    }
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
