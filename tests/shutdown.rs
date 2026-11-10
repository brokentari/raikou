//! Starts the real binary, checks its startup message, and stops it with SIGTERM.

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    listener.local_addr().expect("address").port()
}

#[test]
fn sigterm_stops_the_server_cleanly_and_prints_what_it_did() {
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_raikou"))
        .args(["--port", &port.to_string()])
        .stdout(Stdio::piped())
        .spawn()
        .expect("start raikou");
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let client = (0..100)
        .find_map(|_| {
            TcpStream::connect(address).ok().or_else(|| {
                thread::sleep(Duration::from_millis(50));
                None
            })
        })
        .expect("raikou listens");

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("run kill");
    assert!(killed.success());

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "raikou ignored SIGTERM while a client was connected"
        );
        thread::sleep(Duration::from_millis(20));
    };
    drop(client);
    assert_eq!(status.code(), Some(0), "{status:?}");

    let mut output = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut output)
        .expect("read");
    assert!(output.starts_with("Raikou "), "{output}");
    assert!(
        output.contains(&format!("Listening on 127.0.0.1:{port}")),
        "{output}"
    );
    assert!(
        output.contains("Received SIGTERM, shutting down"),
        "{output}"
    );
    assert!(output.trim_end().ends_with("Bye"), "{output}");
}
