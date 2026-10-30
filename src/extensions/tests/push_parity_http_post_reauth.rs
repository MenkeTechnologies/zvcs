//! `git push`, `git fetch` and `git ls-remote` against an `http://` remote whose
//! `POST` to `git-upload-pack` / `git-receive-pack` is the request that demands
//! Basic auth, compared byte for byte with stock git, helper input included.
//!
//! The server is a small HTTP/1.1 front that hands every request it accepts to
//! stock `git http-backend` as a CGI. The credential helper is a shell script
//! that logs each operation and every line it was given, `capability[]` and
//! `wwwauth[]` included, and can answer with extra lines.
//!
//! What has to match (remote-curl.c, http.c, credential.c):
//!
//! * a `401` to a `POST` whose body is in memory is filled in through
//!   `http_reauth_prepare(0)` and sent again (remote-curl.c:1070-1074): the
//!   helper's `get` carries the challenge as `wwwauth[]` and no `capability[]`,
//!   and the retry that goes through is approved with a `store`;
//! * a `POST` larger than `http.postBuffer` is preceded by `probe_rpc()`'s
//!   `0000` request, which draws the `401` out (remote-curl.c:943-953);
//! * a `401` to the handshake's `GET` refills with `all_capabilities=1`
//!   (http.c:2475-2476), so the `get` announces `authtype` and `state`;
//! * a helper's `capability[]=state` and `state[]` answer comes back to it in
//!   the `store` (credential.c:409-442, :547-560);
//! * every `WWW-Authenticate` header is one `wwwauth[]` line, in order, a folded
//!   continuation joined with a space (http.c:220-317);
//! * a retried `POST` refused again is `credential_reject()`, an `erase` with
//!   the challenge of that refusal, `run_slot()`'s `RPC failed` line, and for a
//!   v2 `ls-refs` `get_remote_refs()`'s `expected flush after ref listing`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn stock_available() -> bool {
    Path::new(STOCK).exists()
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Which requests the server refuses without the expected `Authorization`.
#[derive(Clone, Copy)]
enum Guard {
    /// Every request.
    All,
    /// Only `POST`s.
    Post,
    /// Every `POST`, whatever it carries.
    PostAlways,
}

/// One HTTP request off `stream`: method, target, lower-cased headers, body
/// (`Content-Length` or chunked).
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

/// Serve every request on one connection, which libcurl and hyper both reuse.
fn serve(stream: TcpStream, root: &Path, auth: &str, guard: Guard, challenge: &str, log: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(&stream);
    while let Some((method, target, headers, body)) = read_request(&mut reader) {
        let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let given = header("authorization").unwrap_or_default();
        let verdict = if given.is_empty() {
            "none"
        } else if given == auth {
            "good"
        } else {
            "bad"
        };
        log.lock().unwrap().push(format!("{method} {target} auth={verdict}"));
        let refuse = match guard {
            Guard::All => verdict != "good",
            Guard::Post => method == "POST" && verdict != "good",
            Guard::PostAlways => method == "POST",
        };
        let mut out = &stream;
        if refuse {
            let body = b"denied\n";
            let head = format!(
                "HTTP/1.1 401 Unauthorized\r\n{challenge}Content-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            out.write_all(head.as_bytes()).ok();
            out.write_all(body).ok();
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
        out.write_all(rest).ok();
    }
}

struct Case<'a> {
    guard: Guard,
    /// The `WWW-Authenticate` header lines of a refusal, each ending in CRLF.
    challenge: &'a str,
    user: &'a str,
    pass: &'a str,
    /// Lines the helper adds to its `get` answer.
    extra: &'a [&'a str],
    /// Size of a random blob committed before the command runs, 0 for none.
    big: usize,
    args: &'a [&'a str],
}

const BASIC: &str = "WWW-Authenticate: Basic realm=\"fixture\"\r\n";

impl Default for Case<'_> {
    fn default() -> Self {
        Case {
            guard: Guard::Post,
            challenge: BASIC,
            user: "user",
            pass: "pass",
            extra: &[],
            big: 0,
            args: &[],
        }
    }
}

#[derive(Debug, PartialEq)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    server: Vec<String>,
    helper: String,
    refs: String,
}

/// A bare repository with `main` at one commit, a clone one commit ahead, the
/// server in front of the bare repository, and `bin` run in the clone.
fn run(bin: &str, tag: &str, case: &Case<'_>) -> Run {
    let root: PathBuf = std::env::temp_dir().join(format!("zvcs-postreauth-{tag}-{}", std::process::id()));
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
    if case.big > 0 {
        // Incompressible, so the pack stays bigger than `http.postBuffer`.
        let mut x: u32 = 7;
        let blob: Vec<u8> = (0..case.big)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (x >> 16) as u8
            })
            .collect();
        std::fs::write(work.join("big"), blob).unwrap();
        stock(&work, &["add", "big"]);
        stock(&work, &["commit", "-qm", "big"]);
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let auth = format!("Basic {}", base64(b"user:pass"));
    {
        let (log, srv, auth, guard, challenge) = (log.clone(), root.join("srv"), auth, case.guard, case.challenge.to_owned());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (log, srv, auth, challenge) = (log.clone(), srv.clone(), auth.clone(), challenge.clone());
                std::thread::spawn(move || serve(stream, &srv, &auth, guard, &challenge, &log));
            }
        });
    }

    let helper_log = root.join("helper.log");
    let script = root.join("helper.sh");
    let extra: String = case.extra.iter().map(|l| format!("{l}\\n")).collect();
    std::fs::write(
        &script,
        format!(
            "echo \"-- $1\" >>'{log}'\nwhile read -r l; do [ -z \"$l\" ] && break; echo \"$l\" >>'{log}'; done\n\
             [ \"$1\" = get ] && [ -n '{user}' ] && printf 'username=%s\\npassword=%s\\n{extra}' '{user}' '{pass}'\nexit 0\n",
            log = helper_log.display(),
            user = case.user,
            pass = case.pass,
        ),
    )
    .unwrap();
    let url = format!("http://127.0.0.1:{port}/r.git");
    stock(&work, &["remote", "add", "origin", &url]);
    stock(&work, &["config", "credential.helper", &format!("!sh '{}'", script.display())]);

    let out = Command::new(bin)
        .args(case.args)
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
    let helper = std::fs::read_to_string(&helper_log).unwrap_or_default().replace(&host, "HOST");
    let refs = stock(&bare, &["for-each-ref"]);
    let server = log.lock().unwrap().clone();
    let _ = std::fs::remove_dir_all(&root);
    Run {
        code: out.status.code(),
        stdout: scrub(&out.stdout),
        stderr: scrub(&out.stderr),
        server,
        helper,
        refs,
    }
}

fn compare(tag: &str, case: &Case<'_>) -> Run {
    let want = run(STOCK, &format!("{tag}-stock"), case);
    let got = run(BIN, &format!("{tag}-zvcs"), case);
    assert_eq!(got.stderr, want.stderr, "{tag}: stderr");
    assert_eq!(got.stdout, want.stdout, "{tag}: stdout");
    assert_eq!(got.code, want.code, "{tag}: exit code");
    assert_eq!(got.server, want.server, "{tag}: requests the server saw");
    assert_eq!(got.helper, want.helper, "{tag}: helper operations and every field");
    assert_eq!(got.refs, want.refs, "{tag}: remote refs afterwards");
    want
}

/// The `POST` refused for want of a credential is filled in and sent again, for
/// each of the three commands, and the credential is stored once it went through.
#[test]
fn refused_post_is_filled_in_and_retried() {
    if !stock_available() {
        return;
    }
    for (tag, args) in [
        ("push-post", &["push", "origin", "main"][..]),
        ("fetch-post", &["fetch", "origin"][..]),
        ("lsremote-post", &["ls-remote", "origin"][..]),
    ] {
        let want = compare(tag, &Case { args, ..Case::default() });
        assert_eq!(want.code, Some(0), "{tag}");
        assert_eq!(
            want.server.iter().filter(|r| r.starts_with("POST")).count(),
            2,
            "{tag}: stock retries the POST once: {:?}",
            want.server
        );
        assert!(want.helper.contains("wwwauth[]=Basic realm=\"fixture\"\n"), "{tag}: {}", want.helper);
        assert!(!want.helper.contains("capability[]"), "{tag}: a POST refill announces nothing: {}", want.helper);
        assert!(want.helper.contains("-- store\n"), "{tag}: {}", want.helper);
    }
}

/// A push bigger than `http.postBuffer` cannot be sent twice, so the `401` is
/// drawn out by a `0000` probe first.
#[test]
fn large_push_probes_before_the_chunked_post() {
    if !stock_available() {
        return;
    }
    let want = compare(
        "push-large",
        &Case {
            big: 300_000,
            args: &["-c", "http.postBuffer=65520", "push", "origin", "main"],
            ..Case::default()
        },
    );
    assert_eq!(want.code, Some(0));
    assert_eq!(want.server.iter().filter(|r| r.starts_with("POST")).count(), 3, "{:?}", want.server);
}

/// The handshake's refill announces both capabilities, and a helper that takes
/// up `state` gets its `state[]` back in the `store`; several challenges are
/// several `wwwauth[]` lines and a folded one is joined.
#[test]
fn get_refill_announces_capabilities_and_state_round_trips() {
    if !stock_available() {
        return;
    }
    let want = compare(
        "push-all-state",
        &Case {
            guard: Guard::All,
            challenge: "WWW-Authenticate: Basic realm=\"fixture\"\r\nWWW-Authenticate: Basic\r\n  realm=\"x\",\r\n\tcharset=\"UTF-8\"\r\n",
            extra: &["capability[]=state", "state[]=helper:seen=1"],
            args: &["push", "origin", "main"],
            ..Case::default()
        },
    );
    assert!(want.helper.contains("capability[]=authtype\ncapability[]=state\n"), "{}", want.helper);
    assert!(want.helper.contains("wwwauth[]=Basic realm=\"x\", charset=\"UTF-8\"\n"), "{}", want.helper);
    assert!(want.helper.contains("state[]=helper:seen=1\n"), "{}", want.helper);
    compare(
        "lsremote-all-bad-state",
        &Case {
            guard: Guard::All,
            pass: "wrong",
            extra: &["capability[]=state", "state[]=helper:seen=1"],
            args: &["ls-remote", "origin"],
            ..Case::default()
        },
    );
}

/// A helper's `authtype` and `credential` sign the retry instead of the
/// username and password.
#[test]
fn authtype_credential_answer_signs_the_request() {
    if !stock_available() {
        return;
    }
    for (tag, guard, args) in [
        ("lsremote-bearer", Guard::All, &["ls-remote", "origin"][..]),
        ("fetch-basic-credential", Guard::Post, &["fetch", "origin"][..]),
    ] {
        let extra: &[&str] = match guard {
            Guard::All => &["capability[]=authtype", "authtype=Bearer", "credential=tok"],
            _ => &["capability[]=authtype", "authtype=Basic", "credential=dXNlcjpwYXNz"],
        };
        compare(tag, &Case { guard, pass: "wrong", extra, args, ..Case::default() });
    }
}

/// The retried `POST` refused again: the credential is erased with the challenge
/// of that refusal, and the v2 `ls-refs` reader finds no flush.
#[test]
fn refused_retry_erases_and_dies_expecting_a_flush() {
    if !stock_available() {
        return;
    }
    for (tag, args) in [
        ("fetch-postdeny", &["fetch", "origin"][..]),
        ("lsremote-postdeny", &["ls-remote", "origin"][..]),
    ] {
        let want = compare(tag, &Case { guard: Guard::PostAlways, args, ..Case::default() });
        assert_eq!(want.code, Some(128), "{tag}");
        assert!(want.stderr.contains("error: RPC failed; HTTP 401"), "{tag}: {}", want.stderr);
        assert!(want.helper.contains("-- erase\n"), "{tag}: {}", want.helper);
    }
}
