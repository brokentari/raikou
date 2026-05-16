//! All databases and their keys.

use std::collections::{BTreeSet, HashMap};

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

/// What happens to a key's deadline when its value is replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ttl {
    Clear,
    Keep,
    At(u64),
}

/// When a key stops existing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expiry {
    Missing,
    Never,
    At(u64),
}

#[derive(Debug, Default)]
struct Database {
    entries: HashMap<Bytes, Entry>,
    expires: BTreeSet<(u64, Bytes)>,
}

impl Database {
    fn remove_entry(&mut self, key: &[u8]) -> Option<Entry> {
        let (key, entry) = self.entries.remove_entry(key)?;
        if let Some(deadline) = entry.expires_at {
            self.expires.remove(&(deadline, key));
        }
        Some(entry)
    }

    fn live(&mut self, key: &[u8], now_ms: u64) -> Option<&mut Entry> {
        if self.entries.get(key)?.is_expired(now_ms) {
            self.remove_entry(key);
            return None;
        }
        self.entries.get_mut(key)
    }
}

/// The keyspace: a fixed number of databases.
#[derive(Debug)]
pub(crate) struct Store {
    databases: Vec<Database>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            databases: (0..DATABASES).map(|_| Database::default()).collect(),
        }
    }
}

impl Store {
    /// Returns the value under `key`, dropping the key first if it has expired.
    pub(crate) fn get(&mut self, db: usize, key: &[u8], now_ms: u64) -> Option<&Value> {
        self.databases[db]
            .live(key, now_ms)
            .map(|entry| &entry.value)
    }

    /// Returns the value under `key` for in-place change; an expiry stays as it is.
    pub(crate) fn get_mut(&mut self, db: usize, key: &[u8], now_ms: u64) -> Option<&mut Value> {
        self.databases[db]
            .live(key, now_ms)
            .map(|entry| &mut entry.value)
    }

    /// Stores `value` under `key`, replacing any value and clearing any expiry.
    pub(crate) fn set(&mut self, db: usize, key: Bytes, value: Value) {
        self.put(db, key, value, Ttl::Clear, 0);
    }

    /// Stores `value` under `key`, replacing any value, and applies `ttl` to its deadline.
    pub(crate) fn put(&mut self, db: usize, key: Bytes, value: Value, ttl: Ttl, now_ms: u64) {
        let database = &mut self.databases[db];
        let deadline = match ttl {
            Ttl::Clear => None,
            Ttl::At(deadline) => Some(deadline),
            Ttl::Keep => database
                .live(&key, now_ms)
                .and_then(|entry| entry.expires_at),
        };
        database.remove_entry(&key);
        if let Some(deadline) = deadline {
            database.expires.insert((deadline, key.clone()));
        }
        database.entries.insert(
            key,
            Entry {
                value,
                expires_at: deadline,
            },
        );
    }

    /// Reports whether `key` exists and when it expires.
    pub(crate) fn expiry(&mut self, db: usize, key: &[u8], now_ms: u64) -> Expiry {
        match self.databases[db].live(key, now_ms) {
            None => Expiry::Missing,
            Some(entry) => entry.expires_at.map_or(Expiry::Never, Expiry::At),
        }
    }

    /// Sets or, with `None`, removes the deadline of a live key; returns whether it changed.
    pub(crate) fn set_deadline(
        &mut self,
        db: usize,
        key: &[u8],
        deadline: Option<u64>,
        now_ms: u64,
    ) -> bool {
        let database = &mut self.databases[db];
        let Some(entry) = database.live(key, now_ms) else {
            return false;
        };
        let old = std::mem::replace(&mut entry.expires_at, deadline);
        if old == deadline {
            return false;
        }
        let key = database
            .entries
            .get_key_value(key)
            .map(|(key, _)| key.clone());
        let Some(key) = key else {
            return false;
        };
        if let Some(old) = old {
            database.expires.remove(&(old, key.clone()));
        }
        if let Some(deadline) = deadline {
            database.expires.insert((deadline, key));
        }
        true
    }

    /// Deletes `key`; returns whether a live key was there.
    pub(crate) fn remove(&mut self, db: usize, key: &[u8], now_ms: u64) -> bool {
        self.databases[db]
            .remove_entry(key)
            .is_some_and(|entry| !entry.is_expired(now_ms))
    }

    /// Removes a live key and returns its value and deadline.
    pub(crate) fn take(
        &mut self,
        db: usize,
        key: &[u8],
        now_ms: u64,
    ) -> Option<(Value, Option<u64>)> {
        self.databases[db].live(key, now_ms)?;
        let entry = self.databases[db].remove_entry(key)?;
        Some((entry.value, entry.expires_at))
    }

    /// Counts the keys in one database, including expired keys not yet removed.
    pub(crate) fn len(&self, db: usize) -> usize {
        self.databases[db].entries.len()
    }

    /// Deletes every key in one database.
    pub(crate) fn flush_db(&mut self, db: usize) {
        self.databases[db] = Database::default();
    }

    /// Deletes every key in every database.
    pub(crate) fn flush_all(&mut self) {
        for database in &mut self.databases {
            *database = Database::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deadline(store: &mut Store, key: &[u8], now_ms: u64) -> (bool, Option<u64>) {
        let entry = store.databases[0].live(key, now_ms);
        (entry.is_some(), entry.and_then(|entry| entry.expires_at))
    }

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
    fn get_mut_changes_the_value_and_keeps_the_deadline() {
        let mut store = Store::default();
        store.put(0, Bytes::from("k"), text("a"), Ttl::At(50), 0);
        *store.get_mut(0, b"k", 0).expect("present") = text("b");
        assert_eq!(deadline(&mut store, b"k", 0), (true, Some(50)));
        assert_eq!(store.get(0, b"k", 0), Some(&text("b")));
        assert!(store.get_mut(0, b"k", 50).is_none());
    }

    #[test]
    fn take_moves_value_and_deadline_out() {
        let mut store = Store::default();
        store.put(0, Bytes::from("k"), text("v"), Ttl::At(90), 0);
        assert_eq!(store.take(0, b"k", 0), Some((text("v"), Some(90))));
        assert_eq!(store.take(0, b"k", 0), None);
        assert!(store.databases[0].expires.is_empty());
        store.put(0, Bytes::from("e"), text("v"), Ttl::At(5), 0);
        assert_eq!(store.take(0, b"e", 5), None);
    }

    #[test]
    fn len_and_flush_db() {
        let mut store = Store::default();
        store.set(0, Bytes::from("a"), text("v"));
        store.set(0, Bytes::from("b"), text("v"));
        store.set(1, Bytes::from("c"), text("v"));
        assert_eq!((store.len(0), store.len(1)), (2, 1));
        store.flush_db(0);
        assert_eq!((store.len(0), store.len(1)), (0, 1));
    }

    #[test]
    fn databases_are_separate() {
        let mut store = Store::default();
        store.set(1, Bytes::from("k"), text("v"));
        assert_eq!(store.get(0, b"k", 0), None);
        assert_eq!(store.get(1, b"k", 0), Some(&text("v")));
    }

    #[test]
    fn ttl_clear_keep_and_at() {
        let mut store = Store::default();
        store.put(0, Bytes::from("k"), text("a"), Ttl::At(100), 0);
        assert_eq!(deadline(&mut store, b"k", 0), (true, Some(100)));
        store.put(0, Bytes::from("k"), text("b"), Ttl::Keep, 0);
        assert_eq!(deadline(&mut store, b"k", 0), (true, Some(100)));
        store.put(0, Bytes::from("k"), text("c"), Ttl::Clear, 0);
        assert_eq!(deadline(&mut store, b"k", 0), (true, None));
        assert_eq!(deadline(&mut store, b"missing", 0), (false, None));
    }

    #[test]
    fn expiry_reports_all_three_states() {
        let mut store = Store::default();
        store.set(0, Bytes::from("a"), text("v"));
        store.put(0, Bytes::from("b"), text("v"), Ttl::At(9), 0);
        assert_eq!(store.expiry(0, b"a", 0), Expiry::Never);
        assert_eq!(store.expiry(0, b"b", 0), Expiry::At(9));
        assert_eq!(store.expiry(0, b"c", 0), Expiry::Missing);
        assert_eq!(store.expiry(0, b"b", 9), Expiry::Missing);
    }

    #[test]
    fn keep_ignores_the_deadline_of_an_expired_key() {
        let mut store = Store::default();
        store.put(0, Bytes::from("k"), text("a"), Ttl::At(10), 0);
        store.put(0, Bytes::from("k"), text("b"), Ttl::Keep, 10);
        assert_eq!(deadline(&mut store, b"k", 10), (true, None));
    }

    #[test]
    fn expired_keys_are_gone() {
        let mut store = Store::default();
        store.put(0, Bytes::from("k"), text("v"), Ttl::At(10), 0);
        assert_eq!(store.get(0, b"k", 9), Some(&text("v")));
        assert_eq!(store.get(0, b"k", 10), None);
        assert!(store.databases[0].entries.is_empty());
        assert!(store.databases[0].expires.is_empty());
        store.put(0, Bytes::from("j"), text("v"), Ttl::At(10), 0);
        assert!(!store.remove(0, b"j", 10));
    }

    #[test]
    fn deadline_changes_keep_the_index_in_step() {
        let mut store = Store::default();
        store.set(0, Bytes::from("k"), text("v"));
        assert!(store.set_deadline(0, b"k", Some(50), 0));
        assert!(!store.set_deadline(0, b"k", Some(50), 0));
        assert_eq!(store.databases[0].expires.len(), 1);
        assert!(store.set_deadline(0, b"k", Some(60), 0));
        assert_eq!(
            store.databases[0].expires.iter().next().map(|(at, _)| *at),
            Some(60)
        );
        assert!(store.set_deadline(0, b"k", None, 0));
        assert!(store.databases[0].expires.is_empty());
        assert!(!store.set_deadline(0, b"missing", Some(5), 0));
        store.remove(0, b"k", 0);
        assert!(store.databases[0].expires.is_empty());
    }

    #[test]
    fn flush_all_empties_every_database() {
        let mut store = Store::default();
        store.put(0, Bytes::from("a"), text("v"), Ttl::At(5), 0);
        store.set(5, Bytes::from("b"), text("v"));
        store.flush_all();
        assert_eq!(store.get(0, b"a", 0), None);
        assert_eq!(store.get(5, b"b", 0), None);
        assert!(store.databases[0].expires.is_empty());
    }
}
