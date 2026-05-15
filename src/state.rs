//! The state shared by all clients.

use crate::store::Store;

/// Everything the server keeps outside a single session.
#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) store: Store,
}
