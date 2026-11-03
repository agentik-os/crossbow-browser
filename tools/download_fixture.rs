//! Standalone, synthetic loopback HTTP fixture. Build with rustc; no dependencies.
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    net::{Ipv4Addr, Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const PAYLOAD_LEN: u64 = 8 * 1024 * 1024;
const TRUNCATED_LEN: u64 = 256 * 1024;
const CHUNK_LEN: usize = 32 * 1024;
const ETAG: &str = "\"crossbow-synthetic-v1-8388608\"";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADER_FIELDS: usize = 64;
const MAX_LOG_RECORDS: usize = 10_000;

const PARENT: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Peek network fixture</title><style>body{font:16px system-ui;margin:24px;color:#202124;background:#fafafa}h1{font-size:24px}input,button,a{font:inherit}input,button{padding:8px;max-width:100%;box-sizing:border-box}label{display:block;margin:16px 0}nav{display:flex;flex-wrap:wrap;gap:8px}p{line-height:1.5}</style><h1>Peek network fixture</h1><p>Synthetic loopback page. No account or external request.</p><label>Parent draft <input id="entry" autocomplete="off"></label><nav><button id="slow">Open loading Peek</button><button id="failure">Open failure Peek</button><button id="success">Open ready Peek</button></nav><p><a id="parent-next" href="/parent-next">Parent next page</a></p><p id="result">Parent ready</p><script>for(const [id,path] of [['slow','/slow'],['failure','/flaky'],['success','/ready']])document.getElementById(id).onclick=()=>window.open(path,'_blank','popup,width=700,height=480');</script></html>"#;

const READY: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Peek ready</title><style>body{font:16px system-ui;margin:24px;color:#202124;background:#fafafa}h1{font-size:24px}input{font:inherit;padding:8px;max-width:100%;box-sizing:border-box}label{display:block;margin:16px 0}a{display:block;margin:16px 0}</style><h1 id="result">Peek loaded</h1><label>Peek draft <input id="entry" autocomplete="off"></label><a id="fail-link" href="/flaky">Navigate to failing response</a><a id="slow-link" href="/slow">Navigate to slow response</a><p>Synthetic recovery completed.</p></html>"#;

const DOWNLOADS: &str = r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Download fixture</title><style>body{font:16px system-ui;margin:24px;color:#202124;background:#fafafa;max-width:760px}h1{font-size:24px}li{margin:16px 0;line-height:1.5}a{overflow-wrap:anywhere}code{overflow-wrap:anywhere}</style><h1>Download qualification</h1><p>Synthetic loopback attachments only. Each complete file is the same deterministic 8 MiB payload; no account or external requests.</p><ul><li><a id="fixed" href="/downloads/fixed.bin">Fixed attachment</a> — full response, byte ranges supported.</li><li><a id="slow-download" href="/downloads/slow.bin">Slow attachment</a> — approximately 25.5 seconds, byte ranges supported.</li><li><a id="truncated" href="/downloads/truncated.bin">Truncated attachment</a> — advertises 8 MiB, deliberately closes after 256 KiB.</li><li><a id="resumable" href="/downloads/resumable.bin">Range-resumable attachment</a> — paced; the <code>interrupt-resumable</code> control marker truncates full responses but not accepted ranges.</li><li><a id="refusing" href="/downloads/range-refusing.bin">Range-refusing attachment</a> — paced; Range is ignored. The <code>interrupt-refusing</code> marker truncates every response until removed.</li></ul><p>A successful HTTP range request is not evidence that the browser's native Resume control works. Verify the native action and the resulting file separately.</p><p><a href="/">Peek fixture</a></p></html>"#;

#[derive(Clone, Copy)]
struct Limits {
    header_timeout: Duration,
    io_timeout: Duration,
    response_timeout: Duration,
    legacy_slow_wait: Duration,
    chunk_delay: Duration,
    workers: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            header_timeout: Duration::from_secs(2),
            io_timeout: Duration::from_secs(2),
            response_timeout: Duration::from_secs(90),
            legacy_slow_wait: Duration::from_secs(30),
            chunk_delay: Duration::from_millis(100),
            workers: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Parent,
    ParentNext,
    Ready,
    Flaky,
    LegacySlow,
    Downloads,
    Fixed,
    Slow,
    Truncated,
    Resumable,
    Refusing,
    Favicon,
    Unknown,
}

impl Route {
    fn from_target(target: &str) -> Self {
        match target.split('?').next().unwrap_or("") {
            "/" => Self::Parent,
            "/parent-next" => Self::ParentNext,
            "/ready" => Self::Ready,
            "/flaky" => Self::Flaky,
            "/slow" => Self::LegacySlow,
            "/downloads" => Self::Downloads,
            "/downloads/fixed.bin" => Self::Fixed,
            "/downloads/slow.bin" => Self::Slow,
            "/downloads/truncated.bin" => Self::Truncated,
            "/downloads/resumable.bin" => Self::Resumable,
            "/downloads/range-refusing.bin" => Self::Refusing,
            "/favicon.ico" => Self::Favicon,
            _ => Self::Unknown,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::ParentNext => "parent-next",
            Self::Ready => "ready",
            Self::Flaky => "flaky",
            Self::LegacySlow => "legacy-slow",
            Self::Downloads => "downloads",
            Self::Fixed => "fixed",
            Self::Slow => "slow-download",
            Self::Truncated => "truncated",
            Self::Resumable => "resumable",
            Self::Refusing => "range-refusing",
            Self::Favicon => "favicon",
            Self::Unknown => "unknown",
        }
    }

    fn download(self) -> Option<Download> {
        let (filename, ranges, paced) = match self {
            Self::Fixed => ("crossbow-fixed.bin", true, false),
            Self::Slow => ("crossbow-slow.bin", true, true),
            Self::Truncated => ("crossbow-truncated.bin", false, false),
            Self::Resumable => ("crossbow-resumable.bin", true, true),
            Self::Refusing => ("crossbow-range-refusing.bin", false, true),
            _ => return None,
        };
        Some(Download {
            filename,
            ranges,
            paced,
        })
    }
}

struct Download {
    filename: &'static str,
    ranges: bool,
    paced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Get,
    Head,
    Other,
}

#[derive(Debug)]
struct Request {
    method: Method,
    route: Route,
    range: Option<String>,
    if_range: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeaderError {
    BadRequest,
    TooLarge,
    Timeout,
    Disconnected,
    Stopped,
}

fn read_headers(
    stream: &mut TcpStream,
    accepted: Instant,
    limits: Limits,
    stop: &AtomicBool,
) -> Result<Vec<u8>, HeaderError> {
    let deadline = accepted + limits.header_timeout;
    let mut bytes = Vec::with_capacity(2048);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(HeaderError::Stopped);
        }
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            bytes.truncate(end + 4);
            return Ok(bytes);
        }
        if bytes.len() == MAX_HEADER_BYTES {
            return Err(HeaderError::TooLarge);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HeaderError::Timeout);
        }
        stream
            .set_read_timeout(Some(remaining.min(limits.io_timeout)))
            .map_err(|_| HeaderError::Disconnected)?;
        let mut buffer = [0; 1024];
        let available = buffer.len().min(MAX_HEADER_BYTES - bytes.len());
        match stream.read(&mut buffer[..available]) {
            Ok(0) => return Err(HeaderError::Disconnected),
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(HeaderError::Timeout);
                }
            }
            Err(_) => return Err(HeaderError::Disconnected),
        }
    }
}

fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn decimal(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

fn parse_request(bytes: &[u8]) -> Result<Request, HeaderError> {
    if bytes.len() > MAX_HEADER_BYTES {
        return Err(HeaderError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| HeaderError::BadRequest)?;
    let text = text
        .strip_suffix("\r\n\r\n")
        .ok_or(HeaderError::BadRequest)?;
    let mut lines = text.split("\r\n");
    let first = lines.next().ok_or(HeaderError::BadRequest)?;
    let mut words = first.split(' ');
    let method = words.next().ok_or(HeaderError::BadRequest)?;
    let target = words.next().ok_or(HeaderError::BadRequest)?;
    let version = words.next().ok_or(HeaderError::BadRequest)?;
    if words.next().is_some()
        || method.is_empty()
        || !method.bytes().all(token_byte)
        || target.len() > 2048
        || !target.starts_with('/')
        || !target.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        || target.contains('#')
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
    {
        return Err(HeaderError::BadRequest);
    }
    let mut host_seen = false;
    let mut length_seen = false;
    let mut range = None;
    let mut if_range = None;
    for (index, line) in lines.enumerate() {
        if index >= MAX_HEADER_FIELDS || line.len() > 4096 {
            return Err(HeaderError::TooLarge);
        }
        let (name, value) = line.split_once(':').ok_or(HeaderError::BadRequest)?;
        if name.is_empty()
            || !name.bytes().all(token_byte)
            || !value
                .bytes()
                .all(|byte| byte == b'\t' || (0x20..=0x7e).contains(&byte))
        {
            return Err(HeaderError::BadRequest);
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("host") {
            if host_seen || value.is_empty() {
                return Err(HeaderError::BadRequest);
            }
            host_seen = true;
        } else if name.eq_ignore_ascii_case("content-length") {
            if length_seen || decimal(value) != Some(0) {
                return Err(HeaderError::BadRequest);
            }
            length_seen = true;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(HeaderError::BadRequest);
        } else if name.eq_ignore_ascii_case("range") {
            if range.replace(value.to_owned()).is_some() {
                return Err(HeaderError::BadRequest);
            }
        } else if name.eq_ignore_ascii_case("if-range")
            && if_range.replace(value.to_owned()).is_some()
        {
            return Err(HeaderError::BadRequest);
        }
    }
    if version == "HTTP/1.1" && !host_seen {
        return Err(HeaderError::BadRequest);
    }
    Ok(Request {
        method: match method {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            _ => Method::Other,
        },
        route: Route::from_target(target),
        range,
        if_range,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ByteRange {
    offset: u64,
    length: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RangeError {
    Malformed,
    Unsatisfiable,
}

fn parse_range(value: &str, length: u64) -> Result<ByteRange, RangeError> {
    let spec = value.strip_prefix("bytes=").ok_or(RangeError::Malformed)?;
    let (first, last) = spec.split_once('-').ok_or(RangeError::Malformed)?;
    if first.is_empty() {
        let suffix = decimal(last).ok_or(RangeError::Malformed)?;
        if suffix == 0 || length == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        let count = suffix.min(length);
        return Ok(ByteRange {
            offset: length - count,
            length: count,
        });
    }
    let offset = decimal(first).ok_or(RangeError::Malformed)?;
    let end = if last.is_empty() {
        length.saturating_sub(1)
    } else {
        decimal(last).ok_or(RangeError::Malformed)?
    };
    if !last.is_empty() && end < offset {
        return Err(RangeError::Malformed);
    }
    if offset >= length {
        return Err(RangeError::Unsatisfiable);
    }
    Ok(ByteRange {
        offset,
        length: end.min(length - 1) - offset + 1,
    })
}

fn selected_range(request: &Request, download: &Download) -> Result<Option<ByteRange>, RangeError> {
    // Range applies only to GET. A stale, weak or date validator falls back to a full 200.
    if request.method != Method::Get
        || !download.ranges
        || request
            .if_range
            .as_deref()
            .is_some_and(|value| value != ETAG)
    {
        return Ok(None);
    }
    request
        .range
        .as_deref()
        .map(|value| parse_range(value, PAYLOAD_LEN))
        .transpose()
}

fn fill_payload(offset: u64, bytes: &mut [u8]) {
    for (index, byte) in bytes.iter_mut().enumerate() {
        let position = offset + index as u64;
        *byte = (position.wrapping_mul(31) ^ (position >> 8) ^ (position >> 16) ^ 0xa5) as u8;
    }
}

fn emit_expected(path: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(path)?;
    let mut buffer = [0; CHUNK_LEN];
    let mut offset = 0;
    while offset < PAYLOAD_LEN {
        let count = (PAYLOAD_LEN - offset).min(CHUNK_LEN as u64) as usize;
        fill_payload(offset, &mut buffer[..count]);
        output.write_all(&buffer[..count])?;
        offset += count as u64;
    }
    output.sync_all()
}

#[derive(Clone, Copy, Debug)]
struct Receipt {
    route: &'static str,
    status: u16,
    offset: u64,
    length: u64,
    written: u64,
    outcome: &'static str,
}

impl Receipt {
    fn new(route: &'static str, status: u16, length: u64) -> Self {
        Self {
            route,
            status,
            offset: 0,
            length,
            written: 0,
            outcome: "complete",
        }
    }
}

#[derive(Default)]
struct State {
    stop: AtomicBool,
    active: AtomicUsize,
    records: AtomicUsize,
    #[cfg(test)]
    receipts: std::sync::Mutex<Vec<Receipt>>,
}

impl State {
    fn record(&self, receipt: Receipt) {
        let count = self.records.fetch_add(1, Ordering::Relaxed);
        if count >= MAX_LOG_RECORDS {
            return;
        }
        println!(
            "fixture route={} status={} offset={} length={} written={} outcome={}",
            receipt.route,
            receipt.status,
            receipt.offset,
            receipt.length,
            receipt.written,
            receipt.outcome
        );
        #[cfg(test)]
        self.receipts.lock().unwrap().push(receipt);
    }
}

fn interrupted_wait(duration: Duration, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10)),
        );
    }
    !stop.load(Ordering::Relaxed)
}

fn status_line(status: u16) -> &'static str {
    match status {
        200 => "200 OK",
        206 => "206 Partial Content",
        400 => "400 Bad Request",
        404 => "404 Not Found",
        405 => "405 Method Not Allowed",
        408 => "408 Request Timeout",
        416 => "416 Range Not Satisfiable",
        431 => "431 Request Header Fields Too Large",
        _ => unreachable!(),
    }
}

fn response_headers(status: u16, length: u64, content_type: &str, extra: &str) -> String {
    format!(
        "HTTP/1.1 {}\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n{extra}\r\n",
        status_line(status)
    )
}

fn write_error(error: io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "write-timeout",
        io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::NotConnected => "disconnected",
        _ => "io-error",
    }
}

struct Connection<'a> {
    stream: TcpStream,
    limits: Limits,
    state: &'a State,
    deadline: Instant,
}

impl Connection<'_> {
    fn write(&mut self, bytes: &[u8], written: &mut u64) -> Result<(), &'static str> {
        let mut remaining = bytes;
        while !remaining.is_empty() {
            if self.state.stop.load(Ordering::Relaxed) {
                return Err("stopped");
            }
            let time = self.deadline.saturating_duration_since(Instant::now());
            if time.is_zero() {
                return Err("write-timeout");
            }
            self.stream
                .set_write_timeout(Some(time.min(self.limits.io_timeout)))
                .map_err(write_error)?;
            match self.stream.write(remaining) {
                Ok(0) => return Err("disconnected"),
                Ok(count) => {
                    *written += count as u64;
                    remaining = &remaining[count..];
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(write_error(error)),
            }
        }
        Ok(())
    }

    fn text(
        &mut self,
        route: &'static str,
        status: u16,
        body: &str,
        head: bool,
        extra: &str,
    ) -> Receipt {
        let mut receipt = Receipt::new(route, status, body.len() as u64);
        let headers = response_headers(status, receipt.length, "text/html; charset=utf-8", extra);
        if let Err(outcome) = self.write(headers.as_bytes(), &mut 0) {
            receipt.outcome = outcome;
        } else if head {
            receipt.outcome = "head";
        } else if let Err(outcome) = self.write(body.as_bytes(), &mut receipt.written) {
            receipt.outcome = outcome;
        }
        receipt
    }

    fn attachment(&mut self, request: &Request, download: Download, controls: &Path) -> Receipt {
        let selected = match selected_range(request, &download) {
            Ok(range) => range,
            Err(RangeError::Malformed) => {
                return self.text(
                    request.route.label(),
                    400,
                    "Invalid single byte range",
                    false,
                    "",
                );
            }
            Err(RangeError::Unsatisfiable) => {
                return self.text(
                    request.route.label(),
                    416,
                    "",
                    false,
                    &format!("Content-Range: bytes */{PAYLOAD_LEN}\r\nAccept-Ranges: bytes\r\nETag: {ETAG}\r\n"),
                );
            }
        };
        let range = selected.unwrap_or(ByteRange {
            offset: 0,
            length: PAYLOAD_LEN,
        });
        let status = if selected.is_some() { 206 } else { 200 };
        let mut receipt = Receipt::new(request.route.label(), status, range.length);
        receipt.offset = range.offset;
        let mut extra = format!(
            "Content-Disposition: attachment; filename=\"{}\"\r\nAccept-Ranges: {}\r\nETag: {ETAG}\r\n",
            download.filename,
            if download.ranges { "bytes" } else { "none" }
        );
        if selected.is_some() {
            extra.push_str(&format!(
                "Content-Range: bytes {}-{}/{PAYLOAD_LEN}\r\n",
                range.offset,
                range.offset + range.length - 1
            ));
        }
        let headers = response_headers(status, range.length, "application/octet-stream", &extra);
        if let Err(outcome) = self.write(headers.as_bytes(), &mut 0) {
            receipt.outcome = outcome;
            return receipt;
        }
        if request.method == Method::Head {
            receipt.outcome = "head";
            return receipt;
        }
        let truncate = status == 200
            && match request.route {
                Route::Truncated => true,
                Route::Resumable => controls.join("interrupt-resumable").is_file(),
                Route::Refusing => controls.join("interrupt-refusing").is_file(),
                _ => false,
            };
        let send_length = if truncate {
            range.length.min(TRUNCATED_LEN)
        } else {
            range.length
        };
        let mut buffer = [0; CHUNK_LEN];
        while receipt.written < send_length {
            if receipt.written > 0
                && download.paced
                && !interrupted_wait(self.limits.chunk_delay, &self.state.stop)
            {
                receipt.outcome = "stopped";
                return receipt;
            }
            let count = (send_length - receipt.written).min(CHUNK_LEN as u64) as usize;
            fill_payload(range.offset + receipt.written, &mut buffer[..count]);
            if let Err(outcome) = self.write(&buffer[..count], &mut receipt.written) {
                receipt.outcome = outcome;
                return receipt;
            }
        }
        if truncate {
            receipt.outcome = "deliberately-truncated";
        }
        receipt
    }
}

fn handle(
    mut stream: TcpStream,
    accepted: Instant,
    controls: &Path,
    limits: Limits,
    state: &State,
) -> Receipt {
    if stream.set_nonblocking(false).is_err() {
        let mut receipt = Receipt::new("request", 0, 0);
        receipt.outcome = "io-error";
        return receipt;
    }
    let request = read_headers(&mut stream, accepted, limits, &state.stop)
        .and_then(|bytes| parse_request(&bytes));
    let mut connection = Connection {
        stream,
        limits,
        state,
        deadline: Instant::now() + limits.response_timeout,
    };
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            let (status, body) = match error {
                HeaderError::BadRequest => (400, "Malformed synthetic fixture request"),
                HeaderError::TooLarge => (431, "Synthetic fixture header limit"),
                HeaderError::Timeout => (408, "Synthetic fixture header timeout"),
                HeaderError::Disconnected | HeaderError::Stopped => {
                    let mut receipt = Receipt::new("request", 0, 0);
                    receipt.outcome = if error == HeaderError::Stopped {
                        "stopped"
                    } else {
                        "disconnected"
                    };
                    return receipt;
                }
            };
            return connection.text("request", status, body, false, "");
        }
    };
    let route = request.route;
    let label = route.label();
    if request.method == Method::Other {
        return connection.text(
            label,
            405,
            "GET or HEAD required",
            false,
            "Allow: GET, HEAD\r\n",
        );
    }
    if let Some(download) = route.download() {
        return connection.attachment(&request, download, controls);
    }
    let head = request.method == Method::Head;
    match route {
        Route::Parent => connection.text(label, 200, PARENT, head, ""),
        Route::ParentNext => connection.text(
            label,
            200,
            &PARENT.replace("Parent ready", "Parent next ready"),
            head,
            "",
        ),
        Route::Ready => connection.text(label, 200, READY, head, ""),
        Route::Flaky if !controls.join("recover").is_file() => {
            let body = b"CROSSBOW_SYNTHETIC_INVALID_HTTP\r\n\r\n";
            let mut receipt = Receipt::new(label, 0, body.len() as u64);
            receipt.outcome = match connection.write(body, &mut receipt.written) {
                Ok(()) => "deliberately-malformed",
                Err(outcome) => outcome,
            };
            receipt
        }
        Route::Flaky => connection.text(label, 200, READY, head, ""),
        Route::LegacySlow => {
            let deadline = Instant::now() + limits.legacy_slow_wait;
            while !controls.join("release-slow").is_file() && Instant::now() < deadline {
                if !interrupted_wait(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(100)),
                    &state.stop,
                ) {
                    let mut receipt = Receipt::new(label, 0, 0);
                    receipt.outcome = "stopped";
                    return receipt;
                }
            }
            connection.text(label, 200, READY, head, "")
        }
        Route::Downloads => connection.text(label, 200, DOWNLOADS, head, ""),
        Route::Favicon => connection.text(label, 404, "", head, ""),
        _ => connection.text(label, 404, "Synthetic fixture route not found", head, ""),
    }
}

struct ActiveWorker(Arc<State>);

impl Drop for ActiveWorker {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

fn serve(
    listener: TcpListener,
    controls: Arc<PathBuf>,
    limits: Limits,
    state: Arc<State>,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let mut workers: Vec<thread::JoinHandle<()>> = Vec::with_capacity(limits.workers);
    let result = loop {
        let mut index = 0;
        while index < workers.len() {
            if workers[index].is_finished() {
                if workers.swap_remove(index).join().is_err() {
                    let mut receipt = Receipt::new("worker", 0, 0);
                    receipt.outcome = "panicked";
                    state.record(receipt);
                }
            } else {
                index += 1;
            }
        }
        if state.stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let accepted = Instant::now();
                if workers.len() >= limits.workers {
                    let _ = stream.shutdown(Shutdown::Both);
                    let mut receipt = Receipt::new("capacity", 0, 0);
                    receipt.outcome = "refused";
                    state.record(receipt);
                    continue;
                }
                state.active.fetch_add(1, Ordering::Relaxed);
                let active = ActiveWorker(Arc::clone(&state));
                let controls = Arc::clone(&controls);
                match thread::Builder::new()
                    .name("synthetic-http".into())
                    .spawn(move || {
                        let receipt = handle(stream, accepted, &controls, limits, &active.0);
                        active.0.record(receipt);
                    }) {
                    Ok(worker) => workers.push(worker),
                    Err(error) => break Err(error),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => break Err(error),
        }
    };
    state.stop.store(true, Ordering::Relaxed);
    for worker in workers {
        let _ = worker.join();
    }
    result
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Serve { controls: PathBuf, port: u16 },
    Emit { controls: PathBuf, output: PathBuf },
    Help,
}

fn parse_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Command, &'static str> {
    let mut args = args.into_iter();
    let first = args.next().ok_or("fixture control directory required")?;
    if first == "--help" {
        return if args.next().is_none() {
            Ok(Command::Help)
        } else {
            Err("unexpected fixture arguments")
        };
    }
    let controls = PathBuf::from(first);
    let option = args.next();
    let command = match option.as_deref().and_then(|value| value.to_str()) {
        None if option.is_none() => Command::Serve {
            controls,
            port: 18791,
        },
        Some("--port") => {
            let port = args
                .next()
                .and_then(|value| value.to_str().and_then(decimal))
                .and_then(|value| u16::try_from(value).ok())
                .ok_or("port must be an integer from 0 through 65535")?;
            Command::Serve { controls, port }
        }
        Some("--emit-expected") => Command::Emit {
            controls,
            output: PathBuf::from(args.next().ok_or("new expected-output file required")?),
        },
        _ => return Err("unknown fixture option"),
    };
    if args.next().is_some() {
        return Err("unexpected fixture arguments");
    }
    Ok(command)
}

fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    let controls = match &command {
        Command::Serve { controls, .. } | Command::Emit { controls, .. } => controls,
        Command::Help => {
            println!(
                "Usage: download_fixture EXISTING_SYNTHETIC_CONTROLS [--port PORT | --emit-expected NEW_FILE]\nDefault: 127.0.0.1:18791; port 0 requests an ephemeral loopback port.\nExpected-file mode writes exactly 8388608 synthetic bytes, refuses overwrite, and binds no socket."
            );
            return Ok(());
        }
    };
    if !controls.is_dir() {
        return Err("fixture control directory must already exist".into());
    }
    match command {
        Command::Serve { controls, port } => {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
            println!(
                "Synthetic download/Peek fixture ready on {}",
                listener.local_addr()?
            );
            serve(
                listener,
                Arc::new(controls),
                Limits::default(),
                Arc::new(State::default()),
            )?;
        }
        Command::Emit { output, .. } => {
            emit_expected(&output)?;
            println!(
                "fixture route=expected status=0 offset=0 length={PAYLOAD_LEN} written={PAYLOAD_LEN} outcome=complete"
            );
        }
        Command::Help => unreachable!(),
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    run(parse_args(std::env::args_os().skip(1))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, net::SocketAddr, sync::atomic::AtomicU64};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        address: SocketAddr,
        controls: PathBuf,
        state: Arc<State>,
        thread: Option<thread::JoinHandle<io::Result<()>>>,
    }

    impl Fixture {
        fn new(limits: Limits) -> Self {
            let root = PathBuf::from(std::env::var_os("CROSSBOW_FIXTURE_TEST_ROOT").expect(
                "set CROSSBOW_FIXTURE_TEST_ROOT to an existing disposable scratch directory",
            ));
            assert!(root.is_dir());
            let controls = root.join(format!(
                "download-fixture-{}-{}",
                std::process::id(),
                NEXT_TEST.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&controls).unwrap();
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let address = listener.local_addr().unwrap();
            assert_ne!(address.port(), 18791);
            let state = Arc::new(State::default());
            let server_state = Arc::clone(&state);
            let root = Arc::new(controls.clone());
            let thread = thread::spawn(move || serve(listener, root, limits, server_state));
            Self {
                address,
                controls,
                state,
                thread: Some(thread),
            }
        }

        fn fast() -> Self {
            Self::new(Limits {
                header_timeout: Duration::from_millis(400),
                io_timeout: Duration::from_millis(400),
                response_timeout: Duration::from_secs(4),
                legacy_slow_wait: Duration::from_millis(350),
                chunk_delay: Duration::from_millis(1),
                workers: 2,
            })
        }

        fn connect(&self) -> TcpStream {
            let stream = TcpStream::connect_timeout(&self.address, Duration::from_secs(1)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            stream
        }

        fn raw(&self, request: &[u8]) -> Vec<u8> {
            let mut stream = self.connect();
            stream.write_all(request).unwrap();
            read_to_close(&mut stream)
        }

        fn request(&self, method: &str, route: &str, headers: &str) -> Response {
            Response::parse(
                &self.raw(
                    format!(
                        "{method} {route} HTTP/1.1\r\nHost: synthetic.invalid\r\n{headers}\r\n"
                    )
                    .as_bytes(),
                ),
            )
        }

        fn marker(&self, name: &str) {
            fs::write(self.controls.join(name), b"synthetic\n").unwrap();
        }

        fn receipt(&self, label: &str) -> Receipt {
            wait_until(|| {
                self.state
                    .receipts
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| r.route == label)
            });
            *self
                .state
                .receipts
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|r| r.route == label)
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.state.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap().unwrap();
            }
            fs::remove_dir_all(&self.controls).unwrap();
        }
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !predicate() {
            assert!(Instant::now() < deadline, "finite fixture wait expired");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn read_to_close(stream: &mut TcpStream) -> Vec<u8> {
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        bytes
    }

    struct Response {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Response {
        fn parse(raw: &[u8]) -> Self {
            let split = raw.windows(4).position(|part| part == b"\r\n\r\n").unwrap();
            let text = std::str::from_utf8(&raw[..split]).unwrap();
            let mut lines = text.split("\r\n");
            let status = lines
                .next()
                .unwrap()
                .split(' ')
                .nth(1)
                .unwrap()
                .parse()
                .unwrap();
            let headers = lines
                .map(|line| {
                    let (name, value) = line.split_once(':').unwrap();
                    (name.to_ascii_lowercase(), value.trim().to_owned())
                })
                .collect();
            Self {
                status,
                headers,
                body: raw[split + 4..].to_vec(),
            }
        }

        fn header(&self, name: &str) -> &str {
            &self.headers.iter().find(|(key, _)| key == name).unwrap().1
        }

        fn declared(&self) -> usize {
            self.header("content-length").parse().unwrap()
        }
    }

    fn payload(offset: u64, length: u64) -> Vec<u8> {
        let mut bytes = vec![0; length as usize];
        fill_payload(offset, &mut bytes);
        bytes
    }

    #[test]
    fn deterministic_payload_known_bytes_and_slices() {
        assert_eq!(
            payload(0, 16),
            [
                165, 186, 155, 248, 217, 62, 31, 124, 93, 178, 147, 240, 209, 54, 23, 116
            ]
        );
        assert_eq!(payload(256, 1), [164]);
        assert_eq!(payload(65535, 2), [187, 164]);
        let all = payload(0, 100_000);
        assert_eq!(&all[32761..65538], payload(32761, 32777));
    }

    #[test]
    fn single_ranges_boundaries_suffix_overflow_and_malformed() {
        assert_eq!(
            parse_range("bytes=0-0", 10),
            Ok(ByteRange {
                offset: 0,
                length: 1
            })
        );
        assert_eq!(
            parse_range("bytes=9-99", 10),
            Ok(ByteRange {
                offset: 9,
                length: 1
            })
        );
        assert_eq!(
            parse_range("bytes=4-", 10),
            Ok(ByteRange {
                offset: 4,
                length: 6
            })
        );
        assert_eq!(
            parse_range("bytes=-3", 10),
            Ok(ByteRange {
                offset: 7,
                length: 3
            })
        );
        assert_eq!(
            parse_range("bytes=-99", 10),
            Ok(ByteRange {
                offset: 0,
                length: 10
            })
        );
        for range in ["bytes=10-", "bytes=999-1000", "bytes=-0"] {
            assert_eq!(parse_range(range, 10), Err(RangeError::Unsatisfiable));
        }
        for range in [
            "bytes=9-8",
            "bytes=",
            "bytes=-",
            "bytes=+1-2",
            "bytes=0-1,4-5",
            "items=0-1",
            "bytes=0-18446744073709551616",
        ] {
            assert_eq!(
                parse_range(range, 10),
                Err(RangeError::Malformed),
                "{range}"
            );
        }
        assert_eq!(parse_range("bytes=-1", 0), Err(RangeError::Unsatisfiable));
    }

    #[test]
    fn request_parser_rejects_ambiguous_or_unbounded_headers() {
        for input in [
            "GET / HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: a\r\n folded: b\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: a\r\nRange: bytes=0-0\r\nRange: bytes=1-1\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: a\r\nIf-Range: a\r\nIf-Range: b\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: a\r\nContent-Length: 1\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: a\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: a\r\nTransfer-Encoding: chunked\r\n\r\n",
            "GET http://synthetic.invalid/ HTTP/1.1\r\nHost: a\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: a\nBad: b\r\n\r\n",
            "GET  / HTTP/1.1\r\nHost: a\r\n\r\n",
            "GET /#fragment HTTP/1.1\r\nHost: a\r\n\r\n",
        ] {
            assert_eq!(
                parse_request(input.as_bytes()).unwrap_err(),
                HeaderError::BadRequest
            );
        }
        let many = format!(
            "GET / HTTP/1.1\r\nHost: a\r\n{}\r\n",
            "X-A: b\r\n".repeat(64)
        );
        assert_eq!(
            parse_request(many.as_bytes()).unwrap_err(),
            HeaderError::TooLarge
        );
        assert_eq!(
            parse_request(&vec![b'x'; MAX_HEADER_BYTES + 1]).unwrap_err(),
            HeaderError::TooLarge
        );
        let request =
            parse_request(b"GET /ready?ignored=synthetic HTTP/1.0\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        assert_eq!(request.route, Route::Ready);
    }

    #[test]
    fn cli_preserves_controls_first_and_rejects_extra_arguments() {
        let args = |values: &[&str]| parse_args(values.iter().map(std::ffi::OsString::from));
        assert_eq!(
            args(&["controls"]),
            Ok(Command::Serve {
                controls: "controls".into(),
                port: 18791
            })
        );
        assert_eq!(
            args(&["controls", "--port", "0"]),
            Ok(Command::Serve {
                controls: "controls".into(),
                port: 0
            })
        );
        assert_eq!(
            args(&["controls", "--emit-expected", "expected.bin"]),
            Ok(Command::Emit {
                controls: "controls".into(),
                output: "expected.bin".into()
            })
        );
        for values in [
            &[][..],
            &["controls", "--bind", "0.0.0.0"],
            &["controls", "--port", "65536"],
            &["controls", "--port", "1", "extra"],
            &["controls", "--emit-expected"],
        ] {
            assert!(args(values).is_err());
        }
    }

    #[test]
    fn loopback_full_fixed_slow_head_and_materialized_bytes() {
        let fixture = Fixture::fast();
        let expected = fixture.controls.join("expected.bin");
        emit_expected(&expected).unwrap();
        assert_eq!(
            emit_expected(&expected).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let bytes = fs::read(expected).unwrap();
        assert_eq!(bytes.len() as u64, PAYLOAD_LEN);
        for route in ["/downloads/fixed.bin", "/downloads/slow.bin"] {
            let response = fixture.request("GET", route, "");
            assert_eq!(response.status, 200);
            assert_eq!(response.declared(), bytes.len());
            assert_eq!(response.body, bytes);
            assert_eq!(response.header("etag"), ETAG);
            assert_eq!(response.header("accept-ranges"), "bytes");
            assert_eq!(response.header("content-type"), "application/octet-stream");
            assert!(
                response
                    .header("content-disposition")
                    .starts_with("attachment; filename=\"crossbow-")
            );
        }
        for route in [
            "/downloads/fixed.bin",
            "/downloads/slow.bin",
            "/downloads/truncated.bin",
            "/downloads/resumable.bin",
            "/downloads/range-refusing.bin",
        ] {
            let head = fixture.request("HEAD", route, "Range: bytes=1-1\r\n");
            assert_eq!(head.status, 200);
            assert_eq!(head.declared() as u64, PAYLOAD_LEN);
            assert!(head.body.is_empty());
        }
    }

    #[test]
    fn loopback_truncation_and_etag_guarded_resume_reconstruct_expected_file() {
        let fixture = Fixture::fast();
        let truncated = fixture.request("GET", "/downloads/truncated.bin", "");
        assert_eq!(truncated.status, 200);
        assert_eq!(truncated.declared() as u64, PAYLOAD_LEN);
        assert_eq!(truncated.body, payload(0, TRUNCATED_LEN));
        assert_eq!(
            fixture.receipt("truncated").outcome,
            "deliberately-truncated"
        );
        fixture.marker("interrupt-resumable");
        let interrupted = fixture.request("GET", "/downloads/resumable.bin", "");
        assert_eq!(interrupted.body.len() as u64, TRUNCATED_LEN);
        assert_eq!(interrupted.declared() as u64, PAYLOAD_LEN);
        let resumed = fixture.request(
            "GET",
            "/downloads/resumable.bin",
            &format!("Range: bytes={TRUNCATED_LEN}-\r\nIf-Range: {ETAG}\r\n"),
        );
        assert_eq!(resumed.status, 206);
        assert_eq!(
            resumed.header("content-range"),
            format!("bytes {TRUNCATED_LEN}-{}/{PAYLOAD_LEN}", PAYLOAD_LEN - 1)
        );
        assert_eq!(resumed.declared() as u64, PAYLOAD_LEN - TRUNCATED_LEN);
        assert_eq!(
            resumed.body,
            payload(TRUNCATED_LEN, PAYLOAD_LEN - TRUNCATED_LEN)
        );
        let output = fixture.controls.join("reconstructed.bin");
        fs::write(&output, &interrupted.body).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&output)
            .unwrap()
            .write_all(&resumed.body)
            .unwrap();
        assert_eq!(fs::read(output).unwrap(), payload(0, PAYLOAD_LEN));
    }

    #[test]
    fn loopback_single_byte_suffix_416_and_if_range_fallback() {
        let fixture = Fixture::fast();
        for (range, offset, length) in [
            ("bytes=0-0", 0, 1),
            ("bytes=8388607-", PAYLOAD_LEN - 1, 1),
            ("bytes=-7", PAYLOAD_LEN - 7, 7),
            ("bytes=8388606-9999999", PAYLOAD_LEN - 2, 2),
        ] {
            let response = fixture.request(
                "GET",
                "/downloads/fixed.bin",
                &format!("Range: {range}\r\nIf-Range: {ETAG}\r\n"),
            );
            assert_eq!(response.status, 206);
            assert_eq!(response.declared() as u64, length);
            assert_eq!(response.body, payload(offset, length));
        }
        for range in ["bytes=8388608-", "bytes=-0"] {
            let response = fixture.request(
                "GET",
                "/downloads/fixed.bin",
                &format!("Range: {range}\r\n"),
            );
            assert_eq!(response.status, 416);
            assert_eq!(
                response.header("content-range"),
                format!("bytes */{PAYLOAD_LEN}")
            );
            assert_eq!(response.declared(), 0);
            assert!(response.body.is_empty());
        }
        for validator in [
            "\"stale\"",
            "W/\"crossbow-synthetic-v1-8388608\"",
            "Wed, 21 Oct 2015 07:28:00 GMT",
        ] {
            let response = fixture.request(
                "GET",
                "/downloads/fixed.bin",
                &format!("Range: bytes=123-\r\nIf-Range: {validator}\r\n"),
            );
            assert_eq!(response.status, 200);
            assert_eq!(response.body, payload(0, PAYLOAD_LEN));
            assert!(
                !response
                    .headers
                    .iter()
                    .any(|(name, _)| name == "content-range")
            );
        }
        let malformed = fixture.request("GET", "/downloads/fixed.bin", "Range: bytes=0-1,4-5\r\n");
        assert_eq!(malformed.status, 400);
    }

    #[test]
    fn loopback_range_refusal_and_explicit_full_retry() {
        let fixture = Fixture::fast();
        fixture.marker("interrupt-refusing");
        let first = fixture.request("GET", "/downloads/range-refusing.bin", "");
        assert_eq!(first.body.len() as u64, TRUNCATED_LEN);
        let refused = fixture.request(
            "GET",
            "/downloads/range-refusing.bin",
            &format!("Range: bytes={TRUNCATED_LEN}-\r\nIf-Range: {ETAG}\r\n"),
        );
        assert_eq!(refused.status, 200);
        assert_eq!(refused.header("accept-ranges"), "none");
        assert_eq!(refused.declared() as u64, PAYLOAD_LEN);
        assert_eq!(refused.body, first.body);
        assert!(
            !refused
                .headers
                .iter()
                .any(|(name, _)| name == "content-range")
        );
        fs::remove_file(fixture.controls.join("interrupt-refusing")).unwrap();
        let retry = fixture.request(
            "GET",
            "/downloads/range-refusing.bin",
            "Range: bytes=999999999-\r\n",
        );
        assert_eq!(retry.status, 200);
        assert_eq!(retry.body, payload(0, PAYLOAD_LEN));
    }

    #[test]
    fn loopback_fragmented_complete_headers_and_malformed_requests() {
        let fixture = Fixture::fast();
        let mut stream = fixture.connect();
        for fragment in [
            b"GET /downloads/fixed.bin HTTP/1.1\r\nHo".as_slice(),
            b"st: synthetic.invalid\r\nRa",
            b"nge: bytes=2-4\r\n\r",
            b"\n",
        ] {
            stream.write_all(fragment).unwrap();
            thread::sleep(Duration::from_millis(10));
        }
        let response = Response::parse(&read_to_close(&mut stream));
        assert_eq!(response.status, 206);
        assert_eq!(response.body, payload(2, 3));
        let malformed = fixture.raw(b"GET / HTTP/1.1\r\nHost: a\r\nNo-Colon\r\n\r\n");
        assert_eq!(Response::parse(&malformed).status, 400);
        let method = fixture.request("POST", "/downloads", "");
        assert_eq!(method.status, 405);
        assert_eq!(method.header("allow"), "GET, HEAD");
        let mut oversized = b"GET / HTTP/1.1\r\nHost: a\r\nX-Large: ".to_vec();
        oversized.resize(MAX_HEADER_BYTES, b'x');
        assert_eq!(Response::parse(&fixture.raw(&oversized)).status, 431);
    }

    #[test]
    fn loopback_header_timeout_is_absolute_not_reset_by_fragments() {
        let fixture = Fixture::fast();
        let started = Instant::now();
        let mut stream = fixture.connect();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: a\r\nX-Slow: ")
            .unwrap();
        for _ in 0..3 {
            thread::sleep(Duration::from_millis(90));
            stream.write_all(b"x").unwrap();
        }
        let response = Response::parse(&read_to_close(&mut stream));
        assert_eq!(response.status, 408);
        assert!(started.elapsed() < Duration::from_millis(650));
    }

    #[test]
    fn loopback_cancel_disconnect_releases_worker_and_stops_payload() {
        let fixture = Fixture::fast();
        let mut stream = fixture.connect();
        stream
            .write_all(b"GET /downloads/slow.bin HTTP/1.1\r\nHost: a\r\n\r\n")
            .unwrap();
        let mut first = [0; 4096];
        assert!(stream.read(&mut first).unwrap() > 0);
        stream.shutdown(Shutdown::Both).unwrap();
        drop(stream);
        let receipt = fixture.receipt("slow-download");
        // macOS may fail the next timeout setsockopt rather than the next write.
        // Require a terminal I/O failure and early worker release, not one errno.
        assert!(matches!(receipt.outcome, "disconnected" | "io-error"));
        assert!(receipt.written < PAYLOAD_LEN);
        wait_until(|| fixture.state.active.load(Ordering::Relaxed) == 0);
        assert_eq!(fixture.request("GET", "/ready", "").status, 200);
    }

    #[test]
    fn loopback_default_pacing_and_limits_are_preserved() {
        let limits = Limits::default();
        assert_eq!(limits.workers, 8);
        assert_eq!(limits.header_timeout, Duration::from_secs(2));
        assert_eq!(limits.io_timeout, Duration::from_secs(2));
        assert_eq!(limits.response_timeout, Duration::from_secs(90));
        assert_eq!(limits.legacy_slow_wait, Duration::from_secs(30));
        let fixture = Fixture::new(limits);
        let started = Instant::now();
        let response = fixture.request("GET", "/downloads/slow.bin", "Range: bytes=0-65535\r\n");
        assert_eq!(response.status, 206);
        assert_eq!(response.body, payload(0, 65536));
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn loopback_response_deadline_closes_a_paced_transfer() {
        let fixture = Fixture::new(Limits {
            response_timeout: Duration::from_millis(80),
            chunk_delay: Duration::from_millis(10),
            ..Limits::default()
        });
        let response = fixture.request("GET", "/downloads/slow.bin", "");
        assert_eq!(response.status, 200);
        assert_eq!(response.declared() as u64, PAYLOAD_LEN);
        assert!(!response.body.is_empty());
        assert!(response.body.len() < PAYLOAD_LEN as usize);
        assert_eq!(response.body, payload(0, response.body.len() as u64));
        assert_eq!(fixture.receipt("slow-download").outcome, "write-timeout");
        wait_until(|| fixture.state.active.load(Ordering::Relaxed) == 0);
    }

    #[test]
    fn loopback_stalled_receiver_hits_the_write_timeout() {
        let fixture = Fixture::new(Limits {
            io_timeout: Duration::from_millis(80),
            response_timeout: Duration::from_secs(2),
            ..Limits::default()
        });
        let mut stream = fixture.connect();
        stream
            .write_all(b"GET /downloads/fixed.bin HTTP/1.1\r\nHost: a\r\n\r\n")
            .unwrap();
        // Leave the receive side unread so the TCP send window fills.
        let receipt = fixture.receipt("fixed");
        assert_eq!(receipt.outcome, "write-timeout");
        assert!(receipt.written < PAYLOAD_LEN);
        drop(stream);
        wait_until(|| fixture.state.active.load(Ordering::Relaxed) == 0);
    }

    #[test]
    fn controls_must_exist_and_control_files_are_never_served() {
        let fixture = Fixture::fast();
        let missing = fixture.controls.join("missing");
        let error = run(Command::Serve {
            controls: missing,
            port: 0,
        })
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "fixture control directory must already exist"
        );
        fixture.marker("not-public.bin");
        let response = fixture.request("GET", "/not-public.bin", "");
        assert_eq!(response.status, 404);
        assert_eq!(response.body, b"Synthetic fixture route not found");
    }

    #[test]
    fn loopback_worker_concurrency_is_bounded_and_excess_connections_close() {
        let fixture = Fixture::fast();
        let first = fixture.connect();
        let second = fixture.connect();
        wait_until(|| fixture.state.active.load(Ordering::Relaxed) == 2);
        let mut excess = fixture.connect();
        let mut byte = [0];
        let read = excess.read(&mut byte);
        assert!(
            matches!(read, Ok(0))
                || read.is_err_and(|error| error.kind() == io::ErrorKind::ConnectionReset)
        );
        assert_eq!(fixture.receipt("capacity").outcome, "refused");
        assert_eq!(fixture.state.active.load(Ordering::Relaxed), 2);
        drop(first);
        drop(second);
        wait_until(|| fixture.state.active.load(Ordering::Relaxed) == 0);
    }

    #[test]
    fn loopback_old_routes_marker_recovery_and_delayed_release() {
        let fixture = Fixture::fast();
        for (route, body) in [
            ("/", PARENT.to_owned()),
            (
                "/parent-next",
                PARENT.replace("Parent ready", "Parent next ready"),
            ),
            ("/ready?fixture=query", READY.to_owned()),
        ] {
            let response = fixture.request("GET", route, "");
            assert_eq!(response.status, 200);
            assert_eq!(response.body, body.as_bytes());
        }
        assert_eq!(
            fixture.raw(b"GET /flaky HTTP/1.1\r\nHost: a\r\n\r\n"),
            b"CROSSBOW_SYNTHETIC_INVALID_HTTP\r\n\r\n"
        );
        fixture.marker("recover");
        assert_eq!(fixture.request("GET", "/flaky", "").body, READY.as_bytes());
        let mut slow = fixture.connect();
        slow.write_all(b"GET /slow HTTP/1.1\r\nHost: a\r\n\r\n")
            .unwrap();
        slow.set_read_timeout(Some(Duration::from_millis(60)))
            .unwrap();
        let mut byte = [0];
        assert!(slow.read(&mut byte).is_err_and(|error| matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        )));
        fixture.marker("release-slow");
        slow.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert_eq!(
            Response::parse(&read_to_close(&mut slow)).body,
            READY.as_bytes()
        );
        fs::remove_file(fixture.controls.join("release-slow")).unwrap();
        let started = Instant::now();
        assert_eq!(fixture.request("GET", "/slow", "").body, READY.as_bytes());
        assert!(started.elapsed() >= Duration::from_millis(350));
        assert_eq!(fixture.request("GET", "/favicon.ico", "").status, 404);
        assert_eq!(
            fixture
                .request("GET", "/../private-owner-data?secret=ignored", "")
                .status,
            404
        );
        assert_eq!(fixture.receipt("unknown").route, "unknown");
        let page = fixture.request("GET", "/downloads", "");
        assert_eq!(page.status, 200);
        assert_eq!(page.body, DOWNLOADS.as_bytes());
    }
}
