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
        one_problem(&text("<BooleanExpression/>")).1,
        "inclusion by a boolean expression isn't supported yet"
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
    let entries = [
        (
            r#"<ContainerRefEntry containerRef="P"/>"#,
            "ContainerRefEntry isn't supported yet",
        ),
        (
            r#"<ParameterRefEntry parameterRef="X"><RepeatEntry><Count><FixedValue>2</FixedValue></Count></RepeatEntry></ParameterRefEntry>"#,
            "repeated entries aren't supported yet",
        ),
        (
            r#"<xi:include xmlns:xi="http://www.w3.org/2001/XInclude" href="more.xml"/>"#,
            "XInclude isn't supported; give the importer the included files instead",
        ),
    ];
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
         <CommandMetaData><MetaCommandSet/></CommandMetaData>"
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
            "FloatParameterType Temp's DefaultAlarm isn't imported: alarms aren't imported",
            "type Flag's labels (ON, OFF) aren't imported: MDL booleans are true and false",
            "parameter /Sat/Unused is in no packet, so it isn't imported",
            "aliases in ops aren't imported: MDL reads only the MDL namespace",
            "TelemetryMetaData's AlgorithmSet isn't imported: MDL decodes what packets carry, \
             not what algorithms compute",
            "CommandMetaData's MetaCommandSet isn't imported: it doesn't change decoded values",
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
            "/Sat/A is in this packet twice, and MDL maps a param from one field per packet".into()
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
        message.starts_with("aggregates nested more than 32 deep"),
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

#[test]
fn every_code_is_listed_once_under_the_importers_name() {
    let mut seen = std::collections::BTreeSet::new();
    for code in Code::ALL {
        assert!(code.as_str().starts_with("xtce."), "{code:?}");
        assert!(seen.insert(code.as_str()), "{code:?}");
    }
}

proptest! {
    #[test]
    fn any_text_never_panics(text in "\\PC{0,200}") {
        let _ = import_all(&[("any.xml", &text)]);
    }

    #[test]
    fn any_cut_of_a_real_document_never_panics(cut in 0_usize..8000, length in 0_usize..400) {
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
