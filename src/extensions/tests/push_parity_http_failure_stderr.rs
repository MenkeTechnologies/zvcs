//! `git push`, `git fetch`, `git ls-remote` and `git clone` against an `http://`
//! remote whose requests fail, compared byte for byte with stock git.
//!
//! Stock smart HTTP is three processes: the command with transport-helper.c,
//! remote-curl, and for v0/v1 a `send-pack`/`fetch-pack` under remote-curl.
//! A failed `POST` never reaches the reader as an error: remote-curl reports
//! it (`run_slot()`'s `RPC failed`, remote-curl.c:848-876, or the `die()` of a
//! `credential_fill()` nobody answered) and leaves the reader at the end of
//! its pipe, after a `0002` for a v2 request (remote-curl.c:1086-1087). What
//! follows is each reader's own end-of-file handling:
//!
//! * send-pack: `send-pack: unexpected disconnect while reading sideband
//!   packet` (sideband.c:325-331), `fatal: the remote end hung up
//!   unexpectedly` (pkt-line.c:364-372), and then the parent: a remote-curl
//!   that survived answers with no statuses and exits 128, so `Everything
//!   up-to-date` and exit 1; a dead one never answers, so `error: failed to
//!   push some refs` (transport-helper.c:946-958, transport.c:1560-1564);
//! * a v2 `ls-refs`: `expected flush after ref listing` (connect.c:609-615);
//! * a v0 fetch-pack: `the remote end hung up unexpectedly`.
//!
//! A refused ref advertisement is `discover_refs()`'s `die()` after
//! `show_http_message()` (remote-curl.c:516-545).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Path::new(STOCK).exists()
}

/// What the server does to which request.
#[derive(Clone, Copy)]
enum Fault {
    /// Every `GET` is answered with this status and a `text/plain` body.
    Get(u16),
    /// The `n`-th `POST` (1-based) is answered with this status.
    Post(u16, usize),
    /// The first `POST` is answered with `401` and a Basic challenge.
    PostAuth,
    /// The first `POST` sends its headers and half its body, then closes.
    PostTruncated,
    /// The first `POST` sends its headers and half its body, then resets the connection.
    PostReset,
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

/// Serve the requests of one connection through stock `git http-backend`,
/// applying `fault`. `posts` counts `POST`s across connections.
fn serve(stream: TcpStream, root: &Path, fault: Fault, posts: &AtomicUsize) {
    let mut reader = BufReader::new(&stream);
    while let Some((method, target, headers, body)) = read_request(&mut reader) {
        let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let mut out = &stream;
        let nth_post = (method == "POST").then(|| posts.fetch_add(1, Ordering::SeqCst) + 1);
        let refuse = match (fault, nth_post) {
            (Fault::Get(code), None) => Some((code, String::new())),
            (Fault::Post(code, n), Some(nth)) if nth == n => Some((code, String::new())),
            (Fault::PostAuth, Some(1)) => Some((401, "WWW-Authenticate: Basic realm=\"fixture\"\r\n".to_owned())),
            _ => None,
        };
        if let Some((code, extra)) = refuse {
            let body = format!("injected {code}\n");
            let head = format!(
                "HTTP/1.1 {code} X\r\n{extra}Content-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            out.write_all(head.as_bytes()).ok();
            out.write_all(body.as_bytes()).ok();
            continue;
        }
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let mut child = Command::new(STOCK)
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
        let head = format!("HTTP/1.1 {status}\r\n{response}Content-Length: {}\r\n\r\n", rest.len());
        out.write_all(head.as_bytes()).ok();
        if matches!(fault, Fault::PostTruncated) && nth_post == Some(1) {
            out.write_all(&rest[..rest.len() / 2]).ok();
            return;
        }
        if matches!(fault, Fault::PostReset) && nth_post == Some(1) {
            out.write_all(&rest[..rest.len() / 2]).ok();
            // SO_LINGER with a zero timeout makes the close send a RST instead of a FIN.
            let linger = libc::linger { l_onoff: 1, l_linger: 0 };
            // SAFETY: a valid socket and a `linger` that outlives the call.
            unsafe {
                libc::setsockopt(
                    std::os::fd::AsRawFd::as_raw_fd(&stream),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    (&raw const linger).cast(),
                    std::mem::size_of::<libc::linger>() as libc::socklen_t,
                );
            }
            return;
        }
        out.write_all(rest).ok();
    }
}

#[derive(Debug, PartialEq)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    refs: String,
    tracking: String,
}

/// A bare repository with `main` at one commit, a work repository one commit
/// ahead of it, and, with `behind`, the bare repository one commit ahead on a
/// side branch the work repository has not fetched. `bin` runs `args` in the
/// work repository against the faulty server, `@URL@` standing for its URL.
fn run(bin: &str, tag: &str, fault: Fault, behind: bool, args: &[&str]) -> Run {
    let root: PathBuf = std::env::temp_dir().join(format!("zvcs-httpfail-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("srv")).unwrap();
    let stock = |cwd: &Path, args: &[&str]| {
        let out = Command::new(STOCK)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap();
        assert!(out.status.success(), "fixture `git {args:?}`: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let bare = root.join("srv/r.git");
    let work = root.join("w");
    stock(&root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    stock(&bare, &["config", "http.receivepack", "true"]);
    stock(&root, &["init", "-q", "-b", "main", work.to_str().unwrap()]);
    std::fs::write(work.join("f"), "a\n").unwrap();
    stock(&work, &["add", "f"]);
    stock(&work, &["commit", "-qm", "a"]);
    stock(&work, &["push", "-q", bare.to_str().unwrap(), "main"]);
    std::fs::write(work.join("f"), "b\n").unwrap();
    stock(&work, &["commit", "-qam", "b"]);
    if behind {
        let other = root.join("o");
        stock(&root, &["clone", "-q", bare.to_str().unwrap(), other.to_str().unwrap()]);
        std::fs::write(other.join("g"), "c\n").unwrap();
        stock(&other, &["add", "g"]);
        stock(&other, &["commit", "-qm", "c"]);
        stock(&other, &["push", "-q", "origin", "HEAD:side"]);
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    {
        let srv = root.join("srv");
        let posts = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (srv, posts) = (srv.clone(), posts.clone());
                std::thread::spawn(move || serve(stream, &srv, fault, &posts));
            }
        });
    }
    let url = format!("http://127.0.0.1:{port}/r.git");
    stock(&work, &["remote", "add", "origin", &url]);
    // A helper that answers nothing, so a fill falls through to the disabled prompt.
    stock(&work, &["config", "credential.helper", "!true"]);

    let args: Vec<&str> = args.iter().map(|a| if *a == "@URL@" { url.as_str() } else { a }).collect();
    let out = Command::new(bin)
        .args(&args)
        .current_dir(&work)
        .env("HOME", &root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let host = format!("127.0.0.1:{port}");
    let scrub = |s: &[u8]| String::from_utf8_lossy(s).replace(&host, "HOST");
    let refs = stock(&bare, &["for-each-ref"]);
    let tracking = stock(&work, &["for-each-ref"]);
    let _ = std::fs::remove_dir_all(&root);
    Run {
        code: out.status.code(),
        stdout: scrub(&out.stdout),
        stderr: scrub(&out.stderr),
        refs,
        tracking,
    }
}

fn check(tag: &str, fault: Fault, behind: bool, args: &[&str], expected_stderr: &str) {
    if !stock_available() {
        return;
    }
    let want = run(STOCK, &format!("{tag}-s"), fault, behind, args);
    let got = run(BIN, &format!("{tag}-z"), fault, behind, args);
    assert_eq!(got, want, "{tag}: zvcs and stock git differ");
    assert_eq!(want.stderr, expected_stderr, "{tag}: the fixture produces the failure it is about");
}

#[test]
fn push_parity_post_refused_ends_send_pack_and_remote_curl_answers_nothing() {
    check(
        "post500",
        Fault::Post(500, 1),
        false,
        &["push", "origin", "main"],
        "error: RPC failed; HTTP 500 curl 22 The requested URL returned error: 500\n\
         send-pack: unexpected disconnect while reading sideband packet\n\
         fatal: the remote end hung up unexpectedly\n\
         Everything up-to-date\n",
    );
}

#[test]
fn push_parity_post_fill_that_dies_kills_remote_curl() {
    check(
        "post401",
        Fault::PostAuth,
        false,
        &["push", "origin", "main"],
        "fatal: could not read Username for 'http://HOST': terminal prompts disabled\n\
         send-pack: unexpected disconnect while reading sideband packet\n\
         fatal: the remote end hung up unexpectedly\n\
         error: failed to push some refs to 'http://HOST/r.git'\n",
    );
}

#[test]
fn push_parity_truncated_response_is_curl_partial_file() {
    if !stock_available() {
        return;
    }
    // The number of missing bytes depends on the response, so only the shape is fixed here.
    let want = run(STOCK, "trunc-s", Fault::PostTruncated, false, &["push", "origin", "main"]);
    let got = run(BIN, "trunc-z", Fault::PostTruncated, false, &["push", "origin", "main"]);
    assert_eq!(got, want);
    assert!(want.stderr.starts_with("error: RPC failed; curl 18 end of response with "));
}

#[test]
fn push_parity_reset_response_is_curl_recv_error() {
    // A reset is a failed recv(), CURLE_RECV_ERROR, whatever the body's length (cf-socket.c:1663-1680).
    check(
        "reset",
        Fault::PostReset,
        false,
        &["push", "origin", "main"],
        "error: RPC failed; curl 56 Recv failure: Connection reset by peer\n\
         send-pack: unexpected disconnect while reading sideband packet\n\
         fatal: the remote end hung up unexpectedly\n\
         Everything up-to-date\n",
    );
}

#[test]
fn push_parity_get_refused_is_discover_refs_die() {
    check(
        "get500",
        Fault::Get(500),
        false,
        &["push", "origin", "main"],
        "remote: injected 500\n\
         fatal: unable to access 'http://HOST/r.git/': The requested URL returned error: 500\n",
    );
    check(
        "get404",
        Fault::Get(404),
        false,
        &["push", "origin", "main"],
        "remote: injected 404\nfatal: repository 'http://HOST/r.git/' not found\n",
    );
}

#[test]
fn fetch_parity_get_refused_is_discover_refs_die() {
    check(
        "fget403",
        Fault::Get(403),
        false,
        &["fetch", "origin"],
        "remote: injected 403\n\
         fatal: unable to access 'http://HOST/r.git/': The requested URL returned error: 403\n",
    );
    check(
        "cget404",
        Fault::Get(404),
        false,
        &["clone", "@URL@", "c"],
        "Cloning into 'c'...\nremote: injected 404\nfatal: repository 'http://HOST/r.git/' not found\n",
    );
}

#[test]
fn ls_remote_parity_get_refused_is_discover_refs_die() {
    check(
        "lget500",
        Fault::Get(500),
        false,
        &["ls-remote", "origin"],
        "remote: injected 500\n\
         fatal: unable to access 'http://HOST/r.git/': The requested URL returned error: 500\n",
    );
}

#[test]
fn ls_remote_parity_ls_refs_fill_that_dies_is_expected_flush() {
    check(
        "lpost401",
        Fault::PostAuth,
        false,
        &["ls-remote", "origin"],
        "fatal: could not read Username for 'http://HOST': terminal prompts disabled\n\
         fatal: expected flush after ref listing\n",
    );
}

#[test]
fn fetch_parity_ls_refs_refused_is_expected_flush() {
    check(
        "fpost500",
        Fault::Post(500, 1),
        true,
        &["fetch", "origin"],
        "error: RPC failed; HTTP 500 curl 22 The requested URL returned error: 500\n\
         fatal: expected flush after ref listing\n",
    );
    check(
        "fpost401",
        Fault::PostAuth,
        true,
        &["fetch", "origin"],
        "fatal: could not read Username for 'http://HOST': terminal prompts disabled\n\
         fatal: expected flush after ref listing\n",
    );
}

#[test]
fn fetch_parity_v0_fetch_pack_refused_hangs_up() {
    check(
        "fv0post404",
        Fault::Post(404, 1),
        true,
        &["-c", "protocol.version=0", "fetch", "origin"],
        "error: RPC failed; HTTP 404 curl 22 The requested URL returned error: 404\n\
         fatal: the remote end hung up unexpectedly\n",
    );
}

#[test]
fn push_parity_porcelain_without_a_ref_to_report_prints_no_to_line() {
    if !stock_available() {
        return;
    }
    let args = &["push", "--porcelain", "origin", "main"];
    let want = run(STOCK, "porc-s", Fault::Post(500, 1), false, args);
    let got = run(BIN, "porc-z", Fault::Post(500, 1), false, args);
    assert_eq!(got, want);
    assert_eq!(want.stdout, "Done\n", "remote-curl answered, so push_ret is 0 and nothing is listed");
}
