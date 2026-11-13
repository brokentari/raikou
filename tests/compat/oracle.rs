use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

const ORACLE_IMAGE: &str = "redis:8.10.3";

/// A server that `redis-cli` can reach from inside the oracle container.
pub(crate) struct Target {
    pub(crate) name: &'static str,
    pub(crate) port: u16,
}

/// The oracle container and the server under test; both stop on drop.
pub(crate) struct Environment {
    container: String,
    subject: Option<Child>,
    pub(crate) oracle: Target,
    pub(crate) subject_target: Target,
}

impl Environment {
    pub(crate) fn container(&self) -> &str {
        &self.container
    }

    pub(crate) fn start() -> Result<Self, String> {
        let docker = Command::new("docker").arg("version").output();
        if !docker.is_ok_and(|output| output.status.success()) {
            return Err("the compat test needs Docker: `docker version` failed".to_owned());
        }
        let oracle_port = free_port()?;
        let container = format!("raikou-oracle-{}", std::process::id());
        let started = Command::new("docker")
            .args([
                "run",
                "-d",
                "--rm",
                "--network",
                "host",
                "--name",
                &container,
                ORACLE_IMAGE,
            ])
            .args([
                "timeout",
                "1800",
                "redis-server",
                "--bind",
                "127.0.0.1",
                "--port",
                &oracle_port.to_string(),
            ])
            .args(["--save", "", "--appendonly", "no"])
            .output()
            .map_err(|error| format!("cannot run docker: {error}"))?;
        if !started.status.success() {
            return Err(format!(
                "cannot start {ORACLE_IMAGE}: {}",
                lossy(&started.stderr)
            ));
        }
        let mut environment = Self {
            container,
            subject: None,
            oracle: Target {
                name: "oracle",
                port: oracle_port,
            },
            subject_target: Target {
                name: "raikou",
                port: oracle_port,
            },
        };
        wait_for(oracle_port)?;
        if std::env::var_os("RAIKOU_COMPAT_SELF").is_some() {
            environment.subject_target.name = "oracle (self-check)";
            return Ok(environment);
        }
        let subject_port = free_port()?;
        let child = Command::new(env!("CARGO_BIN_EXE_raikou"))
            .args(["--port", &subject_port.to_string()])
            .stdin(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot start raikou: {error}"))?;
        environment.subject = Some(child);
        environment.subject_target.port = subject_port;
        wait_for(subject_port)?;
        Ok(environment)
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        if let Some(child) = &mut self.subject {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.container])
            .output();
    }
}

fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| format!("cannot find a free port: {error}"))?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    Ok(address.port())
}

fn wait_for(port: u16) -> Result<(), String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    for _ in 0..100 {
        if TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!("nothing listens on port {port} after 10 seconds"))
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}
