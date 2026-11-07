//! XTCE documents: parsing, namespaces, and reading elements and
//! attributes with the spans diagnostics point at.
//!
//! Every element the importer reads is checked against what it handles
//! ([`Expect`]): an attribute or child it doesn't know is an error, since
//! the importer can't tell whether leaving it out would change a value,
//! and one it knows doesn't matter is a warning. Nothing is dropped
//! without a diagnostic.

use std::borrow::Cow;

use mdl::v2::source::{SourceFile, Span};
use roxmltree::{Document, Node, ParsingOptions, TextPos};

use crate::Code;
use crate::report::Report;

/// XTCE 1.0 and 1.1 share a namespace.
pub(crate) const XTCE_1_1: &str = "http://www.omg.org/space/xtce";
pub(crate) const XTCE_1_2: &str = "http://www.omg.org/spec/XTCE/20180204";
const XINCLUDE: &str = "http://www.w3.org/2001/XInclude";

/// The file's text as an XTCE document, or its problems reported.
pub(crate) fn parse<'a>(file: SourceFile<'a>, report: &mut Report) -> Option<Document<'a>> {
    let options = ParsingOptions {
        // No DTDs: an XTCE document needs none, and refusing them rules out
        // external entities and entity expansion.
        allow_dtd: false,
        ..ParsingOptions::default()
    };
    let document = match Document::parse_with_options(file.text(), options) {
        Ok(document) => document,
        Err(err) => {
            report.error(Code::Syntax, error_span(file, &err), error_message(&err));
            return None;
        }
    };
    let root = document.root_element();
    let name = root.tag_name();
    let known = matches!(name.namespace(), Some(XTCE_1_1 | XTCE_1_2));
    if name.name() != "SpaceSystem" || !known {
        let found = match name.namespace() {
            Some(namespace) => format!("{} in namespace {namespace}", name.name()),
            None => format!("{} in no namespace", name.name()),
        };
        let span = El::new(root, file).span();
        report.error(
            Code::Document,
            span,
            format!(
                "this isn't an XTCE document: its root is {found}, not a SpaceSystem in \
                 {XTCE_1_1} (XTCE 1.0 and 1.1) or {XTCE_1_2} (XTCE 1.2)"
            ),
        );
        return None;
    }
    Some(document)
}

/// roxmltree's message without the position it appends, which the span
/// shows.
fn error_message(err: &roxmltree::Error) -> String {
    let text = err.to_string();
    let suffix = format!(" at {}", err.pos());
    let message = text.strip_suffix(&suffix).unwrap_or(&text);
    match err {
        roxmltree::Error::DtdDetected => {
            "the document has a DTD, which XTCE documents don't need; remove the DOCTYPE"
                .to_string()
        }
        _ => format!("this isn't well-formed XML: {message}"),
    }
}

fn error_span(file: SourceFile<'_>, err: &roxmltree::Error) -> Span {
    let text = file.text();
    let at_end = || file.span(text.len(), text.len());
    match err {
        roxmltree::Error::UnexpectedEndOfStream | roxmltree::Error::UnclosedRootNode => at_end(),
        roxmltree::Error::DtdDetected => match text.find("<!DOCTYPE") {
            Some(start) => file.span(start, start + "<!DOCTYPE".len()),
            None => at_end(),
        },
        _ => {
            let offset = offset_of(file, err.pos());
            let end = text[offset..]
                .chars()
                .next()
                .map_or(offset, |c| offset + c.len_utf8());
            file.span(offset, end)
        }
    }
}

/// The byte offset of a line and column (characters), both from 1.
fn offset_of(file: SourceFile<'_>, pos: TextPos) -> usize {
    let text = file.text();
    let line = usize::try_from(pos.row).unwrap_or(usize::MAX);
    let Some(start) = file.line_start(line) else {
        return text.len();
    };
    let skip = usize::try_from(pos.col.saturating_sub(1)).unwrap_or(usize::MAX);
    text[start..]
        .char_indices()
        .nth(skip)
        .map_or(text.len(), |(offset, _)| start + offset)
}

/// An element of an XTCE document.
#[derive(Clone, Copy)]
pub(crate) struct El<'d> {
    pub(crate) node: Node<'d, 'd>,
    pub(crate) file: SourceFile<'d>,
}

/// An attribute's value and where it is.
#[derive(Clone, Copy)]
pub(crate) struct Attr<'d> {
    pub(crate) name: &'d str,
    pub(crate) value: &'d str,
    pub(crate) span: Span,
}

impl<'d> El<'d> {
    pub(crate) fn new(node: Node<'d, 'd>, file: SourceFile<'d>) -> Self {
        El { node, file }
    }

    /// The element's local name.
    pub(crate) fn name(&self) -> &'d str {
        self.node.tag_name().name()
    }

    /// The `name` attribute, which every named XTCE item has.
    pub(crate) fn item_name(&self) -> Option<&'d str> {
        self.node.attribute("name")
    }

    /// Where diagnostics about the element point: its tag in the start tag.
    pub(crate) fn span(&self) -> Span {
        let start = self.node.range().start;
        let rest = self.file.text().get(start..).unwrap_or_default();
        let len = rest
            .char_indices()
            .skip(1)
            .find(|(_, c)| c.is_whitespace() || *c == '>' || *c == '/')
            .map_or(rest.len(), |(offset, _)| offset);
        self.file.span(start, start + len)
    }

    pub(crate) fn attr(&self, name: &str) -> Option<Attr<'d>> {
        self.node
            .attributes()
            .find(|a| a.namespace().is_none() && a.name() == name)
            .map(|a| {
                let range = a.range_value();
                Attr {
                    name: a.name(),
                    value: a.value(),
                    span: self.file.span(range.start, range.end),
                }
            })
    }

    /// A required attribute, or an error saying it's missing.
    pub(crate) fn required(&self, name: &str, report: &mut Report) -> Option<Attr<'d>> {
        let attr = self.attr(name);
        if attr.is_none() {
            report.error(
                Code::Invalid,
                self.span(),
                format!("{} needs a {name} attribute", self.name()),
            );
        }
        attr
    }

    /// Element children in the element's own namespace.
    pub(crate) fn children(&self) -> impl Iterator<Item = El<'d>> + 'd {
        let namespace = self.node.tag_name().namespace();
        let file = self.file;
        self.node
            .children()
            .filter(move |n| n.is_element() && n.tag_name().namespace() == namespace)
            .map(move |n| El::new(n, file))
    }

    /// The children named `name`.
    pub(crate) fn all(&self, name: &'d str) -> impl Iterator<Item = El<'d>> + 'd {
        self.children().filter(move |c| c.name() == name)
    }

    /// The first child named `name`.
    pub(crate) fn child(&self, name: &str) -> Option<El<'d>> {
        self.children().find(|c| c.name() == name)
    }

    /// The element's text, trimmed, and where it is (the element itself
    /// when it has none). Text split by comments is joined, as XML reads
    /// an element's text.
    pub(crate) fn text(&self) -> (Cow<'d, str>, Span) {
        let pieces: Vec<Node<'d, 'd>> = self.node.children().filter(|n| n.is_text()).collect();
        let filled: Vec<&Node<'d, 'd>> = pieces
            .iter()
            .filter(|n| !n.text().unwrap_or_default().trim().is_empty())
            .collect();
        let (Some(first), Some(last)) = (filled.first(), filled.last()) else {
            return (Cow::Borrowed(""), self.span());
        };
        let first_text = first.text().unwrap_or_default();
        let last_text = last.text().unwrap_or_default();
        let start = first.range().start + (first_text.len() - first_text.trim_start().len());
        let end = last.range().end - (last_text.len() - last_text.trim_end().len());
        let span = self.file.span(start, end.max(start));
        if filled.len() == 1 {
            return (Cow::Borrowed(first_text.trim()), span);
        }
        let joined: String = pieces
            .iter()
            .map(|n| n.text().unwrap_or_default())
            .collect();
        (Cow::Owned(joined.trim().to_string()), span)
    }

    /// The element for messages: its kind, and its name if it has one
    /// (`SpaceSystem EPS`).
    pub(crate) fn describe(&self) -> String {
        match self.item_name() {
            Some(name) => format!("{} {name}", self.name()),
            None => self.name().to_string(),
        }
    }

    /// Reports every attribute and child the caller doesn't handle.
    pub(crate) fn check(&self, expect: &Expect<'_>, report: &mut Report) {
        let owner = &self.describe();
        for attr in self.node.attributes() {
            let range = attr.range_qname();
            let span = self.file.span(range.start, range.end);
            match attr.namespace() {
                // xsi:schemaLocation, xml:lang and the like are about the
                // document, not what it defines.
                Some(namespace) if is_document_namespace(namespace) => {}
                Some(namespace) => report.warning(
                    Code::Ignored,
                    span,
                    format!(
                        "{owner}'s attribute {} from namespace {namespace} isn't imported: \
                         MDL reads only XTCE",
                        attr.name()
                    ),
                ),
                None if expect.attrs.contains(&attr.name()) => {}
                None if expect.ignored.contains(&attr.name()) => {
                    report.warning(Code::Ignored, span, ignored(owner, attr.name()));
                }
                None => report.error(
                    Code::Unsupported,
                    span,
                    format!(
                        "{owner}'s attribute {} isn't supported, so this can't be imported \
                         faithfully",
                        attr.name()
                    ),
                ),
            }
        }
        let namespace = self.node.tag_name().namespace();
        for node in self.node.children() {
            if node.is_text() {
                let text = node.text().unwrap_or_default();
                if !expect.text && !text.trim().is_empty() {
                    let range = node.range();
                    report.error(
                        Code::Invalid,
                        self.file.span(range.start, range.end),
                        format!("{owner} holds elements, not text"),
                    );
                }
                continue;
            }
            if !node.is_element() {
                continue;
            }
            let child = El::new(node, self.file);
            let name = child.name();
            match node.tag_name().namespace() {
                Some(XINCLUDE) => report.error(
                    Code::Unsupported,
                    child.span(),
                    "XInclude isn't supported; give the importer the included files instead",
                ),
                other if other != namespace => report.error(
                    Code::Unsupported,
                    child.span(),
                    format!(
                        "{name} from namespace {} isn't XTCE, so this can't be imported \
                         faithfully",
                        other.unwrap_or("(none)")
                    ),
                ),
                _ if expect.children.contains(&name) => {}
                _ if expect.ignored.contains(&name) => {
                    report.warning(Code::Ignored, child.span(), ignored(owner, name));
                }
                _ => report.error(
                    Code::Unsupported,
                    child.span(),
                    format!(
                        "{owner}'s {name} isn't supported, so this can't be imported faithfully"
                    ),
                ),
            }
        }
    }
}

/// What an element's reader handles.
pub(crate) struct Expect<'s> {
    /// Attributes it reads.
    pub(crate) attrs: &'s [&'s str],
    /// Children it reads.
    pub(crate) children: &'s [&'s str],
    /// Attributes and children that don't change decoded values and
    /// aren't imported, each a warning.
    pub(crate) ignored: &'s [&'s str],
    /// Whether the element holds text.
    pub(crate) text: bool,
}

fn is_document_namespace(namespace: &str) -> bool {
    matches!(
        namespace,
        "http://www.w3.org/2001/XMLSchema-instance" | "http://www.w3.org/XML/1998/namespace"
    )
}

/// The warning for something left out, saying what it is.
fn ignored(owner: &str, name: &str) -> String {
    let what = match name {
        "shortDescription" | "LongDescription" => "MDL keeps descriptions for catalog params only",
        "AliasSet" => "MDL reads only aliases in the MDL namespace",
        "AncillaryDataSet" => "MDL has no ancillary data",
        "initialValue" => "MDL has no initial values",
        "changeThreshold" => "MDL has no change thresholds",
        "DefaultAlarm" | "ContextAlarmList" => "alarms aren't imported",
        "WatchRange" | "DistressRange" | "SevereRange" => {
            "MDL limits have two levels, yellow and red (XTCE's warning and critical)"
        }
        "ChangeAlarmRanges" => "MDL limits are on values, not their changes",
        "minConformance" => "MDL limits apply to each value",
        "ValidRange" | "validRangeAppliesToCalibrated" => "MDL has no valid ranges",
        "ToString" => "MDL has no display formats",
        "dataSource" | "readOnly" | "persistence" | "SystemName" | "PhysicalAddressSet" => {
            "MDL has no parameter properties"
        }
        "ValidityCondition" => {
            "MDL has no validity conditions, which set a value's quality, not the value"
        }
        "DefaultRateInStream" | "RateInStreamSet" => "MDL has no expected rates",
        "idlePattern" => "MDL has no idle patterns",
        "Header" => "MDL keeps no document header",
        "AlgorithmSet" => "MDL decodes what packets carry, not what algorithms compute",
        "StreamSet" => "framing streams is the link's job, before MDL",
        "ServiceSet" => "MDL has no services",
        "operationalStatus" => "MDL has no operational status",
        "oneStringValue" | "zeroStringValue" => "MDL booleans are true and false",
        "maxSizeInBits" => "MDL doesn't bound a variable string's size",
        "description" | "power" | "factor" | "form" => "it's part of a unit",
        "name" => "MDL doesn't name calibrators",
        _ => "it doesn't change decoded values",
    };
    format!("{owner}'s {name} isn't imported: {what}")
}

/// An `xs:boolean`.
pub(crate) fn boolean(attr: Attr<'_>, report: &mut Report) -> Option<bool> {
    match attr.value.trim() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        other => {
            report.error(
                Code::Invalid,
                attr.span,
                format!("{} is {other:?}, not true or false", attr.name),
            );
            None
        }
    }
}

/// An `xs:long`.
pub(crate) fn long(attr: Attr<'_>, report: &mut Report) -> Option<i64> {
    long_text(attr.value, attr.name, attr.span, report)
}

pub(crate) fn long_text(text: &str, what: &str, span: Span, report: &mut Report) -> Option<i64> {
    let trimmed = text.trim();
    let digits = trimmed.strip_prefix('+').unwrap_or(trimmed);
    match digits.parse::<i64>() {
        Ok(n) => Some(n),
        Err(_) => {
            report.error(
                Code::Invalid,
                span,
                format!("{what} is {trimmed:?}, not an integer"),
            );
            None
        }
    }
}

/// An `xs:double`, finite: an MDL definition holds no NaN or infinity.
pub(crate) fn double(attr: Attr<'_>, report: &mut Report) -> Option<f64> {
    let trimmed = attr.value.trim();
    match trimmed.parse::<f64>() {
        Ok(x) if x.is_finite() => Some(x),
        _ => {
            report.error(
                Code::Invalid,
                attr.span,
                format!("{} is {trimmed:?}, not a finite number", attr.name),
            );
            None
        }
    }
}
