//! Messages: what a decoder is given.

use crate::ir::Encoding;
use crate::time::Timestamp;

/// One message as the link delivered it: the payload, the metadata the
/// link's protocol stack provides, and when it arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Message<'a> {
    pub payload: Payload<'a>,
    /// Values by name (`apid`, `sysid`). Each name appears at most once.
    pub metadata: &'a [(&'a str, MetaValue<'a>)],
    /// When the ground received the message. Bindings are resolved at this
    /// time, so decoding a message again later gives the same asset.
    pub ground_time: Timestamp,
}

impl<'a> Message<'a> {
    pub fn new(payload: Payload<'a>, ground_time: Timestamp) -> Self {
        Message {
            payload,
            metadata: &[],
            ground_time,
        }
    }

    pub fn with_metadata(mut self, metadata: &'a [(&'a str, MetaValue<'a>)]) -> Self {
        self.metadata = metadata;
        self
    }
}

/// A payload, in its family's encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Payload<'a> {
    Binary(&'a [u8]),
    /// One JSON document, as bytes. MDL parses it itself and refuses
    /// duplicate keys, so every implementation reads the same values.
    Json(&'a [u8]),
}

impl Payload<'_> {
    /// The payload's encoding.
    pub fn encoding(&self) -> Encoding {
        match self {
            Payload::Binary(_) => Encoding::Binary,
            Payload::Json(_) => Encoding::Json,
        }
    }
}

/// A metadata value: an integer or text. Integers compare by value with
/// set versions and identify values, and text in an integer's one decimal
/// spelling is that integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MetaValue<'a> {
    Uint(u64),
    Int(i64),
    Text(&'a str),
}

impl From<u64> for MetaValue<'_> {
    fn from(value: u64) -> Self {
        MetaValue::Uint(value)
    }
}

impl From<i64> for MetaValue<'_> {
    fn from(value: i64) -> Self {
        MetaValue::Int(value)
    }
}

impl From<u32> for MetaValue<'_> {
    fn from(value: u32) -> Self {
        MetaValue::Uint(u64::from(value))
    }
}

/// So `MetaValue::from(2)` works: an unsuffixed literal is an `i32`.
impl From<i32> for MetaValue<'_> {
    fn from(value: i32) -> Self {
        MetaValue::Int(i64::from(value))
    }
}

impl<'a> From<&'a str> for MetaValue<'a> {
    fn from(value: &'a str) -> Self {
        MetaValue::Text(value)
    }
}
