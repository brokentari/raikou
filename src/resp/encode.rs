//! Frame encoding.

use bytes::{BufMut, BytesMut};

use crate::resp::Frame;

/// Appends the RESP2 wire form of `frame` to `out`.
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
        Frame::Array(items) => {
            line(out, b'*', items.len().to_string().as_bytes());
            for item in items {
                encode(item, out);
            }
        }
        Frame::NullArray => out.put_slice(b"*-1\r\n"),
        _ => unreachable!("RESP3 frames are not encoded yet"),
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
}
