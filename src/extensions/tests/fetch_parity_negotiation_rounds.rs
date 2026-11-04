//! The `have` rounds a fetch negotiates, packet for packet.
//!
//! Protocol v2's `send_fetch_request()` writes `done` only once the negotiator has
//! nothing left or MAX_IN_VAIN haves went unacknowledged (fetch-pack.c:1443-1447), so a
//! first round that runs short of its window is answered with `acknowledgments`, not a
//! pack. Protocol v0/v1's `find_common()` flushes at 16 and then at `next_flush()` of the
//! running count (fetch-pack.c:276-290, :521-537), stays one window ahead of the server
//! on a persistent connection (fetch-pack.c:539-544), and sends `done` with the haves of
//! the window still open (fetch-pack.c:612-619). In both, an advertised ref we hold —
//! an annotated tag included, peeled locally — is a COMMON_REF whose ancestors are never
//! offered (fetch-pack.c:851-863, negotiator/default.c:129-146). zvcs sent `done` with
//! any short round, doubled its v0/v1 windows, and offered a tag's history.
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

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fetch-negotiation-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn stock(&self, dir: &str, args: &[&str]) -> String {
        let stock = stock_git::stock_git().expect("checked by caller");
        let (out, err, code) = git(stock, &self.root.join(dir), &self.root, &[], args);
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

    /// `up` has ten base commits with `v1` on the seventh; `down` clones it. Then `up`
    /// gains `remote` commits and `v2`, and `down` gains `local` commits on `main` plus
    /// `branches` side branches of three commits each.
    fn diverged(tag: &str, local: usize, remote: usize, branches: usize) -> Self {
        let f = Fixture::new(tag);
        let mut t = 1_700_000_000;
        std::fs::create_dir_all(f.root.join("up")).unwrap();
        f.stock("up", &["init", "-q", "-b", "main"]);
        f.commits("up", "main", None, "base", 10, &mut t);
        f.stock("up", &["tag", "-a", "-m", "v1", "v1", "main~3"]);
        f.stock(".", &["clone", "-q", "up", "down"]);
        f.commits("up", "main", Some("refs/heads/main^0"), "remote", remote, &mut t);
        f.stock("up", &["tag", "-a", "-m", "v2", "v2", "main"]);
        f.commits("down", "main", Some("refs/heads/main^0"), "local", local, &mut t);
        for b in 1..=branches {
            let base = format!("refs/remotes/origin/main~{}", b % 5);
            f.commits("down", &format!("br{b}"), Some(&base), &format!("br{b}-"), 3, &mut t);
        }
        f.stock("down", &["reset", "-q", "--hard"]);
        f
    }

    /// `down` is unrelated to `up`: no history in common at all.
    fn unrelated(tag: &str) -> Self {
        let f = Fixture::new(tag);
        let mut t = 1_700_000_000;
        for dir in ["up", "down"] {
            std::fs::create_dir_all(f.root.join(dir)).unwrap();
            f.stock(dir, &["init", "-q", "-b", "main"]);
        }
        f.commits("up", "main", None, "up", 5, &mut t);
        f.commits("down", "main", None, "down", 300, &mut t);
        let up = f.root.join("up");
        f.stock("down", &["remote", "add", "origin", up.to_str().unwrap()]);
        f
    }

    /// Run `fetch` with `bin` in a fresh copy of `down`, against stock `upload-pack`.
    /// Returns what the client sent (the server's view), stderr, exit code, and the
    /// refs and object counts left behind.
    fn fetch(&self, bin: &str, label: &str, extra: &[&str]) -> (Vec<String>, String, i32, String, String) {
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
        let stock = stock_git::stock_git().expect("checked by caller");
        let upload_pack = format!("remote.origin.uploadpack={stock} upload-pack");
        let mut args = vec!["-c", upload_pack.as_str()];
        args.extend_from_slice(extra);
        args.push("fetch");
        args.push("origin");
        let (_, err, code) = git(
            bin,
            &copy,
            &self.root,
            &[("GIT_TRACE_PACKET", trace.to_str().unwrap())],
            &args,
        );
        let sent = std::fs::read_to_string(&trace)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once("packet:  upload-pack< ").map(|(_, p)| p.to_owned()))
            // The `agent=` value names the client, which is not what this compares.
            .filter(|p| !p.starts_with("agent="))
            .map(|p| match p.strip_prefix("want ") {
                // The v0/v1 capability list rides on the first want; keep the oid only.
                Some(rest) => format!("want {}", &rest[..rest.find(' ').unwrap_or(rest.len())]),
                None => p,
            })
            .filter(|p| p.starts_with("want ") || p.starts_with("have ") || p == "done" || p == "0000")
            .collect();
        let refs = self.stock(&format!("down-{label}"), &["for-each-ref"]);
        let counts = self
            .stock(&format!("down-{label}"), &["count-objects", "-v"])
            .lines()
            .filter(|l| l.starts_with("count:") || l.starts_with("in-pack:") || l.starts_with("packs:"))
            .collect::<Vec<_>>()
            .join("\n");
        (sent, err, code, refs, counts)
    }

    fn assert_parity(&self, extra: &[&str]) {
        let stock = stock_git::stock_git().expect("checked by caller");
        let want = self.fetch(stock, "stock", extra);
        let got = self.fetch(ZVCS, "zvcs", extra);
        assert_eq!(got.0, want.0, "request packets for {extra:?}");
        assert_eq!((&got.1, got.2), (&want.1, want.2), "stderr and exit code for {extra:?}");
        assert_eq!(got.3, want.3, "refs for {extra:?}");
        assert_eq!(got.4, want.4, "objects for {extra:?}");
    }
}

const PROTOCOLS: [&str; 2] = ["protocol.version=2", "protocol.version=0"];

#[test]
fn a_short_first_round_is_not_done_and_a_tag_ancestry_is_not_offered() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::diverged("short", 5, 5, 0);
    for p in PROTOCOLS {
        f.assert_parity(&["-c", p]);
    }
}

#[test]
fn a_hundred_commit_divergence_takes_the_same_windows() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::diverged("hundred", 100, 5, 0);
    for p in PROTOCOLS {
        f.assert_parity(&["-c", p]);
    }
}

#[test]
fn many_local_branches_and_tags_take_the_same_windows() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::diverged("branches", 3, 100, 40);
    for p in PROTOCOLS {
        f.assert_parity(&["-c", p]);
        f.assert_parity(&["-c", p, "-c", "fetch.negotiationAlgorithm=skipping"]);
    }
}

#[test]
fn no_common_history_ends_with_an_empty_done_round_and_v0_warns() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let f = Fixture::unrelated("unrelated");
    for p in PROTOCOLS {
        f.assert_parity(&["-c", p]);
    }
}
