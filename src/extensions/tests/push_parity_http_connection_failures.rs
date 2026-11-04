//! `git push`, `git fetch`, `git ls-remote` and `git clone` against an `http://`
//! remote whose connection fails below HTTP — refused, closed before any
//! response, or reset — compared byte for byte with stock git.
//!
//! curl ends such a transfer with a `CURLcode` and the text `failf()` left in
//! the error buffer git calls `curl_errorstr`. A failed ref advertisement is
//! `discover_refs()`'s `die(_("unable to access '%s': %s"))`
//! (remote-curl.c:541-545); a failed `POST` is `run_slot()`'s
//! `error(_("RPC failed; %s"))` with `curl <code> <text>` (remote-curl.c:859-873),
//! after which each reader ends at its end of file as it does for an HTTP
//! status (see `push_parity_http_failure_stderr.rs`).
//!
//! A request that went out on a connection an earlier request had used and got
//! no byte back is sent again on a fresh connection by `Curl_retry_request()`
//! (transfer.c:595-657 in curl 8.22.0), so a server that drops one `POST` on a
//! kept-alive connection costs nothing, and a streamed `POST` that cannot be
//! rewound fails with remote-curl's `rpc_seek()` error (remote-curl.c:753-770).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// What the server does to which request. A `POST` fault names the 1-based
/// `POST` it applies to, `0` for every one.
#[derive(Clone, Copy)]
enum Fault {
    /// Nothing goes wrong.
    None,
    /// Every `GET` is read and the connection closed without a response.
    GetClose,
    /// Every `GET` is read and the connection reset without a response.
    GetReset,
    /// The `POST` is read and the connection closed without a response.
    PostClose(usize),
    /// The `POST` is read and the connection reset without a response.
    PostReset(usize),
    /// Nothing listens on the port.
    Refused,
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

/// Serve the requests of one connection through stock `git http-backend`,
/// applying `fault`. `posts` counts `POST`s across connections.
fn serve(stock: &str, stream: TcpStream, root: &Path, fault: Fault, posts: &AtomicUsize) {
    let mut reader = BufReader::new(&stream);
    while let Some((method, target, headers, body)) = read_request(&mut reader) {
        let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let nth_post = (method == "POST").then(|| posts.fetch_add(1, Ordering::SeqCst) + 1);
        let hit = |n: usize| nth_post.is_some_and(|nth| n == 0 || nth == n);
        match fault {
            Fault::GetClose if nth_post.is_none() => return,
            Fault::GetReset if nth_post.is_none() => return reset(&stream),
            Fault::PostClose(n) if hit(n) => return,
            Fault::PostReset(n) if hit(n) => return reset(&stream),
            _ => {}
        }
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
        let head = format!("HTTP/1.1 {status}\r\n{response}Content-Length: {}\r\n\r\n", rest.len());
        let mut out = &stream;
        out.write_all(head.as_bytes()).ok();
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
    clone: String,
}

/// A bare repository with `main` at one commit, a work repository one commit
/// ahead of it, and, with `behind`, the bare repository one commit ahead on a
/// side branch the work repository has not fetched. `bin` runs `args` in the
/// work repository against the faulty server, `@URL@` standing for its URL.
fn run(stock: &str, bin: &str, tag: &str, fault: Fault, behind: bool, args: &[&str]) -> Run {
    let root: PathBuf = std::env::temp_dir().join(format!("zvcs-httpconn-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("srv")).unwrap();
    let git = |cwd: &Path, args: &[&str]| {
        let out = Command::new(stock)
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
    git(&root, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
    git(&bare, &["config", "http.receivepack", "true"]);
    git(&root, &["init", "-q", "-b", "main", work.to_str().unwrap()]);
    std::fs::write(work.join("f"), "a\n").unwrap();
    git(&work, &["add", "f"]);
    git(&work, &["commit", "-qm", "a"]);
    git(&work, &["push", "-q", bare.to_str().unwrap(), "main"]);
    std::fs::write(work.join("f"), "b\n").unwrap();
    git(&work, &["commit", "-qam", "b"]);
    if behind {
        let other = root.join("o");
        git(&root, &["clone", "-q", bare.to_str().unwrap(), other.to_str().unwrap()]);
        std::fs::write(other.join("g"), "c\n").unwrap();
        git(&other, &["add", "g"]);
        git(&other, &["commit", "-qm", "c"]);
        git(&other, &["push", "-q", "origin", "HEAD:side"]);
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    if matches!(fault, Fault::Refused) {
        drop(listener);
    } else {
        let srv = root.join("srv");
        let posts = Arc::new(AtomicUsize::new(0));
        let stock = stock.to_owned();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (srv, posts, stock) = (srv.clone(), posts.clone(), stock.clone());
                std::thread::spawn(move || serve(&stock, stream, &srv, fault, &posts));
            }
        });
    }
    let url = format!("http://127.0.0.1:{port}/r.git");
    git(&work, &["remote", "add", "origin", &url]);

    let args: Vec<&str> = args.iter().map(|a| if *a == "@URL@" { url.as_str() } else { a }).collect();
    let out = Command::new(bin)
        .args(&args)
        .current_dir(&work)
        .env("HOME", &root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let host = format!("127.0.0.1:{port}");
    // The time curl took to be refused is its own measurement, not part of what is compared.
    let scrub = |s: &[u8]| {
        let mut text = String::from_utf8_lossy(s).replace(&host, "HOST");
        if let Some(at) = text.find(" after ") {
            let digits = text[at + 7..].bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 && text[at + 7 + digits..].starts_with(" ms") {
                text.replace_range(at + 7..at + 7 + digits, "N");
            }
        }
        text
    };
    let refs = git(&bare, &["for-each-ref"]);
    let tracking = git(&work, &["for-each-ref"]);
    let clone = if work.join("c").exists() {
        git(&work.join("c"), &["for-each-ref"])
    } else {
        String::new()
    };
    let _ = std::fs::remove_dir_all(&root);
    Run {
        code: out.status.code(),
        stdout: scrub(&out.stdout),
        stderr: scrub(&out.stderr),
        refs,
        tracking,
        clone,
    }
}

/// Run `args` through both binaries and compare everything; `expected_stderr`
/// pins what stock says, so a fixture that stopped failing is noticed.
fn check(tag: &str, fault: Fault, behind: bool, args: &[&str], expected_stderr: &str) {
    let Some(stock) = stock_git() else { return };
    let want = run(stock, stock, &format!("{tag}-s"), fault, behind, args);
    let got = run(stock, BIN, &format!("{tag}-z"), fault, behind, args);
    assert_eq!(got, want, "{tag}: zvcs and stock git differ");
    assert_eq!(want.stderr, expected_stderr, "{tag}: the fixture produces the failure it is about");
}

#[test]
fn fetch_parity_advertisement_without_a_response_is_empty_reply() {
    let msg = "fatal: unable to access 'http://HOST/r.git/': Empty reply from server\n";
    check("gclose-fetch", Fault::GetClose, false, &["fetch", "origin"], msg);
    check("gclose-clone", Fault::GetClose, false, &["clone", "@URL@", "c"], &format!("Cloning into 'c'...\n{msg}"));
}

#[test]
fn ls_remote_parity_advertisement_reset_is_recv_failure() {
    check(
        "greset-ls",
        Fault::GetReset,
        false,
        &["ls-remote", "origin"],
        "fatal: unable to access 'http://HOST/r.git/': Recv failure: Connection reset by peer\n",
    );
}

#[test]
fn push_parity_advertisement_failures_are_discover_refs_die() {
    check(
        "gclose-push",
        Fault::GetClose,
        false,
        &["push", "origin", "main"],
        "fatal: unable to access 'http://HOST/r.git/': Empty reply from server\n",
    );
    check(
        "greset-push",
        Fault::GetReset,
        false,
        &["push", "origin", "main"],
        "fatal: unable to access 'http://HOST/r.git/': Recv failure: Connection reset by peer\n",
    );
}

#[test]
fn fetch_parity_refused_connection_is_couldnt_connect() {
    let Some(stock) = stock_git() else { return };
    for (tag, args) in [
        ("refused-fetch", &["fetch", "origin"][..]),
        ("refused-ls", &["ls-remote", "origin"][..]),
        ("refused-push", &["push", "origin", "main"][..]),
        ("refused-clone", &["clone", "@URL@", "c"][..]),
    ] {
        let want = run(stock, stock, &format!("{tag}-s"), Fault::Refused, false, args);
        let got = run(stock, BIN, &format!("{tag}-z"), Fault::Refused, false, args);
        // The wording is cf-ip-happy.c:717-729 and strerror.c:60-61 in curl 8.22.0. libcurl before 8.12 said
        // "Failed to connect to <host> port <port> after <ms> ms: Couldn't connect to server" (connect.c:729 and
        // strerror.c:82-83 in 8.7.1), so against a stock git built on an older libcurl only the rest is compared.
        const CURRENT: &str = "Failed to connect to HOST after N ms: Could not connect to server\n";
        if want.stderr.ends_with(CURRENT) {
            assert_eq!(got, want, "{tag}: zvcs and stock git differ");
        } else {
            let line = |r: &Run| r.stderr.rsplit_once("': ").map(|(head, _)| head.to_owned());
            assert_eq!(line(&got), line(&want), "{tag}: zvcs and stock git differ");
            assert_eq!((got.code, &got.stdout), (want.code, &want.stdout), "{tag}");
            assert!(got.stderr.ends_with(CURRENT), "{tag}: {}", got.stderr);
        }
        assert!(
            want.stderr.contains("fatal: unable to access 'http://HOST/r.git/': Failed to connect to "),
            "{tag}: {}",
            want.stderr
        );
    }
}

#[test]
fn fetch_parity_post_without_a_response_is_empty_reply() {
    check(
        "pclose-fetch",
        Fault::PostClose(0),
        true,
        &["fetch", "origin"],
        "error: RPC failed; curl 52 Empty reply from server\nfatal: expected flush after ref listing\n",
    );
    check(
        "pclose-fetch-v0",
        Fault::PostClose(0),
        true,
        &["-c", "protocol.version=0", "fetch", "origin"],
        "error: RPC failed; curl 52 Empty reply from server\nfatal: the remote end hung up unexpectedly\n",
    );
    check(
        "pclose-clone",
        Fault::PostClose(0),
        false,
        &["clone", "@URL@", "c"],
        "Cloning into 'c'...\n\
         error: RPC failed; curl 52 Empty reply from server\n\
         fatal: expected flush after ref listing\n",
    );
}

#[test]
fn ls_remote_parity_post_without_a_response_is_empty_reply() {
    check(
        "pclose-ls",
        Fault::PostClose(0),
        false,
        &["ls-remote", "origin"],
        "error: RPC failed; curl 52 Empty reply from server\nfatal: expected flush after ref listing\n",
    );
}

#[test]
fn push_parity_post_without_a_response_ends_send_pack() {
    let tail = "send-pack: unexpected disconnect while reading sideband packet\n\
                fatal: the remote end hung up unexpectedly\n\
                Everything up-to-date\n";
    check(
        "pclose-push",
        Fault::PostClose(0),
        false,
        &["push", "origin", "main"],
        &format!("error: RPC failed; curl 52 Empty reply from server\n{tail}"),
    );
    check(
        "preset-push",
        Fault::PostReset(0),
        false,
        &["push", "origin", "main"],
        &format!("error: RPC failed; curl 56 Recv failure: Connection reset by peer\n{tail}"),
    );
    // The `0000` probe of a request larger than `http.postBuffer` fails the same way.
    check(
        "pclose-push-probe",
        Fault::PostClose(0),
        false,
        &["-c", "http.postBuffer=100", "push", "origin", "main"],
        &format!("error: RPC failed; curl 52 Empty reply from server\n{tail}"),
    );
}

#[test]
fn push_parity_dead_kept_alive_connection_is_retried() {
    // The first `POST` goes out on the connection the ref advertisement used.
    check("pclose1-push", Fault::PostClose(1), false, &["push", "origin", "main"], "To http://HOST/r.git\n   9cb5715..167e180  main -> main\n");
}

#[test]
fn fetch_parity_dead_kept_alive_connection_is_retried() {
    let msg = "From http://HOST/r\n \
               * [new branch]      main       -> origin/main\n \
               * [new branch]      side       -> origin/side\n";
    check("pclose1-fetch", Fault::PostClose(1), true, &["fetch", "origin"], msg);
    check("preset1-fetch", Fault::PostReset(1), true, &["fetch", "origin"], msg);
    check("pclose1-fetch-v0", Fault::PostClose(1), true, &["-c", "protocol.version=0", "fetch", "origin"], msg);
}

#[test]
fn ls_remote_parity_dead_kept_alive_connection_is_retried() {
    let Some(stock) = stock_git() else { return };
    let args = &["ls-remote", "origin"];
    let want = run(stock, stock, "pclose1-ls-s", Fault::PostClose(1), false, args);
    let got = run(stock, BIN, "pclose1-ls-z", Fault::PostClose(1), false, args);
    assert_eq!(got, want);
    assert_eq!((want.code, want.stderr.as_str()), (Some(0), ""));
}

#[test]
fn push_parity_streamed_post_on_a_dead_connection_cannot_be_rewound() {
    // The probe (`POST` 1) answers; the chunked request after it finds the kept-alive connection dead, and
    // `rpc_seek()` cannot go back past the buffer curl already took.
    check(
        "prewind-push",
        Fault::PostClose(2),
        false,
        &["-c", "http.postBuffer=100", "push", "origin", "main"],
        "error: unable to rewind rpc post data - try increasing http.postBuffer\n\
         error: RPC failed; curl 65 seek callback returned error 1\n\
         send-pack: unexpected disconnect while reading sideband packet\n\
         fatal: the remote end hung up unexpectedly\n\
         Everything up-to-date\n",
    );
}

#[test]
fn push_parity_verbose_announces_each_post() {
    let push = "Pushing to http://HOST/r.git\n";
    let done = "To http://HOST/r.git\n   9cb5715..167e180  main -> main\nupdating local tracking ref 'refs/remotes/origin/main'\n";
    let Some(stock) = stock_git() else { return };
    for (tag, args) in [
        ("v-push", &["push", "-v", "origin", "main"][..]),
        ("vv-push", &["push", "-vv", "origin", "main"][..]),
        ("v-push-chunked", &["-c", "http.postBuffer=100", "push", "-v", "origin", "main"][..]),
        ("qv-push", &["push", "-q", "-v", "origin", "main"][..]),
        ("vq-push", &["push", "-v", "-q", "origin", "main"][..]),
    ] {
        let want = run(stock, stock, &format!("{tag}-s"), Fault::None, false, args);
        let got = run(stock, BIN, &format!("{tag}-z"), Fault::None, false, args);
        assert_eq!(got, want, "{tag}: zvcs and stock git differ");
        if tag != "vq-push" {
            assert!(want.stderr.starts_with(push) && want.stderr.ends_with(done), "{tag}: {}", want.stderr);
            assert!(want.stderr.contains("\nPOST git-receive-pack ("), "{tag}: {}", want.stderr);
        }
    }
}
