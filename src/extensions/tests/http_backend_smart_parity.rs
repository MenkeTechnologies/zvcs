//! `git http-backend`'s **smart** routes, driven as a CGI.
//!
//! The dumb routes serve files; the smart ones are the whole of `git clone` and
//! `git push` over HTTP, and they are the two rows of `services[]`
//! (http-backend.c:733, :744-746) that do not answer out of the object store at
//! all. `get_info_refs` with a `service=` parameter runs
//! `<svc> --http-backend-info-refs .` behind an
//! `application/x-git-<svc>-advertisement` header (http-backend.c:547-565), and
//! `service_rpc` runs `<svc> --stateless-rpc .` behind an
//! `…-result` header with the POST body piped in (http-backend.c:654-680). A
//! backend that answers those two with anything but the service's own byte
//! stream cannot be cloned from, so the assertions here are on bytes, not on
//! exit codes: the header block, the `# service=` banner and flush that only a
//! v0/v1 client gets, the v2 capability advertisement that replaces it, and the
//! `ls-refs` answer to a POST.
//!
//! No sockets: `http-backend` is a CGI, so the "server" is the process
//! environment and the request body is stdin. Nothing here listens, binds, or
//! resolves a name, so it runs on a network-less CI box.
//!
//! Every expectation was captured from stock git 2.55.0 run against the same
//! fixture, with the one unavoidable substitution called out where it is made:
//! the `agent=` capability names the implementation and cannot match by
//! construction.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A bare, exported repository with one commit on `refs/heads/main`, built
    /// with the binary under test so the fixture never depends on stock git.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-httpsmart-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fixture = Fixture { root };

        let work = fixture.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fixture.git(&work, &["init", "-q", "-b", "main", "."]);
        std::fs::write(work.join("a.txt"), b"hi\n").unwrap();
        fixture.git(&work, &["add", "a.txt"]);
        fixture.git(&work, &["commit", "-q", "-m", "one"]);

        let bare = fixture.bare();
        fixture.git(&fixture.root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
        fixture.git(&work, &["push", "-q", bare.to_str().unwrap(), "main"]);
        std::fs::write(bare.join("git-daemon-export-ok"), b"").unwrap();
        fixture
    }

    fn bare(&self) -> PathBuf {
        self.root.join("repo.git")
    }

    fn git(&self, cwd: &Path, args: &[&str]) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", "100000000 +0000")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "100000000 +0000")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "fixture `git {args:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn head_oid(&self) -> String {
        let out = Command::new(BIN)
            .args(["rev-parse", "refs/heads/main"])
            .current_dir(self.bare())
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// One CGI request. `env` is the request environment on top of the fixed
    /// `GIT_PROJECT_ROOT`/`GIT_HTTP_EXPORT_ALL` pair; `body` is stdin.
    fn cgi(&self, env: &[(&str, &str)], body: &[u8]) -> Cgi {
        let mut cmd = Command::new(BIN);
        cmd.arg("http-backend")
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_PROJECT_ROOT", &self.root)
            .env("GIT_HTTP_EXPORT_ALL", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(body).unwrap();
        let out = child.wait_with_output().unwrap();
        let raw = out.stdout;
        let split = find(&raw, b"\r\n\r\n").expect("CGI response has a header block");
        Cgi {
            headers: String::from_utf8_lossy(&raw[..split + 2]).into_owned(),
            body: raw[split + 4..].to_vec(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

struct Cgi {
    /// The header block, terminated by the final header's own CRLF.
    headers: String,
    body: Vec<u8>,
    stderr: String,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Split a pkt-line stream into its payloads, with `None` for a flush packet.
/// A delim (`0001`) is reported as an empty payload so its position is visible.
fn pkt_lines(mut buf: &[u8]) -> Vec<Option<Vec<u8>>> {
    let mut out = Vec::new();
    while buf.len() >= 4 {
        let len = usize::from_str_radix(std::str::from_utf8(&buf[..4]).unwrap(), 16).unwrap();
        if len == 0 {
            out.push(None);
            buf = &buf[4..];
            continue;
        }
        if len == 1 {
            out.push(Some(Vec::new()));
            buf = &buf[4..];
            continue;
        }
        out.push(Some(buf[4..len].to_vec()));
        buf = &buf[len..];
    }
    assert!(buf.is_empty(), "trailing {} bytes are not a pkt-line", buf.len());
    out
}

/// The no-cache block `hdr_nocache` writes, which every smart answer opens with
/// (http-backend.c:115-120).
const NOCACHE: &str = "Expires: Fri, 01 Jan 1980 00:00:00 GMT\r\n\
                       Pragma: no-cache\r\n\
                       Cache-Control: no-cache, max-age=0, must-revalidate\r\n";

/// `GET /info/refs?service=git-upload-pack` with no `Git-Protocol` header.
///
/// Stock git 2.55.0 answers the no-cache block, the advertisement content type,
/// then `001e# service=git-upload-pack\n0000` and the v0 ref advertisement whose
/// first ref carries the capability list behind a NUL (http-backend.c:559-562,
/// upload-pack.c's `write_v0_ref`). A backend that cannot run the service at all
/// produces no body, which is what this pins.
#[test]
fn v0_info_refs_banner_and_advertisement() {
    let fx = Fixture::new("v0refs");
    let oid = fx.head_oid();
    let res = fx.cgi(
        &[
            ("REQUEST_METHOD", "GET"),
            ("PATH_INFO", "/repo.git/info/refs"),
            ("QUERY_STRING", "service=git-upload-pack"),
        ],
        b"",
    );

    assert_eq!(
        res.headers,
        format!("{NOCACHE}Content-Type: application/x-git-upload-pack-advertisement\r\n"),
        "header block diverged; stderr was {:?}",
        res.stderr
    );

    let pkts = pkt_lines(&res.body);
    assert_eq!(
        pkts[0].as_deref(),
        Some(&b"# service=git-upload-pack\n"[..]),
        "first packet must be the service banner; body was {:?}",
        String::from_utf8_lossy(&res.body)
    );
    assert_eq!(pkts[1], None, "the banner is followed by a flush");

    let first_ref = pkts[2].clone().expect("a ref advertisement follows the flush");
    let nul = first_ref.iter().position(|&b| b == 0).expect("caps behind a NUL");
    assert_eq!(
        &first_ref[..nul],
        format!("{oid} refs/heads/main").as_bytes(),
        "first advertised ref"
    );
    let caps = String::from_utf8_lossy(&first_ref[nul + 1..]).into_owned();
    for want in ["side-band-64k", "ofs-delta", "object-format=sha1"] {
        assert!(caps.contains(want), "capability {want:?} missing from {caps:?}");
    }
    assert_eq!(*pkts.last().unwrap(), None, "the advertisement ends in a flush");
}

/// The same request with `Git-Protocol: version=2`.
///
/// http-backend.c:559 suppresses the `# service=` banner for v2, and
/// http-backend.c:822-824 is what turns the header into the child's
/// `GIT_PROTOCOL` — drop either and the client is silently served v0, which a v2
/// client then fails to parse. Stock git opens the body with `000eversion 2`.
#[test]
fn v2_info_refs_has_no_banner_and_advertises_commands() {
    let fx = Fixture::new("v2refs");
    let res = fx.cgi(
        &[
            ("REQUEST_METHOD", "GET"),
            ("PATH_INFO", "/repo.git/info/refs"),
            ("QUERY_STRING", "service=git-upload-pack"),
            ("HTTP_GIT_PROTOCOL", "version=2"),
        ],
        b"",
    );

    assert_eq!(
        res.headers,
        format!("{NOCACHE}Content-Type: application/x-git-upload-pack-advertisement\r\n"),
        "header block diverged; stderr was {:?}",
        res.stderr
    );

    let pkts = pkt_lines(&res.body);
    let payloads: Vec<String> = pkts
        .iter()
        .map(|p| p.as_ref().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default())
        .collect();
    assert_eq!(payloads[0], "version 2\n", "v2 opens with the version line, not a banner");
    assert!(
        payloads.iter().any(|p| p.starts_with("ls-refs")),
        "the v2 advertisement must offer ls-refs: {payloads:?}"
    );
    assert!(
        payloads.iter().any(|p| p.starts_with("fetch")),
        "the v2 advertisement must offer fetch: {payloads:?}"
    );
    assert!(
        payloads.iter().any(|p| p == "object-format=sha1\n"),
        "the v2 advertisement must name the object format: {payloads:?}"
    );
    assert_eq!(*pkts.last().unwrap(), None, "the advertisement ends in a flush");
}

/// `POST /git-upload-pack` carrying a v2 `ls-refs`.
///
/// This is `service_rpc` end to end: the `…-result` content type
/// (http-backend.c:672), the request body reaching the child's stdin — buffered
/// whole, since `rpc_service[]` sets `buffer_input` for `upload-pack`
/// (http-backend.c:43) — and the child's answer reaching the client. Stock git
/// answers `003d<oid> refs/heads/main` then a flush.
#[test]
fn v2_post_ls_refs_round_trips_through_the_child() {
    let fx = Fixture::new("v2post");
    let oid = fx.head_oid();
    let body = b"0014command=ls-refs\n0017object-format=sha1\n00010009peel\n0000";
    let res = fx.cgi(
        &[
            ("REQUEST_METHOD", "POST"),
            ("PATH_INFO", "/repo.git/git-upload-pack"),
            ("CONTENT_TYPE", "application/x-git-upload-pack-request"),
            ("HTTP_GIT_PROTOCOL", "version=2"),
        ],
        body,
    );

    assert_eq!(
        res.headers,
        format!("{NOCACHE}Content-Type: application/x-git-upload-pack-result\r\n"),
        "header block diverged; stderr was {:?}",
        res.stderr
    );
    let pkts = pkt_lines(&res.body);
    assert_eq!(
        pkts[0].as_deref().map(|b| String::from_utf8_lossy(b).into_owned()),
        Some(format!("{oid} refs/heads/main\n")),
        "ls-refs answer; stderr was {:?}",
        res.stderr
    );
    assert_eq!(pkts[1], None, "ls-refs ends in a flush");
}

/// The same POST with an explicit `CONTENT_LENGTH`, which selects
/// `read_request_fixed_len` over the read-to-EOF path (http-backend.c:376-382).
/// A backend that ignored the length would block waiting for an EOF a real web
/// server never sends on a keep-alive connection.
#[test]
fn v2_post_honours_content_length() {
    let fx = Fixture::new("v2len");
    let oid = fx.head_oid();
    let body = b"0014command=ls-refs\n0017object-format=sha1\n00010009peel\n0000";
    let res = fx.cgi(
        &[
            ("REQUEST_METHOD", "POST"),
            ("PATH_INFO", "/repo.git/git-upload-pack"),
            ("CONTENT_TYPE", "application/x-git-upload-pack-request"),
            ("CONTENT_LENGTH", &body.len().to_string()),
            ("HTTP_GIT_PROTOCOL", "version=2"),
        ],
        body,
    );
    let pkts = pkt_lines(&res.body);
    assert_eq!(
        pkts[0].as_deref().map(|b| String::from_utf8_lossy(b).into_owned()),
        Some(format!("{oid} refs/heads/main\n")),
        "ls-refs answer under CONTENT_LENGTH; stderr was {:?}",
        res.stderr
    );
}

/// `GIT_HTTP_MAX_REQUEST_BUFFER` smaller than the body.
///
/// http-backend.c:350-354 dies before the body is read. The `die` lands after
/// `close(1)` (http-backend.c:507), so stock git emits the `fatal:` line on
/// stderr and **no** `Status: 500` block — the headers were already sent and the
/// fd is gone. Both halves are asserted: a port that kept fd 1 open would append
/// a second header block to the response body.
#[test]
fn oversized_request_body_is_refused_after_the_headers() {
    let fx = Fixture::new("maxbuf");
    let body = b"0014command=ls-refs\n0017object-format=sha1\n00010009peel\n0000";
    let res = fx.cgi(
        &[
            ("REQUEST_METHOD", "POST"),
            ("PATH_INFO", "/repo.git/git-upload-pack"),
            ("CONTENT_TYPE", "application/x-git-upload-pack-request"),
            ("CONTENT_LENGTH", &body.len().to_string()),
            ("GIT_HTTP_MAX_REQUEST_BUFFER", "4"),
            ("HTTP_GIT_PROTOCOL", "version=2"),
        ],
        body,
    );

    assert_eq!(
        res.headers,
        format!("{NOCACHE}Content-Type: application/x-git-upload-pack-result\r\n"),
        "the headers go out before the body is read"
    );
    assert!(
        res.body.is_empty(),
        "nothing may follow the headers once fd 1 is closed, got {:?}",
        String::from_utf8_lossy(&res.body)
    );
    assert_eq!(
        res.stderr.trim_end(),
        format!(
            "fatal: request was larger than our maximum size (4): {}; \
             try setting GIT_HTTP_MAX_REQUEST_BUFFER",
            body.len()
        ),
        "diagnostic text"
    );
}

/// `POST /git-receive-pack` without `REMOTE_USER`.
///
/// `rpc_service[]` gives `receive-pack` a negative default (http-backend.c:44),
/// so `select_service` enables it only for an authenticated request
/// (http-backend.c:288-293). The refusal must come *before* any service runs,
/// and with `REMOTE_USER` set the service must actually be reached — pinning
/// both directions keeps a port from "passing" by refusing everything.
#[test]
fn receive_pack_needs_remote_user() {
    let fx = Fixture::new("rpauth");
    let anon = fx.cgi(
        &[
            ("REQUEST_METHOD", "POST"),
            ("PATH_INFO", "/repo.git/git-receive-pack"),
            ("CONTENT_TYPE", "application/x-git-receive-pack-request"),
        ],
        b"0000",
    );
    assert_eq!(
        anon.headers,
        format!("Status: 403 Forbidden\r\n{NOCACHE}"),
        "anonymous receive-pack must be refused"
    );
    assert_eq!(anon.stderr.trim_end(), "Service not enabled: 'receive-pack'");

    let authed = fx.cgi(
        &[
            ("REQUEST_METHOD", "POST"),
            ("PATH_INFO", "/repo.git/git-receive-pack"),
            ("CONTENT_TYPE", "application/x-git-receive-pack-request"),
            ("REMOTE_USER", "bob"),
        ],
        b"0000",
    );
    assert_eq!(
        authed.headers,
        format!("{NOCACHE}Content-Type: application/x-git-receive-pack-result\r\n"),
        "an authenticated receive-pack reaches the service; stderr was {:?}",
        authed.stderr
    );
}

/// A `Content-Encoding: gzip` request body.
///
/// `remote-curl` gzips small `upload-pack` POSTs, so this is the common path for
/// an HTTP fetch, not a corner. http-backend.c:384-445 inflates it on the way to
/// the child with a gzip-only stream; a backend that passed the compressed bytes
/// through would hand `upload-pack` garbage.
#[test]
fn gzipped_request_body_is_inflated() {
    let fx = Fixture::new("gzip");
    let oid = fx.head_oid();
    let plain = b"0014command=ls-refs\n0017object-format=sha1\n00010009peel\n0000";
    let gz = gzip(plain);
    let res = fx.cgi(
        &[
            ("REQUEST_METHOD", "POST"),
            ("PATH_INFO", "/repo.git/git-upload-pack"),
            ("CONTENT_TYPE", "application/x-git-upload-pack-request"),
            ("HTTP_CONTENT_ENCODING", "gzip"),
            ("CONTENT_LENGTH", &gz.len().to_string()),
            ("HTTP_GIT_PROTOCOL", "version=2"),
        ],
        &gz,
    );
    let pkts = pkt_lines(&res.body);
    assert_eq!(
        pkts[0].as_deref().map(|b| String::from_utf8_lossy(b).into_owned()),
        Some(format!("{oid} refs/heads/main\n")),
        "ls-refs answer from a gzipped body; stderr was {:?}",
        res.stderr
    );
}

/// `GIT_HTTP_EXPORT_ALL=` — set, but empty.
///
/// http-backend.c:814 is `!getenv("GIT_HTTP_EXPORT_ALL")`, a NULL test with no
/// look at the value, so an empty one exports the repository. A port that reads
/// the variable as a string and treats `""` as unset locks out every deployment
/// whose web server exports the name unconditionally — the request 404s with the
/// repository sitting right there.
#[test]
fn empty_export_all_still_exports() {
    let fx = Fixture::new("exportall");
    std::fs::remove_file(fx.bare().join("git-daemon-export-ok")).unwrap();

    let mut cmd = Command::new(BIN);
    let out = cmd
        .arg("http-backend")
        .current_dir(&fx.root)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &fx.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_PROJECT_ROOT", &fx.root)
        .env("GIT_HTTP_EXPORT_ALL", "")
        .env("REQUEST_METHOD", "GET")
        .env("PATH_INFO", "/repo.git/HEAD")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.starts_with("Status: 404"),
        "an empty GIT_HTTP_EXPORT_ALL must still export: {text:?} / {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("Content-Type: text/plain\r\n"),
        "the HEAD route should have answered: {text:?}"
    );
}

/// `REQUEST_METHOD=` — set, but empty.
///
/// http-backend.c:777 only rejects a NULL, so an empty method is carried into
/// the route table, matches `/HEAD$` on method mismatch, and comes back as
/// `bad_request` — a `400` here, because `SERVER_PROTOCOL` is unset
/// (http-backend.c:753-758). Stock git 2.55.0 answers `Status: 400 Bad Request`
/// where a port that treats `""` as absent answers `500` with
/// `fatal: No REQUEST_METHOD from server`.
#[test]
fn empty_request_method_is_a_bad_request_not_a_missing_one() {
    let fx = Fixture::new("emptymethod");
    let res = fx.cgi(&[("REQUEST_METHOD", ""), ("PATH_INFO", "/repo.git/HEAD")], b"");
    assert_eq!(
        res.headers,
        format!("Status: 400 Bad Request\r\n{NOCACHE}"),
        "stderr was {:?}",
        res.stderr
    );
    assert!(res.stderr.is_empty(), "bad_request says nothing on stderr");
}

/// An unparseable `GIT_HTTP_MAX_REQUEST_BUFFER`.
///
/// `git_env_ulong` dies rather than falling back to the default
/// (http-backend.c:820), and it does so before the route handler runs, so the
/// answer is a clean `500` with no service started. A port that silently ignored
/// the value would serve the request with a buffer cap the operator believed
/// they had changed.
#[test]
fn unparseable_max_request_buffer_is_fatal() {
    let fx = Fixture::new("badmaxbuf");
    let res = fx.cgi(
        &[
            ("REQUEST_METHOD", "GET"),
            ("PATH_INFO", "/repo.git/HEAD"),
            ("GIT_HTTP_MAX_REQUEST_BUFFER", "bogus"),
        ],
        b"",
    );
    assert_eq!(
        res.headers,
        format!("Status: 500 Internal Server Error\r\n{NOCACHE}")
    );
    assert_eq!(
        res.stderr.trim_end(),
        "fatal: failed to parse GIT_HTTP_MAX_REQUEST_BUFFER"
    );
}

/// gzip-wrap `data` the way a client would, with no compressor and no external
/// tool: an RFC 1952 header, RFC 1951 *stored* deflate blocks, and the CRC-32 /
/// length trailer. Stored blocks are a legal deflate stream, so this exercises
/// the same inflater a real `Content-Encoding: gzip` body would, and the test
/// stays hermetic on a CI box with no `gzip(1)`.
fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff];
    // Stored deflate blocks: LEN/NLEN framed, at most 65535 bytes each.
    let mut chunks = data.chunks(65535).peekable();
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        out.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// The IEEE CRC-32 the gzip trailer carries.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}
