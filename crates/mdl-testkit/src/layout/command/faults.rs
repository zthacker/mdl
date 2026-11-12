//! Faults planted in valid requests and payloads, with the error each one
//! gives. A request is refused at the first problem `spec/encoding.md`
//! reaches: its set and command (§1), then §2.1's two passes (the given
//! names in the order given, then every argument in catalog order), then
//! §3's entries in order (in an array, its records' lists compared, then
//! each element and record field), then length fields, then counts. A
//! payload's decoding fails where its fault is (§7). Its checksums are
//! worked out again after the fault, so the fault is the only thing wrong
//! with it, unless the fault is a checksum's.

mod garbled;
mod refused;

pub(super) use garbled::garbling;
pub(super) use refused::refusal;

use super::{Elements, Layout, Placed, Source};

fn argument_name(index: usize) -> String {
    format!("a{index}")
}

fn entry_name(layout: &Layout, entry: usize) -> Option<String> {
    match layout.entries.get(entry) {
        Some(
            Placed::Field { name, .. } | Placed::Checksum { name, .. } | Placed::Array { name, .. },
        ) => Some(name.clone()),
        _ => None,
    }
}

/// Where a list argument's values are written: its array's entry and name,
/// and its element's place in a record (0 for a scalar element) and field.
#[derive(Debug, Clone)]
struct ListPlace {
    entry: usize,
    array: String,
    position: usize,
    field: Option<String>,
}

impl ListPlace {
    /// The path an error names element `index` by (`decoding.md` §9).
    fn path(&self, index: usize) -> String {
        match &self.field {
            Some(field) => format!("{}[{index}].{field}", self.array),
            None => format!("{}[{index}]", self.array),
        }
    }
}

fn list_place(layout: &Layout, argument: usize) -> Option<ListPlace> {
    layout
        .entries
        .iter()
        .enumerate()
        .find_map(|(entry, placed)| {
            let Placed::Array { name, elements, .. } = placed else {
                return None;
            };
            let place = |position: usize, field: Option<String>| ListPlace {
                entry,
                array: name.clone(),
                position,
                field,
            };
            match elements {
                Elements::Scalar { argument: a, .. } => (*a == argument).then(|| place(0, None)),
                Elements::Records { entries, .. } => {
                    entries
                        .iter()
                        .enumerate()
                        .find_map(|(position, field)| match field {
                            Placed::Field {
                                name,
                                source: Source::Argument(a),
                                ..
                            } if *a == argument => Some(place(position, Some(name.clone()))),
                            _ => None,
                        })
                }
            }
        })
}
