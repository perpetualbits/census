use std::net::{TcpStream, SocketAddr};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[allow(dead_code)] // endpoint fields kept for diagnostics; only local_port is read today
pub struct Tunnel {
    pub local_host: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    // None when reusing an existing forward we did not spawn
    child: Option<Child>,
}

impl Tunnel {
    #[allow(dead_code)] // useful for a future "(reused tunnel)" status line
    pub fn is_reused(&self) -> bool { self.child.is_none() }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn ensure(
    ssh_alias: &str,
    remote_host: &str,
    remote_port: u16,
    timeout: Duration,
) -> anyhow::Result<Tunnel> {
    if let Some(port) = find_existing(remote_host, remote_port) {
        return Ok(Tunnel {
            local_host: "127.0.0.1".into(),
            local_port: port,
            remote_host: remote_host.into(),
            remote_port,
            child: None,
        });
    }

    let local_port = free_port()?;
    let forward = format!("127.0.0.1:{local_port}:{remote_host}:{remote_port}");
    let child = Command::new("ssh")
        .args([
            "-o", "ExitOnForwardFailure=yes",
            "-o", "BatchMode=yes",
            "-o", "ServerAliveInterval=30",
            "-o", "ServerAliveCountMax=3",
            "-N",
            "-L", &forward,
            ssh_alias,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if port_open("127.0.0.1", local_port) {
            return Ok(Tunnel {
                local_host: "127.0.0.1".into(),
                local_port,
                remote_host: remote_host.into(),
                remote_port,
                child: Some(child),
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    anyhow::bail!("SSH tunnel to {remote_host}:{remote_port} did not come up within {}s", timeout.as_secs())
}

fn find_existing(remote_host: &str, remote_port: u16) -> Option<u16> {
    let out = Command::new("ps")
        .args(["-eo", "command"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if !line.contains("ssh") { continue; }
        if let Some(local_port) = parse_ssh_l_forward(line, remote_host, remote_port) {
            if port_open("127.0.0.1", local_port) {
                return Some(local_port);
            }
        }
    }
    None
}

// Parse the local port from an `ssh -L` command line.
// Handles both:
//   -L LOCAL_PORT:REMOTE_HOST:REMOTE_PORT
//   -L 127.0.0.1:LOCAL_PORT:REMOTE_HOST:REMOTE_PORT
fn parse_ssh_l_forward(line: &str, remote_host: &str, remote_port: u16) -> Option<u16> {
    let mut tokens = line.split_whitespace();
    while let Some(tok) = tokens.next() {
        let spec = if tok == "-L" {
            tokens.next()?
        } else if let Some(s) = tok.strip_prefix("-L") {
            s
        } else {
            continue;
        };
        let parts: Vec<&str> = spec.splitn(4, ':').collect();
        let (local_port_s, rh, rp_s) = match parts.as_slice() {
            [lp, rh, rp] => (*lp, *rh, *rp),
            [_bind, lp, rh, rp] => (*lp, *rh, *rp),
            _ => continue,
        };
        if rh == remote_host && rp_s.parse::<u16>().ok() == Some(remote_port) {
            return local_port_s.parse::<u16>().ok();
        }
    }
    None
}

fn port_open(host: &str, port: u16) -> bool {
    let addr: SocketAddr = format!("{host}:{port}").parse().unwrap();
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

fn free_port() -> anyhow::Result<u16> {
    let sock = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(sock.local_addr()?.port())
}
