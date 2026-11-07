//! `git update-ref --stdin --batch-updates` when an update of the branch `HEAD`
//! points at is rejected.
//!
//! lock_ref_for_update() runs split_head_update() and split_symref_update()
//! before it checks the old value (refs/files-backend.c:2709-2713, :2772-2814),
//! and ref_transaction_maybe_set_rejected() marks only the update that failed
//! (refs.c:1276-1310). The log-only updates split off it stay in the
//! transaction, so files_transaction_finish() still writes them (:3352-3360):
//! `.git/logs/HEAD` gains `<old> <new>` with the rejected update's own values
//! and message — the null id for a `create`, the expected value under
//! `REF_HAVE_OLD` (:2760-2764), the null id as the new value of a `verify` — and
//! a symref the update came through gains `<null> <new>`. Its `rejected` line
//! names the end of the symref chain. zvcs wrote none of these lines, wrote a
//! rejected `verify`'s mirror with the ref's current value, and wrote a second
//! `HEAD` line for an accepted `verify`. The reftable backend already agreed.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct World {
    root: PathBuf,
    bin: String,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl World {
    /// `main` at `a` then `b`, built by `bin` in the given ref format.
    fn new(label: &str, bin: &str, format: &str) -> World {
        let root = std::env::temp_dir().join(format!("zvcs-update-ref-head-log-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let w = World { root: std::fs::canonicalize(&root).unwrap(), bin: bin.to_owned() };
        w.run(&w.bin, &["init", "-q", "-b", "main", &format!("--ref-format={format}"), "repo"], None);
        for name in ["a", "b"] {
            std::fs::write(w.repo().join(name), format!("{name}\n")).unwrap();
            w.run(&w.bin, &["add", name], None);
            w.run(&w.bin, &["commit", "-q", "-m", name], None);
        }
        w
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn run(&self, bin: &str, args: &[&str], stdin: Option<&str>) -> (String, String, i32) {
        let dir: &Path = if args.first() == Some(&"init") { &self.root } else { &self.repo() };
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", self.root.parent().unwrap())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.unwrap_or("").as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        (text(&out.stdout), text(&out.stderr), out.status.code().unwrap_or(-1))
    }

    fn oid(&self, stock: &str, spec: &str) -> String {
        self.run(stock, &["rev-parse", spec], None).0.trim().to_string()
    }

    /// The batch's own answer, then the repository as stock reads it back.
    fn batch(&self, stock: &str, msg: Option<&str>, input: &str) -> Vec<(String, String, i32)> {
        let mut args = vec!["update-ref"];
        if let Some(m) = msg {
            args.extend(["-m", m]);
        }
        args.extend(["--stdin", "--batch-updates"]);
        let mut seen = vec![self.run(&self.bin, &args, Some(input))];
        for read in [
            &["for-each-ref"][..],
            &["reflog", "show", "HEAD", "--format=%H %gs|"],
            &["reflog", "show", "main", "--format=%H %gs|"],
            &["reflog", "show", "refs/heads/sym", "--format=%H %gs|"],
            &["log", "-g", "HEAD", "--format=%gD %gn %gs|"],
            &["fsck"],
        ] {
            seen.push(self.run(stock, read, None));
        }
        for log in ["HEAD", "refs/heads/main", "refs/heads/sym"] {
            let raw = std::fs::read(self.repo().join(".git/logs").join(log)).unwrap_or_default();
            seen.push((String::from_utf8_lossy(&raw).into_owned(), String::new(), 0));
        }
        seen
    }
}

/// Each case: an optional `-m`, whether `refs/heads/sym` points at `main`
/// first, and the input, with `{A}`, `{B}` and `{Z}` standing for `main~1`, `main`
/// and the null id.
const CASES: &[(Option<&str>, bool, &str)] = &[
    (None, false, "create refs/heads/main {A}\n"),
    (Some("why"), false, "create refs/heads/main {A}\n"),
    (None, false, "update refs/heads/main {A} {A}\n"),
    (None, false, "update refs/heads/main {A} {Z}\n"),
    (Some("bye"), false, "delete refs/heads/main {A}\n"),
    (None, false, "verify refs/heads/main {A}\n"),
    (None, false, "verify refs/heads/main {B}\n"),
    (None, false, "verify refs/heads/main {Z}\n"),
    (None, false, "create refs/heads/other {A}\ncreate refs/heads/main {A}\n"),
    (None, false, "update HEAD {A} {A}\n"),
    (None, false, "create HEAD {A}\n"),
    (None, false, "delete HEAD {A}\n"),
    (None, true, "update refs/heads/sym {A} {A}\n"),
    (None, true, "update refs/heads/sym {A} {B}\n"),
];

#[test]
fn a_rejected_update_of_the_head_branch_still_logs_its_splits() {
    let Some(stock) = stock_git::stock_git() else { return };
    for format in ["files", "reftable"] {
        for (n, &(msg, sym, input)) in CASES.iter().enumerate() {
            let label = format!("{format}-{n}");
            let s = World::new(&format!("{label}-stock"), stock, format);
            let z = World::new(&format!("{label}-zvcs"), ZVCS, format);
            let input = input
                .replace("{A}", &s.oid(stock, "main~1"))
                .replace("{B}", &s.oid(stock, "main"))
                .replace("{Z}", &"0".repeat(40));
            assert_eq!(z.oid(stock, "main"), s.oid(stock, "main"), "{label}: fixtures differ");
            for w in [&s, &z] {
                if sym {
                    w.run(stock, &["symbolic-ref", "refs/heads/sym", "refs/heads/main"], None);
                }
            }
            assert_eq!(z.batch(stock, msg, &input), s.batch(stock, msg, &input), "{label}: {input}");
        }
    }
}
