//! The `<rev>^!`, `<rev>^@` and `<rev>^-<n>` marks, and the line between the two
//! functions that read an object name.
//!
//! `handle_revision_arg_1()` (`revision.c`) strips these three marks *before* it
//! resolves anything:
//!
//! ```c
//! mark = strstr(arg, "^@");
//! if (mark && !mark[2]) { … }
//! mark = strstr(arg, "^!");
//! if (mark && !mark[2]) { … }
//! mark = strstr(arg, "^-");
//! if (mark) { … }
//! ```
//!
//! so a *revision walk* understands them. `repo_get_oid()` does not go through
//! that function, and `get_oid_1()` (`object-name.c:1084-1142`) has no case for
//! them at all — it reads a `^` only as a trailing `~<n>`/`^<n>` suffix it strips
//! before recursing, or as the `^{` that opens a `peel_onion()` group:
//!
//! ```c
//! for (cp = name + len - 1; name <= cp; cp--) {
//!         int ch = *cp;
//!         if ('0' <= ch && ch <= '9')
//!                 continue;
//!         if (ch == '~' || ch == '^')
//!                 has_suffix = ch;
//!         break;
//! }
//! …
//! ret = peel_onion(r, name, len, oid, lookup_flags);
//! if (!ret) return FOUND;
//! ret = get_oid_basic(r, name, len, oid, lookup_flags);
//! ```
//!
//! Whatever survives that reduction is handed to `get_oid_basic()`,
//! `get_describe_name()` and `get_short_oid()`, and none of the three can accept
//! a residual `^`: two want hex digits, one wants a `-g<hex>` tail, and
//! `repo_dwim_ref()` cannot match because `check_refname_format()` bans `^` in a
//! refname (`refname_disposition[0x5e] == 4`, `refs.c:80-89`).
//!
//! So `git rev-list HEAD^!` walks a range while `git cat-file -t HEAD^!` is
//! `fatal: Not a valid object name HEAD^!` — one command line, two answers,
//! decided by which resolver the builtin reaches. gitoxide draws no such line:
//! its parser returns `Spec::ExcludeParents` for `<rev>^!` and
//! `gix::revision::Spec::single()` hands that back as an ordinary single object,
//! so every command resolving an argv operand used to accept a spelling git
//! refuses.
//!
//! Every expectation below was measured from stock git 2.55.0 against the same
//! fixture, and the controls matter as much as the cases: the reduction must
//! keep accepting `HEAD^^`, `HEAD^{commit}`, a `:/<regex>` search pattern that
//! *contains* a `^`, and a `<rev>:<path>` whose path does.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "zvcs test")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "zvcs test")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn git(repo: &Path, home: &Path, args: &[&str]) {
    let out = run(repo, home, args);
    assert!(out.status.success(), "git {args:?} failed: {}", err_of(&out));
}

fn err_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn out_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Three linear commits, a lightweight tag and a second branch — enough for
/// every mark to have a real revision in front of it.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-objname-marks-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    for (n, body) in [("one", "one\n"), ("two", "two\n"), ("three", "three\n")] {
        std::fs::write(repo.join("f.txt"), body).unwrap();
        git(&repo, &home, &["add", "f.txt"]);
        git(&repo, &home, &["commit", "-q", "-m", n]);
    }
    git(&repo, &home, &["tag", "ltag", "HEAD~1"]);
    (repo, home)
}

/// Every marked spelling, against the commands that reach `repo_get_oid()`
/// directly. The message and the exit status are each builtin's own — that is
/// the point: the *rejection* is shared, the diagnostic is not.
#[test]
fn parent_marks_are_not_object_names() {
    let (repo, home) = fixture("reject");

    for spec in ["HEAD^!", "HEAD^@", "HEAD^-1", "HEAD^!^!", "main^!", "HEAD^{commit}^!"] {
        for (args, code, want_err) in [
            (vec!["cat-file", "-t", spec], 128, format!("fatal: Not a valid object name {spec}\n")),
            (vec!["describe", spec], 128, format!("fatal: Not a valid object name {spec}\n")),
            (vec!["ls-tree", spec], 128, format!("fatal: Not a valid object name {spec}\n")),
            (
                vec!["merge-base", "--is-ancestor", "HEAD", spec],
                128,
                format!("fatal: Not a valid object name {spec}\n"),
            ),
            (
                vec!["notes", "list", spec],
                128,
                format!("fatal: failed to resolve '{spec}' as a valid ref.\n"),
            ),
            (
                vec!["branch", "--contains", spec],
                129,
                format!("error: malformed object name {spec}\n"),
            ),
            (
                vec!["cherry", "HEAD", spec],
                128,
                format!("fatal: unknown commit {spec}\n"),
            ),
            (
                vec!["merge-subtree", spec, "--", "HEAD", "HEAD~1"],
                128,
                format!("fatal: could not parse object '{spec}'\n"),
            ),
        ] {
            let out = run(&repo, &home, &args);
            assert_eq!(
                out.status.code(),
                Some(code),
                "`git {}`: wrong exit status; stderr:\n{}",
                args.join(" "),
                err_of(&out)
            );
            assert_eq!(err_of(&out), want_err, "`git {}`: wrong diagnostic", args.join(" "));
            assert_eq!(out_of(&out), "", "`git {}` writes nothing", args.join(" "));
        }
    }
}

/// `--points-at` is the one member of the option-callback family that quotes the
/// operand, and `name-rev` is the one command in the whole set that treats an
/// unresolvable name as a *skip* rather than a failure. Both are easy to get
/// wrong by sharing one message across the family.
#[test]
fn option_callbacks_and_name_rev_keep_their_own_wording() {
    let (repo, home) = fixture("callbacks");

    for spec in ["HEAD^!", "HEAD^@", "HEAD^-1"] {
        let out = run(&repo, &home, &["for-each-ref", &format!("--points-at={spec}")]);
        assert_eq!(out.status.code(), Some(129));
        assert_eq!(err_of(&out), format!("error: malformed object name '{spec}'\n"));

        let out = run(&repo, &home, &["name-rev", spec]);
        assert_eq!(out.status.code(), Some(0), "name-rev skips and exits 0");
        assert_eq!(err_of(&out), format!("Could not get sha1 for {spec}. Skipping.\n"));
        assert_eq!(out_of(&out), "");
    }
}

/// The reduction that decides the rejection must not eat a `^` that
/// `get_oid_1()` really does have a case for, nor one that never reaches it.
///
/// `HEAD^^` recurses twice through the suffix rule; `HEAD^{commit}` and `HEAD^{}`
/// are `peel_onion()` groups; `:/^two` is an anchored commit-message search
/// handled by `get_oid_with_context_1()`'s `name[0] == ':'` branch before
/// `get_oid_1()` decides anything; and `HEAD:f.txt` splits at the colon, so a
/// `^` on the *path* side is not the revision's problem.
#[test]
fn the_suffix_reduction_still_accepts_what_git_accepts() {
    let (repo, home) = fixture("controls");

    for spec in ["HEAD^^", "HEAD^1", "HEAD~2", "HEAD^{commit}", "HEAD^{}"] {
        let out = run(&repo, &home, &["cat-file", "-t", spec]);
        assert!(out.status.success(), "`cat-file -t {spec}` must resolve:\n{}", err_of(&out));
        assert_eq!(out_of(&out), "commit\n");
    }

    let out = run(&repo, &home, &["cat-file", "-t", "HEAD:f.txt"]);
    assert!(out.status.success(), "{}", err_of(&out));
    assert_eq!(out_of(&out), "blob\n");

    // A `^` inside a search pattern, on both spellings that carry one.
    let two = out_of(&run(&repo, &home, &["rev-parse", "HEAD~1"]));
    for spec in [":/^two", "HEAD^{/two}"] {
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert!(out.status.success(), "`rev-parse {spec}` must resolve:\n{}", err_of(&out));
        assert_eq!(out_of(&out), two, "`rev-parse {spec}` must find the `two` commit");
    }
}

/// A `^!` on the *path* side of a `<rev>:<path>` operand is part of the path, and
/// a file may legitimately be named that way.
///
/// `get_oid_with_context_1()` splits at the first unbracketed `:` and hands only
/// the left half to `get_oid_1()`, so the caret rule has to be applied to that
/// half and not to the operand — and `:<path>` (`object-name.c:1758`) never
/// reaches `get_oid_1()` at all. A rule applied to the whole string would turn
/// both of these into failures.
#[test]
fn a_caret_on_the_path_side_still_resolves() {
    let (repo, home) = fixture("colon");

    std::fs::write(repo.join("f^!.txt"), "caret\n").unwrap();
    git(&repo, &home, &["add", "--", "f^!.txt"]);
    git(&repo, &home, &["commit", "-q", "-m", "caret"]);

    for spec in ["HEAD:f^!.txt", ":f^!.txt"] {
        let out = run(&repo, &home, &["cat-file", "-t", spec]);
        assert!(out.status.success(), "`cat-file -t {spec}` must resolve:\n{}", err_of(&out));
        assert_eq!(out_of(&out), "blob\n");
    }
}
