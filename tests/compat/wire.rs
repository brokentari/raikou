//! Layer 2: raw bytes over TCP, compared byte for byte.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, thread};

use raikou::resp;

use crate::oracle::{Environment, Target};

const MARKER: &str = "raikou-wire-marker";
const PAUSE: Duration = Duration::from_millis(100);
const IDLE: Duration = Duration::from_millis(1500);

struct WireCase {
    name: String,
    line: usize,
    resp3: bool,
    writes: Vec<Vec<u8>>,
}

#[derive(PartialEq, Eq)]
enum End {
    Marker,
    Eof,
    Timeout,
}

#[derive(PartialEq, Eq)]
struct Transcript {
    bytes: Vec<u8>,
    end: End,
}

fn unescape(text: &str, location: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        let escape = bytes.get(index + 1).copied();
        index += 2;
        match escape {
            Some(b'r') => out.push(b'\r'),
            Some(b'n') => out.push(b'\n'),
            Some(b't') => out.push(b'\t'),
            Some(b'\\') => out.push(b'\\'),
            Some(b'"') => out.push(b'"'),
            Some(b'x') => {
                let pair = text
                    .get(index..index + 2)
                    .ok_or(format!("{location}: short \\x"))?;
                out.push(u8::from_str_radix(pair, 16).map_err(|_| format!("{location}: bad \\x"))?);
                index += 2;
            }
            _ => return Err(format!("{location}: unknown escape")),
        }
    }
    Ok(out)
}

fn load(path: &Path) -> Result<Vec<WireCase>, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut cases: Vec<WireCase> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let location = format!("{}:{line}", path.display());
        if raw.is_empty() || raw.starts_with("//") {
            continue;
        }
        if let Some(name) = raw.strip_prefix("@case ") {
            let name = name.trim().to_owned();
            cases.push(WireCase {
                name,
                line,
                resp3: false,
                writes: vec![Vec::new()],
            });
            continue;
        }
        let case = cases
            .last_mut()
            .ok_or(format!("{location}: data before @case"))?;
        if raw == "@resp3" {
            case.resp3 = true;
        } else if raw == "@split" {
            case.writes.push(Vec::new());
        } else if let Some(rest) = raw.strip_prefix("@repeat ") {
            let (count, piece) = rest
                .split_once(' ')
                .ok_or(format!("{location}: bad @repeat"))?;
            let count: usize = count
                .parse()
                .map_err(|_| format!("{location}: bad @repeat"))?;
            let piece = unescape(piece, &location)?;
            let write = case.writes.last_mut().ok_or("no write")?;
            for _ in 0..count {
                write.extend_from_slice(&piece);
            }
        } else if raw.starts_with('@') {
            return Err(format!("{location}: unknown directive"));
        } else {
            let bytes = unescape(raw, &location)?;
            case.writes
                .last_mut()
                .ok_or("no write")?
                .extend_from_slice(&bytes);
        }
    }
    Ok(cases)
}

fn request(parts: &[&str]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", parts.len()).into_bytes();
    for part in parts {
        out.extend_from_slice(format!("${}\r\n{part}\r\n", part.len()).as_bytes());
    }
    out
}

fn exchange(case: &WireCase, target: &Target) -> Result<Transcript, String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port));
    let mut stream = TcpStream::connect(address).map_err(|error| error.to_string())?;
    stream
        .set_nodelay(true)
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(IDLE))
        .map_err(|error| error.to_string())?;
    let mut received = Vec::new();
    if case.resp3 {
        stream
            .write_all(&request(&["HELLO", "3"]))
            .map_err(|error| error.to_string())?;
        loop {
            if let Ok(Some((_, used))) = resp::decode(&received) {
                received.drain(..used);
                break;
            }
            read_more(&mut stream, &mut received)?.ok_or("server closed during HELLO")?;
        }
    }
    for write in &case.writes {
        if stream.write_all(write).is_err() {
            break;
        }
        thread::sleep(PAUSE);
    }
    let _ = stream.write_all(&request(&["ECHO", MARKER]));
    let reply = format!("${}\r\n{MARKER}\r\n", MARKER.len()).into_bytes();
    loop {
        if received.ends_with(&reply) {
            received.truncate(received.len() - reply.len());
            return Ok(Transcript {
                bytes: received,
                end: End::Marker,
            });
        }
        match read_more(&mut stream, &mut received)? {
            Some(true) => {}
            Some(false) => {
                return Ok(Transcript {
                    bytes: received,
                    end: End::Timeout,
                });
            }
            None => {
                return Ok(Transcript {
                    bytes: received,
                    end: End::Eof,
                });
            }
        }
    }
}

fn read_more(stream: &mut TcpStream, received: &mut Vec<u8>) -> Result<Option<bool>, String> {
    let mut chunk = [0; 4096];
    match stream.read(&mut chunk) {
        Ok(0) => Ok(None),
        Ok(count) => {
            received.extend_from_slice(&chunk[..count]);
            Ok(Some(true))
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Ok(Some(false))
        }
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn show(transcript: &Transcript) -> String {
    let text: String = transcript
        .bytes
        .iter()
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .collect();
    let end = match transcript.end {
        End::Marker => "<marker>",
        End::Eof => "<eof>",
        End::Timeout => "<timeout>",
    };
    format!("{text} {end}")
}

fn files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = root.join("tests/cases/wire");
    let entries = fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "txt"))
        .collect();
    paths.sort();
    if let Ok(filter) = std::env::var("RAIKOU_COMPAT_FILTER") {
        paths.retain(|path| path.to_string_lossy().contains(&filter));
    }
    Ok(paths)
}

pub(crate) fn run_all(env: &Environment, root: &Path) -> Result<usize, String> {
    let mut failures = 0;
    for path in files(root)? {
        let shown = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let mut passed = 0;
        let cases = load(&path)?;
        for case in &cases {
            let expected = exchange(case, &env.oracle)?;
            let actual = exchange(case, &env.subject_target)?;
            if expected == actual {
                passed += 1;
            } else {
                failures += 1;
                println!("FAIL {shown}:{} {}", case.line, case.name);
                println!("  {}: {}", env.oracle.name, show(&expected));
                println!("  {}: {}", env.subject_target.name, show(&actual));
            }
        }
        println!(
            "{} {shown} {passed}/{} cases",
            if passed == cases.len() {
                "ok  "
            } else {
                "FAIL"
            },
            cases.len()
        );
    }
    Ok(failures)
}
