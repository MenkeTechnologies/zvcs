//! A deepening fetch's negotiation, and the end of a conversation a fetch dies in.
//!
//! `mark_complete_and_common_ref()` marks nothing COMPLETE under `args->deepen`
//! (fetch-pack.c:838-848), so no advertised ref becomes a COMMON_REF and every local
//! ref's history is offered, and `transport_fetch_refs()` leaves a ref whose tracking
//! ref already holds the advertised object out of the wants (transport.c:1646-1668).
//! Over v0/v1 `find_common()` sends the wants and the deepen lines on their own, reads
//! the shallow list, registers it, and only then walks for `have`s (fetch-pack.c:452-481),
//! the request lines in the order wants, shallow, deepen (fetch-pack.c:380-451). zvcs
//! wanted every ref, offered only what the COMMON_REFs left, sent its first window of
//! haves ahead of the shallow list and the deepen line ahead of the shallow ones.
//!
//! A fetch that dies between the ref listing and the pack never reaches
//! `disconnect_git()`'s flush (transport.c:1020-1030); a v0/v1 `upload-pack` then meets
//! the end of its input and says so on the stderr both share. zvcs wrote the flush, so the
//! server exited quietly. A v2 fetch with nothing to fetch writes one flush, not two.
//!
//! Stock git is the oracle (`support/stock_git.rs`); each client fetches from its own
//! `upload-pack`, whose shallow lines are compared as a set, the order being the server's.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

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

#[derive(Debug, PartialEq, Eq)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
    sent: Vec<String>,
    refs: String,
    shallow: String,
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
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    let root = root.to_string_lossy().into_owned();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
    (text(&out.stdout), text(&out.stderr), out.status.code().expect("no signal"))
}

impl Fixture {
    /// `repo` has `c1..c3` on `main` and `side` off `c1`; `behind` clones it and commits on
    /// `main`; `repo` then gains `four` and is cloned bare to `remote.git`, the remote of
    /// `behind`, of `shallow` (a `--depth=1` clone taken before `four`) and of `synced`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fetch-deepen-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        f.stock(".", &["init", "-q", "-b", "main", "repo"]);
        for n in 1..=3 {
            let body: String = (1..=n).map(|i| format!("{i}\n")).collect();
            std::fs::write(f.root.join("repo/a"), body).unwrap();
            f.stock("repo", &["add", "a"]);
            f.stock("repo", &["commit", "-q", "-m", &format!("c{n}")]);
        }
        f.stock("repo", &["checkout", "-q", "-b", "side", "main~2"]);
        std::fs::write(f.root.join("repo/a"), "1\n2\n").unwrap();
        f.stock("repo", &["commit", "-q", "-am", "two-again"]);
        f.stock("repo", &["checkout", "-q", "main"]);
        let repo_url = format!("file://{}", f.root.join("repo").display());
        f.stock(".", &["-c", "maintenance.auto=false", "clone", "-q", "repo", "behind"]);
        f.stock(".", &["-c", "maintenance.auto=false", "clone", "-q", "--depth=1", &repo_url, "shallow"]);
        f.stock("behind", &["commit", "-q", "--allow-empty", "-m", "local"]);
        f.stock("repo", &["commit", "-q", "--allow-empty", "-m", "four"]);
        f.stock(".", &["-c", "maintenance.auto=false", "clone", "-q", "--bare", "repo", "remote.git"]);
        f.stock(".", &["-c", "maintenance.auto=false", "clone", "-q", "remote.git", "synced"]);
        for dir in ["behind", "shallow"] {
            f.stock(dir, &["remote", "set-url", "origin", "../remote.git"]);
        }
        f
    }

    fn stock(&self, dir: &str, args: &[&str]) -> String {
        let stock = stock_git::stock_git().expect("checked by caller");
        let (out, err, code) = git(stock, &self.root.join(dir), &self.root, &[], args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }

    /// `bin` runs `args` in a fresh copy of `repo`: its output, the packets the client sent and
    /// read, and the refs and shallow boundary it left.
    fn run(&self, bin: &str, label: &str, repo: &str, args: &[&str]) -> Run {
        let copy = self.root.join(format!("{repo}-{label}"));
        let _ = std::fs::remove_dir_all(&copy);
        let status = Command::new("cp").arg("-R").arg(self.root.join(repo)).arg(&copy).status().unwrap();
        assert!(status.success());
        let trace = self.root.join(format!("trace-{label}"));
        let _ = std::fs::remove_file(&trace);
        let mut all = vec!["-c", "maintenance.auto=false"];
        all.extend_from_slice(args);
        let (stdout, stderr, code) = git(
            bin,
            &copy,
            &self.root,
            &[("GIT_TRACE_PACKET", trace.to_str().unwrap()), ("GIT_TRACE_BARE", "1")],
            &all,
        );
        let mut sent: Vec<String> = std::fs::read_to_string(&trace)
            .unwrap_or_default()
            .lines()
            // The client's side: `fetch`, or `fetch-pack` under remote-curl; the server and the
            // demultiplexer trace under names of their own.
            .filter_map(|l| {
                let (who, packet) = l.strip_prefix("packet: ")?.trim_start().split_once(['<', '>'])?;
                let dir = &l[l.len() - packet.len() - 1..][..1];
                matches!(who, "fetch" | "fetch-pack").then(|| format!("{dir}{packet}"))
            })
            // The agent names the client; `peel` and `include-tag` sit where the pending v2
            // argument order puts them, which is not what this compares.
            .filter(|p| !p.contains("agent=") && !p.ends_with(" peel") && !p.ends_with(" include-tag"))
            // Progress is chunked by the server's timing.
            .filter(|p| !p.starts_with("< PACK"))
            .collect();
        // The order of a shallow list is the server's walk; sort each run of it.
        let is_shallow = |p: &String| p.starts_with("< shallow ") || p.starts_with("< unshallow ");
        let mut start = 0;
        while start < sent.len() {
            let len = sent[start..].iter().take_while(|p| is_shallow(p)).count();
            sent[start..start + len].sort();
            start += len.max(1);
        }
        let refs = self.stock(&format!("{repo}-{label}"), &["for-each-ref"]);
        let shallow = std::fs::read_to_string(copy.join(".git/shallow")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&copy);
        Run {
            code,
            stdout,
            stderr: stderr.replace(&format!("{repo}-{label}"), &format!("{repo}-<side>")),
            sent,
            refs,
            shallow,
        }
    }

    fn assert_parity(&self, repo: &str, args: &[&str]) {
        let stock = stock_git::stock_git().expect("checked by caller");
        let want = self.run(stock, "stock", repo, args);
        let got = self.run(ZVCS, "zvcs", repo, args);
        assert_eq!(got, want, "{repo}: {args:?}");
    }
}

#[test]
fn a_deepening_fetch_wants_what_changed_and_offers_every_local_commit() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::new("behind");
    for v in ["0", "1", "2"] {
        let proto = format!("protocol.version={v}");
        for deepen in ["--depth=1", "--depth=2", "--depth=3", "--shallow-exclude=side", "--shallow-since=1699999000"] {
            f.assert_parity("behind", &["-c", &proto, "fetch", deepen, "origin"]);
        }
        // Every ref already up to date, a configured `branch.main.merge` among them: all are wanted.
        f.assert_parity("synced", &["-c", &proto, "fetch", "--depth=1", "origin"]);
    }
}

#[test]
fn a_shallow_repository_reads_the_shallow_list_before_its_haves() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::new("shallow");
    for v in ["0", "1"] {
        let proto = format!("protocol.version={v}");
        for deepen in ["--depth=3", "--unshallow", "--shallow-exclude=side"] {
            f.assert_parity("shallow", &["-c", &proto, "fetch", deepen, "origin"]);
        }
    }
}

#[test]
fn a_fetch_that_dies_after_the_listing_writes_no_flush() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::new("dies");
    for v in ["0", "1", "2"] {
        let proto = format!("protocol.version={v}");
        // v2 aside: its ls-refs prefixes for a missing ref differ in a way of their own.
        if v != "2" {
            f.assert_parity("behind", &["-c", &proto, "fetch", "origin", "refs/heads/nosuch"]);
        }
        f.assert_parity("behind", &["-c", &proto, "fetch", "origin", "main:refs/heads/main"]);
        f.assert_parity("behind", &["-c", &proto, "-c", "fetch.fsck.badTree=bogus", "fetch", "origin"]);
        // Nothing to fetch: the closing flush is written once.
        f.assert_parity("synced", &["-c", &proto, "fetch", "origin"]);
        f.assert_parity("synced", &["-c", &proto, "fetch", "-v", "origin"]);
    }
}
