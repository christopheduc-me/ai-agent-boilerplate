//! Core of the Docker HEALTHCHECK probe (ADR-014): the final image has no
//! curl, so a tiny static binary performs a raw HTTP/1.1 GET /healthz.
//! The logic lives here in the library so it is unit-tested; the binary in
//! `src/bin/healthcheck.rs` is a thin exit-code shell around it.

use std::io::{Read, Write};
use std::net::TcpStream;

pub fn addr_from_env() -> String {
    std::env::var("HEALTHCHECK_ADDR").unwrap_or_else(|_| "127.0.0.1:8000".into())
}

/// Ok when `addr` answers `HTTP/1.1 200` on `GET /healthz`.
pub fn check(addr: &str) -> Result<(), String> {
    let mut stream =
        TcpStream::connect(addr).map_err(|e| format!("cannot connect to {addr}: {e}"))?;
    let request = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("cannot send request: {e}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("cannot read response: {e}"))?;
    if response.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        Err(format!(
            "unexpected response: {}",
            response.lines().next().unwrap_or("<empty>")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// One-shot HTTP stub answering with the given status line.
    fn spawn_stub(status_line: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                let _ = std::io::Read::read(&mut stream, &mut buf);
                let _ = stream.write_all(
                    format!("{status_line}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                        .as_bytes(),
                );
            }
        });
        addr
    }

    #[test]
    fn healthy_server_passes() {
        let addr = spawn_stub("HTTP/1.1 200 OK");
        assert_eq!(check(&addr), Ok(()));
    }

    #[test]
    fn non_200_fails() {
        let addr = spawn_stub("HTTP/1.1 503 Service Unavailable");
        let err = check(&addr).unwrap_err();
        assert!(err.contains("503"), "{err}");
    }

    #[test]
    fn unreachable_server_fails() {
        // Bind then drop: the port exists but nothing listens anymore.
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().to_string()
        };
        assert!(check(&addr).is_err());
    }

    #[test]
    fn default_addr_comes_from_env_or_fallback() {
        // Only the fallback path is asserted (env mutation is process-global).
        if std::env::var("HEALTHCHECK_ADDR").is_err() {
            assert_eq!(addr_from_env(), "127.0.0.1:8000");
        }
    }
}
