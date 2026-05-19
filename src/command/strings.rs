//! String commands.

use bytes::{Bytes, BytesMut};

use crate::command::Context;
use crate::error::CommandError;
use crate::long_double::{LongDouble, Parsed};
use crate::number;
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
        Value::List(_) | Value::Hash(_) | Value::Set(_) => Err(CommandError::WrongType),
    }
}

const MAX_STRING_LEN: usize = 512 * 1024 * 1024;

fn too_long() -> CommandError {
    CommandError::other("ERR string exceeds maximum allowed size (proto-max-bulk-len)")
}

fn edit(data: &mut Bytes, change: impl FnOnce(&mut BytesMut)) {
    let mut buffer = match std::mem::take(data).try_into_mut() {
        Ok(buffer) => buffer,
        Err(shared) => BytesMut::from(&shared[..]),
    };
    change(&mut buffer);
    *data = buffer.freeze();
}

fn deadline(
    amount: &Bytes,
    unit: Unit,
    now_ms: u64,
    command: &'static str,
) -> Result<u64, CommandError> {
    let amount = number::parse_i64(amount).ok_or(CommandError::NotInteger)?;
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
    let existing = state.store.get(db, key, now);
    let found = existing.is_some();
    let needs_value =
        options.get || matches!(options.condition, Condition::IfEq(_) | Condition::IfNe(_));
    let current = if needs_value {
        existing.map(as_str).transpose()?.cloned()
    } else {
        None
    };
    let reply_to_get = || current.clone().map_or(Frame::Null, Frame::bulk);
    let abort = || {
        if options.get {
            reply_to_get()
        } else {
            Frame::Null
        }
    };
    let blocked = match &options.condition {
        Condition::Nx => found,
        Condition::Xx => !found,
        Condition::IfEq(expected) => current.as_ref().is_none_or(|data| data != *expected),
        Condition::IfNe(expected) => current.as_ref().is_some_and(|data| data == *expected),
        Condition::None => false,
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

fn increment(context: &mut Context<'_>, key: &Bytes, by: i64) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let current = match state.store.get(db, key, now) {
        Some(value) => number::parse_i64(as_str(value)?).ok_or(CommandError::NotInteger)?,
        None => 0,
    };
    let updated = current
        .checked_add(by)
        .ok_or_else(|| CommandError::other("ERR increment or decrement would overflow"))?;
    let text = Bytes::from(updated.to_string());
    state
        .store
        .put(db, key.clone(), Value::Str(text), Ttl::Keep, now);
    Ok(Frame::Integer(updated))
}

fn amount(arg: &Bytes) -> Result<i64, CommandError> {
    number::parse_i64(arg).ok_or(CommandError::NotInteger)
}

pub(super) fn incr(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    increment(context, &argv[1], 1)
}

pub(super) fn decr(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    increment(context, &argv[1], -1)
}

pub(super) fn incrby(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    increment(context, &argv[1], amount(&argv[2])?)
}

pub(super) fn decrby(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let by = amount(&argv[2])?;
    let negated = by
        .checked_neg()
        .ok_or_else(|| CommandError::other("ERR decrement would overflow"))?;
    increment(context, &argv[1], negated)
}

pub(super) fn incrbyfloat(
    context: &mut Context<'_>,
    argv: &[Bytes],
) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let current = match state.store.get(db, &argv[1], now) {
        Some(value) => LongDouble::parse(as_str(value)?),
        None => LongDouble::parse(b"0"),
    };
    let (Some(current), Some(by)) = (current, LongDouble::parse(&argv[2])) else {
        return Err(CommandError::NotFloat);
    };
    let (Parsed::Finite(current), Parsed::Finite(by)) = (current, by) else {
        return Err(infinite_sum());
    };
    let total = current.checked_add(by).ok_or_else(infinite_sum)?;
    let text = Bytes::from(total.to_human_string());
    state.store.put(
        db,
        argv[1].clone(),
        Value::Str(text.clone()),
        Ttl::Keep,
        now,
    );
    Ok(Frame::bulk(text))
}

fn infinite_sum() -> CommandError {
    CommandError::other("ERR increment would produce NaN or Infinity")
}

pub(super) fn append(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let Some(value) = state.store.get_mut(db, &argv[1], now) else {
        state
            .store
            .set(db, argv[1].clone(), Value::Str(argv[2].clone()));
        return Ok(Frame::Integer(len_frame(argv[2].len())));
    };
    let Value::Str(data) = value else {
        return Err(CommandError::WrongType);
    };
    if data.len() + argv[2].len() > MAX_STRING_LEN {
        return Err(too_long());
    }
    edit(data, |buffer| buffer.extend_from_slice(&argv[2]));
    Ok(Frame::Integer(len_frame(data.len())))
}

fn len_frame(len: usize) -> i64 {
    i64::try_from(len).unwrap_or(i64::MAX)
}

pub(super) fn strlen(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    match state.store.get(session.db, &argv[1], *now_ms) {
        Some(value) => Ok(Frame::Integer(len_frame(as_str(value)?.len()))),
        None => Ok(Frame::Integer(0)),
    }
}

pub(super) fn getrange(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let (start, end) = (amount(&argv[2])?, amount(&argv[3])?);
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let Some(value) = state.store.get(session.db, &argv[1], *now_ms) else {
        return Ok(Frame::bulk(""));
    };
    let data = as_str(value)?;
    let len = len_frame(data.len());
    if (start < 0 && end < 0 && start > end) || len == 0 {
        return Ok(Frame::bulk(""));
    }
    let start = if start < 0 { len + start } else { start }.max(0);
    let end = if end < 0 { len + end } else { end }.max(0).min(len - 1);
    if start > end {
        return Ok(Frame::bulk(""));
    }
    let (start, end) = (
        usize::try_from(start).unwrap_or(0),
        usize::try_from(end).unwrap_or(0),
    );
    Ok(Frame::bulk(data.slice(start..=end)))
}

pub(super) fn setrange(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let offset = amount(&argv[2])?;
    let offset =
        usize::try_from(offset).map_err(|_| CommandError::other("ERR offset is out of range"))?;
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let piece = &argv[3];
    let existing = state
        .store
        .get(db, &argv[1], now)
        .map(as_str)
        .transpose()?
        .map(Bytes::len);
    if piece.is_empty() {
        return Ok(Frame::Integer(len_frame(existing.unwrap_or(0))));
    }
    if offset
        .checked_add(piece.len())
        .is_none_or(|total| total > MAX_STRING_LEN)
    {
        return Err(too_long());
    }
    let change = |buffer: &mut BytesMut| {
        if buffer.len() < offset + piece.len() {
            buffer.resize(offset + piece.len(), 0);
        }
        buffer[offset..offset + piece.len()].copy_from_slice(piece);
    };
    if let Some(value) = state.store.get_mut(db, &argv[1], now) {
        let Value::Str(data) = value else {
            return Err(CommandError::WrongType);
        };
        edit(data, change);
        return Ok(Frame::Integer(len_frame(data.len())));
    }
    let mut buffer = BytesMut::new();
    change(&mut buffer);
    let total = buffer.len();
    state
        .store
        .set(db, argv[1].clone(), Value::Str(buffer.freeze()));
    Ok(Frame::Integer(len_frame(total)))
}

pub(super) fn mget(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let values = argv[1..]
        .iter()
        .map(|key| match state.store.get(session.db, key, *now_ms) {
            Some(Value::Str(data)) => Frame::bulk(data.clone()),
            _ => Frame::Null,
        })
        .collect();
    Ok(Frame::Array(values))
}

fn mset_generic(
    context: &mut Context<'_>,
    argv: &[Bytes],
    name: &str,
    only_new: bool,
) -> Result<Frame, CommandError> {
    if argv.len().is_multiple_of(2) {
        return Err(CommandError::WrongArity(name.to_owned()));
    }
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let (pairs, _) = argv[1..].as_chunks::<2>();
    if only_new
        && pairs
            .iter()
            .any(|[key, _]| state.store.get(db, key, now).is_some())
    {
        return Ok(Frame::Integer(0));
    }
    for [key, value] in pairs {
        state.store.set(db, key.clone(), Value::Str(value.clone()));
    }
    Ok(if only_new {
        Frame::Integer(1)
    } else {
        Frame::ok()
    })
}

pub(super) fn mset(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    mset_generic(context, argv, "mset", false)
}

pub(super) fn msetnx(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    mset_generic(context, argv, "msetnx", true)
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
    fn counters() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["INCR", "n"]), Frame::Integer(1));
        assert_eq!(client.run(&["INCRBY", "n", "9"]), Frame::Integer(10));
        assert_eq!(client.run(&["DECR", "n"]), Frame::Integer(9));
        assert_eq!(client.run(&["DECRBY", "n", "20"]), Frame::Integer(-11));
        assert_eq!(client.run(&["GET", "n"]), Frame::bulk("-11"));
        assert_eq!(client.run(&["DECRBY", "fresh", "5"]), Frame::Integer(-5));
    }

    #[test]
    fn counters_keep_the_expiry() {
        let mut client = Harness::new();
        let now = client.now_ms;
        client.run(&["SET", "n", "1", "PX", "100"]);
        client.run(&["INCR", "n"]);
        client.now_ms = now + 100;
        assert_eq!(client.run(&["GET", "n"]), Frame::Null);
    }

    #[test]
    fn counter_errors() {
        let mut client = Harness::new();
        let not_int = error("ERR value is not an integer or out of range");
        client.run(&["SET", "s", "text"]);
        client.run(&["SET", "f", "1.5"]);
        client.run(&["SET", "max", "9223372036854775807"]);
        assert_eq!(client.run(&["INCR", "s"]), not_int);
        assert_eq!(client.run(&["INCR", "f"]), not_int);
        assert_eq!(client.run(&["INCRBY", "x", "1.5"]), not_int);
        assert_eq!(
            client.run(&["INCR", "max"]),
            error("ERR increment or decrement would overflow")
        );
        assert_eq!(
            client.run(&["DECRBY", "x", "-9223372036854775808"]),
            error("ERR decrement would overflow")
        );
        assert_eq!(
            client.run(&["GET", "max"]),
            Frame::bulk("9223372036854775807")
        );
    }

    #[test]
    fn incrbyfloat_sums_like_redis() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["INCRBYFLOAT", "f", "0.1"]), Frame::bulk("0.1"));
        assert_eq!(client.run(&["INCRBYFLOAT", "f", "0.2"]), Frame::bulk("0.3"));
        assert_eq!(client.run(&["INCRBYFLOAT", "f", "-0.3"]), Frame::bulk("0"));
        client.run(&["SET", "g", "5.0e3"]);
        assert_eq!(
            client.run(&["INCRBYFLOAT", "g", "2.0e2"]),
            Frame::bulk("5200")
        );
        assert_eq!(client.run(&["GET", "g"]), Frame::bulk("5200"));
    }

    #[test]
    fn incrbyfloat_errors() {
        let mut client = Harness::new();
        let not_float = error("ERR value is not a valid float");
        client.run(&["SET", "s", "text"]);
        assert_eq!(client.run(&["INCRBYFLOAT", "s", "1"]), not_float);
        assert_eq!(client.run(&["INCRBYFLOAT", "n", "abc"]), not_float);
        assert_eq!(client.run(&["INCRBYFLOAT", "n", "nan"]), not_float);
        let infinite = error("ERR increment would produce NaN or Infinity");
        assert_eq!(client.run(&["INCRBYFLOAT", "n", "inf"]), infinite);
        assert_eq!(client.run(&["INCRBYFLOAT", "n", "-inf"]), infinite);
    }

    #[test]
    fn append_and_strlen() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["STRLEN", "k"]), Frame::Integer(0));
        assert_eq!(client.run(&["APPEND", "k", "ab"]), Frame::Integer(2));
        assert_eq!(client.run(&["APPEND", "k", "cd"]), Frame::Integer(4));
        assert_eq!(client.run(&["APPEND", "k", ""]), Frame::Integer(4));
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk("abcd"));
        assert_eq!(client.run(&["STRLEN", "k"]), Frame::Integer(4));
    }

    #[test]
    fn append_keeps_the_expiry() {
        let mut client = Harness::new();
        let now = client.now_ms;
        client.run(&["SET", "k", "a", "PX", "100"]);
        client.run(&["APPEND", "k", "b"]);
        client.now_ms = now + 100;
        assert_eq!(client.run(&["GET", "k"]), Frame::Null);
    }

    #[test]
    fn getrange_follows_redis_index_rules() {
        let mut client = Harness::new();
        client.run(&["SET", "k", "Hello World"]);
        let cases = [
            ("0", "4", "Hello"),
            ("-5", "-1", "World"),
            ("0", "-1", "Hello World"),
            ("5", "3", ""),
            ("-1", "-5", ""),
            ("-100", "2", "Hel"),
            ("6", "100", "World"),
            ("100", "200", ""),
            ("-100", "-50", "H"),
        ];
        for (start, end, expected) in cases {
            assert_eq!(
                client.run(&["GETRANGE", "k", start, end]),
                Frame::bulk(expected),
                "{start} {end}"
            );
        }
        assert_eq!(
            client.run(&["GETRANGE", "missing", "0", "1"]),
            Frame::bulk("")
        );
        assert_eq!(
            client.run(&["GETRANGE", "k", "a", "1"]),
            error("ERR value is not an integer or out of range")
        );
    }

    #[test]
    fn setrange_pads_with_zero_bytes() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["SETRANGE", "k", "3", "ab"]), Frame::Integer(5));
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk(&b"\0\0\0ab"[..]));
        assert_eq!(client.run(&["SETRANGE", "k", "1", "XY"]), Frame::Integer(5));
        assert_eq!(client.run(&["GET", "k"]), Frame::bulk(&b" XYab"[..]));
        assert_eq!(
            client.run(&["SETRANGE", "none", "5", ""]),
            Frame::Integer(0)
        );
        assert_eq!(client.run(&["GET", "none"]), Frame::Null);
    }

    #[test]
    fn setrange_limits() {
        let mut client = Harness::new();
        assert_eq!(
            client.run(&["SETRANGE", "k", "-1", "a"]),
            error("ERR offset is out of range")
        );
        assert_eq!(
            client.run(&["SETRANGE", "k", "536870912", "a"]),
            error("ERR string exceeds maximum allowed size (proto-max-bulk-len)")
        );
    }

    #[test]
    fn mget_mset_msetnx() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["MSET", "a", "1", "b", "2"]), Frame::ok());
        assert_eq!(
            client.run(&["MGET", "a", "x", "b"]),
            Frame::Array(vec![Frame::bulk("1"), Frame::Null, Frame::bulk("2")])
        );
        assert_eq!(
            client.run(&["MSETNX", "c", "3", "a", "9"]),
            Frame::Integer(0)
        );
        assert_eq!(client.run(&["GET", "c"]), Frame::Null);
        assert_eq!(
            client.run(&["MSETNX", "c", "3", "d", "4"]),
            Frame::Integer(1)
        );
        assert_eq!(
            client.run(&["MSET", "a", "1", "b"]),
            error("ERR wrong number of arguments for 'mset' command")
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
