//! Per-client state.

use bytes::Bytes;

use crate::resp::Protocol;

/// The server's state for one client.
#[derive(Debug)]
pub(crate) struct Session {
    pub(crate) id: i64,
    pub(crate) protocol: Protocol,
    pub(crate) name: Option<Bytes>,
}

impl Session {
    /// Creates the session of a client that has just connected.
    pub(crate) fn new(id: i64) -> Self {
        Self {
            id,
            protocol: Protocol::Resp2,
            name: None,
        }
    }
}
