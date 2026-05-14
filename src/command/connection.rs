//! Connection commands: `PING`, `ECHO` and `HELLO`.

use bytes::{BufMut, Bytes, BytesMut};

use crate::command::{Context, REDIS_VERSION};
use crate::error::CommandError;
use crate::number;
use crate::resp::{Frame, Protocol};

pub(super) fn ping(_: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    match argv {
        [_] => Ok(Frame::simple("PONG")),
        [_, message] => Ok(Frame::bulk(message.clone())),
        _ => Err(CommandError::WrongArity("ping".to_owned())),
    }
}

pub(super) fn echo(_: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    Ok(Frame::bulk(argv[1].clone()))
}

pub(super) fn hello(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let mut rest = &argv[1..];
    let mut protocol = None;
    if let Some((version, tail)) = rest.split_first() {
        let version = number::parse_i64(version).ok_or_else(|| {
            CommandError::other("ERR Protocol version is not an integer or out of range")
        })?;
        protocol = Some(match version {
            2 => Protocol::Resp2,
            3 => Protocol::Resp3,
            _ => return Err(CommandError::NoProto),
        });
        rest = tail;
    }
    let mut credentials = None;
    let mut name = None;
    while let Some(option) = rest.first() {
        match (option.to_ascii_uppercase().as_slice(), &rest[1..]) {
            (b"AUTH", [user, password, ..]) => {
                credentials = Some((user, password));
                rest = &rest[3..];
            }
            (b"SETNAME", [value, ..]) => {
                name = Some(value);
                rest = &rest[2..];
            }
            _ => return Err(hello_syntax_error(option)),
        }
    }
    if let Some((user, password)) = credentials {
        authenticate(user, password)?;
    }
    if let Some(name) = name {
        validate_client_name(name)?;
        context.session.name = if name.is_empty() {
            None
        } else {
            Some(name.clone())
        };
    }
    if let Some(protocol) = protocol {
        context.session.protocol = protocol;
    }
    Ok(hello_reply(context))
}

fn hello_syntax_error(option: &Bytes) -> CommandError {
    let mut text = BytesMut::from("ERR Syntax error in HELLO option '");
    text.put_slice(option);
    text.put_u8(b'\'');
    CommandError::other(text.freeze())
}

fn hello_reply(context: &Context<'_>) -> Frame {
    let proto = match context.session.protocol {
        Protocol::Resp2 => 2,
        Protocol::Resp3 => 3,
    };
    Frame::Map(vec![
        (Frame::bulk("server"), Frame::bulk("redis")),
        (Frame::bulk("version"), Frame::bulk(REDIS_VERSION)),
        (Frame::bulk("proto"), Frame::Integer(proto)),
        (Frame::bulk("id"), Frame::Integer(context.session.id)),
        (Frame::bulk("mode"), Frame::bulk("standalone")),
        (Frame::bulk("role"), Frame::bulk("master")),
        (Frame::bulk("modules"), Frame::Array(Vec::new())),
    ])
}

fn authenticate(user: &Bytes, _password: &Bytes) -> Result<(), CommandError> {
    // shortcut: the only user is `default`, which has no password, so any password passes
    if user.as_ref() == b"default" {
        Ok(())
    } else {
        Err(CommandError::other(
            "WRONGPASS invalid username-password pair or user is disabled.",
        ))
    }
}

fn validate_client_name(name: &[u8]) -> Result<(), CommandError> {
    if name.iter().all(|byte| (b'!'..=b'~').contains(byte)) {
        Ok(())
    } else {
        Err(CommandError::other(
            "ERR Client names cannot contain spaces, newlines or special characters.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::command::dispatch;
    use crate::session::Session;

    use super::*;

    fn run(session: &mut Session, parts: &[&str]) -> Frame {
        let argv: Vec<Bytes> = parts
            .iter()
            .map(|part| Bytes::copy_from_slice(part.as_bytes()))
            .collect();
        dispatch(session, &argv)
    }

    fn error(text: &str) -> Frame {
        Frame::Error(Bytes::copy_from_slice(text.as_bytes()))
    }

    fn field<'a>(reply: &'a Frame, key: &str) -> &'a Frame {
        let Frame::Map(pairs) = reply else {
            panic!("not a map: {reply:?}")
        };
        let wanted = Frame::bulk(Bytes::copy_from_slice(key.as_bytes()));
        &pairs
            .iter()
            .find(|(name, _)| *name == wanted)
            .expect("field present")
            .1
    }

    #[test]
    fn hello_reports_server_facts() {
        let mut session = Session::new(7);
        let reply = run(&mut session, &["HELLO"]);
        assert_eq!(field(&reply, "server"), &Frame::bulk("redis"));
        assert_eq!(field(&reply, "version"), &Frame::bulk(REDIS_VERSION));
        assert_eq!(field(&reply, "proto"), &Frame::Integer(2));
        assert_eq!(field(&reply, "id"), &Frame::Integer(7));
        assert_eq!(field(&reply, "mode"), &Frame::bulk("standalone"));
        assert_eq!(field(&reply, "role"), &Frame::bulk("master"));
        assert_eq!(field(&reply, "modules"), &Frame::Array(Vec::new()));
    }

    #[test]
    fn hello_switches_protocol() {
        let mut session = Session::new(1);
        let reply = run(&mut session, &["HELLO", "3"]);
        assert_eq!(field(&reply, "proto"), &Frame::Integer(3));
        assert_eq!(session.protocol, Protocol::Resp3);
        run(&mut session, &["hello", "2"]);
        assert_eq!(session.protocol, Protocol::Resp2);
    }

    #[test]
    fn hello_sets_and_clears_the_client_name() {
        let mut session = Session::new(1);
        run(&mut session, &["HELLO", "3", "SETNAME", "worker"]);
        assert_eq!(session.name, Some(Bytes::from("worker")));
        run(&mut session, &["HELLO", "3", "SETNAME", ""]);
        assert_eq!(session.name, None);
    }

    #[test]
    fn hello_rejects_bad_input_without_changing_the_session() {
        let mut session = Session::new(1);
        let cases = [
            (
                vec!["HELLO", "x"],
                "ERR Protocol version is not an integer or out of range",
            ),
            (vec!["HELLO", "4"], "NOPROTO unsupported protocol version"),
            (
                vec!["HELLO", "3", "foo"],
                "ERR Syntax error in HELLO option 'foo'",
            ),
            (
                vec!["HELLO", "3", "AUTH", "a"],
                "ERR Syntax error in HELLO option 'AUTH'",
            ),
            (
                vec!["HELLO", "3", "SETNAME"],
                "ERR Syntax error in HELLO option 'SETNAME'",
            ),
            (
                vec!["HELLO", "3", "AUTH", "a", "b"],
                "WRONGPASS invalid username-password pair or user is disabled.",
            ),
            (
                vec!["HELLO", "3", "SETNAME", "a b"],
                "ERR Client names cannot contain spaces, newlines or special characters.",
            ),
        ];
        for (parts, text) in cases {
            assert_eq!(run(&mut session, &parts), error(text), "{parts:?}");
            assert_eq!(session.protocol, Protocol::Resp2);
            assert_eq!(session.name, None);
        }
    }

    #[test]
    fn hello_accepts_the_default_user() {
        let mut session = Session::new(1);
        let reply = run(&mut session, &["HELLO", "3", "AUTH", "default", "anything"]);
        assert_eq!(field(&reply, "proto"), &Frame::Integer(3));
    }

    #[test]
    fn echo_and_ping_reply() {
        let mut session = Session::new(1);
        assert_eq!(run(&mut session, &["PING"]), Frame::simple("PONG"));
        assert_eq!(run(&mut session, &["PING", "x"]), Frame::bulk("x"));
        assert_eq!(run(&mut session, &["ECHO", "x"]), Frame::bulk("x"));
    }
}
