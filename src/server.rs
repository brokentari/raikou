//! The TCP accept loop.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::{TcpListener, TcpSocket};

use crate::state::State;
use crate::{clock, connection};

const SWEEP_INTERVAL: Duration = Duration::from_millis(100);
const SWEEP_BATCH: usize = 1000;
const LISTEN_BACKLOG: u32 = 1024;
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// Listens on `host:port` with a backlog sized for bursts of hundreds of clients.
pub async fn bind(host: &str, port: u16) -> io::Result<TcpListener> {
    let address = tokio::net::lookup_host((host, port))
        .await?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no address to listen on"))?;
    let socket = if address.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    socket.set_reuseaddr(true)?;
    socket.bind(address)?;
    socket.listen(LISTEN_BACKLOG)
}

/// Accepts clients on `listener` and serves each on its own task.
pub async fn serve(listener: TcpListener) -> io::Result<()> {
    let shared = Arc::new(Mutex::new(State::default()));
    let sweeper = tokio::spawn(sweep(Arc::clone(&shared)));
    let result = accept_loop(&listener, &shared).await;
    sweeper.abort();
    result
}

async fn accept_loop(listener: &TcpListener, shared: &Arc<Mutex<State>>) -> io::Result<()> {
    let mut next_id = 1;
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            // shortcut: accept errors (file limit, aborted connections) are retried after a pause
            tokio::time::sleep(ACCEPT_RETRY).await;
            continue;
        };
        let id = next_id;
        next_id += 1;
        let shared = Arc::clone(shared);
        tokio::spawn(async move {
            // shortcut: a connection that ends with an I/O error has nobody left to tell
            let _ = connection::run(stream, id, shared).await;
        });
    }
}

async fn sweep(shared: Arc<Mutex<State>>) {
    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
    loop {
        ticker.tick().await;
        loop {
            let purged = {
                let mut state = shared.lock().expect("state mutex poisoned");
                state.store.purge_expired(clock::now_ms(), SWEEP_BATCH)
            };
            if purged < SWEEP_BATCH {
                break;
            }
            tokio::task::yield_now().await;
        }
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
    async fn sweeps_expired_keys_in_the_background() {
        let mut client = start().await;
        client
            .write_all(b"SET gone v PX 50\r\nSET stays v PX 60000\r\nSET forever v\r\n")
            .await
            .expect("write");
        let mut reply = [0; 15];
        client.read_exact(&mut reply).await.expect("read");
        tokio::time::sleep(Duration::from_millis(400)).await;
        client.write_all(b"DBSIZE\r\n").await.expect("write");
        let mut size = [0; 4];
        client.read_exact(&mut size).await.expect("read");
        assert_eq!(&size, b":2\r\n");
    }

    #[tokio::test]
    async fn large_pipelined_replies_arrive_complete_and_in_order() {
        let mut client = start().await;
        let value = "x".repeat(600_000);
        let mut request =
            format!("*3\r\n$3\r\nSET\r\n$3\r\nbig\r\n$600000\r\n{value}\r\n").into_bytes();
        for _ in 0..6 {
            request.extend_from_slice(b"GET big\r\n");
        }
        request.extend_from_slice(b"PING\r\n");
        client.write_all(&request).await.expect("write");
        let mut expected = b"+OK\r\n".to_vec();
        for _ in 0..6 {
            expected.extend_from_slice(format!("$600000\r\n{value}\r\n").as_bytes());
        }
        expected.extend_from_slice(b"+PONG\r\n");
        let mut reply = vec![0; expected.len()];
        client.read_exact(&mut reply).await.expect("read");
        assert!(reply == expected, "replies differ");
    }

    #[tokio::test]
    async fn a_burst_of_connections_does_not_wait_for_syn_retries() {
        let listener = bind("127.0.0.1", 0).await.expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(serve(listener));
        let started = tokio::time::Instant::now();
        let mut clients = Vec::new();
        for _ in 0..400 {
            clients.push(TcpStream::connect(address).await.expect("connect"));
        }
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn closes_after_a_protocol_error() {
        let reply = exchange(b"*x\r\nPING\r\n").await;
        assert_eq!(reply, b"-ERR Protocol error: invalid multibulk length\r\n");
    }
}
