//! Property tests for the RESP codec.

use bytes::{Bytes, BytesMut};
use proptest::prelude::*;
use raikou::resp::{Frame, Protocol, decode, encode};

fn line_text() -> impl Strategy<Value = Bytes> {
    proptest::collection::vec(
        any::<u8>().prop_filter("no CR or LF", |b| *b != b'\r' && *b != b'\n'),
        0..16,
    )
    .prop_map(Bytes::from)
}

fn payload() -> impl Strategy<Value = Bytes> {
    proptest::collection::vec(any::<u8>(), 0..32).prop_map(Bytes::from)
}

fn leaf() -> impl Strategy<Value = Frame> {
    prop_oneof![
        line_text().prop_map(Frame::SimpleString),
        line_text().prop_map(Frame::Error),
        any::<i64>().prop_map(Frame::Integer),
        payload().prop_map(Frame::BulkString),
        Just(Frame::Null),
        any::<bool>().prop_map(Frame::Boolean),
        any::<f64>()
            .prop_filter("not NaN", |n| !n.is_nan())
            .prop_map(Frame::Double),
        "-?[0-9]{1,30}".prop_map(|digits| Frame::BigNumber(Bytes::from(digits))),
        payload().prop_map(Frame::BulkError),
        (any::<[u8; 3]>(), payload())
            .prop_map(|(format, data)| Frame::VerbatimString { format, data }),
    ]
}

fn frame() -> impl Strategy<Value = Frame> {
    leaf().prop_recursive(4, 64, 6, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Frame::Array),
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Frame::Set),
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Frame::Push),
            proptest::collection::vec((inner.clone(), inner), 0..4).prop_map(Frame::Map),
        ]
    })
}

proptest! {
    #[test]
    fn resp3_round_trip(frame in frame()) {
        let mut wire = BytesMut::new();
        encode(&frame, Protocol::Resp3, &mut wire);
        let decoded = decode(&wire).expect("encoded frames decode");
        prop_assert_eq!(decoded, Some((frame, wire.len())));
    }

    #[test]
    fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = decode(&bytes);
    }

    #[test]
    fn decode_never_panics_on_frame_like_bytes(text in "[*$+:_#,%~>=!(\\-0-9a-z\r\n]{0,64}") {
        let _ = decode(text.as_bytes());
    }
}
