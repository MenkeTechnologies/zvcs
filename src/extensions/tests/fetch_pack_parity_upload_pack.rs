//! `fetch-pack --upload-pack=<program>` / `--exec=<program>`, and the one connection it runs.
//!
//! `cmd_fetch_pack()` takes the last `--upload-pack=` or `--exec=` as `args.uploadpack`
//! (builtin/fetch-pack.c:74-90) and hands it to the single `git_connect()` whose
//! advertisement `fetch_pack()` then fetches over (:226-259), so the program runs once.
//! `report_unmatched_refs()` names a sought ref the server lacked after that fetch, ahead
//! of the fetched refs (:268-281, fetch-pack.c:2333-2354), a v0/v1 server that advertised
//! nothing is `die(_("no matching remote head"))` (fetch-pack.c:2165-2168), and a command
//! line naming no ref still connects before it exits 1. zvcs refused any program but
//! `git-upload-pack`, listed the refs over one connection and fetched over a second, and
//! decided the no-ref and missing-ref cases before connecting.
//!
//! Stock git is the oracle (`support/stock_git.rs`); the program is a script that says it
//! ran and then execs stock `upload-pack`, so both clients talk to the same server.
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

fn git(bin: &str, dir: &Path, root: &Path, args: &[&str]) -> (String, String, i32) {
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
        .output()
        .unwrap();
    let root = root.to_string_lossy().into_owned();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
    (text(&out.stdout), text(&out.stderr), out.status.code().expect("no signal"))
}

impl Fixture {
    /// `remote.git` with `main`, `side` and a tag, an empty `empty.git`, an empty `dest` to
    /// fetch into, and `upload.sh`, which names its arguments on stderr and execs stock
    /// `upload-pack`.
    fn new() -> Self {
        let stock = stock_git::stock_git().expect("checked by caller");
        let root = std::env::temp_dir().join(format!("zvcs-fetch-pack-upload-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root };
        f.stock(".", &["init", "-q", "-b", "main", "repo"]);
        for n in 1..=3 {
            std::fs::write(f.root.join("repo/a"), format!("{n}\n")).unwrap();
            f.stock("repo", &["add", "a"]);
            f.stock("repo", &["commit", "-q", "-m", &format!("c{n}")]);
        }
        f.stock("repo", &["branch", "side", "main~1"]);
        f.stock("repo", &["tag", "-a", "-m", "t", "v1", "main~2"]);
        f.stock(".", &["-c", "maintenance.auto=false", "clone", "-q", "--bare", "repo", "remote.git"]);
        f.stock(".", &["init", "-q", "--bare", "empty.git"]);
        f.stock(".", &["init", "-q", "dest"]);
        let script = f.root.join("upload.sh");
        std::fs::write(&script, format!("#!/bin/sh\necho \"upload.sh $#: $*\" >&2\nexec '{stock}' upload-pack \"$@\"\n"))
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    fn stock(&self, dir: &str, args: &[&str]) -> String {
        let stock = stock_git::stock_git().expect("checked by caller");
        let (out, err, code) = git(stock, &self.root.join(dir), &self.root, args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }

    /// `bin` runs `git -c protocol.version=<v> fetch-pack <args>` in a fresh copy of `dest`: its
    /// output and the objects it left there.
    fn run(&self, bin: &str, label: &str, v: &str, args: &[&str]) -> (i32, String, String, String) {
        let copy = self.root.join(format!("dest-{label}"));
        let _ = std::fs::remove_dir_all(&copy);
        let status = Command::new("cp").arg("-R").arg(self.root.join("dest")).arg(&copy).status().unwrap();
        assert!(status.success());
        let proto = format!("protocol.version={v}");
        let mut all = vec!["-c", "maintenance.auto=false", "-c", proto.as_str(), "fetch-pack"];
        all.extend_from_slice(args);
        let (stdout, stderr, code) = git(bin, &copy, &self.root, &all);
        let objects = self
            .stock(&format!("dest-{label}"), &["count-objects", "-v"])
            .lines()
            .filter(|l| l.starts_with("count:") || l.starts_with("in-pack:") || l.starts_with("packs:"))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::remove_dir_all(&copy);
        (code, stdout, stderr, objects)
    }
}

#[test]
fn the_named_program_serves_the_one_connection() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let stock = stock_git::stock_git().unwrap();
    let f = Fixture::new();
    let script = f.root.join("upload.sh");
    let upload_pack = format!("--upload-pack={}", script.display());
    let exec = format!("--exec={}", script.display());
    let missing = format!("--upload-pack={}", f.root.join("nosuch").display());
    for v in ["0", "1", "2"] {
        let runs: Vec<Vec<&str>> = vec![
            vec![&upload_pack, "--no-progress", "../remote.git", "refs/heads/main"],
            vec![&exec, "--no-progress", "../remote.git", "refs/heads/main", "refs/heads/side"],
            vec![&upload_pack, "--no-progress", "--all", "../remote.git"],
            vec![&upload_pack, "-q", "-k", "../remote.git", "refs/heads/side"],
            vec![&exec, &upload_pack, "--no-progress", "../remote.git", "refs/heads/main"],
            vec!["--upload-pack=git-upload-pack", "--no-progress", "../remote.git", "refs/heads/main"],
            vec![&missing, "../remote.git", "refs/heads/main"],
            // No ref named: the program still runs, and the command exits 1.
            vec![&upload_pack, "../remote.git"],
            // A sought ref the server lacks is reported after the fetch.
            vec![&upload_pack, "--no-progress", "../remote.git", "refs/heads/main", "refs/heads/nosuch"],
            vec![&upload_pack, "../remote.git", "refs/heads/nosuch"],
            vec![&upload_pack, "--all", "../empty.git"],
            vec![&upload_pack, "../empty.git", "refs/heads/main"],
        ];
        for args in runs {
            let want = f.run(stock, "stock", v, &args);
            let got = f.run(ZVCS, "zvcs", v, &args);
            assert_eq!(got, want, "v{v} fetch-pack {args:?}");
        }
    }
}
