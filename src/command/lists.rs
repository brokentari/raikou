//! List commands.

use std::collections::VecDeque;

use bytes::Bytes;

use crate::command::Context;
use crate::error::CommandError;
use crate::number;
use crate::resp::Frame;
use crate::value::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Head,
    Tail,
}

fn count_frame(len: usize) -> Frame {
    Frame::Integer(i64::try_from(len).unwrap_or(i64::MAX))
}

fn integer(arg: &Bytes) -> Result<i64, CommandError> {
    number::parse_i64(arg).ok_or(CommandError::NotInteger)
}

/// Resolves Redis range indices against `len`; the result is inclusive.
fn resolve_range(start: i64, end: i64, len: usize) -> Option<(usize, usize)> {
    let len = i64::try_from(len).unwrap_or(i64::MAX);
    let start = if start < 0 { start + len } else { start }.max(0);
    let end = if end < 0 { end + len } else { end };
    if start > end || start >= len {
        return None;
    }
    let end = end.min(len - 1);
    Some((usize::try_from(start).ok()?, usize::try_from(end).ok()?))
}

fn list_ref<'a>(
    context: &'a mut Context<'_>,
    key: &[u8],
) -> Result<Option<&'a VecDeque<Bytes>>, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    match state.store.get(session.db, key, *now_ms) {
        None => Ok(None),
        Some(Value::List(list)) => Ok(Some(list)),
        Some(_) => Err(CommandError::WrongType),
    }
}

fn list_mut<'a>(
    context: &'a mut Context<'_>,
    key: &[u8],
) -> Result<Option<&'a mut VecDeque<Bytes>>, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    match state.store.get_mut(session.db, key, *now_ms) {
        None => Ok(None),
        Some(Value::List(list)) => Ok(Some(list)),
        Some(_) => Err(CommandError::WrongType),
    }
}

/// Removes the key when its list has become empty.
fn drop_if_empty(context: &mut Context<'_>, key: &[u8]) {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let empty = matches!(state.store.get(session.db, key, *now_ms), Some(Value::List(list)) if list.is_empty());
    if empty {
        state.store.remove(session.db, key, *now_ms);
    }
}

fn push(
    context: &mut Context<'_>,
    argv: &[Bytes],
    end: End,
    only_existing: bool,
) -> Result<Frame, CommandError> {
    if list_ref(context, &argv[1])?.is_none() {
        if only_existing {
            return Ok(Frame::Integer(0));
        }
        let Context { state, session, .. } = context;
        state
            .store
            .set(session.db, argv[1].clone(), Value::List(VecDeque::new()));
    }
    let Some(list) = list_mut(context, &argv[1])? else {
        return Ok(Frame::Integer(0));
    };
    for element in &argv[2..] {
        match end {
            End::Head => list.push_front(element.clone()),
            End::Tail => list.push_back(element.clone()),
        }
    }
    Ok(count_frame(list.len()))
}

pub(super) fn lpush(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    push(context, argv, End::Head, false)
}

pub(super) fn rpush(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    push(context, argv, End::Tail, false)
}

pub(super) fn lpushx(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    push(context, argv, End::Head, true)
}

pub(super) fn rpushx(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    push(context, argv, End::Tail, true)
}

fn pop(context: &mut Context<'_>, argv: &[Bytes], end: End) -> Result<Frame, CommandError> {
    if argv.len() > 3 {
        return Err(CommandError::WrongArity(
            if end == End::Head { "lpop" } else { "rpop" }.to_owned(),
        ));
    }
    let count = match argv.get(2) {
        None => None,
        Some(arg) => Some(
            number::parse_i64(arg)
                .and_then(|count| usize::try_from(count).ok())
                .ok_or_else(|| {
                    CommandError::other("ERR value is out of range, must be positive")
                })?,
        ),
    };
    let missing = if count.is_some() {
        Frame::NullArray
    } else {
        Frame::Null
    };
    let Some(list) = list_mut(context, &argv[1])? else {
        return Ok(missing);
    };
    let take = |list: &mut VecDeque<Bytes>| match end {
        End::Head => list.pop_front(),
        End::Tail => list.pop_back(),
    };
    let reply = match count {
        None => take(list).map_or(Frame::Null, Frame::bulk),
        Some(count) => {
            let popped = std::iter::from_fn(|| take(list)).take(count);
            Frame::Array(popped.map(Frame::bulk).collect())
        }
    };
    drop_if_empty(context, &argv[1]);
    Ok(reply)
}

pub(super) fn lpop(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    pop(context, argv, End::Head)
}

pub(super) fn rpop(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    pop(context, argv, End::Tail)
}

pub(super) fn llen(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    Ok(count_frame(
        list_ref(context, &argv[1])?.map_or(0, VecDeque::len),
    ))
}

pub(super) fn lrange(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let (start, end) = (integer(&argv[2])?, integer(&argv[3])?);
    let Some(list) = list_ref(context, &argv[1])? else {
        return Ok(Frame::Array(Vec::new()));
    };
    let items = resolve_range(start, end, list.len()).map_or_else(Vec::new, |(first, last)| {
        list.range(first..=last).cloned().map(Frame::bulk).collect()
    });
    Ok(Frame::Array(items))
}

pub(super) fn lindex(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Some(list) = list_ref(context, &argv[1])? else {
        return Ok(Frame::Null);
    };
    let index = integer(&argv[2])?;
    let len = i64::try_from(list.len()).unwrap_or(i64::MAX);
    let index = if index < 0 { index + len } else { index };
    let found = usize::try_from(index)
        .ok()
        .and_then(|index| list.get(index));
    Ok(found.cloned().map_or(Frame::Null, Frame::bulk))
}

#[cfg(test)]
mod tests {
    use crate::command::harness::{Harness, error};

    use super::*;

    fn strings(items: &[&str]) -> Frame {
        Frame::Array(
            items
                .iter()
                .map(|item| Frame::bulk(Bytes::copy_from_slice(item.as_bytes())))
                .collect(),
        )
    }

    #[test]
    fn push_orders_elements() {
        let mut client = Harness::new();
        assert_eq!(
            client.run(&["LPUSH", "l", "a", "b", "c"]),
            Frame::Integer(3)
        );
        assert_eq!(client.run(&["RPUSH", "l", "x", "y"]), Frame::Integer(5));
        assert_eq!(
            client.run(&["LRANGE", "l", "0", "-1"]),
            strings(&["c", "b", "a", "x", "y"])
        );
        assert_eq!(client.run(&["LLEN", "l"]), Frame::Integer(5));
        assert_eq!(client.run(&["TYPE", "l"]), Frame::simple("list"));
    }

    #[test]
    fn pushx_needs_an_existing_list() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["LPUSHX", "l", "a"]), Frame::Integer(0));
        assert_eq!(client.run(&["EXISTS", "l"]), Frame::Integer(0));
        client.run(&["RPUSH", "l", "a"]);
        assert_eq!(client.run(&["RPUSHX", "l", "b", "c"]), Frame::Integer(3));
        assert_eq!(client.run(&["LPUSHX", "l", "z"]), Frame::Integer(4));
        assert_eq!(
            client.run(&["LRANGE", "l", "0", "-1"]),
            strings(&["z", "a", "b", "c"])
        );
    }

    #[test]
    fn pop_with_and_without_count() {
        let mut client = Harness::new();
        client.run(&["RPUSH", "l", "a", "b", "c", "d"]);
        assert_eq!(client.run(&["LPOP", "l"]), Frame::bulk("a"));
        assert_eq!(client.run(&["RPOP", "l"]), Frame::bulk("d"));
        assert_eq!(client.run(&["LPOP", "l", "0"]), Frame::Array(Vec::new()));
        assert_eq!(client.run(&["RPUSH", "l", "e"]), Frame::Integer(3));
        assert_eq!(client.run(&["RPOP", "l", "2"]), strings(&["e", "c"]));
        assert_eq!(client.run(&["LPOP", "l", "10"]), strings(&["b"]));
        assert_eq!(client.run(&["EXISTS", "l"]), Frame::Integer(0));
        assert_eq!(client.run(&["LPOP", "l"]), Frame::Null);
        assert_eq!(client.run(&["LPOP", "l", "2"]), Frame::NullArray);
    }

    #[test]
    fn pop_errors() {
        let mut client = Harness::new();
        client.run(&["RPUSH", "l", "a"]);
        assert_eq!(
            client.run(&["LPOP", "l", "-1"]),
            error("ERR value is out of range, must be positive")
        );
        assert_eq!(
            client.run(&["LPOP", "l", "x"]),
            error("ERR value is out of range, must be positive")
        );
        assert_eq!(
            client.run(&["LPOP", "l", "1", "2"]),
            error("ERR wrong number of arguments for 'lpop' command")
        );
    }

    #[test]
    fn range_and_index_follow_redis_rules() {
        let mut client = Harness::new();
        client.run(&["RPUSH", "l", "a", "b", "c"]);
        assert_eq!(
            client.run(&["LRANGE", "l", "-2", "-1"]),
            strings(&["b", "c"])
        );
        assert_eq!(
            client.run(&["LRANGE", "l", "-100", "100"]),
            strings(&["a", "b", "c"])
        );
        assert_eq!(client.run(&["LRANGE", "l", "2", "1"]), strings(&[]));
        assert_eq!(client.run(&["LRANGE", "l", "5", "9"]), strings(&[]));
        assert_eq!(client.run(&["LRANGE", "none", "0", "-1"]), strings(&[]));
        assert_eq!(client.run(&["LINDEX", "l", "-1"]), Frame::bulk("c"));
        assert_eq!(client.run(&["LINDEX", "l", "3"]), Frame::Null);
        assert_eq!(client.run(&["LINDEX", "l", "-4"]), Frame::Null);
        assert_eq!(client.run(&["LINDEX", "none", "0"]), Frame::Null);
    }

    #[test]
    fn wrong_type_is_reported() {
        let mut client = Harness::new();
        client.run(&["SET", "s", "v"]);
        client.run(&["RPUSH", "l", "a"]);
        let wrong = error("WRONGTYPE Operation against a key holding the wrong kind of value");
        assert_eq!(client.run(&["LPUSH", "s", "a"]), wrong);
        assert_eq!(client.run(&["LLEN", "s"]), wrong);
        assert_eq!(client.run(&["LRANGE", "s", "0", "1"]), wrong);
        assert_eq!(client.run(&["GET", "l"]), wrong);
        assert_eq!(client.run(&["APPEND", "l", "x"]), wrong);
        assert_eq!(
            client.run(&["MGET", "s", "l"]),
            Frame::Array(vec![Frame::bulk("v"), Frame::Null])
        );
    }
}
