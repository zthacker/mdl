//! Bindings files (`spec/bindings.md`), with what they mean: rows of two
//! families, each native id written as an integer, a wide integer's
//! decimal string, a negative integer or text, binding assets to sets or
//! retiring ids at instants with fractions. [`resolve`] is §4's timeline;
//! [`expected`] is the diagnostics §1, §3, §4 and §7 call for: an integer
//! spelled as text or too wide for a number, an empty id or asset, a
//! family or set that isn't a name, a row that neither binds nor retires
//! or retires yet names an asset or set (which then takes no part in its
//! timeline), `retired = false`, a non-canonical instant (whose row takes
//! no part either), and every row but the first of those sharing a family,
//! native id and instant.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;

use crate::layout::{instant_text, label};

/// A native id as a file writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Id {
    /// Within ±(2^53 − 1), written as an integer.
    Int(i64),
    /// Past 2^53 − 1, written as its decimal string, as §3 requires.
    Wide(u64),
    Text(String),
    /// An integer within ±(2^53 − 1) written as text, which §3 refuses
    /// (`integer_as_string`) but which still means that integer.
    IntAsText(i64),
    /// An integer past 2^53 − 1 written as a number, which §3 refuses
    /// (`unsafe_integer`) but which still means that integer. It lies
    /// within i64, past which TOML has no integer and the file no meaning.
    BareWide(u64),
}

/// A native id as a reader compares it (§3.2): its integer, or its text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Int(i128),
    Text(String),
}

impl Id {
    fn toml(&self) -> String {
        match self {
            Id::Int(n) => n.to_string(),
            Id::Wide(n) => format!("\"{n}\""),
            Id::Text(text) => format!("\"{text}\""),
            Id::IntAsText(n) => format!("\"{n}\""),
            Id::BareWide(n) => n.to_string(),
        }
    }

    pub fn key(&self) -> Key {
        match self {
            Id::Int(n) | Id::IntAsText(n) => Key::Int(i128::from(*n)),
            Id::Wide(n) | Id::BareWide(n) => Key::Int(i128::from(*n)),
            Id::Text(text) => Key::Text(text.clone()),
        }
    }
}

/// When a row takes effect: an instant, or a spelling MDL refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Since {
    At(i128),
    Bad(&'static str),
}

/// One row of a bindings file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub family: &'static str,
    pub id: Id,
    pub asset: Option<String>,
    pub set: Option<String>,
    /// `retired`, if the row writes it.
    pub retired: Option<bool>,
    pub since: Since,
}

impl Row {
    fn bound(family: &'static str, id: Id, asset: String, set: String, at: i128) -> Row {
        Row {
            family,
            id,
            asset: Some(asset),
            set: Some(set),
            retired: None,
            since: Since::At(at),
        }
    }

    /// Whether its meaning is unclear (§7): it neither binds an asset nor
    /// retires the id, or it retires the id yet names an asset or set.
    fn unclear(&self) -> bool {
        let retired = self.retired == Some(true);
        (self.asset.is_none() && !retired)
            || (retired && (self.asset.is_some() || self.set.is_some()))
    }

    /// The instant it takes part in its timeline from, if it does.
    fn in_timeline(&self) -> Option<i128> {
        match self.since {
            Since::At(at) if !self.unclear() => Some(at),
            _ => None,
        }
    }

    /// What it says: an asset and maybe a set, or retirement.
    fn binding(&self) -> Option<(String, Option<String>)> {
        match (self.retired, &self.asset) {
            (Some(true), _) | (_, None) => None,
            (_, Some(asset)) => Some((asset.clone(), self.set.clone())),
        }
    }
}

/// The file's text, rows in order.
pub fn text(rows: &[Row]) -> String {
    let mut text = String::from("mdl_version = \"2.0\"\n");
    for row in rows {
        text.push_str(&format!(
            "\n[[binding]]\nfamily = \"{}\"\nnative_id = {}\n",
            row.family,
            row.id.toml()
        ));
        if let Some(asset) = &row.asset {
            text.push_str(&format!("asset = \"{asset}\"\n"));
        }
        if let Some(set) = &row.set {
            text.push_str(&format!("set = \"{set}\"\n"));
        }
        if let Some(retired) = row.retired {
            text.push_str(&format!("retired = {retired}\n"));
        }
        let from = match &row.since {
            Since::At(at) => instant_text(*at),
            Since::Bad(text) => (*text).to_string(),
        };
        text.push_str(&format!("effective_from = \"{from}\"\n"));
    }
    text
}

/// The asset and set a native id of a family is bound to at an instant
/// (§4): the row in effect, the last of its timeline from at or before
/// it, whose order in the file doesn't matter.
pub fn resolve(
    rows: &[Row],
    family: &str,
    key: &Key,
    at: i128,
) -> Option<(String, Option<String>)> {
    rows.iter()
        .filter_map(|row| match row.in_timeline() {
            Some(from) if row.family == family && &row.id.key() == key && from <= at => {
                Some((from, row))
            }
            _ => None,
        })
        .max_by_key(|(from, _)| *from)
        .and_then(|(_, row)| row.binding())
}

/// Every family and native id the rows name.
pub fn ids(rows: &[Row]) -> Vec<(&'static str, Key)> {
    let mut ids: Vec<(&'static str, Key)> =
        rows.iter().map(|row| (row.family, row.id.key())).collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Every instant worth resolving at: each row's, and 1 ns either side.
pub fn instants(rows: &[Row]) -> Vec<i128> {
    rows.iter()
        .filter_map(|row| match row.since {
            Since::At(at) => Some([at - 1, at, at + 1]),
            Since::Bad(_) => None,
        })
        .flatten()
        .collect()
}

/// Whether a family or set name is one (§1): lowercase letters, digits and
/// `_`, starting with a letter.
fn is_name(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_lowercase())
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The diagnostics a reader gives, by code and path, sorted.
pub fn expected(rows: &[Row]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let at = |key: &str| format!("/binding/{index}/{key}");
        let mut note = |code: &str, path: String| found.push((code.to_string(), path));
        if !is_name(row.family) {
            note("invalid_name", at("family"));
        }
        if row.asset.as_deref() == Some("") {
            note("empty", at("asset"));
        }
        if let Some(set) = &row.set
            && !is_name(set)
        {
            note("invalid_name", at("set"));
        }
        match &row.id {
            Id::IntAsText(_) => note("integer_as_string", at("native_id")),
            Id::BareWide(_) => note("unsafe_integer", at("native_id")),
            Id::Text(text) if text.is_empty() => note("empty", at("native_id")),
            _ => {}
        }
        let retired = row.retired == Some(true);
        if row.asset.is_none() && !retired {
            note("binding_shape", format!("/binding/{index}"));
        }
        if retired && row.asset.is_some() {
            note("binding_shape", at("asset"));
        }
        if retired && row.set.is_some() {
            note("binding_shape", at("set"));
        }
        if row.retired == Some(false) {
            note("explicit_default", at("retired"));
        }
        if let Since::Bad(_) = row.since {
            note("invalid_instant", at("effective_from"));
        }
        // Every row but the first sharing a family, native id and instant,
        // of those that take part in a timeline.
        if let Some(since) = row.in_timeline() {
            let earlier = rows[..index].iter().any(|other| {
                other.family == row.family
                    && other.id.key() == row.id.key()
                    && other.in_timeline() == Some(since)
            });
            if earlier {
                note("duplicate_binding", at("effective_from"));
            }
        }
    }
    found.sort();
    found
}

/// A file no reader should refuse: two families, a few native ids each
/// in every written form, timelines of one to four rows at distinct
/// instants (to the millisecond), some retirements, in any order.
pub fn valid() -> BoxedStrategy<Vec<Row>> {
    let id = prop_oneof![
        (-1000_i64..1000).prop_map(Id::Int),
        ((1_u64 << 53)..=u64::MAX).prop_map(Id::Wide),
        select(&["SN-7", "007", "box-1", "ü"][..]).prop_map(|text| Id::Text(text.to_string())),
    ];
    let timeline = (
        select(&["fam_a", "fam_b"][..]),
        id,
        vec((0_i64..1_000_000_000, any::<bool>(), 0_u8..3), 1..=4),
    )
        .prop_map(|(family, id, changes)| {
            let start = label((2020, 1, 1), (0, 0, 0), 0);
            let mut froms: Vec<i64> = changes.iter().map(|(millis, _, _)| *millis).collect();
            froms.sort_unstable();
            froms.dedup();
            froms
                .into_iter()
                .zip(changes)
                .map(|(millis, (_, retired, asset))| {
                    let at = start + i128::from(millis) * 1_000_000;
                    if retired {
                        Row {
                            family,
                            id: id.clone(),
                            asset: None,
                            set: None,
                            retired: Some(true),
                            since: Since::At(at),
                        }
                    } else {
                        Row::bound(
                            family,
                            id.clone(),
                            format!("A{asset}"),
                            format!("s{asset}"),
                            at,
                        )
                    }
                })
                .collect::<Vec<Row>>()
        });
    vec(timeline, 1..=4)
        .prop_map(|timelines| {
            // One timeline per family and native id.
            let mut seen: Vec<(&str, Key)> = Vec::new();
            timelines
                .into_iter()
                .filter(|rows| {
                    let key = (rows[0].family, rows[0].id.key());
                    let new = !seen.contains(&key);
                    seen.push(key);
                    new
                })
                .flatten()
                .collect::<Vec<Row>>()
        })
        .prop_shuffle()
        .boxed()
}

/// A valid file with problems planted at random places, each a copy of a
/// row of the file, so it shares that row's family, native id and instant
/// unless the problem changes them, and is a duplicate if it still takes
/// part in that timeline. A third of them go in twice, so that whether a
/// row takes part shows for every problem, even one that changes the id:
/// - a duplicate (some spelling the id as its integer's text, which is
///   still that id);
/// - an instant MDL doesn't spell;
/// - a wide id written as a number (the same id, if the row's was wide);
/// - an empty id, an empty asset, or a family or set that isn't a name;
/// - a row that neither binds nor retires, or retires yet names an asset
///   or a set, whose meaning is unclear, so it can't be a duplicate;
/// - `retired = false` with an asset, which still binds.
pub fn faulty() -> BoxedStrategy<Vec<Row>> {
    valid()
        .prop_flat_map(|rows| {
            let count = rows.len();
            let planted = vec(
                (
                    0..count,
                    0_u8..11,
                    select(
                        &[
                            "2026-06-01T12:00:00.250Z",
                            "2026-06-01T14:00:00+02:00",
                            "2026-06-01t12:00:00z",
                            "2026-06-01T23:59:60Z",
                        ][..],
                    ),
                    any::<usize>(),
                ),
                1..=4,
            );
            (Just(rows), planted)
        })
        .prop_map(|(mut rows, planted)| {
            for (source, kind, bad, at) in planted {
                let mut row = rows[source].clone();
                match kind {
                    0 => {
                        row.asset = Some("DUP".into());
                        row.set = None;
                        row.retired = None;
                        if let Id::Int(n) = row.id
                            && at % 2 == 0
                        {
                            row.id = Id::IntAsText(n);
                        }
                    }
                    1 => row.since = Since::Bad(bad),
                    2 => {
                        row.id = match row.id {
                            Id::Wide(n) if i64::try_from(n).is_ok() => Id::BareWide(n),
                            _ => Id::BareWide((1 << 53) + (at as u64 % 1000)),
                        }
                    }
                    3 => row.id = Id::Text(String::new()),
                    4 => {
                        row.asset = None;
                        row.set = None;
                        row.retired = None;
                    }
                    5 => {
                        row.asset = Some("GONE".into());
                        row.retired = Some(true);
                    }
                    6 => {
                        row.asset = None;
                        row.set = Some("s9".into());
                        row.retired = Some(true);
                    }
                    7 => {
                        row.asset = Some("KEPT".into());
                        row.retired = Some(false);
                    }
                    8 => {
                        row.asset = Some(String::new());
                        row.set = None;
                        row.retired = None;
                    }
                    9 => {
                        row.asset = Some("A0".into());
                        row.set = Some("Set-0".into());
                        row.retired = None;
                    }
                    _ => row.family = "Fam_a",
                }
                if at % 3 == 0 {
                    let place = (at / 3) % (rows.len() + 1);
                    rows.insert(place, row.clone());
                }
                let place = at % (rows.len() + 1);
                rows.insert(place, row);
            }
            rows
        })
        .boxed()
}
