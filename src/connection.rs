//! One client connection: read, parse, execute, write.

use std::io;

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::request;
use crate::resp::{self, Frame, Protocol};

// shortcut: Redis closes a client at 1 GiB of unparsed input, so do we
const MAX_INPUT_LEN: usize = 1 << 30;

pub(crate) async fn run(mut stream: TcpStream) -> io::Result<()> {
    let mut input = BytesMut::with_capacity(4096);
    let mut output = BytesMut::new();
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
                        resp::encode(&execute(&argv), Protocol::Resp2, &mut output);
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let text = format!("ERR Protocol error: {error}");
                    resp::encode(
                        &Frame::Error(Bytes::from(text)),
                        Protocol::Resp2,
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

fn execute(argv: &[Bytes]) -> Frame {
    let name = argv[0].to_ascii_uppercase();
    match (name.as_slice(), &argv[1..]) {
        (b"PING", []) => Frame::SimpleString(Bytes::from("PONG")),
        (b"PING" | b"ECHO", [message]) => Frame::BulkString(message.clone()),
        (b"PING" | b"ECHO", _) => {
            let name = String::from_utf8_lossy(&name).to_lowercase();
            Frame::Error(Bytes::from(format!(
                "ERR wrong number of arguments for '{name}' command"
            )))
        }
        _ => Frame::Error(Bytes::from("ERR unknown command")),
    }
}
