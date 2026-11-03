//! The sync server a build is made for. `CROSSBOW_API_URL` at compile time
//! names it; a build without it (or with it empty) has no sync server, and the
//! app offers no sign-in until the user enters a server of their own.
//!
//! The value must be an `https://` URL, or `http://` on 127.0.0.1, `[::1]` or
//! localhost. Anything else fails the build.

/// The sync server this build was made for, if any.
pub const BUILD_URL: Option<&str> = build_url(option_env!("CROSSBOW_API_URL"));

/// What the app says when it has no sync server to offer.
pub const NO_SERVER: &str = "This build has no sync server.";

/// The build's value checked: `None` when unset or empty, and a compile-time
/// panic (so a failed build) when it isn't a server address [`check`] accepts.
pub const fn build_url(value: Option<&'static str>) -> Option<&'static str> {
    match value {
        None => None,
        Some(url) if url.is_empty() => None,
        Some(url) => match check(url) {
            Ok(()) => Some(url),
            Err(_) => panic!("CROSSBOW_API_URL must be an https:// URL, or an http:// URL on 127.0.0.1, [::1] or localhost, with no user, query or fragment"),
        },
    }
}

/// Whether `url` is a sync server address: `https://host[:port][/path]`, or
/// the same with `http://` on a loopback host. Why not, otherwise.
pub const fn check(url: &str) -> Result<(), &'static str> {
    let b = url.as_bytes();
    let (secure, start) = if starts_with(b, b"https://", 0) {
        (true, 8)
    } else if starts_with(b, b"http://", 0) {
        (false, 7)
    } else {
        return Err("the address must start with https://");
    };
    let mut i = start;
    if i < b.len() && b[i] == b'[' {
        while i < b.len() && b[i] != b']' {
            i += 1;
        }
        if i == b.len() {
            return Err("the address has an unclosed [");
        }
        i += 1;
    } else {
        while i < b.len() && b[i] != b'/' && b[i] != b':' {
            if !(b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'-') {
                return Err("the host name has a character a host name can't have");
            }
            i += 1;
        }
    }
    let host_end = i;
    if host_end == start {
        return Err("the address has no host");
    }
    if i < b.len() && b[i] == b':' {
        i += 1;
        let mut port: u32 = 0;
        let digits = i;
        while i < b.len() && b[i].is_ascii_digit() && i - digits < 5 {
            port = port * 10 + (b[i] - b'0') as u32;
            i += 1;
        }
        if i == digits || port == 0 || port > 65_535 || (i < b.len() && b[i] != b'/') {
            return Err("the port must be a number from 1 to 65535");
        }
    }
    if i < b.len() && b[i] != b'/' {
        return Err("the host must be followed by a port or a path");
    }
    while i < b.len() {
        if b[i] <= b' ' || b[i] == 0x7f || b[i] == b'?' || b[i] == b'#' {
            return Err("the path has a space, control character, query or fragment");
        }
        i += 1;
    }
    if !secure && !is_loopback(b, start, host_end) {
        return Err("plain http:// is allowed only on 127.0.0.1, [::1] or localhost");
    }
    Ok(())
}

const fn is_loopback(b: &[u8], start: usize, end: usize) -> bool {
    let hosts: [&[u8]; 3] = [b"127.0.0.1", b"[::1]", b"localhost"];
    let mut n = 0;
    while n < hosts.len() {
        if end - start == hosts[n].len() && starts_with(b, hosts[n], start) {
            return true;
        }
        n += 1;
    }
    false
}

const fn starts_with(b: &[u8], prefix: &[u8], at: usize) -> bool {
    if b.len() < at + prefix.len() {
        return false;
    }
    let mut i = 0;
    while i < prefix.len() {
        if b[at + i].to_ascii_lowercase() != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_anywhere_and_http_only_on_loopback_are_servers() {
        for url in [
            "https://sync.example.com",
            "https://sync.example.com/",
            "https://sync.example.com:8443/base",
            "HTTPS://Sync.Example.com",
            "https://[2001:db8::1]:443",
            "http://127.0.0.1:8787",
            "http://localhost:8787/",
            "http://[::1]:1",
        ] {
            assert_eq!(check(url), Ok(()), "{url}");
        }
    }

    #[test]
    fn plain_http_elsewhere_and_malformed_addresses_are_refused() {
        for (url, why) in [
            ("http://sync.example.com", "plain http:// is allowed only on 127.0.0.1, [::1] or localhost"),
            ("http://127.0.0.2:8787", "plain http:// is allowed only on 127.0.0.1, [::1] or localhost"),
            ("http://localhost.example.com", "plain http:// is allowed only on 127.0.0.1, [::1] or localhost"),
            ("ftp://sync.example.com", "the address must start with https://"),
            ("sync.example.com", "the address must start with https://"),
            ("https://", "the address has no host"),
            ("https://:443", "the address has no host"),
            ("https://user@sync.example.com", "the host name has a character a host name can't have"),
            ("https://sync example.com", "the host name has a character a host name can't have"),
            ("https://sync.example.com:0", "the port must be a number from 1 to 65535"),
            ("https://sync.example.com:65536", "the port must be a number from 1 to 65535"),
            ("https://sync.example.com:443443", "the port must be a number from 1 to 65535"),
            ("https://sync.example.com:x", "the port must be a number from 1 to 65535"),
            ("https://[::1", "the address has an unclosed ["),
            ("https://[::1]x", "the host must be followed by a port or a path"),
            ("https://sync.example.com/a b", "the path has a space, control character, query or fragment"),
            ("https://sync.example.com/?next=x", "the path has a space, control character, query or fragment"),
        ] {
            assert_eq!(check(url), Err(why), "{url}");
        }
    }

    #[test]
    fn an_unset_or_empty_build_value_means_no_server() {
        assert_eq!(build_url(None), None);
        assert_eq!(build_url(Some("")), None);
        assert_eq!(build_url(Some("https://sync.example.com")), Some("https://sync.example.com"));
    }

    #[test]
    #[should_panic(expected = "CROSSBOW_API_URL must be an https:// URL")]
    fn a_plain_http_build_value_off_loopback_fails() {
        build_url(Some("http://sync.example.com"));
    }
}
