//! Transaction-local overlays over disposable canonical indexes. Only touched
//! keys are cloned; a failed plan never mutates the index.
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
pub(super) struct OverlayMap<'a, K, V> {
    base: &'a HashMap<K, V>,
    changes: HashMap<K, V>,
}
impl<'a, K: Eq + Hash + Clone, V: Clone> OverlayMap<'a, K, V> {
    pub fn new(base: &'a HashMap<K, V>) -> Self {
        Self {
            base,
            changes: HashMap::new(),
        }
    }
    pub fn get(&self, key: &K) -> Option<&V> {
        self.changes.get(key).or_else(|| self.base.get(key))
    }
    pub fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        if !self.changes.contains_key(key) {
            let value = self.base.get(key)?.clone();
            self.changes.insert(key.clone(), value);
        }
        self.changes.get_mut(key)
    }
    pub fn insert(&mut self, key: K, value: V) {
        self.changes.insert(key, value);
    }
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.changes.iter().chain(
            self.base
                .iter()
                .filter(|(key, _)| !self.changes.contains_key(*key)),
        )
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }
    pub fn into_changes(self) -> HashMap<K, V> {
        self.changes
    }
}
pub(super) struct OverlaySet<'a, K> {
    base: &'a HashSet<K>,
    changes: HashSet<K>,
}
impl<'a, K: Eq + Hash> OverlaySet<'a, K> {
    pub fn new(base: &'a HashSet<K>) -> Self {
        Self {
            base,
            changes: HashSet::new(),
        }
    }
    pub fn contains(&self, key: &K) -> bool {
        self.changes.contains(key) || self.base.contains(key)
    }
    pub fn insert(&mut self, key: K) -> bool {
        if self.contains(&key) {
            false
        } else {
            self.changes.insert(key)
        }
    }
    pub fn into_changes(self) -> HashSet<K> {
        self.changes
    }
}
