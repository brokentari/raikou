//! Starts the server from library code and talks to it over TCP.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

#[tokio::test]
async fn a_program_can_start_and_stop_a_server() {
    let listener = raikou::server::bind("127.0.0.1", 0).await.expect("bind");
    let address = listener.local_addr().expect("address");
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(raikou::server::serve_until(listener, async {
        let _ = stopped.await;
    }));

    let mut client = TcpStream::connect(address).await.expect("connect");
    client
        .write_all(b"*1\r\n$4\r\nPING\r\n")
        .await
        .expect("send");
    let mut reply = [0; 7];
    client.read_exact(&mut reply).await.expect("reply");
    assert_eq!(&reply, b"+PONG\r\n");

    stop.send(()).expect("server is running");
    server.await.expect("task").expect("clean stop");
}
