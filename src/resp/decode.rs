//! Frame decoding.

use std::fmt;

use bytes::Bytes;

use crate::resp::Frame;

const MAX_DEPTH: usize = 128;
const MAX_BULK_LEN: i64 = 512 * 1024 * 1024;
const MAX_ARRAY_LEN: i64 = i32::MAX as i64;

/// Why a buffer is not a valid frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The first byte of a frame is not a known type byte.
    InvalidType(u8),
    /// A length is negative, too large or not a number.
    InvalidLength,
    /// An integer is not a number.
    InvalidNumber,
    /// A bulk payload is not followed by CRLF.
    MissingTerminator,
    /// Arrays nest deeper than the decoder allows.
    TooDeep,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidType(byte) => write!(f, "invalid type byte {byte:#04x}"),
            Self::InvalidLength => f.write_str("invalid length"),
            Self::InvalidNumber => f.write_str("invalid number"),
            Self::MissingTerminator => f.write_str("missing CRLF after payload"),
            Self::TooDeep => f.write_str("frames nest too deeply"),
        }
    }
}

impl std::error::Error for DecodeError {}

type Decoded = Result<Option<(Frame, usize)>, DecodeError>;

/// Decodes the first frame in `buf`; `None` means the frame is incomplete.
pub fn decode(buf: &[u8]) -> Decoded {
    parse(buf, 0, 0)
}

fn parse(buf: &[u8], pos: usize, depth: usize) -> Decoded {
    if depth > MAX_DEPTH {
        return Err(DecodeError::TooDeep);
    }
    let Some(&kind) = buf.get(pos) else {
        return Ok(None);
    };
    let Some((head, next)) = read_line(buf, pos + 1) else {
        return Ok(None);
    };
    match kind {
        b'+' => Ok(Some((
            Frame::SimpleString(Bytes::copy_from_slice(head)),
            next,
        ))),
        b'-' => Ok(Some((Frame::Error(Bytes::copy_from_slice(head)), next))),
        b':' => Ok(Some((Frame::Integer(number(head)?), next))),
        b'$' => bulk(buf, head, next),
        b'*' => array(buf, head, next, depth),
        other => Err(DecodeError::InvalidType(other)),
    }
}

fn read_line(buf: &[u8], pos: usize) -> Option<(&[u8], usize)> {
    let rest = buf.get(pos..)?;
    let end = rest.windows(2).position(|pair| pair == b"\r\n")?;
    Some((&rest[..end], pos + end + 2))
}

fn number(text: &[u8]) -> Result<i64, DecodeError> {
    std::str::from_utf8(text)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or(DecodeError::InvalidNumber)
}

fn length(text: &[u8], max: i64) -> Result<Option<usize>, DecodeError> {
    match number(text).map_err(|_| DecodeError::InvalidLength)? {
        -1 => Ok(None),
        n if (0..=max).contains(&n) => Ok(Some(
            usize::try_from(n).map_err(|_| DecodeError::InvalidLength)?,
        )),
        _ => Err(DecodeError::InvalidLength),
    }
}

fn bulk(buf: &[u8], head: &[u8], start: usize) -> Decoded {
    let Some(len) = length(head, MAX_BULK_LEN)? else {
        return Ok(Some((Frame::NullBulk, start)));
    };
    let end = start + len;
    let Some(tail) = buf.get(end..end + 2) else {
        return Ok(None);
    };
    if tail != b"\r\n" {
        return Err(DecodeError::MissingTerminator);
    }
    Ok(Some((
        Frame::BulkString(Bytes::copy_from_slice(&buf[start..end])),
        end + 2,
    )))
}

fn array(buf: &[u8], head: &[u8], start: usize, depth: usize) -> Decoded {
    let Some(len) = length(head, MAX_ARRAY_LEN)? else {
        return Ok(Some((Frame::NullArray, start)));
    };
    let mut items = Vec::new();
    let mut pos = start;
    for _ in 0..len {
        let Some((item, next)) = parse(buf, pos, depth + 1)? else {
            return Ok(None);
        };
        items.push(item);
        pos = next;
    }
    Ok(Some((Frame::Array(items), pos)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(input: &[u8]) -> (Frame, usize) {
        decode(input).expect("valid input").expect("complete frame")
    }

    #[test]
    fn simple_types() {
        assert_eq!(ok(b"+OK\r\n"), (Frame::SimpleString(Bytes::from("OK")), 5));
        assert_eq!(ok(b"-ERR x\r\n"), (Frame::Error(Bytes::from("ERR x")), 8));
        assert_eq!(ok(b":-12\r\n"), (Frame::Integer(-12), 6));
    }

    #[test]
    fn bulk_strings() {
        assert_eq!(
            ok(b"$3\r\nabc\r\n"),
            (Frame::BulkString(Bytes::from("abc")), 9)
        );
        assert_eq!(ok(b"$0\r\n\r\n"), (Frame::BulkString(Bytes::new()), 6));
        assert_eq!(
            ok(b"$2\r\n\r\n\r\n").0,
            Frame::BulkString(Bytes::from("\r\n"))
        );
        assert_eq!(ok(b"$-1\r\n"), (Frame::NullBulk, 5));
    }

    #[test]
    fn arrays() {
        let (frame, used) = ok(b"*2\r\n:1\r\n*1\r\n$-1\r\n+x");
        assert_eq!(
            frame,
            Frame::Array(vec![Frame::Integer(1), Frame::Array(vec![Frame::NullBulk])])
        );
        assert_eq!(used, 17);
        assert_eq!(ok(b"*0\r\n"), (Frame::Array(vec![]), 4));
        assert_eq!(ok(b"*-1\r\n"), (Frame::NullArray, 5));
    }

    #[test]
    fn every_proper_prefix_is_incomplete() {
        let input = b"*3\r\n$3\r\nabc\r\n:7\r\n*1\r\n+ok\r\n";
        for end in 0..input.len() {
            assert_eq!(decode(&input[..end]), Ok(None), "prefix of {end} bytes");
        }
        assert_eq!(ok(input).1, input.len());
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(decode(b"?x\r\n"), Err(DecodeError::InvalidType(b'?')));
        assert_eq!(decode(b":abc\r\n"), Err(DecodeError::InvalidNumber));
        assert_eq!(decode(b"$-2\r\n"), Err(DecodeError::InvalidLength));
        assert_eq!(decode(b"$x\r\n"), Err(DecodeError::InvalidLength));
        assert_eq!(decode(b"*9999999999\r\n"), Err(DecodeError::InvalidLength));
        assert_eq!(decode(b"$2\r\nabXY"), Err(DecodeError::MissingTerminator));
    }

    #[test]
    fn huge_declared_length_waits_for_data() {
        assert_eq!(decode(b"$536870912\r\nabc"), Ok(None));
        assert_eq!(decode(b"*2147483647\r\n:1\r\n"), Ok(None));
    }

    #[test]
    fn deep_nesting_is_an_error() {
        let input = "*1\r\n".repeat(MAX_DEPTH + 2);
        assert_eq!(decode(input.as_bytes()), Err(DecodeError::TooDeep));
    }
}
