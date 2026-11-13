use std::collections::{HashMap, HashSet, VecDeque};

use bytes::Bytes;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    Str(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<Bytes, Bytes>),
    Set(HashSet<Bytes>),
}

impl Value {
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Self::Str(_) => "string",
            Self::List(_) => "list",
            Self::Hash(_) => "hash",
            Self::Set(_) => "set",
        }
    }
}
