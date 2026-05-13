//! The command table and dispatch.

#![allow(
    clippy::unnecessary_wraps,
    reason = "every handler shares one signature"
)]

mod connection;

use bytes::Bytes;

use crate::error::CommandError;
use crate::resp::Frame;
use crate::session::Session;

/// What a handler can see and change.
pub(crate) struct Context<'a> {
    #[expect(dead_code, reason = "HELLO reads the session in a later commit")]
    pub(crate) session: &'a mut Session,
}

pub(crate) type Handler = fn(&mut Context<'_>, &[Bytes]) -> Result<Frame, CommandError>;

/// One row of the command table.
pub(crate) struct Command {
    pub(crate) name: &'static str,
    pub(crate) arity: i32,
    pub(crate) handler: Handler,
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
}

// Sorted by name: `find` binary-searches it.
static COMMANDS: &[Command] = &[
    Command {
        name: "echo",
        arity: 2,
        handler: connection::echo,
    },
    Command {
        name: "ping",
        arity: -1,
        handler: connection::ping,
    },
];

fn find(name: &[u8]) -> Option<&'static Command> {
    let index = COMMANDS
        .binary_search_by(|command| {
            command
                .name
                .bytes()
                .cmp(name.iter().map(u8::to_ascii_lowercase))
        })
        .ok()?;
    Some(&COMMANDS[index])
}

/// Runs the request `argv` for a client and returns the reply.
pub(crate) fn dispatch(session: &mut Session, argv: &[Bytes]) -> Frame {
    execute(&mut Context { session }, argv).unwrap_or_else(Frame::from)
}

fn execute(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let command = find(&argv[0]).ok_or_else(|| CommandError::unknown_command(argv))?;
    if !command.accepts(argv.len()) {
        return Err(CommandError::WrongArity(command.name.to_owned()));
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
        dispatch(&mut Session::new(), &argv)
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
        let exact = Command {
            name: "x",
            arity: 2,
            handler: connection::ping,
        };
        assert!(exact.accepts(2) && !exact.accepts(1) && !exact.accepts(3));
        let at_least = Command {
            name: "x",
            arity: -2,
            handler: connection::ping,
        };
        assert!(at_least.accepts(2) && at_least.accepts(9) && !at_least.accepts(1));
    }
}
