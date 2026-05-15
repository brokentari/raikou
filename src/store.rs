//! All databases and their keys.

use std::collections::HashMap;

use bytes::Bytes;

use crate::session::DATABASES;
use crate::value::Value;

#[derive(Debug)]
pub(crate) struct Entry {
    pub(crate) value: Value,
    pub(crate) expires_at: Option<u64>,
}

impl Entry {
    fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at.is_some_and(|deadline| deadline <= now_ms)
    }
}

/// The keyspace: a fixed number of databases.
#[derive(Debug)]
pub(crate) struct Store {
    databases: Vec<HashMap<Bytes, Entry>>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            databases: (0..DATABASES).map(|_| HashMap::new()).collect(),
        }
    }
}

impl Store {
    /// Returns the value under `key`, dropping the key first if it has expired.
    pub(crate) fn get(&mut self, db: usize, key: &[u8], now_ms: u64) -> Option<&Value> {
        let entries = &mut self.databases[db];
        if entries.get(key)?.is_expired(now_ms) {
            entries.remove(key);
            return None;
        }
        entries.get(key).map(|entry| &entry.value)
    }

    /// Stores `value` under `key`, replacing any value and clearing any expiry.
    pub(crate) fn set(&mut self, db: usize, key: Bytes, value: Value) {
        self.databases[db].insert(
            key,
            Entry {
                value,
                expires_at: None,
            },
        );
    }

    /// Deletes `key`; returns whether a live key was there.
    pub(crate) fn remove(&mut self, db: usize, key: &[u8], now_ms: u64) -> bool {
        self.databases[db]
            .remove(key)
            .is_some_and(|entry| !entry.is_expired(now_ms))
    }

    /// Deletes every key in every database.
    pub(crate) fn flush_all(&mut self) {
        for entries in &mut self.databases {
            entries.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Value {
        Value::Str(Bytes::copy_from_slice(value.as_bytes()))
    }

    #[test]
    fn set_get_remove() {
        let mut store = Store::default();
        assert_eq!(store.get(0, b"k", 0), None);
        store.set(0, Bytes::from("k"), text("v"));
        assert_eq!(store.get(0, b"k", 0), Some(&text("v")));
        assert!(store.remove(0, b"k", 0));
        assert!(!store.remove(0, b"k", 0));
        assert_eq!(store.get(0, b"k", 0), None);
    }

    #[test]
    fn databases_are_separate() {
        let mut store = Store::default();
        store.set(1, Bytes::from("k"), text("v"));
        assert_eq!(store.get(0, b"k", 0), None);
        assert_eq!(store.get(1, b"k", 0), Some(&text("v")));
    }

    #[test]
    fn set_clears_the_expiry() {
        let mut store = Store::default();
        store.set(0, Bytes::from("k"), text("old"));
        store.databases[0]
            .get_mut(&b"k"[..])
            .expect("entry")
            .expires_at = Some(10);
        store.set(0, Bytes::from("k"), text("new"));
        assert_eq!(store.get(0, b"k", 100), Some(&text("new")));
    }

    #[test]
    fn expired_keys_are_gone() {
        let mut store = Store::default();
        store.set(0, Bytes::from("k"), text("v"));
        store.databases[0]
            .get_mut(&b"k"[..])
            .expect("entry")
            .expires_at = Some(10);
        assert_eq!(store.get(0, b"k", 9), Some(&text("v")));
        assert_eq!(store.get(0, b"k", 10), None);
        assert!(store.databases[0].is_empty());
        store.set(0, Bytes::from("j"), text("v"));
        store.databases[0]
            .get_mut(&b"j"[..])
            .expect("entry")
            .expires_at = Some(10);
        assert!(!store.remove(0, b"j", 10));
    }

    #[test]
    fn flush_all_empties_every_database() {
        let mut store = Store::default();
        store.set(0, Bytes::from("a"), text("v"));
        store.set(5, Bytes::from("b"), text("v"));
        store.flush_all();
        assert_eq!(store.get(0, b"a", 0), None);
        assert_eq!(store.get(5, b"b", 0), None);
    }
}
