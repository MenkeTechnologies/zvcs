//! `am` makes its state directory first and reads `core.logAllRefUpdates` right after.
//!
//! `am_setup()` runs `mkdir(state->dir)`, then `delete_ref(NULL, "REBASE_HEAD", NULL, REF_NO_DEREF)`,
//! and only then `split_mail()` writes `0001` and the option files. A deletion opens a ref
//! transaction whether or not the ref exists, and in 2.56 the files backend reads its write options
//! (`core.logAllRefUpdates`) when the transaction is prepared, so a value `git_config_bool()`
//! refuses is a `fatal:` at that point: `.git/rebase-apply` is left behind, empty. zvcs skipped the
//! deletion for an absent `REBASE_HEAD`, wrote the whole session and died later, at the
//! `ORIG_HEAD` update, with `rebase-apply` complete.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

const MAIL: &[u8] = b"From 1234567890abcdef1234567890abcdef12345678 Mon Sep 17 00:00:00 2001\n\
From: Example Author <author@example.invalid>\n\
Date: Tue, 14 Nov 2023 22:13:20 +0000\n\
Subject: [PATCH] add a line\n\
\n\
commit body\n\
---\n README.md | 1 +\n 1 file changed, 1 insertion(+)\n\n\
diff --git a/README.md b/README.md\n\
index 9741694..2a1b3c4 100644\n\
--- a/README.md\n\
+++ b/README.md\n\
@@ -1 +1,2 @@\n # fixture\n+added line\n-- \n2.55.0\n";

fn git(bin: &str, dir: &Path, stdin: &[u8], args: &[&str]) -> (String, String, i32) {
    let root = dir.parent().unwrap();
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
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
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn repo(label: &str, bin: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-am-statedir-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    git(bin, &work, b"", &["init", "-q", "-b", "main", "."]);
    std::fs::write(work.join("README.md"), "# fixture\n").unwrap();
    git(bin, &work, b"", &["add", "README.md"]);
    git(bin, &work, b"", &["commit", "-q", "-m", "base"]);
    work
}

/// The sorted names under `.git/rebase-apply`, or `None` when the directory is absent.
fn session_files(work: &Path) -> Option<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(work.join(".git/rebase-apply"))
        .ok()?
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Some(names)
}

fn same(label: &str, args: &[&str]) -> (String, Option<Vec<String>>) {
    let stock = stock_git::stock_git().expect("checked by caller");
    let s = repo(&format!("{label}-stock"), stock);
    let z = repo(&format!("{label}-zvcs"), ZVCS);
    let want = git(stock, &s, MAIL, args);
    let got = git(ZVCS, &z, MAIL, args);
    let (want_files, got_files) = (session_files(&s), session_files(&z));
    let _ = std::fs::remove_dir_all(s.parent().unwrap());
    let _ = std::fs::remove_dir_all(z.parent().unwrap());
    assert_eq!(got, want, "git {args:?}: left is zvcs, right is stock");
    assert_eq!(got_files, want_files, "git {args:?}: .git/rebase-apply, left is zvcs, right is stock");
    (want.1, want_files)
}

#[test]
fn a_refused_reflog_setting_leaves_the_empty_state_directory() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let (stderr, files) = same("refused", &["-c", "core.logAllRefUpdates= ", "am"]);
    assert!(stderr.contains("bad boolean config value"), "{stderr}");
    assert_eq!(files, Some(Vec::new()));
    same("refused-word", &["-c", "core.logAllRefUpdates=sometimes", "am", "--quiet"]);
}

#[test]
fn an_accepted_setting_still_writes_the_whole_session_and_applies() {
    if stock_git::stock_git().is_none() {
        return;
    }
    let (_, files) = same("accepted", &["-c", "core.logAllRefUpdates=always", "am"]);
    assert_eq!(files, None, "a finished session removes its directory");
}
