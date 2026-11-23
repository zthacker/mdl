//! A container's entries: parameters (scalars, aggregates, arrays),
//! repeated entries, embedded containers, and where each goes.

use mdl::source::Span;
use mdl::validate::MAX_NESTING;
use serde_json::{Value, json};

use super::{
    ArrayField, CONTAINER, Composite, Earlier, Element, Entry, Field, Fields, Import, Known,
    ParamKey, StructField, StructType, TypeId, id,
};
use crate::Code;
use crate::ancillary::{Mark, NO_MARK};
use crate::names::identifier;
use crate::out::Object;
use crate::space::Kind;
use crate::types::{
    ArrayShape, Count, Dynamic, IntegerValue, Length, Member, Order, Raw, Scalar, Shape,
    dimensions, integer_value,
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
/// element, where they go, how their fields are named, the parameter's own
/// MDL ancillary data (§4.1), and whether they're a composite's members,
/// which map to no param of their own.
struct Source<'p, 'd> {
    el: El<'d>,
    place: &'p Place,
    naming: Naming,
    mark: &'p Mark,
    unmapped: bool,
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
    /// An EntryList's entries and the fields they add, given what's
    /// already decoded in the same MDL container.
    pub(super) fn entries(
        &mut self,
        space: usize,
        list: El<'d>,
        mut known: Earlier<'d>,
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
                    known.fields.extend(new_fields.iter().cloned());
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
        known: &Earlier<'d>,
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
            Err(unresolved) => {
                self.report
                    .error(unresolved.code, reference.span, unresolved.message);
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
        let mark = self.parameter_mark(found.el);
        if array_ref {
            shape = self.array_reference(space, entry, shape)?;
        }
        let place = self.place(space, entry, known)?;
        let key = (found.space, name, Vec::new());
        let mut out = Vec::new();
        out.extend(place.padding());
        if let Some(repeat) = entry.child("RepeatEntry") {
            // A composite's members are fields of the packet, one of each.
            let type_frame = match &shape {
                Shape::Aggregate(_, type_mark) => type_mark.frame,
                Shape::Array(array) => array.mark.frame,
                Shape::Scalar(_) => None,
            };
            if let Some((_, span)) = mark.frame.or(type_frame) {
                self.report.error(
                    Code::Unsupported,
                    span,
                    format!(
                        "parameter {name} is a composite in a repeated entry, which isn't \
                         supported: a composite's members are one of each in the packet"
                    ),
                );
                return None;
            }
            // The elements are built without the parameter's own mark, so
            // its MDL.members without a frame is refused here, as it is on
            // an entry that isn't repeated.
            if let Some((_, span)) = &mark.members {
                self.report.error(
                    Code::Invalid,
                    *span,
                    format!(
                        "MDL.members orders a composite's members, and {} is no composite: \
                         MDL.frame makes it one",
                        self.describe(&key)
                    ),
                );
                return None;
            }
            let (count, offset) = self.repeat(space, repeat, known)?;
            let element = self.element(&key, found.el, &shape, offset)?;
            let array = self.array_field(&key, found.el, element, count, &place, false)?;
            out.push(array);
            return Some((out, Vec::new()));
        }
        let mut fields = Vec::new();
        let source = Source {
            el: found.el,
            place: &place,
            naming: Naming::Parameter,
            mark: &mark,
            unmapped: false,
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
        known: &Earlier<'d>,
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
            Err(unresolved) => {
                self.report
                    .error(unresolved.code, reference.span, unresolved.message);
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
                argument: None,
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
        let start = self.composites.len();
        let mut entries = self.struct_body(space, el);
        self.embedding.pop();
        // A composite's members are fields of the packet (decoding.md
        // §7.4), so one in an embedded container isn't supported.
        for composite in self.composites.split_off(start) {
            self.report.error(
                Code::Unsupported,
                composite.frame.1,
                format!(
                    "composite {} is in embedded container {name}, which isn't supported: a \
                     composite's members are fields of the packet",
                    composite.canonical.0
                ),
            );
            entries = None;
        }
        let Some(entries) = entries else {
            self.broken.insert(element);
            return None;
        };
        self.structs.insert(
            ty.clone(),
            StructType {
                name: String::new(),
                record: None,
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
        let encoding = self.container_encoding(space, el, true)?;
        let Some(list) = el.child("EntryList") else {
            self.report.error(
                Code::Invalid,
                el.span(),
                "a SequenceContainer has an EntryList",
            );
            return None;
        };
        let (mut entries, _) = self.entries(space, list, Earlier::default())?;
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
        // Its size counts from its own start, as its entries' locations do.
        self.pad_to(&mut entries, encoding.size, true)?;
        Some(entries)
    }

    /// A RepeatEntry's count and the bits between repetitions.
    fn repeat(
        &mut self,
        space: usize,
        repeat: El<'d>,
        known: &Earlier<'d>,
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
    fn place(&mut self, space: usize, entry: El<'d>, known: &Earlier<'d>) -> Option<Place> {
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
    /// A marked aggregate's members in MDL's order, x, y, z or w, x, y, z,
    /// and its param's type (§4.1): three numbers make a vec3, four a quat.
    /// `MDL.members` names them in that order; without it, it's theirs.
    fn composite_members<'m>(
        &mut self,
        key: &ParamKey<'d>,
        members: &'m [Member<'d>],
        mark: &Mark,
        frame_span: Span,
        naming: Naming,
    ) -> Option<(&'static str, Vec<&'m Member<'d>>)> {
        let described = self.describe(key);
        if let Naming::Members(_) = naming {
            self.report.error(
                Code::Unsupported,
                frame_span,
                format!(
                    "{described} is a composite in an array's elements, which isn't supported: \
                     a composite's members are one of each in the packet"
                ),
            );
            return None;
        }
        let ordered: Vec<&Member<'d>> = match &mark.members {
            None => members.iter().collect(),
            Some((names, span)) => {
                let mut ordered: Vec<&Member<'d>> = Vec::new();
                for name in names {
                    let Some(member) = members.iter().find(|member| member.name == name) else {
                        self.report.error(
                            Code::Invalid,
                            *span,
                            format!("MDL.members names {name}, which {described} has no member of"),
                        );
                        return None;
                    };
                    if ordered.iter().any(|seen| seen.name == member.name) {
                        self.report.error(
                            Code::Invalid,
                            *span,
                            format!("MDL.members names {name} twice"),
                        );
                        return None;
                    }
                    ordered.push(member);
                }
                let missing: Vec<&str> = members
                    .iter()
                    .filter(|member| !ordered.iter().any(|seen| seen.name == member.name))
                    .map(|member| member.name)
                    .collect();
                if !missing.is_empty() {
                    self.report.error(
                        Code::Invalid,
                        *span,
                        format!(
                            "MDL.members names each of {described}'s members once, and leaves \
                             out {}",
                            missing.join(", ")
                        ),
                    );
                    return None;
                }
                ordered
            }
        };
        let ty = match ordered.len() {
            3 => "vec3",
            4 => "quat",
            n => {
                self.report.error(
                    Code::Invalid,
                    frame_span,
                    format!(
                        "MDL.frame makes {described} a composite, a vec3 of 3 members or a quat \
                         of 4, and it has {n}"
                    ),
                );
                return None;
            }
        };
        for member in &ordered {
            let numeric = matches!(
                &member.shape,
                Shape::Scalar(scalar) if matches!(scalar.param, "f64" | "i64" | "u64")
            );
            if !numeric {
                self.report.error(
                    Code::Invalid,
                    frame_span,
                    format!(
                        "member {} of {described} isn't a number, so it can't be part of a \
                         composite",
                        member.name
                    ),
                );
                return None;
            }
        }
        Some((ty, ordered))
    }

    /// A marked array's elements in MDL's order, as positions from 0, and
    /// its param's type (§4.1): a fixed three numbers make a vec3, four a
    /// quat. `MDL.members` names them by the indices XTCE numbers them
    /// with, from the array's starting index.
    fn array_composite(
        &mut self,
        key: &ParamKey<'d>,
        array: &ArrayShape<'d>,
        mark: &Mark,
        frame_span: Span,
        naming: Naming,
    ) -> Option<(&'static str, Vec<u32>)> {
        let described = self.describe(key);
        if let Naming::Members(_) = naming {
            self.report.error(
                Code::Unsupported,
                frame_span,
                format!(
                    "{described} is a composite in an array's elements, which isn't supported: \
                     a composite's members are one of each in the packet"
                ),
            );
            return None;
        }
        let Some(Count::Fixed { count, start, .. }) = array.count else {
            self.report.error(
                Code::Invalid,
                frame_span,
                format!(
                    "MDL.frame makes {described} a composite, an array of a fixed 3 or 4 \
                     elements, and its count isn't fixed"
                ),
            );
            return None;
        };
        let ty = match count {
            3 => "vec3",
            4 => "quat",
            n => {
                self.report.error(
                    Code::Invalid,
                    frame_span,
                    format!(
                        "MDL.frame makes {described} a composite, a vec3 of 3 elements or a \
                         quat of 4, and it has {n}"
                    ),
                );
                return None;
            }
        };
        let numeric = matches!(
            &array.element,
            Shape::Scalar(scalar) if matches!(scalar.param, "f64" | "i64" | "u64")
        );
        if !numeric {
            self.report.error(
                Code::Invalid,
                frame_span,
                format!("{described}'s elements aren't numbers, so it can't be a composite"),
            );
            return None;
        }
        let Some((names, span)) = &mark.members else {
            return Some((ty, (0..count).collect()));
        };
        // The document's EndingIndex, which may be i64::MAX: adding the
        // count first would overflow.
        let end = start + (i64::from(count) - 1);
        let mut order: Vec<u32> = Vec::new();
        for name in names {
            let index = name
                .parse::<i64>()
                .ok()
                .filter(|index| (start..=end).contains(index));
            let Some(position) = index.and_then(|index| u32::try_from(index - start).ok()) else {
                self.report.error(
                    Code::Invalid,
                    *span,
                    format!(
                        "MDL.members names {name}, which isn't an index of {described}: its \
                         indices are {start} to {end}"
                    ),
                );
                return None;
            };
            if order.contains(&position) {
                self.report.error(
                    Code::Invalid,
                    *span,
                    format!("MDL.members names {name} twice"),
                );
                return None;
            }
            order.push(position);
        }
        if order.len() != count as usize {
            let missing: Vec<String> = (0..count)
                .filter(|position| !order.contains(position))
                .map(|position| (start + i64::from(position)).to_string())
                .collect();
            self.report.error(
                Code::Invalid,
                *span,
                format!(
                    "MDL.members names each of {described}'s indices once, and leaves out {}",
                    missing.join(", ")
                ),
            );
            return None;
        }
        Some((ty, order))
    }

    fn build(
        &mut self,
        key: &ParamKey<'d>,
        shape: &Shape<'d>,
        description: Option<(String, Span)>,
        source: &Source<'_, 'd>,
        known: &Earlier<'d>,
        fields: &mut Fields<'d>,
    ) -> Option<Vec<Entry<'d>>> {
        let Source {
            el,
            place,
            naming,
            unmapped,
            ..
        } = *source;
        let mut out = Vec::new();
        let first = |out: &Vec<Entry<'d>>| out.iter().all(|e| e.span() != Some(place.span));
        match shape {
            Shape::Scalar(scalar) => {
                let name = self.name_of(key, el, naming);
                let canonical = if unmapped {
                    None
                } else {
                    self.register(key, el, scalar, description)?
                };
                let offset = place.offset().filter(|_| first(&out));
                let seen = known.with(fields);
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
                    canonical,
                    argument: None,
                    value: None,
                    offset,
                    when: place.when.clone(),
                })));
            }
            Shape::Aggregate(members, type_mark) => {
                // A parameter's own mark speaks for its aggregate, over its
                // type's; a member aggregate's type speaks for it (§4.1).
                let mark = if key.2.is_empty() {
                    source.mark.or(type_mark)
                } else {
                    type_mark.clone()
                };
                let composite = match (mark.frame, &mark.members) {
                    (Some(frame), _) => {
                        let (ty, order) =
                            self.composite_members(key, members, &mark, frame.1, naming)?;
                        Some((ty, order, frame))
                    }
                    (None, Some((_, span))) => {
                        self.report.error(
                            Code::Invalid,
                            *span,
                            format!(
                                "MDL.members orders a composite's members, and {} is no \
                                 composite: MDL.frame makes it one",
                                self.describe(key)
                            ),
                        );
                        return None;
                    }
                    (None, None) => None,
                };
                let member_source = Source {
                    mark: &NO_MARK,
                    unmapped: unmapped || composite.is_some(),
                    ..*source
                };
                // Each member's field, by member name, for the composite.
                let mut member_fields: Vec<(&'d str, String)> = Vec::new();
                for member in members {
                    let mut member_key = key.clone();
                    member_key.2.push(member.name);
                    let seen = known.with(fields);
                    let mut added = Vec::new();
                    let built = self.build(
                        &member_key,
                        &member.shape,
                        member.description.clone(),
                        &member_source,
                        &seen,
                        &mut added,
                    )?;
                    if composite.is_some()
                        && let Some(field) = built.iter().find_map(|entry| match entry {
                            Entry::Field(field) if field.key.as_ref() == Some(&member_key) => {
                                Some(field.name.clone())
                            }
                            _ => None,
                        })
                    {
                        member_fields.push((member.name, field));
                    }
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
                if let Some((ty, order, frame)) = composite {
                    let units: Vec<Option<(String, Span)>> = order
                        .iter()
                        .map(|member| match &member.shape {
                            Shape::Scalar(scalar) => scalar.unit.clone(),
                            _ => None,
                        })
                        .collect();
                    let canonical =
                        self.register_composite(key, el, ty, frame, &units, description)?;
                    let members = order
                        .iter()
                        .filter_map(|member| {
                            member_fields
                                .iter()
                                .find(|(name, _)| *name == member.name)
                                .map(|(_, field)| (field.clone(), place.span))
                        })
                        .collect();
                    self.composites.push(Composite {
                        canonical,
                        frame,
                        members,
                    });
                }
            }
            Shape::Array(array) => {
                // As for an aggregate: the parameter's own mark over its
                // type's, and a member array's type's for it (§4.1).
                let mark = if key.2.is_empty() {
                    source.mark.or(&array.mark)
                } else {
                    array.mark.clone()
                };
                // Its count first: an XTCE 1.1 array without a size is
                // reported as any is.
                let seen = known.with(fields);
                let count = self.array_count(array, key, &seen)?;
                let composite = match (mark.frame, &mark.members) {
                    (Some(frame), _) => {
                        let (ty, order) =
                            self.array_composite(key, array, &mark, frame.1, naming)?;
                        Some((ty, order, frame))
                    }
                    (None, Some((_, span))) => {
                        self.report.error(
                            Code::Invalid,
                            *span,
                            format!(
                                "MDL.members orders a composite's members, and {} is no \
                                 composite: MDL.frame makes it one",
                                self.describe(key)
                            ),
                        );
                        return None;
                    }
                    (None, None) => None,
                };
                let element = self.element(key, el, &array.element, 0)?;
                let mut entry = self.array_field(
                    key,
                    el,
                    element,
                    count,
                    place,
                    unmapped || composite.is_some(),
                )?;
                if !first(&out) {
                    clear_offset(&mut entry);
                }
                if let (Entry::Array(field), Naming::Members(_)) = (&mut entry, naming) {
                    field.name = self.name_of(key, el, naming);
                }
                if let (Some((ty, order, frame)), Entry::Array(field)) = (composite, &entry) {
                    let unit = match &array.element {
                        Shape::Scalar(scalar) => scalar.unit.clone(),
                        _ => None,
                    };
                    let units = vec![unit; order.len()];
                    let canonical =
                        self.register_composite(key, el, ty, frame, &units, description)?;
                    let members = order
                        .iter()
                        .map(|position| (format!("{}[{position}]", field.name), place.span))
                        .collect();
                    self.composites.push(Composite {
                        canonical,
                        frame,
                        members,
                    });
                }
                out.push(entry);
            }
        }
        Some(out)
    }

    /// An array of `element`s reading `key`.
    /// An array entry. Its scalar elements map to the parameter's param,
    /// unless they're a composite's members (`unmapped`), which map to none.
    fn array_field(
        &mut self,
        key: &ParamKey<'d>,
        el: El<'d>,
        element: Element<'d>,
        count: (Value, Span),
        place: &Place,
        unmapped: bool,
    ) -> Option<Entry<'d>> {
        let canonical = match &element {
            Element::Scalar(_) if unmapped => None,
            Element::Scalar(scalar) => self.register(key, el, scalar, None)?,
            Element::Struct(_) => None,
        };
        Some(Entry::Array(Box::new(ArrayField {
            name: self.name_of(key, el, Naming::Parameter),
            key: Some(key.clone()),
            span: place.span,
            element,
            count,
            canonical,
            argument: None,
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
            mark: &NO_MARK,
            unmapped: false,
        };
        let mut entries =
            self.build(key, shape, None, &source, &Earlier::default(), &mut fields)?;
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
                record: None,
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
        known: &Earlier<'d>,
    ) -> Option<(Value, Span)> {
        match &array.count {
            Some(Count::Fixed { count, span, .. }) => {
                Some((json!({ "kind": "fixed", "count": count }), *span))
            }
            Some(Count::Counted { dynamic, start }) => {
                let linear = dynamic.count(*start);
                self.read_from(dynamic, linear, known)
            }
            None => {
                self.report.error(
                    Code::Invalid,
                    array.el.span(),
                    format!(
                        "array {} has no DimensionList, and its entry gives it no size; use \
                         an ArrayParameterRefEntry with one",
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
        known: &Earlier<'d>,
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
        known: &Earlier<'d>,
    ) -> Option<(String, u32, i64)> {
        if dynamic.argument {
            self.report.error(
                Code::Invalid,
                dynamic.span,
                "an ArgumentInstanceRef names an argument, which only a command has",
            );
            return None;
        }
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
            Err(unresolved) => {
                self.report
                    .error(unresolved.code, dynamic.span, unresolved.message);
                return None;
            }
        };
        let name = found.el.item_name()?;
        self.seen.insert((found.space, name));
        let key = (found.space, name, found.members);
        let Some(field) = known.field(&key) else {
            if known.lost(&key) {
                return None;
            }
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
pub(super) fn size_tag<'d>(
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
        signed: None,
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
        stated: None,
        initial: None,
    }
}

fn clear_offset(entry: &mut Entry<'_>) {
    match entry {
        Entry::Field(field) => field.offset = None,
        Entry::Array(array) => array.offset = None,
        Entry::Struct(inner) => inner.offset = None,
        Entry::Checksum(checksum) => checksum.offset = None,
        Entry::Padding { .. } => {}
    }
}

/// MDL's `{kind: field, field, multiply, add}`, leaving out the defaults.
pub(super) fn from_field(field: &str, multiply: u32, add: i64) -> Value {
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
        (Count::Fixed { count: x, .. }, Count::Fixed { count: y, .. }) => x == y,
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
