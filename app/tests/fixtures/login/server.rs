//! A loopback fixture for saving and filling website passwords: two
//! servers on two OS-selected ports of 127.0.0.1, the second named
//! localhost, so two origins of two sites. Pages are
//! synthetic; their fields have no names, so no typed value is ever sent here,
//! and nothing a request carries is read past its first line, stored or printed.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

const INDEX: &str = include_str!("index.html");
const FORM: &str = include_str!("form.html");
const SIGNED_IN: &str = include_str!("signed-in.html");
const FETCH: &str = include_str!("fetch.html");
const FRAMES: &str = include_str!("frames.html");
const PATCHED: &str = include_str!("patched.html");
const VISIBILITY: &str = include_str!("visibility.html");
const PAYMENT: &str = include_str!("payment.html");

/// The response to one request line, given the other server's origin.
fn route(line: &[u8], other: &str) -> (&'static str, &'static str, String) {
    let html = "text/html; charset=utf-8";
    let page = |body: &str| ("200 OK", html, body.replace("{{OTHER}}", other));
    match line {
        b"GET / HTTP/1.1" | b"GET / HTTP/1.0" => page(INDEX),
        b"GET /form HTTP/1.1" | b"GET /form HTTP/1.0" => page(FORM),
        b"POST /signed-in HTTP/1.1" | b"POST /signed-in HTTP/1.0" => page(SIGNED_IN),
        b"GET /fetch HTTP/1.1" | b"GET /fetch HTTP/1.0" => page(FETCH),
        b"POST /api/login HTTP/1.1" | b"POST /api/login HTTP/1.0" => ("200 OK", "application/json", "{\"ok\":true}".to_owned()),
        b"GET /frames HTTP/1.1" | b"GET /frames HTTP/1.0" => page(FRAMES),
        b"GET /patched HTTP/1.1" | b"GET /patched HTTP/1.0" => page(PATCHED),
        b"GET /visibility HTTP/1.1" | b"GET /visibility HTTP/1.0" => page(VISIBILITY),
        b"GET /payment HTTP/1.1" | b"GET /payment HTTP/1.0" => page(PAYMENT),
        _ => ("404 Not Found", "text/plain; charset=utf-8", "Not found".to_owned()),
    }
}

fn respond(mut stream: TcpStream, other: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    // Only the request line is kept; headers and any body are discarded unread.
    let mut line = Vec::new();
    let mut byte = [0; 1];
    while line.len() < 256 && stream.read(&mut byte)? == 1 && byte[0] != b'\n' {
        line.push(byte[0]);
    }
    let line = line.strip_suffix(b"\r").unwrap_or(&line);
    let (status, kind, body) = route(line, other);
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; form-action 'self'; frame-src 'self' http://127.0.0.1:* http://localhost:*; frame-ancestors 'self' http://127.0.0.1:* http://localhost:*; base-uri 'none'\r\n\r\n{body}", body.len())?;
    // Discard what the client still sends, so closing doesn't reset the
    // connection before it reads the response.
    stream.shutdown(Shutdown::Write)?;
    let mut sink = [0; 1024];
    let mut left: usize = 64 * 1024;
    while left > 0 {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(count) => left = left.saturating_sub(count),
        }
    }
    Ok(())
}

fn serve(listener: TcpListener, other: String) {
    for stream in listener.incoming().flatten() {
        let _ = respond(stream, &other);
    }
}

/// The two servers' origins: the site on 127.0.0.1, and the other server,
/// also listening on 127.0.0.1, named localhost. Another host, not only
/// another port, makes it another site, so Chromium's site isolation puts
/// its frames in another renderer process than the page that embeds them.
fn origins(site: SocketAddr, other: SocketAddr) -> (String, String) {
    (format!("http://{site}"), format!("http://localhost:{}", other.port()))
}

fn main() -> std::io::Result<()> {
    // Port zero avoids reusing or stopping another fixture service.
    let site = TcpListener::bind(("127.0.0.1", 0))?;
    let other = TcpListener::bind(("127.0.0.1", 0))?;
    let (site_origin, other_origin) = origins(site.local_addr()?, other.local_addr()?);
    println!("Login fixture: {site_origin}/");
    println!("Other origin:  {other_origin}/");
    let back = site_origin.clone();
    let second = std::thread::spawn(move || serve(other, back));
    serve(site, other_origin);
    let _ = second.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OTHER: &str = "http://localhost:65000";

    fn fetch(request: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || respond(listener.accept().unwrap().0, OTHER).unwrap());
        let mut client = TcpStream::connect(address).unwrap();
        client.write_all(request.replace("{host}", &address.to_string()).as_bytes()).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        drop(client);
        server.join().unwrap();
        response
    }

    fn get(path: &str) -> String {
        fetch(&format!("GET {path} HTTP/1.1\r\nHost: {{host}}\r\n\r\n"))
    }

    #[test]
    fn serves_each_scenario() {
        for (path, marker) in [("/", "Password save and fill fixture"), ("/form", "action=\"/signed-in\""), ("/fetch", "fetch('/api/login'"), ("/frames", "id=\"cross\""), ("/patched", "window.loginProbe"), ("/visibility", "attachShadow({ mode: 'closed' })"), ("/payment", "autocomplete=\"cc-number\"")] {
            let response = get(path);
            assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{path}");
            assert!(response.contains(marker), "{path}");
            assert!(response.contains("Cache-Control: no-store") && response.contains("form-action 'self'") && response.contains("connect-src 'self'"), "{path}");
        }
        let signed_in = fetch("POST /signed-in HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\n\r\n");
        assert!(signed_in.starts_with("HTTP/1.1 200 OK\r\n") && signed_in.contains("Signed in"));
        let api = fetch("POST /api/login HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\n\r\n");
        assert!(api.ends_with("{\"ok\":true}") && api.contains("application/json"));
    }

    #[test]
    fn the_frames_page_embeds_the_other_origin_and_its_own() {
        let frames = get("/frames");
        assert!(frames.contains(&format!("src=\"{OTHER}/form\"")));
        assert!(frames.contains("src=\"/form\""));
        assert!(get("/").contains(&format!("href=\"{OTHER}/form\"")));
        assert!(frames.contains("frame-src 'self' http://127.0.0.1:* http://localhost:*") && frames.contains("frame-ancestors 'self' http://127.0.0.1:* http://localhost:*"));
    }

    /// C10-4: the other origin is another host, not only another port, so
    /// it is another site, which Chromium's site isolation puts in another
    /// renderer process than the page that embeds it.
    #[test]
    fn the_other_origin_is_another_site() {
        let (site, other) = origins(([127, 0, 0, 1], 8080).into(), ([127, 0, 0, 1], 9090).into());
        assert_eq!((site.as_str(), other.as_str()), ("http://127.0.0.1:8080", "http://localhost:9090"));
    }

    #[test]
    fn no_field_has_a_name_so_no_typed_value_is_ever_sent() {
        for page in [INDEX, FORM, FETCH, FRAMES, PATCHED, SIGNED_IN, VISIBILITY, PAYMENT] {
            assert!(!page.contains(" name=\"") || page.matches(" name=\"").count() == page.matches("<meta name=\"").count(), "only meta tags have names");
        }
        assert!(FETCH.contains("body: ''") && PATCHED.contains("body: ''"), "requests carry nothing typed");
    }

    /// Round 10's native check: every case states its expected result, the
    /// payment frames come from the other origin, and the planted values are
    /// synthetic.
    #[test]
    fn the_visibility_page_states_each_expected_result_and_embeds_the_other_origin() {
        let page = get("/visibility");
        for case in ["v1", "v2", "v3", "v4", "v5", "v6", "v7"] {
            let section = &page[page.find(&format!("<section id=\"{case}\">")).unwrap_or_else(|| panic!("{case}"))..];
            let section = &section[..section.find("</section>").unwrap()];
            assert!(section.contains("<p class=\"expected\">"), "{case} states its expected result");
        }
        assert_eq!(page.matches(&format!("src=\"{OTHER}/payment\"")).count(), 3);
        for planted in ["Planted Org", "+1 555 0199", "planted@attacker.example"] {
            assert!(page.contains(&format!("value=\"{planted}\"")));
        }
        assert!(page.contains("a toast says \"Filled 2 of 3 fields. 1 wasn't clearly visible, so it was left empty.\""));
    }

    #[test]
    fn a_request_body_is_never_read_or_echoed() {
        let response = fetch("POST /signed-in HTTP/1.1\r\nHost: {host}\r\nContent-Length: 29\r\n\r\nfixture-body-marker=never-read");
        assert!(!response.contains("fixture-body-marker"));
    }

    #[test]
    fn does_not_serve_files_unknown_paths_queries_or_other_methods() {
        for request in [
            "GET /favicon.ico HTTP/1.1\r\n\r\n", "GET /etc/passwd HTTP/1.1\r\n\r\n", "GET /../form HTTP/1.1\r\n\r\n", "GET /form?user=x HTTP/1.1\r\n\r\n",
            "GET /signed-in HTTP/1.1\r\n\r\n", "POST /form HTTP/1.1\r\n\r\n", "PUT /api/login HTTP/1.1\r\n\r\n", "GET /form.html HTTP/1.1\r\n\r\n",
        ] {
            assert!(fetch(request).starts_with("HTTP/1.1 404 Not Found\r\n"), "{request:?}");
        }
    }

    #[test]
    fn the_patched_page_replaces_every_built_in_the_listener_captures() {
        let listener = include_str!("../../../../crates/crossbow-engine/src/login.js");
        let (prologue, _) = listener.split_once("// -- Everything below").unwrap();
        for (captured, patched) in [
            ("Reflect.apply", "wrapValue(Reflect, 'apply'"), ("Object.getOwnPropertyDescriptor", "wrapValue(Object, 'getOwnPropertyDescriptor'"),
            ("Function.prototype[Symbol.hasInstance]", "wrapValue(Function.prototype, Symbol.hasInstance"),
            ("EventTarget.prototype.addEventListener", "'addEventListener'"), ("EventTarget.prototype.dispatchEvent", "'dispatchEvent'"),
            ("Element.prototype.querySelectorAll", "'querySelectorAll'"), ("Element.prototype.getAttribute", "'getAttribute'"),
            ("String.prototype.includes", "'includes'"), ("String.prototype.toLowerCase", "'toLowerCase'"),
            ("NodeList.prototype.item", "wrapValue(NodeList.prototype, 'item'"), ("HTMLCollection.prototype.item", "wrapValue(HTMLCollection.prototype, 'item'"),
            ("get(NodeList.prototype, 'length')", "wrapAccessor(NodeList.prototype, 'length'"), ("get(HTMLCollection.prototype, 'length')", "wrapAccessor(HTMLCollection.prototype, 'length'"),
            ("get(Event.prototype, 'target')", "wrapAccessor(Event.prototype, 'target'"), ("get(Navigator.prototype, 'userActivation')", "'userActivation'"),
            ("get(UserActivation.prototype, 'isActive')", "'isActive'"), ("get(Document.prototype, 'activeElement')", "'activeElement'"),
            ("get(Document.prototype, 'forms')", "'forms'"), ("get(InputCtor.prototype, 'type')", "wrapAccessor(HTMLInputElement.prototype, 'type'"),
            ("get(InputCtor.prototype, 'form')", "wrapAccessor(HTMLInputElement.prototype, 'form'"), ("gopd(InputCtor.prototype, 'value')", "wrapAccessor(HTMLInputElement.prototype, 'value'"),
            ("Performance.prototype.now", "'now'"), ("Observer.prototype.observe", "'observe'"), ("PerformanceObserverEntryList.prototype.getEntries", "'getEntries'"),
            ("get(PerformanceEntry.prototype, 'startTime')", "'startTime'"), ("get(PerformanceResourceTiming.prototype, 'initiatorType')", "'initiatorType'"),
            ("const EventCtor = Event;", "wrapConstructor('Event')"), ("const Observer = PerformanceObserver;", "wrapConstructor('PerformanceObserver')"),
        ] {
            assert!(prologue.contains(captured), "the listener no longer captures {captured}; update this test and the page");
            assert!(PATCHED.contains(patched), "the page doesn't patch {captured}");
        }
        assert!(PATCHED.contains("Function.prototype, 'call'") && PATCHED.contains("Function.prototype, 'apply'") && PATCHED.contains("Function.prototype, 'bind'"));
    }
}
