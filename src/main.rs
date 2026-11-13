//! The `raikou` server binary.

use std::process::ExitCode;

use raikou::server;
use tokio::signal::unix::{SignalKind, signal};

const USAGE: &str = "usage: raikou [--bind ADDRESS] [--port PORT]";

struct Options {
    bind: String,
    port: u16,
}

fn parse_options(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        bind: "127.0.0.1".to_owned(),
        port: 6379,
    };
    let mut args = args;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--bind" => options.bind = value,
            "--port" => {
                options.port = value
                    .parse()
                    .map_err(|_| format!("invalid port '{value}'"))?;
            }
            _ => return Err(format!("unknown option '{flag}'")),
        }
    }
    Ok(options)
}

async fn stop_signal() -> &'static str {
    let Ok(mut terminate) = signal(SignalKind::terminate()) else {
        return tokio::signal::ctrl_c().await.map_or("error", |()| "SIGINT");
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => "SIGINT",
        _ = terminate.recv() => "SIGTERM",
    }
}

fn is_loopback(bind: &str) -> bool {
    bind == "localhost"
        || bind
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[tokio::main]
async fn main() -> ExitCode {
    let options = match parse_options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("raikou: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let listener = match server::bind(&options.bind, options.port).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!(
                "raikou: cannot listen on {}:{}: {error}",
                options.bind, options.port
            );
            return ExitCode::FAILURE;
        }
    };
    match listener.local_addr() {
        Ok(address) => println!("{}", server::banner(address)),
        Err(error) => eprintln!("raikou: cannot read the listening address: {error}"),
    }
    if !is_loopback(&options.bind) {
        eprintln!(
            "raikou: warning: there is no password support; anyone who can reach this port has full access"
        );
    }
    let result = server::serve_until(listener, async {
        let name = stop_signal().await;
        println!("Received {name}, shutting down (send it again to stop at once)");
        tokio::spawn(async {
            stop_signal().await;
            std::process::exit(130);
        });
    })
    .await;
    if let Err(error) = result {
        eprintln!("raikou: {error}");
        return ExitCode::FAILURE;
    }
    println!("Bye");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(args: &[&str]) -> Result<Options, String> {
        parse_options(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn defaults() {
        let options = options(&[]).expect("valid");
        assert_eq!((options.bind.as_str(), options.port), ("127.0.0.1", 6379));
    }

    #[test]
    fn reads_both_flags() {
        let options = options(&["--port", "7000", "--bind", "0.0.0.0"]).expect("valid");
        assert_eq!((options.bind.as_str(), options.port), ("0.0.0.0", 7000));
    }

    #[test]
    fn only_local_addresses_are_loopback() {
        for bind in ["127.0.0.1", "::1", "localhost", "127.0.0.2"] {
            assert!(is_loopback(bind), "{bind}");
        }
        for bind in ["0.0.0.0", "192.168.1.5", "::", "example.com"] {
            assert!(!is_loopback(bind), "{bind}");
        }
    }

    #[test]
    fn rejects_bad_flags() {
        assert!(options(&["--port", "x"]).is_err());
        assert!(options(&["--port"]).is_err());
        assert!(options(&["--nope", "1"]).is_err());
    }
}
