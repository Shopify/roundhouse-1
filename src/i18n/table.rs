//! Compact storage for a loaded catalog's keys, values and node order.
//!
//! An app with many locales loads the same dotted keys once per locale:
//! the keys and the order of each node's children are nearly identical
//! from one locale to the next, and only the values differ. While a
//! catalog is loading, [`Entries`] and [`Order`] are ordinary
//! `BTreeMap`s, since the deep merge inserts and removes keys. Once every
//! locale is loaded, [`super::Catalog::freeze`] moves them into this
//! packed form: one sorted [`KeyTable`] shared by every locale, and per
//! locale only the indices of the keys it has, its values in one buffer,
//! and its nodes' children as key indices.
//!
//! Both forms answer the same reads in the same order (keys sort as the
//! `BTreeMap` sorts `String`s, by bytes), serialize to the same shape, and
//! compare equal when they hold the same entries.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Every key a set of frozen catalogs uses, sorted by bytes, in one
/// buffer.
#[derive(Debug, Default)]
pub struct KeyTable {
    text: String,
    /// `ends[i]` is where key `i` ends in `text`; it starts where key
    /// `i - 1` ends.
    ends: Vec<u32>,
}

impl KeyTable {
    /// A table of `keys`, which must be sorted and unique.
    pub fn new<'a>(keys: impl IntoIterator<Item = &'a str>) -> KeyTable {
        let mut table = KeyTable::default();
        for key in keys {
            debug_assert!(table.ends.is_empty() || table.key(table.ends.len() as u32 - 1) < key, "keys are sorted and unique");
            table.text.push_str(key);
            table.ends.push(offset(table.text.len()));
        }
        table.text.shrink_to_fit();
        table.ends.shrink_to_fit();
        table
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn key(&self, index: u32) -> &str {
        let index = index as usize;
        let start = if index == 0 { 0 } else { self.ends[index - 1] as usize };
        &self.text[start..self.ends[index] as usize]
    }

    /// The index of `key`.
    pub fn find(&self, key: &str) -> Option<u32> {
        self.search(key).ok()
    }

    /// The index of `key`, or where it would sort.
    fn search(&self, key: &str) -> Result<u32, u32> {
        let (mut low, mut high) = (0usize, self.ends.len());
        while low < high {
            let middle = low + (high - low) / 2;
            match self.key(middle as u32).cmp(key) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Ok(middle as u32),
            }
        }
        Err(low as u32)
    }

    /// The first index whose key sorts at or after `key`.
    fn lower_bound(&self, key: &str) -> u32 {
        match self.search(key) {
            Ok(index) | Err(index) => index,
        }
    }
}

fn offset(position: usize) -> u32 {
    u32::try_from(position).expect("a locale catalog is smaller than 4 GiB")
}

/// A catalog's string leaves: dotted key to text.
#[derive(Clone)]
pub enum Entries {
    Building(BTreeMap<String, String>),
    Frozen(FrozenEntries),
}

#[derive(Clone)]
pub struct FrozenEntries {
    keys: Arc<KeyTable>,
    /// The key indices this catalog has, ascending.
    present: Box<[u32]>,
    text: Box<str>,
    /// `ends[i]` is where the value of `present[i]` ends in `text`.
    ends: Box<[u32]>,
}

impl FrozenEntries {
    fn value(&self, slot: usize) -> &str {
        let start = if slot == 0 { 0 } else { self.ends[slot - 1] as usize };
        &self.text[start..self.ends[slot] as usize]
    }

    fn slot(&self, key: &str) -> Option<usize> {
        let index = self.keys.find(key)?;
        self.present.binary_search(&index).ok()
    }

    fn pair(&self, slot: usize) -> (&str, &str) {
        (self.keys.key(self.present[slot]), self.value(slot))
    }
}

impl Default for Entries {
    fn default() -> Self {
        Entries::Building(BTreeMap::new())
    }
}

impl Entries {
    pub fn is_empty(&self) -> bool {
        match self {
            Entries::Building(map) => map.is_empty(),
            Entries::Frozen(frozen) => frozen.present.is_empty(),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Entries::Building(map) => map.len(),
            Entries::Frozen(frozen) => frozen.present.len(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        match self {
            Entries::Building(map) => map.get(key).map(String::as_str),
            Entries::Frozen(frozen) => frozen.slot(key).map(|slot| frozen.value(slot)),
        }
    }

    /// The stored key equal to `key`, with its value.
    pub fn get_key_value(&self, key: &str) -> Option<(&str, &str)> {
        match self {
            Entries::Building(map) => map.get_key_value(key).map(|(k, v)| (k.as_str(), v.as_str())),
            Entries::Frozen(frozen) => frozen.slot(key).map(|slot| frozen.pair(slot)),
        }
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Every entry, in key order.
    pub fn iter(&self) -> Box<dyn Iterator<Item = (&str, &str)> + '_> {
        match self {
            Entries::Building(map) => Box::new(map.iter().map(|(k, v)| (k.as_str(), v.as_str()))),
            Entries::Frozen(frozen) => Box::new((0..frozen.present.len()).map(|slot| frozen.pair(slot))),
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.iter().map(|(key, _)| key)
    }

    /// The entries whose keys sort at or after `start`, in key order: the
    /// `BTreeMap::range(start..)` the prefix scans walk.
    pub fn range_from(&self, start: &str) -> Box<dyn Iterator<Item = (&str, &str)> + '_> {
        match self {
            Entries::Building(map) => Box::new(map.range::<str, _>((std::ops::Bound::Included(start), std::ops::Bound::Unbounded)).map(|(k, v)| (k.as_str(), v.as_str()))),
            Entries::Frozen(frozen) => {
                let first = frozen.keys.lower_bound(start);
                let slot = frozen.present.partition_point(|index| *index < first);
                Box::new((slot..frozen.present.len()).map(|slot| frozen.pair(slot)))
            }
        }
    }

    /// The map a load merges into. A frozen catalog is unpacked first.
    pub fn building(&mut self) -> &mut BTreeMap<String, String> {
        if let Entries::Frozen(_) = self {
            let map = self.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            *self = Entries::Building(map);
        }
        match self {
            Entries::Building(map) => map,
            Entries::Frozen(_) => unreachable!("unpacked above"),
        }
    }

    /// Every key, for building a shared [`KeyTable`].
    pub fn key_set<'a>(&'a self, into: &mut std::collections::BTreeSet<&'a str>) {
        into.extend(self.keys());
    }

    /// The packed form, against `keys`, which holds every key here.
    pub fn freeze(&self, keys: &Arc<KeyTable>) -> Entries {
        let mut present = Vec::with_capacity(self.len());
        let mut text = String::new();
        let mut ends = Vec::with_capacity(self.len());
        for (key, value) in self.iter() {
            present.push(keys.find(key).expect("the shared table holds every key"));
            text.push_str(value);
            ends.push(offset(text.len()));
        }
        Entries::Frozen(FrozenEntries {
            keys: Arc::clone(keys),
            present: present.into_boxed_slice(),
            text: text.into_boxed_str(),
            ends: ends.into_boxed_slice(),
        })
    }
}

impl PartialEq for Entries {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl fmt::Debug for Entries {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl FromIterator<(String, String)> for Entries {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Entries::Building(iter.into_iter().collect())
    }
}

impl Serialize for Entries {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        BTreeMap::deserialize(deserializer).map(Entries::Building)
    }
}

/// Each node's children in the order the files give them ("" for the
/// locale's root), as the deep merge of Ruby Hashes keeps it.
#[derive(Clone)]
pub enum Order {
    Building(BTreeMap<String, Vec<String>>),
    Frozen(FrozenOrder),
}

#[derive(Clone)]
pub struct FrozenOrder {
    keys: Arc<KeyTable>,
    /// The node key indices, ascending.
    nodes: Box<[u32]>,
    /// The key index of each child's full path, node after node.
    children: Box<[u32]>,
    /// `ends[i]` is where the children of `nodes[i]` end in `children`.
    ends: Box<[u32]>,
}

impl FrozenOrder {
    fn slot(&self, node: &str) -> Option<usize> {
        let index = self.keys.find(node)?;
        self.nodes.binary_search(&index).ok()
    }

    fn children(&self, slot: usize) -> Children<'_> {
        let start = if slot == 0 { 0 } else { self.ends[slot - 1] as usize };
        let node_len = self.keys.key(self.nodes[slot]).len();
        Children::Frozen { keys: &self.keys, indices: &self.children[start..self.ends[slot] as usize], skip: if node_len == 0 { 0 } else { node_len + 1 } }
    }
}

/// A node's child names, in order.
pub enum Children<'a> {
    Building(&'a [String]),
    Frozen { keys: &'a KeyTable, indices: &'a [u32], skip: usize },
}

impl<'a> Children<'a> {
    pub fn is_empty(&self) -> bool {
        match self {
            Children::Building(names) => names.is_empty(),
            Children::Frozen { indices, .. } => indices.is_empty(),
        }
    }

    pub fn iter(&self) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        match *self {
            Children::Building(names) => Box::new(names.iter().map(String::as_str)),
            Children::Frozen { keys, indices, skip } => Box::new(indices.iter().map(move |index| &keys.key(*index)[skip..])),
        }
    }
}

impl Default for Order {
    fn default() -> Self {
        Order::Building(BTreeMap::new())
    }
}

impl Order {
    pub fn is_empty(&self) -> bool {
        match self {
            Order::Building(map) => map.is_empty(),
            Order::Frozen(frozen) => frozen.nodes.is_empty(),
        }
    }

    pub fn contains_key(&self, node: &str) -> bool {
        self.get(node).is_some()
    }

    pub fn get(&self, node: &str) -> Option<Children<'_>> {
        match self {
            Order::Building(map) => map.get(node).map(|names| Children::Building(names)),
            Order::Frozen(frozen) => frozen.slot(node).map(|slot| frozen.children(slot)),
        }
    }

    /// Every node with its children, in key order.
    pub fn iter(&self) -> Box<dyn Iterator<Item = (&str, Children<'_>)> + '_> {
        match self {
            Order::Building(map) => Box::new(map.iter().map(|(node, names)| (node.as_str(), Children::Building(names)))),
            Order::Frozen(frozen) => Box::new((0..frozen.nodes.len()).map(|slot| (frozen.keys.key(frozen.nodes[slot]), frozen.children(slot)))),
        }
    }

    /// The map a load merges into. A frozen order is unpacked first.
    pub fn building(&mut self) -> &mut BTreeMap<String, Vec<String>> {
        if let Order::Frozen(_) = self {
            let map = self.iter().map(|(node, children)| (node.to_string(), children.iter().map(str::to_string).collect())).collect();
            *self = Order::Building(map);
        }
        match self {
            Order::Building(map) => map,
            Order::Frozen(_) => unreachable!("unpacked above"),
        }
    }

    /// Every node key and every child's full path, for building a shared
    /// [`KeyTable`].
    pub fn key_set(&self, into: &mut std::collections::BTreeSet<String>) {
        for (node, children) in self.iter() {
            into.insert(node.to_string());
            for child in children.iter() {
                into.insert(child_path(node, child));
            }
        }
    }

    /// The packed form, against `keys`, which holds every node and child
    /// path here.
    pub fn freeze(&self, keys: &Arc<KeyTable>) -> Order {
        let mut nodes = Vec::new();
        let mut children = Vec::new();
        let mut ends = Vec::new();
        for (node, names) in self.iter() {
            nodes.push(keys.find(node).expect("the shared table holds every node"));
            for name in names.iter() {
                children.push(keys.find(&child_path(node, name)).expect("the shared table holds every child path"));
            }
            ends.push(offset(children.len()));
        }
        Order::Frozen(FrozenOrder {
            keys: Arc::clone(keys),
            nodes: nodes.into_boxed_slice(),
            children: children.into_boxed_slice(),
            ends: ends.into_boxed_slice(),
        })
    }
}

/// The dotted key of `name` under `node`.
pub fn child_path(node: &str, name: &str) -> String {
    if node.is_empty() { name.to_string() } else { format!("{node}.{name}") }
}

impl PartialEq for Order {
    fn eq(&self, other: &Self) -> bool {
        let flat = |order: &Order| -> Vec<(String, Vec<String>)> {
            order.iter().map(|(node, children)| (node.to_string(), children.iter().map(str::to_string).collect())).collect()
        };
        flat(self) == flat(other)
    }
}

impl fmt::Debug for Order {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter().map(|(node, children)| (node, children.iter().collect::<Vec<_>>()))).finish()
    }
}

impl Serialize for Order {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter().map(|(node, children)| (node, children.iter().collect::<Vec<_>>())))
    }
}

impl<'de> Deserialize<'de> for Order {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        BTreeMap::deserialize(deserializer).map(Order::Building)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn building(pairs: &[(&str, &str)]) -> Entries {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn table(entries: &[&Entries], order: &[&Order]) -> Arc<KeyTable> {
        let mut keys = std::collections::BTreeSet::new();
        for e in entries {
            e.key_set(&mut keys);
        }
        let mut owned = std::collections::BTreeSet::new();
        for o in order {
            o.key_set(&mut owned);
        }
        let all: std::collections::BTreeSet<&str> = keys.into_iter().chain(owned.iter().map(String::as_str)).collect();
        Arc::new(KeyTable::new(all))
    }

    #[test]
    fn frozen_entries_answer_every_read_as_the_map_does() {
        let en = building(&[("a", "1"), ("a.b", "2"), ("a.c", ""), ("b", "3"), ("ba", "4")]);
        let fr = building(&[("a.b", "deux"), ("ba", "quatre"), ("z", "fin")]);
        let keys = table(&[&en, &fr], &[]);
        for map in [&en, &fr] {
            let frozen = map.freeze(&keys);
            assert_eq!(&frozen, map);
            for key in ["", "a", "a.", "a.b", "a.c", "a.d", "b", "ba", "bb", "z", "zz"] {
                assert_eq!(frozen.get(key), map.get(key), "get {key:?}");
                assert_eq!(frozen.get_key_value(key), map.get_key_value(key), "get_key_value {key:?}");
                assert_eq!(frozen.range_from(key).collect::<Vec<_>>(), map.range_from(key).collect::<Vec<_>>(), "from {key:?}");
            }
            assert_eq!(frozen.iter().collect::<Vec<_>>(), map.iter().collect::<Vec<_>>());
            assert_eq!(serde_json::to_string(&frozen).unwrap(), serde_json::to_string(map).unwrap());
        }
    }

    #[test]
    fn frozen_order_keeps_each_nodes_children_in_file_order() {
        let mut order = Order::default();
        let map = order.building();
        map.insert(String::new(), vec!["b".into(), "a".into()]);
        map.insert("a".into(), vec!["z".into(), "y".into()]);
        map.insert("a.y".into(), Vec::new());
        map.insert("b".into(), vec!["only".into()]);
        let keys = table(&[], &[&order]);
        let frozen = order.freeze(&keys);
        assert_eq!(frozen, order);
        for node in ["", "a", "a.y", "b", "c"] {
            let names = |o: &Order| o.get(node).map(|c| c.iter().map(str::to_string).collect::<Vec<_>>());
            assert_eq!(names(&frozen), names(&order), "children of {node:?}");
        }
        assert!(frozen.get("a.y").is_some_and(|c| c.is_empty()));
        assert_eq!(serde_json::to_string(&frozen).unwrap(), serde_json::to_string(&order).unwrap());
    }

    #[test]
    fn a_frozen_catalog_unpacks_for_another_merge() {
        let en = building(&[("a", "1"), ("b", "2")]);
        let keys = table(&[&en], &[]);
        let mut frozen = en.freeze(&keys);
        frozen.building().insert("c".into(), "3".into());
        assert_eq!(frozen.get("c"), Some("3"));
        assert_eq!(frozen.get("a"), Some("1"));
    }
}
