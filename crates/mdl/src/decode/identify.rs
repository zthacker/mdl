//! Identification: which packet of a set a message is.

use std::collections::BTreeMap;
use std::ops::Bound;

use crate::validate::{KeyId, KeyValue};

/// Packets grouped by the keys they read, most keys first. At most one
/// packet of a group can match a message (validation guarantees it), so
/// each group is one lookup, however many packets share its keys.
#[derive(Debug, Clone)]
pub(super) struct Index {
    groups: Vec<Group>,
}

#[derive(Debug, Clone)]
struct Group {
    /// In `KeyId` order: metadata, pointers, bits, byte runs, then keys
    /// that need a trial decode, so the costly ones come last.
    keys: Vec<KeyId>,
    /// The values each packet's keys must have, in `keys` order.
    packets: BTreeMap<Vec<KeyValue>, usize>,
}

impl Group {
    /// Does some packet's key values start with `prefix`?
    fn has_prefix(&self, prefix: &[KeyValue]) -> bool {
        self.packets
            .range::<[KeyValue], _>((Bound::Included(prefix), Bound::Unbounded))
            .next()
            .is_some_and(|(values, _)| values.starts_with(prefix))
    }
}

impl Index {
    /// `identities[i]` is packet `i`'s keys and values.
    pub(super) fn new(identities: Vec<BTreeMap<KeyId, KeyValue>>) -> Index {
        let mut by_keys: BTreeMap<Vec<KeyId>, BTreeMap<Vec<KeyValue>, usize>> = BTreeMap::new();
        for (packet, identity) in identities.into_iter().enumerate() {
            let (keys, values): (Vec<KeyId>, Vec<KeyValue>) = identity.into_iter().unzip();
            by_keys.entry(keys).or_default().insert(values, packet);
        }
        let mut groups: Vec<Group> = by_keys
            .into_iter()
            .map(|(keys, packets)| Group { keys, packets })
            .collect();
        // Groups with equally many keys never match one message, so their
        // order among themselves doesn't matter.
        groups.sort_by_key(|group| std::cmp::Reverse(group.keys.len()));
        Index { groups }
    }

    /// The packet whose keys all match, trying the most keys first.
    /// `evaluate` gives a key's value in the message, or `None` if it can't
    /// be read. Each key is evaluated at most once, and a group stops
    /// evaluating as soon as no packet in it has the values read so far.
    pub(super) fn find(
        &self,
        evaluate: &mut dyn FnMut(&KeyId) -> Option<KeyValue>,
    ) -> Option<usize> {
        let mut known: Vec<(&KeyId, Option<KeyValue>)> = Vec::new();
        'groups: for group in &self.groups {
            let mut values = Vec::with_capacity(group.keys.len());
            for key in &group.keys {
                let value = match known.iter().find(|(seen, _)| *seen == key) {
                    Some((_, value)) => value.clone(),
                    None => {
                        let value = evaluate(key);
                        known.push((key, value.clone()));
                        value
                    }
                };
                match value {
                    Some(value) => values.push(value),
                    None => continue 'groups,
                }
                if !group.has_prefix(&values) {
                    continue 'groups;
                }
            }
            if let Some(&packet) = group.packets.get(&values) {
                return Some(packet);
            }
        }
        None
    }
}
