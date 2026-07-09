//! Tiny HTTP healthcheck used by the Docker HEALTHCHECK (the final image has no curl).

use std::io::{Read, Write};
use std::net::TcpStream;

fn main() {
    let addr = std::env::var("HEALTHCHECK_ADDR").unwrap_or_else(|_| "127.0.0.1:8000".into());
    let Ok(mut stream) = TcpStream::connect(&addr) else {
        std::process::exit(1);
    };
    let request = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    if stream.write_all(request.as_bytes()).is_err() {
        std::process::exit(1);
    }
    let mut response = String::new();
    if stream.read_to_string(&mut response).is_err() || !response.starts_with("HTTP/1.1 200") {
        std::process::exit(1);
    }
}
