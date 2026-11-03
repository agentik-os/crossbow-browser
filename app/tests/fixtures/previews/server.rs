use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const INDEX: &str = include_str!("index.html");
const ARTICLE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Fixture article</title>\
<meta name=\"description\" content=\"A synthetic article for the 5-Second Previews check.\"></head><body>\
<h1>Fixture article</h1><p>This synthetic article describes a small lighthouse on a rocky island. \
Its keeper climbs the stairs every evening, trims the wick and writes the weather in a logbook.</p>\
<script>document.title = 'scripts are never sent';</script></body></html>";

/// What one request asked for: its path and whether it carried a cookie or a Referer.
struct Asked {
    path: String,
    cookie: bool,
    referer: bool,
}

fn read(stream: &mut TcpStream) -> std::io::Result<Option<Asked>> {
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    while request.len() < 8192 {
        let count = stream.read(&mut chunk)?;
        if count == 0 { break; }
        request.extend_from_slice(&chunk[..count]);
        if request.windows(4).any(|part| part == b"\r\n\r\n") { break; }
    }
    let text = String::from_utf8_lossy(&request);
    let mut lines = text.split("\r\n");
    let Some(path) = lines.next().and_then(|line| line.strip_prefix("GET ")).and_then(|rest| rest.strip_suffix(" HTTP/1.1").or_else(|| rest.strip_suffix(" HTTP/1.0"))) else { return Ok(None) };
    let header = |name: &str| text.split("\r\n").skip(1).any(|line| line.len() > name.len() && line[..name.len()].eq_ignore_ascii_case(name) && line.as_bytes()[name.len()] == b':');
    Ok(Some(Asked { path: path.to_owned(), cookie: header("cookie"), referer: header("referer") }))
}

/// Status, extra headers and body for `path`.
fn answer(path: &str) -> (&'static str, String, String) {
    let html = "Content-Type: text/html; charset=utf-8\r\n".to_owned();
    match path {
        // A cookie the preview must never send back.
        "/" => ("200 OK", format!("{html}Set-Cookie: fixture=signed-in; Path=/; SameSite=Lax\r\n"), INDEX.into()),
        "/article" => ("200 OK", html, ARTICLE.into()),
        "/away" => ("302 Found", "Location: http://192.168.0.1/\r\n".into(), String::new()),
        "/report.pdf" => ("200 OK", "Content-Type: application/pdf\r\n".into(), "%PDF-1.7 synthetic".into()),
        "/download" => ("200 OK", format!("{html}Content-Disposition: attachment; filename=\"fixture.html\"\r\n"), ARTICLE.into()),
        "/empty" => ("200 OK", html, "<!doctype html><title></title><script>void 0</script>".into()),
        "/slow" => {
            std::thread::sleep(Duration::from_secs(6));
            ("200 OK", html, ARTICLE.into())
        }
        _ => match path.strip_prefix("/hops/").and_then(|hops| hops.parse::<u8>().ok()).filter(|hops| (1..=9).contains(hops)) {
            Some(1) => ("302 Found", "Location: /article\r\n".into(), String::new()),
            Some(hops) => ("302 Found", format!("Location: /hops/{}\r\n", hops - 1), String::new()),
            None => ("404 Not Found", html, "<!doctype html><title>Not found</title><p>Not found</p>".into()),
        },
    }
}

fn respond(mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(8)))?;
    let Some(asked) = read(&mut stream)? else { return Ok(()) };
    // Only the path and whether the headers came; never their values.
    println!("GET {} cookie={} referer={}", asked.path, if asked.cookie { "yes" } else { "no" }, if asked.referer { "yes" } else { "no" });
    let (status, headers, body) = answer(&asked.path);
    write!(stream, "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n{body}", body.len())
}

fn main() -> std::io::Result<()> {
    // Port zero avoids reusing or stopping another fixture service.
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    println!("5-Second Previews fixture: http://{}/", listener.local_addr()?);
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || respond(stream));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch(request: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || respond(listener.accept().unwrap().0).unwrap());
        let mut client = TcpStream::connect(address).unwrap();
        write!(client, "{request}").unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        response
    }

    #[test]
    fn serves_the_synthetic_pages() {
        let index = fetch("GET / HTTP/1.1\r\nHost: fixture\r\n\r\n");
        assert!(index.starts_with("HTTP/1.1 200 OK\r\n") && index.contains("Set-Cookie: fixture=signed-in") && index.ends_with(INDEX));
        let article = fetch("GET /article HTTP/1.1\r\nCookie: fixture=signed-in\r\n\r\n");
        assert!(article.contains("Content-Type: text/html; charset=utf-8") && article.ends_with(ARTICLE));
        assert!(fetch("GET /report.pdf HTTP/1.1\r\n\r\n").contains("Content-Type: application/pdf"));
        assert!(fetch("GET /download HTTP/1.1\r\n\r\n").contains("Content-Disposition: attachment"));
        assert!(fetch("GET /away HTTP/1.1\r\n\r\n").contains("Location: http://192.168.0.1/"));
    }

    #[test]
    fn the_redirect_chain_counts_down_to_the_article() {
        assert!(fetch("GET /hops/4 HTTP/1.1\r\n\r\n").contains("Location: /hops/3\r\n"));
        assert!(fetch("GET /hops/1 HTTP/1.1\r\n\r\n").contains("Location: /article\r\n"));
        for path in ["/hops/0", "/hops/10", "/hops/x", "/etc/passwd", "/../index.html"] {
            assert!(fetch(&format!("GET {path} HTTP/1.1\r\n\r\n")).starts_with("HTTP/1.1 404 Not Found\r\n"), "{path}");
        }
    }

    #[test]
    fn requests_are_read_for_their_path_and_header_names_only() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        write!(client, "GET /article HTTP/1.1\r\nCOOKIE: a=b\r\nReferer: http://example.test/\r\n\r\n").unwrap();
        let asked = read(&mut listener.accept().unwrap().0).unwrap().unwrap();
        assert_eq!((asked.path.as_str(), asked.cookie, asked.referer), ("/article", true, true));
        let mut client = TcpStream::connect(address).unwrap();
        write!(client, "GET /article HTTP/1.1\r\nX-Cookie: a\r\n\r\n").unwrap();
        let asked = read(&mut listener.accept().unwrap().0).unwrap().unwrap();
        assert!(!asked.cookie && !asked.referer);
        drop(client);
    }
}
