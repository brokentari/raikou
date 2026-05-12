//! The `raikou` server binary.

use std::process::ExitCode;

use raikou::server;
use tokio::net::TcpListener;

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

#[tokio::main]
async fn main() -> ExitCode {
    let options = match parse_options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("raikou: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind((options.bind.as_str(), options.port)).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!(
                "raikou: cannot listen on {}:{}: {error}",
                options.bind, options.port
            );
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = server::serve(listener).await {
        eprintln!("raikou: {error}");
        return ExitCode::FAILURE;
    }
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
    fn rejects_bad_flags() {
        assert!(options(&["--port", "x"]).is_err());
        assert!(options(&["--port"]).is_err());
        assert!(options(&["--nope", "1"]).is_err());
    }
}
