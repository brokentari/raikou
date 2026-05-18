//! The command table and dispatch.

#![allow(
    clippy::unnecessary_wraps,
    reason = "every handler shares one signature"
)]

mod connection;
#[cfg(test)]
pub(crate) mod harness;
mod hashes;
mod keys;
mod lists;
mod server;
mod strings;

use bytes::Bytes;

use crate::error::CommandError;
use crate::resp::Frame;
use crate::session::Session;
use crate::state::State;

/// The Redis version that Raikou reports and is tested against.
pub(crate) const REDIS_VERSION: &str = "8.10.3";

/// What a handler can see and change.
pub(crate) struct Context<'a> {
    pub(crate) state: &'a mut State,
    pub(crate) session: &'a mut Session,
    pub(crate) now_ms: u64,
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
    command("append", 3, strings::append),
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
    command("dbsize", 1, server::dbsize),
    command("decr", 2, strings::decr),
    command("decrby", 3, strings::decrby),
    command("del", -2, keys::del),
    command("echo", 2, connection::echo),
    command("exists", -2, keys::exists),
    command("expire", -3, keys::expire),
    command("expireat", -3, keys::expireat),
    command("expiretime", 2, keys::expiretime),
    command("flushall", -1, server::flushall),
    command("flushdb", -1, server::flushdb),
    command("get", 2, strings::get),
    command("getdel", 2, strings::getdel),
    command("getex", -2, strings::getex),
    command("getrange", 4, strings::getrange),
    command("getset", 3, strings::getset),
    command("hdel", -3, hashes::hdel),
    command("hello", -1, connection::hello),
    command("hexists", 3, hashes::hexists),
    command("hget", 3, hashes::hget),
    command("hgetall", 2, hashes::hgetall),
    command("hkeys", 2, hashes::hkeys),
    command("hlen", 2, hashes::hlen),
    command("hmget", -3, hashes::hmget),
    command("hmset", -4, hashes::hmset),
    command("hset", -4, hashes::hset),
    command("hvals", 2, hashes::hvals),
    command("incr", 2, strings::incr),
    command("incrby", 3, strings::incrby),
    command("incrbyfloat", 3, strings::incrbyfloat),
    command("info", -1, server::info),
    command("keys", 2, keys::keys),
    command("lindex", 3, lists::lindex),
    command("linsert", 5, lists::linsert),
    command("llen", 2, lists::llen),
    command("lmove", 5, lists::lmove),
    command("lpop", -2, lists::lpop),
    command("lpush", -3, lists::lpush),
    command("lpushx", -3, lists::lpushx),
    command("lrange", 4, lists::lrange),
    command("lrem", 4, lists::lrem),
    command("lset", 4, lists::lset),
    command("ltrim", 4, lists::ltrim),
    command("mget", -2, strings::mget),
    command("mset", -3, strings::mset),
    command("msetnx", -3, strings::msetnx),
    command("persist", 2, keys::persist),
    command("pexpire", -3, keys::pexpire),
    command("pexpireat", -3, keys::pexpireat),
    command("pexpiretime", 2, keys::pexpiretime),
    command("ping", -1, connection::ping),
    command("psetex", 4, strings::psetex),
    command("pttl", 2, keys::pttl),
    command("quit", -1, connection::quit),
    command("randomkey", 1, keys::randomkey),
    command("rename", 3, keys::rename),
    command("renamenx", 3, keys::renamenx),
    command("reset", 1, connection::reset),
    command("rpop", -2, lists::rpop),
    command("rpoplpush", 3, lists::rpoplpush),
    command("rpush", -3, lists::rpush),
    command("rpushx", -3, lists::rpushx),
    command("scan", -2, keys::scan),
    command("select", 2, connection::select),
    command("set", -3, strings::set),
    command("setex", 4, strings::setex),
    command("setnx", 3, strings::setnx),
    command("setrange", 4, strings::setrange),
    command("strlen", 2, strings::strlen),
    command("touch", -2, keys::touch),
    command("ttl", 2, keys::ttl),
    command("type", 2, keys::type_of),
    command("unlink", -2, keys::unlink),
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
pub(crate) fn dispatch(
    state: &mut State,
    session: &mut Session,
    now_ms: u64,
    argv: &[Bytes],
) -> Frame {
    execute(
        &mut Context {
            state,
            session,
            now_ms,
        },
        argv,
    )
    .unwrap_or_else(Frame::from)
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
    use crate::command::harness::{Harness, error};

    fn run(parts: &[&str]) -> Frame {
        Harness::new().run(parts)
    }

    fn check_table(table: &[Command]) {
        assert!(table.windows(2).all(|pair| pair[0].name < pair[1].name));
        for command in table {
            assert!(
                command.name.chars().all(|c| !c.is_uppercase()),
                "{}",
                command.name
            );
            check_table(command.subcommands);
        }
    }

    #[test]
    fn table_is_sorted_and_unique() {
        check_table(COMMANDS);
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
