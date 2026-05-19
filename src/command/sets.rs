//! Set commands.

use std::collections::HashSet;

use bytes::Bytes;

use crate::command::Context;
use crate::error::CommandError;
use crate::resp::Frame;
use crate::value::Value;
use crate::{number, random};

type Set = HashSet<Bytes>;

fn count_frame(len: usize) -> Frame {
    Frame::Integer(i64::try_from(len).unwrap_or(i64::MAX))
}

fn set_ref<'a>(context: &'a mut Context<'_>, key: &[u8]) -> Result<Option<&'a Set>, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    match state.store.get(session.db, key, *now_ms) {
        None => Ok(None),
        Some(Value::Set(set)) => Ok(Some(set)),
        Some(_) => Err(CommandError::WrongType),
    }
}

fn set_mut<'a>(
    context: &'a mut Context<'_>,
    key: &[u8],
) -> Result<Option<&'a mut Set>, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    match state.store.get_mut(session.db, key, *now_ms) {
        None => Ok(None),
        Some(Value::Set(set)) => Ok(Some(set)),
        Some(_) => Err(CommandError::WrongType),
    }
}

fn drop_if_empty(context: &mut Context<'_>, key: &[u8]) {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let empty = matches!(state.store.get(session.db, key, *now_ms), Some(Value::Set(set)) if set.is_empty());
    if empty {
        state.store.remove(session.db, key, *now_ms);
    }
}

pub(super) fn sadd(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    if set_ref(context, &argv[1])?.is_none() {
        let Context { state, session, .. } = context;
        state
            .store
            .set(session.db, argv[1].clone(), Value::Set(Set::new()));
    }
    let set = set_mut(context, &argv[1])?.ok_or(CommandError::WrongType)?;
    let added = argv[2..]
        .iter()
        .filter(|member| set.insert((*member).clone()))
        .count();
    Ok(count_frame(added))
}

pub(super) fn srem(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Some(set) = set_mut(context, &argv[1])? else {
        return Ok(Frame::Integer(0));
    };
    let removed = argv[2..]
        .iter()
        .filter(|member| set.remove(*member))
        .count();
    drop_if_empty(context, &argv[1]);
    Ok(count_frame(removed))
}

pub(super) fn smembers(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let members = set_ref(context, &argv[1])?.map_or_else(Vec::new, |set| {
        set.iter().cloned().map(Frame::bulk).collect()
    });
    Ok(Frame::Set(members))
}

pub(super) fn sismember(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let found = set_ref(context, &argv[1])?.is_some_and(|set| set.contains(&argv[2]));
    Ok(Frame::Integer(i64::from(found)))
}

pub(super) fn smismember(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let set = set_ref(context, &argv[1])?;
    let flags = argv[2..]
        .iter()
        .map(|member| Frame::Integer(i64::from(set.is_some_and(|set| set.contains(member)))))
        .collect();
    Ok(Frame::Array(flags))
}

pub(super) fn scard(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    Ok(count_frame(set_ref(context, &argv[1])?.map_or(0, Set::len)))
}

/// Picks `count` distinct random members, or all of them when the set is smaller.
fn sample_distinct(set: &Set, count: usize) -> Vec<Bytes> {
    let mut members: Vec<Bytes> = set.iter().cloned().collect();
    let take = count.min(members.len());
    for index in 0..take {
        let pick = index + random::below(members.len() - index);
        members.swap(index, pick);
    }
    members.truncate(take);
    members
}

pub(super) fn spop(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    if argv.len() > 3 {
        return Err(CommandError::Syntax);
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
    let Some(set) = set_mut(context, &argv[1])? else {
        return Ok(if count.is_some() {
            Frame::Set(Vec::new())
        } else {
            Frame::Null
        });
    };
    let popped = sample_distinct(set, count.unwrap_or(1));
    for member in &popped {
        set.remove(member);
    }
    drop_if_empty(context, &argv[1]);
    if count.is_some() {
        Ok(Frame::Set(popped.into_iter().map(Frame::bulk).collect()))
    } else {
        Ok(popped.into_iter().next().map_or(Frame::Null, Frame::bulk))
    }
}

pub(super) fn srandmember(
    context: &mut Context<'_>,
    argv: &[Bytes],
) -> Result<Frame, CommandError> {
    if argv.len() > 3 {
        return Err(CommandError::Syntax);
    }
    let Some(arg) = argv.get(2) else {
        let Some(set) = set_ref(context, &argv[1])? else {
            return Ok(Frame::Null);
        };
        let pick = sample_distinct(set, 1).into_iter().next();
        return Ok(pick.map_or(Frame::Null, Frame::bulk));
    };
    let count = number::parse_i64(arg).ok_or(CommandError::NotInteger)?;
    if count == i64::MIN {
        return Err(CommandError::other(
            "ERR value is out of range, value must between -9223372036854775807 and 9223372036854775807",
        ));
    }
    let Some(set) = set_ref(context, &argv[1])? else {
        return Ok(Frame::Array(Vec::new()));
    };
    let wanted = usize::try_from(count.unsigned_abs()).unwrap_or(usize::MAX);
    let members = if count < 0 {
        let all: Vec<&Bytes> = set.iter().collect();
        let picks = (0..wanted).map(|_| all[random::below(all.len())].clone());
        picks.collect()
    } else {
        sample_distinct(set, wanted)
    };
    Ok(Frame::Array(members.into_iter().map(Frame::bulk).collect()))
}

#[cfg(test)]
mod tests {
    use crate::command::harness::{Harness, error};

    use super::*;

    fn sorted(frame: Frame) -> Vec<String> {
        let (Frame::Array(items) | Frame::Set(items)) = frame else {
            panic!("collection: {frame:?}")
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
    fn sadd_srem_scard() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["SADD", "s", "a", "b", "a"]), Frame::Integer(2));
        assert_eq!(client.run(&["SADD", "s", "b", "c"]), Frame::Integer(1));
        assert_eq!(client.run(&["SCARD", "s"]), Frame::Integer(3));
        assert_eq!(
            client.run(&["SREM", "s", "a", "zz", "a"]),
            Frame::Integer(1)
        );
        assert_eq!(sorted(client.run(&["SMEMBERS", "s"])), ["b", "c"]);
        assert_eq!(client.run(&["SREM", "s", "b", "c"]), Frame::Integer(2));
        assert_eq!(client.run(&["EXISTS", "s"]), Frame::Integer(0));
        assert_eq!(client.run(&["SCARD", "s"]), Frame::Integer(0));
        assert_eq!(client.run(&["SMEMBERS", "s"]), Frame::Set(Vec::new()));
        assert_eq!(client.run(&["TYPE", "s"]), Frame::simple("none"));
    }

    #[test]
    fn membership_checks() {
        let mut client = Harness::new();
        client.run(&["SADD", "s", "a", "b"]);
        assert_eq!(client.run(&["SISMEMBER", "s", "a"]), Frame::Integer(1));
        assert_eq!(client.run(&["SISMEMBER", "s", "z"]), Frame::Integer(0));
        assert_eq!(
            client.run(&["SMISMEMBER", "s", "a", "z", "b"]),
            Frame::Array(vec![
                Frame::Integer(1),
                Frame::Integer(0),
                Frame::Integer(1)
            ])
        );
        assert_eq!(
            client.run(&["SMISMEMBER", "none", "a"]),
            Frame::Array(vec![Frame::Integer(0)])
        );
    }

    #[test]
    fn spop_removes_members() {
        let mut client = Harness::new();
        client.run(&["SADD", "s", "a", "b", "c"]);
        let Frame::BulkString(first) = client.run(&["SPOP", "s"]) else {
            panic!("bulk")
        };
        assert_eq!(client.run(&["SCARD", "s"]), Frame::Integer(2));
        assert_eq!(
            client.run(&["SISMEMBER", "s", &String::from_utf8_lossy(&first)]),
            Frame::Integer(0)
        );
        assert_eq!(sorted(client.run(&["SPOP", "s", "10"])).len(), 2);
        assert_eq!(client.run(&["EXISTS", "s"]), Frame::Integer(0));
        assert_eq!(client.run(&["SPOP", "s"]), Frame::Null);
        assert_eq!(client.run(&["SPOP", "s", "2"]), Frame::Set(Vec::new()));
        assert_eq!(
            client.run(&["SPOP", "s", "-1"]),
            error("ERR value is out of range, must be positive")
        );
        assert_eq!(
            client.run(&["SPOP", "s", "1", "2"]),
            error("ERR syntax error")
        );
    }

    #[test]
    fn srandmember_counts() {
        let mut client = Harness::new();
        client.run(&["SADD", "s", "a", "b", "c"]);
        assert_eq!(
            sorted(client.run(&["SRANDMEMBER", "s", "10"])),
            ["a", "b", "c"]
        );
        assert_eq!(sorted(client.run(&["SRANDMEMBER", "s", "2"])).len(), 2);
        assert_eq!(sorted(client.run(&["SRANDMEMBER", "s", "-7"])).len(), 7);
        assert_eq!(
            client.run(&["SRANDMEMBER", "s", "0"]),
            Frame::Array(Vec::new())
        );
        assert_eq!(
            client.run(&["SRANDMEMBER", "none", "3"]),
            Frame::Array(Vec::new())
        );
        assert_eq!(client.run(&["SRANDMEMBER", "none"]), Frame::Null);
        assert_eq!(client.run(&["SCARD", "s"]), Frame::Integer(3));
        assert_eq!(
            client.run(&["SRANDMEMBER", "s", "x"]),
            error("ERR value is not an integer or out of range")
        );
    }

    #[test]
    fn wrong_type_is_reported() {
        let mut client = Harness::new();
        client.run(&["SET", "str", "v"]);
        client.run(&["SADD", "s", "a"]);
        let wrong = error("WRONGTYPE Operation against a key holding the wrong kind of value");
        assert_eq!(client.run(&["SADD", "str", "a"]), wrong);
        assert_eq!(client.run(&["SMEMBERS", "str"]), wrong);
        assert_eq!(client.run(&["GET", "s"]), wrong);
        assert_eq!(client.run(&["HGET", "s", "a"]), wrong);
        assert_eq!(client.run(&["SPOP", "str"]), wrong);
    }
}
