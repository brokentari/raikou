//! The values a key can hold.

use bytes::Bytes;

/// The value stored under a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    Str(Bytes),
}

impl Value {
    /// The name `TYPE` reports for this kind of value.
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Self::Str(_) => "string",
        }
    }
}
