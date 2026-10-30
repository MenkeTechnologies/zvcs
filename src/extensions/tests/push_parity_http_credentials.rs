//! `git push`, `git fetch` and `git ls-remote` against an `http://` remote that
//! demands Basic auth, compared byte for byte with stock git.
//!
//! The server is a small HTTP/1.1 front written here: it answers `401` with a
//! `WWW-Authenticate: Basic` challenge and a `text/plain` body to any request
//! whose `Authorization` is not the expected one, and hands every other request
//! to stock `git http-backend` as a CGI. The credential helper is a shell script
//! that logs each operation and the fields it was given.
//!
//! What has to match (remote-curl.c, http.c, credential.c, prompt.c):
//!
//! * credentials go out over plain `http://` (`init_curl_http_auth()`,
//!   http.c:638-667, applies to every scheme);
//! * a `401` to the retry that carried credentials is `credential_reject()` —
//!   an `erase` to every helper — then `show_http_message()`'s `remote: ` lines
//!   and `fatal: Authentication failed for '<url>/'`, exit 128
//!   (http.c:1945-1955, remote-curl.c:370-395, :524-527);
//! * a helper that answers nothing under `GIT_TERMINAL_PROMPT=0` is
//!   `credential_fill()`'s `git_prompt()` dying with `could not read Username for
//!   '<url>': terminal prompts disabled` (prompt.c:46-77);
//! * a successful authenticated exchange is `credential_approve()`: a `store`.
//!
//! The helper's `capability[]` and `wwwauth[]` lines are left out of the
//! comparison: zvcs's transport does not send them yet (credential.c:409-439
//! writes them; the vendored credential context has no field for either), and
//! the request and response side of the exchange is what these cases pin.

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

/// One HTTP request off `stream`: method, target, lower-cased headers, body
/// (`Content-Length` or chunked).
fn read_request(stream: &TcpStream) -> Option<(String, String, Vec<(String, String)>, Vec<u8>)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
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

fn serve(stream: TcpStream, root: &Path, auth: &str, log: &Mutex<Vec<String>>) {
    let Some((method, target, headers, body)) = read_request(&stream) else {
        return;
    };
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
    let mut out = &stream;
    if verdict != "good" {
        let body = b"denied\nsecond line\n";
        let head = format!(
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"fixture\"\r\n\
             Content-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        out.write_all(head.as_bytes()).ok();
        out.write_all(body).ok();
        return;
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
    let head = format!(
        "HTTP/1.1 {status}\r\n{response}Content-Length: {}\r\nConnection: close\r\n\r\n",
        rest.len()
    );
    out.write_all(head.as_bytes()).ok();
    out.write_all(rest).ok();
}

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
fn run(bin: &str, tag: &str, helper_user: &str, helper_pass: &str, env: &[(&str, &str)], args: &[&str]) -> Run {
    let root: PathBuf =
        std::env::temp_dir().join(format!("zvcs-httpcred-{tag}-{}", std::process::id()));
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

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let auth = format!("Basic {}", base64(b"user:pass"));
    {
        let (log, srv, auth) = (log.clone(), root.join("srv"), auth.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve(stream, &srv, &auth, &log);
            }
        });
    }

    let helper_log = root.join("helper.log");
    let script = root.join("helper.sh");
    std::fs::write(
        &script,
        format!(
            "echo \"-- $1\" >>'{log}'\nwhile read -r l; do [ -z \"$l\" ] && break; echo \"$l\" >>'{log}'; done\n\
             [ \"$1\" = get ] && [ -n '{helper_user}' ] && printf 'username=%s\\npassword=%s\\n' '{helper_user}' '{helper_pass}'\nexit 0\n",
            log = helper_log.display()
        ),
    )
    .unwrap();
    let url = format!("http://127.0.0.1:{port}/r.git");
    stock(&work, &["remote", "add", "origin", &url]);
    stock(&work, &["config", "credential.helper", &format!("!sh '{}'", script.display())]);

    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(&work)
        .env("HOME", &root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let host = format!("127.0.0.1:{port}");
    let scrub = |s: &[u8]| String::from_utf8_lossy(s).replace(&host, "HOST");
    let helper = std::fs::read_to_string(&helper_log)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with("capability[]=") && !l.starts_with("wwwauth[]="))
        .map(|l| format!("{}\n", l.replace(&host, "HOST")))
        .collect();
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

fn compare(tag: &str, user: &str, pass: &str, env: &[(&str, &str)], args: &[&str]) -> Run {
    let want = run(STOCK, &format!("{tag}-stock"), user, pass, env, args);
    let got = run(BIN, &format!("{tag}-zvcs"), user, pass, env, args);
    assert_eq!(got.stderr, want.stderr, "{tag}: stderr");
    assert_eq!(got.stdout, want.stdout, "{tag}: stdout");
    assert_eq!(got.code, want.code, "{tag}: exit code");
    assert_eq!(got.server, want.server, "{tag}: requests the server saw");
    assert_eq!(got.helper, want.helper, "{tag}: helper operations and fields");
    assert_eq!(got.refs, want.refs, "{tag}: remote refs afterwards");
    want
}

/// The credential a helper hands out is sent over `http://`, the push lands,
/// and the helper is told to `store` it.
#[test]
fn push_authenticates_over_plain_http_and_approves() {
    if !stock_available() {
        return;
    }
    let want = compare("push-good", "user", "pass", &[], &["push", "origin", "main"]);
    assert_eq!(want.code, Some(0));
    assert!(want.helper.contains("-- store\n"), "stock approves: {}", want.helper);
}

/// A refused retry erases the credential and dies with the server's message.
#[test]
fn push_fetch_ls_remote_refused_credentials_die_authentication_failed() {
    if !stock_available() {
        return;
    }
    for (tag, args) in [
        ("push-bad", &["push", "origin", "main"][..]),
        ("fetch-bad", &["fetch", "origin"][..]),
        ("lsremote-bad", &["ls-remote", "origin"][..]),
    ] {
        let want = compare(tag, "user", "wrong", &[], args);
        assert_eq!(want.code, Some(128), "{tag}");
        assert!(want.helper.contains("-- erase\n"), "{tag}: stock rejects: {}", want.helper);
    }
}

/// No helper answer, terminal prompts disabled: `git_prompt()`'s die.
#[test]
fn missing_credentials_with_prompts_disabled_die_like_git_prompt() {
    if !stock_available() {
        return;
    }
    for (tag, args) in [
        ("push-none", &["push", "origin", "main"][..]),
        ("fetch-none", &["fetch", "origin"][..]),
        ("lsremote-none", &["ls-remote", "origin"][..]),
    ] {
        compare(tag, "", "", &[], args);
    }
    // A failing askpass is reported and the terminal is tried next.
    compare("push-askpass-false", "", "", &[("GIT_ASKPASS", "/usr/bin/false")], &["push", "origin", "main"]);
    compare(
        "push-askpass-missing",
        "",
        "",
        &[("GIT_ASKPASS", "/nonexistent/askpass")],
        &["push", "origin", "main"],
    );
}

/// An askpass program that answers supplies the credential.
#[test]
fn askpass_answer_is_used_when_helpers_have_none() {
    if !stock_available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("zvcs-httpcred-askpass-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let askpass = dir.join("askpass.sh");
    std::fs::write(&askpass, "#!/bin/sh\ncase \"$1\" in Username*) echo user;; *) echo pass;; esac\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let want = compare(
        "lsremote-askpass",
        "",
        "",
        &[("GIT_ASKPASS", askpass.to_str().unwrap())],
        &["ls-remote", "origin"],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(want.code, Some(0));
}
