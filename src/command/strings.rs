//! String commands.

use bytes::Bytes;

use crate::command::Context;
use crate::error::CommandError;
use crate::resp::Frame;
use crate::value::Value;

pub(super) fn get(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    let Context {
        state,
        session,
        now_ms,
    } = context;
    match state.store.get(session.db, &argv[1], *now_ms) {
        Some(Value::Str(data)) => Ok(Frame::bulk(data.clone())),
        None => Ok(Frame::Null),
    }
}

pub(super) fn set(context: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    if argv.len() > 3 {
        return Err(CommandError::Syntax);
    }
    let db = context.session.db;
    context
        .state
        .store
        .set(db, argv[1].clone(), Value::Str(argv[2].clone()));
    Ok(Frame::ok())
}

#[cfg(test)]
mod tests {
    use crate::command::harness::{Harness, error};

    use super::*;

    #[test]
    fn set_then_get() {
        let mut client = Harness::new();
        assert_eq!(client.run(&["GET", "k"]), Frame::Null);
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
        assert_eq!(client.run(&["GET", "k"]), Frame::Null);
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
        assert_eq!(
            client.run(&["SET", "k", "v", "nope"]),
            error("ERR syntax error")
        );
    }
}
