//! Family checks: names, byte order, and locators that every set of the
//! family can read before the set is known.

use std::collections::{BTreeMap, BTreeSet};

use super::rules::{check_not_empty, check_pointer, is_token};
use crate::diagnostics::{Code, Place, Report};
use crate::ir::{ByteOrder, Encoding, Family, Locator};

/// Check every family; return them by name (the first of any duplicates).
pub(super) fn check_all<'a>(
    families: &'a [Family],
    root: &Place,
    report: &mut Report,
) -> BTreeMap<&'a str, &'a Family> {
    let mut by_name: BTreeMap<&str, (usize, &Family)> = BTreeMap::new();
    for (index, family) in families.iter().enumerate() {
        let place = root.item("families", index, "family", &family.name);
        check(family, &place, report);
        if let Some((first, _)) = by_name.get(family.name.as_str()) {
            report.duplicate(
                Code::DuplicateName,
                &place,
                &root.at("families", *first),
                format!("family {} is defined twice", family.name),
            );
        } else {
            by_name.insert(&family.name, (index, family));
        }
    }
    by_name
        .into_iter()
        .map(|(name, (_, family))| (name, family))
        .collect()
}

fn check(family: &Family, place: &Place, report: &mut Report) {
    if !is_token(&family.name) {
        report.error(
            Code::InvalidName,
            &place.key("name"),
            "family names are lowercase letters, digits and '_', starting with a letter",
        );
    }
    let binary = family.encoding == Encoding::Binary;
    match (binary, family.byte_order) {
        (true, None) => report.error(
            Code::ByteOrder,
            &place.key("byte_order"),
            "binary families say their byte_order: big or little",
        ),
        (false, Some(_)) => report.error(
            Code::ByteOrder,
            &place.key("byte_order"),
            "only binary families have a byte_order",
        ),
        _ => {}
    }
    let mut declared = BTreeSet::new();
    for (index, name) in family.metadata.iter().enumerate() {
        let name_place = place.at("metadata", index);
        if !is_token(name) {
            report.error(
                Code::InvalidName,
                &name_place,
                format!(
                    "metadata name {name:?} must be lowercase letters, digits and '_', starting \
                     with a letter"
                ),
            );
        }
        if !declared.insert(name.as_str()) {
            report.error(
                Code::DuplicateValue,
                &name_place,
                format!("metadata name {name} is listed twice"),
            );
        } else if index > 0 && family.metadata[index - 1] > *name {
            // The list means a set: one order keeps one family one digest.
            report.error(
                Code::NotSorted,
                &name_place,
                format!(
                    "metadata lists {name} after {}; list metadata names in sorted order",
                    family.metadata[index - 1]
                ),
            );
        }
    }
    check_locator(family, &family.alias, &place.key("alias"), report);
    if let Some(version) = &family.version {
        let version_place = place.key("version");
        if matches!(version, Locator::Fixed { .. }) {
            report.error(
                Code::FixedVersion,
                &version_place,
                "a fixed version locator would pick the same set for every message; remove it \
                 and give the family one set",
            );
        } else {
            check_locator(family, version, &version_place, report);
        }
    }
}

fn check_locator(family: &Family, locator: &Locator, place: &Place, report: &mut Report) {
    let encoding = family.encoding;
    match locator {
        Locator::Bits {
            offset_bits,
            size_bits,
        } => {
            if encoding != Encoding::Binary {
                report.error(
                    Code::WrongEncoding,
                    place,
                    "bit locators only apply to binary families; use a path",
                );
            }
            if !(1..=64).contains(size_bits) {
                report.error(
                    Code::BitSize,
                    place,
                    format!("a locator reads 1 to 64 bits, not {size_bits}"),
                );
            } else if family.byte_order == Some(ByteOrder::Little) {
                // Locators read in the family's order, under the same
                // rules as a little-endian field.
                let start = offset_bits % 8;
                let inside_one_byte = start + size_bits <= 8;
                let whole_bytes = start == 0 && size_bits % 8 == 0;
                if !inside_one_byte && !whole_bytes {
                    report.error(
                        Code::Alignment,
                        place,
                        format!(
                            "a locator reads in its family's byte order, and a little-endian \
                             value is whole bytes on byte boundaries or fits inside one byte; \
                             {size_bits} bits at bit {offset_bits} is neither"
                        ),
                    );
                }
            }
        }
        Locator::Path { pointer } => {
            if encoding == Encoding::Binary {
                report.error(
                    Code::WrongEncoding,
                    place,
                    "path locators only apply to self-describing families; use bits or metadata",
                );
            }
            check_pointer(pointer, place, report);
        }
        Locator::Metadata { name } => {
            if !family.metadata.contains(name) {
                report.error(
                    Code::UnknownMetadata,
                    place,
                    format!(
                        "family {} doesn't declare metadata {name}; list it in the family's \
                         metadata",
                        family.name
                    ),
                );
            }
        }
        Locator::Fixed { value } => check_not_empty(value, "a fixed value", place, report),
    }
}
