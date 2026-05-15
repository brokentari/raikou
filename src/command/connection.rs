//! Connection commands: `PING`, `ECHO`, `HELLO`, `CLIENT`, `SELECT`, `QUIT` and `RESET`.

use bytes::{BufMut, Bytes, BytesMut};

use crate::command::{Context, REDIS_VERSION};
use crate::error::CommandError;
use crate::number;
use crate::resp::{Frame, Protocol};
use crate::session::DATABASES;

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

pub(super) fn auth(_: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    match argv {
        [_, _] => Err(CommandError::other(
            "ERR AUTH <password> called without any password configured for the default user. \
             Are you sure your configuration is correct?",
        )),
        [_, user, password] => authenticate(user, password).map(|()| Frame::ok()),
        _ => Err(CommandError::Syntax),
    }
}

pub(super) fn client_id(context: &mut Context<'_>, _: &[Bytes]) -> Result<Frame, CommandError> {
    Ok(Frame::Integer(context.session.id))
}

pub(super) fn client_getname(
    context: &mut Context<'_>,
    _: &[Bytes],
) -> Result<Frame, CommandError> {
    Ok(context
        .session
        .name
        .clone()
        .map_or(Frame::Null, Frame::bulk))
}

pub(super) fn client_setname(
    context: &mut Context<'_>,
    argv: &[Bytes],
) -> Result<Frame, CommandError> {
    validate_client_name(&argv[2])?;
    context.session.name = if argv[2].is_empty() {
        None
    } else {
        Some(argv[2].clone())
    };
    Ok(Frame::ok())
}

pub(super) fn client_setinfo(
    context: &mut Context<'_>,
    argv: &[Bytes],
) -> Result<Frame, CommandError> {
    let attribute = argv[2].to_ascii_lowercase();
    if attribute != b"lib-name" && attribute != b"lib-ver" {
        let mut text = BytesMut::from("ERR Unrecognized option '");
        text.put_slice(&argv[2]);
        text.put_u8(b'\'');
        return Err(CommandError::other(text.freeze()));
    }
    if !is_plain_text(&argv[3]) {
        let shown = String::from_utf8_lossy(&argv[2]);
        return Err(CommandError::other(format!(
            "ERR {shown} cannot contain spaces, newlines or special characters."
        )));
    }
    if attribute == b"lib-name" {
        context.session.lib_name = argv[3].clone();
    } else {
        context.session.lib_ver = argv[3].clone();
    }
    Ok(Frame::ok())
}

pub(super) fn client_info(context: &mut Context<'_>, _: &[Bytes]) -> Result<Frame, CommandError> {
    let session = &*context.session;
    let name = session
        .name
        .as_deref()
        .map(String::from_utf8_lossy)
        .unwrap_or_default();
    let resp = if session.protocol == Protocol::Resp3 {
        3
    } else {
        2
    };
    // shortcut: the memory, buffer and traffic counters of Redis are not tracked
    let line = format!(
        "id={} addr={} laddr={} fd=-1 name={name} age=0 idle=0 flags=N db={} sub=0 psub=0 \
         ssub=0 multi=-1 watch=0 cmd=client|info user=default redir=-1 resp={resp} \
         lib-name={} lib-ver={}\n",
        session.id,
        session.peer,
        session.local,
        session.db,
        String::from_utf8_lossy(&session.lib_name),
        String::from_utf8_lossy(&session.lib_ver),
    );
    Ok(Frame::VerbatimString {
        format: *b"txt",
        data: Bytes::from(line),
    })
}

pub(super) fn select(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let index = number::parse_i64(&argv[1]).ok_or(CommandError::NotInteger)?;
    if i32::try_from(index).is_err() {
        return Err(CommandError::other(
            "ERR value is out of range, value must between -2147483648 and 2147483647",
        ));
    }
    let index = usize::try_from(index)
        .ok()
        .filter(|index| *index < DATABASES)
        .ok_or_else(|| CommandError::other("ERR DB index is out of range"))?;
    context.session.db = index;
    Ok(Frame::ok())
}

pub(super) fn quit(context: &mut Context<'_>, _: &[Bytes]) -> Result<Frame, CommandError> {
    context.session.closing = true;
    Ok(Frame::ok())
}

pub(super) fn reset(context: &mut Context<'_>, _: &[Bytes]) -> Result<Frame, CommandError> {
    context.session.reset();
    Ok(Frame::simple("RESET"))
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

fn is_plain_text(text: &[u8]) -> bool {
    text.iter().all(|byte| (b'!'..=b'~').contains(byte))
}

fn validate_client_name(name: &[u8]) -> Result<(), CommandError> {
    if is_plain_text(name) {
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
        let mut session = Session::new(7, String::new(), String::new());
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
        let mut session = Session::new(1, String::new(), String::new());
        let reply = run(&mut session, &["HELLO", "3"]);
        assert_eq!(field(&reply, "proto"), &Frame::Integer(3));
        assert_eq!(session.protocol, Protocol::Resp3);
        run(&mut session, &["hello", "2"]);
        assert_eq!(session.protocol, Protocol::Resp2);
    }

    #[test]
    fn hello_sets_and_clears_the_client_name() {
        let mut session = Session::new(1, String::new(), String::new());
        run(&mut session, &["HELLO", "3", "SETNAME", "worker"]);
        assert_eq!(session.name, Some(Bytes::from("worker")));
        run(&mut session, &["HELLO", "3", "SETNAME", ""]);
        assert_eq!(session.name, None);
    }

    #[test]
    fn hello_rejects_bad_input_without_changing_the_session() {
        let mut session = Session::new(1, String::new(), String::new());
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
        let mut session = Session::new(1, String::new(), String::new());
        let reply = run(&mut session, &["HELLO", "3", "AUTH", "default", "anything"]);
        assert_eq!(field(&reply, "proto"), &Frame::Integer(3));
    }

    #[test]
    fn client_name_round_trip() {
        let mut session = Session::new(9, String::new(), String::new());
        assert_eq!(run(&mut session, &["CLIENT", "GETNAME"]), Frame::Null);
        assert_eq!(
            run(&mut session, &["CLIENT", "SETNAME", "app"]),
            Frame::ok()
        );
        assert_eq!(
            run(&mut session, &["CLIENT", "GETNAME"]),
            Frame::bulk("app")
        );
        assert_eq!(run(&mut session, &["CLIENT", "SETNAME", ""]), Frame::ok());
        assert_eq!(run(&mut session, &["CLIENT", "GETNAME"]), Frame::Null);
        assert_eq!(run(&mut session, &["CLIENT", "ID"]), Frame::Integer(9));
    }

    #[test]
    fn client_setinfo_stores_library_details() {
        let mut session = Session::new(1, String::new(), String::new());
        assert_eq!(
            run(&mut session, &["CLIENT", "SETINFO", "lib-name", "lib"]),
            Frame::ok()
        );
        assert_eq!(
            run(&mut session, &["CLIENT", "SETINFO", "LIB-VER", "1.0"]),
            Frame::ok()
        );
        let Frame::VerbatimString { data, .. } = run(&mut session, &["CLIENT", "INFO"]) else {
            panic!("CLIENT INFO is a verbatim string");
        };
        let text = String::from_utf8_lossy(&data).into_owned();
        assert!(text.contains("lib-name=lib lib-ver=1.0\n"), "{text}");
        assert_eq!(
            run(&mut session, &["CLIENT", "SETINFO", "foo", "x"]),
            error("ERR Unrecognized option 'foo'")
        );
    }

    #[test]
    fn client_errors_name_the_subcommand() {
        let mut session = Session::new(1, String::new(), String::new());
        assert_eq!(
            run(&mut session, &["CLIENT", "ID", "x"]),
            error("ERR wrong number of arguments for 'client|id' command")
        );
        assert_eq!(
            run(&mut session, &["CLIENT", "FOO"]),
            error("ERR unknown subcommand 'FOO'. Try CLIENT HELP.")
        );
        assert_eq!(
            run(&mut session, &["CLIENT"]),
            error("ERR wrong number of arguments for 'client' command")
        );
    }

    #[test]
    fn select_checks_the_index() {
        let mut session = Session::new(1, String::new(), String::new());
        assert_eq!(run(&mut session, &["SELECT", "15"]), Frame::ok());
        assert_eq!(session.db, 15);
        assert_eq!(
            run(&mut session, &["SELECT", "16"]),
            error("ERR DB index is out of range")
        );
        assert_eq!(
            run(&mut session, &["SELECT", "-1"]),
            error("ERR DB index is out of range")
        );
        assert_eq!(
            run(&mut session, &["SELECT", "x"]),
            error("ERR value is not an integer or out of range")
        );
        assert_eq!(session.db, 15);
    }

    #[test]
    fn quit_marks_the_session_closing() {
        let mut session = Session::new(1, String::new(), String::new());
        assert_eq!(run(&mut session, &["QUIT"]), Frame::ok());
        assert!(session.closing);
    }

    #[test]
    fn reset_restores_the_connection_state() {
        let mut session = Session::new(1, String::new(), String::new());
        run(&mut session, &["HELLO", "3", "SETNAME", "n"]);
        run(&mut session, &["SELECT", "2"]);
        assert_eq!(run(&mut session, &["RESET"]), Frame::simple("RESET"));
        assert_eq!(
            (session.protocol, session.name.clone(), session.db),
            (Protocol::Resp2, None, 0)
        );
    }

    #[test]
    fn auth_follows_a_server_without_a_password() {
        let mut session = Session::new(1, String::new(), String::new());
        assert!(
            matches!(run(&mut session, &["AUTH", "secret"]), Frame::Error(text)
            if text.starts_with(b"ERR AUTH <password> called without any password configured"))
        );
        assert_eq!(run(&mut session, &["AUTH", "default", "x"]), Frame::ok());
        assert_eq!(
            run(&mut session, &["AUTH", "bob", "x"]),
            error("WRONGPASS invalid username-password pair or user is disabled.")
        );
        assert_eq!(
            run(&mut session, &["AUTH", "a", "b", "c"]),
            error("ERR syntax error")
        );
    }

    #[test]
    fn echo_and_ping_reply() {
        let mut session = Session::new(1, String::new(), String::new());
        assert_eq!(run(&mut session, &["PING"]), Frame::simple("PONG"));
        assert_eq!(run(&mut session, &["PING", "x"]), Frame::bulk("x"));
        assert_eq!(run(&mut session, &["ECHO", "x"]), Frame::bulk("x"));
    }
}
