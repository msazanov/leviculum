//! A small HTTP/1.1 server of files held in memory, answering range
//! requests, on a loopback port it binds itself: the map source of the
//! tests (1089), which must not touch the network. One thread accepts,
//! one serves each connection; every response closes its connection.
//!
//! Not a server for anything else: no keep-alive, no `HEAD`, one range per
//! request, which is all [`crate::http::HttpRange`] sends.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// A running server; it stops when dropped.
pub struct Serve {
    base: String,
    shared: Arc<Shared>,
}

struct Shared {
    files: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    requests: AtomicUsize,
    /// Answer every request with `503`, for a source that fails.
    failing: AtomicBool,
    stop: AtomicBool,
}

impl Serve {
    /// Serve `files` (path without the leading slash, bytes) on a port of
    /// 127.0.0.1 bound here and held.
    pub fn start(files: Vec<(String, Vec<u8>)>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let base = format!("http://{}/", listener.local_addr()?);
        let shared = Arc::new(Shared {
            files: Mutex::new(files.into_iter().map(|(p, b)| (p, Arc::new(b))).collect()),
            requests: AtomicUsize::new(0),
            failing: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let theirs = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("map-serve".to_owned())
            .spawn(move || {
                for stream in listener.incoming() {
                    if theirs.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = Arc::clone(&theirs);
                    let _ = std::thread::Builder::new()
                        .name("map-serve-conn".to_owned())
                        .spawn(move || {
                            let _ = answer(&shared, stream);
                        });
                }
            })?;
        Ok(Self { base, shared })
    }

    /// `http://127.0.0.1:<port>/`.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The URL of `path`.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Requests answered so far.
    pub fn requests(&self) -> usize {
        self.shared.requests.load(Ordering::SeqCst)
    }

    /// Answer `503` from now on, or serve again.
    pub fn fail(&self, failing: bool) {
        self.shared.failing.store(failing, Ordering::SeqCst);
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let addr = self
            .base
            .trim_start_matches("http://")
            .trim_end_matches('/');
        let _ = TcpStream::connect(addr);
    }
}

fn answer(shared: &Shared, mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .trim_start_matches('/')
        .to_owned();
    let mut range: Option<(u64, Option<u64>)> = None;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        let Some((k, v)) = h.split_once(':') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("range") {
            range = v.trim().strip_prefix("bytes=").and_then(|r| {
                let (a, b) = r.split_once('-')?;
                Some((a.parse().ok()?, b.parse().ok()))
            });
        }
    }
    shared.requests.fetch_add(1, Ordering::SeqCst);
    if shared.failing.load(Ordering::SeqCst) {
        return write!(
            stream,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    }
    let file = shared
        .files
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&path)
        .cloned();
    let Some(file) = file else {
        return write!(
            stream,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    };
    let len = file.len() as u64;
    match range {
        Some((start, end)) if start < len => {
            let end = end.unwrap_or(len - 1).min(len - 1);
            let body = &file[start as usize..=end as usize];
            write!(
                stream,
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{len}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            stream.write_all(body)
        }
        Some(_) => write!(
            stream,
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nContent-Range: bytes */{len}\r\nConnection: close\r\n\r\n"
        ),
        None => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
            )?;
            stream.write_all(&file)
        }
    }
}
