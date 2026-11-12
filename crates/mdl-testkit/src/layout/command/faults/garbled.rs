//! Faults planted in valid payloads, and the code decoding each gives
//! (`spec/encoding.md` §7, `decoding.md` §5).

use proptest::prelude::*;
use proptest::sample::Index;

use super::{argument_name, entry_name, list_place};
use crate::layout::command::requests::{
    ElementPlace, Packed, buffer, pack, parts, values, write_checksums,
};
use crate::layout::command::values::{list_value, unrounded};
use crate::layout::command::{
    Arg, ArgType, Const, Counted, Elements, Encoder, Encoding, Garbled, GarbledKind, Group, Layout,
    Len, Placed, Shape, Source, Target, Through,
};
use crate::layout::{fit, put_bits};

/// A valid payload for the layout's command with one fault planted in it,
/// and the code decoding it gives. `keys` are every packet's key values.
pub(in super::super) fn garbling(layout: Layout, keys: Vec<Const>) -> BoxedStrategy<Garbled> {
    (
        Just(layout.clone()),
        parts(&layout),
        any::<Index>(),
        any::<Index>(),
        beyond_bounds(&layout),
        any::<Index>(),
    )
        .prop_map(move |(layout, parts, which, detail, beyond, kind)| {
            let values = values(&layout, &parts.given);
            let target = parts.target.as_ref();
            let packed = pack(&layout, &values, target);
            let mut faults = garbled(&layout, &keys, &packed, &detail);
            faults.extend(beyond_payloads(&layout, &values, target, &beyond));
            // A kind of fault first, by its weight, then one of that kind.
            let mut kinds: Vec<GarbledKind> = faults.iter().map(|fault| fault.kind).collect();
            kinds.sort_unstable();
            kinds.dedup();
            let weighted: Vec<GarbledKind> = kinds
                .iter()
                .flat_map(|kind| std::iter::repeat_n(*kind, kind.weight()))
                .collect();
            let kind = weighted.get(kind.index(weighted.len().max(1))).copied();
            let faults: Vec<Garbled> = faults
                .into_iter()
                .filter(|fault| Some(fault.kind) == kind)
                .collect();
            let pick = which.index(faults.len().max(1));
            faults.into_iter().nth(pick).unwrap_or_else(|| Garbled {
                set: "fam_v9".into(),
                payload: packed.payload.clone(),
                encoding: Encoding::Binary,
                metadata: Vec::new(),
                code: "unknown_set",
                at: None,
                kind: GarbledKind::Set,
                planted: "an unknown set".into(),
            })
        })
        .boxed()
}

/// Every fault the payload can be given, each with its kind, the code it
/// gives and the argument or entry that error names.
fn garbled(layout: &Layout, keys: &[Const], packed: &Packed, detail: &Index) -> Vec<Garbled> {
    let metadata = metadata(layout);
    let fault = |kind: GarbledKind,
                 payload: Vec<u8>,
                 code: &'static str,
                 at: Option<String>,
                 planted: String| Garbled {
        set: "fam_v1".into(),
        payload,
        encoding: Encoding::Binary,
        metadata: metadata.clone(),
        code,
        at,
        kind,
        planted,
    };
    // The payload with raw bits written at an entry, its checksums worked
    // out again.
    let rewritten = |entry: usize, bits_value: u64| -> Option<Vec<u8>> {
        let (start, size) = packed.places.get(entry).copied().flatten()?;
        let Placed::Field { little, .. } = layout.entries.get(entry)? else {
            return None;
        };
        let mut payload = packed.payload.clone();
        put_bits(&mut payload, start, size as u32, bits_value, *little);
        write_checksums(layout, &mut payload, &packed.places);
        Some(payload)
    };
    let mut faults = vec![
        Garbled {
            set: "fam_v9".into(),
            ..fault(
                GarbledKind::Set,
                packed.payload.clone(),
                "unknown_set",
                None,
                "an unknown set".into(),
            )
        },
        Garbled {
            encoding: Encoding::Json,
            ..fault(
                GarbledKind::Encoding,
                b"{}".to_vec(),
                "wrong_encoding",
                None,
                "a JSON payload".into(),
            )
        },
        fault(
            GarbledKind::NoPacket,
            Vec::new(),
            "no_packet",
            None,
            "an empty payload".into(),
        ),
    ];
    if !metadata.is_empty() {
        faults.push(Garbled {
            metadata: Vec::new(),
            ..fault(
                GarbledKind::NoPacket,
                packed.payload.clone(),
                "no_packet",
                None,
                "no metadata".into(),
            )
        });
    }
    if let Some(payload) = unknown_key(layout, keys, packed) {
        faults.push(fault(
            GarbledKind::NoPacket,
            payload,
            "no_packet",
            None,
            "a key no packet has".into(),
        ));
    }
    // A stored checksum changed (§5.6): no algorithm accepts its value
    // with one bit flipped. It's the checksum that fails first: one before
    // it covers only bytes before that one, and a later one is static only
    // when it is too, and static ones are verified in order of position.
    let sums: Vec<usize> = (0..layout.entries.len())
        .filter(|entry| matches!(layout.entries[*entry], Placed::Checksum { .. }))
        .collect();
    if !sums.is_empty() {
        let entry = sums[detail.index(sums.len())];
        if let (Some(Placed::Checksum { algorithm, .. }), Some((start, _))) =
            (layout.entries.get(entry), packed.places[entry])
        {
            let stored_at = (start / 8) as usize;
            let width = (algorithm.bits() / 8) as usize;
            let mut payload = packed.payload.clone();
            payload[stored_at + width - 1] ^= 1;
            let named = entry_name(layout, entry);
            let planted = format!("checksum {entry}");
            faults.push(fault(
                GarbledKind::Checksum,
                payload,
                "bad_checksum",
                named,
                planted,
            ));
        }
    }
    // A constant that isn't a key, holding something else (§7.3).
    let loose: Vec<usize> = (0..layout.entries.len())
        .filter(|entry| {
            !layout.statics[*entry]
                && matches!(
                    layout.entries[*entry],
                    Placed::Field {
                        source: Source::Constant(_),
                        ..
                    }
                )
        })
        .collect();
    if !loose.is_empty() {
        let entry = loose[detail.index(loose.len())];
        if let Some(payload) = other_constant(layout, packed, entry) {
            let named = entry_name(layout, entry);
            let planted = format!("constant {entry}");
            faults.push(fault(
                GarbledKind::Constant,
                payload,
                "wrong_constant",
                named,
                planted,
            ));
        }
    }
    // Arguments' raw values decoding refuses (§7.4).
    for (entry, placed) in layout.entries.iter().enumerate() {
        let Placed::Field {
            shape,
            source: Source::Argument(index),
            ..
        } = placed
        else {
            continue;
        };
        let encoder = &layout.arguments[*index].0;
        for (raw, code) in refused_raws(encoder, shape) {
            if let Some(payload) = rewritten(entry, raw) {
                let named = Some(argument_name(*index));
                let planted = format!("raw {raw:#x} at {entry}");
                faults.push(fault(GarbledKind::Raw, payload, code, named, planted));
            }
        }
    }
    // Cut short inside an entry after the keys (§7.3): every constant at a
    // place each payload has is a key (§7.2), which a cut would leave
    // unread, so that no packet matched. The cut is at the entry's start or
    // past it, so no entry before it is cut short.
    let key_bytes = (0..layout.entries.len())
        .filter(|entry| {
            layout.statics[*entry]
                && matches!(
                    layout.entries[*entry],
                    Placed::Field {
                        source: Source::Constant(_),
                        ..
                    }
                )
        })
        .filter_map(|entry| packed.places[entry])
        .map(|(start, size)| (start + size).div_ceil(8))
        .max()
        .unwrap_or(1);
    let cuts: Vec<(u64, u64)> = (1..layout.entries.len())
        .filter_map(|entry| {
            let fixed = match &layout.entries[entry] {
                Placed::Field { shape, .. } => shape.bits().is_some(),
                Placed::Checksum { .. } => true,
                Placed::Padding(_) | Placed::Array { .. } => false,
            };
            let (start, size) = packed.places[entry]?;
            let low = key_bytes.max(start.div_ceil(8));
            let high = (start + size).div_ceil(8).checked_sub(1)?;
            (fixed && size > 0 && low <= high).then_some((low, high))
        })
        .collect();
    if !cuts.is_empty() {
        let (low, high) = cuts[detail.index(cuts.len())];
        let length = (low + (high - low) / 2) as usize;
        let named = cut_entry(layout, packed, 8 * length as u64);
        faults.push(fault(
            GarbledKind::Cut,
            packed.payload[..length].to_vec(),
            "past_end",
            named,
            format!("cut to {length} bytes"),
        ));
    }
    for (kind, payload, code, at, planted) in list_faults_in(layout, packed, detail, key_bytes) {
        faults.push(fault(kind, payload, code, Some(at), planted));
    }
    faults
}

/// The metadata the link frames the layout's payloads with.
fn metadata(layout: &Layout) -> Vec<(String, i64)> {
    layout
        .apid
        .map(|apid| vec![("apid".to_string(), apid)])
        .unwrap_or_default()
}

/// The payload with raw bits written at a place, its checksums worked out
/// again.
fn written(layout: &Layout, packed: &Packed, place: (u64, u64), raw: u64, little: bool) -> Vec<u8> {
    let (start, size) = place;
    let mut payload = packed.payload.clone();
    put_bits(&mut payload, start, size as u32, raw, little);
    write_checksums(layout, &mut payload, &packed.places);
    payload
}

/// The bits each element of an array takes, when every element takes as
/// many.
fn element_bits(elements: &Elements) -> Option<u64> {
    match elements {
        Elements::Scalar { shape, .. } => shape.bits().map(u64::from),
        Elements::Records { entries, .. } => entries
            .iter()
            .map(|entry| match entry {
                Placed::Padding(bits) => Some(*bits),
                Placed::Field { shape, .. } => shape.bits().map(u64::from),
                _ => None,
            })
            .sum(),
    }
}

/// The code a value of this shape fails with when the packet ends inside
/// it: a terminated value finds no terminator, any other runs past the end.
fn ended_inside(shape: &Shape) -> &'static str {
    match shape.len() {
        Some(Len::Terminator(_)) => "no_terminator",
        _ => "past_end",
    }
}

/// The first element, or record field, of an array that runs past bit
/// `end`, and the code it fails with, named by its path; the elements
/// placed are those from index `first`. Padding ends on a byte, as `end`
/// does, so it's never the first.
fn first_past(
    name: &str,
    elements: &Elements,
    places: &[ElementPlace],
    first: usize,
    end: u64,
) -> Option<(&'static str, String)> {
    places
        .iter()
        .zip(first..)
        .find_map(|(element, index)| match elements {
            Elements::Scalar { shape, .. } => (element.start + element.size > end)
                .then(|| (ended_inside(shape), format!("{name}[{index}]"))),
            Elements::Records { entries, .. } => {
                entries
                    .iter()
                    .zip(&element.fields)
                    .find_map(|(entry, (from, bits))| match entry {
                        Placed::Field {
                            name: field, shape, ..
                        } if from + bits > end => {
                            Some((ended_inside(shape), format!("{name}[{index}].{field}")))
                        }
                        _ => None,
                    })
            }
        })
}

/// Faults in a payload's arrays (`spec/encoding.md` §7, `decoding.md`
/// §5.2, §5.4, §5.5), each with its kind, the payload, its code, what the
/// error names, and what was planted.
fn list_faults_in(
    layout: &Layout,
    packed: &Packed,
    detail: &Index,
    key_bytes: u64,
) -> Vec<ListFault> {
    let mut faults = Vec::new();
    let mut counted: Vec<&str> = Vec::new();
    for (entry, placed) in layout.entries.iter().enumerate() {
        let Placed::Array {
            name,
            count,
            elements,
        } = placed
        else {
            continue;
        };
        let array = ArrayAt {
            entry,
            name,
            count,
            elements,
            places: &packed.elements[entry],
        };
        faults.extend(element_faults_in(layout, packed, &array, detail));
        // A count field's faults show at the first array it counts.
        if let Counted::Field { name: field, .. } = count
            && !counted.contains(&field.as_str())
        {
            counted.push(field);
            faults.extend(count_faults_in(layout, packed, &array));
        }
        faults.extend(cut_inside(packed, &array, key_bytes));
        faults.extend(one_more_element(packed, &array, detail));
    }
    faults
}

/// An array of a payload, as its faults need it.
struct ArrayAt<'a> {
    entry: usize,
    name: &'a str,
    count: &'a Counted,
    elements: &'a Elements,
    /// Its elements' places.
    places: &'a [ElementPlace],
}

/// In one element of the array: each field that takes a list holding a
/// raw value decoding refuses, named by its list and index; and each of a
/// record's constants holding another value, named by its path.
fn element_faults_in(
    layout: &Layout,
    packed: &Packed,
    array: &ArrayAt<'_>,
    detail: &Index,
) -> Vec<ListFault> {
    let mut faults = Vec::new();
    if array.places.is_empty() {
        return faults;
    }
    let (name, index) = (array.name, detail.index(array.places.len()));
    let element = &array.places[index];
    let fields: Vec<(usize, &Shape, bool, (u64, u64))> = match array.elements {
        Elements::Scalar {
            argument,
            shape,
            little,
            ..
        } => vec![(*argument, shape, *little, (element.start, element.size))],
        Elements::Records { entries, .. } => entries
            .iter()
            .zip(&element.fields)
            .filter_map(|(field, place)| match field {
                Placed::Field {
                    shape,
                    little,
                    source: Source::Argument(argument),
                    ..
                } => Some((*argument, shape, *little, *place)),
                _ => None,
            })
            .collect(),
    };
    for (argument, shape, little, place) in fields {
        for (raw, code) in refused_raws(&layout.arguments[argument].0, shape) {
            faults.push((
                GarbledKind::Element,
                written(layout, packed, place, raw, little),
                code,
                format!("{}[{index}]", argument_name(argument)),
                format!("raw {raw:#x} in {name}[{index}]"),
            ));
        }
    }
    if let Elements::Records { entries, .. } = array.elements {
        for (field, place) in entries.iter().zip(&element.fields) {
            let Placed::Field {
                name: field,
                shape,
                little,
                source: Source::Constant(value),
                ..
            } = field
            else {
                continue;
            };
            if let Some(payload) = another_constant(layout, packed, *place, shape, *little, value) {
                faults.push((
                    GarbledKind::RecordConstant,
                    payload,
                    "wrong_constant",
                    format!("{name}[{index}].{field}"),
                    format!("constant {name}[{index}].{field}"),
                ));
            }
        }
    }
    faults
}

/// The count field of the array, the first it counts, holding a count
/// below 0 (`bad_length`, §5.4), or its largest when a count of elements
/// of one size then runs past the end, which is checked before any
/// element is read (`past_end`, §5.5); both named by the array.
fn count_faults_in(layout: &Layout, packed: &Packed, array: &ArrayAt<'_>) -> Vec<ListFault> {
    let mut faults = Vec::new();
    let Counted::Field {
        name: field,
        multiply,
        add,
    } = array.count
    else {
        return faults;
    };
    let count_field = layout
        .entries
        .iter()
        .position(|entry| matches!(entry, Placed::Field { name, .. } if name == field));
    let Some(at) = count_field else {
        return faults;
    };
    let (
        Some(place),
        Some((start, _)),
        Placed::Field {
            shape: Shape::Uint(bits),
            little,
            ..
        },
    ) = (
        packed.places[at],
        packed.places[array.entry],
        &layout.entries[at],
    )
    else {
        return faults;
    };
    let name = array.name;
    let step = i64::from(*multiply);
    if let Some(raw) = (0..=1_i64).find(|raw| raw * step + add < 0) {
        faults.push((
            GarbledKind::Count,
            written(layout, packed, place, raw as u64, *little),
            "bad_length",
            name.to_string(),
            format!("count {raw} for {name}"),
        ));
    }
    let most = (1_u64 << bits) - 1;
    let counted = most as i64 * step + add;
    let left = 8 * packed.payload.len() as u64 - start;
    if let Some(each) = element_bits(array.elements)
        && counted as u64 * each > left
    {
        faults.push((
            GarbledKind::Count,
            written(layout, packed, place, most, *little),
            "past_end",
            name.to_string(),
            format!("count {most} for {name}"),
        ));
    }
    faults
}

/// The payload cut inside the array, when its count is known. A count is
/// checked first against the bits left (§5.5): elements of one size as a
/// whole, so the array is past the end; others a bit each, then the first
/// element or record field the cut leaves short fails.
fn cut_inside(packed: &Packed, array: &ArrayAt<'_>, key_bytes: u64) -> Option<ListFault> {
    if matches!(array.count, Counted::Remaining) {
        return None;
    }
    let (start, size) = packed.places[array.entry].filter(|(_, size)| *size > 0)?;
    let low = key_bytes.max(start.div_ceil(8));
    let high = (start + size).div_ceil(8).checked_sub(1)?;
    if low > high {
        return None;
    }
    let length = low + (high - low) / 2;
    let end = 8 * length;
    let name = array.name;
    let (code, at) = match element_bits(array.elements) {
        Some(_) => ("past_end", name.to_string()),
        None if array.places.len() as u64 > end - start => ("past_end", name.to_string()),
        None => first_past(name, array.elements, array.places, 0, end)?,
    };
    Some((
        GarbledKind::CutInsideArray,
        packed.payload[..length as usize].to_vec(),
        code,
        at,
        format!("cut to {length} bytes, inside {name}"),
    ))
}

/// Bytes after an array that runs to the end, which is on a byte, making
/// part of one more element, which runs past it: zeros for elements of one
/// size, letters that aren't the terminator for a terminated one.
fn one_more_element(packed: &Packed, array: &ArrayAt<'_>, detail: &Index) -> Option<ListFault> {
    if !matches!(array.count, Counted::Remaining) {
        return None;
    }
    let (name, index) = (array.name, array.places.len());
    let (bytes, code, at) = match array.elements {
        Elements::Scalar { shape, .. } => match (shape.bits(), shape.len()) {
            (Some(bits), _) if bits >= 16 => {
                let bytes = 1 + detail.index(bits as usize / 8 - 1);
                (vec![0; bytes], "past_end", format!("{name}[{index}]"))
            }
            (None, Some(Len::Terminator(byte))) => {
                let letter = if *byte == b'q' { b'r' } else { b'q' };
                let bytes = vec![letter; 1 + detail.index(3)];
                (bytes, "no_terminator", format!("{name}[{index}]"))
            }
            _ => return None,
        },
        Elements::Records { .. } => {
            let bits = element_bits(array.elements).filter(|bits| *bits >= 16)?;
            let bytes = 1 + detail.index(bits as usize / 8 - 1);
            let part = ElementPlace {
                start: 0,
                size: bits,
                fields: record_places(array.elements),
            };
            let (code, at) = first_past(name, array.elements, &[part], index, 8 * bytes as u64)?;
            (vec![0; bytes], code, at)
        }
    };
    let mut payload = packed.payload.clone();
    payload.extend(&bytes);
    Some((
        GarbledKind::MoreBytes,
        payload,
        code,
        at,
        format!("{} more bytes in {name}", bytes.len()),
    ))
}

/// A fault in a payload's arrays: its kind, the payload, the code, what
/// the error names, and what was planted.
type ListFault = (GarbledKind, Vec<u8>, &'static str, String, String);

/// Each entry's place in a record of exact size, from its start.
fn record_places(elements: &Elements) -> Vec<(u64, u64)> {
    let Elements::Records { entries, .. } = elements else {
        return Vec::new();
    };
    let mut at = 0;
    entries
        .iter()
        .map(|entry| {
            let bits = match entry {
                Placed::Padding(bits) => *bits,
                Placed::Field { shape, .. } => shape.bits().map_or(0, u64::from),
                _ => 0,
            };
            let place = (at, bits);
            at += bits;
            place
        })
        .collect()
}

/// For each group whose lists a payload can give a length their bounds
/// don't allow, lists of such lengths: through a count its count field
/// holds past the group's, or for an array that runs to the end, one past
/// `max` (or one short of `min`).
fn beyond_bounds(layout: &Layout) -> BoxedStrategy<Vec<Vec<(usize, Arg)>>> {
    let groups: Vec<BoxedStrategy<Vec<(usize, Arg)>>> = layout
        .groups
        .iter()
        .map(|group| match beyond_count(layout, group) {
            Some(count) => {
                let lists: Vec<BoxedStrategy<(usize, Arg)>> = group
                    .members
                    .iter()
                    .map(|(member, scale)| {
                        let member = *member;
                        list_value(&layout.arguments[member].0, scale.length(count))
                            .prop_map(move |list| (member, list))
                            .boxed()
                    })
                    .collect();
                lists.boxed()
            }
            None => Just(Vec::new()).boxed(),
        })
        .collect();
    groups.boxed()
}

/// A count past what a group's bounds allow that a payload can hold.
fn beyond_count(layout: &Layout, group: &Group) -> Option<i64> {
    let (first, _) = group.members.first()?;
    let place = list_place(layout, *first)?;
    let Placed::Array { count, .. } = layout.entries.get(place.entry)? else {
        return None;
    };
    match count {
        Counted::Fixed(_) => None,
        Counted::Field { name, .. } => {
            let bits = layout.entries.iter().find_map(|entry| match entry {
                Placed::Field {
                    name: field,
                    shape: Shape::Uint(bits),
                    ..
                } if field == name => Some(*bits),
                _ => None,
            })?;
            let next = group.counts.iter().max()? + 1;
            (next < 1_i64 << bits).then_some(next)
        }
        Counted::Remaining => {
            let bounds = layout.arguments[*first].0.list?;
            match (bounds.max, bounds.min) {
                (Some(max), _) => Some(i64::from(max) + 1),
                (None, Some(min)) if min >= 1 => Some(i64::from(min) - 1),
                _ => None,
            }
        }
    }
}

/// Payloads whose lists are as long as `beyond` makes them: decoding reads
/// them whole, then refuses the first list, in catalog order, whose length
/// its bounds don't allow (§7.4).
fn beyond_payloads(
    layout: &Layout,
    values: &[Option<Arg>],
    target: Option<&Target>,
    beyond: &[Vec<(usize, Arg)>],
) -> Vec<Garbled> {
    beyond
        .iter()
        .filter_map(|lists| {
            let refused = lists
                .iter()
                .filter(|(member, list)| {
                    let bounds = layout.arguments[*member].0.list;
                    let length = match list {
                        Arg::List(values) => values.len(),
                        _ => 0,
                    };
                    bounds.is_some_and(|bounds| {
                        bounds.min.is_some_and(|min| length < min as usize)
                            || bounds.max.is_some_and(|max| length > max as usize)
                    })
                })
                .map(|(member, _)| *member)
                .min()?;
            let mut values = values.to_vec();
            for (member, list) in lists {
                values[*member] = Some(list.clone());
            }
            let packed = pack(layout, &values, target);
            Some(Garbled {
                set: "fam_v1".into(),
                payload: packed.payload,
                encoding: Encoding::Binary,
                metadata: metadata(layout),
                code: "list_length",
                at: Some(argument_name(refused)),
                kind: GarbledKind::ListLength,
                planted: format!("lists past their bounds, from {}", argument_name(refused)),
            })
        })
        .collect()
}

/// The entry a payload cut to `bits` fails at (§5.6): the first, in
/// order, that reaches past the cut. A static checksum past it isn't a
/// problem when static checksums are verified; the walk finds it in order.
fn cut_entry(layout: &Layout, packed: &Packed, bits: u64) -> Option<String> {
    let past = |entry: usize| packed.places[entry].is_some_and(|(start, size)| start + size > bits);
    (0..layout.entries.len())
        .find(|&entry| past(entry))
        .and_then(|entry| entry_name(layout, entry))
}

/// The payload with its key holding a value no packet has.
fn unknown_key(layout: &Layout, keys: &[Const], packed: &Packed) -> Option<Vec<u8>> {
    let (start, size) = packed.places.first().copied().flatten()?;
    let Placed::Field { shape, little, .. } = layout.entries.first()? else {
        return None;
    };
    let mut payload = packed.payload.clone();
    match shape {
        Shape::Uint(bits) | Shape::Int(bits) => {
            let unused = shape
                .integers()?
                .find(|n| !keys.contains(&Const::Int(*n)))?;
            put_bits(
                &mut payload,
                start,
                size as u32,
                fit(unused as u64, *bits),
                *little,
            );
        }
        Shape::Text(_, len) => {
            // Tags are letters a to j, so z's are no tag.
            let length = keys.iter().find_map(|key| match key {
                Const::Text(text) => Some(text.len()),
                _ => None,
            })?;
            let bytes = buffer(vec![b'z'; length], len);
            let first = (start / 8) as usize;
            payload[first..first + bytes.len()].copy_from_slice(&bytes);
        }
        _ => return None,
    }
    Some(payload)
}

/// The payload with a constant holding another value, its checksums
/// worked out again.
fn other_constant(layout: &Layout, packed: &Packed, entry: usize) -> Option<Vec<u8>> {
    let place = packed.places.get(entry).copied().flatten()?;
    let Placed::Field {
        shape,
        little,
        source: Source::Constant(value),
        ..
    } = layout.entries.get(entry)?
    else {
        return None;
    };
    another_constant(layout, packed, place, shape, *little, value)
}

/// The payload with the constant at `place` holding another value, its
/// checksums worked out again.
fn another_constant(
    layout: &Layout,
    packed: &Packed,
    place: (u64, u64),
    shape: &Shape,
    little: bool,
    value: &Const,
) -> Option<Vec<u8>> {
    let (start, size) = place;
    let mut payload = packed.payload.clone();
    match (shape, value) {
        (Shape::Text(_, len), Const::Text(text)) => {
            let letter = b"qrs".iter().copied().find(|letter| {
                Some(*letter) != len.terminator() && text.as_bytes().first() != Some(letter)
            })?;
            let bytes = match len {
                Len::Fixed(n) => vec![letter; *n],
                _ => buffer(vec![letter], len),
            };
            let first = (start / 8) as usize;
            payload[first..first + bytes.len()].copy_from_slice(&bytes);
        }
        (_, Const::Int(n)) => {
            let bits = size as u32;
            put_bits(&mut payload, start, bits, fit(*n as u64, bits) ^ 1, little);
        }
        (_, Const::Bool(flag)) => {
            put_bits(&mut payload, start, size as u32, u64::from(!flag), little);
        }
        _ => return None,
    }
    write_checksums(layout, &mut payload, &packed.places);
    Some(payload)
}

/// Raw values an argument's field holds that decoding refuses (§7.4), as
/// the bits the field writes, each with its code.
fn refused_raws(encoder: &Encoder, shape: &Shape) -> Vec<(u64, &'static str)> {
    let mut raws = Vec::new();
    let bits = shape.bits().unwrap_or(0);
    let held = shape.integers();
    let integer = |n: i128| -> Option<u64> {
        let range = held.clone()?;
        range.contains(&n).then(|| fit(n as u64, bits))
    };
    match (&encoder.ty, &encoder.through) {
        // A state's raw value the conversion doesn't list.
        (ArgType::Enum(_), Through::States(states)) => {
            let listed: Vec<i128> = states.iter().map(|(raw, _)| *raw).collect();
            if let Some(range) = held.clone()
                && let Some(unlisted) = range.take(64).find(|raw| !listed.contains(raw))
            {
                raws.extend(integer(unlisted).map(|raw| (raw, "unknown_state")));
            }
        }
        // 2, which no bool is.
        (ArgType::Bool, _) => raws.extend(integer(2).map(|raw| (raw, "out_of_range"))),
        // Past its range or its type.
        (ArgType::U64 | ArgType::I64, _) => {
            let types = encoder.ty.integers();
            let (min, max) = encoder.integer_range;
            let high = max.unwrap_or(*types.end());
            let low = min.unwrap_or(*types.start());
            let past = integer(high + 1).or_else(|| integer(low - 1));
            raws.extend(past.map(|raw| (raw, "out_of_range")));
        }
        (ArgType::F64, _) => {
            let float = |x: f64| match shape {
                Shape::Float(32) => u64::from((x as f32).to_bits()),
                _ => x.to_bits(),
            };
            if matches!(shape, Shape::Float(_)) {
                for infinite in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                    raws.push((float(infinite), "non_finite"));
                }
            }
            // One past what the range's ends encode to.
            if let (Some(min), Some(max)) = (encoder.min, encoder.max)
                && let (Some(a), Some(b)) = (unrounded(encoder, min), unrounded(encoder, max))
            {
                let (low, high) = (a.min(b), a.max(b));
                match shape {
                    Shape::Float(32) => {
                        let past = f64::from((high as f32).next_up());
                        if past.is_finite() {
                            raws.push((float(past), "out_of_range"));
                        }
                    }
                    Shape::Float(_) => raws.push((float(high.next_up()), "out_of_range")),
                    _ => {
                        let past = integer(high.round() as i128 + 1)
                            .or_else(|| integer(low.round() as i128 - 1));
                        raws.extend(past.map(|raw| (raw, "out_of_range")));
                    }
                }
            }
        }
        _ => {}
    }
    raws
}
