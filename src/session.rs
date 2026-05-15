//! Per-client state.

use bytes::Bytes;

use crate::resp::Protocol;

/// The number of databases a client can select.
pub(crate) const DATABASES: usize = 16;

/// The server's state for one client.
#[derive(Debug)]
pub(crate) struct Session {
    pub(crate) id: i64,
    pub(crate) peer: String,
    pub(crate) local: String,
    pub(crate) protocol: Protocol,
    pub(crate) name: Option<Bytes>,
    pub(crate) db: usize,
    pub(crate) lib_name: Bytes,
    pub(crate) lib_ver: Bytes,
    pub(crate) closing: bool,
}

impl Session {
    /// Creates the session of a client that has just connected.
    pub(crate) fn new(id: i64, peer: String, local: String) -> Self {
        Self {
            id,
            peer,
            local,
            protocol: Protocol::Resp2,
            name: None,
            db: 0,
            lib_name: Bytes::new(),
            lib_ver: Bytes::new(),
            closing: false,
        }
    }

    /// Returns the session to the state of a new connection, as `RESET` does.
    pub(crate) fn reset(&mut self) {
        self.protocol = Protocol::Resp2;
        self.name = None;
        self.db = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_keeps_identity_and_library_info() {
        let mut session = Session::new(4, "peer".to_owned(), "local".to_owned());
        session.protocol = Protocol::Resp3;
        session.name = Some(Bytes::from("n"));
        session.db = 3;
        session.lib_name = Bytes::from("lib");
        session.reset();
        assert_eq!(
            (session.protocol, session.name.clone(), session.db),
            (Protocol::Resp2, None, 0)
        );
        assert_eq!((session.id, session.lib_name.as_ref()), (4, &b"lib"[..]));
    }
}
