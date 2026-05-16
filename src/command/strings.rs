//! String commands.

use bytes::Bytes;

use crate::command::Context;
use crate::error::CommandError;
use crate::resp::Frame;
use crate::store::Ttl;
use crate::value::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Ex,
    Px,
    ExAt,
    PxAt,
}

enum Expire<'a> {
    None,
    KeepTtl,
    Persist,
    After(Unit, &'a Bytes),
}

enum Condition<'a> {
    None,
    Nx,
    Xx,
    IfEq(&'a Bytes),
    IfNe(&'a Bytes),
}

struct Options<'a> {
    condition: Condition<'a>,
    get: bool,
    expire: Expire<'a>,
}

fn as_str(value: &Value) -> Result<&Bytes, CommandError> {
    match value {
        Value::Str(data) => Ok(data),
    }
}

fn deadline(
    amount: &Bytes,
    unit: Unit,
    now_ms: u64,
    command: &'static str,
) -> Result<u64, CommandError> {
    let amount = crate::number::parse_i64(amount).ok_or(CommandError::NotInteger)?;
    let invalid = CommandError::InvalidExpire(command);
    let seconds = matches!(unit, Unit::Ex | Unit::ExAt);
    if amount <= 0 || (seconds && amount > i64::MAX / 1000) {
        return Err(invalid);
    }
    let millis = if seconds { amount * 1000 } else { amount };
    let absolute = if matches!(unit, Unit::Ex | Unit::Px) {
        millis
            .checked_add(i64::try_from(now_ms).unwrap_or(i64::MAX))
            .ok_or(invalid)?
    } else {
        millis
    };
    u64::try_from(absolute).map_err(|_| CommandError::InvalidExpire(command))
}

fn parse_options(argv: &[Bytes], start: usize, for_set: bool) -> Result<Options<'_>, CommandError> {
    let mut options = Options {
        condition: Condition::None,
        get: false,
        expire: Expire::None,
    };
    let mut index = start;
    while index < argv.len() {
        let name = argv[index].to_ascii_lowercase();
        let next = argv.get(index + 1);
        let unit = match name.as_slice() {
            b"ex" => Some(Unit::Ex),
            b"px" => Some(Unit::Px),
            b"exat" => Some(Unit::ExAt),
            b"pxat" => Some(Unit::PxAt),
            _ => None,
        };
        match (name.as_slice(), unit, next) {
            (b"nx", _, _)
                if for_set && matches!(options.condition, Condition::None | Condition::Nx) =>
            {
                options.condition = Condition::Nx;
            }
            (b"xx", _, _)
                if for_set && matches!(options.condition, Condition::None | Condition::Xx) =>
            {
                options.condition = Condition::Xx;
            }
            (b"get", _, _) if for_set => options.get = true,
            (b"ifeq", _, Some(value))
                if for_set && matches!(options.condition, Condition::None | Condition::IfEq(_)) =>
            {
                options.condition = Condition::IfEq(value);
                index += 1;
            }
            (b"ifne", _, Some(value))
                if for_set && matches!(options.condition, Condition::None | Condition::IfNe(_)) =>
            {
                options.condition = Condition::IfNe(value);
                index += 1;
            }
            (b"keepttl", _, _)
                if for_set && matches!(options.expire, Expire::None | Expire::KeepTtl) =>
            {
                options.expire = Expire::KeepTtl;
            }
            (b"persist", _, _)
                if !for_set && matches!(options.expire, Expire::None | Expire::Persist) =>
            {
                options.expire = Expire::Persist;
            }
            (_, Some(unit), Some(amount))
                if match options.expire {
                    Expire::None => true,
                    Expire::After(previous, _) => previous == unit,
                    Expire::KeepTtl | Expire::Persist => false,
                } =>
            {
                options.expire = Expire::After(unit, amount);
                index += 1;
            }
            _ => return Err(CommandError::Syntax),
        }
        index += 1;
    }
    Ok(options)
}

pub(super) fn get(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    match state.store.get(session.db, &argv[1], *now_ms) {
        Some(value) => Ok(Frame::bulk(as_str(value)?.clone())),
        None => Ok(Frame::Null),
    }
}

pub(super) fn set(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let options = parse_options(argv, 3, true)?;
    set_generic(context, &argv[1], &argv[2], &options, "set")
}

fn set_generic(
    context: &mut Context<'_>,
    key: &Bytes,
    value: &Bytes,
    options: &Options<'_>,
    command: &'static str,
) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let deadline = match options.expire {
        Expire::After(unit, amount) => Some(deadline(amount, unit, now, command)?),
        _ => None,
    };
    let current = state
        .store
        .get(db, key, now)
        .map(as_str)
        .transpose()?
        .cloned();
    let reply_to_get = || current.clone().map_or(Frame::Null, Frame::bulk);
    let abort = || {
        if options.get {
            reply_to_get()
        } else {
            Frame::Null
        }
    };
    let found = current.is_some();
    let blocked = match (&options.condition, &current) {
        (Condition::Nx, Some(_)) | (Condition::Xx | Condition::IfEq(_), None) => true,
        (Condition::IfEq(expected), Some(data)) => data != *expected,
        (Condition::IfNe(expected), Some(data)) => data == *expected,
        _ => false,
    };
    if blocked {
        return Ok(abort());
    }
    let reply = if options.get {
        reply_to_get()
    } else {
        Frame::ok()
    };
    if deadline.is_some_and(|at| at <= now) {
        if found {
            state.store.remove(db, key, now);
        }
        return Ok(reply);
    }
    let ttl = match (&options.expire, deadline) {
        (_, Some(at)) => Ttl::At(at),
        (Expire::KeepTtl, None) => Ttl::Keep,
        _ => Ttl::Clear,
    };
    state
        .store
        .put(db, key.clone(), Value::Str(value.clone()), ttl, now);
    Ok(reply)
}

pub(super) fn setnx(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let options = Options {
        condition: Condition::Nx,
        get: false,
        expire: Expire::None,
    };
    let stored = set_generic(context, &argv[1], &argv[2], &options, "setnx")?;
    Ok(Frame::Integer(i64::from(stored == Frame::ok())))
}

fn set_with_expiry(
    context: &mut Context<'_>,
    argv: &[Bytes],
    unit: Unit,
    command: &'static str,
) -> Result<Frame, CommandError> {
    let options = Options {
        condition: Condition::None,
        get: false,
        expire: Expire::After(unit, &argv[2]),
    };
    set_generic(context, &argv[1], &argv[3], &options, command)
}

pub(super) fn setex(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    set_with_expiry(context, argv, Unit::Ex, "setex")
}

pub(super) fn psetex(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    set_with_expiry(context, argv, Unit::Px, "psetex")
}

pub(super) fn getset(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let old = state
        .store
        .get(db, &argv[1], now)
        .map(as_str)
        .transpose()?
        .cloned();
    state
        .store
        .set(db, argv[1].clone(), Value::Str(argv[2].clone()));
    Ok(old.map_or(Frame::Null, Frame::bulk))
}

pub(super) fn getdel(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let old = state
        .store
        .get(db, &argv[1], now)
        .map(as_str)
        .transpose()?
        .cloned();
    state.store.remove(db, &argv[1], now);
    Ok(old.map_or(Frame::Null, Frame::bulk))
}

pub(super) fn getex(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let options = parse_options(argv, 2, false)?;
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let Some(current) = state
        .store
        .get(db, &argv[1], now)
        .map(as_str)
        .transpose()?
        .cloned()
    else {
        return Ok(Frame::Null);
    };
    match options.expire {
        Expire::After(unit, amount) => {
            let at = deadline(amount, unit, now, "getex")?;
            if at <= now {
                state.store.remove(db, &argv[1], now);
            } else {
                state.store.set_deadline(db, &argv[1], Some(at), now);
            }
        }
        Expire::Persist => {
            state.store.set_deadline(db, &argv[1], None, now);
        }
        Expire::None | Expire::KeepTtl => {}
    }
    Ok(Frame::bulk(current))
}

#[cfg(test)]
mod tests {
    use crate::command::harness::{Harness, error};

    use super::*;

    fn nil() -> Frame {
        Frame::Null
    }

    #[test]
    fn set_then_get() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["GET", "k"]), nil());
        assert_eq!(client.run(&["SET", "k", "v"]), Frame::ok());
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk("v"));
        assert_eq!(client.run(&["SET", "k", ""]), Frame::ok());
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk(""));
    }

    #[test]
    fn keys_live_in_the_selected_database() {
        let mut client = Harness::new();
        client.run(&["SET", "k", "zero"]);
        client.run(&["SELECT", "1"]);
        assert_eq!(client.run(&["GET", "k"]), nil());
        client.run(&["SET", "k", "one"]);
        client.run(&["SELECT", "0"]);
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk("zero"));
    }

    #[test]
    fn arity_and_syntax() {
        let mut client = Harness::new();
        assert_eq!(
            client.run(&["GET"]),
            error("ERR wrong number of arguments for 'get' command")
        );
        assert_eq!(
            client.run(&["SET", "k"]),
            error("ERR wrong number of arguments for 'set' command")
        );
        for options in [
            vec!["nope"],
            vec!["NX", "XX"],
            vec!["EX"],
            vec!["EX", "1", "PX", "5"],
            vec!["KEEPTTL", "EX", "1"],
            vec!["IFEQ"],
            vec!["IFEQ", "a", "NX"],
            vec!["PERSIST"],
        ] {
            let mut parts = vec!["SET", "k", "v"];
            parts.extend(options);
            assert_eq!(client.run(&parts), error("ERR syntax error"), "{parts:?}");
        }
    }

    #[test]
    fn nx_xx_and_get() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["SET", "k", "a", "XX"]), nil());
        assert_eq!(client.run(&["SET", "k", "a", "NX"]), Frame::ok());
        assert_eq!(client.run(&["SET", "k", "b", "NX"]), nil());
        assert_eq!(
            client.run(&["SET", "k", "b", "xx", "get"]),
            Frame::bulk("a")
        );
        assert_eq!(
            client.run(&["SET", "k", "c", "NX", "GET"]),
            Frame::bulk("b")
        );
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk("b"));
        assert_eq!(client.run(&["SET", "new", "v", "GET"]), nil());
    }

    #[test]
    fn ifeq_and_ifne() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["SET", "k", "a", "IFEQ", "a"]), nil());
        assert_eq!(client.run(&["SET", "k", "a", "IFNE", "z"]), Frame::ok());
        assert_eq!(client.run(&["SET", "k", "b", "IFEQ", "x"]), nil());
        assert_eq!(client.run(&["SET", "k", "b", "ifeq", "a"]), Frame::ok());
        assert_eq!(client.run(&["SET", "k", "c", "IFNE", "b"]), nil());
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk("b"));
    }

    #[test]
    fn expiry_options() {
        let mut client = Harness::new();
        let now = client.now_ms;
        assert_eq!(client.run(&["SET", "k", "v", "EX", "10"]), Frame::ok());
        client.now_ms = now + 9_999;
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk("v"));
        client.now_ms = now + 10_000;
        assert_eq!(client.run(&["GET", "k"]), nil());
        client.now_ms = now;
        client.run(&["SET", "k", "v", "PX", "50"]);
        client.now_ms = now + 50;
        assert_eq!(client.run(&["GET", "k"]), nil());
        client.now_ms = now;
        let at = (now + 5_000).to_string();
        client.run(&["SET", "k", "v", "PXAT", &at]);
        client.run(&["SET", "k", "w", "KEEPTTL"]);
        client.now_ms = now + 5_000;
        assert_eq!(client.run(&["GET", "k"]), nil());
    }

    #[test]
    fn plain_set_clears_the_expiry() {
        let mut client = Harness::new();
        let now = client.now_ms;
        client.run(&["SET", "k", "v", "EX", "1"]);
        client.run(&["SET", "k", "w"]);
        client.now_ms = now + 5_000;
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk("w"));
    }

    #[test]
    fn a_past_deadline_deletes_the_key() {
        let mut client = Harness::new();
        client.run(&["SET", "k", "v"]);
        assert_eq!(client.run(&["SET", "k", "w", "PXAT", "1"]), Frame::ok());
        assert_eq!(client.run(&["GET", "k"]), nil());
    }

    #[test]
    fn bad_expire_values() {
        let mut client = Harness::new();
        let invalid = error("ERR invalid expire time in 'set' command");
        let not_int = error("ERR value is not an integer or out of range");
        assert_eq!(client.run(&["SET", "k", "v", "EX", "0"]), invalid);
        assert_eq!(client.run(&["SET", "k", "v", "EX", "-1"]), invalid);
        assert_eq!(
            client.run(&["SET", "k", "v", "EX", "9223372036854775807"]),
            invalid
        );
        assert_eq!(
            client.run(&["SET", "k", "v", "PX", "9223372036854775807"]),
            invalid
        );
        assert_eq!(client.run(&["SET", "k", "v", "EX", "abc"]), not_int);
        assert_eq!(client.run(&["GET", "k"]), nil());
    }

    #[test]
    fn setnx_setex_psetex() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["SETNX", "k", "a"]), Frame::Integer(1));
        assert_eq!(client.run(&["SETNX", "k", "b"]), Frame::Integer(0));
        assert_eq!(client.run(&["SETEX", "e", "10", "v"]), Frame::ok());
        assert_eq!(client.run(&["PSETEX", "p", "10000", "v"]), Frame::ok());
        assert_eq!(
            client.run(&["SETEX", "e", "0", "v"]),
            error("ERR invalid expire time in 'setex' command")
        );
        assert_eq!(
            client.run(&["PSETEX", "e", "-1", "v"]),
            error("ERR invalid expire time in 'psetex' command")
        );
        assert_eq!(
            client.run(&["SETEX", "e", "x", "v"]),
            error("ERR value is not an integer or out of range")
        );
    }

    #[test]
    fn getset_getdel() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["GETSET", "k", "a"]), nil());
        assert_eq!(client.run(&["GETSET", "k", "b"]), Frame::bulk("a"));
        assert_eq!(client.run(&["GETDEL", "k"]), Frame::bulk("b"));
        assert_eq!(client.run(&["GETDEL", "k"]), nil());
        assert_eq!(client.run(&["GET", "k"]), nil());
    }

    #[test]
    fn getex_changes_the_deadline() {
        let mut client = Harness::new();
        let now = client.now_ms;
        assert_eq!(client.run(&["GETEX", "k", "EX", "5"]), nil());
        client.run(&["SET", "k", "v"]);
        assert_eq!(client.run(&["GETEX", "k", "PX", "500"]), Frame::bulk("v"));
        client.now_ms = now + 500;
        assert_eq!(client.run(&["GET", "k"]), nil());
        client.now_ms = now;
        client.run(&["SET", "k", "v", "EX", "1"]);
        assert_eq!(client.run(&["GETEX", "k", "PERSIST"]), Frame::bulk("v"));
        client.now_ms = now + 5_000;
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk("v"));
        assert_eq!(client.run(&["GETEX", "k", "PXAT", "1"]), Frame::bulk("v"));
        assert_eq!(client.run(&["GET", "k"]), nil());
    }

    #[test]
    fn getex_errors() {
        let mut client = Harness::new();
        client.run(&["SET", "k", "v"]);
        assert_eq!(
            client.run(&["GETEX", "k", "EX", "0"]),
            error("ERR invalid expire time in 'getex' command")
        );
        assert_eq!(
            client.run(&["GETEX", "k", "EX", "1", "PX", "5"]),
            error("ERR syntax error")
        );
        assert_eq!(
            client.run(&["GETEX", "k", "PERSIST", "EX", "5"]),
            error("ERR syntax error")
        );
        assert_eq!(client.run(&["GETEX", "k", "NX"]), error("ERR syntax error"));
        assert_eq!(client.run(&["GETEX", "missing", "EX", "0"]), nil());
    }
}
