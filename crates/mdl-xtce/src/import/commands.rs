//! Meta-commands as catalog commands and command packets.
//!
//! A concrete MetaCommand is a command. Its arguments are those of every
//! meta-command it inherits from (its BaseMetaCommand, and so on up), root
//! first, then its own, less those an ArgumentAssignment along the way
//! gives a value. Its packet holds the entries of each one's
//! CommandContainer, root first. That is how Yamcs encodes a command: it
//! follows the BaseMetaCommand chain, an ArgumentRefEntry names an
//! argument of its own meta-command, and an assigned argument is written
//! with its assigned value, which a caller can't change.
//!
//! An assigned argument's entry, and a FixedValueEntry, are constant
//! fields, and so is a ParameterRefEntry, which takes its value from the
//! `==` ComparisonList of the sent command's container, as Yamcs fills it.
//! An argument no entry writes changes no bytes, so it isn't imported.

use std::collections::{BTreeMap, BTreeSet};

use mdl::source::Span;
use serde_json::{Value, json};

use super::arguments::{
    Leaf, Measure, SizeField, json_text, leaves, member_field, member_name, size_references,
    unknown_member,
};
use super::entries::{Location, from_field};
use super::{ArrayField, Element, Entry, Field, Import, ParamKey, StructType, TypeId, id};
use crate::Code;
use crate::calibration::duration_seconds;
use crate::names::{catalog_part, identifier};
use crate::out::integer_json;
use crate::space::Kind;
use crate::types::{
    ArrayShape, Count, Length, Order, Raw, Scalar, Shape, Wanted, description, raw_value,
};
use crate::xml::{self, El, Expect};

/// A command, once worked out.
pub(super) struct Command<'d> {
    /// Its packet's MDL name, worked out once every command is known.
    pub(super) name: String,
    space: usize,
    pub(super) el: El<'d>,
    /// Its catalog name, and where it came from.
    pub(super) canonical: (String, Span),
    pub(super) description: Option<(String, Span)>,
    pub(super) arguments: Vec<Argument<'d>>,
    pub(super) entries: Vec<Entry<'d>>,
}

/// A catalog argument.
pub(super) struct Argument<'d> {
    pub(super) name: (String, Span),
    pub(super) el: El<'d>,
    pub(super) scalar: Scalar<'d>,
    pub(super) default: Option<(Value, Span)>,
    pub(super) description: Option<(String, Span)>,
    /// A list argument's bounds.
    pub(super) list: Option<Bounds>,
}

/// A list's least and greatest number of values; an absent end is open.
pub(super) type Bounds = (Option<u32>, Option<u32>);

/// An argument of a meta-command in a command's chain.
struct Declared<'d> {
    /// Where in the chain the meta-command that declares it is.
    owner: usize,
    el: El<'d>,
    xtce: &'d str,
    /// Its catalog name; `None` after reporting why there's none.
    name: Option<(String, Span)>,
    /// `None` after reporting why its type can't be read.
    shape: Option<Shape<'d>>,
    /// The value an ArgumentAssignment gives it, where that value is, and
    /// where the assignment is.
    assigned: Option<(&'d str, Span, Span)>,
    /// Whether an entry writes it.
    written: bool,
    /// What it's the size of, when another argument's size is its value.
    size: Option<Measure>,
    /// A list argument's bounds, once its array is written.
    list: Option<Bounds>,
}

/// A command's chain: each meta-command, root first, with its SpaceSystem.
type Chain<'d> = [(usize, El<'d>)];

/// A value the sent command's criteria give a parameter or member: the
/// value as written, whether it's the calibrated value, and where it is.
#[derive(Clone, Copy)]
struct Given<'d> {
    value: &'d str,
    calibrated: bool,
    span: Span,
}

/// The values the sent command's criteria give, by parameter or member.
type Values<'d> = BTreeMap<ParamKey<'d>, Given<'d>>;

const META_COMMAND: Expect<'static> = Expect {
    attrs: &["name", "abstract", "shortDescription"],
    children: &[
        "BaseMetaCommand",
        "ArgumentList",
        "CommandContainer",
        "LongDescription",
        "AliasSet",
    ],
    ignored: &[
        "AncillaryDataSet",
        "SystemName",
        "TransmissionConstraintList",
        "DefaultSignificance",
        "ContextSignificanceList",
        "Interlock",
        "VerifierSet",
        "ParameterToSetList",
        "ParametersToSuspendAlarmsOnSet",
    ],
    text: false,
};

const ARGUMENT: Expect<'static> = Expect {
    attrs: &[
        "name",
        "argumentTypeRef",
        "initialValue",
        "shortDescription",
    ],
    children: &["LongDescription", "AliasSet"],
    ignored: &["AncillaryDataSet"],
    text: false,
};

const COMMAND_CONTAINER: Expect<'static> = Expect {
    attrs: &["name"],
    children: &["EntryList", "BaseContainer", "BinaryEncoding"],
    ignored: &[
        "shortDescription",
        "LongDescription",
        "AliasSet",
        "AncillaryDataSet",
        "DefaultRateInStream",
        "RateInStreamSet",
    ],
    text: false,
};

const COMMAND_ENTRIES: &[&str] = &[
    "ParameterRefEntry",
    "ParameterSegmentRefEntry",
    "ContainerRefEntry",
    "ContainerSegmentRefEntry",
    "StreamSegmentEntry",
    "IndirectParameterRefEntry",
    "ArrayParameterRefEntry",
    "ArgumentRefEntry",
    "ArrayArgumentRefEntry",
    "FixedValueEntry",
];

/// What an argument or fixed value entry holds besides its own attributes.
/// A time association means nothing for a value a command writes.
const ENTRY_CHILDREN: &[&str] = &[
    "LocationInContainerInBits",
    "IncludeCondition",
    "RepeatEntry",
];
const ENTRY_IGNORED: &[&str] = &["shortDescription", "AncillaryDataSet", "TimeAssociation"];

impl<'d> Import<'d, '_> {
    /// Every concrete meta-command, in document order, its packet named.
    pub(super) fn commands(&mut self) -> Vec<Command<'d>> {
        let tree = self.tree;
        let mut commands = Vec::new();
        for (space, el) in tree.items(Kind::MetaCommand) {
            let concrete = match el.attr("abstract") {
                Some(attr) => !xml::boolean(attr, self.report).unwrap_or(true),
                None => true,
            };
            if concrete && let Some(command) = self.command(space, el) {
                commands.push(command);
            }
        }
        self.name_commands(&mut commands);
        commands
    }

    fn command(&mut self, space: usize, el: El<'d>) -> Option<Command<'d>> {
        let chain = self.command_chain(space, el)?;
        let mut declared: Vec<Declared<'d>> = Vec::new();
        let mut complete = true;
        for (index, (at_space, meta)) in chain.iter().enumerate() {
            meta.check(&META_COMMAND, self.report);
            // A BaseMetaCommand assigns arguments of the meta-commands
            // above, before this one declares its own.
            complete &= self.assign(*at_space, *meta, &mut declared).is_some();
            complete &= self
                .declare(index, *at_space, *meta, &mut declared)
                .is_some();
        }
        let (sized, unread) = self.mark_sizes(&mut declared);
        complete &= sized;
        // A size reported as unreadable stands in, so what it measures isn't
        // reported again.
        let mut sizes: Vec<(&'d str, SizeField)> = unread
            .into_iter()
            .map(|name| {
                let field = SizeField {
                    name: name.to_string(),
                    assigned: None,
                    raw: (0, 0),
                };
                (name, field)
            })
            .collect();
        let (values, read_whole) = self.criteria_values(&chain);
        complete &= read_whole;
        let mut entries = Vec::new();
        let mut unnamed = 0;
        for index in 0..chain.len() {
            let (at_space, meta) = chain[index];
            let Some(container) = meta.child("CommandContainer") else {
                continue;
            };
            let owner = Owner {
                index,
                space: at_space,
                meta,
                last: index + 1 == chain.len(),
            };
            let read = Read {
                chain: &chain,
                values: &values,
                read_whole,
                unnamed: &mut unnamed,
                sizes: &mut sizes,
            };
            match self.command_entries(&owner, container, &mut declared, read) {
                Some(added) => entries.extend(added),
                None => complete = false,
            }
        }
        complete &= self.check_measured(&declared).is_some();
        if !complete {
            return None;
        }
        self.check_entries(&entries, "command packet")?;
        let arguments = self.catalog_arguments(&chain, &declared)?;
        let canonical = self.base_name(space, el)?;
        Some(Command {
            name: String::new(),
            space,
            el,
            canonical,
            description: description(el),
            arguments,
            entries,
        })
    }

    /// Each size argument an entry writes measures a value an entry writes,
    /// else MDL has nothing to work it out from, and a length one value
    /// only, since an MDL field gives one value its length.
    fn check_measured(&mut self, declared: &[Declared<'d>]) -> Option<()> {
        let mut complete = true;
        for size in declared.iter().filter(|d| d.size.is_some() && d.written) {
            if size.assigned.is_some() {
                continue;
            }
            let uses: Vec<Measure> = declared
                .iter()
                .filter(|d| d.written)
                .filter_map(|d| d.shape.as_ref())
                .flat_map(size_references)
                .filter(|(dynamic, _)| dynamic.reference == size.xtce)
                .map(|(_, measure)| measure)
                .collect();
            if uses.iter().filter(|m| **m == Measure::Length).count() > 1 {
                self.report.error(
                    Code::Unsupported,
                    size.el.span(),
                    format!(
                        "argument {} gives the length of two values, and an MDL field gives one \
                         value its length",
                        size.xtce
                    ),
                );
                complete = false;
            }
            let measured = !uses.is_empty();
            if !measured {
                self.report.error(
                    Code::Unsupported,
                    size.el.span(),
                    format!(
                        "argument {} gives the size of an argument no entry writes, so MDL has \
                         nothing to work it out from",
                        size.xtce
                    ),
                );
                complete = false;
            }
        }
        complete.then_some(())
    }

    /// The meta-command and those it inherits from, root first.
    fn command_chain(&mut self, space: usize, el: El<'d>) -> Option<Vec<(usize, El<'d>)>> {
        let mut chain = vec![(space, el)];
        let mut seen = BTreeSet::from([id(&el)]);
        let (mut at_space, mut at) = (space, el);
        while let Some(base) = at.child("BaseMetaCommand") {
            let reference = base.required("metaCommandRef", self.report)?;
            let found = match self
                .tree
                .resolve(at_space, reference.value, Kind::MetaCommand)
            {
                Ok(found) => found,
                Err(message) => {
                    self.report.error(Code::Reference, reference.span, message);
                    return None;
                }
            };
            if !seen.insert(id(&found.el)) {
                self.report.error(
                    Code::Invalid,
                    reference.span,
                    format!(
                        "meta-command {} inherits from itself",
                        found.el.item_name().unwrap_or_default()
                    ),
                );
                return None;
            }
            chain.push((found.space, found.el));
            (at_space, at) = (found.space, found.el);
        }
        chain.reverse();
        Some(chain)
    }

    /// The ArgumentAssignments of a meta-command's BaseMetaCommand: each
    /// gives an argument of the meta-commands above a value.
    fn assign(&mut self, space: usize, meta: El<'d>, declared: &mut [Declared<'d>]) -> Option<()> {
        let Some(base) = meta.child("BaseMetaCommand") else {
            return Some(());
        };
        base.check(
            &Expect {
                attrs: &["metaCommandRef"],
                children: &["ArgumentAssignmentList"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let Some(list) = base.child("ArgumentAssignmentList") else {
            return Some(());
        };
        list.check(
            &Expect {
                attrs: &[],
                children: &["ArgumentAssignment"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let mut complete = true;
        for assignment in list.all("ArgumentAssignment") {
            assignment.check(
                &Expect {
                    attrs: &["argumentName", "argumentValue"],
                    children: &[],
                    ignored: &[],
                    text: false,
                },
                self.report,
            );
            let (Some(name), Some(value)) = (
                assignment.required("argumentName", self.report),
                assignment.required("argumentValue", self.report),
            ) else {
                complete = false;
                continue;
            };
            match declared.iter_mut().find(|d| d.xtce == name.value) {
                None => {
                    self.report.error(
                        Code::Reference,
                        name.span,
                        format!(
                            "{} names no argument that meta-command {}/{} inherits",
                            name.value,
                            self.tree.path(space),
                            meta.item_name().unwrap_or_default()
                        ),
                    );
                    complete = false;
                }
                Some(argument) => match argument.assigned {
                    Some((_, _, first)) => {
                        self.report.duplicate(
                            Code::Invalid,
                            assignment.span(),
                            first,
                            format!("argument {} is given a value already", name.value),
                        );
                        complete = false;
                    }
                    None => argument.assigned = Some((value.value, value.span, assignment.span())),
                },
            }
        }
        complete.then_some(())
    }

    /// A meta-command's own arguments, after those it inherits.
    fn declare(
        &mut self,
        owner: usize,
        space: usize,
        meta: El<'d>,
        declared: &mut Vec<Declared<'d>>,
    ) -> Option<()> {
        let Some(list) = meta.child("ArgumentList") else {
            return Some(());
        };
        list.check(
            &Expect {
                attrs: &[],
                children: &["Argument"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let meta_path = format!(
            "{}/{}",
            self.tree.path(space),
            meta.item_name().unwrap_or_default()
        );
        let mut complete = true;
        for el in list.all("Argument") {
            el.check(&ARGUMENT, self.report);
            let Some(xtce) = el.required("name", self.report) else {
                complete = false;
                continue;
            };
            if let Some(first) = declared.iter().find(|d| d.xtce == xtce.value) {
                let message = if first.owner == owner {
                    format!("{meta_path} declares argument {} twice", xtce.value)
                } else {
                    format!(
                        "{meta_path} declares argument {}, which it inherits already",
                        xtce.value
                    )
                };
                self.report
                    .duplicate(Code::Duplicate, el.span(), first.el.span(), message);
                complete = false;
                continue;
            }
            let typed = self.argument_type(space, el);
            let name = self.argument_name(el, xtce.value);
            complete &= typed.is_some() && name.is_some();
            let shape = typed.map(|(shape, _)| shape);
            declared.push(Declared {
                owner,
                el,
                xtce: xtce.value,
                name,
                shape,
                assigned: None,
                written: false,
                size: None,
                list: None,
            });
        }
        complete.then_some(())
    }

    /// An argument's type: its shape, and its element.
    fn argument_type(&mut self, space: usize, el: El<'d>) -> Option<(Shape<'d>, El<'d>)> {
        let type_ref = el.required("argumentTypeRef", self.report)?;
        let found = match self.tree.resolve(space, type_ref.value, Kind::ArgumentType) {
            Ok(found) => found,
            Err(message) => {
                self.report.error(Code::Reference, type_ref.span, message);
                return None;
            }
        };
        let shape = self
            .types
            .shape(self.tree, found.space, found.el, self.report)?;
        Some((shape, found.el))
    }

    /// Marks each argument whose value another's size is, and what it
    /// measures. A size reads an argument of the command, by name. The
    /// flag is false after reporting a size that can't be read, and the
    /// names are those sizes'.
    fn mark_sizes(&mut self, declared: &mut [Declared<'d>]) -> (bool, Vec<&'d str>) {
        let mut complete = true;
        let mut unread = Vec::new();
        let mut uses: Vec<(usize, Measure, Span)> = Vec::new();
        for argument in declared.iter() {
            let Some(shape) = &argument.shape else {
                continue;
            };
            for (dynamic, measure) in size_references(shape) {
                if !dynamic.argument {
                    self.report.error(
                        Code::Unsupported,
                        dynamic.span,
                        "a size a parameter gives isn't supported in a command: MDL works a \
                         command's sizes out from its arguments, and Yamcs refuses a parameter \
                         here too",
                    );
                    complete = false;
                    unread.push(dynamic.reference);
                    continue;
                }
                match declared.iter().position(|o| o.xtce == dynamic.reference) {
                    Some(index) => uses.push((index, measure, dynamic.span)),
                    None => {
                        self.report.error(
                            Code::Reference,
                            dynamic.span,
                            format!("{} names no argument of this command", dynamic.reference),
                        );
                        complete = false;
                        unread.push(dynamic.reference);
                    }
                }
            }
        }
        for (index, measure, span) in uses {
            let xtce = declared[index].xtce;
            if !matches!(declared[index].shape, Some(Shape::Scalar(_)) | None) {
                self.report.error(
                    Code::Unsupported,
                    span,
                    format!(
                        "argument {xtce} gives a size, and MDL takes a size from one unsigned \
                         integer field, not an aggregate's or a list's"
                    ),
                );
                complete = false;
                unread.push(xtce);
                continue;
            }
            match declared[index].size {
                Some(other) if other != measure => {
                    self.report.error(
                        Code::Unsupported,
                        span,
                        format!(
                            "argument {} gives both a count and a length, and an MDL field holds \
                             one",
                            declared[index].xtce
                        ),
                    );
                    complete = false;
                }
                _ => declared[index].size = Some(measure),
            }
        }
        (complete, unread)
    }

    /// An argument's catalog name: its alias in the MDL namespace, else its
    /// name as a catalog name part.
    fn argument_name(&mut self, el: El<'d>, xtce: &str) -> Option<(String, Span)> {
        if let Some(alias) = self.mdl_alias(el)? {
            return Some(alias);
        }
        let span = el.attr("name").map_or(el.span(), |a| a.span);
        match catalog_part(xtce) {
            Some(name) => Some((name, span)),
            None => {
                self.report.error(
                    Code::Name,
                    span,
                    format!(
                        "argument {xtce} has no letters or digits for a catalog name; give it an \
                         alias in the MDL namespace"
                    ),
                );
                None
            }
        }
    }

    /// The values the sent command's criteria give parameters: those of an
    /// `==` ComparisonList on its container's BaseContainer, the only ones
    /// Yamcs fills parameter entries from. Its other criteria aren't read.
    /// The flag is false after reporting a comparison that can't be read.
    fn criteria_values(&mut self, chain: &Chain<'d>) -> (Values<'d>, bool) {
        let mut values = Values::new();
        let Some(&(space, meta)) = chain.last() else {
            return (values, true);
        };
        let criteria = meta
            .child("CommandContainer")
            .and_then(|c| c.child("BaseContainer"))
            .and_then(|b| b.child("RestrictionCriteria"));
        let Some(criteria) = criteria else {
            return (values, true);
        };
        criteria.check(
            &Expect {
                attrs: &[],
                children: &[
                    "ComparisonList",
                    "Comparison",
                    "BooleanExpression",
                    "CustomAlgorithm",
                    "NextContainer",
                ],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        for other in criteria.children().filter(|c| c.name() != "ComparisonList") {
            self.report.warning(
                Code::Ignored,
                other.span(),
                format!(
                    "a command container's {} isn't read: Yamcs fills parameter entries from \
                     a ComparisonList only",
                    other.name()
                ),
            );
        }
        let Some(list) = criteria.child("ComparisonList") else {
            return (values, true);
        };
        list.check(
            &Expect {
                attrs: &[],
                children: &["Comparison"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let mut complete = true;
        for comparison in list.all("Comparison") {
            comparison.check(
                &Expect {
                    attrs: &[
                        "parameterRef",
                        "value",
                        "comparisonOperator",
                        "useCalibratedValue",
                        "instance",
                    ],
                    children: &[],
                    ignored: &[],
                    text: false,
                },
                self.report,
            );
            let (Some(reference), Some(value)) = (
                comparison.required("parameterRef", self.report),
                comparison.required("value", self.report),
            ) else {
                complete = false;
                continue;
            };
            if let Some(operator) = comparison.attr("comparisonOperator")
                && operator.value.trim() != "=="
            {
                self.report.warning(
                    Code::Ignored,
                    operator.span,
                    "a comparison that isn't == isn't read: Yamcs fills parameter entries from \
                     == comparisons only",
                );
                continue;
            }
            if let Some(instance) = comparison.attr("instance")
                && xml::long(instance, self.report) != Some(0)
            {
                self.report.error(
                    Code::Unsupported,
                    instance.span,
                    "a comparison with an instance other than 0 isn't supported",
                );
                complete = false;
                continue;
            }
            let calibrated = match comparison.attr("useCalibratedValue") {
                Some(attr) => {
                    let calibrated = xml::boolean(attr, self.report).unwrap_or(true);
                    if !calibrated {
                        self.report.warning(
                            Code::Differs,
                            attr.span,
                            "Yamcs reads a command's criteria value as an engineering value \
                             whatever useCalibratedValue says; MDL reads this one as the raw \
                             value, as XTCE says",
                        );
                    }
                    calibrated
                }
                None => true,
            };
            let found = match self.tree.resolve(space, reference.value, Kind::Parameter) {
                Ok(found) => found,
                Err(message) => {
                    self.report.error(Code::Reference, reference.span, message);
                    complete = false;
                    continue;
                }
            };
            let Some(name) = found.el.item_name() else {
                complete = false;
                continue;
            };
            self.seen.insert((found.space, name));
            let key = (found.space, name, found.members);
            let given = Given {
                value: value.value,
                calibrated,
                span: value.span,
            };
            match values.get(&key) {
                Some(first) if first.value != given.value => {
                    self.report.duplicate(
                        Code::Invalid,
                        value.span,
                        first.span,
                        format!("the criteria give {} two values", reference.value),
                    );
                    complete = false;
                }
                Some(_) => {}
                None => {
                    values.insert(key, given);
                }
            }
        }
        (values, complete)
    }

    /// The entries a meta-command's CommandContainer adds.
    fn command_entries(
        &mut self,
        owner: &Owner<'d>,
        container: El<'d>,
        declared: &mut [Declared<'d>],
        read: Read<'_, 'd>,
    ) -> Option<Vec<Entry<'d>>> {
        let Read {
            chain,
            values,
            read_whole,
            unnamed,
            sizes,
        } = read;
        container.check(&COMMAND_CONTAINER, self.report);
        if let Some(size) = container.child("BinaryEncoding") {
            self.report.error(
                Code::Unsupported,
                size.span(),
                "a container's own size (BinaryEncoding) isn't supported yet",
            );
            return None;
        }
        self.command_base(owner, chain, container)?;
        let Some(list) = container.child("EntryList") else {
            self.report.error(
                Code::Invalid,
                container.span(),
                "a CommandContainer has an EntryList",
            );
            return None;
        };
        list.check(
            &Expect {
                attrs: &[],
                children: COMMAND_ENTRIES,
                ignored: &[],
                text: false,
            },
            self.report,
        );
        let mut entries = Vec::new();
        let mut complete = true;
        for entry in list.children() {
            let added = match entry.name() {
                "ArgumentRefEntry" | "ArrayArgumentRefEntry" => {
                    self.argument_entry(owner, entry, declared, sizes)
                }
                "FixedValueEntry" => self.fixed_entry(entry, unnamed),
                "ParameterRefEntry" => self.parameter_entry_value(owner, entry, values, read_whole),
                "ArrayParameterRefEntry" => {
                    self.report.error(
                        Code::Unsupported,
                        entry.span(),
                        "an array parameter in a command isn't supported",
                    );
                    None
                }
                name @ ("ContainerRefEntry"
                | "ParameterSegmentRefEntry"
                | "ContainerSegmentRefEntry"
                | "StreamSegmentEntry"
                | "IndirectParameterRefEntry") => {
                    self.report.error(
                        Code::Unsupported,
                        entry.span(),
                        format!(
                            "{name} in a command isn't supported: an MDL command packet holds \
                             arguments and constants"
                        ),
                    );
                    None
                }
                _ => continue,
            };
            match added {
                Some(added) => entries.extend(added),
                None => complete = false,
            }
        }
        complete.then_some(entries)
    }

    /// A command container built on another is built on the container of
    /// the meta-command its meta-command inherits, which is what Yamcs
    /// encodes ahead of it.
    fn command_base(
        &mut self,
        owner: &Owner<'d>,
        chain: &Chain<'d>,
        container: El<'d>,
    ) -> Option<()> {
        let Some(base) = container.child("BaseContainer") else {
            return Some(());
        };
        base.check(
            &Expect {
                attrs: &["containerRef"],
                children: &["RestrictionCriteria"],
                ignored: &[],
                text: false,
            },
            self.report,
        );
        // The sent command's criteria are read with its parameter entries;
        // Yamcs reads no others, so those of a meta-command never sent mean
        // nothing.
        if let Some(criteria) = base.child("RestrictionCriteria")
            && !owner.last
            && owner
                .meta
                .attr("abstract")
                .is_some_and(|a| xml::boolean(a, self.report) == Some(true))
        {
            self.report.warning(
                Code::Ignored,
                criteria.span(),
                format!(
                    "these criteria aren't read: Yamcs reads only the sent command's, and \
                     meta-command {} is abstract",
                    owner.meta.item_name().unwrap_or_default()
                ),
            );
        }
        let reference = base.required("containerRef", self.report)?;
        let named = match self
            .tree
            .resolve(owner.space, reference.value, Kind::CommandContainer)
        {
            Ok(found) => found.el,
            Err(message) => {
                if self
                    .tree
                    .resolve(owner.space, reference.value, Kind::Container)
                    .is_ok()
                {
                    self.report.error(
                        Code::Unsupported,
                        reference.span,
                        "a command container built on a telemetry container isn't supported",
                    );
                } else {
                    self.report.error(Code::Reference, reference.span, message);
                }
                return None;
            }
        };
        let inherited = chain[..owner.index]
            .iter()
            .rev()
            .find_map(|(_, meta)| meta.child("CommandContainer").map(|c| (*meta, c)));
        let Some((meta, inherited)) = inherited else {
            self.report.error(
                Code::Unsupported,
                reference.span,
                "a command container built on another container is supported only when its \
                 meta-command inherits that container's meta-command (BaseMetaCommand), since \
                 Yamcs encodes the meta-commands a command inherits",
            );
            return None;
        };
        if id(&named) != id(&inherited) {
            let name = inherited.item_name().unwrap_or_default();
            self.report.error(
                Code::Unsupported,
                reference.span,
                format!(
                    "this container is built on {}, but its meta-command inherits {}, whose \
                     container {name} Yamcs encodes ahead of it; build it on {name}",
                    reference.value,
                    meta.item_name().unwrap_or_default()
                ),
            );
            return None;
        }
        Some(())
    }

    /// An ArgumentRefEntry (or ArrayArgumentRefEntry): the fields writing
    /// the argument (one, or one per member of an aggregate), each encoding
    /// it or holding the value assigned to it, or the array writing a list
    /// argument.
    fn argument_entry(
        &mut self,
        owner: &Owner<'d>,
        entry: El<'d>,
        declared: &mut [Declared<'d>],
        sizes: &mut Vec<(&'d str, SizeField)>,
    ) -> Option<Vec<Entry<'d>>> {
        let array_entry = entry.name() == "ArrayArgumentRefEntry";
        let mut children = ENTRY_CHILDREN.to_vec();
        let mut attrs = vec!["argumentRef"];
        if array_entry {
            children.push("DimensionList");
            attrs.push("lastEntryForThisArrayInstance");
        }
        entry.check(
            &Expect {
                attrs: &attrs,
                children: &children,
                ignored: ENTRY_IGNORED,
                text: false,
            },
            self.report,
        );
        if let Some(list) = entry.child("DimensionList") {
            self.report.error(
                Code::Unsupported,
                list.span(),
                "a subset of an array argument isn't supported: MDL writes whole lists",
            );
            return None;
        }
        if array_entry {
            // Without a subset, the entry writes the whole array whether it's
            // marked the last for it or not.
            if let Some(last) = entry.attr("lastEntryForThisArrayInstance") {
                xml::boolean(last, self.report)?;
            }
            self.report.warning(
                Code::Differs,
                entry.span(),
                "Yamcs skips an ArrayArgumentRefEntry and writes nothing for it; MDL writes the \
                 array, as XTCE says",
            );
        }
        let location = self.command_location(entry)?;
        let reference = entry.required("argumentRef", self.report)?;
        let meta = owner.meta.item_name().unwrap_or_default();
        let meta_path = format!("{}/{meta}", self.tree.path(owner.space));
        let found = declared
            .iter()
            .position(|d| d.owner == owner.index && d.xtce == reference.value);
        let Some(index) = found else {
            let message = format!(
                "{} names no argument of meta-command {meta_path}; an ArgumentRefEntry names an \
                 argument its own meta-command declares, as Yamcs reads it",
                reference.value
            );
            self.report.error(Code::Reference, reference.span, message);
            return None;
        };
        if array_entry && !matches!(declared[index].shape, Some(Shape::Array(_)) | None) {
            self.report.error(
                Code::Invalid,
                entry.span(),
                "an ArrayArgumentRefEntry names an array argument",
            );
            return None;
        }
        if declared[index].written && declared[index].assigned.is_none() {
            self.report.error(
                Code::Unsupported,
                entry.span(),
                format!(
                    "argument {} is in this container twice, and MDL encodes an argument from \
                     one field",
                    reference.value
                ),
            );
            return None;
        }
        declared[index].written = true;
        let argument = &declared[index];
        let shape = argument.shape.clone()?;
        let xtce = argument.xtce;
        let (field, renamed) = identifier(xtce);
        if renamed {
            self.report.warning(
                Code::Renamed,
                entry.span(),
                format!(
                    "argument {xtce} of meta-command {meta_path} is field {field}: MDL names are \
                     letters, digits and '_'"
                ),
            );
        }
        let written = Written {
            xtce,
            meta,
            field,
            catalog: argument.name.clone(),
            assigned: argument.assigned,
            size: argument.size.is_some(),
            el: argument.el,
            entry: entry.span(),
            reference: reference.span,
        };
        let mut out = Vec::new();
        out.extend(location.padding());
        let mut built = match &shape {
            Shape::Array(array) => {
                let (array, bounds) = self.array_argument(&written, array, owner.space, sizes)?;
                declared[index].list = Some(bounds);
                vec![array]
            }
            _ => self.argument_fields(&written, &shape, sizes)?,
        };
        if let Some(first) = built.first_mut() {
            place(first, location.offset());
        }
        out.append(&mut built);
        Some(out)
    }

    /// The fields writing a scalar or aggregate argument: one per value it
    /// takes, named for the argument and its members.
    fn argument_fields(
        &mut self,
        written: &Written<'d>,
        shape: &Shape<'d>,
        sizes: &mut Vec<(&'d str, SizeField)>,
    ) -> Option<Vec<Entry<'d>>> {
        let Some(leaves) = leaves(shape) else {
            self.report.error(
                Code::Unsupported,
                written.entry,
                "an array inside an aggregate argument isn't supported: MDL lists are \
                 arguments of their own",
            );
            return None;
        };
        let aggregate = matches!(shape, Shape::Aggregate(_));
        // An assigned aggregate is a JSON object of its members.
        let assigned = match (written.assigned, aggregate) {
            (Some((text, span, _)), true) => {
                self.check_member_initials(written.xtce, &leaves)?;
                Some((self.json_of(text, span, written.xtce, &leaves)?, span))
            }
            _ => None,
        };
        let mut out = Vec::new();
        for leaf in &leaves {
            let shown = std::iter::once(written.xtce)
                .chain(leaf.path.iter().copied())
                .collect::<Vec<_>>()
                .join(".");
            let value = match (written.assigned, &assigned) {
                (_, Some((json, span))) => {
                    match self.member_value(leaf, &leaves, written.xtce, Some((json, *span))) {
                        Ok(Some(value)) => Some(value),
                        Ok(None) => {
                            self.report.error(
                                Code::Invalid,
                                *span,
                                format!(
                                    "the value assigned to argument {} gives member {} no value, \
                                     and it has no initial value",
                                    written.xtce,
                                    leaf.shown()
                                ),
                            );
                            return None;
                        }
                        Err(()) => return None,
                    }
                }
                (Some((text, span, _)), None) => Some((text.to_string(), span)),
                (None, _) => None,
            };
            let catalog = match &written.catalog {
                Some((base, _)) => member_name(base, &leaf.path),
                None => None,
            };
            let (name, renamed) = member_field(&written.field, &leaf.path);
            if renamed {
                self.report.warning(
                    Code::Renamed,
                    written.entry,
                    format!(
                        "member {} of argument {} is field {name}: MDL names are letters, digits \
                         and '_'",
                        leaf.shown(),
                        written.xtce
                    ),
                );
            }
            let field = LeafField {
                name,
                scalar: leaf.scalar.clone(),
                catalog: catalog.map(|name| (name, written.reference)),
                value,
                size_of: written.size.then_some(written.xtce),
                span: written.entry,
                shown,
            };
            out.extend(self.leaf_field(field, sizes)?);
        }
        Some(out)
    }

    /// The field writing one value: encoding a catalog argument, holding an
    /// assigned value, or, for a size argument, worked out from what it
    /// measures.
    fn leaf_field(
        &mut self,
        leaf: LeafField<'d>,
        sizes: &mut Vec<(&'d str, SizeField)>,
    ) -> Option<Vec<Entry<'d>>> {
        let mut scalar = leaf.scalar;
        let (argument, value) = match leaf.value {
            Some((text, span)) => {
                let value = self.assigned_value(&scalar, &leaf.shown, &text, span)?;
                (None, Some((value, span)))
            }
            None if leaf.size_of.is_some() => (None, None),
            None => (Some(leaf.catalog?), None),
        };
        if value.is_some() || leaf.size_of.is_some() {
            // A constant or a size is written as its raw value.
            scalar.conversion = None;
            scalar.inverse = None;
            scalar.unit = None;
        }
        if let Some(xtce) = leaf.size_of {
            let raw = self.check_size_field(&scalar, xtce, leaf.span);
            let assigned = value.as_ref().and_then(|(value, _)| match value {
                Value::Number(n) => n.as_i64().map(i128::from),
                Value::String(text) => text.parse().ok(),
                _ => None,
            });
            // Kept even when it can't be a size (reported), so what it
            // measures isn't reported as having none.
            sizes.push((
                xtce,
                SizeField {
                    name: leaf.name.clone(),
                    assigned,
                    raw: raw.unwrap_or((0, 0)),
                },
            ));
            raw?;
        }
        let field = Field {
            name: leaf.name,
            key: None,
            span: leaf.span,
            scalar,
            length: None,
            canonical: None,
            argument,
            value,
            offset: None,
            when: None,
        };
        self.length_entries(field, sizes)
    }

    /// The array writing a list argument, and the list's bounds: its count
    /// fixed or from a size argument's field, its elements scalars, or for
    /// an aggregate, records taking one list per member.
    fn array_argument(
        &mut self,
        written: &Written<'d>,
        array: &ArrayShape<'d>,
        space: usize,
        sizes: &[(&'d str, SizeField)],
    ) -> Option<(Entry<'d>, Bounds)> {
        if let Some((_, span, _)) = written.assigned {
            self.report.error(
                Code::Unsupported,
                span,
                "a value assigned to an array argument isn't supported: MDL constants are single \
                 values",
            );
            return None;
        }
        let name = written.field.clone();
        let (count, bounds) = self.list_count(array, &name, sizes)?;
        let base = written.catalog.clone()?.0;
        let (element, argument) = match &array.element {
            Shape::Scalar(scalar) => {
                let unsupported = match scalar.length {
                    None | Some(Length::Written(..)) => None,
                    Some(Length::Leading { .. }) => Some(
                        "array elements with a leading size aren't supported: MDL elements have \
                         sizes of their own",
                    ),
                    Some(Length::Counted(_)) => Some(
                        "array elements whose size another value gives aren't supported: MDL \
                         elements have sizes of their own",
                    ),
                };
                if let Some(message) = unsupported {
                    self.report
                        .error(Code::Unsupported, scalar.el.span(), message);
                    return None;
                }
                (
                    Element::Scalar(scalar.clone()),
                    Some((base, written.reference)),
                )
            }
            shape => {
                let Some(leaves) = leaves(shape) else {
                    self.report.error(
                        Code::Unsupported,
                        array.el.span(),
                        "an array of arrays (a multidimensional array) isn't supported: MDL \
                         lists have one dimension",
                    );
                    return None;
                };
                let ty = self.record_type(written, &leaves, &base, space)?;
                (Element::Struct(ty), None)
            }
        };
        let array = ArrayField {
            name,
            key: None,
            span: written.entry,
            element,
            count,
            canonical: None,
            argument,
            offset: None,
            when: None,
        };
        Some((Entry::Array(Box::new(array)), bounds))
    }

    /// An array argument's count and its list's bounds: fixed, or from the
    /// field of the argument giving its size (fixed after all when that's
    /// assigned).
    fn list_count(
        &mut self,
        array: &ArrayShape<'d>,
        name: &str,
        sizes: &[(&'d str, SizeField)],
    ) -> Option<((Value, Span), Bounds)> {
        let fixed = |n: u32, span: Span| {
            let bounds = (Some(n), Some(n));
            Some(((json!({ "kind": "fixed", "count": n }), span), bounds))
        };
        match &array.count {
            Some(Count::Fixed(n, span)) => fixed(*n, *span),
            Some(Count::Counted { dynamic, start }) => {
                let (multiply, add) = match dynamic.count(*start) {
                    Ok(linear) => linear,
                    Err(reason) => {
                        self.report.error(Code::Unsupported, dynamic.span, reason);
                        return None;
                    }
                };
                let measured = format!("array {name}");
                let field = self.written_size(dynamic, sizes, &measured)?.clone();
                let count = |raw: i128| raw * i128::from(multiply) + i128::from(add);
                if let Some(value) = field.assigned {
                    let Ok(n) = u32::try_from(count(value)) else {
                        self.report.error(
                            Code::Invalid,
                            dynamic.span,
                            format!(
                                "argument {} gives {measured} {} values",
                                dynamic.reference,
                                count(value)
                            ),
                        );
                        return None;
                    };
                    return fixed(n, dynamic.span);
                }
                // No packet holds more values than a u32 counts, so a list's
                // bounds stop there however much the field holds.
                let max = u32::try_from(count(field.raw.1)).unwrap_or(u32::MAX);
                let min = u32::try_from(count(field.raw.0).max(0)).ok();
                let value = from_field(&field.name, multiply, add);
                Some(((value, dynamic.span), (min, Some(max))))
            }
            None => {
                self.report.error(
                    Code::Unsupported,
                    array.el.span(),
                    format!(
                        "array argument type {} has no DimensionList, and MDL takes an array \
                         argument's size from its type",
                        array.el.item_name().unwrap_or_default()
                    ),
                );
                None
            }
        }
    }

    /// The struct type of an array argument's records: a field per member,
    /// each taking its own list argument, named for the field with
    /// `_element`.
    fn record_type(
        &mut self,
        written: &Written<'d>,
        leaves: &[Leaf<'d>],
        base: &str,
        space: usize,
    ) -> Option<TypeId<'d>> {
        let ty = TypeId::Record(id(&written.el));
        if self.structs.contains_key(&ty) {
            return Some(ty);
        }
        let meta = identifier(written.meta).0;
        let mut entries = Vec::new();
        for leaf in leaves {
            let length = match &leaf.scalar.length {
                None => None,
                Some(Length::Written(length, span)) => Some((length.clone(), *span)),
                Some(_) => {
                    self.report.error(
                        Code::Unsupported,
                        leaf.scalar.el.span(),
                        "a record member whose size another value gives isn't supported: MDL \
                         record fields have their own sizes",
                    );
                    return None;
                }
            };
            let Some(list) = member_name(base, &leaf.path) else {
                self.report.error(
                    Code::Name,
                    written.reference,
                    format!(
                        "member {} of argument {} has no letters or digits for a catalog name",
                        leaf.shown(),
                        written.xtce
                    ),
                );
                return None;
            };
            entries.push(Entry::Field(Box::new(Field {
                name: self.record_field(written, leaf),
                key: None,
                span: written.entry,
                scalar: leaf.scalar.clone(),
                length,
                canonical: None,
                argument: Some((list, written.reference)),
                value: None,
                offset: None,
                when: None,
            })));
        }
        self.check_entries(&entries, "array element")?;
        self.structs.insert(
            ty.clone(),
            StructType {
                name: String::new(),
                base: format!("{meta}_{}_element", written.field),
                record: Some((meta.clone(), written.field.clone())),
                space,
                span: written.entry,
                entries,
            },
        );
        Some(ty)
    }

    /// A record field's name: the member names below the element joined by
    /// `_`, a change to fit MDL's names reported.
    fn record_field(&mut self, written: &Written<'d>, leaf: &Leaf<'d>) -> String {
        let joined = leaf.path.join("_");
        let (name, renamed) = identifier(&joined);
        if renamed {
            self.report.warning(
                Code::Renamed,
                written.entry,
                format!(
                    "member {} of argument {}'s elements is record field {name}: MDL names are \
                     letters, digits and '_'",
                    leaf.shown(),
                    written.xtce
                ),
            );
        }
        name
    }

    /// A JSON object of an aggregate's members, as Yamcs reads an
    /// aggregate's value; one naming a member it doesn't have is refused,
    /// as Yamcs refuses it.
    fn json_of(
        &mut self,
        text: &str,
        span: Span,
        argument: &str,
        leaves: &[Leaf<'d>],
    ) -> Option<Value> {
        match serde_json::from_str::<Value>(text) {
            Ok(value) if value.is_object() => {
                if let Some(member) = unknown_member(&value, leaves) {
                    self.report.error(
                        Code::Invalid,
                        span,
                        format!("argument {argument} has no member {member}"),
                    );
                    return None;
                }
                Some(value)
            }
            _ => {
                self.report.error(
                    Code::Invalid,
                    span,
                    format!(
                        "{text:?} isn't a JSON object of argument {argument}'s members, as Yamcs \
                         reads an aggregate's value"
                    ),
                );
                None
            }
        }
    }

    /// A ParameterRefEntry: a constant field for the parameter, or one for
    /// each member of an aggregate, each holding the value the sent
    /// command's criteria give it.
    fn parameter_entry_value(
        &mut self,
        owner: &Owner<'d>,
        entry: El<'d>,
        values: &Values<'d>,
        read_whole: bool,
    ) -> Option<Vec<Entry<'d>>> {
        entry.check(
            &Expect {
                attrs: &["parameterRef"],
                children: ENTRY_CHILDREN,
                ignored: ENTRY_IGNORED,
                text: false,
            },
            self.report,
        );
        let location = self.command_location(entry)?;
        let reference = entry.required("parameterRef", self.report)?;
        let found = match self
            .tree
            .resolve(owner.space, reference.value, Kind::Parameter)
        {
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
        let shape = self.parameter_shape(found.space, found.el)?;
        let mut scalars = Vec::new();
        if !flatten((found.space, name, Vec::new()), &shape, &mut scalars) {
            self.report.error(
                Code::Unsupported,
                reference.span,
                "an array parameter in a command isn't supported",
            );
            return None;
        }
        let mut out = Vec::new();
        out.extend(location.padding());
        for (index, (key, mut scalar)) in scalars.into_iter().enumerate() {
            let shown = self.describe(&key);
            let Some(given) = values.get(&key) else {
                // A comparison that couldn't be read may have given it.
                if !read_whole {
                    return None;
                }
                self.report.error(
                    Code::Unsupported,
                    entry.span(),
                    format!(
                        "the sent command's criteria give {shown} no value: Yamcs fills a \
                         parameter entry from an == ComparisonList on the sent command's \
                         container, and otherwise sends the parameter's latest value, which MDL \
                         can't"
                    ),
                );
                return None;
            };
            let value = self.parameter_value(&scalar, &shown, *given)?;
            scalar.conversion = None;
            scalar.inverse = None;
            scalar.unit = None;
            let length = match &scalar.length {
                Some(crate::types::Length::Written(length, span)) => Some((length.clone(), *span)),
                _ => None,
            };
            out.push(Entry::Field(Box::new(Field {
                name: self.field_name(&key, found.el),
                key: None,
                span: entry.span(),
                scalar,
                length,
                canonical: None,
                argument: None,
                value: Some((value, given.span)),
                offset: if index == 0 { location.offset() } else { None },
                when: None,
            })));
        }
        Some(out)
    }

    /// The raw value criteria give a parameter, read as a comparison's
    /// value is: an integer, an enumeration's label, or a boolean's labels.
    fn parameter_value(
        &mut self,
        scalar: &Scalar<'d>,
        shown: &str,
        given: Given<'d>,
    ) -> Option<Value> {
        let unsupported = |report: &mut crate::report::Report, why: &str| {
            report.error(
                Code::Unsupported,
                given.span,
                format!("the value criteria give {shown} isn't supported: {why}"),
            );
        };
        if matches!(scalar.raw, Raw::Other) && given.calibrated {
            unsupported(
                self.report,
                "MDL constants are raw integers and text, and its engineering value is \
                 calibrated, a float or text",
            );
            return None;
        }
        match raw_value(scalar.ty.0, &scalar.raw, given.value, given.calibrated) {
            Ok(Wanted::One(raw)) => Some(integer_json(i128::from(raw))),
            // A boolean's true is any raw value but 0; 1 is written.
            Ok(Wanted::NonZero) => Some(json!(1)),
            Ok(Wanted::Any(_)) => {
                unsupported(self.report, "the label stands for several raw values");
                None
            }
            Err(reason) => {
                self.report.error(
                    Code::Invalid,
                    given.span,
                    format!("the value criteria give {shown}: {reason}"),
                );
                None
            }
        }
    }

    /// The raw value an ArgumentAssignment gives a field, as Yamcs reads
    /// it: an integer, an enumeration's label, a boolean's labels, or text.
    fn assigned_value(
        &mut self,
        scalar: &Scalar<'d>,
        argument: &str,
        text: &str,
        span: Span,
    ) -> Option<Value> {
        if scalar.ty.0 == "string" {
            return Some(Value::String(text.to_string()));
        }
        // Yamcs writes a float through its own arithmetic (§6.3 of the
        // spec), however the value is spelled.
        if scalar.param == "f64" || matches!(scalar.raw, Raw::Other) {
            self.report.error(
                Code::Unsupported,
                span,
                format!(
                    "a value assigned to argument {argument} isn't supported: MDL constants are \
                     integers and text, and the argument is a {}",
                    if scalar.param == "f64" {
                        "float"
                    } else {
                        scalar.ty.0
                    }
                ),
            );
            return None;
        }
        // Integers are read as Yamcs reads an argument's value, as defaults
        // are; labels as comparisons read them.
        let raw = match &scalar.raw {
            Raw::Integer => {
                argument_integer(text).ok_or_else(|| format!("{text:?} isn't an integer"))
            }
            raw => match raw_value(scalar.ty.0, raw, text, true) {
                Ok(Wanted::One(raw)) => Ok(i128::from(raw)),
                // A boolean's true is any raw value but 0, and Yamcs writes 1.
                Ok(Wanted::NonZero) => Ok(1),
                // An argument type keeps a label's first value only.
                Ok(Wanted::Any(raws)) => Ok(raws.first().map_or(0, |raw| i128::from(*raw))),
                Err(reason) => Err(reason),
            },
        };
        let raw = match raw {
            Ok(raw) => raw,
            Err(reason) => {
                self.report.error(
                    Code::Invalid,
                    span,
                    format!("the value assigned to argument {argument}: {reason}"),
                );
                return None;
            }
        };
        // Yamcs checks an assigned value against the valid range when the
        // command is sent. Compared as a double, which is exact within
        // ±2^53, where every end an integer type gives lies.
        if let (Raw::Integer, Some(range)) = (&scalar.raw, &scalar.range) {
            let value = raw as f64;
            let outside = match (range.min, range.max) {
                (Some((min, _)), _) if value < min => Some(format!("below its minimum, {min}")),
                (_, Some((max, _))) if value > max => Some(format!("above its maximum, {max}")),
                _ => None,
            };
            if let Some(outside) = outside {
                self.report.error(
                    Code::Invalid,
                    span,
                    format!(
                        "the value assigned to argument {argument}, {text}, is {outside}, so \
                         Yamcs refuses the command"
                    ),
                );
                return None;
            }
        }
        Some(integer_json(raw))
    }

    /// A FixedValueEntry: an unsigned integer field holding `binaryValue`'s
    /// low `sizeInBits` bits, most significant first, as Yamcs writes
    /// them. A value wider than 64 bits is fields of 64 bits each, the
    /// first taking what's left over.
    fn fixed_entry(&mut self, entry: El<'d>, unnamed: &mut usize) -> Option<Vec<Entry<'d>>> {
        entry.check(
            &Expect {
                attrs: &["name", "binaryValue", "sizeInBits"],
                children: ENTRY_CHILDREN,
                ignored: ENTRY_IGNORED,
                text: false,
            },
            self.report,
        );
        let location = self.command_location(entry)?;
        let hex = entry.required("binaryValue", self.report)?;
        let Some(bytes) = hex_bytes(hex.value) else {
            self.report.error(
                Code::Invalid,
                hex.span,
                format!("binaryValue {:?} isn't hexadecimal", hex.value),
            );
            return None;
        };
        let available = u64::try_from(bytes.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(8);
        let (size, size_span) = match entry.attr("sizeInBits") {
            Some(attr) => match u64::try_from(xml::long(attr, self.report)?) {
                Ok(bits) if bits > 0 => (bits, attr.span),
                _ => {
                    self.report.error(
                        Code::Invalid,
                        attr.span,
                        "a FixedValueEntry is at least one bit",
                    );
                    return None;
                }
            },
            None => (available, hex.span),
        };
        if available < size {
            self.report.error(
                Code::Invalid,
                hex.span,
                format!(
                    "binaryValue {} has {available} bits, fewer than the entry's {size}",
                    hex.value
                ),
            );
            return None;
        }
        let bits: Vec<bool> = bytes
            .iter()
            .flat_map(|byte| (0..8).rev().map(move |i| byte >> i & 1 == 1))
            .collect();
        let dropped = usize::try_from(available - size).unwrap_or(usize::MAX);
        let (high, kept) = bits.split_at(dropped.min(bits.len()));
        if high.contains(&true) {
            self.report.warning(
                Code::Ignored,
                hex.span,
                format!(
                    "binaryValue {}'s bits above its low {size} aren't imported: the entry is \
                     {size} bits, and Yamcs drops them too",
                    hex.value
                ),
            );
        }
        let base = match entry.attr("name") {
            Some(attr) => {
                let (name, renamed) = identifier(attr.value);
                if renamed {
                    self.report.warning(
                        Code::Renamed,
                        attr.span,
                        format!(
                            "fixed value {} is field {name}: MDL names are letters, digits and \
                             '_'",
                            attr.value
                        ),
                    );
                }
                name
            }
            None => {
                *unnamed += 1;
                format!("fixed{unnamed}")
            }
        };
        let first = match kept.len() % 64 {
            0 => 64,
            rest => rest,
        };
        let mut chunks = vec![&kept[..first.min(kept.len())]];
        chunks.extend(kept[first.min(kept.len())..].chunks(64));
        let split = chunks.len() > 1;
        let mut out = Vec::new();
        out.extend(location.padding());
        for (index, chunk) in chunks.into_iter().enumerate() {
            let value = chunk.iter().fold(0u64, |n, bit| (n << 1) | u64::from(*bit));
            let width = u32::try_from(chunk.len()).unwrap_or(u32::MAX);
            out.push(Entry::Field(Box::new(Field {
                name: if split {
                    format!("{base}_{}", index + 1)
                } else {
                    base.clone()
                },
                key: None,
                span: entry.span(),
                scalar: pattern(entry, width, size_span),
                length: None,
                canonical: None,
                argument: None,
                value: Some((integer_json(i128::from(value)), hex.span)),
                offset: if index == 0 { location.offset() } else { None },
                when: None,
            })));
        }
        Some(out)
    }

    /// An entry's location. Every entry of an MDL command packet is
    /// written, so a condition is unsupported, and so is a repeat.
    fn command_location(&mut self, entry: El<'d>) -> Option<CommandPlace> {
        if let Some(condition) = entry.child("IncludeCondition") {
            self.report.error(
                Code::Unsupported,
                condition.span(),
                "an entry of a command on a condition isn't supported: every entry of an MDL \
                 command packet is written",
            );
            return None;
        }
        if let Some(repeat) = entry.child("RepeatEntry") {
            self.report.error(
                Code::Unsupported,
                repeat.span(),
                "a repeated entry in a command isn't supported: an MDL list is an argument of its own",
            );
            return None;
        }
        let location = match entry.child("LocationInContainerInBits") {
            Some(location) => Some(self.location(location)?),
            None => None,
        };
        Some(CommandPlace { location })
    }

    /// The catalog arguments: those no ArgumentAssignment gives a value,
    /// in order, an aggregate's members each its own, each named once. One
    /// no entry writes changes no bytes, so it's left out, and so is one
    /// that gives another's size, which MDL works out.
    fn catalog_arguments(
        &mut self,
        chain: &Chain<'d>,
        declared: &[Declared<'d>],
    ) -> Option<Vec<Argument<'d>>> {
        let mut arguments: Vec<Argument<'d>> = Vec::new();
        let mut complete = true;
        for argument in declared {
            if argument.assigned.is_some() {
                continue;
            }
            let (space, meta) = chain[argument.owner];
            let meta_path = format!(
                "{}/{}",
                self.tree.path(space),
                meta.item_name().unwrap_or_default()
            );
            if argument.size.is_some() {
                self.report.warning(
                    Code::Ignored,
                    argument.el.span(),
                    format!(
                        "argument {} of meta-command {meta_path} isn't imported: it gives the \
                         size of another argument, which MDL works out from that argument's value",
                        argument.xtce
                    ),
                );
                continue;
            }
            if !argument.written {
                self.report.warning(
                    Code::Ignored,
                    argument.el.span(),
                    format!(
                        "argument {} of meta-command {meta_path} is in no entry, so it isn't \
                         imported: it changes no bytes",
                        argument.xtce
                    ),
                );
                continue;
            }
            let (Some(name), Some(shape)) = (argument.name.clone(), argument.shape.as_ref()) else {
                complete = false;
                continue;
            };
            let made = match shape {
                Shape::Scalar(scalar) => self.scalar_argument(argument, name, scalar),
                Shape::Aggregate(_) => self.member_arguments(argument, &name, shape),
                Shape::Array(array) => self.list_arguments(argument, &name, array),
            };
            let Some(made) = made else {
                complete = false;
                continue;
            };
            for made in made {
                if let Some(first) = arguments.iter().find(|a| a.name.0 == made.name.0) {
                    self.report.duplicate(
                        Code::Name,
                        made.el.span(),
                        first.el.span(),
                        format!(
                            "arguments {} and {} both have catalog name {}",
                            first.el.item_name().unwrap_or_default(),
                            argument.xtce,
                            made.name.0
                        ),
                    );
                    complete = false;
                    continue;
                }
                arguments.push(made);
            }
        }
        complete.then_some(arguments)
    }

    /// A scalar argument, its default its own initial value or its type's.
    fn scalar_argument(
        &mut self,
        argument: &Declared<'d>,
        name: (String, Span),
        scalar: &Scalar<'d>,
    ) -> Option<Vec<Argument<'d>>> {
        let initial = argument
            .el
            .attr("initialValue")
            .map(|a| (a.value, a.span))
            .or(scalar.initial);
        let default = match initial {
            Some((text, span)) => {
                Some((self.default_value(scalar, argument.xtce, text, span)?, span))
            }
            None => None,
        };
        Some(vec![Argument {
            name,
            el: argument.el,
            scalar: scalar.clone(),
            default,
            description: description(argument.el),
            list: None,
        }])
    }

    /// An aggregate argument's members, each a catalog argument named
    /// `<argument>_<member>`. A member's default is what the argument's
    /// initial value (a JSON object) gives it, completed as Yamcs completes
    /// a value. An aggregate type's initial value isn't one Yamcs reads.
    fn member_arguments(
        &mut self,
        argument: &Declared<'d>,
        name: &(String, Span),
        shape: &Shape<'d>,
    ) -> Option<Vec<Argument<'d>>> {
        let leaves = leaves(shape)?;
        self.check_member_initials(argument.xtce, &leaves)?;
        let json = match argument.el.attr("initialValue") {
            Some(attr) => Some((
                self.json_of(attr.value, attr.span, argument.xtce, &leaves)?,
                attr.span,
            )),
            None => None,
        };
        let mut out = Vec::new();
        for leaf in &leaves {
            let member = self.member_catalog_name(argument, name, leaf)?;
            let given = json.as_ref().map(|(json, span)| (json, *span));
            let initial = self
                .member_value(leaf, &leaves, argument.xtce, given)
                .ok()?;
            let shown = format!("{}.{}", argument.xtce, leaf.shown());
            let default = match initial {
                Some((text, span)) => {
                    Some((self.default_value(&leaf.scalar, &shown, &text, span)?, span))
                }
                None => None,
            };
            out.push(Argument {
                name: (member, name.1),
                el: argument.el,
                scalar: leaf.scalar.clone(),
                default,
                description: leaf
                    .description
                    .clone()
                    .or_else(|| description(argument.el)),
                list: None,
            });
        }
        Some(out)
    }

    /// An array argument's list arguments: one, or for an aggregate's
    /// elements one per member, filling records. A default is what the
    /// array's initial value, its own or else its type's, gives: a JSON
    /// array of values, or for records of objects, each completed as an
    /// aggregate's value is.
    fn list_arguments(
        &mut self,
        argument: &Declared<'d>,
        name: &(String, Span),
        array: &ArrayShape<'d>,
    ) -> Option<Vec<Argument<'d>>> {
        let leaves = leaves(&array.element)?;
        // Set when the array was written, which failed if it isn't.
        let bounds = argument.list?;
        let initial = argument
            .el
            .attr("initialValue")
            .or_else(|| array.el.attr("initialValue"))
            .map(|a| (a.value, a.span));
        let json = match initial {
            Some((text, span)) => match serde_json::from_str::<Value>(text) {
                Ok(Value::Array(items)) => Some((items, span)),
                _ => {
                    self.report.error(
                        Code::Invalid,
                        span,
                        format!(
                            "{text:?} isn't a JSON array of argument {}'s values",
                            argument.xtce
                        ),
                    );
                    return None;
                }
            },
            None => None,
        };
        let record = matches!(array.element, Shape::Aggregate(_));
        if record {
            self.check_member_initials(argument.xtce, &leaves)?;
        }
        if let Some((items, span)) = &json
            && record
        {
            for item in items {
                if !item.is_object() || unknown_member(item, &leaves).is_some() {
                    self.report.error(
                        Code::Invalid,
                        *span,
                        format!(
                            "argument {}'s initial value holds {item}, which isn't an object of \
                             its elements' members",
                            argument.xtce
                        ),
                    );
                    return None;
                }
            }
        }
        let mut out = Vec::new();
        for leaf in &leaves {
            let list = self.member_catalog_name(argument, name, leaf)?;
            let shown = match leaf.path.is_empty() {
                true => argument.xtce.to_string(),
                false => format!("{}.{}", argument.xtce, leaf.shown()),
            };
            let default = match &json {
                Some((items, span)) => {
                    let mut values = Vec::with_capacity(items.len());
                    for (index, item) in items.iter().enumerate() {
                        let text = if record {
                            match self.member_value(
                                leaf,
                                &leaves,
                                argument.xtce,
                                Some((item, *span)),
                            ) {
                                Ok(Some((text, _))) => text,
                                Ok(None) => {
                                    self.report.error(
                                        Code::Invalid,
                                        *span,
                                        format!(
                                            "argument {}'s initial value gives value {index} no \
                                             {}, and the member has no initial value",
                                            argument.xtce,
                                            leaf.shown()
                                        ),
                                    );
                                    return None;
                                }
                                Err(()) => return None,
                            }
                        } else {
                            json_text(item).into_owned()
                        };
                        values.push(self.default_value(&leaf.scalar, &shown, &text, *span)?);
                    }
                    Some((Value::Array(values), *span))
                }
                None => None,
            };
            out.push(Argument {
                name: (list, name.1),
                el: argument.el,
                scalar: leaf.scalar.clone(),
                default,
                description: leaf
                    .description
                    .clone()
                    .or_else(|| description(argument.el)),
                list: Some(bounds),
            });
        }
        Some(out)
    }

    /// Every member's own initial value, as Yamcs checks them when it loads
    /// the type, whether a value reaches them or not: an aggregate member's
    /// a JSON object of its members, another a value of its type.
    fn check_member_initials(&mut self, argument: &str, leaves: &[Leaf<'d>]) -> Option<()> {
        let mut complete = true;
        for leaf in leaves {
            let last = leaf.path.len().saturating_sub(1);
            for (level, initial) in leaf.initials.iter().enumerate() {
                let Some((text, span)) = *initial else {
                    continue;
                };
                let prefix = &leaf.path[..=level];
                let shown = format!("{argument}.{}", prefix.join("."));
                complete &= if level == last {
                    self.default_value(&leaf.scalar, &shown, text, span)
                        .is_some()
                } else {
                    self.member_initial(text, span, argument, prefix, leaves)
                        .is_ok()
                };
            }
        }
        complete.then_some(())
    }

    /// A member's catalog name, or the argument's for a leaf with no
    /// members.
    fn member_catalog_name(
        &mut self,
        argument: &Declared<'d>,
        name: &(String, Span),
        leaf: &Leaf<'d>,
    ) -> Option<String> {
        let member = member_name(&name.0, &leaf.path);
        if member.is_none() {
            self.report.error(
                Code::Name,
                name.1,
                format!(
                    "member {} of argument {} has no letters or digits for a catalog name",
                    leaf.shown(),
                    argument.xtce
                ),
            );
        }
        member
    }

    /// An initial value as the argument's default, read as Yamcs reads it:
    /// integers in decimal, or hexadecimal, octal or binary with `0x`,
    /// `0o` or `0b`, with `_` between digits; a boolean's labels; an
    /// enumeration's label; binary as hexadecimal digits.
    fn default_value(
        &mut self,
        scalar: &Scalar<'d>,
        argument: &str,
        text: &str,
        span: Span,
    ) -> Option<Value> {
        let relative_time = matches!(
            scalar.el.name(),
            "RelativeTimeArgumentType" | "RelativeTimeAgumentType"
        );
        let value = match (scalar.param, &scalar.raw) {
            // An xs:duration, in the argument's unit.
            _ if relative_time => match (duration_seconds(text), scalar.unit.as_ref()) {
                (Some(seconds), Some((unit, _))) => {
                    let value = match unit.as_str() {
                        "ps" => seconds * 1e12,
                        "d" => seconds / 86_400.0,
                        _ => seconds,
                    };
                    Ok(json!(value))
                }
                (Some(_), None) => Err(format!(
                    "{text:?} is a duration, and this argument's months or years vary in length"
                )),
                (None, _) => Err(format!(
                    "{text:?} isn't a duration in days, hours, minutes and seconds (PT1.5S)"
                )),
            },
            ("u64" | "i64", _) => argument_integer(text)
                .map(integer_json)
                .ok_or_else(|| format!("{text:?} isn't an integer")),
            ("f64", _) => match text.trim().parse::<f64>() {
                Ok(value) if value.is_finite() => Ok(json!(value)),
                _ => Err(format!("{text:?} isn't a finite number")),
            },
            ("bool", Raw::Boolean { one, zero }) => {
                if text == one || text == "true" {
                    Ok(json!(true))
                } else if text == zero || text == "false" {
                    Ok(json!(false))
                } else {
                    Err(format!("{text:?} isn't one of its values ({one}, {zero})"))
                }
            }
            ("enum", _) => {
                if scalar.states.iter().any(|(state, _)| state == text) {
                    Ok(Value::String(text.to_string()))
                } else {
                    let states: Vec<&str> = scalar.states.iter().map(|(s, _)| s.as_str()).collect();
                    Err(format!(
                        "{text:?} isn't one of its labels ({})",
                        states.join(", ")
                    ))
                }
            }
            ("bytes", _) => hex_bytes(text)
                .map(|bytes| Value::String(bytes.iter().map(|b| format!("{b:02x}")).collect()))
                .ok_or_else(|| format!("{text:?} isn't hexadecimal")),
            _ => Ok(Value::String(text.to_string())),
        };
        match value {
            Ok(value) => Some(value),
            Err(reason) => {
                self.report.error(
                    Code::Invalid,
                    span,
                    format!("argument {argument}'s initial value: {reason}"),
                );
                None
            }
        }
    }

    /// Command packet names: each meta-command's, qualified as packets
    /// are; catalog names, each once.
    fn name_commands(&mut self, commands: &mut [Command<'d>]) {
        let items: Vec<(&'d str, usize, Span)> = commands
            .iter()
            .map(|c| (c.el.item_name().unwrap_or_default(), c.space, c.el.span()))
            .collect();
        let names = self.unique_names(&items, ("meta-command", "command packet"));
        for (command, name) in commands.iter_mut().zip(names) {
            command.name = name;
        }
        let mut packets: BTreeMap<&str, Span> = BTreeMap::new();
        let mut canonical: BTreeMap<&str, Span> = BTreeMap::new();
        for command in commands.iter() {
            let span = command.el.span();
            match packets.get(command.name.as_str()) {
                Some(first) => self.report.duplicate(
                    Code::Name,
                    span,
                    *first,
                    format!("two command packets are named {}", command.name),
                ),
                None => {
                    packets.insert(&command.name, span);
                }
            }
            let name = command.canonical.0.as_str();
            // Params and commands share the catalog's names.
            if let Some(param) = self.params.get(name) {
                let message = format!(
                    "meta-command {}/{} and parameter {} both have catalog name {name}",
                    self.tree.path(command.space),
                    command.el.item_name().unwrap_or_default(),
                    self.describe(&param.key)
                );
                self.report
                    .duplicate(Code::Name, span, param.el.span(), message);
                continue;
            }
            match canonical.get(name) {
                Some(first) => self.report.duplicate(
                    Code::Name,
                    span,
                    *first,
                    format!("two meta-commands have catalog name {name}"),
                ),
                None => {
                    canonical.insert(name, span);
                }
            }
        }
    }
}

/// The meta-command whose container is being read: where it is in the
/// chain, its SpaceSystem, its element, and whether it's the command sent.
struct Owner<'d> {
    index: usize,
    space: usize,
    meta: El<'d>,
    last: bool,
}

/// What reading a container's entries needs from the command: its chain,
/// the values its criteria give (and whether every comparison was read),
/// how many unnamed fixed values came before, and the size arguments'
/// fields written so far, by argument.
struct Read<'a, 'd> {
    chain: &'a Chain<'d>,
    values: &'a Values<'d>,
    read_whole: bool,
    unnamed: &'a mut usize,
    sizes: &'a mut Vec<(&'d str, SizeField)>,
}

/// An argument an entry writes: its XTCE name, field name, catalog name,
/// the value assigned to it, whether it's another's size, its element, and
/// where the entry and its reference are.
struct Written<'d> {
    xtce: &'d str,
    /// The meta-command declaring it.
    meta: &'d str,
    field: String,
    catalog: Option<(String, Span)>,
    assigned: Option<(&'d str, Span, Span)>,
    size: bool,
    el: El<'d>,
    entry: Span,
    reference: Span,
}

/// One field writing an argument's value: its name and scalar, the catalog
/// argument it encodes, the value assigned to it, the argument it's the
/// size of, where the entry is, and the argument (and member) for
/// messages.
struct LeafField<'d> {
    name: String,
    scalar: Scalar<'d>,
    catalog: Option<(String, Span)>,
    value: Option<(String, Span)>,
    size_of: Option<&'d str>,
    span: Span,
    shown: String,
}

/// Places an entry at an offset from the start of the packet.
fn place(entry: &mut Entry<'_>, offset: Option<(u32, Span)>) {
    match entry {
        Entry::Field(field) => field.offset = offset,
        Entry::Array(array) => array.offset = offset,
        Entry::Struct(inner) => inner.offset = offset,
        Entry::Padding { .. } => {}
    }
}

/// The scalars a parameter's shape holds, with the parameter or member
/// each is: one, or an aggregate's members in order. `false` for an
/// array, which a command can't fix the values of.
fn flatten<'d>(
    key: ParamKey<'d>,
    shape: &Shape<'d>,
    out: &mut Vec<(ParamKey<'d>, Scalar<'d>)>,
) -> bool {
    match shape {
        Shape::Scalar(scalar) => {
            out.push((key, (**scalar).clone()));
            true
        }
        Shape::Aggregate(members) => members.iter().all(|member| {
            let mut member_key = key.clone();
            member_key.2.push(member.name);
            flatten(member_key, &member.shape, out)
        }),
        Shape::Array(_) => false,
    }
}

/// Where a command entry goes.
struct CommandPlace {
    location: Option<Location>,
}

impl CommandPlace {
    /// The padding a `previousEntry` location asks for, ahead of the entry.
    fn padding<'d>(&self) -> Option<Entry<'d>> {
        match self.location {
            Some(Location::After(bits, span)) if bits > 0 => Some(Entry::Padding {
                bits,
                span,
                when: None,
            }),
            _ => None,
        }
    }

    fn offset(&self) -> Option<(u32, Span)> {
        match self.location {
            Some(Location::Start(bits, span)) => Some((bits, span)),
            _ => None,
        }
    }
}

/// The scalar of a fixed value: an unsigned integer of `bits` bits, most
/// significant byte first.
fn pattern(entry: El<'_>, bits: u32, span: Span) -> Scalar<'_> {
    Scalar {
        el: entry,
        ty: ("uint", entry.span()),
        size_bits: Some((bits, span)),
        order: (bits > 8).then_some((Order::Big, span)),
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

/// Hexadecimal digits as bytes, an odd count read with a leading 0, as
/// Yamcs reads them. `None` for anything else.
fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let digits = text.trim();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let padded = if digits.len() % 2 == 1 {
        format!("0{digits}")
    } else {
        digits.to_string()
    };
    (0..padded.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&padded[i..i + 2], 16).ok())
        .collect()
}

/// An integer as Yamcs reads an argument's: an optional sign, then decimal
/// digits, or `0x`, `0o` or `0b` and digits in that base, with `_` allowed
/// between digits.
fn argument_integer(text: &str) -> Option<i128> {
    let text = text.trim();
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (radix, digits) = match unsigned.get(..2) {
        Some("0x" | "0X") => (16, &unsigned[2..]),
        Some("0o" | "0O") => (8, &unsigned[2..]),
        Some("0b" | "0B") => (2, &unsigned[2..]),
        _ => (10, unsigned),
    };
    if digits.is_empty() || digits.starts_with(['_', '+', '-']) || digits.ends_with('_') {
        return None;
    }
    let digits: String = digits.chars().filter(|c| *c != '_').collect();
    let magnitude = u64::from_str_radix(&digits, radix).ok()?;
    let value = i128::from(magnitude);
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_read_as_yamcs_reads_them() {
        assert_eq!(argument_integer("42"), Some(42));
        assert_eq!(argument_integer("-0x1F"), Some(-31));
        assert_eq!(argument_integer("0b1010_1010"), Some(170));
        assert_eq!(argument_integer("0o17"), Some(15));
        assert_eq!(argument_integer("+7"), Some(7));
        assert_eq!(
            argument_integer("18446744073709551615"),
            Some(u64::MAX.into())
        );
        for bad in [
            "",
            "0x",
            "_1",
            "1_",
            "1.5",
            "0x1G",
            "--1",
            "0x+5",
            "+-1",
            "18446744073709551616",
        ] {
            assert_eq!(argument_integer(bad), None, "{bad}");
        }
    }

    #[test]
    fn hex_reads_odd_digit_counts_with_a_leading_zero() {
        assert_eq!(hex_bytes("1A2b"), Some(vec![0x1a, 0x2b]));
        assert_eq!(hex_bytes("abc"), Some(vec![0x0a, 0xbc]));
        assert_eq!(hex_bytes("0x12"), None);
        assert_eq!(hex_bytes(""), None);
    }

    #[test]
    fn integers_past_two_to_the_53_are_strings() {
        assert_eq!(
            integer_json(9_007_199_254_740_991),
            json!(9_007_199_254_740_991_i64)
        );
        assert_eq!(
            integer_json(9_007_199_254_740_992),
            json!("9007199254740992")
        );
        assert_eq!(
            integer_json(-9_007_199_254_740_992),
            json!("-9007199254740992")
        );
        assert_eq!(integer_json(u64::MAX.into()), json!("18446744073709551615"));
    }
}
