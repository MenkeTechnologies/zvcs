//! `fetch --negotiate-only`, MAX_IN_VAIN and a clone's want list, packet for packet.
//!
//! `negotiate_using_fetch()` (fetch-pack.c:2226-2331) writes only `wait-for-done` and
//! the haves, no `thin-pack`/`ofs-delta`, and closes without a flush (transport.c:574-580).
//! A request with no new have still goes out, and `upload-pack` answers it with a
//! `packfile` section, so a tip the remote shares nothing with ends in
//! `fatal: expected 'acknowledgments', received 'packfile'`; MAX_IN_VAIN haves after
//! an ACK end the rounds instead. The acknowledged commits print in oidset order
//! (builtin/fetch.c:2812-2814). An ACK marks every SEEN ancestor common
//! (negotiator/default.c:57-103), so those are never offered again.
//!
//! `fetch_pack()` drops sought refs only by name (fetch-pack.c:2009-2028), so a clone
//! wants `HEAD` and the branch it points at, the same oid twice.
//!
//! The server is stock `upload-pack` for both clients, so its packet trace is what each
//! client sent. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// What a client sent, as the stock server's trace shows it. The `agent=` value names
/// the client, and its position among the capabilities is not what this compares.
fn sent_lines(trace: &Path) -> Vec<String> {
    std::fs::read_to_string(trace)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once("packet:  upload-pack< ").map(|(_, p)| p.to_owned()))
        .filter(|p| !p.starts_with("agent="))
        .map(|p| match p.strip_prefix("want ") {
            // The v0/v1 capability list rides on the first want; keep the oid only.
            Some(rest) => format!("want {}", &rest[..rest.find(' ').unwrap_or(rest.len())]),
            None => p,
        })
        .collect()
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-negotiate-only-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    /// Stock git with auto-maintenance off: a fetch would otherwise leave a detached
    /// `maintenance run --auto` writing into `dir` while [`Fixture::run`] copies it.
    fn stock(&self, dir: &str, args: &[&str]) -> String {
        let stock = stock_git::stock_git().expect("checked by caller");
        let all = [&["-c", "maintenance.auto=false"][..], args].concat();
        let (out, err, code) = git(stock, &self.root.join(dir), &self.root, &[], &all);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }

    /// `n` commits on `branch` of `dir`, one second apart from `*t`, the first on `from`.
    fn commits(&self, dir: &str, branch: &str, from: Option<&str>, prefix: &str, n: usize, t: &mut u64) {
        let mut stream = String::new();
        for i in 1..=n {
            *t += 1;
            let msg = format!("{prefix}{i}\n");
            stream += &format!("commit refs/heads/{branch}\ncommitter A <a@example.com> {t} +0000\n");
            stream += &format!("data {}\n{msg}", msg.len());
            if i == 1 {
                if let Some(from) = from {
                    stream += &format!("from {from}\n");
                }
            }
            stream += &format!("M 644 inline f\ndata {}\n{msg}\n", msg.len());
        }
        let stock = stock_git::stock_git().expect("checked by caller");
        let mut child = Command::new(stock)
            .args(["fast-import", "--quiet", "--force"])
            .current_dir(self.root.join(dir))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
    }

    /// `up` has ten base commits; `down` clones it, then `up` gains five and `down` gains
    /// two on `main`, branches `x` and `y` of three commits on `main~4` and `main~2` of the
    /// clone, and `orphan`, `orphans` unrelated commits older than everything else.
    fn build(tag: &str, orphans: usize) -> Self {
        let f = Fixture::new(tag);
        let mut t = 1_600_000_000;
        std::fs::create_dir_all(f.root.join("up")).unwrap();
        f.stock("up", &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(f.root.join("seed")).unwrap();
        f.stock("seed", &["init", "-q", "-b", "main"]);
        if orphans > 0 {
            f.commits("seed", "orphan", None, "orphan", orphans, &mut t);
        }
        t = 1_700_000_000;
        f.commits("up", "main", None, "base", 10, &mut t);
        f.stock(".", &["clone", "-q", "up", "down"]);
        f.commits("up", "main", Some("refs/heads/main^0"), "remote", 5, &mut t);
        f.commits("down", "main", Some("refs/heads/main^0"), "local", 2, &mut t);
        f.commits("down", "x", Some("refs/remotes/origin/main~4"), "x", 3, &mut t);
        f.commits("down", "y", Some("refs/remotes/origin/main~2"), "y", 3, &mut t);
        if orphans > 0 {
            let seed = f.root.join("seed");
            f.stock("down", &["fetch", "-q", seed.to_str().unwrap(), "orphan:orphan"]);
        }
        f.stock("down", &["reset", "-q", "--hard"]);
        f
    }

    /// Run `bin` with `args` in a fresh copy of `down`, against stock `upload-pack`.
    fn run(&self, bin: &str, label: &str, args: &[&str]) -> (Vec<String>, String, String, i32) {
        let copy = self.root.join(format!("down-{label}"));
        let _ = std::fs::remove_dir_all(&copy);
        let status = Command::new("cp")
            .args(["-R", "down"])
            .arg(&copy)
            .current_dir(&self.root)
            .status()
            .unwrap();
        assert!(status.success());
        let trace = self.root.join(format!("trace-{label}"));
        let _ = std::fs::remove_file(&trace);
        let stock = stock_git::stock_git().expect("checked by caller");
        let upload_pack = format!("remote.origin.uploadpack={stock} upload-pack");
        let mut all = vec!["-c", upload_pack.as_str()];
        all.extend_from_slice(args);
        let (out, err, code) = git(bin, &copy, &self.root, &[("GIT_TRACE_PACKET", trace.to_str().unwrap())], &all);
        (sent_lines(&trace), out, err, code)
    }

    fn assert_parity(&self, args: &[&str]) {
        let stock = stock_git::stock_git().expect("checked by caller");
        let want = self.run(stock, "stock", args);
        let got = self.run(ZVCS, "zvcs", args);
        assert_eq!(got.0, want.0, "request packets for {args:?}");
        assert_eq!((&got.1, &got.2, got.3), (&want.1, &want.2, want.3), "stdout, stderr and exit code for {args:?}");
    }
}

#[test]
fn negotiate_only_rounds_and_the_common_listing() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::build("rounds", 3);
    for tip in ["main", "x", "y", "refs/heads/[xy]"] {
        f.assert_parity(&["fetch", "--negotiate-only", &format!("--negotiation-tip={tip}"), "origin"]);
    }
    f.assert_parity(&[
        "fetch",
        "--negotiate-only",
        "--negotiation-tip=x",
        "--negotiation-include=refs/remotes/origin/main~6",
        "origin",
    ]);
}

#[test]
fn negotiate_only_with_an_unshared_tip_sends_an_empty_round_and_dies() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::build("unshared", 3);
    f.assert_parity(&["fetch", "--negotiate-only", "--negotiation-tip=refs/heads/*", "origin"]);
}

#[test]
fn max_in_vain_ends_the_rounds_after_an_ack() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::build("in-vain", 400);
    f.assert_parity(&["fetch", "--negotiate-only", "--negotiation-tip=refs/heads/*", "origin"]);
    // A plain fetch over protocol v0. Over v2 the haves are the same, but the request's `symrefs`/`peel` and
    // `ofs-delta`/`include-tag` arguments still go out in gitoxide's order, which `sent_lines()` compares.
    f.assert_parity(&["-c", "protocol.version=0", "fetch", "origin"]);
}

#[test]
fn clone_wants_head_and_its_branch() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::build("clone", 0);
    let stock = stock_git::stock_git().expect("checked above");
    let up = f.root.join("up");
    let upload_pack = format!("{stock} upload-pack");
    for p in ["protocol.version=2", "protocol.version=0"] {
        let mut runs = Vec::new();
        for (label, bin) in [("stock", stock), ("zvcs", ZVCS)] {
            let trace = f.root.join(format!("clone-trace-{label}"));
            let _ = std::fs::remove_file(&trace);
            let dest = f.root.join(format!("clone-{label}"));
            let _ = std::fs::remove_dir_all(&dest);
            let (_, err, code) = git(
                bin,
                &f.root,
                &f.root,
                &[("GIT_TRACE_PACKET", trace.to_str().unwrap())],
                &[
                    "-c",
                    p,
                    "clone",
                    "-q",
                    "-u",
                    &upload_pack,
                    &format!("file://{}", up.display()),
                    dest.to_str().unwrap(),
                ],
            );
            let wants: Vec<String> = sent_lines(&trace).into_iter().filter(|l| l.starts_with("want ")).collect();
            runs.push((wants, err, code));
        }
        assert_eq!(runs[1], runs[0], "want lines, stderr and exit code for {p}");
    }
}
