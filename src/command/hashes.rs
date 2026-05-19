//! Hash commands.

use std::collections::HashMap;

use bytes::Bytes;

use crate::command::Context;
use crate::error::CommandError;
use crate::long_double::{LongDouble, Parsed};
use crate::number;
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

pub(super) fn hsetnx(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let hash = hash_or_create(context, &argv[1])?;
    if hash.contains_key(&argv[2]) {
        return Ok(Frame::Integer(0));
    }
    hash.insert(argv[2].clone(), argv[3].clone());
    Ok(Frame::Integer(1))
}

pub(super) fn hstrlen(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let found = hash_ref(context, &argv[1])?.and_then(|hash| hash.get(&argv[2]));
    Ok(count_frame(found.map_or(0, Bytes::len)))
}

pub(super) fn hincrby(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let by = number::parse_i64(&argv[3]).ok_or(CommandError::NotInteger)?;
    let hash = hash_or_create(context, &argv[1])?;
    let current = match hash.get(&argv[2]) {
        Some(text) => number::parse_i64(text)
            .ok_or_else(|| CommandError::other("ERR hash value is not an integer"))?,
        None => 0,
    };
    let updated = current
        .checked_add(by)
        .ok_or_else(|| CommandError::other("ERR increment or decrement would overflow"))?;
    hash.insert(argv[2].clone(), Bytes::from(updated.to_string()));
    Ok(Frame::Integer(updated))
}

pub(super) fn hincrbyfloat(
    context: &mut Context<'_>,
    argv: &[Bytes],
) -> Result<Frame, CommandError> {
    let by = match LongDouble::parse(&argv[3]) {
        None => return Err(CommandError::NotFloat),
        Some(Parsed::Infinite) => return Err(CommandError::other("ERR value is NaN or Infinity")),
        Some(Parsed::Finite(by)) => by,
    };
    let hash = hash_or_create(context, &argv[1])?;
    let current = match hash.get(&argv[2]) {
        None => LongDouble::parse(b"0"),
        Some(text) => LongDouble::parse(text),
    };
    let infinite = || CommandError::other("ERR increment would produce NaN or Infinity");
    let Some(current) = current else {
        return Err(CommandError::other("ERR hash value is not a float"));
    };
    let Parsed::Finite(current) = current else {
        return Err(infinite());
    };
    let total = current.checked_add(by).ok_or_else(infinite)?;
    let text = Bytes::from(total.to_human_string());
    hash.insert(argv[2].clone(), text.clone());
    Ok(Frame::bulk(text))
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
    fn hsetnx_and_hstrlen() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["HSETNX", "h", "a", "xyz"]), Frame::Integer(1));
        assert_eq!(
            client.run(&["HSETNX", "h", "a", "other"]),
            Frame::Integer(0)
        );
        assert_eq!(client.run(&["HGET", "h", "a"]), Frame::bulk("xyz"));
        assert_eq!(client.run(&["HSTRLEN", "h", "a"]), Frame::Integer(3));
        assert_eq!(client.run(&["HSTRLEN", "h", "zz"]), Frame::Integer(0));
        assert_eq!(client.run(&["HSTRLEN", "none", "a"]), Frame::Integer(0));
    }

    #[test]
    fn hincrby_counts() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["HINCRBY", "h", "n", "5"]), Frame::Integer(5));
        assert_eq!(client.run(&["HINCRBY", "h", "n", "-8"]), Frame::Integer(-3));
        client.run(&["HSET", "h", "s", "text", "max", "9223372036854775807"]);
        assert_eq!(
            client.run(&["HINCRBY", "h", "s", "1"]),
            error("ERR hash value is not an integer")
        );
        assert_eq!(
            client.run(&["HINCRBY", "h", "max", "1"]),
            error("ERR increment or decrement would overflow")
        );
        assert_eq!(
            client.run(&["HINCRBY", "h", "n", "x"]),
            error("ERR value is not an integer or out of range")
        );
    }

    #[test]
    fn hincrbyfloat_sums() {
        let mut client = Harness::new();
        assert_eq!(
            client.run(&["HINCRBYFLOAT", "h", "f", "0.1"]),
            Frame::bulk("0.1")
        );
        assert_eq!(
            client.run(&["HINCRBYFLOAT", "h", "f", "0.2"]),
            Frame::bulk("0.3")
        );
        client.run(&["HSET", "h", "s", "text"]);
        assert_eq!(
            client.run(&["HINCRBYFLOAT", "h", "s", "1"]),
            error("ERR hash value is not a float")
        );
        assert_eq!(
            client.run(&["HINCRBYFLOAT", "h", "f", "abc"]),
            error("ERR value is not a valid float")
        );
        assert_eq!(
            client.run(&["HINCRBYFLOAT", "h", "f", "inf"]),
            error("ERR value is NaN or Infinity")
        );
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
