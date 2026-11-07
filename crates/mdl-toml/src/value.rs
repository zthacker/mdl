//! TOML values as the JSON values of the IR, with each value's span
//! recorded under its JSON Pointer.

use mdl::v2::diagnostics::escape_pointer_token;
use mdl::v2::source::{SourceDiagnostic, SourceFile, SourceMap, Span};
use serde_json::{Number, Value};
use toml::Spanned;
use toml::de::{DeInteger, DeValue};
use toml::value::{Datetime, Offset};

use crate::Code;

/// The largest integer every JSON implementation reads exactly. The IR
/// writes wider ones as decimal strings.
const MAX_SAFE_INTEGER: i128 = (1 << 53) - 1;

/// Converts the values of one file.
pub(crate) struct Converter<'a, 'm> {
    pub(crate) file: SourceFile<'a>,
    pub(crate) map: &'m mut SourceMap,
    pub(crate) errors: &'m mut Vec<SourceDiagnostic>,
}

impl Converter<'_, '_> {
    /// `value` as JSON, its span and the spans of everything in it
    /// recorded under `pointer`. A value JSON can't hold is reported and
    /// comes back as null; the bundle isn't built then.
    pub(crate) fn convert(&mut self, value: &Spanned<DeValue<'_>>, pointer: &mut String) -> Value {
        let range = value.span();
        let span = self.file.span(range.start, range.end);
        self.map.insert(pointer.clone(), span);
        match value.get_ref() {
            DeValue::String(text) => Value::String(text.to_string()),
            DeValue::Integer(integer) => self.integer(integer, span),
            DeValue::Float(float) => self.float(float.as_str(), span),
            DeValue::Boolean(b) => Value::Bool(*b),
            DeValue::Datetime(datetime) => self.instant(datetime, span),
            DeValue::Array(items) => Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| self.nested(item, pointer, &index.to_string()))
                    .collect(),
            ),
            DeValue::Table(table) => Value::Object(
                table
                    .iter()
                    .map(|(key, item)| {
                        let key = key.get_ref().to_string();
                        let item = self.nested(item, pointer, &escape_pointer_token(&key));
                        (key, item)
                    })
                    .collect(),
            ),
        }
    }

    fn nested(&mut self, value: &Spanned<DeValue<'_>>, pointer: &mut String, token: &str) -> Value {
        let parent = pointer.len();
        pointer.push('/');
        pointer.push_str(token);
        let converted = self.convert(value, pointer);
        pointer.truncate(parent);
        converted
    }

    /// TOML's 64-bit integers; beyond ±(2^53 - 1), the decimal string the IR
    /// writes them as.
    fn integer(&mut self, integer: &DeInteger<'_>, span: Span) -> Value {
        let in_toml = i128::from_str_radix(integer.as_str(), integer.radix())
            .ok()
            .and_then(|n| i64::try_from(n).ok());
        match in_toml {
            Some(n) if i128::from(n).abs() <= MAX_SAFE_INTEGER => Value::Number(Number::from(n)),
            Some(n) => Value::String(n.to_string()),
            None => self.error(
                Code::Number,
                format!(
                    "{integer} is outside TOML's 64-bit integers; write a wider value as a \
                     decimal string"
                ),
                span,
            ),
        }
    }

    fn float(&mut self, text: &str, span: Span) -> Value {
        match text.parse::<f64>().ok().and_then(Number::from_f64) {
            Some(number) => Value::Number(number),
            None => self.error(
                Code::Number,
                format!("{text} isn't a finite number, and the IR's numbers are"),
                span,
            ),
        }
    }

    /// A TOML offset date-time in UTC, written in MDL's one spelling of an
    /// instant: no trailing zeros in the fraction, and `Z`.
    fn instant(&mut self, datetime: &Datetime, span: Span) -> Value {
        let (Some(date), Some(time), Some(offset)) =
            (datetime.date, datetime.time, datetime.offset)
        else {
            return self.error(
                Code::Datetime,
                format!(
                    "{datetime} isn't an instant: write a date, a time and Z, like \
                     2000-01-01T00:00:00Z"
                ),
                span,
            );
        };
        let utc = match offset {
            Offset::Z => true,
            Offset::Custom { minutes } => minutes == 0,
        };
        if !utc {
            return self.error(
                Code::Datetime,
                format!("{datetime} isn't in UTC; write instants in UTC, ending in Z"),
                span,
            );
        }
        // TOML allows both; MDL's instants (`bindings.md` §2) don't.
        if time.second == Some(60) {
            return self.error(
                Code::Datetime,
                format!("{datetime} is a leap second, and MDL instants have none"),
                span,
            );
        }
        if date.year == 0 {
            return self.error(
                Code::Datetime,
                format!("{datetime} is in year 0; MDL instants start at year 0001"),
                span,
            );
        }
        let fraction = match time.nanosecond {
            Some(nanos) if nanos > 0 => {
                let digits = format!("{nanos:09}");
                format!(".{}", digits.trim_end_matches('0'))
            }
            _ => String::new(),
        };
        Value::String(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{fraction}Z",
            date.year,
            date.month,
            date.day,
            time.hour,
            time.minute,
            time.second.unwrap_or(0)
        ))
    }

    fn error(&mut self, code: Code, message: String, span: Span) -> Value {
        self.errors
            .push(SourceDiagnostic::error(code.as_str(), message).with_span(span));
        Value::Null
    }
}
