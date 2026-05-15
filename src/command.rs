//! The command table and dispatch.

#![allow(
    clippy::unnecessary_wraps,
    reason = "every handler shares one signature"
)]

mod connection;
mod server;

use bytes::Bytes;

use crate::error::CommandError;
use crate::resp::Frame;
use crate::session::Session;

/// The Redis version that Raikou reports and is tested against.
pub(crate) const REDIS_VERSION: &str = "8.10.3";

/// What a handler can see and change.
pub(crate) struct Context<'a> {
    pub(crate) session: &'a mut Session,
}

pub(crate) type Handler = fn(&mut Context<'_>, &[Bytes]) -> Result<Frame, CommandError>;

/// One row of the command table.
pub(crate) struct Command {
    pub(crate) name: &'static str,
    pub(crate) arity: i32,
    pub(crate) handler: Handler,
    pub(crate) subcommands: &'static [Command],
}

impl Command {
    fn accepts(&self, argc: usize) -> bool {
        let argc = i64::try_from(argc).unwrap_or(i64::MAX);
        let arity = i64::from(self.arity);
        if arity >= 0 {
            argc == arity
        } else {
            argc >= -arity
        }
    }

    const fn with_subcommands(mut self, subcommands: &'static [Command]) -> Self {
        self.subcommands = subcommands;
        self
    }
}

fn container(_: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    Err(CommandError::unknown_command(argv))
}

const fn parent(name: &'static str, arity: i32, subcommands: &'static [Command]) -> Command {
    Command {
        name,
        arity,
        handler: container,
        subcommands,
    }
}

const fn command(name: &'static str, arity: i32, handler: Handler) -> Command {
    Command {
        name,
        arity,
        handler,
        subcommands: &[],
    }
}

// Sorted by name: `find` binary-searches each table.
static COMMANDS: &[Command] = &[
    command("auth", -2, connection::auth),
    parent(
        "client",
        -2,
        &[
            command("getname", 2, connection::client_getname),
            command("id", 2, connection::client_id),
            command("info", 2, connection::client_info),
            command("setinfo", 4, connection::client_setinfo),
            command("setname", 3, connection::client_setname),
        ],
    ),
    command("command", -1, server::command).with_subcommands(&[
        command("count", 2, server::command_count),
        command("docs", -2, server::command_docs),
    ]),
    parent("config", -2, &[command("get", -3, server::config_get)]),
    command("echo", 2, connection::echo),
    command("hello", -1, connection::hello),
    command("info", -1, server::info),
    command("ping", -1, connection::ping),
    command("quit", -1, connection::quit),
    command("reset", 1, connection::reset),
    command("select", 2, connection::select),
];

fn find(table: &'static [Command], name: &[u8]) -> Option<&'static Command> {
    let index = table
        .binary_search_by(|command| {
            command
                .name
                .bytes()
                .cmp(name.iter().map(u8::to_ascii_lowercase))
        })
        .ok()?;
    Some(&table[index])
}

/// The number of top-level commands in the table.
pub(crate) fn count() -> usize {
    COMMANDS.len()
}

/// Runs the request `argv` for a client and returns the reply.
pub(crate) fn dispatch(session: &mut Session, argv: &[Bytes]) -> Frame {
    execute(&mut Context { session }, argv).unwrap_or_else(Frame::from)
}

fn execute(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let mut command =
        find(COMMANDS, &argv[0]).ok_or_else(|| CommandError::unknown_command(argv))?;
    if !command.accepts(argv.len()) {
        return Err(CommandError::WrongArity(command.name.to_owned()));
    }
    if !command.subcommands.is_empty() && argv.len() >= 2 {
        let parent = command;
        command = find(parent.subcommands, &argv[1])
            .ok_or_else(|| CommandError::unknown_subcommand(parent.name, &argv[1]))?;
        if !command.accepts(argv.len()) {
            return Err(CommandError::WrongArity(format!(
                "{}|{}",
                parent.name, command.name
            )));
        }
    }
    (command.handler)(context, argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(parts: &[&str]) -> Frame {
        let argv: Vec<Bytes> = parts
            .iter()
            .map(|part| Bytes::copy_from_slice(part.as_bytes()))
            .collect();
        dispatch(&mut Session::new(1, String::new(), String::new()), &argv)
    }

    fn error(text: &str) -> Frame {
        Frame::Error(Bytes::copy_from_slice(text.as_bytes()))
    }

    #[test]
    fn table_is_sorted_and_unique() {
        assert!(COMMANDS.windows(2).all(|pair| pair[0].name < pair[1].name));
        assert!(
            COMMANDS
                .iter()
                .all(|command| command.name.chars().all(|c| !c.is_uppercase()))
        );
    }

    #[test]
    fn names_match_in_any_case() {
        assert_eq!(run(&["PING"]), Frame::SimpleString(Bytes::from("PONG")));
        assert_eq!(run(&["pInG"]), Frame::SimpleString(Bytes::from("PONG")));
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert_eq!(
            run(&["nope", "a"]),
            error("ERR unknown command 'nope', with args beginning with: 'a' ")
        );
    }

    #[test]
    fn arity_is_checked_before_the_handler_runs() {
        let message = "ERR wrong number of arguments for 'echo' command";
        assert_eq!(run(&["echo"]), error(message));
        assert_eq!(run(&["echo", "a", "b"]), error(message));
    }

    #[test]
    fn accepts_follows_the_arity_sign() {
        let exact = command("x", 2, connection::ping);
        assert!(exact.accepts(2) && !exact.accepts(1) && !exact.accepts(3));
        let at_least = command("x", -2, connection::ping);
        assert!(at_least.accepts(2) && at_least.accepts(9) && !at_least.accepts(1));
    }
}
