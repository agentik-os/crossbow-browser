//! Test fixtures: keys made at run time, fake bundles, hand-built archives,
//! a stand-in for the Security framework and a loopback HTTP server.

use super::codesign::CodeCheck;
use super::feed::{self, Archive, Release};
use super::feed::SigningKey;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A fresh key pair and its public key.
pub fn keypair() -> (SigningKey, [u8; 32]) {
    let (key, _) = SigningKey::generate().unwrap();
    let public = key.public();
    (key, public)
}

pub fn info_plist(version: &str, build: u64, minimum_macos: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key><string>crossbow</string>
	<key>CFBundleIdentifier</key><string>app.crossbow.Crossbow</string>
	<key>CFBundleShortVersionString</key><string>{version}</string>
	<key>CFBundleVersion</key><string>{build}</string>
	<key>LSMinimumSystemVersion</key><string>{minimum_macos}</string>
</dict>
</plist>
"#
    )
}

/// A shell script that answers the probe as `version build`.
pub fn probe_script(version: &str, build: u64) -> String {
    format!("#!/bin/sh\n[ \"$1\" = \"--update-probe\" ] && printf '%s %s\\n' '{version}' '{build}' > \"$2\"\n")
}

/// A small `Crossbow.app` at `app`: Info.plist, an executable that answers
/// the probe, a resource and a versioned framework with its symlinks.
pub fn fake_app(app: &Path, version: &str, build: u64) {
    use std::os::unix::fs::PermissionsExt;
    let contents = app.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    std::fs::create_dir_all(contents.join("Resources")).unwrap();
    std::fs::create_dir_all(contents.join("Frameworks/Engine.framework/Versions/A")).unwrap();
    std::fs::write(contents.join("Info.plist"), info_plist(version, build, "11.0")).unwrap();
    let executable = contents.join("MacOS/crossbow");
    std::fs::write(&executable, probe_script(version, build)).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(contents.join("Resources/notes.txt"), format!("Crossbow {version}\n")).unwrap();
    std::fs::write(contents.join("Frameworks/Engine.framework/Versions/A/Engine"), b"engine").unwrap();
    std::os::unix::fs::symlink("A", contents.join("Frameworks/Engine.framework/Versions/Current")).unwrap();
    std::os::unix::fs::symlink("Versions/Current/Engine", contents.join("Frameworks/Engine.framework/Engine")).unwrap();
}

/// A packed fake app's bytes.
pub fn fake_archive(dir: &Path, version: &str, build: u64) -> Vec<u8> {
    let root = dir.join(format!("pack-{version}-{build}"));
    fake_app(&root.join("Crossbow.app"), version, build);
    let out = root.join("update.tar.gz");
    super::archive::pack(&root.join("Crossbow.app"), &out).unwrap();
    std::fs::read(out).unwrap()
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).as_slice().try_into().unwrap()
}

/// A day into the fixture releases' fortnight: when tests judge them.
pub fn now() -> u64 {
    feed::parse_time("2026-11-02T11:23:42Z").unwrap()
}

/// A release of `archive` at `url`, its archive signed with `key`.
pub fn release(url: &str, archive: &[u8], key: &SigningKey, version: &str, build: u64) -> Release {
    let digest = sha256(archive);
    Release {
        channel: feed::CHANNEL.into(),
        version: version.into(),
        build,
        minimum_macos: "11.0".into(),
        published: "2026-11-01T11:23:42Z".into(),
        expires: "2026-11-15T11:23:42Z".into(),
        notes: "Faster tabs.\nFewer crashes.".into(),
        archive: Archive {
            url: url.into(),
            size: archive.len() as u64,
            sha256: data_encoding::HEXLOWER.encode(&digest),
            signature: feed::sign_archive(key, &digest, archive.len() as u64),
        },
    }
}

/// One hand-made tar entry, with any path at all.
#[derive(Clone, Copy)]
pub enum Raw<'a> {
    File(&'a str, &'a [u8]),
    Dir(&'a str),
    Symlink(&'a str, &'a str),
    HardLink(&'a str, &'a str),
    Fifo(&'a str),
}

/// A `.tar.gz` of `entries`, paths written as given, unchecked.
pub fn raw_tar_gz(entries: &[Raw]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        let (path, kind, body, link): (&str, tar::EntryType, &[u8], Option<&str>) = match entry {
            Raw::File(path, body) => (path, tar::EntryType::Regular, body, None),
            Raw::Dir(path) => (path, tar::EntryType::Directory, b"", None),
            Raw::Symlink(path, target) => (path, tar::EntryType::Symlink, b"", Some(target)),
            Raw::HardLink(path, target) => (path, tar::EntryType::Link, b"", Some(target)),
            Raw::Fifo(path) => (path, tar::EntryType::Fifo, b"", None),
        };
        let name = &mut header.as_old_mut().name;
        name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_entry_type(kind);
        header.set_mode(if kind == tar::EntryType::Directory { 0o755 } else { 0o644 });
        header.set_size(body.len() as u64);
        if let Some(link) = link {
            header.set_link_name_literal(link).unwrap();
        }
        header.set_cksum();
        builder.append(&header, body).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// The entries of a valid minimal bundle, to add hostile ones to.
pub fn minimal_bundle(version: &str, build: u64) -> (String, String) {
    (info_plist(version, build, "11.0"), probe_script(version, build))
}

/// A stand-in for the Security framework.
#[derive(Clone, Debug)]
pub struct FakeCode {
    pub team: Option<String>,
    pub valid: bool,
    pub signed_by: String,
}

impl FakeCode {
    pub fn signed(team: &str) -> FakeCode {
        FakeCode { team: Some(team.into()), valid: true, signed_by: team.into() }
    }
}

impl CodeCheck for FakeCode {
    fn running_team(&self) -> Option<String> {
        self.team.clone()
    }

    fn signer(&self, _app: &Path) -> Option<String> {
        Some(self.signed_by.clone()).filter(|team| !team.is_empty())
    }

    fn valid(&self, _app: &Path) -> bool {
        self.valid
    }

    fn meets(&self, _app: &Path, team: &str) -> bool {
        self.signed_by == team
    }
}

/// How the loopback server answers one path.
#[derive(Clone)]
pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
    /// Send this many bytes, then hold the connection open until the
    /// server is dropped.
    pub stall_after: Option<usize>,
    /// Send the body one byte at a time, this long apart.
    pub trickle: Option<Duration>,
}

impl Answer {
    pub fn ok(body: impl Into<Vec<u8>>) -> Answer {
        Answer { status: 200, body: body.into(), stall_after: None, trickle: None }
    }

    pub fn status(status: u16) -> Answer {
        Answer { status, body: b"error".to_vec(), stall_after: None, trickle: None }
    }
}

/// An HTTP/1.1 server on 127.0.0.1 that answers fixed paths; every
/// request's path is recorded.
pub struct Server {
    pub base: String,
    pub requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
}

impl Server {
    pub fn start(answers: HashMap<String, Answer>) -> Server {
        Server::listen(TcpListener::bind("127.0.0.1:0").unwrap(), answers)
    }

    /// On the address in `base` (from [`free_base`]).
    pub fn start_at(base: &str, answers: HashMap<String, Answer>) -> Server {
        Server::listen(TcpListener::bind(base.trim_start_matches("http://")).unwrap(), answers)
    }

    fn listen(listener: TcpListener, answers: HashMap<String, Answer>) -> Server {
        let addr = listener.local_addr().unwrap();
        let (stop, requests) = (Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(Vec::new())));
        let (stopping, recorded) = (stop.clone(), requests.clone());
        let answers = Arc::new(answers);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (answers, recorded, stopping) = (answers.clone(), recorded.clone(), stopping.clone());
                std::thread::spawn(move || serve(stream, &answers, &recorded, &stopping));
            }
        });
        Server { base: format!("http://{addr}"), requests, stop, addr }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }
}

fn serve(mut stream: TcpStream, answers: &HashMap<String, Answer>, recorded: &Mutex<Vec<String>>, stopping: &AtomicBool) {
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while !request.ends_with(b"\r\n\r\n") && request.len() < 16 * 1024 {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            return;
        }
        request.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&request);
    let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
    recorded.lock().unwrap().push(path.clone());
    let answer = answers.get(&path).cloned().unwrap_or(Answer::status(404));
    let head = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", answer.status, answer.body.len());
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    if let Some(every) = answer.trickle {
        for byte in &answer.body {
            if stopping.load(Ordering::SeqCst) || stream.write_all(std::slice::from_ref(byte)).is_err() {
                return;
            }
            let _ = stream.flush();
            std::thread::sleep(every);
        }
        return;
    }
    let sent = answer.stall_after.unwrap_or(answer.body.len()).min(answer.body.len());
    if stream.write_all(&answer.body[..sent]).is_err() {
        return;
    }
    let _ = stream.flush();
    if answer.stall_after.is_some() {
        while !stopping.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A loopback base URL whose port was free a moment ago.
pub fn free_base() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

/// A loopback address nothing listens on.
pub fn closed_port() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}
