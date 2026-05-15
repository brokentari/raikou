//! Command errors, with the Redis wire text as their `Display`.

use std::fmt;

use bytes::{BufMut, Bytes, BytesMut};

use crate::resp::Frame;

const MAX_PREVIEW_LEN: usize = 128;

/// An error reply to one command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// The command name is not in the command table.
    UnknownCommand {
        /// The name as the client sent it, cut to the preview limit.
        name: Bytes,
        /// The quoted leading arguments.
        args: Bytes,
    },
    /// The command has too few or too many arguments; holds the command name.
    WrongArity(String),
    /// The subcommand name is not known to its command.
    UnknownSubcommand {
        /// The name as the client sent it, cut to the preview limit.
        name: Bytes,
        /// The command that owns the subcommands, in upper case.
        command: String,
    },
    /// An argument is not a 64-bit integer.
    NotInteger,
    /// The arguments do not fit the command's syntax.
    Syntax,
    /// The client asked for a RESP version the server lacks.
    NoProto,
    /// Any other error; holds the full text, including the `ERR` word.
    Other(Bytes),
}

impl CommandError {
    /// Builds the unknown-command error for a request `argv`.
    #[must_use]
    pub fn unknown_command(argv: &[Bytes]) -> Self {
        let mut preview = BytesMut::new();
        for arg in &argv[1..] {
            if preview.len() >= MAX_PREVIEW_LEN {
                break;
            }
            let room = MAX_PREVIEW_LEN - preview.len();
            preview.put_u8(b'\'');
            preview.put_slice(&arg[..arg.len().min(room)]);
            preview.put_slice(b"' ");
        }
        Self::UnknownCommand {
            name: argv[0].slice(..argv[0].len().min(MAX_PREVIEW_LEN)),
            args: preview.freeze(),
        }
    }

    /// Builds an error from its full text, such as `ERR syntax error`.
    #[must_use]
    pub fn other(text: impl Into<Bytes>) -> Self {
        Self::Other(text.into())
    }

    /// Builds the unknown-subcommand error for `command` and the name `sub` a client sent.
    #[must_use]
    pub fn unknown_subcommand(command: &str, sub: &Bytes) -> Self {
        Self::UnknownSubcommand {
            name: sub.slice(..sub.len().min(MAX_PREVIEW_LEN)),
            command: command.to_ascii_uppercase(),
        }
    }

    fn message(&self) -> Bytes {
        match self {
            Self::UnknownCommand { name, args } => {
                let mut text = BytesMut::new();
                text.put_slice(b"ERR unknown command '");
                text.put_slice(name);
                text.put_u8(b'\'');
                if !args.is_empty() {
                    text.put_slice(b", with args beginning with: ");
                    text.put_slice(args);
                }
                text.freeze()
            }
            Self::UnknownSubcommand { name, command } => {
                let mut text = BytesMut::new();
                text.put_slice(b"ERR unknown subcommand '");
                text.put_slice(name);
                text.put_slice(format!("'. Try {command} HELP.").as_bytes());
                text.freeze()
            }
            Self::Syntax => Bytes::from_static(b"ERR syntax error"),
            Self::NotInteger => Bytes::from_static(b"ERR value is not an integer or out of range"),
            Self::NoProto => Bytes::from_static(b"NOPROTO unsupported protocol version"),
            Self::Other(text) => text.clone(),
            Self::WrongArity(name) => Bytes::from(format!(
                "ERR wrong number of arguments for '{name}' command"
            )),
        }
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for CommandError {}

impl From<CommandError> for Frame {
    fn from(error: CommandError) -> Self {
        Frame::Error(error.message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<Bytes> {
        parts
            .iter()
            .map(|part| Bytes::copy_from_slice(part.as_bytes()))
            .collect()
    }

    #[test]
    fn unknown_command_lists_arguments() {
        let error = CommandError::unknown_command(&argv(&["foo", "a", "b c"]));
        assert_eq!(
            error.to_string(),
            "ERR unknown command 'foo', with args beginning with: 'a' 'b c' "
        );
    }

    #[test]
    fn unknown_command_without_arguments() {
        let error = CommandError::unknown_command(&argv(&["foo"]));
        assert_eq!(error.to_string(), "ERR unknown command 'foo'");
    }

    #[test]
    fn unknown_command_cuts_long_input() {
        let long = "x".repeat(300);
        let error = CommandError::unknown_command(&argv(&[&long, &long, "late"]));
        let expected = format!(
            "ERR unknown command '{}', with args beginning with: '{}' ",
            "x".repeat(128),
            "x".repeat(128)
        );
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn wrong_arity_names_the_command() {
        let error = CommandError::WrongArity("get".to_owned());
        assert_eq!(
            error.to_string(),
            "ERR wrong number of arguments for 'get' command"
        );
    }

    #[test]
    fn unknown_subcommand_names_the_command() {
        let error = CommandError::unknown_subcommand("client", &Bytes::from("foo"));
        assert_eq!(
            error.to_string(),
            "ERR unknown subcommand 'foo'. Try CLIENT HELP."
        );
    }

    #[test]
    fn fixed_texts() {
        assert_eq!(
            CommandError::NotInteger.to_string(),
            "ERR value is not an integer or out of range"
        );
        assert_eq!(
            CommandError::NoProto.to_string(),
            "NOPROTO unsupported protocol version"
        );
        assert_eq!(CommandError::other("ERR boom").to_string(), "ERR boom");
    }

    #[test]
    fn converts_to_an_error_frame() {
        let frame = Frame::from(CommandError::WrongArity("get".to_owned()));
        assert_eq!(
            frame,
            Frame::Error(Bytes::from(
                "ERR wrong number of arguments for 'get' command"
            ))
        );
    }
}
