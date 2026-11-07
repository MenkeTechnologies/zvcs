//! `GIT_TRACE_PACKET` under protocol v0 and v1 over `file://`, for `ls-remote`,
//! `fetch`, `clone` and `push`.
//!
//! Two things decide these traces beyond `packet_trace()` itself:
//!
//! * `git_connect()` sets `GIT_PROTOCOL=version=<n>` for every version above 0
//!   (connect.c:1525-1529), so `protocol.version=1` makes the server open with
//!   `version 1`, which `discover_version()` reads and drops (connect.c:143-181).
//!   zvcs set the variable for v0 and not for v1, so a v1 conversation was a v0
//!   one.
//! * After `find_common()`'s last `NAK`, the pack is read by `sideband_demux()`'s
//!   async, whose identity is `sideband` (fetch-pack.c:991-1000, pkt-line.c:20-23).
//!   zvcs looked ahead at the first pack packet while still reading
//!   acknowledgements, and traced it as `fetch<` or `clone<`.
//!
//! The client and the server write to one trace concurrently, so lines are
//! compared one identity at a time, object names and `agent=` masked. The `want`
//! line's capability order is a pending decision and is masked too.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, dir: &Path, root: &Path, envs: &[(&str, &str)], args: &[&str]) -> (String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
}

/// One trace line with what may legitimately differ masked: object names, the
/// `agent=` value and, on a `want` line, the capability list after the name.
fn mask(line: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for w in line.split(' ') {
        let w = if w.len() == 40 && w.bytes().all(|b| b.is_ascii_hexdigit()) {
            "<H>".to_owned()
        } else if w.starts_with("agent=") {
            "agent=X".to_owned()
        } else {
            w.to_owned()
        };
        words.push(w);
    }
    let masked = words.join(" ");
    match masked.find(" want <H> ") {
        Some(at) => format!("{} want <H> <caps>", &masked[..at]),
        None => masked,
    }
}

fn by_identity(trace: &str) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in trace.lines() {
        let rest = line.strip_prefix("packet: ").unwrap_or_else(|| panic!("not a packet line: {line:?}"));
        map.entry(rest[..12].trim().to_owned()).or_default().push(mask(line));
    }
    map
}

fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-trace-packet-v1-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    let f = Fixture { root };
    let stock = stock_git::stock_git().expect("checked by caller");
    let src = f.root.join("src");
    git(stock, &src, &f.root, &[], &["init", "-q", "-b", "main"]);
    for i in 1..=2 {
        std::fs::write(src.join("f"), format!("{i}\n")).unwrap();
        git(stock, &src, &f.root, &[], &["add", "f"]);
        git(stock, &src, &f.root, &[], &["commit", "-qm", &format!("c{i}")]);
    }
    git(stock, &src, &f.root, &[], &["tag", "-a", "-m", "t", "v1"]);
    f
}

/// Run `op` with `bin` at `protocol.version=<version>`, returning the bare trace.
fn traced(f: &Fixture, bin: &str, name: &str, version: &str, op: &str) -> String {
    let stock = stock_git::stock_git().unwrap();
    let root = &f.root;
    let src = root.join("src");
    let url = format!("file://{}", src.display());
    let out = root.join(format!("out-{name}-{op}-v{version}"));
    let trace = root.join(format!("trace-{name}-{op}-v{version}"));
    let envs = [("GIT_TRACE_PACKET", trace.to_str().unwrap()), ("GIT_TRACE_BARE", "1")];
    let pv = format!("protocol.version={version}");
    let (err, code) = match op {
        "ls-remote" => git(bin, root, root, &envs, &["-c", &pv, "ls-remote", &url]),
        "clone" => git(bin, root, root, &envs, &["-c", &pv, "clone", "-q", &url, out.to_str().unwrap()]),
        "fetch" => {
            git(stock, root, root, &[], &["init", "-q", out.to_str().unwrap()]);
            git(bin, &out, root, &envs, &["-c", &pv, "fetch", "-q", &url, "refs/heads/*:refs/remotes/o/*"])
        }
        "push" => {
            git(stock, root, root, &[], &["init", "-q", "--bare", out.to_str().unwrap()]);
            let dst = format!("file://{}", out.display());
            git(bin, &src, root, &envs, &["-c", &pv, "push", "-q", &dst, "main", "v1"])
        }
        _ => unreachable!(),
    };
    assert_eq!(code, 0, "{name} {op} v{version}: {err}");
    std::fs::read_to_string(&trace).unwrap_or_default()
}

fn compare(version: &str, op: &str) -> BTreeMap<String, Vec<String>> {
    let f = fixture(&format!("{op}-v{version}"));
    let want = by_identity(&traced(&f, stock_git::stock_git().unwrap(), "stock", version, op));
    let got = by_identity(&traced(&f, ZVCS, "zvcs", version, op));
    assert!(!want.is_empty());
    assert_eq!(want, got, "{op} under protocol v{version}");
    want
}

/// Protocol v1: both sides trace the `version 1` line, on every transport command.
#[test]
fn trace_packet_parity_protocol_v1_over_file() {
    if stock_git::stock_git().is_none() {
        return;
    }
    for op in ["ls-remote", "fetch", "clone", "push"] {
        let want = compare("1", op);
        let client = if op == "push" { "push" } else { op };
        assert!(want[client].iter().any(|l| l.ends_with("< version 1")), "{op}: {:?}", want[client]);
    }
}

/// The first pack packet is the demultiplexer's, under v0 and v1 alike.
#[test]
fn trace_packet_parity_pack_read_by_sideband() {
    if stock_git::stock_git().is_none() {
        return;
    }
    for version in ["0", "1"] {
        for op in ["fetch", "clone"] {
            let want = compare(version, op);
            assert!(want["sideband"].iter().any(|l| l.ends_with("sideband< PACK ...")), "{op} v{version}");
        }
    }
}
