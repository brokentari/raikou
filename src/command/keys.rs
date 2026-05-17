//! Key commands.

use bytes::Bytes;

use crate::command::Context;
use crate::error::CommandError;
use crate::resp::Frame;
use crate::store::{Expiry, Ttl};
use crate::value::Value;
use crate::{glob, number};

const NX: u8 = 1;
const XX: u8 = 2;
const GT: u8 = 4;
const LT: u8 = 8;

fn expire_flags(options: &[Bytes]) -> Result<u8, CommandError> {
    let mut flags = 0;
    for option in options {
        flags |= match option.to_ascii_lowercase().as_slice() {
            b"nx" => NX,
            b"xx" => XX,
            b"gt" => GT,
            b"lt" => LT,
            _ => {
                let mut text = bytes::BytesMut::from("ERR Unsupported option ");
                bytes::BufMut::put_slice(&mut text, option);
                return Err(CommandError::other(text.freeze()));
            }
        };
    }
    if flags & NX != 0 && flags & (XX | GT | LT) != 0 {
        return Err(CommandError::other(
            "ERR NX and XX, GT or LT options at the same time are not compatible",
        ));
    }
    if flags & GT != 0 && flags & LT != 0 {
        return Err(CommandError::other(
            "ERR GT and LT options at the same time are not compatible",
        ));
    }
    Ok(flags)
}

fn expire_generic(
    context: &mut Context<'_>,
    argv: &[Bytes],
    name: &'static str,
    seconds: bool,
    relative: bool,
) -> Result<Frame, CommandError> {
    let flags = expire_flags(&argv[3..])?;
    let mut when = number::parse_i64(&argv[2]).ok_or(CommandError::NotInteger)?;
    let invalid = CommandError::InvalidExpire(name);
    if seconds {
        when = when.checked_mul(1000).ok_or(invalid.clone())?;
    }
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let now_signed = i64::try_from(now).unwrap_or(i64::MAX);
    if relative {
        when = when.checked_add(now_signed).ok_or(invalid)?;
    }
    let current = match state.store.expiry(db, &argv[1], now) {
        Expiry::Missing => return Ok(Frame::Integer(0)),
        Expiry::Never => None,
        Expiry::At(at) => Some(i64::try_from(at).unwrap_or(i64::MAX)),
    };
    let refused = (flags & NX != 0 && current.is_some())
        || (flags & XX != 0 && current.is_none())
        || (flags & GT != 0 && current.is_none_or(|at| when <= at))
        || (flags & LT != 0 && current.is_some_and(|at| when >= at));
    if refused {
        return Ok(Frame::Integer(0));
    }
    match u64::try_from(when).ok().filter(|at| *at > now) {
        Some(at) => state.store.set_deadline(db, &argv[1], Some(at), now),
        None => state.store.remove(db, &argv[1], now),
    };
    Ok(Frame::Integer(1))
}

pub(super) fn expire(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    expire_generic(context, argv, "expire", true, true)
}

pub(super) fn pexpire(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    expire_generic(context, argv, "pexpire", false, true)
}

pub(super) fn expireat(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    expire_generic(context, argv, "expireat", true, false)
}

pub(super) fn pexpireat(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    expire_generic(context, argv, "pexpireat", false, false)
}

fn ttl_generic(context: &mut Context<'_>, argv: &[Bytes], millis: bool, absolute: bool) -> Frame {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let at = match state.store.expiry(session.db, &argv[1], *now_ms) {
        Expiry::Missing => return Frame::Integer(-2),
        Expiry::Never => return Frame::Integer(-1),
        Expiry::At(at) => i64::try_from(at).unwrap_or(i64::MAX),
    };
    let now = i64::try_from(*now_ms).unwrap_or(i64::MAX);
    let value = if absolute { at } else { (at - now).max(0) };
    Frame::Integer(if millis { value } else { (value + 500) / 1000 })
}

pub(super) fn ttl(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    Ok(ttl_generic(context, argv, false, false))
}

pub(super) fn pttl(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    Ok(ttl_generic(context, argv, true, false))
}

pub(super) fn expiretime(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    Ok(ttl_generic(context, argv, false, true))
}

pub(super) fn pexpiretime(
    context: &mut Context<'_>,
    argv: &[Bytes],
) -> Result<Frame, CommandError> {
    Ok(ttl_generic(context, argv, true, true))
}

pub(super) fn persist(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let changed = state
        .store
        .set_deadline(session.db, &argv[1], None, *now_ms);
    Ok(Frame::Integer(i64::from(changed)))
}

pub(super) fn keys(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    // shortcut: KEYS walks the whole database under the lock, as Redis does
    let all = argv[1].as_ref() == b"*";
    let matching = state
        .store
        .keys(session.db, *now_ms)
        .into_iter()
        .filter(|key| all || glob::matches(&argv[1], key))
        .map(Frame::bulk)
        .collect();
    Ok(Frame::Array(matching))
}

pub(super) fn randomkey(context: &mut Context<'_>, _: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    Ok(state
        .store
        .random_key(session.db, *now_ms)
        .map_or(Frame::Null, Frame::bulk))
}

pub(super) fn scan(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let cursor = std::str::from_utf8(&argv[1])
        .ok()
        .filter(|text| text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse::<u64>().ok())
        .ok_or_else(|| CommandError::other("ERR invalid cursor"))?;
    let mut count = 10;
    let mut pattern: Option<&Bytes> = None;
    let mut type_name: Option<String> = None;
    let mut index = 2;
    while index < argv.len() {
        let option = argv[index].to_ascii_lowercase();
        let value = argv.get(index + 1);
        match (option.as_slice(), value) {
            (b"count", Some(value)) => {
                count = number::parse_i64(value).ok_or(CommandError::NotInteger)?;
                if count < 1 {
                    return Err(CommandError::Syntax);
                }
            }
            (b"match", Some(value)) => pattern = Some(value).filter(|value| value.as_ref() != b"*"),
            (b"type", Some(value)) => {
                type_name = Some(String::from_utf8_lossy(value).to_ascii_lowercase());
            }
            _ => return Err(CommandError::Syntax),
        }
        index += 2;
    }
    let Context {
        state,
        session,
        now_ms,
    } = context;
    let (db, now) = (session.db, *now_ms);
    let count = usize::try_from(count).unwrap_or(usize::MAX);
    let (next, candidates) = state.store.scan(db, cursor, count, now);
    let keys = candidates
        .into_iter()
        .filter(|key| pattern.is_none_or(|pattern| glob::matches(pattern, key)))
        .filter(|key| {
            type_name.as_deref().is_none_or(|wanted| {
                state
                    .store
                    .get(db, key, now)
                    .is_some_and(|value| value.type_name() == wanted)
            })
        })
        .map(Frame::bulk)
        .collect();
    Ok(Frame::Array(vec![
        Frame::bulk(next.to_string()),
        Frame::Array(keys),
    ]))
}

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
    fn expire_sets_a_deadline() {
        let mut client = Harness::new();
        let now = client.now_ms;
        assert_eq!(client.run(&["EXPIRE", "k", "10"]), Frame::Integer(0));
        client.run(&["SET", "k", "v"]);
        assert_eq!(client.run(&["TTL", "k"]), Frame::Integer(-1));
        assert_eq!(client.run(&["EXPIRE", "k", "10"]), Frame::Integer(1));
        assert_eq!(client.run(&["TTL", "k"]), Frame::Integer(10));
        assert_eq!(client.run(&["PTTL", "k"]), Frame::Integer(10_000));
        client.now_ms = now + 1_600;
        assert_eq!(client.run(&["TTL", "k"]), Frame::Integer(8));
        assert_eq!(client.run(&["PERSIST", "k"]), Frame::Integer(1));
        assert_eq!(client.run(&["PERSIST", "k"]), Frame::Integer(0));
        assert_eq!(client.run(&["TTL", "k"]), Frame::Integer(-1));
        assert_eq!(client.run(&["TTL", "missing"]), Frame::Integer(-2));
        assert_eq!(client.run(&["PERSIST", "missing"]), Frame::Integer(0));
    }

    #[test]
    fn absolute_times_and_past_deadlines() {
        let mut client = Harness::new();
        client.run(&["SET", "k", "v"]);
        assert_eq!(
            client.run(&["PEXPIREAT", "k", "4102444800123"]),
            Frame::Integer(1)
        );
        assert_eq!(
            client.run(&["PEXPIRETIME", "k"]),
            Frame::Integer(4_102_444_800_123)
        );
        assert_eq!(
            client.run(&["EXPIRETIME", "k"]),
            Frame::Integer(4_102_444_800)
        );
        assert_eq!(client.run(&["EXPIREAT", "k", "1"]), Frame::Integer(1));
        assert_eq!(client.run(&["EXISTS", "k"]), Frame::Integer(0));
        client.run(&["SET", "k", "v"]);
        assert_eq!(client.run(&["EXPIRE", "k", "-1"]), Frame::Integer(1));
        assert_eq!(client.run(&["GET", "k"]), Frame::Null);
    }

    #[test]
    fn expire_flags_gate_the_change() {
        let mut client = Harness::new();
        client.run(&["SET", "k", "v"]);
        assert_eq!(client.run(&["EXPIRE", "k", "100", "XX"]), Frame::Integer(0));
        assert_eq!(client.run(&["EXPIRE", "k", "100", "GT"]), Frame::Integer(0));
        assert_eq!(client.run(&["EXPIRE", "k", "100", "NX"]), Frame::Integer(1));
        assert_eq!(client.run(&["EXPIRE", "k", "200", "NX"]), Frame::Integer(0));
        assert_eq!(client.run(&["EXPIRE", "k", "200", "GT"]), Frame::Integer(1));
        assert_eq!(client.run(&["EXPIRE", "k", "300", "LT"]), Frame::Integer(0));
        assert_eq!(
            client.run(&["EXPIRE", "k", "50", "lt", "xx"]),
            Frame::Integer(1)
        );
        assert_eq!(client.run(&["TTL", "k"]), Frame::Integer(50));
    }

    #[test]
    fn expire_errors() {
        let mut client = Harness::new();
        client.run(&["SET", "k", "v"]);
        assert_eq!(
            client.run(&["EXPIRE", "k", "x"]),
            error("ERR value is not an integer or out of range")
        );
        assert_eq!(
            client.run(&["EXPIRE", "k", "10", "ZZ"]),
            error("ERR Unsupported option ZZ")
        );
        assert_eq!(
            client.run(&["EXPIRE", "k", "10", "NX", "XX"]),
            error("ERR NX and XX, GT or LT options at the same time are not compatible")
        );
        assert_eq!(
            client.run(&["EXPIRE", "k", "10", "GT", "LT"]),
            error("ERR GT and LT options at the same time are not compatible")
        );
        assert_eq!(
            client.run(&["EXPIRE", "k", "9223372036854775807"]),
            error("ERR invalid expire time in 'expire' command")
        );
        assert_eq!(
            client.run(&["PEXPIRE", "k", "9223372036854775807"]),
            error("ERR invalid expire time in 'pexpire' command")
        );
    }

    #[test]
    fn keys_filters_with_a_glob() {
        let mut client = Harness::new();
        client.run(&["MSET", "user:1", "a", "user:2", "b", "other", "c"]);
        let Frame::Array(mut found) = client.run(&["KEYS", "user:*"]) else {
            panic!("array")
        };
        found.sort_by_key(|frame| format!("{frame:?}"));
        assert_eq!(found, vec![Frame::bulk("user:1"), Frame::bulk("user:2")]);
        let Frame::Array(all) = client.run(&["KEYS", "*"]) else {
            panic!("array")
        };
        assert_eq!(all.len(), 3);
        assert_eq!(client.run(&["KEYS", "nope*"]), Frame::Array(Vec::new()));
    }

    #[test]
    fn scan_walks_the_keyspace() {
        let mut client = Harness::new();
        for index in 0..25 {
            client.run(&["SET", &format!("k{index}"), "v"]);
        }
        let mut seen = 0;
        let mut cursor = "0".to_owned();
        loop {
            let Frame::Array(reply) = client.run(&["SCAN", &cursor, "COUNT", "4", "MATCH", "k*"])
            else {
                panic!("array")
            };
            let (Frame::BulkString(next), Frame::Array(keys)) = (&reply[0], &reply[1]) else {
                panic!("shape")
            };
            seen += keys.len();
            cursor = String::from_utf8(next.to_vec()).expect("digits");
            if cursor == "0" {
                break;
            }
        }
        assert_eq!(seen, 25);
    }

    #[test]
    fn scan_errors_and_type_filter() {
        let mut client = Harness::new();
        client.run(&["SET", "a", "1"]);
        assert_eq!(client.run(&["SCAN", "x"]), error("ERR invalid cursor"));
        assert_eq!(
            client.run(&["SCAN", "0", "COUNT", "0"]),
            error("ERR syntax error")
        );
        assert_eq!(
            client.run(&["SCAN", "0", "COUNT"]),
            error("ERR syntax error")
        );
        assert_eq!(
            client.run(&["SCAN", "0", "FOO", "1"]),
            error("ERR syntax error")
        );
        let nothing = Frame::Array(vec![Frame::bulk("0"), Frame::Array(Vec::new())]);
        assert_eq!(client.run(&["SCAN", "0", "TYPE", "nope"]), nothing);
        let reply = client.run(&["SCAN", "0", "TYPE", "list"]);
        assert_eq!(
            reply,
            Frame::Array(vec![Frame::bulk("0"), Frame::Array(Vec::new())])
        );
        let reply = client.run(&["SCAN", "0", "TYPE", "STRING"]);
        assert_eq!(
            reply,
            Frame::Array(vec![Frame::bulk("0"), Frame::Array(vec![Frame::bulk("a")])])
        );
    }

    #[test]
    fn randomkey_picks_an_existing_key() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["RANDOMKEY"]), Frame::Null);
        client.run(&["SET", "only", "1"]);
        assert_eq!(client.run(&["RANDOMKEY"]), Frame::bulk("only"));
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
