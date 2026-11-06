//! `git fetch` and `git clone` over smart HTTP whose pack response is cut
//! short, compared with stock git: stderr, exit code, the refs, `fsck` and the
//! object files left behind.
//!
//! `get_pack()` reads the pack through `sideband_demux()`, whose
//! `recv_sideband()` stops at the packet that ends the stream or reports
//! `fetch-pack: unexpected disconnect while reading sideband packet` at the
//! end of the stream (sideband.c:325-331). The child that takes the pack then
//! meets its end of input: `read_pack_header()`'s `protocol error: bad pack
//! header`, or `unpack-objects`/`index-pack`'s `early EOF` and `get_pack()`'s
//! `<cmd> failed` or `fetch-pack: invalid index-pack output`; a child that
//! read everything is followed by `error in sideband demultiplexer`
//! (fetch-pack.c:960-1100). Over protocol v2 remote-curl ends every response
//! with a response-end packet (remote-curl.c:1086-1087), which ends the
//! demultiplexer cleanly at a packet boundary, and `do_check_stateless_delimiter()`
//! then finds the end of the stream (fetch-pack.c:1847).
//!
//! A request whose response was cut short is not sent again: curl only
//! retries one that received nothing (transfer.c:595-657 in curl 8.22.0).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

#[path = "support/cgi_server.rs"]
mod cgi_server;
use cgi_server::{CutAt, CutHow, Fault};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// `n` bytes that do not compress, so the pack spans several sideband packets.
fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed;
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 56) as u8
        })
        .collect()
}

#[derive(Debug, PartialEq)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    refs: String,
    fsck: String,
    objects: Vec<String>,
    clone_exists: bool,
    posts: usize,
}

/// A bare repository one commit ahead of a work repository that cloned it, served
/// with `fault`. `bin` runs `git -c protocol.version=<protocol> <config...> fetch origin`
/// in the work repository, or `clone <url> c` beside it.
fn run(stock: &str, bin: &str, tag: &str, protocol: u8, config: &[&str], clone: bool, fault: Fault) -> Run {
    let root: PathBuf = std::env::temp_dir().join(format!("zvcs-packcut-{tag}-{}", std::process::id()));
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
    let src = root.join("src");
    let bare = root.join("srv/r.git");
    let work = root.join("w");
    git(&root, &["init", "-q", "-b", "main", src.to_str().unwrap()]);
    std::fs::write(src.join("big"), noise(1, 200_000)).unwrap();
    std::fs::write(src.join("f"), "a\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-qm", "a"]);
    git(&root, &["clone", "-q", "--bare", src.to_str().unwrap(), bare.to_str().unwrap()]);
    git(&root, &["clone", "-q", src.to_str().unwrap(), work.to_str().unwrap()]);
    std::fs::write(src.join("big2"), noise(2, 200_000)).unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-qm", "b"]);
    git(&src, &["push", "-q", bare.to_str().unwrap(), "main"]);

    let (port, posts) = cgi_server::spawn(stock, root.join("srv"), fault);
    let url = format!("http://127.0.0.1:{port}/r.git");
    git(&work, &["remote", "set-url", "origin", &url]);

    let protocol = format!("protocol.version={protocol}");
    let mut args = vec!["-c", protocol.as_str()];
    for c in config {
        args.extend(["-c", c]);
    }
    if clone {
        args.extend(["clone", url.as_str(), "c"]);
    } else {
        args.extend(["fetch", "origin"]);
    }
    let out = Command::new(bin)
        .args(&args)
        .current_dir(if clone { &root } else { &work })
        .env("HOME", &root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let host = format!("127.0.0.1:{port}");
    // `check_pktline()`'s count of the bytes a cut pkt-line still owed (remote-curl.c:1081-1084) depends on where
    // upload-pack's sideband packets fall, which follows how pack-objects' output reached it, so stock's own count
    // differs from run to run of one fixture; the line itself is compared, the number is not.
    let still_expected = regex::Regex::new(r"error: \d+ bytes of body are still expected").unwrap();
    let scrub = |s: &[u8]| {
        let s = String::from_utf8_lossy(s).replace(&host, "HOST");
        still_expected.replace_all(&s, "error: N bytes of body are still expected").into_owned()
    };

    let fsck = Command::new(stock)
        .args(["fsck"])
        .current_dir(&work)
        .env("HOME", &root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let tmp_pack = regex::Regex::new(r"tmp_pack_\w{6}$").unwrap();
    let mut objects = Vec::new();
    let mut stack = vec![work.join(".git/objects")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = path.strip_prefix(&work).unwrap().to_string_lossy().into_owned();
                // `mkstemp()`'s six random characters.
                objects.push(tmp_pack.replace_all(&name, "tmp_pack_XXXXXX").into_owned());
            }
        }
    }
    objects.sort();
    let result = Run {
        code: out.status.code(),
        stdout: scrub(&out.stdout),
        stderr: scrub(&out.stderr),
        refs: git(&work, &["for-each-ref"]),
        fsck: format!("{}{}", String::from_utf8_lossy(&fsck.stdout), String::from_utf8_lossy(&fsck.stderr)),
        objects,
        clone_exists: root.join("c").exists(),
        posts: posts.load(std::sync::atomic::Ordering::SeqCst),
    };
    let _ = std::fs::remove_dir_all(&root);
    result
}

/// Run one case through both binaries and compare everything; `expected` is a line
/// stock's stderr must contain, so a fixture that stopped failing is noticed.
fn check(tag: &str, protocol: u8, config: &[&str], clone: bool, fault: Fault, expected: &str) {
    let Some(stock) = stock_git() else { return };
    let want = run(stock, stock, &format!("{tag}-s"), protocol, config, clone, fault);
    let got = run(stock, BIN, &format!("{tag}-z"), protocol, config, clone, fault);
    assert_eq!(got, want, "{tag}: zvcs and stock git differ");
    assert!(want.stderr.contains(expected), "{tag}: stock's stderr lacks {expected:?}:\n{}", want.stderr);
}

const CLOSE: CutHow = CutHow::Close;

#[test]
fn fetch_parity_pack_cut_before_its_header_is_bad_pack_header() {
    for protocol in [0, 1, 2] {
        let tag = format!("before-v{protocol}");
        let fault = Fault::cut(CutAt::BeforePack, CLOSE);
        check(&tag, protocol, &[], false, fault, "fatal: protocol error: bad pack header\n");
        let fault = Fault::cut(CutAt::InHeader, CLOSE);
        check(&format!("{tag}-hdr"), protocol, &[], false, fault, "fatal: protocol error: bad pack header\n");
    }
}

#[test]
fn fetch_parity_pack_cut_midway_is_unpack_objects_early_eof() {
    for protocol in [0, 1, 2] {
        for how in [CutHow::Close, CutHow::Reset, CutHow::ShortLength] {
            let tag = format!("half-v{protocol}-{how:?}");
            let fault = Fault::cut(CutAt::Half, how);
            check(&tag, protocol, &[], false, fault, "fatal: early EOF\nfatal: unpack-objects failed\n");
        }
    }
}

#[test]
fn fetch_parity_pack_without_its_flush_is_a_demultiplexer_error() {
    for protocol in [0, 1] {
        for how in [CutHow::Close, CutHow::ShortLength] {
            let tag = format!("flush-v{protocol}-{how:?}");
            let fault = Fault::cut(CutAt::LastFlush, how);
            check(&tag, protocol, &[], false, fault, "fatal: error in sideband demultiplexer\n");
        }
        // `index-pack` read the whole pack, so it stays with the `.keep` the child could not report.
        let tag = format!("flush-v{protocol}-kept");
        let fault = Fault::cut(CutAt::LastFlush, CLOSE);
        check(&tag, protocol, &["fetch.unpackLimit=1"], false, fault, "fatal: error in sideband demultiplexer\n");
    }
    for config in [&[][..], &["fetch.unpackLimit=1"][..]] {
        let tag = format!("flush-v2-{}", config.len());
        let fault = Fault::cut(CutAt::LastFlush, CLOSE);
        check(&tag, 2, config, false, fault, "fatal: the remote end hung up unexpectedly\n");
    }
}

/// `fetch.unpackLimit=0` reads no pack header, so `do_keep` stays `args->keep_pack` and a fetch gives every pack
/// to `unpack-objects` (fetch-pack.c:991-998, :1007): a pack that arrived whole is left loose, a cut one is
/// `early EOF` with the objects before the cut written.
#[test]
fn fetch_parity_pack_cut_with_unpack_limit_zero_goes_to_unpack_objects() {
    let config = ["transfer.unpackLimit=0"];
    for protocol in [0, 1, 2] {
        let tag = format!("limit0-v{protocol}");
        let expected = if protocol == 2 {
            "fatal: the remote end hung up unexpectedly\n"
        } else {
            "fatal: error in sideband demultiplexer\n"
        };
        check(&tag, protocol, &config, false, Fault::cut(CutAt::LastFlush, CLOSE), expected);
        let tag = format!("limit0-half-v{protocol}");
        let expected = "fatal: early EOF\nfatal: unpack-objects failed\n";
        check(&tag, protocol, &config, false, Fault::cut(CutAt::Half, CLOSE), expected);
    }
}

/// `fetch.fsckObjects` sends a small pack to `index-pack`, which is told `--keep` only for a pack `get_pack()`
/// keeps (fetch-pack.c:1019-1020), so the pack a v0/v1 child indexed before the demultiplexer failed has no
/// `.keep`, and a cut one leaves `index-pack`'s `tmp_pack_XXXXXX`.
#[test]
fn fetch_parity_pack_cut_under_fsck_objects_goes_to_index_pack() {
    let config = ["fetch.fsckObjects=true"];
    for protocol in [0, 1, 2] {
        let tag = format!("fsck-flush-v{protocol}");
        let expected = if protocol == 2 {
            "fatal: the remote end hung up unexpectedly\n"
        } else {
            "fatal: error in sideband demultiplexer\n"
        };
        check(&tag, protocol, &config, false, Fault::cut(CutAt::LastFlush, CLOSE), expected);
        let tag = format!("fsck-half-v{protocol}");
        let expected = "fatal: early EOF\nfatal: index-pack failed\n";
        check(&tag, protocol, &config, false, Fault::cut(CutAt::Half, CLOSE), expected);
    }
}

#[test]
fn clone_parity_pack_cut_short_removes_the_clone() {
    for protocol in [0, 1, 2] {
        let tag = format!("clone-before-v{protocol}");
        check(&tag, protocol, &[], true, Fault::cut(CutAt::BeforePack, CLOSE), "fatal: ");
        // Protocol v0/v1 runs a `fetch-pack` child that was not told to keep the pack, so a small one goes to
        // `unpack-objects`; v2 keeps it, so `index-pack` takes it.
        let expected = if protocol == 2 {
            "fatal: early EOF\nfatal: fetch-pack: invalid index-pack output\n"
        } else {
            "fatal: early EOF\nfatal: unpack-objects failed\n"
        };
        check(&format!("clone-half-v{protocol}"), protocol, &[], true, Fault::cut(CutAt::Half, CLOSE), expected);
        let tag = format!("clone-flush-v{protocol}");
        check(&tag, protocol, &[], true, Fault::cut(CutAt::LastFlush, CLOSE), "fatal: ");
    }
}

#[test]
fn fetch_parity_cut_response_is_not_sent_again() {
    for protocol in [0, 1, 2] {
        let tag = format!("once-v{protocol}");
        let fault = Fault {
            cut: Some((CutAt::Half, CLOSE)),
            once: true,
        };
        check(&tag, protocol, &[], false, fault, "fatal: unpack-objects failed\n");
    }
}
