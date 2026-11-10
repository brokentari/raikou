//! Request parsing: bytes from a client to command arguments.

use std::fmt;

use bytes::Bytes;

use crate::number;

const MAX_LINE_LEN: usize = 64 * 1024;
const MAX_BULK_LEN: i64 = 512 * 1024 * 1024;

/// Why a client's bytes are not a request; the connection closes after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProtocolError {
    /// An inline request has no end within the line limit.
    TooBigInline,
    /// An inline request has a quote that is not closed or not followed by a space.
    UnbalancedQuotes,
    /// The multibulk count line has no end within the line limit.
    TooBigMultibulkCount,
    /// The multibulk count is not a number or is out of range.
    InvalidMultibulkLength,
    /// An argument does not start with `$`.
    ExpectedDollar(u8),
    /// The bulk length line has no end within the line limit.
    TooBigBulkCount,
    /// The bulk length is not a number or is out of range.
    InvalidBulkLength,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooBigInline => f.write_str("too big inline request"),
            Self::UnbalancedQuotes => f.write_str("unbalanced quotes in request"),
            Self::TooBigMultibulkCount => f.write_str("too big mbulk count string"),
            Self::InvalidMultibulkLength => f.write_str("invalid multibulk length"),
            Self::ExpectedDollar(got) => write!(f, "expected '$', got '{}'", char::from(*got)),
            Self::TooBigBulkCount => f.write_str("too big bulk count string"),
            Self::InvalidBulkLength => f.write_str("invalid bulk length"),
        }
    }
}

impl std::error::Error for ProtocolError {}

type Parsed = Result<Option<(Vec<Bytes>, usize)>, ProtocolError>;

/// Parses requests from a growing buffer and remembers how far an unfinished one got.
#[derive(Debug, Default)]
pub(crate) struct Parser {
    partial: Option<Partial>,
}

#[derive(Debug)]
struct Partial {
    left: i64,
    args: Vec<Bytes>,
    pos: usize,
}

impl Parser {
    /// Parses the first request in `buf`; `None` means it is incomplete, so call again with more.
    pub(crate) fn parse(&mut self, buf: &[u8]) -> Parsed {
        let result = if self.partial.is_some() {
            self.resume(buf)
        } else {
            match buf.first() {
                None => Ok(None),
                Some(b'*') => self.start(buf),
                Some(_) => inline(buf),
            }
        };
        if result.is_err() {
            self.partial = None;
        }
        result
    }

    fn start(&mut self, buf: &[u8]) -> Parsed {
        let Some((head, pos)) = line(buf, 1, ProtocolError::TooBigMultibulkCount)? else {
            return Ok(None);
        };
        let count = number::parse_i64(head)
            .filter(|&count| count <= i64::from(i32::MAX))
            .ok_or(ProtocolError::InvalidMultibulkLength)?;
        if count <= 0 {
            return Ok(Some((Vec::new(), pos)));
        }
        self.partial = Some(Partial {
            left: count,
            args: Vec::new(),
            pos,
        });
        self.resume(buf)
    }

    fn resume(&mut self, buf: &[u8]) -> Parsed {
        let Some(partial) = self.partial.as_mut() else {
            return Ok(None);
        };
        while partial.left > 0 {
            let Some((head, start)) = line(buf, partial.pos, ProtocolError::TooBigBulkCount)?
            else {
                return Ok(None);
            };
            if buf[partial.pos] != b'$' {
                return Err(ProtocolError::ExpectedDollar(buf[partial.pos]));
            }
            let len = number::parse_i64(&head[1..])
                .filter(|len| (0..=MAX_BULK_LEN).contains(len))
                .and_then(|len| usize::try_from(len).ok())
                .ok_or(ProtocolError::InvalidBulkLength)?;
            let end = start + len;
            if buf.len() < end + 2 {
                return Ok(None);
            }
            partial.args.push(Bytes::copy_from_slice(&buf[start..end]));
            partial.pos = end + 2;
            partial.left -= 1;
        }
        Ok(self.partial.take().map(|done| (done.args, done.pos)))
    }
}

/// Parses the first request in `buf` with a fresh parser.
#[cfg(test)]
pub(crate) fn parse(buf: &[u8]) -> Parsed {
    Parser::default().parse(buf)
}

fn inline(buf: &[u8]) -> Parsed {
    let Some(end) = buf.iter().position(|&byte| byte == b'\n') else {
        return if buf.len() > MAX_LINE_LEN {
            Err(ProtocolError::TooBigInline)
        } else {
            Ok(None)
        };
    };
    let text = &buf[..end];
    let text = text.strip_suffix(b"\r").unwrap_or(text);
    Ok(Some((split_args(text)?, end + 1)))
}

fn split_args(text: &[u8]) -> Result<Vec<Bytes>, ProtocolError> {
    let text = text.split(|&byte| byte == 0).next().unwrap_or_default();
    let mut args = Vec::new();
    let mut pos = 0;
    loop {
        while text.get(pos).is_some_and(|byte| is_space(*byte)) {
            pos += 1;
        }
        if pos >= text.len() {
            return Ok(args);
        }
        let mut arg = Vec::new();
        while let Some(&byte) = text.get(pos).filter(|byte| !is_space(**byte)) {
            match byte {
                b'"' => {
                    pos = double_quoted(text, pos + 1, &mut arg)?;
                    break;
                }
                b'\'' => {
                    pos = single_quoted(text, pos + 1, &mut arg)?;
                    break;
                }
                _ => {
                    arg.push(byte);
                    pos += 1;
                }
            }
        }
        args.push(Bytes::from(arg));
    }
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn closes_cleanly(text: &[u8], pos: usize) -> Result<usize, ProtocolError> {
    match text.get(pos + 1) {
        Some(&next) if !is_space(next) => Err(ProtocolError::UnbalancedQuotes),
        _ => Ok(pos + 1),
    }
}

fn double_quoted(text: &[u8], mut pos: usize, arg: &mut Vec<u8>) -> Result<usize, ProtocolError> {
    loop {
        match text.get(pos) {
            None => return Err(ProtocolError::UnbalancedQuotes),
            Some(b'"') => return closes_cleanly(text, pos),
            Some(b'\\') if text.get(pos + 1) == Some(&b'x') => {
                let hex = text.get(pos + 2..pos + 4).and_then(|pair| {
                    let digits = std::str::from_utf8(pair).ok()?;
                    u8::from_str_radix(digits, 16)
                        .ok()
                        .filter(|_| pair.iter().all(u8::is_ascii_hexdigit))
                });
                if let Some(byte) = hex {
                    arg.push(byte);
                    pos += 4;
                } else {
                    arg.push(b'x');
                    pos += 2;
                }
            }
            Some(b'\\') if pos + 1 < text.len() => {
                arg.push(match text[pos + 1] {
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'b' => 0x08,
                    b'a' => 0x07,
                    other => other,
                });
                pos += 2;
            }
            Some(&byte) => {
                arg.push(byte);
                pos += 1;
            }
        }
    }
}

fn single_quoted(text: &[u8], mut pos: usize, arg: &mut Vec<u8>) -> Result<usize, ProtocolError> {
    loop {
        match text.get(pos) {
            None => return Err(ProtocolError::UnbalancedQuotes),
            Some(b'\'') => return closes_cleanly(text, pos),
            Some(b'\\') if text.get(pos + 1) == Some(&b'\'') => {
                arg.push(b'\'');
                pos += 2;
            }
            Some(&byte) => {
                arg.push(byte);
                pos += 1;
            }
        }
    }
}

fn line(
    buf: &[u8],
    pos: usize,
    too_big: ProtocolError,
) -> Result<Option<(&[u8], usize)>, ProtocolError> {
    let rest = &buf[pos..];
    match rest.iter().position(|&byte| byte == b'\r') {
        Some(end) if end + 1 < rest.len() => Ok(Some((&rest[..end], pos + end + 2))),
        None if rest.len() > MAX_LINE_LEN => Err(too_big),
        Some(_) | None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parsed: Parsed) -> (Vec<Bytes>, usize) {
        parsed.expect("valid request").expect("complete request")
    }

    #[test]
    fn parses_one_request() {
        let (argv, used) = args(parse(b"*2\r\n$4\r\nECHO\r\n$2\r\nhi\r\n*1\r\n"));
        assert_eq!(argv, vec![Bytes::from("ECHO"), Bytes::from("hi")]);
        assert_eq!(used, 22);
    }

    #[test]
    fn payload_may_hold_crlf_and_be_empty() {
        let (argv, _) = args(parse(b"*2\r\n$3\r\na\r\n\r\n$0\r\n\r\n"));
        assert_eq!(argv, vec![Bytes::from("a\r\n"), Bytes::new()]);
    }

    #[test]
    fn skips_empty_multibulk() {
        assert_eq!(args(parse(b"*0\r\n")), (vec![], 4));
        assert_eq!(args(parse(b"*-1\r\n")), (vec![], 5));
    }

    #[test]
    fn every_proper_prefix_is_incomplete() {
        let input = b"*2\r\n$4\r\nECHO\r\n$2\r\nhi\r\n";
        for end in 0..input.len() {
            assert_eq!(parse(&input[..end]), Ok(None), "prefix of {end} bytes");
        }
    }

    fn inline_args(input: &[u8]) -> Vec<String> {
        let (argv, used) = args(parse(input));
        assert_eq!(used, input.len());
        argv.iter()
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect()
    }

    #[test]
    fn splits_inline_commands() {
        assert_eq!(inline_args(b"PING\r\n"), ["PING"]);
        assert_eq!(inline_args(b"  SET   k\tv \n"), ["SET", "k", "v"]);
        assert_eq!(inline_args(b"\r\n"), Vec::<String>::new());
        assert_eq!(inline_args(b"\n"), Vec::<String>::new());
    }

    #[test]
    fn inline_double_quotes() {
        assert_eq!(inline_args(b"ECHO \"a b\"\r\n"), ["ECHO", "a b"]);
        assert_eq!(
            inline_args(b"ECHO \"\\x41\\n\\t\\\\\\\"\"\r\n"),
            ["ECHO", "A\n\t\\\""]
        );
        assert_eq!(inline_args(b"ECHO \"\\xZZ\"\r\n"), ["ECHO", "xZZ"]);
        assert_eq!(inline_args(b"ECHO \"\"\r\n"), ["ECHO", ""]);
        assert_eq!(inline_args(b"ECHO a\"b c\"\r\n"), ["ECHO", "ab c"]);
        let (argv, _) = args(parse(b"ECHO \"\\xff\\b\\a\\r\"\r\n"));
        assert_eq!(argv[1].as_ref(), b"\xff\x08\x07\r");
    }

    #[test]
    fn inline_single_quotes() {
        assert_eq!(inline_args(b"ECHO 'a \\'b' c\r\n"), ["ECHO", "a 'b", "c"]);
        assert_eq!(inline_args(b"ECHO '\\n'\r\n"), ["ECHO", "\\n"]);
    }

    #[test]
    fn inline_errors() {
        assert_eq!(
            parse(b"ECHO \"abc\r\n"),
            Err(ProtocolError::UnbalancedQuotes)
        );
        assert_eq!(
            parse(b"ECHO 'abc\r\n"),
            Err(ProtocolError::UnbalancedQuotes)
        );
        assert_eq!(
            parse(b"ECHO \"a\"b\r\n"),
            Err(ProtocolError::UnbalancedQuotes)
        );
        let long = vec![b'a'; MAX_LINE_LEN + 1];
        assert_eq!(parse(&long), Err(ProtocolError::TooBigInline));
        assert_eq!(parse(&long[..MAX_LINE_LEN]), Ok(None));
    }

    #[test]
    fn inline_waits_for_newline() {
        assert_eq!(parse(b"PING"), Ok(None));
        assert_eq!(parse(b"PING\r"), Ok(None));
    }

    #[test]
    fn a_parser_resumes_where_it_stopped() {
        let input = b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$5\r\nhello\r\n";
        let mut parser = Parser::default();
        for end in 0..input.len() {
            assert_eq!(
                parser.parse(&input[..end]),
                Ok(None),
                "prefix of {end} bytes"
            );
        }
        let (argv, used) = parser.parse(input).expect("valid").expect("complete");
        assert_eq!((argv.len(), used), (3, input.len()));
        let (next, _) = parser.parse(b"PING\r\n").expect("valid").expect("complete");
        assert_eq!(next, vec![Bytes::from("PING")]);
    }

    #[test]
    fn a_parser_forgets_a_failed_request() {
        let mut parser = Parser::default();
        assert_eq!(
            parser.parse(b"*2\r\n$1\r\na\r\n:1\r\n"),
            Err(ProtocolError::ExpectedDollar(b':'))
        );
        let (argv, _) = parser.parse(b"PING\r\n").expect("valid").expect("complete");
        assert_eq!(argv, vec![Bytes::from("PING")]);
    }

    #[test]
    fn rejects_bad_counts_and_lengths() {
        assert_eq!(parse(b"*x\r\n"), Err(ProtocolError::InvalidMultibulkLength));
        assert_eq!(
            parse(b"*2147483648\r\n"),
            Err(ProtocolError::InvalidMultibulkLength)
        );
        assert_eq!(
            parse(b"*1\r\n$x\r\n"),
            Err(ProtocolError::InvalidBulkLength)
        );
        assert_eq!(
            parse(b"*1\r\n$-1\r\n"),
            Err(ProtocolError::InvalidBulkLength)
        );
        assert_eq!(
            parse(b"*1\r\n$536870913\r\n"),
            Err(ProtocolError::InvalidBulkLength)
        );
        assert_eq!(
            parse(b"*1\r\n:1\r\n"),
            Err(ProtocolError::ExpectedDollar(b':'))
        );
    }

    #[test]
    fn rejects_overlong_header_lines() {
        let mut count = b"*".to_vec();
        count.resize(MAX_LINE_LEN + 2, b'1');
        assert_eq!(parse(&count), Err(ProtocolError::TooBigMultibulkCount));
        let mut bulk = b"*1\r\n$".to_vec();
        bulk.resize(MAX_LINE_LEN + 8, b'1');
        assert_eq!(parse(&bulk), Err(ProtocolError::TooBigBulkCount));
    }

    #[test]
    fn huge_declared_length_waits_for_data() {
        assert_eq!(parse(b"*1\r\n$536870912\r\nabc"), Ok(None));
        assert_eq!(parse(b"*2147483647\r\n$1\r\na\r\n"), Ok(None));
    }
}
