//! The importer's diagnostics, reported in file order and then by
//! position. A definition shared by many packets (a header container, a
//! type) is read for each, so a problem with it is kept once.

use std::collections::{BTreeMap, BTreeSet};

use mdl::source::{Severity, SourceDiagnostic, SourceFile, SourceId, Span};

use crate::Code;

pub(crate) struct Report {
    /// Each file's place in the order the files were given.
    order: BTreeMap<SourceId, usize>,
    diagnostics: Vec<SourceDiagnostic>,
    /// Each diagnostic's code, span and message, to keep each once.
    reported: BTreeSet<(&'static str, Option<Span>, String)>,
}

impl Report {
    pub(crate) fn new(files: &[SourceFile<'_>]) -> Self {
        Report {
            order: files
                .iter()
                .enumerate()
                .map(|(index, file)| (file.id(), index))
                .collect(),
            diagnostics: Vec::new(),
            reported: BTreeSet::new(),
        }
    }

    pub(crate) fn error(&mut self, code: Code, span: Span, message: impl Into<String>) {
        self.push(SourceDiagnostic::error(code.as_str(), message).with_span(span));
    }

    pub(crate) fn warning(&mut self, code: Code, span: Span, message: impl Into<String>) {
        self.push(SourceDiagnostic::warning(code.as_str(), message).with_span(span));
    }

    /// An error at the second of two definitions, naming the first.
    pub(crate) fn duplicate(
        &mut self,
        code: Code,
        span: Span,
        first: Span,
        message: impl Into<String>,
    ) {
        self.push(
            SourceDiagnostic::error(code.as_str(), message)
                .with_span(span)
                .with_first(first),
        );
    }

    pub(crate) fn push(&mut self, diagnostic: SourceDiagnostic) {
        let key = (diagnostic.code, diagnostic.span, diagnostic.message.clone());
        if self.reported.insert(key) {
            self.diagnostics.push(diagnostic);
        }
    }

    pub(crate) fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Everything reported, in order.
    pub(crate) fn finish(mut self) -> Vec<SourceDiagnostic> {
        let order = &self.order;
        self.diagnostics.sort_by_key(|d| {
            d.span.map_or((usize::MAX, usize::MAX), |span| {
                (
                    order.get(&span.source).copied().unwrap_or(usize::MAX),
                    span.start,
                )
            })
        });
        self.diagnostics
    }
}
