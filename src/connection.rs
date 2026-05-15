//! One client connection: read, parse, execute, write.

use std::io;

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::resp::{self, Frame};
use crate::session::Session;
use crate::{command, request};

// shortcut: Redis closes a client at 1 GiB of unparsed input, so do we
const MAX_INPUT_LEN: usize = 1 << 30;

pub(crate) async fn run(mut stream: TcpStream, id: i64) -> io::Result<()> {
    let mut input = BytesMut::with_capacity(4096);
    let mut output = BytesMut::new();
    let peer = stream.peer_addr()?.to_string();
    let local = stream.local_addr()?.to_string();
    let mut session = Session::new(id, peer, local);
    loop {
        if stream.read_buf(&mut input).await? == 0 {
            return Ok(());
        }
        let mut close = input.len() > MAX_INPUT_LEN;
        while !close {
            match request::parse(&input) {
                Ok(Some((argv, used))) => {
                    input.advance(used);
                    if !argv.is_empty() {
                        let reply = command::dispatch(&mut session, &argv);
                        resp::encode(&reply, session.protocol, &mut output);
                        close = session.closing;
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
