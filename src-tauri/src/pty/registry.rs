//! Shared PTY registry for agent and build/run processes.
//!
//! Provides a generic, thread-safe, in-process store for live PTY handles.
//! Used by both the agent system (PROCESS_REGISTRY) and the build/run system.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A thread-safe registry mapping keys to PTY process handles.
/// The entire map is protected by a single Mutex, and values are Arcs
/// so callers can hold independent references.
pub struct PtyRegistry<K, V> {
    processes: Arc<Mutex<HashMap<K, Arc<V>>>>,
}

impl<K, V> PtyRegistry<K, V> {
    pub fn new() -> Self {
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<K: std::hash::Hash + Clone + Eq, V> PtyRegistry<K, V> {
    /// Get a clone of the Arc-wrapped value for the given key.
    pub fn get(&self, key: &K) -> Option<Arc<V>> {
        let map = self.processes.lock().unwrap();
        map.get(key).cloned()
    }

    /// Insert a new entry. Replaces any existing entry for the same key.
    pub fn insert(&self, key: K, value: Arc<V>) {
        let mut map = self.processes.lock().unwrap();
        map.insert(key, value);
    }

    /// Remove and return the entry for the given key.
    pub fn remove(&self, key: &K) -> Option<Arc<V>> {
        let mut map = self.processes.lock().unwrap();
        map.remove(key)
    }

    /// Returns true if the registry contains the given key.
    pub fn contains(&self, key: &K) -> bool {
        let map = self.processes.lock().unwrap();
        map.contains_key(key)
    }

    /// Returns the number of entries currently in the registry.
    pub fn len(&self) -> usize {
        let map = self.processes.lock().unwrap();
        map.len()
    }

    /// Returns true if the registry is empty.
    pub fn is_empty(&self) -> bool {
        let map = self.processes.lock().unwrap();
        map.is_empty()
    }

    /// Iterate over all key-value pairs currently in the registry.
    pub fn iter(&self) -> impl Iterator<Item = (K, Arc<V>)> {
        let map = self.processes.lock().unwrap();
        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>().into_iter()
    }
}

impl<K, V> Default for PtyRegistry<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let registry = PtyRegistry::<i64, String>::new();
        let value = Arc::new("test".to_string());
        registry.insert(1, value.clone());
        assert!(registry.contains(&1));
        let retrieved = registry.get(&1).unwrap();
        assert_eq!(*retrieved, "test");
    }

    #[test]
    fn remove_vanishes() {
        let registry = PtyRegistry::<i64, String>::new();
        registry.insert(1, Arc::new("test".to_string()));
        let removed = registry.remove(&1);
        assert!(removed.is_some());
        assert!(registry.get(&1).is_none());
    }

    #[test]
    fn multiple_keys() {
        let registry = PtyRegistry::<&str, i32>::new();
        registry.insert("a", Arc::new(1));
        registry.insert("b", Arc::new(2));
        assert_eq!(registry.len(), 2);
        assert!(registry.contains(&"a"));
        assert!(registry.contains(&"b"));
    }

    #[test]
    fn is_empty_for_new() {
        let registry = PtyRegistry::<i64, String>::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }
}