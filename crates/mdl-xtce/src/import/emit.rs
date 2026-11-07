//! The bundle written out as JSON, with the span of each value.

use std::collections::BTreeMap;

use mdl::v2::ir::IrVersion;
use mdl::v2::source::{SourceMap, Span};
use serde_json::Value;

use super::{Element, Entry, Packet, Param, StructType, TypeId};
use crate::out::{List, Object};
use crate::types::{Order, Scalar};

/// Everything the bundle is made of, once worked out.
pub(super) struct Bundle<'a, 'd> {
    pub(super) root: Span,
    pub(super) params: &'a BTreeMap<String, Param<'d>>,
    pub(super) family: &'a str,
    pub(super) order: Order,
    pub(super) alias: Value,
    pub(super) set: &'a str,
    pub(super) packets: &'a [Packet<'d>],
    pub(super) structs: &'a BTreeMap<TypeId<'d>, StructType<'d>>,
}

pub(super) fn bundle(parts: Bundle<'_, '_>) -> (Value, SourceMap) {
    let writer = Writer {
        order: parts.order,
        structs: parts.structs,
    };
    let mut params = Object::default();
    for (name, param) in parts.params {
        params.put_object(name, param_object(param));
    }
    let mut catalog = Object::at(parts.root);
    catalog.put_object("params", params);

    let mut family = Object::at(parts.root);
    family.put("name", parts.family, None);
    family.put("encoding", "binary", None);
    family.put("byte_order", parts.order.as_str(), None);
    family.put("alias", parts.alias, None);
    let mut families = List::default();
    families.push_object(family);

    let mut set = Object::at(parts.root);
    set.put("name", parts.set, None);
    set.put("family", parts.family, None);
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
    let mut sets = List::default();
    sets.push_object(set);

    let mut bundle = Object::at(parts.root);
    bundle.put("mdl_version", IrVersion::CURRENT.to_string(), None);
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

/// Writes entries, knowing the family's byte order and the struct types'
/// names.
struct Writer<'a, 'd> {
    order: Order,
    structs: &'a BTreeMap<TypeId<'d>, StructType<'d>>,
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
        if let Some((text, span)) = scalar.text {
            object.put("text", text, Some(span));
        }
        if let Some(conversion) = &scalar.conversion {
            object.put_object("conversion", conversion.clone());
        }
        if let Some((unit, span)) = &scalar.unit {
            object.put("unit", unit.as_str(), Some(*span));
        }
    }
}
