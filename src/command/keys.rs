//! Key commands.

use bytes::Bytes;

use crate::command::Context;
use crate::error::CommandError;
use crate::resp::Frame;

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
    use crate::command::harness::Harness;

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
}
