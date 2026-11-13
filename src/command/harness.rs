use bytes::Bytes;

use crate::command;
use crate::resp::Frame;
use crate::session::Session;
use crate::state::State;

pub(crate) struct Harness {
    pub(crate) state: State,
    pub(crate) session: Session,
    pub(crate) now_ms: u64,
}

impl Harness {
    pub(crate) fn new() -> Self {
        Self {
            state: State::default(),
            session: Session::new(1, "127.0.0.1:1".to_owned(), "127.0.0.1:2".to_owned()),
            now_ms: 1_000_000,
        }
    }

    pub(crate) fn run(&mut self, parts: &[&str]) -> Frame {
        let argv: Vec<Bytes> = parts
            .iter()
            .map(|part| Bytes::copy_from_slice(part.as_bytes()))
            .collect();
        command::dispatch(&mut self.state, &mut self.session, self.now_ms, &argv)
    }
}

pub(crate) fn error(text: &str) -> Frame {
    Frame::Error(Bytes::copy_from_slice(text.as_bytes()))
}
