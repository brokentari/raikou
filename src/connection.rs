use std::io;
use std::sync::{Arc, Mutex};

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::resp::{self, Frame};
use crate::session::Session;
use crate::state::State;
use crate::{clock, command, request};

// shortcut: Redis closes a client at 1 GiB of unparsed input, so do we
const MAX_INPUT_LEN: usize = 1 << 30;

const FLUSH_THRESHOLD: usize = 1 << 20;

pub(crate) async fn run(
    mut stream: TcpStream,
    id: i64,
    shared: Arc<Mutex<State>>,
) -> io::Result<()> {
    let mut input = BytesMut::with_capacity(4096);
    let mut output = BytesMut::new();
    // replies are written once per batch, so Nagle's delay only adds latency
    stream.set_nodelay(true)?;
    let peer = stream.peer_addr()?.to_string();
    let local = stream.local_addr()?.to_string();
    let mut session = Session::new(id, peer, local);
    let mut parser = request::Parser::default();
    loop {
        if stream.read_buf(&mut input).await? == 0 {
            return Ok(());
        }
        let mut close = input.len() > MAX_INPUT_LEN;
        while !close {
            match parser.parse(&input) {
                Ok(Some((argv, used))) => {
                    input.advance(used);
                    if !argv.is_empty() {
                        let reply = {
                            let mut state = shared.lock().expect("state mutex poisoned");
                            command::dispatch(&mut state, &mut session, clock::now_ms(), &argv)
                        };
                        resp::encode(&reply, session.protocol, &mut output);
                        close = session.closing;
                        if output.len() >= FLUSH_THRESHOLD {
                            stream.write_all(&output).await?;
                            output.clear();
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let text = format!("ERR Protocol error: {error}");
                    resp::encode(
                        &Frame::Error(Bytes::from(text)),
                        session.protocol,
                        &mut output,
                    );
                    close = true;
                }
            }
        }
        stream.write_all(&output).await?;
        output.clear();
        if close {
            return Ok(());
        }
    }
}
