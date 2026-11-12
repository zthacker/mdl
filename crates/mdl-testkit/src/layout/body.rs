//! A packet's body: the fields, arrays and structs after its header and
//! time fields, and the composites over its numbers. Laid out as
//! `spec/decoding.md` §5.2 reads them: entries one after another; arrays
//! of fixed, field (§5.4) and remaining counts, each element starting
//! where the last ended; struct entries and elements read from their own
//! start, of struct types that nest. And the values a message holds in
//! them, written bit by bit (§2.1, §2.2), with the record (§6) and samples
//! (§7.3, §7.4) they decode to.
//!
//! What varies in size (an array counted by a field, or running to the
//! end) is whole bytes, so where each later entry starts within a byte is
//! known, and the alignment rules apply as they do to a fixed layout.

use std::collections::BTreeMap;
use std::iter::once;

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;
use proptest::strategy::Union;
use serde_json::{Map, Value, json};

use super::meaning::sample;
use super::{
    FieldSpec, Kind, Meaning, Placed, Raw, Sample, encode, field_json, field_spec, fit,
    inside_one_byte, kind, param_json, put_bits, raw_value, scalar_json, spec_of,
};
use composite::{CompositeNode, CompositeSource, CompositeSpec, composite_source, resolve};

/// The most elements an array whose count varies has in a message.
const MOST_ELEMENTS: u32 = 5;

// ---- What a body is -------------------------------------------------------

/// A packet's body, before it's laid out.
#[derive(Debug, Clone)]
pub(super) struct BodySpec {
    items: Vec<ItemSpec>,
    /// An array with a `remaining` count, last (§5.2).
    trailer: Option<ArraySpec>,
    composites: Vec<CompositeSpec>,
    /// The order the packet lists its composites in, which needn't be
    /// their names' (samples follow the names').
    listed: Vec<usize>,
}

impl BodySpec {
    /// A body of fields alone.
    pub(super) fn fields(fields: Vec<FieldSpec>) -> BodySpec {
        BodySpec {
            items: fields.into_iter().map(ItemSpec::Field).collect(),
            trailer: None,
            composites: Vec::new(),
            listed: Vec::new(),
        }
    }
}

/// An entry of a body or struct type, before it's laid out.
#[derive(Debug, Clone)]
enum ItemSpec {
    Field(FieldSpec),
    Array(ArraySpec),
    /// A struct entry, of a struct type of its own.
    Struct(TypeSpec),
}

#[derive(Debug, Clone)]
struct ArraySpec {
    count: CountSpec,
    element: ElementSpec,
    /// For mapped scalar elements, sometimes: the time between samples, in
    /// milliseconds.
    interval: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
enum CountSpec {
    Fixed(u32),
    /// From a count field of `bits`, as `raw * multiply + add` (§5.4): the
    /// container's last count field if `share` and the arrays can agree on
    /// its values, else a new one right before the array.
    Field {
        bits: u32,
        multiply: u32,
        add: i64,
        share: bool,
        mapped: bool,
    },
    Remaining,
}

#[derive(Debug, Clone)]
enum ElementSpec {
    Scalar(FieldSpec),
    Struct(TypeSpec),
}

/// A struct type: its entries, the first a field.
#[derive(Debug, Clone)]
struct TypeSpec {
    items: Vec<ItemSpec>,
}

/// Any body: fields, arrays and structs in any order, sometimes an array
/// that runs to the end, and up to two composites, whose members' items
/// mix with the rest.
pub(super) fn body_spec() -> BoxedStrategy<BodySpec> {
    (
        vec(item_spec(2), 1..=6),
        proptest::option::weighted(0.3, trailer_spec()),
        vec(composite_source(), 0..=2),
    )
        .prop_flat_map(|(items, trailer, sources)| {
            let total = items.len() + sources.iter().map(|s| s.items.len()).sum::<usize>();
            let order = Just((0..total).collect::<Vec<usize>>()).prop_shuffle();
            let listed = Just((0..sources.len()).collect::<Vec<usize>>()).prop_shuffle();
            (Just(items), Just(trailer), Just(sources), order, listed)
        })
        .prop_map(|(items, trailer, sources, order, listed)| {
            mix(items, trailer, sources, &order, listed)
        })
        .boxed()
}

/// The body's items and its composites' items, in `order` (a permutation
/// of them all, the body's first), with each member found where its item
/// went.
fn mix(
    items: Vec<ItemSpec>,
    trailer: Option<ArraySpec>,
    sources: Vec<CompositeSource>,
    order: &[usize],
    listed: Vec<usize>,
) -> BodySpec {
    let mut all: Vec<Option<ItemSpec>> = items.into_iter().map(Some).collect();
    let mut firsts = Vec::new();
    for source in &sources {
        firsts.push(all.len());
        all.extend(source.items.iter().cloned().map(Some));
    }
    let mut position = vec![0; all.len()];
    for (at, &item) in order.iter().enumerate() {
        position[item] = at;
    }
    let items = order.iter().filter_map(|&item| all[item].take()).collect();
    let composites = sources
        .into_iter()
        .zip(firsts)
        .map(|(source, first)| source.placed(&position[first..]))
        .collect();
    BodySpec {
        items,
        trailer,
        composites,
        listed,
    }
}

/// An entry of a body or struct type: struct entries and arrays of
/// structs only while `depth` allows another level.
fn item_spec(depth: u32) -> BoxedStrategy<ItemSpec> {
    let mut items = vec![
        (6, field_spec().prop_map(ItemSpec::Field).boxed()),
        (3, array_spec(depth).prop_map(ItemSpec::Array).boxed()),
    ];
    if depth > 0 {
        items.push((2, type_spec(depth - 1).prop_map(ItemSpec::Struct).boxed()));
    }
    Union::new_weighted(items).boxed()
}

/// An array of a fixed count (now and then none) or one from a field.
fn array_spec(depth: u32) -> BoxedStrategy<ArraySpec> {
    let count = prop_oneof![
        3 => prop_oneof![1 => Just(0_u32), 8 => 1_u32..=4].prop_map(CountSpec::Fixed),
        2 => count_field(),
    ];
    count
        .prop_flat_map(move |count| {
            let varying = !matches!(count, CountSpec::Fixed(_));
            (Just(count), element_spec(depth, varying))
        })
        .prop_flat_map(|(count, element)| (Just(count), interval_for(&element), Just(element)))
        .prop_map(|(count, interval, element)| ArraySpec {
            count,
            element,
            interval,
        })
        .boxed()
}

/// An array that runs to the end of the packet.
fn trailer_spec() -> BoxedStrategy<ArraySpec> {
    element_spec(1, true)
        .prop_flat_map(|element| (interval_for(&element), Just(element)))
        .prop_map(|(interval, element)| ArraySpec {
            count: CountSpec::Remaining,
            element,
            interval,
        })
        .boxed()
}

/// A count field: 2 to 16 bits, scaled now and then.
fn count_field() -> BoxedStrategy<CountSpec> {
    (
        2_u32..=16,
        prop_oneof![3 => Just(1_u32), 1 => 2_u32..=3],
        prop_oneof![3 => Just(0_i64), 1 => -2_i64..=2],
        proptest::bool::weighted(0.4),
        any::<bool>(),
    )
        .prop_map(|(bits, multiply, add, share, mapped)| CountSpec::Field {
            bits,
            multiply,
            add,
            share,
            mapped,
        })
        .boxed()
}

/// An array's element: a scalar, not a bool if the count varies (a varying
/// count keeps whole bytes, and a bool is one bit), or a struct while
/// `depth` allows another level.
fn element_spec(depth: u32, varying: bool) -> BoxedStrategy<ElementSpec> {
    // Integers narrower than a byte, often: bit-packed elements, and
    // little-endian ones that must each sit inside a byte.
    let kinds = prop_oneof![
        3 => kind(!varying),
        1 => (1_u32..=7).prop_map(Kind::Uint),
        1 => (1_u32..=7).prop_map(Kind::Int),
    ];
    let scalar = spec_of(kinds.boxed()).prop_map(ElementSpec::Scalar);
    if depth == 0 {
        return scalar.boxed();
    }
    prop_oneof![
        3 => scalar,
        1 => type_spec(depth - 1).prop_map(ElementSpec::Struct),
    ]
    .boxed()
}

/// A struct type: a field, then up to three entries more.
fn type_spec(depth: u32) -> BoxedStrategy<TypeSpec> {
    (field_spec(), vec(item_spec(depth), 0..=3))
        .prop_map(|(first, rest)| TypeSpec {
            items: once(ItemSpec::Field(first)).chain(rest).collect(),
        })
        .boxed()
}

/// An interval between samples, sometimes, for mapped scalar elements.
fn interval_for(element: &ElementSpec) -> BoxedStrategy<Option<f64>> {
    match element {
        ElementSpec::Scalar(spec) if spec.mapped => {
            proptest::option::weighted(0.35, interval()).boxed()
        }
        _ => Just(None).boxed(),
    }
}

/// The time between a block's samples, in milliseconds: some at half a
/// nanosecond's tie (2.5e-6 puts element 1 at 2.5 ns, which rounds to 3),
/// and any from a microsecond to ten seconds.
fn interval() -> BoxedStrategy<f64> {
    prop_oneof![
        1 => select(&[5e-7, 1.5e-6, 2.5e-6, 0.125, 1.0, 1000.0 / 3.0][..]),
        2 => 1e-3..1e4_f64,
    ]
    .boxed()
}

// ---- Laid out -------------------------------------------------------------

/// A body's or struct type's entry, laid out.
#[derive(Debug, Clone)]
pub(super) enum Node {
    Padding(u64),
    Field(Placed),
    Array(ArrayNode),
    Struct(StructNode),
}

#[derive(Debug, Clone)]
pub(super) struct ArrayNode {
    name: String,
    count: Counted,
    element: ElementNode,
    interval: Option<f64>,
}

#[derive(Debug, Clone)]
enum Counted {
    Fixed(u32),
    Field {
        field: String,
        multiply: u32,
        add: i64,
    },
    Remaining,
}

#[derive(Debug, Clone)]
enum ElementNode {
    Scalar(Placed),
    Struct(TypeNode),
}

/// A struct type, laid out from its own start.
#[derive(Debug, Clone)]
pub(super) struct TypeNode {
    name: String,
    entries: Vec<Node>,
}

#[derive(Debug, Clone)]
pub(super) struct StructNode {
    name: String,
    ty: TypeNode,
}

/// A packet's body, laid out.
#[derive(Debug, Clone)]
pub(super) struct Body {
    /// Where it starts, in every message.
    pub(super) start: u64,
    pub(super) nodes: Vec<Node>,
    /// Its struct types, for the set.
    types: Vec<TypeNode>,
    composites: Vec<CompositeNode>,
    /// The order the packet lists its composites in.
    listed: Vec<usize>,
    /// The furthest bit a message reaches.
    pub(super) most_bits: u64,
}

/// How a body's params and struct types are named, and the types laid
/// out so far.
struct Names {
    packet: String,
    params: String,
    types: Vec<TypeNode>,
}

/// Lays a body out from bit `start`, in a family of the given byte order.
/// Its entries are named `f0`, `f1` and on, and their params and struct
/// types after the packet's.
pub(super) fn lay_out(
    spec: BodySpec,
    little: bool,
    start: u64,
    packet: &str,
    params: &str,
) -> Body {
    let mut names = Names {
        packet: packet.to_string(),
        params: params.to_string(),
        types: Vec::new(),
    };
    let mut placer = Placer::new(little, start, params.to_string());
    for (position, item) in spec.items.into_iter().enumerate() {
        placer.item(format!("f{position}"), item, &mut names);
    }
    if let Some(trailer) = spec.trailer {
        placer.array("rest".into(), trailer, &mut names);
    }
    let nodes = placer.nodes;
    let composites = spec
        .composites
        .into_iter()
        .enumerate()
        .filter_map(|(index, composite)| resolve(composite, &format!("{params}.c{index}"), &nodes))
        .collect();
    let most_bits = start + most_bits(&nodes);
    Body {
        start,
        nodes,
        types: names.types,
        composites,
        listed: spec.listed,
        most_bits,
    }
}

/// Places entries one after another (§5.2). `at` is where the next entry
/// starts when every count that varies is 0; what varies is whole bytes,
/// so `at` modulo 8 is exact either way.
struct Placer {
    little: bool,
    at: u64,
    /// What this container's params' names start with.
    params: String,
    nodes: Vec<Node>,
    counts: Vec<CountField>,
}

/// A container's count field, and the scales, `(multiply, add)`, of the
/// arrays it counts.
struct CountField {
    name: String,
    bits: u32,
    scales: Vec<(u32, i64)>,
}

impl Placer {
    fn new(little: bool, at: u64, params: String) -> Placer {
        Placer {
            little,
            at,
            params,
            nodes: Vec::new(),
            counts: Vec::new(),
        }
    }

    fn pad_to_byte(&mut self) {
        let skip = (8 - self.at % 8) % 8;
        if skip > 0 {
            self.nodes.push(Node::Padding(skip));
            self.at += skip;
        }
    }

    fn item(&mut self, name: String, item: ItemSpec, names: &mut Names) {
        match item {
            ItemSpec::Field(spec) => self.field(name, spec),
            ItemSpec::Array(spec) => self.array(name, spec, names),
            ItemSpec::Struct(spec) => {
                let ty = struct_type(self.little, spec, false, false, names);
                if needs_byte(&ty.entries) {
                    self.pad_to_byte();
                }
                self.at += least_bits(&ty.entries);
                self.nodes.push(Node::Struct(StructNode { name, ty }));
            }
        }
    }

    /// A field: strings and bytes start on a byte; little-endian numbers
    /// that span bytes are whole bytes on byte boundaries (widened if need
    /// be); a byte order is spelled only where it differs from the
    /// family's and matters (§2.1, §2.2).
    fn field(&mut self, name: String, spec: FieldSpec) {
        let param = spec.mapped.then(|| format!("{}.{name}", self.params));
        let FieldSpec {
            mut kind,
            mut meaning,
            invalid,
            flip,
            ..
        } = spec;
        let mut little = self.little;
        let mut explicit = false;
        if flip && kind.ordered() {
            little = !self.little;
            explicit = true;
        }
        let ordered_little = little && kind.ordered();
        if kind.on_a_byte() || (ordered_little && !inside_one_byte(self.at, kind.bits())) {
            self.pad_to_byte();
        }
        if ordered_little && !inside_one_byte(self.at, kind.bits()) && kind.bits() % 8 != 0 {
            (kind, meaning) = widened(&kind, meaning);
        }
        // Inside one byte the order doesn't apply, so it isn't spelled.
        if explicit && inside_one_byte(self.at, kind.bits()) {
            explicit = false;
            little = self.little;
        }
        let bits = kind.bits();
        self.nodes.push(Node::Field(Placed {
            name,
            param,
            kind,
            meaning,
            invalid,
            little,
            explicit,
        }));
        self.at += u64::from(bits);
    }

    /// An array (§5.2), after its count field. A remaining count starts it
    /// on a byte, since it reads to the packet's end.
    fn array(&mut self, name: String, spec: ArraySpec, names: &mut Names) {
        let count = match spec.count {
            CountSpec::Fixed(count) => Counted::Fixed(count),
            CountSpec::Remaining => {
                self.pad_to_byte();
                Counted::Remaining
            }
            CountSpec::Field {
                bits,
                multiply,
                add,
                share,
                mapped,
            } => Counted::Field {
                field: self.count_field(&name, bits, (multiply, add), share, mapped),
                multiply,
                add,
            },
        };
        let fixed = match count {
            Counted::Fixed(count) => Some(count),
            Counted::Field { .. } | Counted::Remaining => None,
        };
        let element = match spec.element {
            ElementSpec::Scalar(spec) => ElementNode::Scalar(self.element(&name, spec, fixed)),
            ElementSpec::Struct(spec) => {
                let ty = struct_type(self.little, spec, true, fixed.is_none(), names);
                if needs_byte(&ty.entries) {
                    self.pad_to_byte();
                }
                ElementNode::Struct(ty)
            }
        };
        if let Some(count) = fixed {
            self.at += u64::from(count) * element_bits(&element, least_bits);
        }
        self.nodes.push(Node::Array(ArrayNode {
            name,
            count,
            element,
            interval: spec.interval,
        }));
    }

    /// A scalar element, its array starting at the next position. Its byte
    /// order is spelled only at 16 bits and more, which never sit inside
    /// one byte. A varying count, or a little-endian number that can't sit
    /// inside one byte wherever an element starts, takes whole bytes; text,
    /// bytes and little-endian whole bytes start the array on a byte.
    fn element(&mut self, array: &str, spec: FieldSpec, fixed: Option<u32>) -> Placed {
        let param = spec.mapped.then(|| format!("{}.{array}", self.params));
        let FieldSpec {
            mut kind,
            mut meaning,
            invalid,
            flip,
            ..
        } = spec;
        let mut little = self.little;
        let mut explicit = false;
        if flip && kind.ordered() && kind.bits() >= 16 {
            little = !little;
            explicit = true;
        }
        let bits = kind.bits();
        if kind.ordered() && bits % 8 != 0 {
            let widen = match fixed {
                None => true,
                Some(_) if !little => false,
                Some(_) if bits > 8 => true,
                Some(count) => {
                    if !narrow_fits(self.at, bits, count) {
                        self.pad_to_byte();
                    }
                    !narrow_fits(self.at, bits, count)
                }
            };
            if widen {
                (kind, meaning) = widened(&kind, meaning);
            }
        }
        if kind.on_a_byte() || (kind.ordered() && little && kind.bits() % 8 == 0) {
            self.pad_to_byte();
        }
        Placed {
            name: array.to_string(),
            param,
            kind,
            meaning,
            invalid,
            little,
            explicit,
        }
    }

    /// The count field for an array: the container's last, if `share` and
    /// it can hold values that give every array it counts, this one too, a
    /// count of 0 to [`MOST_ELEMENTS`]; else a new one, named after the
    /// array, right before it.
    fn count_field(
        &mut self,
        array: &str,
        bits: u32,
        scale: (u32, i64),
        share: bool,
        mapped: bool,
    ) -> String {
        if share && let Some(last) = self.counts.last_mut() {
            let mut scales = last.scales.clone();
            scales.push(scale);
            if raw_range(last.bits, &scales).is_some() {
                last.scales = scales;
                return last.name.clone();
            }
        }
        let name = format!("{array}_n");
        let spec = FieldSpec {
            kind: Kind::Uint(bits),
            meaning: Meaning::U64,
            invalid: Vec::new(),
            flip: false,
            mapped,
        };
        self.field(name.clone(), spec);
        // Placed in a little-endian family, it may have been widened.
        let bits = match self.nodes.last() {
            Some(Node::Field(field)) => field.kind.bits(),
            _ => bits,
        };
        self.counts.push(CountField {
            name: name.clone(),
            bits,
            scales: vec![scale],
        });
        name
    }
}

/// The same kind of integer in whole bytes, and its meaning. Widening
/// keeps every value, state, point and sentinel it held, but an i64 param
/// takes a uint of at most 63 bits.
fn widened(kind: &Kind, meaning: Meaning) -> (Kind, Meaning) {
    let kind = kind.widened(kind.bits().div_ceil(8) * 8);
    let meaning = if kind == Kind::Uint(64) && meaning == Meaning::I64 {
        Meaning::U64
    } else {
        meaning
    };
    (kind, meaning)
}

/// Whether little-endian elements of `bits`, under a byte, each sit inside
/// one byte from `start` on, `count` of them (§5.2 checks element 0 even
/// for a count of 0).
fn narrow_fits(start: u64, bits: u32, count: u32) -> bool {
    (0..u64::from(count.max(1))).all(|k| inside_one_byte(start + k * u64::from(bits), bits))
}

/// A struct type laid out from its own start (§5.2: places within a byte
/// inside it count from 0), named and kept for the set. As an array's
/// element it's padded to whole bytes when it needs a byte start (each
/// element does then), when the count varies, or when its own size does:
/// §5.2 counts every residue after elements whose size varies, unless
/// they're whole bytes.
fn struct_type(
    little: bool,
    spec: TypeSpec,
    element: bool,
    varying: bool,
    names: &mut Names,
) -> TypeNode {
    let index = names.types.len();
    let name = format!("{}_t{index}", names.packet);
    // Its place is kept while the types inside it are laid out.
    names.types.push(TypeNode {
        name: name.clone(),
        entries: Vec::new(),
    });
    let mut inner = Placer::new(little, 0, format!("{}.t{index}", names.params));
    for (position, item) in spec.items.into_iter().enumerate() {
        inner.item(format!("f{position}"), item, names);
    }
    let sized = least_bits(&inner.nodes) == most_bits(&inner.nodes);
    if element && (varying || !sized || needs_byte(&inner.nodes)) {
        inner.pad_to_byte();
    }
    let ty = TypeNode {
        name,
        entries: inner.nodes,
    };
    names.types[index] = ty.clone();
    ty
}

/// Whether a struct of these entries starts on a byte (§5.2): it holds,
/// directly, in an array or in a nested struct, a string, bytes, a
/// little-endian value, or a value of at most 8 bits that names its own
/// byte order. A bool has no byte order (§2.1), so it doesn't count.
fn needs_byte(nodes: &[Node]) -> bool {
    let scalar = |field: &Placed| {
        field.kind.on_a_byte()
            || (field.kind.ordered()
                && (field.little || (field.explicit && field.kind.bits() <= 8)))
    };
    nodes.iter().any(|node| match node {
        Node::Padding(_) => false,
        Node::Field(field) => scalar(field),
        Node::Array(array) => match &array.element {
            ElementNode::Scalar(element) => scalar(element),
            ElementNode::Struct(ty) => needs_byte(&ty.entries),
        },
        Node::Struct(node) => needs_byte(&node.ty.entries),
    })
}

/// The fewest bits entries take: every count that varies at 0.
fn least_bits(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|node| match node {
            Node::Padding(bits) => *bits,
            Node::Field(field) => u64::from(field.kind.bits()),
            Node::Array(array) => match array.count {
                Counted::Fixed(count) => {
                    u64::from(count) * element_bits(&array.element, least_bits)
                }
                Counted::Field { .. } | Counted::Remaining => 0,
            },
            Node::Struct(node) => least_bits(&node.ty.entries),
        })
        .sum()
}

/// The most bits entries take: every count that varies at its most.
fn most_bits(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|node| match node {
            Node::Padding(bits) => *bits,
            Node::Field(field) => u64::from(field.kind.bits()),
            Node::Array(array) => {
                let count = match array.count {
                    Counted::Fixed(count) => count,
                    Counted::Field { .. } | Counted::Remaining => MOST_ELEMENTS,
                };
                u64::from(count) * element_bits(&array.element, most_bits)
            }
            Node::Struct(node) => most_bits(&node.ty.entries),
        })
        .sum()
}

/// An element's size: a scalar's, or `size` of a struct's entries.
fn element_bits(element: &ElementNode, size: fn(&[Node]) -> u64) -> u64 {
    match element {
        ElementNode::Scalar(element) => u64::from(element.kind.bits()),
        ElementNode::Struct(ty) => size(&ty.entries),
    }
}

/// The raw values a count field of `bits` can hold that give each array
/// it counts, by its `(multiply, add)`, a count of 0 to
/// [`MOST_ELEMENTS`]: the lowest and highest, or `None` if there are none.
fn raw_range(bits: u32, scales: &[(u32, i64)]) -> Option<(u64, u64)> {
    let largest = if bits >= 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    };
    let mut low = 0_i128;
    let mut high = i128::from(largest);
    for &(multiply, add) in scales {
        let (multiply, add) = (i128::from(multiply), i128::from(add));
        low = low.max((multiply - 1 - add).div_euclid(multiply));
        high = high.min((i128::from(MOST_ELEMENTS) - add).div_euclid(multiply));
    }
    if low > high {
        return None;
    }
    Some((u64::try_from(low).ok()?, u64::try_from(high).ok()?))
}

// ---- As IR JSON -----------------------------------------------------------

impl Body {
    /// Its entries, padding included.
    pub(super) fn entries(&self) -> Vec<Value> {
        self.nodes.iter().map(node_json).collect()
    }

    /// Every param it maps, as the catalog defines them.
    pub(super) fn params(&self) -> Vec<(String, Value)> {
        let mut params = Vec::new();
        params_of(&self.nodes, &mut params);
        params.extend(self.composites.iter().map(CompositeNode::param_json));
        params
    }

    /// Its struct types, by name.
    pub(super) fn types_json(&self) -> Vec<(String, Value)> {
        self.types
            .iter()
            .map(|ty| {
                let entries: Vec<Value> = ty.entries.iter().map(node_json).collect();
                (ty.name.clone(), json!({ "entries": entries }))
            })
            .collect()
    }

    /// Its composites, in the order the packet lists them.
    pub(super) fn composites_json(&self) -> Vec<Value> {
        self.listed
            .iter()
            .filter_map(|&index| self.composites.get(index))
            .map(CompositeNode::json)
            .collect()
    }
}

fn node_json(node: &Node) -> Value {
    match node {
        Node::Padding(bits) => json!({ "kind": "padding", "size_bits": bits }),
        Node::Field(field) => field_json(field),
        Node::Array(array) => {
            let element = match &array.element {
                ElementNode::Scalar(element) => {
                    let mut json = scalar_json(element);
                    json.insert("kind".into(), json!("scalar"));
                    Value::Object(json)
                }
                ElementNode::Struct(ty) => json!({ "kind": "struct", "name": ty.name }),
            };
            let count = match &array.count {
                Counted::Fixed(count) => json!({ "kind": "fixed", "count": count }),
                Counted::Field {
                    field,
                    multiply,
                    add,
                } => {
                    let mut count = Map::new();
                    count.insert("kind".into(), json!("field"));
                    count.insert("field".into(), json!(field));
                    if *multiply != 1 {
                        count.insert("multiply".into(), json!(multiply));
                    }
                    if *add != 0 {
                        count.insert("add".into(), json!(add));
                    }
                    Value::Object(count)
                }
                Counted::Remaining => json!({ "kind": "remaining" }),
            };
            let mut json = json!({ "kind": "array", "name": array.name, "count": count,
                                   "element": element });
            if let ElementNode::Scalar(Placed {
                param: Some(param), ..
            }) = &array.element
            {
                json["canonical"] = json!(param);
            }
            if let Some(interval) = array.interval {
                json["sample_interval_ms"] = json!(interval);
            }
            json
        }
        Node::Struct(node) => {
            json!({ "kind": "struct", "name": node.name, "struct": node.ty.name })
        }
    }
}

fn params_of(nodes: &[Node], params: &mut Vec<(String, Value)>) {
    let mut add = |field: &Placed| {
        if let Some(param) = &field.param {
            params.push((param.clone(), param_json(&field.meaning)));
        }
    };
    let mut inner = Vec::new();
    for node in nodes {
        match node {
            Node::Padding(_) => {}
            Node::Field(field) => add(field),
            Node::Array(array) => match &array.element {
                ElementNode::Scalar(element) => add(element),
                ElementNode::Struct(ty) => inner.push(&ty.entries),
            },
            Node::Struct(node) => inner.push(&node.ty.entries),
        }
    }
    for entries in inner {
        params_of(entries, params);
    }
}

// ---- Values ---------------------------------------------------------------

/// Values for a container's entries: each one's raw value, by name, in
/// order, its count fields holding counts its arrays can have.
pub(super) fn values(nodes: &[Node]) -> BoxedStrategy<Vec<(String, Raw)>> {
    let counts = count_ranges(nodes);
    let raws: Vec<BoxedStrategy<u64>> = counts
        .iter()
        .map(|(_, (low, high))| (*low..=*high).boxed())
        .collect();
    let nodes = nodes.to_vec();
    raws.prop_flat_map(move |raws| {
        let held: BTreeMap<String, u64> = counts
            .iter()
            .map(|(name, _)| name.clone())
            .zip(raws)
            .collect();
        let entries: Vec<BoxedStrategy<Option<(String, Raw)>>> =
            nodes.iter().map(|node| entry_value(node, &held)).collect();
        entries.prop_map(|entries| entries.into_iter().flatten().collect())
    })
    .boxed()
}

/// Each count field of a container, and the raw values it can hold.
fn count_ranges(nodes: &[Node]) -> Vec<(String, (u64, u64))> {
    let mut scales: BTreeMap<&str, Vec<(u32, i64)>> = BTreeMap::new();
    for node in nodes {
        if let Node::Array(ArrayNode {
            count:
                Counted::Field {
                    field,
                    multiply,
                    add,
                },
            ..
        }) = node
        {
            scales.entry(field).or_default().push((*multiply, *add));
        }
    }
    nodes
        .iter()
        .filter_map(|node| match node {
            Node::Field(field) => {
                let scales = scales.get(field.name.as_str())?;
                Some((field.name.clone(), raw_range(field.kind.bits(), scales)?))
            }
            _ => None,
        })
        .collect()
}

fn entry_value(node: &Node, held: &BTreeMap<String, u64>) -> BoxedStrategy<Option<(String, Raw)>> {
    let named = |name: &str, values: BoxedStrategy<Raw>| {
        let name = name.to_string();
        values
            .prop_map(move |raw| Some((name.clone(), raw)))
            .boxed()
    };
    match node {
        Node::Padding(_) => Just(None).boxed(),
        Node::Field(field) => match held.get(&field.name) {
            Some(raw) => named(&field.name, Just(Raw::Uint(*raw)).boxed()),
            None => named(&field.name, raw_value(field)),
        },
        Node::Array(array) => {
            let element = match &array.element {
                ElementNode::Scalar(element) => raw_value(element),
                ElementNode::Struct(ty) => values(&ty.entries).prop_map(Raw::Record).boxed(),
            };
            let items = match &array.count {
                Counted::Fixed(count) => vec(element, *count as usize).boxed(),
                Counted::Field {
                    field,
                    multiply,
                    add,
                } => {
                    let raw = held.get(field).copied().unwrap_or(0);
                    let count = i128::from(raw) * i128::from(*multiply) + i128::from(*add);
                    vec(element, usize::try_from(count).unwrap_or(0)).boxed()
                }
                Counted::Remaining => vec(element, 0..=MOST_ELEMENTS as usize).boxed(),
            };
            named(&array.name, items.prop_map(Raw::Array).boxed())
        }
        Node::Struct(node) => named(
            &node.name,
            values(&node.ty.entries).prop_map(Raw::Record).boxed(),
        ),
    }
}

// ---- Writing --------------------------------------------------------------

/// Writes a container's values from bit `start` (§2.1, §2.2), leaving
/// padding as it was; returns where the container ends.
pub(super) fn write(
    nodes: &[Node],
    record: &[(String, Raw)],
    payload: &mut [u8],
    start: u64,
) -> u64 {
    let mut at = start;
    let mut values = record.iter().map(|(_, raw)| raw);
    for node in nodes {
        if let Node::Padding(bits) = node {
            at += bits;
            continue;
        }
        let Some(raw) = values.next() else {
            break;
        };
        match (node, raw) {
            (Node::Field(field), raw) => at = put(field, raw, payload, at),
            (Node::Array(array), Raw::Array(items)) => {
                for item in items {
                    at = match (&array.element, item) {
                        (ElementNode::Scalar(element), raw) => put(element, raw, payload, at),
                        (ElementNode::Struct(ty), Raw::Record(fields)) => {
                            write(&ty.entries, fields, payload, at)
                        }
                        _ => at,
                    };
                }
            }
            (Node::Struct(node), Raw::Record(fields)) => {
                at = write(&node.ty.entries, fields, payload, at);
            }
            _ => {}
        }
    }
    at
}

/// Writes one scalar at `at`; returns where it ends.
fn put(field: &Placed, raw: &Raw, payload: &mut [u8], at: u64) -> u64 {
    let bits = field.kind.bits();
    match (&field.kind, raw) {
        (_, Raw::Uint(n)) => put_bits(payload, at, bits, *n, field.little),
        (_, Raw::Int(n)) => put_bits(payload, at, bits, fit(*n as u64, bits), field.little),
        (Kind::Float(32), Raw::Float(x)) => put_bits(
            payload,
            at,
            32,
            u64::from((*x as f32).to_bits()),
            field.little,
        ),
        (_, Raw::Float(x)) => put_bits(payload, at, 64, x.to_bits(), field.little),
        (_, Raw::Bool(b)) => put_bits(payload, at, 1, u64::from(*b), false),
        (
            Kind::Text {
                bytes,
                text,
                terminator,
            },
            Raw::Str(value),
        ) => {
            let encoded = encode(value, *text);
            let start = (at / 8) as usize;
            payload[start..start + encoded.len()].copy_from_slice(&encoded);
            if let Some(byte) = terminator
                && encoded.len() < *bytes as usize
            {
                payload[start + encoded.len()] = *byte;
            }
        }
        (_, Raw::Bytes(value)) => {
            let start = (at / 8) as usize;
            payload[start..start + value.len()].copy_from_slice(value);
        }
        (_, Raw::Str(_) | Raw::Array(_) | Raw::Record(_)) => {}
    }
    at + u64::from(bits)
}

// ---- Samples --------------------------------------------------------------

impl Body {
    /// The samples a message's values give (§7.3): each mapped field and
    /// scalar element in record order, then the composites by name (§7.4).
    pub(super) fn samples(&self, record: &[(String, Raw)]) -> Vec<Sample> {
        let mut samples = Vec::new();
        samples_of(&self.nodes, record, "", None, &mut samples);
        let mut composites: Vec<&CompositeNode> = self.composites.iter().collect();
        composites.sort_by(|a, b| a.param().cmp(b.param()));
        samples.extend(composites.into_iter().filter_map(|c| c.sample(record)));
        samples
    }
}

/// Samples of a container's values, at paths under `prefix`, inside an
/// array element at `index` (the innermost array's).
fn samples_of(
    nodes: &[Node],
    record: &[(String, Raw)],
    prefix: &str,
    index: Option<u64>,
    samples: &mut Vec<Sample>,
) {
    let entries = nodes
        .iter()
        .filter(|node| !matches!(node, Node::Padding(_)));
    for (node, (name, raw)) in entries.zip(record) {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match (node, raw) {
            (Node::Field(field), raw) => samples.extend(sample(field, raw, path, index, None)),
            (Node::Array(array), Raw::Array(items)) => {
                for (position, item) in (0_u64..).zip(items) {
                    let at = format!("{path}[{position}]");
                    match (&array.element, item) {
                        (ElementNode::Scalar(element), raw) => {
                            let offset = array.interval.map(|ms| offset_ns(position, ms));
                            samples.extend(sample(element, raw, at, Some(position), offset));
                        }
                        (ElementNode::Struct(ty), Raw::Record(fields)) => {
                            samples_of(&ty.entries, fields, &at, Some(position), samples);
                        }
                        _ => {}
                    }
                }
            }
            (Node::Struct(node), Raw::Record(fields)) => {
                samples_of(&node.ty.entries, fields, &path, index, samples);
            }
            _ => {}
        }
    }
}

/// How long after the packet's event time element `position` of a block
/// was taken, in whole nanoseconds: `round(i × interval_ms × 1000000)` in
/// binary64, left to right, ties away from zero (§7.3).
fn offset_ns(position: u64, interval_ms: f64) -> i64 {
    (position as f64 * interval_ms * 1_000_000.0).round() as i64
}

mod composite;
#[cfg(test)]
mod tests;
