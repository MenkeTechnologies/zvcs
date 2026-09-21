//! Two things `show-branch` reads indirectly: whether to colour the `*!+-`
//! columns, and which object a `<ref>@{n}` row stands for.
//!
//! **Colour.** `get_color_code()` asks `want_color(showbranch_use_color)`, and
//! `showbranch_use_color` is whatever `git_config_colorbool()` made of
//! `color.showbranch` — or, unset, of `color.ui` (color.c:383-403):
//!
//! ```c
//! if (value) {
//!         if (!strcasecmp(value, "never")) return GIT_COLOR_NEVER;
//!         if (!strcasecmp(value, "always")) return GIT_COLOR_ALWAYS;
//!         if (!strcasecmp(value, "auto")) return GIT_COLOR_AUTO;
//! }
//! if (!var) return GIT_COLOR_UNKNOWN;
//! /* Missing or explicit false to turn off colorization */
//! if (!git_config_bool(var, value)) return GIT_COLOR_NEVER;
//! /* any normal truth value defaults to 'auto' */
//! return GIT_COLOR_AUTO;
//! ```
//!
//! The last two lines are the whole test. A boolean-*true* value is
//! `GIT_COLOR_AUTO`, **not** `GIT_COLOR_ALWAYS` — so `color.ui = true`, which is
//! what `git config --global color.ui true` writes and what a great many
//! checkouts therefore carry, still has to be gated on stdout being a terminal.
//! Treating it as "always" puts ANSI escapes into every pipe and every captured
//! file. `never` is a *word*, not a boolean, so a reader that only knows
//! `true`/`false` spellings rejects a perfectly ordinary setting outright.
//!
//! The command's own arm runs at config time, ahead of `parse_options()`
//! (builtin/show-branch.c:588-591, :718), so a `color.showbranch` that
//! `git_config_bool()` cannot read is fatal even when the command line would
//! have been refused first, and even when `--no-color` would have made the value
//! irrelevant.
//!
//! **Reflog rows.** `cmd_show_branch()` resolves every name in `ref_name[]`
//! afresh (builtin/show-branch.c:879):
//!
//! ```c
//! if (repo_get_oid(the_repository, ref_name[num_rev], &revkey))
//!         die(_("'%s' is not a valid ref."), ref_name[num_rev]);
//! ```
//!
//! The id `read_ref_at()` produced went to `append_ref()`, which uses it only to
//! decide whether the entry peels to a commit, and then drops it. For most refs
//! the two agree and the distinction is invisible. For `HEAD` they do not:
//! `repo_dwim_ref("HEAD")` answers with the name it *resolved to*, so the log
//! that is read is the branch's, while the name recorded is `HEAD@{n}` — and
//! `HEAD@{n}` re-resolves against HEAD's own log, which carries the `checkout:`
//! entries the branch log has never seen. The captions and the commits therefore
//! come from two different logs, and reusing the read id is the one thing that
//! makes them agree.
//!
//! Every expectation here was captured from stock git 2.55.0 in an identical
//! fixture. stdout is a pipe throughout, which is what makes the `auto` cases
//! decide anything at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run `git <args>` in `repo`, returning stdout, stderr and the exit code.
///
/// `TERM` is a real terminal name on purpose: `check_auto_color()` refuses a
/// `dumb` terminal *and* a non-tty, so leaving `TERM` out would make the `auto`
/// cases pass for the wrong reason.
fn git(repo: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .env("ZVCS_HOME", home)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 +0000")
        .env("GIT_COMMITTER_DATE", "1112911993 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("TERM", "xterm-256color")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// `git <args>`, failing loudly on a non-zero exit — for fixture construction,
/// where a partial success would silently weaken the premise.
fn must(repo: &Path, home: &Path, args: &[&str]) -> String {
    let (stdout, stderr, code) = git(repo, home, args);
    assert_eq!(code, 0, "git {args:?} failed: {stderr}");
    stdout.trim_end().to_string()
}

/// `A` on `main`, `B` on `main`, `C` on `side`, then back to `main`.
///
/// HEAD's log is then `checkout`, `commit: C`, `checkout`, `commit: B`,
/// `commit (initial): A` while `main`'s is `commit: B`, `commit (initial): A`.
/// The two disagree at every index, which is what the reflog test needs.
fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-sb-color-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();

    must(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("A"), "A\n").unwrap();
    must(&repo, &home, &["add", "A"]);
    must(&repo, &home, &["commit", "-qm", "A"]);
    must(&repo, &home, &["branch", "side"]);
    std::fs::write(repo.join("B"), "B\n").unwrap();
    must(&repo, &home, &["add", "B"]);
    must(&repo, &home, &["commit", "-qm", "B"]);
    must(&repo, &home, &["checkout", "-q", "side"]);
    std::fs::write(repo.join("C"), "C\n").unwrap();
    must(&repo, &home, &["add", "C"]);
    must(&repo, &home, &["commit", "-qm", "C"]);
    must(&repo, &home, &["checkout", "-q", "main"]);

    (root, repo, home)
}

/// A boolean-true `color.ui` (or `color.showbranch`) is `auto`, so a piped
/// stdout gets no escapes. Only the three words and the command line can turn
/// colour on unconditionally.
#[test]
fn a_true_color_setting_is_auto_and_not_always() {
    let (root, repo, home) = fixture("auto");
    let esc = '\x1b';

    for value in ["true", "yes", "on", "1", "auto"] {
        for key in ["color.ui", "color.showbranch"] {
            let (out, err, code) = git(
                &repo,
                &home,
                &["-c", &format!("{key}={value}"), "show-branch", "main", "side"],
            );
            assert_eq!(err, "", "{key}={value}");
            assert_eq!(code, 0, "{key}={value}");
            assert!(!out.contains(esc), "{key}={value} coloured a pipe: {out:?}");
        }
    }

    // The control: the settings that *do* colour a pipe still colour it, so a
    // port that simply stopped emitting escapes fails here.
    for args in [
        vec!["-c", "color.ui=always", "show-branch", "main", "side"],
        vec!["-c", "color.showbranch=always", "show-branch", "main", "side"],
        vec!["show-branch", "--color", "main", "side"],
        vec!["show-branch", "--color=always", "main", "side"],
    ] {
        let (out, _, code) = git(&repo, &home, &args);
        assert_eq!(code, 0, "{args:?}");
        assert!(out.contains(esc), "{args:?} lost its colour: {out:?}");
    }

    // `color.showbranch` wins over `color.ui`, in both directions.
    let (out, _, _) = git(
        &repo,
        &home,
        &["-c", "color.ui=always", "-c", "color.showbranch=never", "show-branch", "main"],
    );
    assert!(!out.contains(esc), "{out:?}");
    let (out, _, _) = git(
        &repo,
        &home,
        &["-c", "color.ui=never", "-c", "color.showbranch=always", "show-branch", "main", "side"],
    );
    assert!(out.contains(esc), "{out:?}");

    let _ = std::fs::remove_dir_all(&root);
}

/// `never` and `false` are ordinary values, not errors — and the only fatal is
/// the one `git_config_bool()` raises, which happens at config time and so
/// outranks both a command-line refusal and a `--no-color` that would have made
/// the value moot.
#[test]
fn color_words_are_accepted_and_only_a_bad_boolean_is_fatal() {
    let (root, repo, home) = fixture("words");
    let esc = '\x1b';

    for value in ["never", "NEVER", "false", "no", "off", "0"] {
        for key in ["color.ui", "color.showbranch"] {
            let (out, err, code) = git(
                &repo,
                &home,
                &["-c", &format!("{key}={value}"), "show-branch", "main", "side"],
            );
            assert_eq!(err, "", "{key}={value}");
            assert_eq!(code, 0, "{key}={value}");
            assert!(!out.is_empty(), "{key}={value}");
            assert!(!out.contains(esc), "{key}={value}");
        }
    }

    let fatal = "fatal: bad boolean config value 'bogus' for 'color.showbranch'\n";
    for args in [
        vec!["-c", "color.showbranch=bogus", "show-branch", "main"],
        // `git_show_branch_config()` runs before `parse_options()`, so the bad
        // value is reported instead of the unknown option.
        vec!["-c", "color.showbranch=bogus", "show-branch", "--nosuchopt"],
        // ... and instead of nothing at all, though `--no-color` means the value
        // is never consulted.
        vec!["-c", "color.showbranch=bogus", "show-branch", "--no-color", "main"],
    ] {
        let (out, err, code) = git(&repo, &home, &args);
        assert_eq!(err, fatal, "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert_eq!(code, 128, "{args:?}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// `--reflog HEAD` reads the *branch's* log for the captions and resolves
/// `HEAD@{n}` against *HEAD's* log for the commits, so the two halves of a row
/// need not describe the same object.
///
/// Here `HEAD@{1}` captions `commit (initial): A` (`main@{1}`) while the commit
/// it stands for is `C` (`HEAD@{1}`), which is not even on `main`. Reusing the
/// id `read_ref_at()` returned shows `A` there and loses `C` entirely.
#[test]
fn reflog_pseudo_refs_are_resolved_by_name_not_by_the_log_that_was_read() {
    let (root, repo, home) = fixture("head-reflog");

    // The premise: the two logs really do disagree at index 1.
    assert_eq!(
        must(&repo, &home, &["rev-parse", "HEAD@{1}"]),
        must(&repo, &home, &["rev-parse", "side"]),
    );
    assert_eq!(
        must(&repo, &home, &["rev-parse", "main@{1}"]),
        must(&repo, &home, &["rev-parse", "main~1"]),
    );

    let (out, err, code) = git(&repo, &home, &["show-branch", "--no-color", "--reflog=2", "HEAD"]);
    assert_eq!(err, "");
    assert_eq!(code, 0);

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 6, "{out:?}");
    // The two header rows caption from `refs/heads/main`'s log. The relative
    // date between `(` and `)` drifts with the wall clock, so only the message
    // is pinned.
    assert!(lines[0].starts_with("! [HEAD@{0}] ("), "{:?}", lines[0]);
    assert!(lines[0].ends_with(") commit: B"), "{:?}", lines[0]);
    assert!(lines[1].starts_with(" ! [HEAD@{1}] ("), "{:?}", lines[1]);
    assert!(lines[1].ends_with(") commit (initial): A"), "{:?}", lines[1]);
    assert_eq!(lines[2], "--");
    // ... while the body rows are HEAD's log: `C` is what `HEAD@{1}` names.
    assert_eq!(lines[3], " + [HEAD@{1}] C");
    assert_eq!(lines[4], "+  [HEAD@{0}] B");
    assert_eq!(lines[5], "++ [HEAD@{1}^] A");

    let _ = std::fs::remove_dir_all(&root);
}
