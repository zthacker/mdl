use super::*;
use crate::diagnostics::Code;

#[test]
fn locations_count_lines_and_characters_from_one() {
    let mut sources = Sources::new();
    let id = sources.add("a.txt", "ab\ncé\tz\n");
    let at = |offset| sources.location(Span::new(id, offset, offset)).unwrap();
    assert_eq!(at(0), Location { line: 1, column: 1 });
    assert_eq!(at(2), Location { line: 1, column: 3 });
    assert_eq!(at(3), Location { line: 2, column: 1 });
    // é is two bytes; the column counts it once.
    assert_eq!(at(4), Location { line: 2, column: 2 });
    assert_eq!(at(6), Location { line: 2, column: 3 });
    // Inside é counts as é.
    assert_eq!(at(5), Location { line: 2, column: 2 });
    // The end of the text, after the last line break, and past it.
    assert_eq!(at(9), Location { line: 3, column: 1 });
    assert_eq!(at(99), Location { line: 3, column: 1 });
}

#[test]
fn a_span_from_other_sources_has_no_location() {
    let mut other = Sources::new();
    other.add("x", "x");
    let id = other.add("y", "y");
    let sources = Sources::new();
    assert_eq!(sources.location(Span::new(id, 0, 1)), None);
    assert_eq!(sources.name(id), None);
    assert_eq!(sources.text(id), None);
}

#[test]
fn a_pointer_finds_itself_or_its_nearest_recorded_ancestor() {
    let mut sources = Sources::new();
    let id = sources.add("b.json", "{}");
    let mut map = SourceMap::new();
    map.insert("", Span::new(id, 0, 2));
    map.insert("/sets/0", Span::new(id, 10, 20));
    map.insert("/sets/0/packets/1", Span::new(id, 12, 14));
    assert_eq!(map.find("/sets/0/packets/1"), Some(Span::new(id, 12, 14)));
    assert_eq!(
        map.find("/sets/0/packets/1/entries/3/unit"),
        Some(Span::new(id, 12, 14))
    );
    assert_eq!(map.find("/sets/0/packets/0"), Some(Span::new(id, 10, 20)));
    // Not the bundle's span: it can't stand for a family it doesn't have.
    assert_eq!(map.find("/families/2"), None);
    assert_eq!(map.get("/sets"), None);
    assert_eq!(SourceMap::new().find("/sets/0"), None);
}

#[test]
fn the_climb_stops_below_the_bundles_own_lists_and_maps() {
    let mut sources = Sources::new();
    let file = sources.add("catalog.toml", "a\nb\nc\n");
    let mut map = SourceMap::new();
    let first = Span::new(file, 0, 1);
    let b = Span::new(file, 2, 3);
    // As TOML records a map from one file: at its first definition.
    map.insert("/catalog/params", first);
    map.insert("/catalog/params/b", b);
    map.insert("", first);
    // A changed definition is still found; a removed one isn't pinned on
    // whichever definition the map's span happens to cover.
    assert_eq!(map.find("/catalog/params/b/unit"), Some(b));
    assert_eq!(map.find("/catalog/params/gone"), None);
    assert_eq!(map.find("/catalog/params/gone/unit"), None);
    assert_eq!(map.find("/sets/0"), None);
    assert_eq!(map.find("/mdl_version"), None);
    // The lists and maps themselves are found exactly.
    assert_eq!(map.find("/catalog/params"), Some(first));
    assert_eq!(map.find(""), Some(first));
}

#[test]
fn a_diagnostic_shows_its_line_and_underlines_its_span() {
    let mut sources = Sources::new();
    let text = "{\n  \"unit\": \"A\",\n  \"x\": 1\n}\n";
    let id = sources.add("bundle.json", text);
    let start = text.find("\"A\"").unwrap();
    let diagnostic = SourceDiagnostic::error("unit_mismatch", "unit A isn't V")
        .with_context("set s, packet HK, entry bus")
        .with_span(Span::new(id, start, start + 3));
    assert_eq!(
        diagnostic.display(&sources).to_string(),
        "error[unit_mismatch]: unit A isn't V\n \
         --> bundle.json:2:11\n  \
         |\n\
         2 |   \"unit\": \"A\",\n  \
         |           ^^^\n  \
         = set s, packet HK, entry bus\n"
    );
}

#[test]
fn the_gutter_widens_with_the_line_number_and_tabs_line_up() {
    let mut sources = Sources::new();
    let mut text = "\n".repeat(11);
    text.push_str("\tkey = 1\r\n");
    let id = sources.add("t.toml", text.as_str());
    let start = text.find("key").unwrap();
    let diagnostic =
        SourceDiagnostic::warning("toml.example", "here").with_span(Span::new(id, start, start));
    assert_eq!(
        diagnostic.display(&sources).to_string(),
        "warning[toml.example]: here\n  \
         --> t.toml:12:2\n   \
         |\n\
         12 |     key = 1\n   \
         |     ^\n"
    );
}

#[test]
fn a_span_past_its_line_is_underlined_to_the_line_end() {
    let mut sources = Sources::new();
    let text = "[\n  {\"a\": 1,\n   \"b\": 2}\n]";
    let id = sources.add("m.json", text);
    let start = text.find('{').unwrap();
    let end = text.find('}').unwrap() + 1;
    let rendered = SourceDiagnostic::error("wrong_shape", "bad")
        .with_span(Span::new(id, start, end))
        .display(&sources)
        .to_string();
    assert!(
        rendered.ends_with("2 |   {\"a\": 1,\n  |   ^^^^^^^^\n"),
        "{rendered}"
    );
}

#[test]
fn without_a_span_a_diagnostic_names_its_path() {
    let sources = Sources::new();
    let diagnostic = SourceDiagnostic::error("empty", "no packets").with_path("/sets/0/packets");
    assert_eq!(
        diagnostic.display(&sources).to_string(),
        "error[empty]: no packets\n  = at /sets/0/packets\n"
    );
    assert_eq!(diagnostic.to_string(), "error[empty]: no packets");
}

#[test]
fn validation_diagnostics_keep_their_code_message_and_context() {
    let mut sources = Sources::new();
    let id = sources.add("b.json", "{}");
    let mut map = SourceMap::new();
    map.insert("/sets/0", Span::new(id, 0, 2));
    let diagnostic = Diagnostic {
        code: Code::UnitMismatch,
        path: "/sets/0/packets/0".to_string(),
        context: "set s".to_string(),
        message: "m".to_string(),
        first: None,
    };
    let located = map.locate(&diagnostic);
    assert_eq!(located.code, "unit_mismatch");
    assert_eq!(located.severity, Severity::Error);
    assert_eq!(located.path, "/sets/0/packets/0");
    assert_eq!(located.context, "set s");
    assert_eq!(located.span, Some(Span::new(id, 0, 2)));
}

#[test]
fn files_are_read_through_their_id_in_the_order_added() {
    let mut sources = Sources::new();
    let a = sources.add("a.toml", "x = 1\ny = 2\n");
    let b = sources.add("b.toml", "z = 3");
    let file = sources.get(b).unwrap();
    assert_eq!(
        (file.id(), file.name(), file.text()),
        (b, "b.toml", "z = 3")
    );
    assert_eq!(file.span(0, 1), Span::new(b, 0, 1));
    let a_file = sources.get(a).unwrap();
    assert_eq!(a_file.line_start(2), Some(6));
    assert_eq!(a_file.line_start(0), None);
    assert_eq!(a_file.line_start(4), None);
    let names: Vec<_> = sources.iter().map(|f| (f.id(), f.name())).collect();
    assert_eq!(names, [(a, "a.toml"), (b, "b.toml")]);
}

#[test]
fn spans_order_by_file_then_position() {
    let mut sources = Sources::new();
    let a = sources.add("a", "");
    let b = sources.add("b", "");
    let mut spans = vec![Span::new(b, 0, 1), Span::new(a, 5, 6), Span::new(a, 1, 9)];
    spans.sort();
    assert_eq!(
        spans,
        [Span::new(a, 1, 9), Span::new(a, 5, 6), Span::new(b, 0, 1)]
    );
}

#[test]
fn a_long_line_is_shown_as_a_window_around_the_span() {
    let mut sources = Sources::new();
    let text = format!("{}\"unit\": \"A\"{}", "a".repeat(500), "b".repeat(500));
    let id = sources.add("min.json", text.as_str());
    let start = text.find("\"A\"").unwrap();
    let rendered = SourceDiagnostic::error("unit_mismatch", "m")
        .with_span(Span::new(id, start, start + 3))
        .display(&sources)
        .to_string();
    // 40 characters before the span, 80 from its start.
    let shown = format!("...{}\"unit\": \"A\"{}...", "a".repeat(32), "b".repeat(77));
    let caret = format!("{}^^^", " ".repeat(3 + 40));
    assert_eq!(
        rendered,
        format!(
            "error[unit_mismatch]: m\n --> min.json:1:{}\n  |\n1 | {shown}\n  | {caret}\n",
            start + 1
        )
    );
}

#[test]
fn a_long_line_near_its_start_is_cut_only_at_the_end() {
    let mut sources = Sources::new();
    let text = format!("[1, x{}]", ", 2".repeat(100));
    let id = sources.add("n.json", text.as_str());
    let rendered = SourceDiagnostic::error("invalid_json", "m")
        .with_span(Span::new(id, 4, 5))
        .display(&sources)
        .to_string();
    let line = rendered.lines().nth(3).unwrap();
    assert!(line.starts_with("1 | [1, x, 2"), "{line}");
    assert!(line.ends_with("..."), "{line}");
    assert_eq!(line.chars().count(), "1 | ".len() + 4 + 80 + 3);
    assert_eq!(rendered.lines().nth(4).unwrap(), "  |     ^");
}

#[test]
fn control_characters_in_the_source_are_printed_as_spaces() {
    let mut sources = Sources::new();
    let text = "a\rb\u{1b}[2Jc = 1";
    let id = sources.add("c.toml", text);
    let start = text.find('c').unwrap();
    let rendered = SourceDiagnostic::error("toml.example", "m")
        .with_span(Span::new(id, start, start + 1))
        .display(&sources)
        .to_string();
    assert!(!rendered.contains('\r') && !rendered.contains('\u{1b}'));
    assert!(rendered.contains("1 | a b [2Jc = 1\n"), "{rendered}");
    assert!(rendered.contains("  |        ^\n"), "{rendered}");
}

#[test]
fn a_duplicate_names_where_the_first_one_is() {
    let mut sources = Sources::new();
    let a = sources.add("a.toml", "[[families]]\nname = \"x\"\n");
    let b = sources.add("b.toml", "\n[[families]]\nname = \"x\"\n");
    let mut map = SourceMap::new();
    map.insert("/families/0", Span::new(a, 0, 12));
    map.insert("/families/1", Span::new(b, 1, 13));
    let diagnostic = Diagnostic {
        code: Code::DuplicateName,
        path: "/families/1".to_string(),
        context: "family x".to_string(),
        message: "family x is defined twice".to_string(),
        first: Some("/families/0".to_string()),
    };
    assert_eq!(
        diagnostic.to_string(),
        "family x: family x is defined twice; the first is at /families/0 [duplicate_name]"
    );
    let located = map.locate(&diagnostic);
    assert_eq!(located.first, Some(Span::new(a, 0, 12)));
    assert_eq!(located.message, "family x is defined twice");
    assert_eq!(
        located.display(&sources).to_string(),
        "error[duplicate_name]: family x is defined twice\n \
         --> b.toml:2:1\n  \
         |\n\
         2 | [[families]]\n  \
         | ^^^^^^^^^^^^\n  \
         = family x\n  \
         = the first is at a.toml:1:1\n"
    );

    // A first the map doesn't have is named in the message.
    let unmapped = SourceMap::new().locate(&diagnostic);
    assert_eq!(unmapped.first, None);
    assert_eq!(
        unmapped.message,
        "family x is defined twice; the first is at /families/0"
    );
}
