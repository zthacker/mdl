//! The bundle written out as JSON, with the span of each value.

use std::cell::Cell;
use std::collections::BTreeMap;

use mdl::ir::{Catalog, IrVersion, ParamType};
use mdl::source::{SourceMap, Span};
use serde_json::Value;

use super::commands::{Argument, Command};
use super::{Element, Entry, Packet, Param, StructType, TypeId};
use crate::out::{List, Object, integer_json};
use crate::types::{ArgumentRange, Order, Scalar};

/// Everything the bundle is made of, once worked out.
pub(super) struct Bundle<'a, 'd> {
    pub(super) root: Span,
    pub(super) params: &'a BTreeMap<String, Param<'d>>,
    /// The set's family, which the import defines or joins.
    pub(super) family: &'a str,
    pub(super) order: Order,
    /// The family's alias, when the import defines the family; a joining
    /// import writes no family.
    pub(super) alias: Option<Value>,
    /// The metadata name a metadata alias reads, which the family
    /// declares.
    pub(super) metadata: Option<String>,
    pub(super) set: &'a str,
    pub(super) set_version: Option<Value>,
    pub(super) packets: &'a [Packet<'d>],
    pub(super) structs: &'a BTreeMap<TypeId<'d>, StructType<'d>>,
    pub(super) commands: &'a [Command<'d>],
    /// The owner's catalog, when the import refers to it
    /// ([`CatalogRole::Refer`](crate::CatalogRole::Refer)): command
    /// packets encode its commands, so none is written here, and each
    /// packet carries what the vendor says of its arguments, beyond what
    /// the catalog does, as its own limits.
    pub(super) refer: Option<&'a Catalog>,
}

pub(super) fn bundle(parts: Bundle<'_, '_>) -> (Value, SourceMap) {
    let writer = Writer {
        order: parts.order,
        structs: parts.structs,
        signed: Cell::new(false),
        refer: parts.refer,
        limited: Cell::new(false),
        accepts: Cell::new(false),
    };
    let mut params = Object::default();
    for (name, param) in parts.params {
        params.put_object(name, param_object(param));
    }
    let mut catalog = Object::at(parts.root);
    catalog.put_object("params", params);
    if !parts.commands.is_empty() && parts.refer.is_none() {
        let mut commands = Object::default();
        for command in parts.commands {
            commands.put_object(&command.canonical.0, command_object(command));
        }
        catalog.put_object("commands", commands);
    }

    let mut families = List::default();
    if let Some(alias) = parts.alias {
        let mut family = Object::at(parts.root);
        family.put("name", parts.family, None);
        family.put("encoding", "binary", None);
        family.put("byte_order", parts.order.as_str(), None);
        family.put("alias", alias, None);
        if let Some(name) = parts.metadata {
            let mut metadata = List::default();
            metadata.push(name, None);
            family.put_list("metadata", metadata);
        }
        families.push_object(family);
    }

    let mut set = Object::at(parts.root);
    set.put("name", parts.set, None);
    set.put("family", parts.family, None);
    if let Some(version) = parts.set_version {
        set.put("version", version, None);
    }
    if !parts.structs.is_empty() {
        let mut types = Object::default();
        for ty in parts.structs.values() {
            let mut object = Object::at(ty.span);
            object.put_list("entries", writer.entries(&ty.entries));
            types.put_object(&ty.name, object);
        }
        set.put_object("types", types);
    }
    let mut packets = List::default();
    for packet in parts.packets {
        packets.push_object(writer.packet(packet));
    }
    set.put_list("packets", packets);
    if !parts.commands.is_empty() {
        let mut commands = List::default();
        for command in parts.commands {
            commands.push_object(writer.command(command));
        }
        set.put_list("commands", commands);
    }
    let mut sets = List::default();
    sets.push_object(set);

    // The version the bundle needs, as a document states it.
    let version = if writer.accepts.get() {
        IrVersion::new(2, 3)
    } else if writer.limited.get() {
        IrVersion::new(2, 2)
    } else if writer.signed.get() {
        IrVersion::new(2, 1)
    } else {
        IrVersion::V2_0
    };
    let mut bundle = Object::at(parts.root);
    bundle.put("mdl_version", version.to_string(), None);
    bundle.put_object("catalog", catalog);
    bundle.put_list("families", families);
    bundle.put_list("sets", sets);
    bundle.finish()
}

fn param_object(param: &Param<'_>) -> Object {
    let scalar = &param.scalar;
    let mut object = Object::at(param.el.span());
    object.put("type", scalar.param, Some(scalar.el.span()));
    if let Some((unit, span)) = &scalar.unit {
        object.put("unit", unit.as_str(), Some(*span));
    }
    if !scalar.states.is_empty() {
        let mut states = List::default();
        for (state, span) in &scalar.states {
            states.push(state.as_str(), Some(*span));
        }
        object.put_list("states", states);
    }
    if let Some(limits) = &scalar.limits {
        let mut object_limits = Object::at(limits.span);
        for (key, value) in [
            ("red_low", limits.red_low),
            ("yellow_low", limits.yellow_low),
            ("yellow_high", limits.yellow_high),
            ("red_high", limits.red_high),
        ] {
            if let Some((value, span)) = value {
                object_limits.put(key, value, Some(span));
            }
        }
        object.put_object("limits", object_limits);
    }
    if let Some((text, span)) = &param.description {
        object.put("description", text.as_str(), Some(*span));
    }
    object
}

fn command_object(command: &Command<'_>) -> Object {
    let mut object = Object::at(command.el.span());
    if !command.arguments.is_empty() {
        let mut arguments = List::default();
        for argument in &command.arguments {
            arguments.push_object(argument_object(argument));
        }
        object.put_list("arguments", arguments);
    }
    if let Some((text, span)) = &command.description {
        object.put("description", text.as_str(), Some(*span));
    }
    object
}

fn argument_object(argument: &Argument<'_>) -> Object {
    let scalar = &argument.scalar;
    let mut object = Object::at(argument.el.span());
    object.put("name", argument.name.0.as_str(), Some(argument.name.1));
    object.put("type", scalar.param, Some(scalar.el.span()));
    if let Some((unit, span)) = &scalar.unit {
        object.put("unit", unit.as_str(), Some(*span));
    }
    if !scalar.states.is_empty() {
        let mut states = List::default();
        for (state, span) in &scalar.states {
            states.push(state.as_str(), Some(*span));
        }
        object.put_list("states", states);
    }
    if let Some((min, max)) = argument.list {
        let mut list = Object::at(argument.el.span());
        if let Some(min) = min.filter(|min| *min > 0) {
            list.put("min", min, None);
        }
        if let Some(max) = max {
            list.put("max", max, None);
        }
        object.put_object("list", list);
    }
    if let Some(range) = &scalar.range {
        object.put_object("range", range_object(range, scalar.el.span()));
    }
    if let Some((value, span)) = &argument.default {
        object.put("default", value.clone(), Some(*span));
    }
    if let Some((text, span)) = &argument.description {
        object.put("description", text.as_str(), Some(*span));
    }
    object
}

/// An argument's range, at its type.
fn range_object(range: &ArgumentRange, at: Span) -> Object {
    let mut object = Object::at(at);
    match range {
        ArgumentRange::Number(range) => {
            for (key, end) in [("min", range.min), ("max", range.max)] {
                if let Some((value, span)) = end {
                    object.put(key, value, Some(span));
                }
            }
        }
        // A decimal string past ±(2^53 - 1), as the IR writes integers.
        ArgumentRange::Integer(range) => {
            for (key, end) in [("min", range.min), ("max", range.max)] {
                if let Some((value, span)) = end {
                    object.put(key, integer_json(value), Some(span));
                }
            }
        }
    }
    object
}

/// The catalog argument's states a vendor's enumerated type lists, in the
/// catalog's order, each where the vendor lists it, when they're some of
/// its states and not all. Listing them all, the packet accepts every
/// state, as it does without a list; listing one the catalog lacks, the
/// field's own states say so, and validation reports it at the vendor's
/// label.
fn accepted_states<'s>(
    scalar: &'s Scalar<'_>,
    theirs: &mdl::ir::Argument,
) -> Option<Vec<(&'s str, Span)>> {
    if theirs.ty != ParamType::Enum || scalar.states.is_empty() {
        return None;
    }
    if scalar
        .states
        .iter()
        .any(|(label, _)| !theirs.states.contains(label))
    {
        return None;
    }
    let accepted: Vec<(&str, Span)> = theirs
        .states
        .iter()
        .filter_map(|state| {
            scalar
                .states
                .iter()
                .find(|(label, _)| label == state)
                .map(|(label, span)| (label.as_str(), *span))
        })
        .collect();
    (accepted.len() < theirs.states.len()).then_some(accepted)
}

/// Writes entries, knowing the family's byte order and the struct types'
/// names.
struct Writer<'a, 'd> {
    order: Order,
    structs: &'a BTreeMap<TypeId<'d>, StructType<'d>>,
    /// Whether a scalar written spells `signed`, which IR 2.1 has.
    signed: Cell<bool>,
    /// The catalog command packets refer to, when it's another input's.
    refer: Option<&'a Catalog>,
    /// Whether a command packet has limits of its own, which IR 2.2 has.
    limited: Cell<bool>,
    /// Whether one accepts some of an argument's states or requires an
    /// argument, which IR 2.3 has.
    accepts: Cell<bool>,
}

impl Writer<'_, '_> {
    fn packet(&self, packet: &Packet<'_>) -> Object {
        let mut object = Object::at(packet.el.span());
        object.put(
            "name",
            packet.name.as_str(),
            packet.el.attr("name").map(|a| a.span),
        );
        if !packet.identify.is_empty() {
            let mut identify = Object::default();
            for (key, (value, span)) in &packet.identify {
                identify.put(key, *value, Some(*span));
            }
            object.put_object("identify", identify);
        }
        object.put_list("entries", self.entries(&packet.entries));
        object
    }

    fn command(&self, command: &Command<'_>) -> Object {
        let mut object = Object::at(command.el.span());
        object.put(
            "name",
            command.name.as_str(),
            command.el.attr("name").map(|a| a.span),
        );
        object.put(
            "canonical",
            command.canonical.0.as_str(),
            Some(command.canonical.1),
        );
        if let Some(catalog) = self.refer
            && let Some(arguments) = self.packet_arguments(command, catalog)
        {
            object.put_object("arguments", arguments);
        }
        if !command.entries.is_empty() {
            object.put_list("entries", self.entries(&command.entries));
        }
        object
    }

    /// What a referring import's command packet accepts of its
    /// arguments, by name: what the vendor says beyond the owner's
    /// catalog. An argument type's stated range and its initial value are
    /// the packet's own range and default (IR 2.2), in the type's unit
    /// where that isn't the catalog argument's; an enumerated type that
    /// lists some of the catalog argument's states gives the states the
    /// packet accepts; and an argument with no initial value is required
    /// where the catalog's has a default, as Yamcs requires a value for it
    /// (both IR 2.3). `None` when no argument needs any. A range worked
    /// out from the encoding isn't one: validation checks the catalog's
    /// against what each field holds anyway.
    fn packet_arguments(&self, command: &Command<'_>, catalog: &Catalog) -> Option<Object> {
        let cataloged = catalog.commands.get(&command.canonical.0);
        let mut arguments = Object::at(command.el.span());
        let mut any = false;
        for argument in &command.arguments {
            let theirs = cataloged.and_then(|cataloged| {
                cataloged
                    .arguments
                    .iter()
                    .find(|theirs| theirs.name == argument.name.0)
            });
            let scalar = &argument.scalar;
            let range = scalar
                .stated
                .as_ref()
                .map(|range| range_object(range, scalar.el.span()));
            let values = range.is_some() || argument.default.is_some();
            let states = theirs.and_then(|theirs| accepted_states(scalar, theirs));
            let required =
                argument.default.is_none() && theirs.is_some_and(|theirs| theirs.default.is_some());
            if !values && states.is_none() && !required {
                continue;
            }
            let mut object = Object::at(argument.el.span());
            // Without a unit, a packet's limits are in the catalog
            // argument's, so that one isn't written.
            let own = theirs.and_then(|theirs| theirs.unit.as_deref());
            if let Some((unit, span)) = scalar
                .unit
                .as_ref()
                .filter(|(unit, _)| values && own != Some(unit.as_str()))
            {
                object.put("unit", unit.as_str(), Some(*span));
            }
            if let Some(range) = range {
                object.put_object("range", range);
            }
            if let Some((value, span)) = &argument.default {
                object.put("default", value.clone(), Some(*span));
            }
            if let Some(states) = states {
                let mut list = List::default();
                for (state, span) in states {
                    list.push(state, Some(span));
                }
                object.put_list("states", list);
                self.accepts.set(true);
            }
            if required {
                object.put("required", true, Some(argument.el.span()));
                self.accepts.set(true);
            }
            arguments.put_object(&argument.name.0, object);
            any = true;
        }
        if any {
            self.limited.set(true);
        }
        any.then_some(arguments)
    }

    fn entries(&self, entries: &[Entry<'_>]) -> List {
        let mut list = List::default();
        for entry in entries {
            list.push_object(self.entry(entry));
        }
        list
    }

    fn type_name(&self, ty: &TypeId<'_>) -> String {
        self.structs
            .get(ty)
            .map(|t| t.name.clone())
            .unwrap_or_default()
    }

    fn entry(&self, entry: &Entry<'_>) -> Object {
        match entry {
            Entry::Padding { bits, span, when } => {
                let mut object = Object::at(*span);
                object.put("kind", "padding", None);
                object.put("size_bits", *bits, Some(*span));
                if let Some(when) = when {
                    object.put_object("when", when.clone());
                }
                object
            }
            Entry::Field(field) => {
                let mut object = Object::at(field.span);
                object.put("kind", "field", None);
                object.put("name", field.name.as_str(), None);
                if let Some((bits, span)) = field.offset {
                    object.put("offset_bits", bits, Some(span));
                }
                self.scalar(&mut object, &field.scalar);
                if let Some((length, span)) = &field.length {
                    object.put("length", length.clone(), Some(*span));
                }
                if let Some((canonical, span)) = &field.canonical {
                    object.put("canonical", canonical.as_str(), Some(*span));
                }
                if let Some((argument, span)) = &field.argument {
                    object.put("argument", argument.as_str(), Some(*span));
                }
                if let Some((value, span)) = &field.value {
                    object.put("value", value.clone(), Some(*span));
                }
                if let Some(when) = &field.when {
                    object.put_object("when", when.clone());
                }
                object
            }
            Entry::Array(array) => {
                let mut object = Object::at(array.span);
                object.put("kind", "array", None);
                object.put("name", array.name.as_str(), None);
                if let Some((bits, span)) = array.offset {
                    object.put("offset_bits", bits, Some(span));
                }
                object.put("count", array.count.0.clone(), Some(array.count.1));
                let mut element = Object::at(array.span);
                match &array.element {
                    Element::Scalar(scalar) => {
                        element.put("kind", "scalar", None);
                        self.scalar(&mut element, scalar);
                        if let Some(crate::types::Length::Written(length, span)) = &scalar.length {
                            element.put("length", length.clone(), Some(*span));
                        }
                    }
                    Element::Struct(ty) => {
                        element.put("kind", "struct", None);
                        element.put("name", self.type_name(ty), None);
                    }
                }
                object.put_object("element", element);
                if let Some((canonical, span)) = &array.canonical {
                    object.put("canonical", canonical.as_str(), Some(*span));
                }
                if let Some((argument, span)) = &array.argument {
                    object.put("argument", argument.as_str(), Some(*span));
                }
                if let Some(when) = &array.when {
                    object.put_object("when", when.clone());
                }
                object
            }
            Entry::Struct(inner) => {
                let mut object = Object::at(inner.span);
                object.put("kind", "struct", None);
                object.put("name", inner.name.as_str(), None);
                if let Some((bits, span)) = inner.offset {
                    object.put("offset_bits", bits, Some(span));
                }
                object.put("struct", self.type_name(&inner.ty), None);
                if let Some(when) = &inner.when {
                    object.put_object("when", when.clone());
                }
                object
            }
        }
    }

    /// What a field and a scalar element share: type, size, byte order,
    /// text, conversion and unit.
    fn scalar(&self, object: &mut Object, scalar: &Scalar<'_>) {
        object.put("type", scalar.ty.0, Some(scalar.ty.1));
        if let Some((bits, span)) = scalar.size_bits {
            object.put("size_bits", bits, Some(span));
        }
        if let Some((own, span)) = scalar.order
            && own != self.order
        {
            object.put("byte_order", own.as_str(), Some(span));
        }
        if let Some((form, span)) = scalar.signed {
            object.put("signed", form, Some(span));
            self.signed.set(true);
        }
        if let Some((text, span)) = scalar.text {
            object.put("text", text, Some(span));
        }
        if let Some(conversion) = &scalar.conversion {
            object.put_object("conversion", conversion.clone());
        }
        if let Some(inverse) = &scalar.inverse {
            object.put_object("inverse", inverse.clone());
        }
        if let Some((unit, span)) = &scalar.unit {
            object.put("unit", unit.as_str(), Some(*span));
        }
    }
}
