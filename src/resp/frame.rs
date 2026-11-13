use bytes::Bytes;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Protocol {
    #[default]
    Resp2,
    Resp3,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    SimpleString(Bytes),
    Error(Bytes),
    Integer(i64),
    BulkString(Bytes),
    NullBulk,
    Array(Vec<Frame>),
    NullArray,
    Null,
    Boolean(bool),
    Double(f64),
    /// `(digits)`
    BigNumber(Bytes),
    BulkError(Bytes),
    VerbatimString {
        format: [u8; 3],
        data: Bytes,
    },
    Map(Vec<(Frame, Frame)>),
    Set(Vec<Frame>),
    Push(Vec<Frame>),
}

impl Frame {
    #[must_use]
    pub fn bulk(data: impl Into<Bytes>) -> Self {
        Self::BulkString(data.into())
    }

    #[must_use]
    pub fn simple(text: impl Into<Bytes>) -> Self {
        Self::SimpleString(text.into())
    }

    #[must_use]
    pub fn ok() -> Self {
        Self::simple("OK")
    }
}
