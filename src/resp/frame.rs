//! The `Frame` and `Protocol` types.

use bytes::Bytes;

/// The RESP version a client speaks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Protocol {
    /// RESP2, the default.
    #[default]
    Resp2,
    /// RESP3, chosen with `HELLO 3`.
    Resp3,
}

/// One RESP value on the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// `+text`
    SimpleString(Bytes),
    /// `-text`
    Error(Bytes),
    /// `:n`
    Integer(i64),
    /// `$len` and a payload.
    BulkString(Bytes),
    /// `$-1`
    NullBulk,
    /// `*len` and the elements.
    Array(Vec<Frame>),
    /// `*-1`
    NullArray,
    /// `_`
    Null,
    /// `#t` or `#f`
    Boolean(bool),
    /// `,number`
    Double(f64),
    /// `(digits)`
    BigNumber(Bytes),
    /// `!len` and an error text.
    BulkError(Bytes),
    /// `=len` and a three-byte format, a colon and the text.
    VerbatimString {
        /// The format tag, such as `txt`.
        format: [u8; 3],
        /// The text.
        data: Bytes,
    },
    /// `%len` and the key-value pairs.
    Map(Vec<(Frame, Frame)>),
    /// `~len` and the elements.
    Set(Vec<Frame>),
    /// `>len` and the elements.
    Push(Vec<Frame>),
}

impl Frame {
    /// A bulk string frame.
    #[must_use]
    pub fn bulk(data: impl Into<Bytes>) -> Self {
        Self::BulkString(data.into())
    }

    /// A simple string frame.
    #[must_use]
    pub fn simple(text: impl Into<Bytes>) -> Self {
        Self::SimpleString(text.into())
    }

    /// The `+OK` reply.
    #[must_use]
    pub fn ok() -> Self {
        Self::simple("OK")
    }
}
