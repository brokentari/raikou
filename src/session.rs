//! Per-client state.

use crate::resp::Protocol;

/// The server's state for one client.
#[derive(Debug, Default)]
pub(crate) struct Session {
    pub(crate) protocol: Protocol,
}

impl Session {
    /// Creates the session of a client that has just connected.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }
}
