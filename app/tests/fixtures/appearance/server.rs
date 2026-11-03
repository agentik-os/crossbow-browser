use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const PAGE: &str = include_str!("index.html");

fn respond(mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    while request.len() < 8192 {
        let count = stream.read(&mut chunk)?;
        if count == 0 { break; }
        request.extend_from_slice(&chunk[..count]);
        if request.windows(4).any(|part| part == b"\r\n\r\n") { break; }
    }
    let line = request.split(|byte| *byte == b'\n').next().unwrap_or_default();
    let ok = line == b"GET / HTTP/1.1\r" || line == b"GET / HTTP/1.0\r";
    let (status, body) = if ok { ("200 OK", PAGE) } else { ("404 Not Found", "Not found") };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; form-action 'none'; frame-ancestors 'none'; base-uri 'none'\r\n\r\n{body}", body.len())
}

fn main() -> std::io::Result<()> {
    // Port zero avoids reusing or stopping another fixture service.
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    println!("Appearance fixture: http://{}/", listener.local_addr()?);
    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            let _ = respond(stream);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch(path: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || respond(listener.accept().unwrap().0).unwrap());
        let mut client = TcpStream::connect(address).unwrap();
        write!(client, "GET {path} HTTP/1.1\r\nHost: {address}\r\n\r\n").unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        response
    }

    #[test]
    fn serves_only_the_synthetic_fixture() {
        let response = fetch("/");
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with(PAGE));
        assert!(response.contains("form-action 'none'"));
        assert!(response.contains("Cache-Control: no-store"));
    }

    #[test]
    fn does_not_serve_files_or_unknown_paths() {
        for path in ["/favicon.ico", "/etc/passwd", "/../index.html", "/?private=value"] {
            let response = fetch(path);
            assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
            assert!(!response.contains("window.appearanceProbe"));
        }
    }
}
