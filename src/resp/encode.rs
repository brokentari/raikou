//! Frame encoding.

use bytes::{BufMut, BytesMut};

use crate::resp::Frame;

/// Appends the wire form of `frame` to `out`.
pub fn encode(frame: &Frame, out: &mut BytesMut) {
    match frame {
        Frame::SimpleString(text) => line(out, b'+', text),
        Frame::Error(text) => line(out, b'-', text),
        Frame::Integer(n) => line(out, b':', n.to_string().as_bytes()),
        Frame::BulkString(data) => {
            line(out, b'$', data.len().to_string().as_bytes());
            out.put_slice(data);
            out.put_slice(b"\r\n");
        }
        Frame::NullBulk => out.put_slice(b"$-1\r\n"),
        Frame::Array(items) => aggregate(out, b'*', items),
        Frame::NullArray => out.put_slice(b"*-1\r\n"),
        Frame::Null => out.put_slice(b"_\r\n"),
        Frame::Boolean(value) => out.put_slice(if *value { b"#t\r\n" } else { b"#f\r\n" }),
        Frame::Double(value) => line(out, b',', double_text(*value).as_bytes()),
        Frame::BigNumber(digits) => line(out, b'(', digits),
        Frame::BulkError(text) => {
            line(out, b'!', text.len().to_string().as_bytes());
            out.put_slice(text);
            out.put_slice(b"\r\n");
        }
        Frame::VerbatimString { format, data } => {
            line(out, b'=', (data.len() + 4).to_string().as_bytes());
            out.put_slice(format);
            out.put_u8(b':');
            out.put_slice(data);
            out.put_slice(b"\r\n");
        }
        Frame::Map(pairs) => {
            line(out, b'%', pairs.len().to_string().as_bytes());
            for (key, value) in pairs {
                encode(key, out);
                encode(value, out);
            }
        }
        Frame::Set(items) => aggregate(out, b'~', items),
        Frame::Push(items) => aggregate(out, b'>', items),
    }
}

fn aggregate(out: &mut BytesMut, prefix: u8, items: &[Frame]) {
    line(out, prefix, items.len().to_string().as_bytes());
    for item in items {
        encode(item, out);
    }
}

fn double_text(value: f64) -> String {
    if value.is_nan() {
        "nan".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "inf" } else { "-inf" }.to_owned()
    } else {
        value.to_string()
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

    fn wire(frame: &Frame) -> Vec<u8> {
        let mut out = BytesMut::new();
        encode(frame, &mut out);
        out.to_vec()
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
        assert_eq!(wire(&Frame::Null), b"_\r\n");
        assert_eq!(wire(&Frame::Boolean(true)), b"#t\r\n");
        assert_eq!(wire(&Frame::Boolean(false)), b"#f\r\n");
        assert_eq!(wire(&Frame::Double(1.5)), b",1.5\r\n");
        assert_eq!(wire(&Frame::Double(f64::INFINITY)), b",inf\r\n");
        assert_eq!(wire(&Frame::Double(f64::NEG_INFINITY)), b",-inf\r\n");
        assert_eq!(wire(&Frame::Double(f64::NAN)), b",nan\r\n");
        assert_eq!(wire(&Frame::BigNumber(Bytes::from("123"))), b"(123\r\n");
    }

    #[test]
    fn resp3_strings() {
        assert_eq!(
            wire(&Frame::BulkError(Bytes::from("BOOM"))),
            b"!4\r\nBOOM\r\n"
        );
        let verbatim = Frame::VerbatimString {
            format: *b"txt",
            data: Bytes::from("abc"),
        };
        assert_eq!(wire(&verbatim), b"=7\r\ntxt:abc\r\n");
    }

    #[test]
    fn resp3_aggregates() {
        let map = Frame::Map(vec![(Frame::Integer(1), Frame::Null)]);
        assert_eq!(wire(&map), b"%1\r\n:1\r\n_\r\n");
        assert_eq!(wire(&Frame::Set(vec![Frame::Integer(1)])), b"~1\r\n:1\r\n");
        assert_eq!(wire(&Frame::Push(vec![Frame::Integer(1)])), b">1\r\n:1\r\n");
    }
}
