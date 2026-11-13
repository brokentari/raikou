use std::fmt;

use bytes::Bytes;

use crate::resp::Frame;

const MAX_DEPTH: usize = 128;
const MAX_BULK_LEN: i64 = 512 * 1024 * 1024;
const MAX_ARRAY_LEN: i64 = i32::MAX as i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    InvalidType(u8),
    InvalidLength,
    InvalidNumber,
    MissingTerminator,
    InvalidBoolean,
    InvalidVerbatim,
    TooDeep,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidType(byte) => write!(f, "invalid type byte {byte:#04x}"),
            Self::InvalidLength => f.write_str("invalid length"),
            Self::InvalidNumber => f.write_str("invalid number"),
            Self::MissingTerminator => f.write_str("missing CRLF after payload"),
            Self::InvalidBoolean => f.write_str("invalid boolean"),
            Self::InvalidVerbatim => f.write_str("invalid verbatim string"),
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
        b'$' => match payload(buf, head, next)? {
            Payload::Null => Ok(Some((Frame::NullBulk, next))),
            Payload::Incomplete => Ok(None),
            Payload::Data(data, end) => Ok(Some((Frame::BulkString(data), end))),
        },
        b'*' => match elements(buf, head, next, depth, 1)? {
            None => Ok(None),
            Some(None) => Ok(Some((Frame::NullArray, next))),
            Some(Some((items, end))) => Ok(Some((Frame::Array(items), end))),
        },
        b'_' => Ok(Some((Frame::Null, next))),
        b'#' => match head {
            b"t" => Ok(Some((Frame::Boolean(true), next))),
            b"f" => Ok(Some((Frame::Boolean(false), next))),
            _ => Err(DecodeError::InvalidBoolean),
        },
        b',' => Ok(Some((Frame::Double(double(head)?), next))),
        b'(' => Ok(Some((Frame::BigNumber(Bytes::copy_from_slice(head)), next))),
        b'!' => match payload(buf, head, next)? {
            Payload::Null => Err(DecodeError::InvalidLength),
            Payload::Incomplete => Ok(None),
            Payload::Data(data, end) => Ok(Some((Frame::BulkError(data), end))),
        },
        b'=' => match payload(buf, head, next)? {
            Payload::Null => Err(DecodeError::InvalidLength),
            Payload::Incomplete => Ok(None),
            Payload::Data(data, end) => Ok(Some((verbatim(&data)?, end))),
        },
        b'%' => match elements(buf, head, next, depth, 2)? {
            Some(Some((items, end))) => Ok(Some((Frame::Map(pairs(items)), end))),
            Some(None) => Err(DecodeError::InvalidLength),
            None => Ok(None),
        },
        b'~' => match elements(buf, head, next, depth, 1)? {
            Some(Some((items, end))) => Ok(Some((Frame::Set(items), end))),
            Some(None) => Err(DecodeError::InvalidLength),
            None => Ok(None),
        },
        b'>' => match elements(buf, head, next, depth, 1)? {
            Some(Some((items, end))) => Ok(Some((Frame::Push(items), end))),
            Some(None) => Err(DecodeError::InvalidLength),
            None => Ok(None),
        },
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

enum Payload {
    Null,
    Incomplete,
    Data(Bytes, usize),
}

fn payload(buf: &[u8], head: &[u8], start: usize) -> Result<Payload, DecodeError> {
    let Some(len) = length(head, MAX_BULK_LEN)? else {
        return Ok(Payload::Null);
    };
    let end = start + len;
    let Some(tail) = buf.get(end..end + 2) else {
        return Ok(Payload::Incomplete);
    };
    if tail != b"\r\n" {
        return Err(DecodeError::MissingTerminator);
    }
    Ok(Payload::Data(
        Bytes::copy_from_slice(&buf[start..end]),
        end + 2,
    ))
}

type Elements = Result<Option<Option<(Vec<Frame>, usize)>>, DecodeError>;

fn elements(buf: &[u8], head: &[u8], start: usize, depth: usize, per_entry: usize) -> Elements {
    let Some(len) = length(head, MAX_ARRAY_LEN)? else {
        return Ok(Some(None));
    };
    let mut items = Vec::new();
    let mut pos = start;
    for _ in 0..len * per_entry {
        let Some((item, next)) = parse(buf, pos, depth + 1)? else {
            return Ok(None);
        };
        items.push(item);
        pos = next;
    }
    Ok(Some(Some((items, pos))))
}

fn pairs(items: Vec<Frame>) -> Vec<(Frame, Frame)> {
    let mut pairs = Vec::with_capacity(items.len() / 2);
    let mut items = items.into_iter();
    while let (Some(key), Some(value)) = (items.next(), items.next()) {
        pairs.push((key, value));
    }
    pairs
}

fn double(text: &[u8]) -> Result<f64, DecodeError> {
    let text = std::str::from_utf8(text).map_err(|_| DecodeError::InvalidNumber)?;
    match text {
        "inf" => Ok(f64::INFINITY),
        "-inf" => Ok(f64::NEG_INFINITY),
        "nan" => Ok(f64::NAN),
        _ => text.parse().map_err(|_| DecodeError::InvalidNumber),
    }
}

fn verbatim(data: &Bytes) -> Result<Frame, DecodeError> {
    if data.len() < 4 || data[3] != b':' {
        return Err(DecodeError::InvalidVerbatim);
    }
    Ok(Frame::VerbatimString {
        format: [data[0], data[1], data[2]],
        data: data.slice(4..),
    })
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
        assert_eq!(decode(b"|1\r\n"), Err(DecodeError::InvalidType(b'|')));
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

    #[test]
    fn resp3_scalars() {
        assert_eq!(ok(b"_\r\n"), (Frame::Null, 3));
        assert_eq!(ok(b"#t\r\n"), (Frame::Boolean(true), 4));
        assert_eq!(ok(b"#f\r\n"), (Frame::Boolean(false), 4));
        assert_eq!(ok(b",1.5\r\n"), (Frame::Double(1.5), 6));
        assert_eq!(ok(b",-inf\r\n").0, Frame::Double(f64::NEG_INFINITY));
        assert_eq!(ok(b",inf\r\n").0, Frame::Double(f64::INFINITY));
        assert!(matches!(ok(b",nan\r\n").0, Frame::Double(n) if n.is_nan()));
        let big = Bytes::from("3492890328409238509324850943850943825024385");
        assert_eq!(
            ok(b"(3492890328409238509324850943850943825024385\r\n").0,
            Frame::BigNumber(big)
        );
    }

    #[test]
    fn resp3_strings() {
        assert_eq!(
            ok(b"!4\r\nBOOM\r\n").0,
            Frame::BulkError(Bytes::from("BOOM"))
        );
        assert_eq!(
            ok(b"=7\r\ntxt:abc\r\n").0,
            Frame::VerbatimString {
                format: *b"txt",
                data: Bytes::from("abc")
            }
        );
    }

    #[test]
    fn resp3_aggregates() {
        assert_eq!(
            ok(b"%1\r\n+k\r\n:1\r\n").0,
            Frame::Map(vec![(
                Frame::SimpleString(Bytes::from("k")),
                Frame::Integer(1)
            )])
        );
        assert_eq!(
            ok(b"~2\r\n:1\r\n:2\r\n").0,
            Frame::Set(vec![Frame::Integer(1), Frame::Integer(2)])
        );
        assert_eq!(ok(b">1\r\n:1\r\n").0, Frame::Push(vec![Frame::Integer(1)]));
    }

    #[test]
    fn resp3_prefixes_are_incomplete() {
        let input = b"%2\r\n+a\r\n,1.5\r\n+b\r\n=7\r\ntxt:abc\r\n";
        for end in 0..input.len() {
            assert_eq!(decode(&input[..end]), Ok(None), "prefix of {end} bytes");
        }
    }

    #[test]
    fn rejects_bad_resp3() {
        assert_eq!(decode(b"#x\r\n"), Err(DecodeError::InvalidBoolean));
        assert_eq!(decode(b",abc\r\n"), Err(DecodeError::InvalidNumber));
        assert_eq!(decode(b"=3\r\nabc\r\n"), Err(DecodeError::InvalidVerbatim));
        assert_eq!(decode(b"%-1\r\n"), Err(DecodeError::InvalidLength));
    }
}
