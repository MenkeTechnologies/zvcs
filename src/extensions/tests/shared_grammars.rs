//! The three grammars git keeps in exactly one place, checked at the verbs that
//! share them.
//!
//! Each of these was a *duplicated* implementation in this port, and every copy
//! had drifted from the one the verb actually needed:
//!
//!   * `versioncmp.c`'s prerelease-suffix rule reached `for-each-ref` only, so
//!     `tag`, `branch` and `ls-remote` ordered `versionsort.suffix` refs
//!     differently from `for-each-ref` on the same repository;
//!   * `trailer.c`'s trailer-block scan reached `commit -s` with `#` hardcoded,
//!     so a repository with `core.commentChar` set had its sign-off placed by a
//!     different comment character than the rest of the same message;
//!   * `config.c`'s `git_parse_maybe_bool()` had a decimal-only integer
//!     fallback at four call sites, so `0x10` and `010` read as something git
//!     never reads them as.
//!
//! Every expectation below is what stock git 2.55.0 printed for that exact
//! command, measured before the consolidation. The values are inlined rather
//! than diffed against a live stock binary so this file is meaningful in a
//! headless CI container with no other git installed.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run the port with a pinned, empty environment — the same hermetic set the
/// parity harness uses, so no `~/.gitconfig` or ambient identity can reach the
/// command under test.
fn run(dir: &Path, args: &[&str]) -> (String, String, i32) {
    // `$HOME` and the port's own state directory sit *beside* the repository, not
    // inside it: a `.zvcs/` under the work tree would be staged by the `add -A`
    // some of these cases run.
    let home = home_of(dir);
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("ZVCS_HOME", home.join(".zvcs"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "zvcs parity")
        .env("GIT_AUTHOR_EMAIL", "parity@example.invalid")
        .env("GIT_COMMITTER_NAME", "zvcs parity")
        .env("GIT_COMMITTER_EMAIL", "parity@example.invalid")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_EDITOR", "true")
        .env("EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
    let out = cmd.output().expect("spawning the port");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, err, code) = run(dir, args);
    assert_eq!(code, 0, "git {args:?} failed: {err}");
    out
}

/// The `$HOME` that goes with a repository directory.
fn home_of(dir: &Path) -> PathBuf {
    let mut home = dir.as_os_str().to_owned();
    home.push("-home");
    PathBuf::from(home)
}

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-shared-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(home_of(&root));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(home_of(&root)).unwrap();
    root.canonicalize().unwrap()
}

/// One commit, then a spread of version-shaped tags *and* branches of the same
/// names, so the same ordering question can be put to all four verbs.
fn version_repo(name: &str) -> PathBuf {
    let root = scratch(name);
    ok(&root, &["init", "-q", "-b", "main", "."]);
    std::fs::write(root.join("f"), b"seed\n").unwrap();
    ok(&root, &["add", "f"]);
    ok(&root, &["commit", "-q", "-m", "seed"]);
    for tag in [
        "v1.0",
        "v1.0-rc1",
        "v1.0-rc2",
        "v1.0-beta1",
        "v0.9",
        "v1.1",
        "v1.10",
        "v1.2",
    ] {
        ok(&root, &["tag", tag]);
        ok(&root, &["branch", tag]);
    }
    root
}

/// `versionsort.suffix` moves `-rc` releases *ahead* of the release they
/// precede. Stock git applies that to every verb that version-sorts, because all
/// four call one `versioncmp()`; three of this port's four copies never read the
/// configuration at all.
#[test]
fn versionsort_suffix_reaches_tag_branch_and_ls_remote() {
    let root = version_repo("vsort");
    ok(&root, &["config", "--local", "versionsort.suffix", "-rc"]);

    // Measured from stock git 2.55.0 over this tag set.
    let want = "\
v0.9
v1.0-rc1
v1.0-rc2
v1.0
v1.0-beta1
v1.1
v1.2
v1.10
";
    assert_eq!(
        ok(&root, &["tag", "--sort=version:refname"]),
        want,
        "tag --sort=version:refname ignored versionsort.suffix"
    );
    assert_eq!(
        ok(
            &root,
            &[
                "for-each-ref",
                "--sort=version:refname",
                "--format=%(refname:strip=2)",
                "refs/tags",
            ]
        ),
        want,
        "for-each-ref --sort=version:refname ignored versionsort.suffix"
    );

    // `%(refname)` rather than a short name: every branch here shares its name
    // with a tag, so the short form would be the disambiguated `heads/<name>`.
    let branches = ok(
        &root,
        &["branch", "--list", "--format=%(refname)", "--sort=version:refname"],
    );
    let want_branches: String = std::iter::once("main")
        .chain(want.lines())
        .map(|n| format!("refs/heads/{n}\n"))
        .collect();
    assert_eq!(
        branches, want_branches,
        "branch --sort=version:refname ignored versionsort.suffix"
    );

    let remote = ok(&root, &["ls-remote", "--tags", "--sort=v:refname", "."]);
    let names: Vec<&str> = remote
        .lines()
        .filter_map(|l| l.split('\t').nth(1))
        .map(|r| r.trim_start_matches("refs/tags/"))
        .collect();
    assert_eq!(
        names.join("\n") + "\n",
        want,
        "ls-remote --sort=v:refname ignored versionsort.suffix"
    );

    let _ = std::fs::remove_dir_all(home_of(&root));
    let _ = std::fs::remove_dir_all(&root);
}

/// git reads the `versionsort` keys *inside* `versioncmp()`, after the two
/// strings have already been walked to their first difference — so a sort with
/// nothing to compare never reads them and never warns. Stock prints no warning
/// for the single-ref run and exactly one for the many-ref run.
#[test]
fn the_prereleasesuffix_warning_is_emitted_lazily_and_once() {
    let root = version_repo("vsort-warn");
    ok(&root, &["config", "--local", "versionsort.suffix", "-rc"]);
    ok(
        &root,
        &["config", "--local", "versionsort.prereleasesuffix", "-beta"],
    );

    let (_, err, code) = run(
        &root,
        &[
            "for-each-ref",
            "--sort=version:refname",
            "--format=%(refname)",
            "refs/tags/v0.9",
        ],
    );
    assert_eq!(code, 0);
    assert_eq!(err, "", "a single ref is never compared, so nothing warns");

    let (_, err, code) = run(&root, &["tag", "--sort=version:refname"]);
    assert_eq!(code, 0);
    assert_eq!(
        err.matches("ignoring versionsort.prereleasesuffix").count(),
        1,
        "git's `initialized` static warns once per process, got:\n{err}"
    );

    let _ = std::fs::remove_dir_all(home_of(&root));
    let _ = std::fs::remove_dir_all(&root);
}

/// `commit -s` locates the trailer block with `core.commentChar`, so a `;`
/// comment line below the block is *not* part of it and the sign-off is merged
/// into the block rather than pushed past a blank line.
#[test]
fn signoff_finds_the_trailer_block_through_the_configured_comment_char() {
    let root = scratch("signoff");
    ok(&root, &["init", "-q", "-b", "main", "."]);
    std::fs::write(root.join("f"), b"seed\n").unwrap();
    ok(&root, &["add", "f"]);
    ok(&root, &["commit", "-q", "-m", "seed"]);
    ok(&root, &["config", "--local", "core.commentChar", ";"]);
    std::fs::write(
        root.join("MSG"),
        b"subject line\n\nbody text\n\nAcked-by: A U Thor <author@example.com>\n; trailing semicolon line\n",
    )
    .unwrap();
    std::fs::write(root.join("f"), b"seed\nchange\n").unwrap();
    ok(&root, &["add", "f"]);
    ok(&root, &["commit", "-s", "--cleanup=strip", "-F", "MSG"]);

    // Stock git 2.55.0's message for this exact input, byte for byte: the
    // sign-off joins the trailer block, and the `;` line is a comment.
    assert_eq!(
        ok(&root, &["log", "-1", "--format=%B"]),
        "subject line\n\nbody text\n\nAcked-by: A U Thor <author@example.com>\n\
         Signed-off-by: zvcs parity <parity@example.invalid>\n\n",
    );

    let _ = std::fs::remove_dir_all(home_of(&root));
    let _ = std::fs::remove_dir_all(&root);
}

/// A configured `trailer.<token>.key` is what makes a line a *recognised*
/// trailer, and that is what the block scan's 25% rule keys off — so the same
/// message ends with the sign-off inside the block only when the key is set.
#[test]
fn a_configured_trailer_key_changes_where_the_signoff_lands() {
    let quarter = b"subject line\n\nbody text\n\nAcked-by: A U Thor <author@example.com>\n\
                    plain line one\nplain line two\nplain line three\n";

    let build = |name: &str, key: bool| -> String {
        let root = scratch(name);
        ok(&root, &["init", "-q", "-b", "main", "."]);
        std::fs::write(root.join("f"), b"seed\n").unwrap();
        ok(&root, &["add", "f"]);
        ok(&root, &["commit", "-q", "-m", "seed"]);
        if key {
            ok(&root, &["config", "--local", "trailer.ack.key", "Acked-by:"]);
        }
        std::fs::write(root.join("MSG"), quarter).unwrap();
        std::fs::write(root.join("f"), b"seed\nchange\n").unwrap();
        ok(&root, &["add", "f"]);
        ok(&root, &["commit", "-s", "--cleanup=strip", "-F", "MSG"]);
        let msg = ok(&root, &["log", "-1", "--format=%B"]);
        let _ = std::fs::remove_dir_all(home_of(&root));
        let _ = std::fs::remove_dir_all(&root);
        msg
    };

    // With the key configured the four lines are a trailer block (one
    // recognised trailer, `trailer_lines * 3 >= non_trailer_lines`), so the
    // sign-off is appended straight onto it. Both are stock git 2.55.0's output.
    assert_eq!(
        build("trailer-key", true),
        "subject line\n\nbody text\n\nAcked-by: A U Thor <author@example.com>\n\
         plain line one\nplain line two\nplain line three\n\
         Signed-off-by: zvcs parity <parity@example.invalid>\n\n",
    );
    assert_eq!(
        build("trailer-nokey", false),
        "subject line\n\nbody text\n\nAcked-by: A U Thor <author@example.com>\n\
         plain line one\nplain line two\nplain line three\n\n\
         Signed-off-by: zvcs parity <parity@example.invalid>\n\n",
    );
}

/// `git config`'s number reader is C `strtoimax` with **base 0** plus a
/// `k`/`m`/`g` unit, and its boolean reader falls back to that same reader
/// bounded by a C `int`. Every line here is stock git 2.55.0's answer.
#[test]
fn config_reads_numbers_and_booleans_with_gits_base_zero_grammar() {
    let root = scratch("numbers");
    ok(&root, &["init", "-q", "-b", "main", "."]);

    let get = |value: &str, ty: &str| -> (String, String, i32) {
        run(
            &root,
            &["-c", &format!("test.v={value}"), "config", &format!("--type={ty}"), "--get", "test.v"],
        )
    };

    for (value, int, bool_, bool_or_int) in [
        ("0x10", "16\n", "true\n", "16\n"),
        ("0X10", "16\n", "true\n", "16\n"),
        ("010", "8\n", "true\n", "8\n"),
        ("0x0", "0\n", "false\n", "0\n"),
        ("1k", "1024\n", "true\n", "1024\n"),
        ("-1k", "-1024\n", "true\n", "-1024\n"),
        ("  12", "12\n", "true\n", "12\n"),
        ("+1", "1\n", "true\n", "1\n"),
    ] {
        assert_eq!(get(value, "int").0, int, "--type=int over {value}");
        assert_eq!(get(value, "bool").0, bool_, "--type=bool over {value}");
        assert_eq!(
            get(value, "bool-or-int").0,
            bool_or_int,
            "--type=bool-or-int over {value}"
        );
    }

    // `08` is an octal zero followed by an unreadable `8`, not eight; `12 ` has
    // a trailing blank, which is a unit git cannot read either.
    for bad in ["08", "12 "] {
        let (_, err, code) = get(bad, "int");
        assert_eq!(code, 128, "--type=int accepted {bad:?}");
        assert!(err.contains("invalid unit"), "{bad:?}: {err}");
        let (_, err, code) = get(bad, "bool");
        assert_eq!(code, 128, "--type=bool accepted {bad:?}");
        assert!(err.contains("bad boolean config value"), "{bad:?}: {err}");
    }

    // The boolean fallback is bounded by a C `int` while `--type=int` is an
    // `int64_t`, so one value is a number but not a boolean.
    assert_eq!(get("2147483648", "int").0, "2147483648\n");
    assert_eq!(get("2147483648", "bool").2, 128);
    assert_eq!(get("-2147483648", "bool").0, "true\n");

    let _ = std::fs::remove_dir_all(home_of(&root));
    let _ = std::fs::remove_dir_all(&root);
}

/// `diff.renames` is read with `git_config_bool()`, so it takes the same base-0
/// grammar — and dies on a value outside it instead of silently disabling
/// rename detection.
#[test]
fn diff_renames_uses_the_shared_boolean_grammar() {
    let root = scratch("renames");
    ok(&root, &["init", "-q", "-b", "main", "."]);
    std::fs::write(root.join("a"), b"one\n").unwrap();
    ok(&root, &["add", "a"]);
    ok(&root, &["commit", "-q", "-m", "one"]);
    std::fs::rename(root.join("a"), root.join("b")).unwrap();
    ok(&root, &["add", "-A"]);
    ok(&root, &["commit", "-q", "-m", "two"]);

    // `0x1` is one, so rename detection is on and the pair is reported as a
    // rename rather than a delete plus a create.
    let out = ok(
        &root,
        &["-c", "diff.renames=0x1", "diff", "--summary", "HEAD~1", "HEAD"],
    );
    assert!(out.contains("rename a => b"), "expected a rename, got:\n{out}");

    let out = ok(
        &root,
        &["-c", "diff.renames=0x0", "diff", "--summary", "HEAD~1", "HEAD"],
    );
    assert!(
        out.contains("delete mode") && out.contains("create mode"),
        "expected rename detection off, got:\n{out}"
    );

    let (_, err, code) = run(
        &root,
        &["-c", "diff.renames=abc", "diff", "--summary", "HEAD~1", "HEAD"],
    );
    assert_eq!(code, 128, "a bad boolean must die: {err}");
    assert_eq!(
        err.trim_end(),
        "fatal: bad boolean config value 'abc' for 'diff.renames'"
    );

    let _ = std::fs::remove_dir_all(home_of(&root));
    let _ = std::fs::remove_dir_all(&root);
}
