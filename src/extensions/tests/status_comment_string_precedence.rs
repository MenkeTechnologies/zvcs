//! `core.commentChar` and `core.commentString` are one variable, and
//! `status.displayCommentPrefix` has to read it the way `git_default_core_config()`
//! (environment.c:435-456) wrote it:
//!
//! ```c
//! if (!strcmp(var, "core.commentchar") || !strcmp(var, "core.commentstring")) {
//!         if (!value)                       return config_error_nonbool(var);
//!         else if (!strcasecmp(value, "auto")) { auto_comment_line_char = 1;
//!                                                comment_line_str = "#"; }
//!         else if (value[0])                { comment_line_str = value; … }
//!         else                              return error(…"at least one character");
//! }
//! ```
//!
//! Two consequences a per-key resolver gets wrong: the *last* assignment wins no
//! matter which of the two spellings carried it, and `auto` is resolved to `#` right
//! there rather than kept as a literal. `adjust_comment_line_char()` is the only thing
//! that ever revises the value afterwards, and it runs from `builtin/commit.c`'s
//! `prepare_to_commit()` — never for `status` — so `auto` is `#` in every status view.
//!
//! The value is also stored whole: `core.commentString = //` prefixes with `//`, not
//! with `/`.
//!
//! Only the long human format is prefixed at all: git routes it through
//! `status_printf`, while `--short`, `--porcelain` and `--porcelain=v2` never are.
//! Every expectation below was measured from stock git 2.55.0 with the global and
//! system config pinned away.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn real_git_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.contains(".zvcs"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Run `git` with every config source outside the repository pinned away, so a
/// developer's own `core.commentChar` cannot decide the result.
fn git(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("PATH", real_git_path())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

#[test]
fn comment_string_and_comment_char_are_one_last_wins_variable() {
    let root = std::env::temp_dir().join(format!("zvcs-commentstr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&repo).expect("create repo");
    let root = root.canonicalize().expect("canonicalize");
    let (home, repo) = (root.join("home"), root.join("repo"));

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a"), b"a\n").expect("write a");
    git(&repo, &home, &["add", "a"]);
    git(&repo, &home, &["commit", "-q", "-m", "base"]);
    std::fs::write(repo.join("a"), b"a\nmore\n").expect("modify a");

    /// The prefix every long-format line carries, taken from the branch header.
    fn prefix(line: &str) -> &str {
        line.split(" On branch").next().expect("branch header line")
    }

    let long = |flags: &[&str]| -> String {
        let mut args = vec!["-c", "status.displayCommentPrefix=true"];
        args.extend_from_slice(flags);
        args.push("status");
        let out = git(&repo, &home, &args);
        let first = out.lines().next().expect("status has a first line").to_owned();
        assert!(
            first.ends_with(" On branch main"),
            "expected a branch header, got {first:?}"
        );
        prefix(&first).to_owned()
    };

    // Neither key set: the built-in default.
    assert_eq!(long(&[]), "#");

    // `auto` resolves at config time, through either spelling — the literal string
    // must never reach the output.
    assert_eq!(long(&["-c", "core.commentString=auto"]), "#");
    assert_eq!(long(&["-c", "core.commentChar=auto"]), "#");

    // A single non-default character through either spelling.
    assert_eq!(long(&["-c", "core.commentChar=@"]), "@");
    assert_eq!(long(&["-c", "core.commentString=;"]), ";");

    // A multi-character string is kept whole, not truncated to its first byte.
    assert_eq!(long(&["-c", "core.commentString=//"]), "//");

    // Both keys set: the last assignment wins regardless of which spelling it used.
    assert_eq!(
        long(&["-c", "core.commentChar=@", "-c", "core.commentString=|"]),
        "|"
    );
    assert_eq!(
        long(&["-c", "core.commentString=|", "-c", "core.commentChar=@"]),
        "@",
        "commentChar set last wins over an earlier commentString"
    );
    assert_eq!(
        long(&["-c", "core.commentString=//", "-c", "core.commentChar=@"]),
        "@"
    );

    // `auto` as the *last* assignment resets to `#` even over a real earlier value…
    assert_eq!(
        long(&["-c", "core.commentString=|", "-c", "core.commentChar=auto"]),
        "#"
    );
    // …and an earlier `auto` loses to a real later value.
    assert_eq!(
        long(&["-c", "core.commentString=auto", "-c", "core.commentChar=@"]),
        "@"
    );

    // `--long` is the same renderer; the machine formats are never prefixed.
    let both = ["-c", "status.displayCommentPrefix=true", "-c", "core.commentString=//"];
    let mut args = both.to_vec();
    args.extend_from_slice(&["status", "--long"]);
    assert!(
        git(&repo, &home, &args).starts_with("// On branch main\n"),
        "--long is the prefixed renderer"
    );
    for view in [["status", "--short"], ["status", "--porcelain"]] {
        let mut args = both.to_vec();
        args.extend_from_slice(&view);
        assert_eq!(
            git(&repo, &home, &args),
            " M a\n",
            "{view:?} is never comment-prefixed"
        );
    }
    let mut args = both.to_vec();
    args.extend_from_slice(&["status", "--porcelain=v2"]);
    assert!(
        git(&repo, &home, &args).starts_with("1 .M N... 100644 100644 100644 "),
        "porcelain=v2 is never comment-prefixed"
    );
}
