//! The values a key can hold.

use bytes::Bytes;

/// The value stored under a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    Str(Bytes),
}
