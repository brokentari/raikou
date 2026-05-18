//! Hash commands.

use std::collections::HashMap;

use bytes::Bytes;

use crate::command::Context;
use crate::error::CommandError;
use crate::resp::Frame;
use crate::value::Value;

type Hash = HashMap<Bytes, Bytes>;

fn count_frame(len: usize) -> Frame {
    Frame::Integer(i64::try_from(len).unwrap_or(i64::MAX))
}

fn hash_ref<'a>(
    context: &'a mut Context<'_>,
    key: &[u8],
) -> Result<Option<&'a Hash>, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    match state.store.get(session.db, key, *now_ms) {
        None => Ok(None),
        Some(Value::Hash(hash)) => Ok(Some(hash)),
        Some(_) => Err(CommandError::WrongType),
    }
}

fn hash_mut<'a>(
    context: &'a mut Context<'_>,
    key: &[u8],
) -> Result<Option<&'a mut Hash>, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    match state.store.get_mut(session.db, key, *now_ms) {
        None => Ok(None),
        Some(Value::Hash(hash)) => Ok(Some(hash)),
        Some(_) => Err(CommandError::WrongType),
    }
}

/// Returns the hash under `key`, creating an empty one when the key is free.
fn hash_or_create<'a>(
    context: &'a mut Context<'_>,
    key: &Bytes,
) -> Result<&'a mut Hash, CommandError> {
    if hash_ref(context, key)?.is_none() {
        let Context { state, session, .. } = context;
        state
            .store
            .set(session.db, key.clone(), Value::Hash(Hash::new()));
    }
    hash_mut(context, key)?.ok_or(CommandError::WrongType)
}

fn drop_if_empty(context: &mut Context<'_>, key: &[u8]) {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let empty = matches!(state.store.get(session.db, key, *now_ms), Some(Value::Hash(hash)) if hash.is_empty());
    if empty {
        state.store.remove(session.db, key, *now_ms);
    }
}

fn set_fields(
    context: &mut Context<'_>,
    argv: &[Bytes],
    name: &str,
) -> Result<usize, CommandError> {
    if !argv.len().is_multiple_of(2) {
        return Err(CommandError::WrongArity(name.to_owned()));
    }
    let hash = hash_or_create(context, &argv[1])?;
    let (pairs, _) = argv[2..].as_chunks::<2>();
    let mut created = 0;
    for [field, value] in pairs {
        created += usize::from(hash.insert(field.clone(), value.clone()).is_none());
    }
    Ok(created)
}

pub(super) fn hset(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    set_fields(context, argv, "hset").map(count_frame)
}

pub(super) fn hmset(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    set_fields(context, argv, "hmset").map(|_| Frame::ok())
}

pub(super) fn hget(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let found = hash_ref(context, &argv[1])?.and_then(|hash| hash.get(&argv[2]));
    Ok(found.cloned().map_or(Frame::Null, Frame::bulk))
}

pub(super) fn hmget(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let hash = hash_ref(context, &argv[1])?;
    let values = argv[2..]
        .iter()
        .map(|field| {
            hash.and_then(|hash| hash.get(field))
                .cloned()
                .map_or(Frame::Null, Frame::bulk)
        })
        .collect();
    Ok(Frame::Array(values))
}

pub(super) fn hgetall(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let pairs = hash_ref(context, &argv[1])?.map_or_else(Vec::new, |hash| {
        hash.iter()
            .map(|(field, value)| (Frame::bulk(field.clone()), Frame::bulk(value.clone())))
            .collect()
    });
    Ok(Frame::Map(pairs))
}

pub(super) fn hkeys(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let fields = hash_ref(context, &argv[1])?.map_or_else(Vec::new, |hash| {
        hash.keys().cloned().map(Frame::bulk).collect()
    });
    Ok(Frame::Array(fields))
}

pub(super) fn hvals(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let values = hash_ref(context, &argv[1])?.map_or_else(Vec::new, |hash| {
        hash.values().cloned().map(Frame::bulk).collect()
    });
    Ok(Frame::Array(values))
}

pub(super) fn hdel(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Some(hash) = hash_mut(context, &argv[1])? else {
        return Ok(Frame::Integer(0));
    };
    let removed = argv[2..]
        .iter()
        .filter(|field| hash.remove(*field).is_some())
        .count();
    drop_if_empty(context, &argv[1]);
    Ok(count_frame(removed))
}

pub(super) fn hexists(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let found = hash_ref(context, &argv[1])?.is_some_and(|hash| hash.contains_key(&argv[2]));
    Ok(Frame::Integer(i64::from(found)))
}

pub(super) fn hlen(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    Ok(count_frame(
        hash_ref(context, &argv[1])?.map_or(0, Hash::len),
    ))
}

#[cfg(test)]
mod tests {
    use crate::command::harness::{Harness, error};

    use super::*;

    fn sorted(frame: Frame) -> Vec<String> {
        let items = match frame {
            Frame::Array(items) => items,
            Frame::Map(pairs) => pairs
                .into_iter()
                .flat_map(|(key, value)| [key, value])
                .collect(),
            other => panic!("array: {other:?}"),
        };
        let mut texts: Vec<String> = items
            .iter()
            .map(|item| match item {
                Frame::BulkString(data) => String::from_utf8_lossy(data).into_owned(),
                other => format!("{other:?}"),
            })
            .collect();
        texts.sort();
        texts
    }

    #[test]
    fn hset_hget_hexists_hlen() {
        let mut client = Harness::new();
        assert_eq!(
            client.run(&["HSET", "h", "a", "1", "b", "2"]),
            Frame::Integer(2)
        );
        assert_eq!(
            client.run(&["HSET", "h", "a", "9", "c", "3"]),
            Frame::Integer(1)
        );
        assert_eq!(client.run(&["HGET", "h", "a"]), Frame::bulk("9"));
        assert_eq!(client.run(&["HGET", "h", "zz"]), Frame::Null);
        assert_eq!(client.run(&["HGET", "none", "a"]), Frame::Null);
        assert_eq!(client.run(&["HEXISTS", "h", "b"]), Frame::Integer(1));
        assert_eq!(client.run(&["HEXISTS", "h", "zz"]), Frame::Integer(0));
        assert_eq!(client.run(&["HLEN", "h"]), Frame::Integer(3));
        assert_eq!(client.run(&["HLEN", "none"]), Frame::Integer(0));
        assert_eq!(client.run(&["TYPE", "h"]), Frame::simple("hash"));
    }

    #[test]
    fn hmset_hmget() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["HMSET", "h", "a", "1", "b", "2"]), Frame::ok());
        assert_eq!(
            client.run(&["HMGET", "h", "a", "x", "b"]),
            Frame::Array(vec![Frame::bulk("1"), Frame::Null, Frame::bulk("2")])
        );
        assert_eq!(
            client.run(&["HMGET", "none", "a"]),
            Frame::Array(vec![Frame::Null])
        );
        assert_eq!(
            client.run(&["HSET", "h", "a"]),
            error("ERR wrong number of arguments for 'hset' command")
        );
        assert_eq!(
            client.run(&["HMSET", "h", "a", "1", "b"]),
            error("ERR wrong number of arguments for 'hmset' command")
        );
    }

    #[test]
    fn hgetall_hkeys_hvals() {
        let mut client = Harness::new();
        client.run(&["HSET", "h", "a", "1", "b", "2"]);
        assert_eq!(sorted(client.run(&["HGETALL", "h"])), ["1", "2", "a", "b"]);
        assert_eq!(sorted(client.run(&["HKEYS", "h"])), ["a", "b"]);
        assert_eq!(sorted(client.run(&["HVALS", "h"])), ["1", "2"]);
        assert_eq!(client.run(&["HGETALL", "none"]), Frame::Map(Vec::new()));
    }

    #[test]
    fn hdel_removes_the_key_when_empty() {
        let mut client = Harness::new();
        client.run(&["HSET", "h", "a", "1", "b", "2"]);
        assert_eq!(
            client.run(&["HDEL", "h", "a", "zz", "a"]),
            Frame::Integer(1)
        );
        assert_eq!(client.run(&["HDEL", "h", "b"]), Frame::Integer(1));
        assert_eq!(client.run(&["EXISTS", "h"]), Frame::Integer(0));
        assert_eq!(client.run(&["HDEL", "h", "b"]), Frame::Integer(0));
    }

    #[test]
    fn wrong_type_is_reported() {
        let mut client = Harness::new();
        client.run(&["SET", "s", "v"]);
        client.run(&["HSET", "h", "a", "1"]);
        let wrong = error("WRONGTYPE Operation against a key holding the wrong kind of value");
        assert_eq!(client.run(&["HSET", "s", "a", "1"]), wrong);
        assert_eq!(client.run(&["HGET", "s", "a"]), wrong);
        assert_eq!(client.run(&["HGETALL", "s"]), wrong);
        assert_eq!(client.run(&["GET", "h"]), wrong);
        assert_eq!(client.run(&["LPUSH", "h", "x"]), wrong);
    }
}
