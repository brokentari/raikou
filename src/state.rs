use crate::store::Store;

#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) store: Store,
}
