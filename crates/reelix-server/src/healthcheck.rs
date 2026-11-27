//! `reelix healthcheck`: a dependency-free HTTP/1.0 probe for container healthchecks.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::Config;

const TIMEOUT: Duration = Duration::from_secs(5);

/// Address the probe connects to: loopback when bound to an unspecified address.
pub fn target_addr(bind: IpAddr, port: u16) -> SocketAddr {
    let ip = match bind {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    };
    SocketAddr::new(ip, port)
}

/// Liveness endpoint path for the given URL base. It checks only Reelix's own parts (the database and
/// the data directory), so a download client or indexer problem does not mark the container unhealthy.
pub fn health_path(url_base: &str) -> String {
    format!("{url_base}/api/v1/system/health/live")
}

/// Sends `GET path` and returns the HTTP status code.
pub fn probe(addr: SocketAddr, path: &str) -> std::io::Result<u16> {
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let request = format!("GET {path} HTTP/1.0\r\nHost: {addr}\r\nUser-Agent: reelix-healthcheck\r\n\r\n");
    stream.write_all(request.as_bytes())?;

    // Only the status line matters; read until the first CRLF (or EOF).
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    while !buf.windows(2).any(|w| w == b"\r\n") && buf.len() < 4096 {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    parse_status(&buf)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad HTTP response"))
}

/// Parses the status code out of an HTTP status line like `HTTP/1.1 200 OK`.
pub fn parse_status(response: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(response).ok()?;
    let line = text.lines().next()?;
    let mut parts = line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// Runs the healthcheck; returns the process exit code (0 iff HTTP 200).
pub fn run(config: &Config) -> u8 {
    let addr = target_addr(config.bind, config.port);
    let path = health_path(&config.url_base);
    match probe(addr, &path) {
        Ok(200) => 0,
        Ok(code) => {
            eprintln!("healthcheck: http://{addr}{path} returned {code}");
            1
        }
        Err(e) => {
            eprintln!("healthcheck: http://{addr}{path} failed: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn status_line_parsing() {
        assert_eq!(
            parse_status(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n"),
            Some(200)
        );
        assert_eq!(parse_status(b"HTTP/1.0 503 Service Unavailable\r\n"), Some(503));
        assert_eq!(parse_status(b"garbage"), None);
        assert_eq!(parse_status(b""), None);
    }

    #[test]
    fn target_and_path() {
        assert_eq!(
            target_addr("0.0.0.0".parse().unwrap(), 7700),
            "127.0.0.1:7700".parse().unwrap()
        );
        assert_eq!(target_addr("::".parse().unwrap(), 1), "[::1]:1".parse().unwrap());
        assert_eq!(
            target_addr("10.1.2.3".parse().unwrap(), 2),
            "10.1.2.3:2".parse().unwrap()
        );
        assert_eq!(health_path(""), "/api/v1/system/health/live");
        assert_eq!(health_path("/reelix"), "/reelix/api/v1/system/health/live");
    }

    fn serve_once(response: &'static str) -> (SocketAddr, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let n = s.read(&mut req).unwrap();
            s.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&req[..n]).into_owned()
        });
        (addr, handle)
    }

    #[test]
    fn probe_against_local_server() {
        let (addr, h) = serve_once("HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
        assert_eq!(probe(addr, "/x/api/v1/system/health").unwrap(), 200);
        assert!(
            h.join()
                .unwrap()
                .starts_with("GET /x/api/v1/system/health HTTP/1.0\r\n")
        );

        let (addr, h) = serve_once("HTTP/1.1 503 Service Unavailable\r\n\r\n");
        assert_eq!(probe(addr, "/").unwrap(), 503);
        h.join().unwrap();
    }
}
