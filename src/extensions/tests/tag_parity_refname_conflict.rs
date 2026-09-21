//! What `git tag` says when the reference it would write cannot be created, and
//! what it leaves behind when it says it.
//!
//! A tag name that clashes with an existing reference — in either direction — is
//! rejected by `refs_verify_refname_available()` before the lock is taken, and the
//! two directions share one sentence:
//!
//! ```c
//! strbuf_addf(err, _("'%s' exists; cannot create '%s'"), dirname.buf, refname);
//! …
//! strbuf_addf(err, _("'%s' exists; cannot create '%s'"), iter->ref.name, refname);
//! ```
//!
//! (refs.c:2844 for a leading directory of the new name that is itself a
//! reference — `refs/tags/x` blocking `refs/tags/x/y`; refs.c:2897 for a reference
//! *under* the new name — `refs/tags/p/q` blocking `refs/tags/p`.) Both consult
//! packed refs as well as loose ones, and `--force` does not waive either: the
//! name is unavailable, not merely taken. The backend wraps the reason as `cannot
//! lock ref '<ref>': <reason>` and `cmd_tag()` dies with it verbatim.
//!
//! The failure is also where `TAG_EDITMSG`'s lifetime shows:
//!
//! ```c
//! if (!transaction ||
//!     ref_transaction_update(…) ||
//!     ref_transaction_commit(transaction, &err)) {
//!         if (path)
//!                 fprintf(stderr, _("The tag message has been left in %s\n"), path);
//!         die("%s", err.buf);
//! }
//! if (path) {
//!         unlink_or_warn(path);
//!         free(path);
//! }
//! ```
//!
//! (builtin/tag.c:690-707.) `path` is set for every annotated or signed tag, so
//! the extra line is printed whenever a tag *object* was built — and the file is
//! removed only after the reference is in place, which is what makes that line
//! true rather than a claim about a file already deleted. A lightweight tag builds
//! no object and prints no such line.
//!
//! Two neighbours of the same code path are pinned alongside: the editor template
//! a message-less `-a` writes, and what a bad `--column` style costs.
//!
//! Expectations are literal, so this file checks a binary with no stock git
//! present. No gpg, no network, and the only editor is a shell script.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A filesystem- and shell-safe stand-in for the thread id, so the scratch
/// repository path holds no parentheses — the editor scripts below are invoked
/// through `sh -c`, where an unquoted `(` is a syntax error.
fn thread_slug() -> String {
    format!("{:?}", std::thread::current().id())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn cmd(cwd: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(cwd)
        .env_remove("GIT_REFLOG_ACTION")
        .env_remove("EDITOR")
        .env_remove("VISUAL")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-07:00")
        .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-07:00")
        .env("TZ", "UTC")
        .env("LC_ALL", "C");
    c
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    cmd(cwd, args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn ok(cwd: &Path, args: &[&str]) -> String {
    let out = run(cwd, args);
    assert!(
        out.status.success(),
        "`git {args:?}` failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// A repository with one commit and one existing tag named `base`.
fn repo(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "zvcs-tagconflict-{tag}-{}-{}",
        std::process::id(),
        thread_slug()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    let p = p.canonicalize().unwrap();
    ok(&p, &["init", "-q", "--initial-branch=main", "."]);
    std::fs::write(p.join("a.t"), "a\n").unwrap();
    ok(&p, &["add", "a.t"]);
    ok(&p, &["commit", "-q", "-m", "one"]);
    ok(&p, &["tag", "base", "HEAD"]);
    p
}

/// A `GIT_EDITOR` script that dumps the buffer it was handed to `seen.txt` and
/// then replaces it, so the template can be read back exactly.
fn recording_editor(repo: &Path) -> String {
    let path = repo.join("ed.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\ncat \"$1\" > \"$(dirname \"$0\")/seen.txt\"\necho edited > \"$1\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.display().to_string()
}

// ---------------------------------------------------------------------------
// the two directions of the conflict
// ---------------------------------------------------------------------------

/// An existing tag is a leading directory of the new name.
#[test]
fn a_tag_under_an_existing_tag_names_both_references() {
    let r = repo("under");
    for name in ["base/sub", "base/a/b"] {
        let out = run(&r, &["tag", name, "HEAD"]);
        assert_eq!(
            stderr(&out),
            format!(
                "fatal: cannot lock ref 'refs/tags/{name}': \
                 'refs/tags/base' exists; cannot create 'refs/tags/{name}'\n"
            ),
            "{name}"
        );
        assert_eq!(code(&out), 128, "{name}");
    }
    // Nothing was created.
    assert_eq!(ok(&r, &["tag", "-l"]), "base");
}

/// An existing tag lives *under* the new name. `--force` does not help: the name
/// is unavailable, not merely taken.
#[test]
fn a_tag_over_an_existing_directory_names_the_reference_below_it() {
    let r = repo("over");
    ok(&r, &["tag", "p/q", "HEAD"]);
    for extra in [vec!["tag", "p", "HEAD"], vec!["tag", "-f", "p", "HEAD"]] {
        let out = run(&r, &extra);
        assert_eq!(
            stderr(&out),
            "fatal: cannot lock ref 'refs/tags/p': \
             'refs/tags/p/q' exists; cannot create 'refs/tags/p'\n",
            "{extra:?}"
        );
        assert_eq!(code(&out), 128, "{extra:?}");
    }
}

/// The check reads packed refs too, so packing the blocking tag away does not
/// turn the conflict into a filesystem errno.
#[test]
fn the_conflict_is_found_in_packed_refs() {
    let r = repo("packed");
    ok(&r, &["pack-refs", "--all"]);
    assert!(
        !r.join(".git/refs/tags/base").exists(),
        "pack-refs left the loose tag in place, so this test would not exercise packed refs"
    );
    let out = run(&r, &["tag", "base/sub", "HEAD"]);
    assert_eq!(
        stderr(&out),
        "fatal: cannot lock ref 'refs/tags/base/sub': \
         'refs/tags/base' exists; cannot create 'refs/tags/base/sub'\n"
    );
    assert_eq!(code(&out), 128);
}

/// A tag whose name merely resembles a branch is no conflict at all: the
/// namespaces are separate.
#[test]
fn a_name_that_only_clashes_across_namespaces_is_accepted() {
    let r = repo("ns");
    ok(&r, &["branch", "topic", "HEAD"]);
    let out = run(&r, &["tag", "topic/sub", "HEAD"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(ok(&r, &["tag", "-l"]).lines().any(|l| l == "topic/sub"));
}

// ---------------------------------------------------------------------------
// TAG_EDITMSG's lifetime
// ---------------------------------------------------------------------------

/// An annotated tag that cannot be written says where its message is, before the
/// `fatal:` — and the file is really there.
#[test]
fn a_failed_annotated_tag_leaves_its_message_and_says_so() {
    let r = repo("left");
    let ed = recording_editor(&r);
    let out = cmd(&r, &["tag", "-a", "base/sub", "HEAD"])
        .env("GIT_EDITOR", &ed)
        .output()
        .unwrap();
    assert_eq!(
        stderr(&out),
        "The tag message has been left in .git/TAG_EDITMSG\n\
         fatal: cannot lock ref 'refs/tags/base/sub': \
         'refs/tags/base' exists; cannot create 'refs/tags/base/sub'\n"
    );
    assert_eq!(code(&out), 128);
    assert_eq!(
        std::fs::read_to_string(r.join(".git/TAG_EDITMSG")).unwrap(),
        "edited\n",
        "the file the message names has to exist, with what the editor wrote"
    );
}

/// The same failure with `-m`: git sets `path` for every annotated tag, so the
/// line is printed even though no editor ever ran.
#[test]
fn a_failed_annotated_tag_says_so_even_without_an_editor() {
    let r = repo("leftm");
    let out = run(&r, &["tag", "-a", "-m", "msg", "base/sub", "HEAD"]);
    assert_eq!(
        stderr(&out),
        "The tag message has been left in .git/TAG_EDITMSG\n\
         fatal: cannot lock ref 'refs/tags/base/sub': \
         'refs/tags/base' exists; cannot create 'refs/tags/base/sub'\n"
    );
    assert_eq!(code(&out), 128);
}

/// A lightweight tag builds no tag object, so there is no message to leave and no
/// line about one.
#[test]
fn a_failed_lightweight_tag_says_nothing_about_a_message() {
    let r = repo("leftlw");
    let out = run(&r, &["tag", "base/sub", "HEAD"]);
    assert!(
        !stderr(&out).contains("tag message"),
        "lightweight tag mentioned TAG_EDITMSG: {}",
        stderr(&out)
    );
    assert_eq!(code(&out), 128);
}

/// A tag that *is* written takes its editor buffer away again.
#[test]
fn a_successful_annotated_tag_removes_tag_editmsg() {
    let r = repo("unlink");
    let ed = recording_editor(&r);
    let out = cmd(&r, &["tag", "-a", "fresh", "HEAD"])
        .env("GIT_EDITOR", &ed)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(
        !r.join(".git/TAG_EDITMSG").exists(),
        "TAG_EDITMSG survived a successful tag"
    );
}

// ---------------------------------------------------------------------------
// the template an editor is handed
// ---------------------------------------------------------------------------

/// `tag_template` begins with its own `\n` *inside* the commented region, so the
/// blank line `strbuf_addch()` writes is followed by a bare comment character on a
/// line of its own (builtin/tag.c:201-203, :340-343).
#[test]
fn the_tag_template_starts_with_a_blank_line_then_a_bare_comment_line() {
    let r = repo("tmpl");
    let ed = recording_editor(&r);
    let out = cmd(&r, &["tag", "-a", "fresh", "HEAD"])
        .env("GIT_EDITOR", &ed)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(r.join("seen.txt")).unwrap(),
        "\n#\n# Write a message for tag:\n#   fresh\n# Lines starting with '#' will be ignored.\n"
    );
}

// ---------------------------------------------------------------------------
// a bad --column style
// ---------------------------------------------------------------------------

/// `column.c:282`'s `error()` reaches `parse_options()` as `PARSE_OPT_ERROR`,
/// which is a bare `exit(129)` — one `error:` line, no usage block
/// (parse-options.c:975-977).
#[test]
fn a_bad_column_style_is_one_error_line_and_exit_129() {
    let r = repo("col");
    // The offending token is the comma-separated style word, not the whole option.
    for (arg, token) in [
        ("--column=always,width=40", "width=40"),
        ("--column=bogus", "bogus"),
        ("--column=always,dense,nope", "nope"),
    ] {
        let out = run(&r, &["tag", arg]);
        assert_eq!(
            stderr(&out),
            format!("error: unsupported option '{token}'\n"),
            "{arg}"
        );
        assert_eq!(code(&out), 129, "{arg}");
        assert!(out.stdout.is_empty(), "{arg} printed a listing");
    }
}
