//! Request parsing: bytes from a client to command arguments.

use std::fmt;

use bytes::Bytes;

use crate::number;

const MAX_LINE_LEN: usize = 64 * 1024;
const MAX_BULK_LEN: i64 = 512 * 1024 * 1024;

/// Why a client's bytes are not a request; the connection closes after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
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

/// Parses the first request in `buf`; `None` means it is incomplete.
pub fn parse(buf: &[u8]) -> Parsed {
    match buf.first() {
        None => Ok(None),
        Some(b'*') => multibulk(buf),
        Some(&other) => Err(ProtocolError::ExpectedDollar(other)),
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

fn multibulk(buf: &[u8]) -> Parsed {
    let Some((head, mut pos)) = line(buf, 1, ProtocolError::TooBigMultibulkCount)? else {
        return Ok(None);
    };
    let count = number::parse_i64(head)
        .filter(|&count| count <= i64::from(i32::MAX))
        .ok_or(ProtocolError::InvalidMultibulkLength)?;
    let mut args = Vec::new();
    for _ in 0..count {
        let Some((head, start)) = line(buf, pos, ProtocolError::TooBigBulkCount)? else {
            return Ok(None);
        };
        if buf[pos] != b'$' {
            return Err(ProtocolError::ExpectedDollar(buf[pos]));
        }
        let len = number::parse_i64(&head[1..])
            .filter(|len| (0..=MAX_BULK_LEN).contains(len))
            .and_then(|len| usize::try_from(len).ok())
            .ok_or(ProtocolError::InvalidBulkLength)?;
        let end = start + len;
        if buf.len() < end + 2 {
            return Ok(None);
        }
        args.push(Bytes::copy_from_slice(&buf[start..end]));
        pos = end + 2;
    }
    Ok(Some((args, pos)))
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
