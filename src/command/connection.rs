//! Connection commands: `PING` and `ECHO`.

use bytes::Bytes;

use crate::command::Context;
use crate::error::CommandError;
use crate::resp::Frame;

pub(super) fn ping(_: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    match argv {
        [_] => Ok(Frame::SimpleString(Bytes::from_static(b"PONG"))),
        [_, message] => Ok(Frame::BulkString(message.clone())),
        _ => Err(CommandError::WrongArity("ping".to_owned())),
    }
}

pub(super) fn echo(_: &mut Context<'_>, argv: &[Bytes]) -> Result<Frame, CommandError> {
    Ok(Frame::BulkString(argv[1].clone()))
}
