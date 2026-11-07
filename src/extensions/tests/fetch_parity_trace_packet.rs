//! `GIT_TRACE_PACKET`: every pkt-line a transport command writes or reads, as
//! `packet_trace()` reports it (pkt-line.c:40-86).
//!
//! Each line is `packet: %12s%c ` — the identity `packet_trace_identity()` set
//! (`ls-remote`, `push`, `upload-pack`, `receive-pack`, ..., or `sideband` on the
//! demultiplexer's async), `>` for a write and `<` for a read — then the payload with
//! newlines dropped and anything outside 0x20-0x7e as `\%o`. `trace_strbuf_fl()` puts
//! `HH:MM:SS.uuuuuu pkt-line.c:85` in front, padded to column 40, unless
//! `GIT_TRACE_BARE` is set (trace.c:98-124). `get_trace_fd()` takes `1`/`true` for
//! stderr, a single digit for that descriptor, an absolute path to append to, and
//! warns about anything else (trace.c:37-75). zvcs traced nothing.
//!
//! The client and the server it spawns write to the same destination concurrently,
//! so the traces are compared one identity at a time. Protocol v2's capability and
//! argument order is an open question, so v2 lines are compared as sets.
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

fn git(bin: &str, dir: &Path, root: &Path, envs: &[(&str, &str)], args: &[&str]) -> (String, String, i32) {
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
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// The `agent=` value, which names the binary, as `agent=X`.
fn mask_agent(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find("agent=") {
        out.push_str(&rest[..at + 6]);
        out.push('X');
        rest = &rest[at + 6..];
        rest = &rest[rest.find([' ', '\n']).unwrap_or(rest.len())..];
    }
    out.push_str(rest);
    out
}

/// The trace lines grouped by identity, in the order each process wrote them, or
/// sorted when `as_set`.
fn by_identity(trace: &str, as_set: bool) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in trace.lines() {
        let rest = line.strip_prefix("packet: ").unwrap_or_else(|| panic!("not a packet line: {line:?}"));
        let id = rest[..13].trim().to_owned();
        map.entry(id).or_default().push(mask_agent(line));
    }
    if as_set {
        for lines in map.values_mut() {
            lines.sort();
        }
    }
    map
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-trace-packet-{tag}-{}", std::process::id()));
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

    fn url(&self) -> String {
        format!("file://{}", self.root.join("src").display())
    }

    /// `bin` with `GIT_TRACE_PACKET` at a file, returning the trace and the exit code.
    fn traced(&self, bin: &str, dir: &Path, bare: bool, args: &[&str]) -> (String, i32) {
        let trace = self.root.join("trace");
        let _ = std::fs::remove_file(&trace);
        let mut envs = vec![("GIT_TRACE_PACKET", trace.to_str().unwrap())];
        if bare {
            envs.push(("GIT_TRACE_BARE", "1"));
        }
        let (_, err, code) = git(bin, dir, &self.root, &envs, args);
        assert!(code == 0, "{bin} {args:?}: {err}");
        (std::fs::read_to_string(&trace).unwrap_or_default(), code)
    }
}

#[test]
fn trace_packet_parity_ls_remote_per_protocol() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let stock = stock_git::stock_git().unwrap();
    let f = Fixture::new("ls-remote");
    let url = f.url();
    for (version, as_set) in [("0", false), ("2", true)] {
        let args = ["-c", &format!("protocol.version={version}"), "ls-remote", &url];
        let (want, _) = f.traced(stock, &f.root, true, &args);
        let (got, _) = f.traced(ZVCS, &f.root, true, &args);
        assert!(!want.is_empty());
        assert_eq!(by_identity(&want, as_set), by_identity(&got, as_set), "protocol v{version}");
    }
}

/// `prepare_trace_line()`: `HH:MM:SS.uuuuuu pkt-line.c:85`, padded to column 40.
#[test]
fn trace_packet_parity_line_prefix() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let stock = stock_git::stock_git().unwrap();
    let f = Fixture::new("prefix");
    let url = f.url();
    let args = ["-c", "protocol.version=0", "ls-remote", &url];
    let strip = |trace: &str| -> String {
        let mut out = String::new();
        for line in trace.lines() {
            let b = line.as_bytes();
            let clock = b.len() > 40
                && [0, 1, 3, 4, 6, 7].iter().all(|&i| b[i].is_ascii_digit())
                && b[2] == b':'
                && b[5] == b':'
                && b[8] == b'.'
                && b[9..15].iter().all(u8::is_ascii_digit);
            assert!(clock, "no timestamp: {line:?}");
            assert_eq!(&line[15..40], " pkt-line.c:85           ", "{line:?}");
            out.push_str(&line[40..]);
            out.push('\n');
        }
        out
    };
    let (want, _) = f.traced(stock, &f.root, false, &args);
    let (got, _) = f.traced(ZVCS, &f.root, false, &args);
    assert_eq!(by_identity(&strip(&want), false), by_identity(&strip(&got), false));
}

/// `get_trace_fd()`'s destinations, and its warning for a relative path.
#[test]
fn trace_packet_parity_destinations() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let stock = stock_git::stock_git().unwrap();
    let f = Fixture::new("dest");
    let url = f.url();
    let args = ["-c", "protocol.version=0", "ls-remote", &url];
    for value in ["1", "2", "true", "TRUE", "0", "false", "", "relative/trace", "/nonexistent-dir/trace"] {
        let run = |bin: &str| {
            let (out, err, code) =
                git(bin, &f.root, &f.root, &[("GIT_TRACE_PACKET", value), ("GIT_TRACE_BARE", "1")], &args);
            let mut err: Vec<String> = err.lines().map(mask_agent).collect();
            err.sort();
            (out, err, code)
        };
        assert_eq!(run(stock), run(ZVCS), "GIT_TRACE_PACKET={value:?}");
    }
}

/// A push's side-band report: the demultiplexer reads each band as `sideband`,
/// and `receive_status()` reads the `report-status` lines out of band 1 as `push`.
#[test]
fn trace_packet_parity_push_report() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let stock = stock_git::stock_git().unwrap();
    let f = Fixture::new("push");
    let mut traces = Vec::new();
    for (name, bin) in [("stock", stock), ("zvcs", ZVCS)] {
        let bare = f.root.join(format!("bare-{name}"));
        let work = f.root.join(format!("work-{name}"));
        git(stock, &f.root, &f.root, &[], &["init", "-q", "--bare", bare.to_str().unwrap()]);
        git(stock, &f.root, &f.root, &[], &["clone", "-q", f.url().as_str(), work.to_str().unwrap()]);
        let url = format!("file://{}", bare.display());
        let (trace, _) = f.traced(bin, &work, true, &["push", "-q", &url, "main:refs/heads/main"]);
        traces.push(trace);
    }
    let mask_oids = |t: &str| -> String {
        t.split_inclusive(['\n', ' '])
            .map(|w| {
                let core = w.trim_end_matches(['\n', ' ']);
                if core.len() == 40 && core.bytes().all(|b| b.is_ascii_hexdigit()) {
                    w.replacen(core, "<H>", 1)
                } else {
                    w.to_owned()
                }
            })
            .collect()
    };
    let want = mask_oids(&traces[0]);
    let got = mask_oids(&traces[1]);
    assert!(want.contains("sideband<"));
    assert_eq!(by_identity(&want, false), by_identity(&got, false));
}
