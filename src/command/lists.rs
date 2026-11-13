use std::collections::VecDeque;

use bytes::Bytes;

use crate::command::{Context, count_frame, drop_when_empty, typed_mut, typed_ref};
use crate::error::CommandError;
use crate::number;
use crate::resp::Frame;
use crate::value::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Head,
    Tail,
}

fn list_ref<'a>(
    context: &'a mut Context<'_>,
    key: &[u8],
) -> Result<Option<&'a VecDeque<Bytes>>, CommandError> {
    typed_ref(context, key, |value| match value {
        Value::List(list) => Some(list),
        _ => None,
    })
}

fn list_mut<'a>(
    context: &'a mut Context<'_>,
    key: &[u8],
) -> Result<Option<&'a mut VecDeque<Bytes>>, CommandError> {
    typed_mut(context, key, |value| match value {
        Value::List(list) => Some(list),
        _ => None,
    })
}

fn drop_if_empty(context: &mut Context<'_>, key: &[u8]) {
    drop_when_empty(
        context,
        key,
        |value| matches!(value, Value::List(list) if list.is_empty()),
    );
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

pub(super) fn lset(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Some(list) = list_mut(context, &argv[1])? else {
        return Err(CommandError::other("ERR no such key"));
    };
    let index = integer(&argv[2])?;
    let len = i64::try_from(list.len()).unwrap_or(i64::MAX);
    let index = if index < 0 { index + len } else { index };
    let slot = usize::try_from(index)
        .ok()
        .and_then(|index| list.get_mut(index));
    let slot = slot.ok_or_else(|| CommandError::other("ERR index out of range"))?;
    slot.clone_from(&argv[3]);
    Ok(Frame::ok())
}

pub(super) fn linsert(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let after = match argv[2].to_ascii_lowercase().as_slice() {
        b"after" => true,
        b"before" => false,
        _ => return Err(CommandError::Syntax),
    };
    let Some(list) = list_mut(context, &argv[1])? else {
        return Ok(Frame::Integer(0));
    };
    let Some(position) = list.iter().position(|element| *element == argv[3]) else {
        return Ok(Frame::Integer(-1));
    };
    list.insert(position + usize::from(after), argv[4].clone());
    Ok(count_frame(list.len()))
}

pub(super) fn lrem(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let count = integer(&argv[2])?;
    if count == i64::MIN {
        return Err(CommandError::other(
            "ERR value is out of range, value must between -9223372036854775807 and 9223372036854775807",
        ));
    }
    let Some(list) = list_mut(context, &argv[1])? else {
        return Ok(Frame::Integer(0));
    };
    let limit = if count == 0 {
        usize::MAX
    } else {
        usize::try_from(count.unsigned_abs()).unwrap_or(usize::MAX)
    };
    let matching = list
        .iter()
        .enumerate()
        .filter(|(_, element)| **element == argv[3])
        .map(|(index, _)| index);
    let mut hits: Vec<usize> = if count < 0 {
        matching
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .take(limit)
            .collect()
    } else {
        matching.take(limit).collect()
    };
    hits.sort_unstable_by(|left, right| right.cmp(left));
    for index in &hits {
        list.remove(*index);
    }
    let removed = hits.len();
    drop_if_empty(context, &argv[1]);
    Ok(count_frame(removed))
}

pub(super) fn ltrim(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let (start, end) = (integer(&argv[2])?, integer(&argv[3])?);
    let Some(list) = list_mut(context, &argv[1])? else {
        return Ok(Frame::ok());
    };
    match resolve_range(start, end, list.len()) {
        Some((low, high)) => {
            list.truncate(high + 1);
            list.drain(..low);
        }
        None => list.clear(),
    }
    drop_if_empty(context, &argv[1]);
    Ok(Frame::ok())
}

fn direction(arg: &Bytes) -> Result<End, CommandError> {
    match arg.to_ascii_lowercase().as_slice() {
        b"left" => Ok(End::Head),
        b"right" => Ok(End::Tail),
        _ => Err(CommandError::Syntax),
    }
}

fn move_element(
    context: &mut Context<'_>,
    argv: &[Bytes],
    from: End,
    to: End,
) -> Result<Frame, CommandError> {
    let Some(source) = list_mut(context, &argv[1])? else {
        return Ok(Frame::Null);
    };
    if source.is_empty() {
        return Ok(Frame::Null);
    }
    if argv[1] != argv[2] && list_ref(context, &argv[2])?.is_none() {
        let Context { state, session, .. } = context;
        state
            .store
            .set(session.db, argv[2].clone(), Value::List(VecDeque::new()));
    }
    let element = match list_mut(context, &argv[1])? {
        Some(source) if from == End::Head => source.pop_front(),
        Some(source) => source.pop_back(),
        None => None,
    };
    let Some(element) = element else {
        return Ok(Frame::Null);
    };
    if let Some(target) = list_mut(context, &argv[2])? {
        match to {
            End::Head => target.push_front(element.clone()),
            End::Tail => target.push_back(element.clone()),
        }
    }
    drop_if_empty(context, &argv[1]);
    Ok(Frame::bulk(element))
}

pub(super) fn lmove(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let (from, to) = (direction(&argv[3])?, direction(&argv[4])?);
    move_element(context, argv, from, to)
}

pub(super) fn rpoplpush(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    move_element(context, argv, End::Tail, End::Head)
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
    fn lset_linsert_lrem() {
        let mut client = Harness::new();
        client.run(&["RPUSH", "l", "a", "b", "a", "c", "a"]);
        assert_eq!(client.run(&["LSET", "l", "1", "B"]), Frame::ok());
        assert_eq!(client.run(&["LSET", "l", "-1", "Z"]), Frame::ok());
        assert_eq!(
            client.run(&["LSET", "l", "9", "x"]),
            error("ERR index out of range")
        );
        assert_eq!(
            client.run(&["LSET", "none", "0", "x"]),
            error("ERR no such key")
        );
        assert_eq!(
            client.run(&["LINSERT", "l", "BEFORE", "c", "x"]),
            Frame::Integer(6)
        );
        assert_eq!(
            client.run(&["LINSERT", "l", "after", "Z", "y"]),
            Frame::Integer(7)
        );
        assert_eq!(
            client.run(&["LINSERT", "l", "AFTER", "q", "y"]),
            Frame::Integer(-1)
        );
        assert_eq!(
            client.run(&["LINSERT", "none", "AFTER", "q", "y"]),
            Frame::Integer(0)
        );
        assert_eq!(
            client.run(&["LINSERT", "l", "MIDDLE", "q", "y"]),
            error("ERR syntax error")
        );
        assert_eq!(
            client.run(&["LRANGE", "l", "0", "-1"]),
            strings(&["a", "B", "a", "x", "c", "Z", "y"])
        );
        assert_eq!(client.run(&["LREM", "l", "-1", "a"]), Frame::Integer(1));
        assert_eq!(client.run(&["LREM", "l", "0", "a"]), Frame::Integer(1));
        assert_eq!(client.run(&["LREM", "l", "5", "nope"]), Frame::Integer(0));
        assert_eq!(
            client.run(&["LRANGE", "l", "0", "-1"]),
            strings(&["B", "x", "c", "Z", "y"])
        );
    }

    #[test]
    fn ltrim_keeps_a_window() {
        let mut client = Harness::new();
        client.run(&["RPUSH", "l", "a", "b", "c", "d", "e"]);
        assert_eq!(client.run(&["LTRIM", "l", "1", "-2"]), Frame::ok());
        assert_eq!(
            client.run(&["LRANGE", "l", "0", "-1"]),
            strings(&["b", "c", "d"])
        );
        assert_eq!(client.run(&["LTRIM", "l", "5", "9"]), Frame::ok());
        assert_eq!(client.run(&["EXISTS", "l"]), Frame::Integer(0));
        assert_eq!(client.run(&["LTRIM", "none", "0", "1"]), Frame::ok());
    }

    #[test]
    fn lmove_and_rpoplpush() {
        let mut client = Harness::new();
        client.run(&["RPUSH", "src", "a", "b", "c"]);
        assert_eq!(
            client.run(&["LMOVE", "src", "dst", "LEFT", "RIGHT"]),
            Frame::bulk("a")
        );
        assert_eq!(client.run(&["RPOPLPUSH", "src", "dst"]), Frame::bulk("c"));
        assert_eq!(
            client.run(&["LRANGE", "dst", "0", "-1"]),
            strings(&["c", "a"])
        );
        assert_eq!(
            client.run(&["LMOVE", "src", "src", "right", "left"]),
            Frame::bulk("b")
        );
        assert_eq!(client.run(&["LRANGE", "src", "0", "-1"]), strings(&["b"]));
        assert_eq!(
            client.run(&["LMOVE", "none", "dst", "LEFT", "LEFT"]),
            Frame::Null
        );
        assert_eq!(
            client.run(&["LMOVE", "src", "dst", "UP", "LEFT"]),
            error("ERR syntax error")
        );
        client.run(&["SET", "s", "v"]);
        assert_eq!(
            client.run(&["LMOVE", "src", "s", "LEFT", "LEFT"]),
            error("WRONGTYPE Operation against a key holding the wrong kind of value")
        );
        assert_eq!(client.run(&["LRANGE", "src", "0", "-1"]), strings(&["b"]));
        client.run(&["LMOVE", "src", "other", "LEFT", "LEFT"]);
        assert_eq!(client.run(&["EXISTS", "src"]), Frame::Integer(0));
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
