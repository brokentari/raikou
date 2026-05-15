//! The TCP accept loop.

use std::io;
use std::sync::{Arc, Mutex};

use tokio::net::TcpListener;

use crate::connection;
use crate::state::State;

/// Accepts clients on `listener` and serves each on its own task.
pub async fn serve(listener: TcpListener) -> io::Result<()> {
    let shared = Arc::new(Mutex::new(State::default()));
    let mut next_id = 1;
    loop {
        let (stream, _) = listener.accept().await?;
        let id = next_id;
        let shared = Arc::clone(&shared);
        next_id += 1;
        tokio::spawn(async move {
            // shortcut: a connection that ends with an I/O error has nobody left to tell
            let _ = connection::run(stream, id, shared).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;

    async fn start() -> TcpStream {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local address");
        tokio::spawn(serve(listener));
        TcpStream::connect(address).await.expect("connect")
    }

    async fn exchange(input: &[u8]) -> Vec<u8> {
        let mut client = start().await;
        client.write_all(input).await.expect("write");
        client.shutdown().await.expect("shutdown");
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).await.expect("read");
        reply
    }

    #[tokio::test]
    async fn answers_ping_and_echo() {
        assert_eq!(exchange(b"*1\r\n$4\r\nPING\r\n").await, b"+PONG\r\n");
        assert_eq!(
            exchange(b"*2\r\n$4\r\nECHO\r\n$2\r\nhi\r\n").await,
            b"$2\r\nhi\r\n"
        );
        assert_eq!(exchange(b"ping hello\r\n").await, b"$5\r\nhello\r\n");
    }

    #[tokio::test]
    async fn answers_pipelined_requests_in_order() {
        let reply = exchange(b"PING\r\n\r\nECHO a\r\n*0\r\nPING\r\n").await;
        assert_eq!(reply, b"+PONG\r\n$1\r\na\r\n+PONG\r\n");
    }

    #[tokio::test]
    async fn handles_a_request_split_across_writes() {
        let mut client = start().await;
        client.write_all(b"*2\r\n$4\r\nEC").await.expect("write");
        tokio::task::yield_now().await;
        client
            .write_all(b"HO\r\n$2\r\nhi\r\n")
            .await
            .expect("write");
        let mut reply = [0; 8];
        client.read_exact(&mut reply).await.expect("read");
        assert_eq!(&reply, b"$2\r\nhi\r\n");
    }

    #[tokio::test]
    async fn closes_after_a_protocol_error() {
        let reply = exchange(b"*x\r\nPING\r\n").await;
        assert_eq!(reply, b"-ERR Protocol error: invalid multibulk length\r\n");
    }
}
