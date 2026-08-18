//! Four things `git status` and `git commit` decide from `HEAD` and from
//! `status.showUntrackedFiles`, each of which this port previously got wrong.
//! Every expectation below was measured from stock git 2.55.0 with the global
//! and system config pinned away.
//!
//! 1. **`HEAD` need not be a commit for `status`.**
//!    `wt_status_collect_changes_index()` hands `opt.def = s->reference` to
//!    `setup_revisions()`, which resolves it with `get_reference()`
//!    (revision.c:353-369), and `run_diff_index()` then peels *that* object with
//!    `repo_parse_tree_indirect()` (diff-lib.c:555) — to a **tree**, not a
//!    commit. So the two ways it can fail are:
//!
//!    ```c
//!    tree = repo_parse_tree_indirect(the_repository, tree_oid);
//!    if (!tree)
//!            return error("bad tree object %s", tree_name ? tree_name : …);
//!    …
//!    if (diff_cache(revs, &oid, name, cached))
//!            exit(128);
//!    ```
//!
//!    (diff-lib.c:555-558, :647-648) for a `HEAD` on a blob, and
//!    `die("bad object %s", name)` (revision.c:368) for a `HEAD` naming an object
//!    the odb does not have. Both exit 128 with *nothing* on stdout, because
//!    `wt_status_collect()` runs before `wt_status_print()` (builtin/commit.c:1655).
//!
//! 2. **`git commit` does need a commit**, and says so in a different voice:
//!    `lookup_commit_or_die(&oid, "HEAD")` (builtin/commit.c:1816) prints
//!    `error("object %s is a %s, not a %s")` (commit.c:63) and then
//!    `die("could not parse %s")` (commit.c:85). A missing object skips the type
//!    test and prints only the `die()`.
//!
//! 3. **A detached `HEAD` is named after the reflog, not after its object.**
//!    `wt_status_get_detached_from()` (wt-status.c:1709-1743) reads `HEAD`'s
//!    reflog backwards for the most recent `checkout: moving from <x> to <y>`
//!    and reports `<y>`; `skip_prefix` strips `refs/tags/` and `refs/remotes/`
//!    but *not* `refs/heads/`. `at` vs `from` is whether `HEAD` still holds the
//!    object that switch landed on.
//!
//! 4. **`status.showUntrackedFiles` is boolean-coerced and validated.**
//!    `parse_untracked_setting_name()` (builtin/commit.c:1189-1213) runs the
//!    value through `git_parse_maybe_bool` first, so `false` means `no`; an
//!    unparseable value is `return error(_("Invalid untracked files mode '%s'"))`
//!    from the config callback, which kills `git status` *and* `git commit`
//!    before either prints anything.

//! Every template below is read back from an *aborted* commit. On a commit that
//! succeeds this port rewrites `COMMIT_EDITMSG` with the cleaned message before
//! running `commit-msg`, where git leaves the file exactly as the editor did —
//! a separate, pre-existing divergence that these tests deliberately do not
//! depend on either way.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn real_git_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.contains(".zvcs"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Every config source outside the repository pinned away — this machine's
/// `core.commentChar` must not decide what the commit template looks like.
fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    run_env(dir, home, args, &[])
}

fn run_env(dir: &Path, home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(BIN);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(args)
        .env("PATH", real_git_path())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "@1112911993 +0000")
        .env("GIT_COMMITTER_DATE", "@1112911993 +0000")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("EMAIL")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

/// An editor that touches nothing and fails, so `COMMIT_EDITMSG` is left exactly
/// as the template writer wrote it. A *successful* commit is no good for reading
/// the template back: this port rewrites the file with the cleaned message before
/// running `commit-msg` (see the note in the module header of this test).
fn failing_editor(repo: &Path) -> String {
    let script = repo.join("editor-fail.sh");
    std::fs::write(&script, "#!/bin/sh\nexit 1\n").expect("write editor");
    make_executable(&script);
    script.to_string_lossy().into_owned()
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository with two commits and a dirty worktree, plus the scratch `HOME`
/// every invocation is pinned to.
fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&repo).expect("create repo");
    let home = home.canonicalize().expect("canonicalize home");
    let repo = repo.canonicalize().expect("canonicalize repo");

    ok(&repo, &home, &["init", "-q", "-b", "main", "."]);
    std::fs::write(repo.join("f1.txt"), "one\n").expect("write f1");
    ok(&repo, &home, &["add", "f1.txt"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c1"]);
    std::fs::write(repo.join("f2.txt"), "two\n").expect("write f2");
    ok(&repo, &home, &["add", "f2.txt"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c2"]);
    (repo, home)
}

/// Point `.git/HEAD` straight at `oid`, the state `git checkout --detach` can
/// leave behind and no longer refuses to enter.
fn detach_onto(repo: &Path, oid: &str) {
    std::fs::write(repo.join(".git").join("HEAD"), format!("{oid}\n")).expect("write HEAD");
}

#[test]
fn head_on_a_blob_is_a_bad_tree_object_in_every_status_view() {
    let (repo, home) = fixture("head-blob");
    let blob = ok(&repo, &home, &["rev-parse", "HEAD:f1.txt"]).trim().to_string();
    std::fs::write(repo.join("f3.txt"), "three\n").expect("write f3");
    ok(&repo, &home, &["add", "f3.txt"]);
    detach_onto(&repo, &blob);

    // Every view goes through the same `run_diff_index`, so every view dies the
    // same way — and dies during collection, so stdout stays empty.
    for args in [
        vec!["status"],
        vec!["status", "--short"],
        vec!["status", "--long"],
        vec!["status", "--porcelain"],
        vec!["status", "--porcelain=v2", "--branch"],
        vec!["status", "-z"],
        vec!["status", "--branch", "--short"],
    ] {
        let out = run(&repo, &home, &args);
        assert_eq!(code(&out), 128, "exit code for {args:?}");
        assert_eq!(stderr(&out), "error: bad tree object HEAD\n", "stderr for {args:?}");
        assert_eq!(stdout(&out), "", "stdout for {args:?}");
    }
}

#[test]
fn head_naming_a_missing_object_is_a_bad_object() {
    let (repo, home) = fixture("head-missing");
    // A well-formed oid of the repository's hash length that no object has.
    let head = ok(&repo, &home, &["rev-parse", "HEAD"]).trim().to_string();
    let missing: String = head
        .chars()
        .map(|c| if c == 'a' { 'b' } else if c == '0' { '1' } else { 'a' })
        .collect();
    assert_ne!(missing, head);
    detach_onto(&repo, &missing);

    for args in [vec!["status"], vec!["status", "--porcelain"], vec!["status", "--porcelain=v2"]] {
        let out = run(&repo, &home, &args);
        assert_eq!(code(&out), 128, "exit code for {args:?}");
        assert_eq!(stderr(&out), "fatal: bad object HEAD\n", "stderr for {args:?}");
        assert_eq!(stdout(&out), "", "stdout for {args:?}");
    }
}

#[test]
fn commit_refuses_a_head_that_is_not_a_commit_in_two_voices() {
    let (repo, home) = fixture("head-commit");
    let blob = ok(&repo, &home, &["rev-parse", "HEAD:f1.txt"]).trim().to_string();
    let tree = ok(&repo, &home, &["rev-parse", "HEAD^{tree}"]).trim().to_string();

    for (oid, kind) in [(&blob, "blob"), (&tree, "tree")] {
        detach_onto(&repo, oid);
        // `--dry-run` never reaches the report: the check is `cmd_commit()`'s
        // first act, ahead of option validation.
        for args in [vec!["commit", "--dry-run"], vec!["commit", "-m", "x"]] {
            let out = run(&repo, &home, &args);
            assert_eq!(code(&out), 128, "exit code for {args:?} on a {kind}");
            assert_eq!(
                stderr(&out),
                format!("error: object {oid} is a {kind}, not a commit\nfatal: could not parse HEAD\n"),
                "stderr for {args:?} on a {kind}"
            );
        }
    }

    // A missing object fails inside `peel_object_ext()`, before the type test,
    // so only the `die()` is printed.
    let head = ok(&repo, &home, &["rev-parse", "refs/heads/main"]).trim().to_string();
    let missing: String = head.chars().map(|c| if c == 'a' { 'b' } else { 'a' }).collect();
    detach_onto(&repo, &missing);
    let out = run(&repo, &home, &["commit", "--dry-run"]);
    assert_eq!(code(&out), 128);
    assert_eq!(stderr(&out), "fatal: could not parse HEAD\n");
}

#[test]
fn detached_head_is_named_after_the_reflog_switch_not_its_object() {
    let (repo, home) = fixture("detached-name");
    ok(&repo, &home, &["tag", "-a", "-m", "tag1", "v1", "HEAD~1"]);
    ok(&repo, &home, &["branch", "other", "HEAD~1"]);
    let older = ok(&repo, &home, &["rev-parse", "--short", "HEAD~1"]).trim().to_string();

    // Checking out a branch by name: `skip_prefix` strips `refs/tags/` and
    // `refs/remotes/` only, so a branch keeps its full refname.
    ok(&repo, &home, &["checkout", "-q", "--detach", "other"]);
    let head = ok(&repo, &home, &["status"]);
    assert_eq!(
        head.lines().next(),
        Some("HEAD detached at refs/heads/other"),
        "full status was:\n{head}"
    );

    // A tag is reported by its short name, and matches through its peeled commit.
    ok(&repo, &home, &["checkout", "-q", "main"]);
    ok(&repo, &home, &["checkout", "-q", "v1"]);
    let head = ok(&repo, &home, &["status"]);
    assert_eq!(head.lines().next(), Some("HEAD detached at v1"), "full status was:\n{head}");

    // A raw object id dwims to nothing, so the abbreviated id is the name.
    ok(&repo, &home, &["checkout", "-q", "main"]);
    let older_full = ok(&repo, &home, &["rev-parse", "HEAD~1"]).trim().to_string();
    ok(&repo, &home, &["checkout", "-q", &older_full]);
    let head = ok(&repo, &home, &["status"]);
    assert_eq!(
        head.lines().next(),
        Some(format!("HEAD detached at {older}").as_str()),
        "full status was:\n{head}"
    );

    // Committing after the switch moves `HEAD` off `detached_oid`, which is the
    // whole of the `at` / `from` distinction.
    std::fs::write(repo.join("f3.txt"), "three\n").expect("write f3");
    ok(&repo, &home, &["add", "f3.txt"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c3"]);
    let head = ok(&repo, &home, &["status"]);
    assert_eq!(
        head.lines().next(),
        Some(format!("HEAD detached from {older}").as_str()),
        "full status was:\n{head}"
    );

    // A hand-written `HEAD` has no switch entry at all, which is git's NULL
    // `detached_from`.
    let tip = ok(&repo, &home, &["rev-parse", "HEAD"]).trim().to_string();
    std::fs::remove_file(repo.join(".git").join("logs").join("HEAD")).expect("drop HEAD reflog");
    detach_onto(&repo, &tip);
    let head = ok(&repo, &home, &["status"]);
    assert_eq!(head.lines().next(), Some("Not currently on any branch."), "full status was:\n{head}");
}

#[test]
fn show_untracked_files_config_is_boolean_coerced_and_validated() {
    let (repo, home) = fixture("untracked-cfg");
    std::fs::write(repo.join("u.txt"), "unt\n").expect("write u");

    // `git_parse_maybe_bool` runs first, so `false` is `no` — and the trailing
    // summary switches to the `-u` wording that only the `no` mode produces.
    let body = ok(&repo, &home, &["-c", "status.showUntrackedFiles=false", "status"]);
    assert!(
        body.ends_with("nothing to commit (use -u to show untracked files)\n"),
        "false should mean `no`, got:\n{body}"
    );
    assert!(!body.contains("u.txt"), "false should list nothing, got:\n{body}");

    // A non-zero integer is truthy, which `parse_untracked_setting_name` maps to
    // `normal` rather than to an error.
    let body = ok(&repo, &home, &["-c", "status.showUntrackedFiles=2", "status"]);
    assert!(body.contains("u.txt"), "2 should mean `normal`, got:\n{body}");

    // An unparseable value is fatal — for every view, before any output, and
    // even when `-u<mode>` on the command line would have overridden it.
    for args in [
        vec!["-c", "status.showUntrackedFiles=bogus", "status"],
        vec!["-c", "status.showUntrackedFiles=bogus", "status", "--porcelain"],
        vec!["-c", "status.showUntrackedFiles=bogus", "status", "--porcelain=v2"],
        vec!["-c", "status.showUntrackedFiles=bogus", "status", "-uall"],
        // `git commit` reads the same key through `status_init_config()`, so a
        // plain `-m` commit that renders no report dies too.
        vec!["-c", "status.showUntrackedFiles=bogus", "commit", "-m", "x"],
    ] {
        let out = run(&repo, &home, &args);
        assert_eq!(code(&out), 128, "exit code for {args:?}");
        assert_eq!(
            stderr(&out),
            "error: Invalid untracked files mode 'bogus'\n\
             fatal: unable to parse 'status.showuntrackedfiles' from command-line config\n",
            "stderr for {args:?}"
        );
        assert_eq!(stdout(&out), "", "stdout for {args:?}");
    }
}

#[test]
fn commit_editor_template_carries_the_author_and_the_whole_status_body() {
    let (repo, home) = fixture("editor-body");
    // Staged, unstaged and untracked, so all three sections have to appear.
    std::fs::write(repo.join("f1.txt"), "one-mod\n").expect("write f1");
    ok(&repo, &home, &["add", "f1.txt"]);
    std::fs::write(repo.join("f2.txt"), "two-mod\n").expect("write f2");
    std::fs::write(repo.join("untracked.txt"), "unt\n").expect("write untracked");
    let editor = failing_editor(&repo);

    let template = |args: &[&str]| -> String {
        let out = run_env(&repo, &home, args, &[("GIT_EDITOR", editor.as_str())]);
        assert_eq!(code(&out), 1, "git {args:?} stderr: {}", stderr(&out));
        std::fs::read_to_string(repo.join(".git").join("COMMIT_EDITMSG"))
            .expect("read COMMIT_EDITMSG")
    };

    // Measured from stock: the `Author:` line (the env author differs from the
    // env committer, so `ident_cmp` is non-zero), the `status_printf_ln(s, "%s",
    // "")` after it, and then the full `wt_status_print` body — commented,
    // uncolored, and with every `(use "git …")` hint suppressed by `s->hints = 0`.
    assert_eq!(
        template(&["commit"]),
        "\n\
         # Please enter the commit message for your changes. Lines starting\n\
         # with '#' will be ignored, and an empty message aborts the commit.\n\
         #\n\
         # Author:    A U Thor <author@example.com>\n\
         #\n\
         # On branch main\n\
         # Changes to be committed:\n\
         #\tmodified:   f1.txt\n\
         #\n\
         # Changes not staged for commit:\n\
         #\tmodified:   f2.txt\n\
         #\n\
         # Untracked files:\n\
         #\teditor-fail.sh\n\
         #\tuntracked.txt\n\
         #\n"
    );

    // `--amend` sets `author_message = "HEAD"`, which is the whole of
    // `author_date_is_interesting()` — so the inherited author date is named,
    // and the staged section is measured against `HEAD^1`, which is what turns
    // `f2.txt` into a `new file`.
    let amended = template(&["commit", "--amend"]);
    assert!(
        amended.contains("# Date:      Thu Apr 7 22:13:13 2005 +0000\n"),
        "amend should name the inherited author date, got:\n{amended}"
    );
    assert!(
        amended.contains("#\tnew file:   f2.txt\n"),
        "amend measures the staged side against HEAD^1, got:\n{amended}"
    );

    // `--cleanup=scissors` replaces the hint with the cut line and keeps the
    // identity block and the body underneath it.
    let scissors = template(&["commit", "--cleanup=scissors"]);
    assert!(
        scissors.starts_with(
            "\n\
             # ------------------------ >8 ------------------------\n\
             # Do not modify or remove the line above.\n\
             # Everything below it will be ignored.\n\
             #\n\
             # Author:    A U Thor <author@example.com>\n\
             #\n\
             # On branch main\n"
        ),
        "got:\n{scissors}"
    );

    // `--no-status` drops the whole block, body included.
    assert_eq!(template(&["commit", "--no-status"]), "");
}

#[test]
fn commit_editmsg_is_handed_to_the_editor_as_an_absolute_path() {
    let (repo, home) = fixture("editmsg-path");
    let deep = repo.join("deep").join("er");
    std::fs::create_dir_all(&deep).expect("create subdir");
    std::fs::write(deep.join("f3.txt"), "three\n").expect("write f3");
    ok(&repo, &home, &["add", "deep/er/f3.txt"]);

    // `git_path_commit_editmsg()` is absolute because `setup_git_directory()`
    // made `$GIT_DIR` absolute; an editor (or a hook) that chdirs relies on it.
    let record = repo.join("arg.txt");
    let script = repo.join("show-arg.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s' \"$1\" > \"$ZVCS_ARG_RECORD\"\nexit 1\n",
    )
    .expect("write editor");
    make_executable(&script);
    let editor = script.to_string_lossy().into_owned();
    let record_path = record.to_string_lossy().into_owned();

    for dir in [repo.clone(), deep.clone()] {
        let _ = std::fs::remove_file(&record);
        let out = run_env(
            &dir,
            &home,
            &["commit"],
            &[("GIT_EDITOR", editor.as_str()), ("ZVCS_ARG_RECORD", record_path.as_str())],
        );
        assert_eq!(code(&out), 1, "stderr was: {}", stderr(&out));
        let arg = std::fs::read_to_string(&record)
            .unwrap_or_else(|e| panic!("editor did not record its arg when run from {dir:?}: {e}"));
        assert!(
            Path::new(&arg).is_absolute(),
            "editor was handed {arg:?} when run from {dir:?}"
        );
        assert!(arg.ends_with("/COMMIT_EDITMSG"), "editor was handed {arg:?}");
        assert!(!arg.contains("/./"), "path should be normalized, got {arg:?}");
    }
}
