//! Frame encoding.

use bytes::{BufMut, BytesMut};

use crate::number;
use crate::resp::{Frame, Protocol};

/// Appends the wire form of `frame` for a `protocol` client to `out`.
pub fn encode(frame: &Frame, protocol: Protocol, out: &mut BytesMut) {
    let resp3 = protocol == Protocol::Resp3;
    match frame {
        Frame::SimpleString(text) => line(out, b'+', text),
        Frame::Integer(n) => line(out, b':', n.to_string().as_bytes()),
        Frame::Null | Frame::NullBulk | Frame::NullArray if resp3 => out.put_slice(b"_\r\n"),
        Frame::Null | Frame::NullBulk => out.put_slice(b"$-1\r\n"),
        Frame::NullArray => out.put_slice(b"*-1\r\n"),
        Frame::Boolean(value) if resp3 => {
            out.put_slice(if *value { b"#t\r\n" } else { b"#f\r\n" });
        }
        Frame::Boolean(value) => line(out, b':', if *value { b"1" } else { b"0" }),
        Frame::Double(value) if resp3 => line(out, b',', number::format_double(*value).as_bytes()),
        Frame::Double(value) => bulk(out, b'$', number::format_double(*value).as_bytes()),
        Frame::BigNumber(digits) if resp3 => line(out, b'(', digits),
        Frame::BigNumber(digits) => bulk(out, b'$', digits),
        Frame::BulkError(text) if resp3 => bulk(out, b'!', text),
        Frame::VerbatimString { format, data } if resp3 => {
            let mut body = BytesMut::with_capacity(data.len() + 4);
            body.put_slice(format);
            body.put_u8(b':');
            body.put_slice(data);
            bulk(out, b'=', &body);
        }
        Frame::Error(text) | Frame::BulkError(text) => line(out, b'-', text),
        Frame::BulkString(data) | Frame::VerbatimString { data, .. } => bulk(out, b'$', data),
        Frame::Map(pairs) => {
            let (prefix, count) = if resp3 {
                (b'%', pairs.len())
            } else {
                (b'*', pairs.len() * 2)
            };
            line(out, prefix, count.to_string().as_bytes());
            for (key, value) in pairs {
                encode(key, protocol, out);
                encode(value, protocol, out);
            }
        }
        Frame::Set(items) if resp3 => aggregate(out, b'~', items, protocol),
        Frame::Push(items) if resp3 => aggregate(out, b'>', items, protocol),
        Frame::Array(items) | Frame::Set(items) | Frame::Push(items) => {
            aggregate(out, b'*', items, protocol);
        }
    }
}

fn bulk(out: &mut BytesMut, prefix: u8, data: &[u8]) {
    line(out, prefix, data.len().to_string().as_bytes());
    out.put_slice(data);
    out.put_slice(b"\r\n");
}

fn aggregate(out: &mut BytesMut, prefix: u8, items: &[Frame], protocol: Protocol) {
    line(out, prefix, items.len().to_string().as_bytes());
    for item in items {
        encode(item, protocol, out);
    }
}

fn line(out: &mut BytesMut, prefix: u8, text: &[u8]) {
    out.put_u8(prefix);
    out.put_slice(text);
    out.put_slice(b"\r\n");
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    fn wire_as(frame: &Frame, protocol: Protocol) -> Vec<u8> {
        let mut out = BytesMut::new();
        encode(frame, protocol, &mut out);
        out.to_vec()
    }

    fn wire(frame: &Frame) -> Vec<u8> {
        wire_as(frame, Protocol::Resp2)
    }

    fn wire3(frame: &Frame) -> Vec<u8> {
        wire_as(frame, Protocol::Resp3)
    }

    #[test]
    fn simple_string() {
        assert_eq!(wire(&Frame::SimpleString(Bytes::from("OK"))), b"+OK\r\n");
    }

    #[test]
    fn error() {
        assert_eq!(
            wire(&Frame::Error(Bytes::from("ERR boom"))),
            b"-ERR boom\r\n"
        );
    }

    #[test]
    fn integer() {
        assert_eq!(wire(&Frame::Integer(-42)), b":-42\r\n");
    }

    #[test]
    fn bulk_string() {
        assert_eq!(
            wire(&Frame::BulkString(Bytes::from("hi\r\n"))),
            b"$4\r\nhi\r\n\r\n"
        );
        assert_eq!(wire(&Frame::BulkString(Bytes::new())), b"$0\r\n\r\n");
    }

    #[test]
    fn nulls() {
        assert_eq!(wire(&Frame::NullBulk), b"$-1\r\n");
        assert_eq!(wire(&Frame::NullArray), b"*-1\r\n");
    }

    #[test]
    fn nested_array() {
        let frame = Frame::Array(vec![Frame::Integer(1), Frame::Array(vec![Frame::NullBulk])]);
        assert_eq!(wire(&frame), b"*2\r\n:1\r\n*1\r\n$-1\r\n");
        assert_eq!(wire(&Frame::Array(vec![])), b"*0\r\n");
    }

    #[test]
    fn resp3_scalars() {
        assert_eq!(wire3(&Frame::Null), b"_\r\n");
        assert_eq!(wire3(&Frame::Boolean(true)), b"#t\r\n");
        assert_eq!(wire3(&Frame::Boolean(false)), b"#f\r\n");
        assert_eq!(wire3(&Frame::Double(1.5)), b",1.5\r\n");
        assert_eq!(wire3(&Frame::Double(f64::INFINITY)), b",inf\r\n");
        assert_eq!(wire3(&Frame::Double(f64::NEG_INFINITY)), b",-inf\r\n");
        assert_eq!(wire3(&Frame::Double(f64::NAN)), b",nan\r\n");
        assert_eq!(wire3(&Frame::BigNumber(Bytes::from("123"))), b"(123\r\n");
    }

    #[test]
    fn resp3_strings() {
        assert_eq!(
            wire3(&Frame::BulkError(Bytes::from("BOOM"))),
            b"!4\r\nBOOM\r\n"
        );
        let verbatim = Frame::VerbatimString {
            format: *b"txt",
            data: Bytes::from("abc"),
        };
        assert_eq!(wire3(&verbatim), b"=7\r\ntxt:abc\r\n");
    }

    #[test]
    fn resp3_aggregates() {
        let map = Frame::Map(vec![(Frame::Integer(1), Frame::Null)]);
        assert_eq!(wire3(&map), b"%1\r\n:1\r\n_\r\n");
        assert_eq!(wire3(&Frame::Set(vec![Frame::Integer(1)])), b"~1\r\n:1\r\n");
        assert_eq!(
            wire3(&Frame::Push(vec![Frame::Integer(1)])),
            b">1\r\n:1\r\n"
        );
    }

    #[test]
    fn resp3_encodes_both_nulls_as_underscore() {
        assert_eq!(wire3(&Frame::NullBulk), b"_\r\n");
        assert_eq!(wire3(&Frame::NullArray), b"_\r\n");
    }

    #[test]
    fn resp2_downgrades_scalars() {
        assert_eq!(wire(&Frame::Null), b"$-1\r\n");
        assert_eq!(wire(&Frame::Boolean(true)), b":1\r\n");
        assert_eq!(wire(&Frame::Boolean(false)), b":0\r\n");
        assert_eq!(wire(&Frame::Double(1.5)), b"$3\r\n1.5\r\n");
        assert_eq!(wire(&Frame::Double(f64::NEG_INFINITY)), b"$4\r\n-inf\r\n");
        assert_eq!(
            wire(&Frame::BigNumber(Bytes::from("123"))),
            b"$3\r\n123\r\n"
        );
    }

    #[test]
    fn resp2_downgrades_strings() {
        assert_eq!(wire(&Frame::BulkError(Bytes::from("BOOM"))), b"-BOOM\r\n");
        let verbatim = Frame::VerbatimString {
            format: *b"txt",
            data: Bytes::from("abc"),
        };
        assert_eq!(wire(&verbatim), b"$3\r\nabc\r\n");
    }

    #[test]
    fn resp2_downgrades_aggregates() {
        let map = Frame::Map(vec![(Frame::Integer(1), Frame::Null)]);
        assert_eq!(wire(&map), b"*2\r\n:1\r\n$-1\r\n");
        assert_eq!(wire(&Frame::Set(vec![Frame::Integer(1)])), b"*1\r\n:1\r\n");
        assert_eq!(wire(&Frame::Push(vec![Frame::Integer(1)])), b"*1\r\n:1\r\n");
    }

    #[test]
    fn downgrade_reaches_nested_frames() {
        let frame = Frame::Array(vec![Frame::Map(vec![(Frame::Boolean(true), Frame::Null)])]);
        assert_eq!(wire(&frame), b"*1\r\n*2\r\n:1\r\n$-1\r\n");
    }
}
