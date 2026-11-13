use bytes::Bytes;

use crate::command::{self, Context, REDIS_VERSION};
use crate::error::CommandError;
use crate::resp::Frame;

const SERVER_SECTIONS: [&[u8]; 4] = [b"server", b"default", b"all", b"everything"];

pub(super) fn info(_: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let wanted = argv.len() == 1
        || argv[1..].iter().any(|section| {
            SERVER_SECTIONS
                .iter()
                .any(|known| section.eq_ignore_ascii_case(known))
        });
    // shortcut: only the Server section exists; uptime and port need server state
    let text = if wanted {
        format!(
            "# Server\r\nredis_version:{REDIS_VERSION}\r\nredis_mode:standalone\r\nos:{}\r\n\
             arch_bits:{}\r\nprocess_id:{}\r\n",
            std::env::consts::OS,
            usize::BITS,
            std::process::id(),
        )
    } else {
        String::new()
    };
    Ok(Frame::VerbatimString {
        format: *b"txt",
        data: Bytes::from(text),
    })
}

fn check_flush_mode(argv: &[Bytes]) -> Result<(), CommandError> {
    match argv {
        [_] => Ok(()),
        [_, mode] if mode.eq_ignore_ascii_case(b"async") || mode.eq_ignore_ascii_case(b"sync") => {
            Ok(())
        }
        _ => Err(CommandError::Syntax),
    }
}

pub(super) fn dbsize(context: &mut Context<'_>, _: &[Bytes]) -> Result<Frame, CommandError> {
    let count = context.state.store.len(context.session.db);
    Ok(Frame::Integer(i64::try_from(count).unwrap_or(i64::MAX)))
}

pub(super) fn flushdb(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    check_flush_mode(argv)?;
    context.state.store.flush_db(context.session.db);
    Ok(Frame::ok())
}

pub(super) fn flushall(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    check_flush_mode(argv)?;
    // shortcut: FLUSHALL holds the lock until every database is empty, as Redis does
    context.state.store.flush_all();
    Ok(Frame::ok())
}

pub(super) fn command(_: &mut Context<'_>, _: &[Bytes]) -> Result<Frame, CommandError> {
    // shortcut: no command metadata, so clients start without hints
    Ok(Frame::Array(Vec::new()))
}

pub(super) fn command_count(_: &mut Context<'_>, _: &[Bytes]) -> Result<Frame, CommandError> {
    let count = i64::try_from(command::count()).unwrap_or(i64::MAX);
    Ok(Frame::Integer(count))
}

pub(super) fn command_docs(_: &mut Context<'_>, _: &[Bytes]) -> Result<Frame, CommandError> {
    Ok(Frame::Map(Vec::new()))
}

pub(super) fn config_get(_: &mut Context<'_>, _: &[Bytes]) -> Result<Frame, CommandError> {
    // shortcut: no setting is exposed, so every pattern matches nothing
    Ok(Frame::Map(Vec::new()))
}

#[cfg(test)]
mod tests {
    use crate::command::harness::Harness;

    use super::*;

    fn run(parts: &[&str]) -> Frame {
        Harness::new().run(parts)
    }

    fn info_text(parts: &[&str]) -> String {
        let Frame::VerbatimString { format, data } = run(parts) else {
            panic!("INFO is a verbatim string");
        };
        assert_eq!(&format, b"txt");
        String::from_utf8(data.to_vec()).expect("text")
    }

    #[test]
    fn info_lists_the_server_section() {
        let text = info_text(&["INFO"]);
        assert!(text.starts_with("# Server\r\n"));
        assert!(text.contains(&format!("redis_version:{REDIS_VERSION}\r\n")));
        assert!(text.contains("redis_mode:standalone\r\n"));
        assert_eq!(info_text(&["info", "SERVER"]), text);
        assert_eq!(info_text(&["INFO", "everything"]), text);
    }

    #[test]
    fn info_for_an_unknown_section_is_empty() {
        assert_eq!(info_text(&["INFO", "nosuch"]), "");
    }

    #[test]
    fn command_replies_are_empty_or_counts() {
        assert_eq!(run(&["COMMAND"]), Frame::Array(Vec::new()));
        assert_eq!(run(&["COMMAND", "DOCS"]), Frame::Map(Vec::new()));
        assert_eq!(
            run(&["COMMAND", "COUNT"]),
            Frame::Integer(i64::try_from(command::count()).expect("small"))
        );
    }

    #[test]
    fn flushall_empties_every_database() {
        let mut client = Harness::new();
        client.run(&["SET", "a", "1"]);
        client.run(&["SELECT", "2"]);
        client.run(&["SET", "b", "2"]);
        assert_eq!(client.run(&["FLUSHALL", "ASYNC"]), Frame::ok());
        assert_eq!(client.run(&["GET", "b"]), Frame::Null);
        client.run(&["SELECT", "0"]);
        assert_eq!(client.run(&["GET", "a"]), Frame::Null);
        assert_eq!(
            client.run(&["FLUSHALL", "later"]),
            crate::command::harness::error("ERR syntax error")
        );
    }

    #[test]
    fn dbsize_and_flushdb_work_on_one_database() {
        let mut client = Harness::new();
        client.run(&["MSET", "a", "1", "b", "2"]);
        client.run(&["SELECT", "1"]);
        client.run(&["SET", "c", "3"]);
        assert_eq!(client.run(&["DBSIZE"]), Frame::Integer(1));
        assert_eq!(client.run(&["FLUSHDB"]), Frame::ok());
        assert_eq!(client.run(&["DBSIZE"]), Frame::Integer(0));
        client.run(&["SELECT", "0"]);
        assert_eq!(client.run(&["DBSIZE"]), Frame::Integer(2));
        assert_eq!(client.run(&["FLUSHDB", "SYNC"]), Frame::ok());
        assert_eq!(
            client.run(&["FLUSHDB", "x"]),
            crate::command::harness::error("ERR syntax error")
        );
    }

    #[test]
    fn config_get_matches_nothing() {
        assert_eq!(run(&["CONFIG", "GET", "maxmemory"]), Frame::Map(Vec::new()));
    }
}
