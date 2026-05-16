//! Key commands.

use bytes::Bytes;

use crate::command::Context;
use crate::error::CommandError;
use crate::resp::Frame;
use crate::store::Ttl;
use crate::value::Value;

pub(super) fn exists(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let found = argv[1..]
        .iter()
        .filter(|key| state.store.get(session.db, key, *now_ms).is_some())
        .count();
    Ok(Frame::Integer(i64::try_from(found).unwrap_or(i64::MAX)))
}

pub(super) fn touch(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    exists(context, argv)
}

pub(super) fn type_of(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let name = state
        .store
        .get(session.db, &argv[1], *now_ms)
        .map_or("none", Value::type_name);
    Ok(Frame::simple(name))
}

fn rename_generic(
    context: &mut Context<'_>,
    argv: &[Bytes],
    only_new: bool,
) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    if state.store.get(db, &argv[1], now).is_none() {
        return Err(CommandError::other("ERR no such key"));
    }
    let done = if only_new {
        Frame::Integer(1)
    } else {
        Frame::ok()
    };
    if argv[1] == argv[2] {
        return Ok(if only_new { Frame::Integer(0) } else { done });
    }
    if only_new && state.store.get(db, &argv[2], now).is_some() {
        return Ok(Frame::Integer(0));
    }
    if let Some((value, deadline)) = state.store.take(db, &argv[1], now) {
        let ttl = deadline.map_or(Ttl::Clear, Ttl::At);
        state.store.put(db, argv[2].clone(), value, ttl, now);
    }
    Ok(done)
}

pub(super) fn rename(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    rename_generic(context, argv, false)
}

pub(super) fn renamenx(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    rename_generic(context, argv, true)
}

pub(super) fn unlink(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    del(context, argv)
}

pub(super) fn del(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let removed = argv[1..]
        .iter()
        .filter(|key| state.store.remove(session.db, key, *now_ms))
        .count();
    Ok(Frame::Integer(i64::try_from(removed).unwrap_or(i64::MAX)))
}

#[cfg(test)]
mod tests {
    use crate::command::harness::{Harness, error};

    use super::*;

    #[test]
    fn del_counts_removed_keys() {
        let mut client = Harness::new();
        client.run(&["SET", "a", "1"]);
        client.run(&["SET", "b", "2"]);
        assert_eq!(client.run(&["DEL", "a", "b", "c", "a"]), Frame::Integer(2));
        assert_eq!(client.run(&["GET", "a"]), Frame::Null);
        assert_eq!(client.run(&["DEL", "a"]), Frame::Integer(0));
    }

    #[test]
    fn exists_counts_every_argument() {
        let mut client = Harness::new();
        client.run(&["SET", "a", "1"]);
        assert_eq!(client.run(&["EXISTS", "a", "a", "b"]), Frame::Integer(2));
        assert_eq!(client.run(&["TYPE", "a"]), Frame::simple("string"));
        assert_eq!(client.run(&["TYPE", "b"]), Frame::simple("none"));
    }

    #[test]
    fn rename_moves_the_value_and_its_deadline() {
        let mut client = Harness::new();
        let now = client.now_ms;
        client.run(&["SET", "a", "1", "PX", "100"]);
        client.run(&["SET", "b", "old"]);
        assert_eq!(client.run(&["RENAME", "a", "b"]), Frame::ok());
        assert_eq!(client.run(&["GET", "a"]), Frame::Null);
        assert_eq!(client.run(&["GET", "b"]), Frame::bulk("1"));
        client.now_ms = now + 100;
        assert_eq!(client.run(&["GET", "b"]), Frame::Null);
    }

    #[test]
    fn rename_errors_and_same_key() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["RENAME", "a", "b"]), error("ERR no such key"));
        assert_eq!(
            client.run(&["RENAMENX", "a", "b"]),
            error("ERR no such key")
        );
        client.run(&["SET", "a", "1"]);
        assert_eq!(client.run(&["RENAME", "a", "a"]), Frame::ok());
        assert_eq!(client.run(&["RENAMENX", "a", "a"]), Frame::Integer(0));
    }

    #[test]
    fn renamenx_keeps_an_existing_target() {
        let mut client = Harness::new();
        client.run(&["MSET", "a", "1", "b", "2"]);
        assert_eq!(client.run(&["RENAMENX", "a", "b"]), Frame::Integer(0));
        assert_eq!(client.run(&["GET", "a"]), Frame::bulk("1"));
        assert_eq!(client.run(&["RENAMENX", "a", "c"]), Frame::Integer(1));
        assert_eq!(client.run(&["GET", "c"]), Frame::bulk("1"));
    }

    #[test]
    fn touch_and_unlink() {
        let mut client = Harness::new();
        client.run(&["MSET", "a", "1", "b", "2"]);
        assert_eq!(client.run(&["TOUCH", "a", "b", "c"]), Frame::Integer(2));
        assert_eq!(client.run(&["UNLINK", "a", "c"]), Frame::Integer(1));
        assert_eq!(client.run(&["EXISTS", "a"]), Frame::Integer(0));
    }
}
