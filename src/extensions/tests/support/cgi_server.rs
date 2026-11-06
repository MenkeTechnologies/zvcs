//! A smart-HTTP server for tests: a `std::net::TcpListener` that runs stock
//! `git http-backend` for each request, as the `*_parity_http_*` tests do, and
//! can cut short the response that carries a pack.
//!
//! Each file includes this with
//!
//! ```ignore
//! #[path = "support/cgi_server.rs"]
//! mod cgi_server;
//! ```

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Where in the body of the response that carries a pack the server stops.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub enum CutAt {
    /// At the start of the pkt-line that carries `PACK`, a pkt-line boundary.
    BeforePack,
    /// Inside the twelve-byte pack header.
    InHeader,
    /// Half-way through the body.
    Half,
    /// Short of the closing flush packet only.
    LastFlush,
}

/// How the cut response ends.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub enum CutHow {
    /// `Content-Length` of the whole body, then the connection is closed.
    Close,
    /// `Content-Length` of the whole body, then the connection is reset.
    Reset,
    /// A `Content-Length` of only what is sent, on a connection that stays open.
    ShortLength,
}

/// The fault the server applies, `None` for none.
#[derive(Clone, Copy, Debug)]
pub struct Fault {
    pub cut: Option<(CutAt, CutHow)>,
    /// Cut only the first response that carries a pack.
    pub once: bool,
}

#[allow(dead_code)]
impl Fault {
    pub const NONE: Fault = Fault { cut: None, once: false };

    pub fn cut(at: CutAt, how: CutHow) -> Fault {
        Fault {
            cut: Some((at, how)),
            once: false,
        }
    }
}

/// One HTTP request off `stream`: method, target, lower-cased headers, body.
fn read_request(reader: &mut BufReader<&TcpStream>) -> Option<(String, String, Vec<(String, String)>, Vec<u8>)> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
    }
    let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let mut body = Vec::new();
    if let Some(len) = header("content-length") {
        let mut buf = vec![0; len.parse().ok()?];
        reader.read_exact(&mut buf).ok()?;
        body = buf;
    } else if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).ok()?;
            let size = usize::from_str_radix(size.trim().split(';').next()?, 16).ok()?;
            let mut buf = vec![0; size + 2];
            reader.read_exact(&mut buf).ok()?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&buf[..size]);
        }
    }
    Some((method, target, headers, body))
}

/// Close `stream` with a RST instead of a FIN: `SO_LINGER` with a zero timeout.
fn reset(stream: &TcpStream) {
    let linger = libc::linger { l_onoff: 1, l_linger: 0 };
    // SAFETY: a valid socket and a `linger` that outlives the call.
    unsafe {
        libc::setsockopt(
            std::os::fd::AsRawFd::as_raw_fd(stream),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&raw const linger).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
}

/// Serve the requests of one connection through stock `git http-backend`.
fn serve(stock: &str, stream: TcpStream, root: &Path, fault: Fault, cut_done: &AtomicBool, posts: &AtomicUsize) {
    let mut reader = BufReader::new(&stream);
    while let Some((method, target, headers, body)) = read_request(&mut reader) {
        if method == "POST" {
            posts.fetch_add(1, Ordering::SeqCst);
        }
        let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let mut child = Command::new(stock)
            .arg("http-backend")
            .env("GIT_PROJECT_ROOT", root)
            .env("GIT_HTTP_EXPORT_ALL", "1")
            .env("PATH_INFO", path)
            .env("QUERY_STRING", query)
            .env("REQUEST_METHOD", &method)
            .env("REMOTE_USER", "user")
            .env("REMOTE_ADDR", "127.0.0.1")
            .env("CONTENT_TYPE", header("content-type").unwrap_or_default())
            .env("CONTENT_LENGTH", body.len().to_string())
            .env("HTTP_CONTENT_ENCODING", header("content-encoding").unwrap_or_default())
            .env("GIT_PROTOCOL", header("git-protocol").unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("stock http-backend runs");
        child.stdin.take().unwrap().write_all(&body).ok();
        let cgi = child.wait_with_output().expect("http-backend finishes").stdout;
        let split = cgi.windows(4).position(|w| w == b"\r\n\r\n").map(|p| (p, 4));
        let split = split.or_else(|| cgi.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2)));
        let (head, rest) = match split {
            Some((p, n)) => (&cgi[..p], &cgi[p + n..]),
            None => (&cgi[..], &[][..]),
        };
        let mut status = "200 OK".to_owned();
        let mut response = String::new();
        for line in String::from_utf8_lossy(head).lines() {
            match line.split_once(':') {
                Some((k, v)) if k.eq_ignore_ascii_case("status") => status = v.trim().to_owned(),
                Some(_) => response.push_str(&format!("{line}\r\n")),
                None => {}
            }
        }
        let mut out = &stream;
        let pack = rest.windows(4).position(|w| w == b"PACK");
        let cut = match (fault.cut, pack) {
            (Some(cut), Some(at)) if !(fault.once && cut_done.swap(true, Ordering::SeqCst)) => Some((cut, at)),
            _ => None,
        };
        let Some(((at, how), pack_at)) = cut else {
            let head = format!("HTTP/1.1 {status}\r\n{response}Content-Length: {}\r\n\r\n", rest.len());
            out.write_all(head.as_bytes()).ok();
            out.write_all(rest).ok();
            continue;
        };
        let n = match at {
            // The four length digits and the band byte in front of `PACK`.
            CutAt::BeforePack => pack_at - 5,
            CutAt::InHeader => pack_at + 6,
            CutAt::Half => rest.len() / 2,
            CutAt::LastFlush => rest.len() - 4,
        };
        let length = if matches!(how, CutHow::ShortLength) { n } else { rest.len() };
        let head = format!("HTTP/1.1 {status}\r\n{response}Content-Length: {length}\r\n\r\n");
        out.write_all(head.as_bytes()).ok();
        out.write_all(&rest[..n]).ok();
        match how {
            CutHow::ShortLength => continue,
            CutHow::Reset => return reset(&stream),
            CutHow::Close => return,
        }
    }
}

/// Serve the repositories under `root` on a fresh port, applying `fault`. Returns the port and the count of
/// `POST`s served so far.
pub fn spawn(stock: &str, root: PathBuf, fault: Fault) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stock = stock.to_owned();
    let cut_done = Arc::new(AtomicBool::new(false));
    let posts = Arc::new(AtomicUsize::new(0));
    let counted = posts.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (root, stock, cut_done, counted) = (root.clone(), stock.clone(), cut_done.clone(), counted.clone());
            std::thread::spawn(move || serve(&stock, stream, &root, fault, &cut_done, &counted));
        }
    });
    (port, posts)
}
