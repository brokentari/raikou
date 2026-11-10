//! Throughput and latency benchmark for the server: run it with `cargo bench --bench perf`.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use raikou::resp::{self, Frame};

const KEYS: usize = 100_000;
const IDLE_CONNECTIONS: usize = 500;
const LARGE_VALUE: usize = 1 << 20;

struct Settings {
    ops: usize,
    clients: usize,
    pipeline: usize,
    min_ops: f64,
}

fn env_number(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(default)
}

fn seconds(duration: Duration) -> f64 {
    duration.as_secs_f64()
}

fn as_float(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

struct Server {
    child: Child,
    address: SocketAddr,
}

impl Server {
    fn start() -> Self {
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .and_then(|listener| listener.local_addr())
            .map(|address| address.port())
            .expect("free port");
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let child = Command::new(env!("CARGO_BIN_EXE_raikou"))
            .args(["--port", &port.to_string()])
            .stdin(Stdio::null())
            .spawn()
            .expect("start raikou");
        let server = Self { child, address };
        for _ in 0..100 {
            if TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok() {
                return server;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("raikou did not start");
    }

    fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect(self.address).expect("connect");
        stream.set_nodelay(true).expect("nodelay");
        stream
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn command(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", parts.len()).into_bytes();
    for part in parts {
        out.extend_from_slice(format!("${}\r\n", part.len()).as_bytes());
        out.extend_from_slice(part);
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// Sends `request` and reads `replies` complete frames; returns how many were errors.
fn exchange(stream: &mut TcpStream, request: &[u8], replies: usize) -> usize {
    stream.write_all(request).expect("write");
    let mut buffer = Vec::new();
    let mut chunk = vec![0; 1 << 16];
    let (mut seen, mut errors, mut offset) = (0, 0, 0);
    while seen < replies {
        if let Some((frame, used)) = resp::decode(&buffer[offset..]).expect("valid reply") {
            offset += used;
            seen += 1;
            errors += usize::from(matches!(frame, Frame::Error(_)));
        } else {
            let count = stream.read(&mut chunk).expect("read");
            assert!(count > 0, "server closed the connection");
            buffer.extend_from_slice(&chunk[..count]);
        }
    }
    errors
}

struct Workload {
    name: &'static str,
    preload: Option<fn(usize) -> Vec<u8>>,
    request: fn(usize) -> Vec<u8>,
}

fn key(prefix: &str, index: usize, modulo: usize) -> Vec<u8> {
    format!("{prefix}:{}", index % modulo).into_bytes()
}

const VALUE: &[u8] = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "PING",
            preload: None,
            request: |_| command(&[b"PING"]),
        },
        Workload {
            name: "SET 64B",
            preload: None,
            request: |i| command(&[b"SET", &key("key", i, KEYS), VALUE]),
        },
        Workload {
            name: "GET",
            preload: Some(|i| command(&[b"SET", &key("key", i, KEYS), VALUE])),
            request: |i| command(&[b"GET", &key("key", i, KEYS)]),
        },
        Workload {
            name: "INCR",
            preload: None,
            request: |i| command(&[b"INCR", &key("counter", i, 1000)]),
        },
        Workload {
            name: "LPUSH",
            preload: None,
            request: |i| command(&[b"LPUSH", &key("list", i, 100), VALUE]),
        },
        Workload {
            name: "LRANGE 100",
            preload: Some(|i| command(&[b"RPUSH", b"long", &i.to_string().into_bytes()])),
            request: |_| command(&[b"LRANGE", b"long", b"0", b"99"]),
        },
        Workload {
            name: "HSET",
            preload: None,
            request: |i| command(&[b"HSET", &key("hash", i, 100), &key("field", i, 1000), VALUE]),
        },
        Workload {
            name: "SADD",
            preload: None,
            request: |i| command(&[b"SADD", &key("set", i, 100), &i.to_string().into_bytes()]),
        },
        Workload {
            name: "MSET x10",
            preload: None,
            request: |i| {
                let keys: Vec<Vec<u8>> = (0..10).map(|n| key("multi", i * 10 + n, KEYS)).collect();
                let mut parts: Vec<&[u8]> = vec![b"MSET"];
                for key in &keys {
                    parts.push(key);
                    parts.push(VALUE);
                }
                command(&parts)
            },
        },
    ]
}

struct Outcome {
    name: String,
    ops: usize,
    rate: f64,
    p50: Duration,
    p99: Duration,
}

fn run_workload(server: &Server, workload: &Workload, settings: &Settings) -> Outcome {
    if let Some(preload) = workload.preload {
        let mut stream = server.connect();
        let count = if workload.name == "LRANGE 100" {
            1000
        } else {
            KEYS
        };
        for start in (0..count).step_by(1000) {
            let batch: Vec<u8> = (start..(start + 1000).min(count))
                .flat_map(preload)
                .collect();
            let replies = (start + 1000).min(count) - start;
            assert_eq!(exchange(&mut stream, &batch, replies), 0, "preload failed");
        }
    }
    let per_client = settings.ops / settings.clients;
    let barrier = Arc::new(Barrier::new(settings.clients + 1));
    let pipeline = settings.pipeline;
    let handles: Vec<_> = (0..settings.clients)
        .map(|client| {
            let mut stream = server.connect();
            let barrier = Arc::clone(&barrier);
            let build = workload.request;
            thread::spawn(move || {
                let mut latencies = Vec::new();
                barrier.wait();
                let mut sent = 0;
                while sent < per_client {
                    let batch = pipeline.min(per_client - sent);
                    let request: Vec<u8> = (0..batch)
                        .flat_map(|n| build(client * per_client + sent + n))
                        .collect();
                    let begun = Instant::now();
                    let errors = exchange(&mut stream, &request, batch);
                    latencies.push(begun.elapsed());
                    assert_eq!(errors, 0, "the server answered with errors");
                    sent += batch;
                }
                latencies
            })
        })
        .collect();
    barrier.wait();
    let started = Instant::now();
    let mut latencies: Vec<Duration> = handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("client"))
        .collect();
    let elapsed = started.elapsed();
    latencies.sort_unstable();
    let ops = per_client * settings.clients;
    Outcome {
        name: workload.name.to_owned(),
        ops,
        rate: as_float(ops) / seconds(elapsed),
        p50: latencies[latencies.len() / 2],
        p99: latencies[latencies.len() * 99 / 100],
    }
}

fn many_connections(server: &Server) -> (Duration, Duration) {
    let opened = Instant::now();
    let mut streams: Vec<TcpStream> = (0..IDLE_CONNECTIONS).map(|_| server.connect()).collect();
    let open_time = opened.elapsed();
    let started = Instant::now();
    for stream in &mut streams {
        assert_eq!(exchange(stream, &command(&[b"PING"]), 1), 0);
    }
    (open_time, started.elapsed())
}

fn large_values(server: &Server) -> f64 {
    let mut stream = server.connect();
    let value = vec![b'x'; LARGE_VALUE];
    let rounds = 100;
    let started = Instant::now();
    for _ in 0..rounds {
        assert_eq!(
            exchange(&mut stream, &command(&[b"SET", b"large", &value]), 1),
            0
        );
        assert_eq!(exchange(&mut stream, &command(&[b"GET", b"large"]), 1), 0);
    }
    as_float(rounds * 2) / seconds(started.elapsed())
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn main() -> ExitCode {
    let settings = Settings {
        ops: env_number("RAIKOU_PERF_OPS", 200_000),
        clients: env_number("RAIKOU_PERF_CLIENTS", 16).max(1),
        pipeline: env_number("RAIKOU_PERF_PIPELINE", 16).max(1),
        min_ops: as_float(env_number("RAIKOU_PERF_MIN_OPS", 0)),
    };
    let server = Server::start();
    println!(
        "{} ops per workload, {} clients, pipeline depth {}\n",
        settings.ops, settings.clients, settings.pipeline
    );
    println!("| workload | ops | ops/s | batch p50 ms | batch p99 ms |");
    println!("|---|---:|---:|---:|---:|");
    let mut slow = Vec::new();
    for workload in workloads() {
        let outcome = run_workload(&server, &workload, &settings);
        println!(
            "| {} | {} | {:.0} | {:.2} | {:.2} |",
            outcome.name,
            outcome.ops,
            outcome.rate,
            millis(outcome.p50),
            millis(outcome.p99)
        );
        if outcome.rate < settings.min_ops {
            slow.push(outcome.name);
        }
    }
    let (open_time, ping_time) = many_connections(&server);
    println!(
        "\n{IDLE_CONNECTIONS} connections: opened in {:.0} ms, one PING on each in {:.0} ms",
        millis(open_time),
        millis(ping_time)
    );
    println!(
        "{LARGE_VALUE}-byte values: {:.0} SET/GET ops/s",
        large_values(&server)
    );
    if slow.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("below {} ops/s: {}", settings.min_ops, slow.join(", "));
        ExitCode::FAILURE
    }
}
