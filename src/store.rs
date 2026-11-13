use std::collections::{BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};

use bytes::Bytes;

use crate::random;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expiry {
    Missing,
    Never,
    At(u64),
}

fn scan_hash(key: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug, Default)]
struct Database {
    entries: HashMap<Bytes, Entry>,
    expires: BTreeSet<(u64, Bytes)>,
    scan_order: BTreeSet<(u64, Bytes)>,
}

impl Database {
    fn insert_entry(&mut self, key: Bytes, entry: Entry) {
        self.remove_entry(&key);
        if let Some(deadline) = entry.expires_at {
            self.expires.insert((deadline, key.clone()));
        }
        self.scan_order.insert((scan_hash(&key), key.clone()));
        self.entries.insert(key, entry);
    }

    fn remove_entry(&mut self, key: &[u8]) -> Option<Entry> {
        let (key, entry) = self.entries.remove_entry(key)?;
        if let Some(deadline) = entry.expires_at {
            self.expires.remove(&(deadline, key.clone()));
        }
        self.scan_order.remove(&(scan_hash(&key), key));
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
        database.insert_entry(
            key,
            Entry {
                value,
                expires_at: deadline,
            },
        );
    }

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

    /// Lists the live keys of one database in no particular order.
    pub(crate) fn keys(&self, db: usize, now_ms: u64) -> Vec<Bytes> {
        let entries = &self.databases[db].entries;
        let live = entries
            .iter()
            .filter(|(_, entry)| !entry.is_expired(now_ms));
        live.map(|(key, _)| key.clone()).collect()
    }

    /// Returns a random live key, dropping expired keys it meets.
    pub(crate) fn random_key(&mut self, db: usize, now_ms: u64) -> Option<Bytes> {
        loop {
            let database = &self.databases[db];
            let start = (random::random_u64(), Bytes::new());
            let (_, key) = database
                .scan_order
                .range(start..)
                .next()
                .or_else(|| database.scan_order.first())?;
            let key = key.clone();
            if self.get(db, &key, now_ms).is_some() {
                return Some(key);
            }
        }
    }

    /// Visits about `count` keys from `cursor`; returns the next cursor (0 at the end) and keys.
    pub(crate) fn scan(
        &self,
        db: usize,
        cursor: u64,
        count: usize,
        now_ms: u64,
    ) -> (u64, Vec<Bytes>) {
        let database = &self.databases[db];
        let mut found = Vec::new();
        let mut previous = None;
        let from = (cursor, Bytes::new());
        for (visited, (hash, key)) in database.scan_order.range(from..).enumerate() {
            if visited >= count && previous != Some(*hash) {
                return (*hash, found);
            }
            previous = Some(*hash);
            if database
                .entries
                .get(key)
                .is_some_and(|entry| !entry.is_expired(now_ms))
            {
                found.push(key.clone());
            }
        }
        (0, found)
    }

    pub(crate) fn purge_expired(&mut self, now_ms: u64, limit: usize) -> usize {
        let mut purged = 0;
        for database in &mut self.databases {
            while purged < limit {
                let Some((deadline, _)) = database.expires.first() else {
                    break;
                };
                if *deadline > now_ms {
                    break;
                }
                let Some((_, key)) = database.expires.pop_first() else {
                    break;
                };
                database.entries.remove(&key);
                database.scan_order.remove(&(scan_hash(&key), key));
                purged += 1;
            }
        }
        purged
    }

    pub(crate) fn flush_db(&mut self, db: usize) {
        self.databases[db] = Database::default();
    }

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
    fn purge_removes_only_due_keys_up_to_the_limit() {
        let mut store = Store::default();
        store.put(0, Bytes::from("a"), text("v"), Ttl::At(10), 0);
        store.put(0, Bytes::from("b"), text("v"), Ttl::At(20), 0);
        store.put(1, Bytes::from("c"), text("v"), Ttl::At(15), 0);
        store.put(1, Bytes::from("d"), text("v"), Ttl::At(100), 0);
        store.set(1, Bytes::from("e"), text("v"));
        assert_eq!(store.purge_expired(9, 10), 0);
        assert_eq!(store.purge_expired(20, 2), 2);
        assert_eq!(store.purge_expired(20, 10), 1);
        assert_eq!((store.len(0), store.len(1)), (0, 2));
        assert_eq!(store.purge_expired(1_000, 10), 1);
        assert_eq!(store.len(1), 1);
    }

    #[test]
    fn scan_visits_every_key_once_in_cursor_order() {
        let mut store = Store::default();
        for index in 0..50 {
            store.set(0, Bytes::from(format!("key{index}")), text("v"));
        }
        store.put(0, Bytes::from("dead"), text("v"), Ttl::At(5), 0);
        let mut seen = Vec::new();
        let mut cursor = 0;
        loop {
            let (next, keys) = store.scan(0, cursor, 7, 10);
            seen.extend(keys);
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        seen.sort();
        let mut expected: Vec<Bytes> = (0..50)
            .map(|index| Bytes::from(format!("key{index}")))
            .collect();
        expected.sort();
        assert_eq!(seen, expected);
    }

    #[test]
    fn scan_of_an_empty_database_ends_at_once() {
        let store = Store::default();
        assert_eq!(store.scan(0, 0, 10, 0), (0, Vec::new()));
    }

    #[test]
    fn keys_and_random_key_skip_expired_keys() {
        let mut store = Store::default();
        assert_eq!(store.random_key(0, 0), None);
        store.set(0, Bytes::from("live"), text("v"));
        store.put(0, Bytes::from("dead"), text("v"), Ttl::At(5), 0);
        assert_eq!(store.keys(0, 10), vec![Bytes::from("live")]);
        for _ in 0..20 {
            assert_eq!(store.random_key(0, 10), Some(Bytes::from("live")));
        }
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
