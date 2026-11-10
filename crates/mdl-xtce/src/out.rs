//! The bundle as JSON, built with the span each value came from, so the
//! reader's and validation's diagnostics point back into the XTCE.

use mdl::diagnostics::escape_pointer_token;
use mdl::source::{SourceMap, Span};
use serde_json::{Map, Value, json};

/// A JSON object and the spans of its values, by pointer relative to it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Object {
    fields: Map<String, Value>,
    spans: Vec<(String, Span)>,
}

impl Object {
    /// An object whose own span is `span`.
    pub(crate) fn at(span: Span) -> Self {
        Object {
            fields: Map::new(),
            spans: vec![(String::new(), span)],
        }
    }

    pub(crate) fn put(&mut self, key: &str, value: impl Into<Value>, span: Option<Span>) {
        if let Some(span) = span {
            self.spans.push((token(key), span));
        }
        self.fields.insert(key.to_string(), value.into());
    }

    pub(crate) fn put_object(&mut self, key: &str, object: Object) {
        let prefix = token(key);
        self.adopt(&prefix, object.spans);
        self.fields
            .insert(key.to_string(), Value::Object(object.fields));
    }

    pub(crate) fn put_list(&mut self, key: &str, list: List) {
        let prefix = token(key);
        self.adopt(&prefix, list.spans);
        self.fields
            .insert(key.to_string(), Value::Array(list.items));
    }

    fn adopt(&mut self, prefix: &str, spans: Vec<(String, Span)>) {
        self.spans.extend(
            spans
                .into_iter()
                .map(|(pointer, span)| (format!("{prefix}{pointer}"), span)),
        );
    }

    /// The JSON value and the source map of everything in it.
    pub(crate) fn finish(self) -> (Value, SourceMap) {
        let mut map = SourceMap::new();
        for (pointer, span) in self.spans {
            map.insert(pointer, span);
        }
        (Value::Object(self.fields), map)
    }
}

/// A JSON array and the spans of its items.
#[derive(Debug, Clone, Default)]
pub(crate) struct List {
    items: Vec<Value>,
    spans: Vec<(String, Span)>,
}

impl List {
    pub(crate) fn push(&mut self, value: impl Into<Value>, span: Option<Span>) {
        if let Some(span) = span {
            self.spans.push((format!("/{}", self.items.len()), span));
        }
        self.items.push(value.into());
    }

    pub(crate) fn push_object(&mut self, object: Object) {
        let prefix = format!("/{}", self.items.len());
        self.spans.extend(
            object
                .spans
                .into_iter()
                .map(|(pointer, span)| (format!("{prefix}{pointer}"), span)),
        );
        self.items.push(Value::Object(object.fields));
    }
}

fn token(key: &str) -> String {
    format!("/{}", escape_pointer_token(key))
}

/// An integer as the IR writes it: a JSON number within ±(2^53 − 1), where
/// every reader holds it exactly, else a decimal string.
pub(crate) fn integer_json(value: i128) -> Value {
    const SAFE: u128 = (1 << 53) - 1;
    if value.unsigned_abs() <= SAFE {
        match i64::try_from(value) {
            Ok(n) => json!(n),
            Err(_) => Value::String(value.to_string()),
        }
    } else {
        Value::String(value.to_string())
    }
}
