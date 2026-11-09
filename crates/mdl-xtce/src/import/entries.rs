//! A container's entries: parameters (scalars, aggregates, arrays),
//! repeated entries, embedded containers, and where each goes.

use mdl::v2::source::Span;
use mdl::v2::validate::MAX_NESTING;
use serde_json::{Value, json};

use super::{
    ArrayField, CONTAINER, Element, Entry, Field, Fields, Import, Known, ParamKey, StructField,
    StructType, TypeId, id,
};
use crate::Code;
use crate::names::identifier;
use crate::out::Object;
use crate::space::Kind;
use crate::types::{
    ArrayShape, Count, Dynamic, IntegerValue, Length, Order, Raw, Scalar, Shape, dimensions,
    integer_value,
};
use crate::xml::{self, El, Expect};

const ENTRIES: &[&str] = &[
    "ParameterRefEntry",
    "ParameterSegmentRefEntry",
    "ContainerRefEntry",
    "ContainerSegmentRefEntry",
    "StreamSegmentEntry",
    "IndirectParameterRefEntry",
    "ArrayParameterRefEntry",
];

const ENTRY: &[&str] = &[
    "LocationInContainerInBits",
    "IncludeCondition",
    "RepeatEntry",
    "TimeAssociation",
];

/// Where an entry's location puts it.
pub(super) enum Location {
    /// This many bits from the start of the container.
    Start(u32, Span),
    /// This many bits after the previous entry ends.
    After(u32, Span),
}

/// How the entries of one XTCE entry are placed and kept: its location,
/// its condition, and the element it comes from.
struct Place {
    span: Span,
    location: Option<Location>,
    when: Option<Object>,
}

/// What the entries of one XTCE entry are built from: the parameter's
/// element, where they go, and how their fields are named.
struct Source<'p, 'd> {
    el: El<'d>,
    place: &'p Place,
    naming: Naming,
}

impl Place {
    /// The padding a `previousEntry` location asks for, ahead of the entry.
    fn padding<'d>(&self) -> Option<Entry<'d>> {
        match self.location {
            Some(Location::After(bits, span)) if bits > 0 => Some(Entry::Padding {
                bits,
                span,
                when: self.when.clone(),
            }),
            _ => None,
        }
    }

    /// The offset of the entry's first part.
    fn offset(&self) -> Option<(u32, Span)> {
        match self.location {
            Some(Location::Start(bits, span)) => Some((bits, span)),
            _ => None,
        }
    }
}

/// How field names are made: from the parameter (in a packet or an
/// embedded container), or from the members below an element (in an
/// element's struct type).
#[derive(Clone, Copy)]
enum Naming {
    Parameter,
    /// Members from this depth on name the field.
    Members(usize),
}

impl<'d> Import<'d, '_> {
    /// An EntryList's entries and the fields they add, given the fields
    /// already decoded in the same MDL container.
    pub(super) fn entries(
        &mut self,
        space: usize,
        list: El<'d>,
        mut known: Fields<'d>,
    ) -> Option<(Vec<Entry<'d>>, Fields<'d>)> {
        list.check(
            &Expect {
                attrs: &[],
                children: ENTRIES,
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let mut entries = Vec::new();
        let mut fields = Vec::new();
        let mut complete = true;
        for entry in list.children() {
            let added = match entry.name() {
                "ParameterRefEntry" | "ArrayParameterRefEntry" => {
                    self.parameter_entry(space, entry, &known)
                }
                "ContainerRefEntry" => self
                    .container_entry(space, entry, &known)
                    .map(|entries| (entries, Vec::new())),
                name @ ("ParameterSegmentRefEntry"
                | "ContainerSegmentRefEntry"
                | "StreamSegmentEntry"
                | "IndirectParameterRefEntry") => {
                    self.report.error(
                        Code::Unsupported,
                        entry.span(),
                        format!("{name} isn't supported: MDL reads whole values at known places"),
                    );
                    None
                }
                _ => continue,
            };
            match added {
                Some((new_entries, new_fields)) => {
                    entries.extend(new_entries);
                    known.extend(new_fields.iter().cloned());
                    fields.extend(new_fields);
                }
                None => complete = false,
            }
        }
        complete.then_some((entries, fields))
    }

    /// A ParameterRefEntry or ArrayParameterRefEntry: a field, one per
    /// member of an aggregate, an array, or the parameter repeated.
    fn parameter_entry(
        &mut self,
        space: usize,
        entry: El<'d>,
        known: &[(ParamKey<'d>, Known)],
    ) -> Option<(Vec<Entry<'d>>, Fields<'d>)> {
        let array_ref = entry.name() == "ArrayParameterRefEntry";
        let mut children = ENTRY.to_vec();
        if array_ref {
            children.push("DimensionList");
        }
        entry.check(
            &Expect {
                attrs: &["parameterRef", "lastEntryForThisArrayInstance"],
                children: &children,
                ignored: &["shortDescription", "AncillaryDataSet"],
                text: false,
            },
            self.report,
        );
        super::conditions::time_association(entry, self.report)?;
        if let Some(last) = entry.attr("lastEntryForThisArrayInstance")
            && xml::boolean(last, self.report)?
        {
            self.report.error(
                Code::Unsupported,
                last.span,
                "an array split across entries isn't supported",
            );
            return None;
        }
        let reference = entry.required("parameterRef", self.report)?;
        let found = match self.tree.resolve(space, reference.value, Kind::Parameter) {
            Ok(found) => found,
            Err(message) => {
                self.report.error(Code::Reference, reference.span, message);
                return None;
            }
        };
        let name = found.el.item_name()?;
        self.seen.insert((found.space, name));
        if !found.members.is_empty() {
            self.report.error(
                Code::Invalid,
                reference.span,
                "an entry names a whole parameter, not a member of one",
            );
            return None;
        }
        let mut shape = self.parameter_shape(found.space, found.el)?;
        if array_ref {
            shape = self.array_reference(space, entry, shape)?;
        }
        let place = self.place(space, entry, known)?;
        let key = (found.space, name, Vec::new());
        let mut out = Vec::new();
        out.extend(place.padding());
        if let Some(repeat) = entry.child("RepeatEntry") {
            let (count, offset) = self.repeat(space, repeat, known)?;
            let element = self.element(&key, found.el, &shape, offset)?;
            let array = self.array_field(&key, found.el, element, count, &place)?;
            out.push(array);
            return Some((out, Vec::new()));
        }
        let mut fields = Vec::new();
        let source = Source {
            el: found.el,
            place: &place,
            naming: Naming::Parameter,
        };
        out.extend(self.build(&key, &shape, None, &source, known, &mut fields)?);
        Some((out, fields))
    }

    /// An ArrayParameterRefEntry's array: an XTCE 1.1 array takes its size
    /// from the entry; a 1.2 one's entry may only repeat its type's.
    fn array_reference(
        &mut self,
        space: usize,
        entry: El<'d>,
        shape: Shape<'d>,
    ) -> Option<Shape<'d>> {
        let Shape::Array(mut array) = shape else {
            self.report.error(
                Code::Invalid,
                entry.span(),
                "an ArrayParameterRefEntry names an array parameter",
            );
            return None;
        };
        let Some(list) = entry.child("DimensionList") else {
            return Some(Shape::Array(array));
        };
        let count = dimensions(list, space, self.report)?;
        match &array.count {
            None => array.count = Some(count),
            Some(own) if same_count(own, &count) => {}
            Some(_) => {
                self.report.error(
                    Code::Unsupported,
                    list.span(),
                    "a subset of an array isn't supported: MDL reads whole arrays",
                );
                return None;
            }
        }
        Some(Shape::Array(array))
    }

    /// A ContainerRefEntry: a struct entry of the container's type, or an
    /// array of them when repeated.
    fn container_entry(
        &mut self,
        space: usize,
        entry: El<'d>,
        known: &[(ParamKey<'d>, Known)],
    ) -> Option<Vec<Entry<'d>>> {
        entry.check(
            &Expect {
                attrs: &["containerRef"],
                children: ENTRY,
                ignored: &["shortDescription", "AncillaryDataSet"],
                text: false,
            },
            self.report,
        );
        super::conditions::time_association(entry, self.report)?;
        let reference = entry.required("containerRef", self.report)?;
        let found = match self.tree.resolve(space, reference.value, Kind::Container) {
            Ok(found) => found,
            Err(message) => {
                self.report.error(Code::Reference, reference.span, message);
                return None;
            }
        };
        let ty = self.struct_type(found.space, found.el)?;
        let place = self.place(space, entry, known)?;
        let xtce = found.el.item_name()?;
        let (name, _) = identifier(xtce);
        let mut out = Vec::new();
        out.extend(place.padding());
        if let Some(repeat) = entry.child("RepeatEntry") {
            let (count, offset) = self.repeat(space, repeat, known)?;
            if offset > 0 {
                self.report.error(
                    Code::Unsupported,
                    repeat.span(),
                    "a repeated container with an offset between repetitions isn't supported",
                );
                return None;
            }
            out.push(Entry::Array(Box::new(ArrayField {
                name,
                key: None,
                span: place.span,
                element: Element::Struct(ty),
                count,
                canonical: None,
                offset: place.offset(),
                when: place.when,
            })));
            return Some(out);
        }
        out.push(Entry::Struct(Box::new(StructField {
            name,
            ty,
            span: place.span,
            offset: place.offset(),
            when: place.when,
        })));
        Some(out)
    }

    /// The struct type of an embedded container, worked out once. Its
    /// conditions, counts and lengths use its own fields, as MDL's struct
    /// types do; positions are from its own start.
    fn struct_type(&mut self, space: usize, el: El<'d>) -> Option<TypeId<'d>> {
        let element = id(&el);
        let ty = TypeId::Container(element);
        if self.structs.contains_key(&ty) {
            return Some(ty);
        }
        if self.broken.contains(&element) {
            return None;
        }
        let name = el.item_name().unwrap_or_default();
        if self.embedding.contains(&element) {
            self.report.error(
                Code::Invalid,
                el.span(),
                format!("container {name} contains itself"),
            );
            return None;
        }
        if self.embedding.len() >= MAX_NESTING {
            self.report.error(
                Code::Unsupported,
                el.span(),
                format!(
                    "containers embedded more than {MAX_NESTING} deep aren't supported: MDL \
                     bounds struct nesting there"
                ),
            );
            return None;
        }
        self.embedding.push(element);
        let entries = self.struct_body(space, el);
        self.embedding.pop();
        let Some(entries) = entries else {
            self.broken.insert(element);
            return None;
        };
        self.structs.insert(
            ty.clone(),
            StructType {
                name: String::new(),
                base: identifier(name).0,
                space,
                span: el.span(),
                entries,
            },
        );
        Some(ty)
    }

    fn struct_body(&mut self, space: usize, el: El<'d>) -> Option<Vec<Entry<'d>>> {
        el.check(&CONTAINER, self.report);
        if let Some(base) = el.child("BaseContainer") {
            self.report.error(
                Code::Unsupported,
                base.span(),
                "an embedded container that inherits from another isn't supported",
            );
            return None;
        }
        if let Some(size) = el.child("BinaryEncoding") {
            self.report.error(
                Code::Unsupported,
                size.span(),
                "a container's own size (BinaryEncoding) isn't supported yet",
            );
            return None;
        }
        let Some(list) = el.child("EntryList") else {
            self.report.error(
                Code::Invalid,
                el.span(),
                "a SequenceContainer has an EntryList",
            );
            return None;
        };
        let (entries, _) = self.entries(space, list, Vec::new())?;
        self.check_absent_locations(&entries)?;
        self.check_entries(&entries, "embedded container")?;
        if entries.is_empty() {
            self.report.error(
                Code::Unsupported,
                el.span(),
                "an embedded container with no entries isn't supported: MDL struct types have \
                 entries",
            );
            return None;
        }
        Some(entries)
    }

    /// A RepeatEntry's count and the bits between repetitions.
    fn repeat(
        &mut self,
        space: usize,
        repeat: El<'d>,
        known: &[(ParamKey<'d>, Known)],
    ) -> Option<((Value, Span), u32)> {
        repeat.check(
            &Expect {
                attrs: &[],
                children: &["Count", "Offset"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let Some(count) = repeat.child("Count") else {
            self.report
                .error(Code::Invalid, repeat.span(), "a RepeatEntry has a Count");
            return None;
        };
        let count = match integer_value(count, space, self.report)? {
            IntegerValue::Fixed(n, span) => match u32::try_from(n) {
                Ok(n) => (json!({ "kind": "fixed", "count": n }), span),
                Err(_) => {
                    self.report.error(
                        Code::Invalid,
                        span,
                        format!("{n} isn't a count of repetitions"),
                    );
                    return None;
                }
            },
            IntegerValue::Dynamic(dynamic) => {
                let linear = linear_count((dynamic.slope, dynamic.intercept));
                self.read_from(&dynamic, linear, known)?
            }
        };
        let offset = match repeat.child("Offset") {
            Some(offset) => match integer_value(offset, space, self.report)? {
                IntegerValue::Fixed(n, span) => match u32::try_from(n) {
                    Ok(n) => n,
                    Err(_) => {
                        self.report.error(
                            Code::Unsupported,
                            span,
                            "a negative offset between repetitions isn't supported",
                        );
                        return None;
                    }
                },
                IntegerValue::Dynamic(dynamic) => {
                    self.report.error(
                        Code::Unsupported,
                        dynamic.span,
                        "an offset between repetitions another parameter gives isn't supported",
                    );
                    return None;
                }
            },
            None => 0,
        };
        Some((count, offset))
    }

    /// An entry's location and condition.
    fn place(
        &mut self,
        space: usize,
        entry: El<'d>,
        known: &[(ParamKey<'d>, Known)],
    ) -> Option<Place> {
        let when = match entry.child("IncludeCondition") {
            Some(condition) => Some(self.condition(space, condition, known)?),
            None => None,
        };
        let location = match entry.child("LocationInContainerInBits") {
            Some(location) => Some(self.location(location)?),
            None => None,
        };
        Some(Place {
            span: entry.span(),
            location,
            when,
        })
    }

    /// The entries a shape becomes, reading the parameter (or member)
    /// `key`, with `known` the fields decoded before them in the same
    /// container; their fields are added to `fields`.
    fn build(
        &mut self,
        key: &ParamKey<'d>,
        shape: &Shape<'d>,
        description: Option<(String, Span)>,
        source: &Source<'_, 'd>,
        known: &[(ParamKey<'d>, Known)],
        fields: &mut Fields<'d>,
    ) -> Option<Vec<Entry<'d>>> {
        let Source { el, place, naming } = *source;
        let mut out = Vec::new();
        let first = |out: &Vec<Entry<'d>>| out.iter().all(|e| e.span() != Some(place.span));
        match shape {
            Shape::Scalar(scalar) => {
                let name = self.name_of(key, el, naming);
                let canonical = self.register(key, el, scalar, description)?;
                let offset = place.offset().filter(|_| first(&out));
                let mut seen: Vec<(ParamKey<'d>, Known)> = known.to_vec();
                seen.extend(fields.iter().cloned());
                let length = match &scalar.length {
                    None => None,
                    Some(Length::Written(value, span)) => Some((value.clone(), *span)),
                    Some(Length::Counted(dynamic)) => {
                        let linear = dynamic.bytes();
                        Some(self.read_from(dynamic, linear, &seen)?)
                    }
                    Some(Length::Leading { bits, order, span }) => {
                        let tag = format!("{name}_size");
                        out.push(Entry::Field(Box::new(Field {
                            name: tag.clone(),
                            key: None,
                            span: place.span,
                            scalar: size_tag(scalar, *bits, *order, *span),
                            length: None,
                            canonical: None,
                            argument: None,
                            value: None,
                            offset,
                            when: place.when.clone(),
                        })));
                        Some((json!({ "kind": "field", "field": tag }), *span))
                    }
                };
                let offset = offset.filter(|_| first(&out));
                fields.push((
                    key.clone(),
                    Known {
                        name: name.clone(),
                        ty: scalar.ty.0,
                        raw: scalar.raw.clone(),
                    },
                ));
                out.push(Entry::Field(Box::new(Field {
                    name,
                    key: Some(key.clone()),
                    span: place.span,
                    scalar: (**scalar).clone(),
                    length,
                    canonical: Some(canonical),
                    argument: None,
                    value: None,
                    offset,
                    when: place.when.clone(),
                })));
            }
            Shape::Aggregate(members) => {
                for member in members {
                    let mut member_key = key.clone();
                    member_key.2.push(member.name);
                    let mut seen: Vec<(ParamKey<'d>, Known)> = known.to_vec();
                    seen.extend(fields.iter().cloned());
                    let mut added = Vec::new();
                    let built = self.build(
                        &member_key,
                        &member.shape,
                        member.description.clone(),
                        source,
                        &seen,
                        &mut added,
                    )?;
                    // Only the first entry of the XTCE entry takes its
                    // offset.
                    let placed = !first(&out);
                    out.extend(built.into_iter().map(|mut entry| {
                        if placed {
                            clear_offset(&mut entry);
                        }
                        entry
                    }));
                    fields.extend(added);
                }
            }
            Shape::Array(array) => {
                let mut seen: Vec<(ParamKey<'d>, Known)> = known.to_vec();
                seen.extend(fields.iter().cloned());
                let count = self.array_count(array, key, &seen)?;
                let element = self.element(key, el, &array.element, 0)?;
                let mut entry = self.array_field(key, el, element, count, place)?;
                if !first(&out) {
                    clear_offset(&mut entry);
                }
                if let (Entry::Array(field), Naming::Members(_)) = (&mut entry, naming) {
                    field.name = self.name_of(key, el, naming);
                }
                out.push(entry);
            }
        }
        Some(out)
    }

    /// An array of `element`s reading `key`.
    fn array_field(
        &mut self,
        key: &ParamKey<'d>,
        el: El<'d>,
        element: Element<'d>,
        count: (Value, Span),
        place: &Place,
    ) -> Option<Entry<'d>> {
        let canonical = match &element {
            Element::Scalar(scalar) => Some(self.register(key, el, scalar, None)?),
            Element::Struct(_) => None,
        };
        Some(Entry::Array(Box::new(ArrayField {
            name: self.name_of(key, el, Naming::Parameter),
            key: Some(key.clone()),
            span: place.span,
            element,
            count,
            canonical,
            offset: place.offset(),
            when: place.when.clone(),
        })))
    }

    /// An array's element: a scalar, or a struct type holding what the
    /// shape becomes and the padding between elements.
    fn element(
        &mut self,
        key: &ParamKey<'d>,
        el: El<'d>,
        shape: &Shape<'d>,
        padding: u32,
    ) -> Option<Element<'d>> {
        if let (Shape::Scalar(scalar), 0) = (shape, padding) {
            if !matches!(scalar.length, None | Some(Length::Written(..))) {
                self.report.error(
                    Code::Unsupported,
                    scalar.el.span(),
                    "array elements whose size another value gives aren't supported: MDL \
                     elements have their own sizes",
                );
                return None;
            }
            return Some(Element::Scalar(scalar.clone()));
        }
        if matches!(shape, Shape::Array(_)) {
            self.report.error(
                Code::Unsupported,
                el.span(),
                "a repeated array isn't supported: MDL arrays have one dimension",
            );
            return None;
        }
        let ty = TypeId::Element(key.clone(), padding);
        if self.structs.contains_key(&ty) {
            return Some(Element::Struct(ty));
        }
        // An element's members are named below the element, and its
        // fields refer only to each other.
        let place = Place {
            span: el.span(),
            location: None,
            when: None,
        };
        let mut fields = Vec::new();
        let source = Source {
            el,
            place: &place,
            naming: Naming::Members(key.2.len()),
        };
        let mut entries = self.build(key, shape, None, &source, &[], &mut fields)?;
        if padding > 0 {
            entries.push(Entry::Padding {
                bits: padding,
                span: el.span(),
                when: None,
            });
        }
        self.check_entries(&entries, "array element")?;
        // The gap is in the name, so one parameter repeated with different
        // gaps gets a type for each, each named the same wherever it's used.
        let field = self.name_of(key, el, Naming::Parameter);
        let base = match padding {
            0 => format!("{field}_element"),
            bits => format!("{field}_element_gap{bits}"),
        };
        self.structs.insert(
            ty.clone(),
            StructType {
                name: String::new(),
                base,
                space: key.0,
                span: el.span(),
                entries,
            },
        );
        Some(Element::Struct(ty))
    }

    /// An array's count of elements, written out.
    fn array_count(
        &mut self,
        array: &ArrayShape<'d>,
        key: &ParamKey<'d>,
        known: &[(ParamKey<'d>, Known)],
    ) -> Option<(Value, Span)> {
        match &array.count {
            Some(Count::Fixed(n, span)) => Some((json!({ "kind": "fixed", "count": n }), *span)),
            Some(Count::Counted { dynamic, start }) => {
                let linear = dynamic.count(*start);
                self.read_from(dynamic, linear, known)
            }
            None => {
                self.report.error(
                    Code::Invalid,
                    array.el.span(),
                    format!(
                        "array {} has no DimensionList, and a ParameterRefEntry gives it no \
                         size; use an ArrayParameterRefEntry with one",
                        self.describe(key)
                    ),
                );
                None
            }
        }
    }

    /// A count or length read from another field, written out.
    fn read_from(
        &mut self,
        dynamic: &Dynamic<'d>,
        linear: Result<(u32, i64), String>,
        known: &[(ParamKey<'d>, Known)],
    ) -> Option<(Value, Span)> {
        let (field, multiply, add) = self.size_field(dynamic, linear, known)?;
        Some((from_field(&field, multiply, add), dynamic.span))
    }

    /// The field a DynamicValue reads, which MDL takes times `multiply`
    /// plus `add`: an unsigned integer field decoded earlier in the same
    /// container, whose raw value is the value XTCE reads.
    fn size_field(
        &mut self,
        dynamic: &Dynamic<'d>,
        linear: Result<(u32, i64), String>,
        known: &[(ParamKey<'d>, Known)],
    ) -> Option<(String, u32, i64)> {
        let (multiply, add) = match linear {
            Ok(linear) => linear,
            Err(reason) => {
                self.report.error(Code::Unsupported, dynamic.span, reason);
                return None;
            }
        };
        let found = match self
            .tree
            .resolve(dynamic.space, dynamic.reference, Kind::Parameter)
        {
            Ok(found) => found,
            Err(message) => {
                self.report.error(Code::Reference, dynamic.span, message);
                return None;
            }
        };
        let name = found.el.item_name()?;
        self.seen.insert((found.space, name));
        let key = (found.space, name, found.members);
        let Some((_, field)) = known.iter().find(|(k, _)| *k == key) else {
            self.report.error(
                Code::Unsupported,
                dynamic.span,
                format!(
                    "{} isn't a field decoded earlier in the same container, and MDL takes sizes \
                     and counts only from those",
                    dynamic.reference
                ),
            );
            return None;
        };
        let raw_is_value = !dynamic.calibrated || matches!(field.raw, Raw::Integer);
        if field.ty != "uint" || !raw_is_value {
            self.report.error(
                Code::Unsupported,
                dynamic.span,
                format!(
                    "{} isn't an unsigned integer whose raw value is its value, and MDL takes \
                     sizes and counts from those",
                    dynamic.reference
                ),
            );
            return None;
        }
        Some((field.name.clone(), multiply, add))
    }

    pub(super) fn location(&mut self, el: El<'d>) -> Option<Location> {
        el.check(
            &Expect {
                attrs: &["referenceLocation"],
                children: &["FixedValue", "DynamicValue", "DiscreteLookupList"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let Some(fixed) = el.child("FixedValue") else {
            self.report.error(
                Code::Unsupported,
                el.span(),
                "a location worked out from other values isn't supported",
            );
            return None;
        };
        let (text, span) = fixed.text();
        let bits = xml::long_text(&text, "FixedValue", span, self.report)?;
        let reference = el.attr("referenceLocation");
        match reference.map_or("previousEntry", |a| a.value.trim()) {
            "containerStart" => match u32::try_from(bits) {
                Ok(bits) => Some(Location::Start(bits, span)),
                Err(_) => {
                    self.report.error(
                        Code::Unsupported,
                        span,
                        format!("{bits} isn't a bit position MDL can place an entry at"),
                    );
                    None
                }
            },
            "previousEntry" => match u32::try_from(bits) {
                Ok(bits) => Some(Location::After(bits, span)),
                Err(_) => {
                    self.report.error(
                        Code::Unsupported,
                        span,
                        "an entry placed before the end of the previous one isn't supported",
                    );
                    None
                }
            },
            "containerEnd" | "nextEntry" => {
                self.report.error(
                    Code::Unsupported,
                    reference.map_or(el.span(), |a| a.span),
                    "locations from the container's end or the next entry aren't supported: MDL \
                     places entries from the start",
                );
                None
            }
            other => {
                self.report.error(
                    Code::Invalid,
                    reference.map_or(el.span(), |a| a.span),
                    format!("{other:?} isn't a reference location"),
                );
                None
            }
        }
    }

    /// A field's name: the parameter's (qualified as names are), members
    /// joined by `_`; inside an element, the members below it.
    fn name_of(&mut self, key: &ParamKey<'d>, el: El<'d>, naming: Naming) -> String {
        match naming {
            Naming::Parameter => self.field_name(key, el),
            Naming::Members(depth) => {
                let below = key.2.get(depth..).unwrap_or_default();
                if below.is_empty() {
                    return "value".to_string();
                }
                let joined = below.join("_");
                identifier(&joined).0
            }
        }
    }
}

/// The size tag of a leading-size string: an unsigned integer of `bits`
/// bits, in the string encoding's byte order.
fn size_tag<'d>(
    string: &Scalar<'d>,
    bits: u32,
    order: Option<(Order, Span)>,
    span: Span,
) -> Scalar<'d> {
    Scalar {
        el: string.el,
        ty: ("uint", span),
        size_bits: Some((bits, span)),
        order: (bits > 8).then(|| order.unwrap_or((Order::Big, span))),
        length: None,
        text: None,
        conversion: None,
        inverse: None,
        unit: None,
        param: "u64",
        states: Vec::new(),
        raw: Raw::Integer,
        limits: None,
        range: None,
        initial: None,
    }
}

fn clear_offset(entry: &mut Entry<'_>) {
    match entry {
        Entry::Field(field) => field.offset = None,
        Entry::Array(array) => array.offset = None,
        Entry::Struct(inner) => inner.offset = None,
        Entry::Padding { .. } => {}
    }
}

/// MDL's `{kind: field, field, multiply, add}`, leaving out the defaults.
fn from_field(field: &str, multiply: u32, add: i64) -> Value {
    let mut value = json!({ "kind": "field", "field": field });
    if multiply != 1 {
        value["multiply"] = json!(multiply);
    }
    if add != 0 {
        value["add"] = json!(add);
    }
    value
}

/// A count that is a parameter's value as it is: `value × slope +
/// intercept`.
fn linear_count((slope, intercept): (i64, i64)) -> Result<(u32, i64), String> {
    match u32::try_from(slope) {
        Ok(multiply) if multiply > 0 => Ok((multiply, intercept)),
        _ => Err(format!(
            "a slope of {slope} isn't supported: MDL takes a count from a field times a positive \
             whole number"
        )),
    }
}

/// Two counts that are the same: fixed and equal, or the same reference.
fn same_count(a: &Count<'_>, b: &Count<'_>) -> bool {
    match (a, b) {
        (Count::Fixed(x, _), Count::Fixed(y, _)) => x == y,
        (
            Count::Counted {
                dynamic: x,
                start: s,
            },
            Count::Counted {
                dynamic: y,
                start: t,
            },
        ) => {
            x.reference == y.reference && x.slope == y.slope && x.intercept == y.intercept && s == t
        }
        _ => false,
    }
}
