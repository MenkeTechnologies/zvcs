//! `get_oid_basic()`'s first branch, the half that is not the decode:
//!
//! ```c
//! if (len == r->hash_algo->hexsz && !get_oid_hex(str, oid)) {
//!         if (!(flags & GET_OID_SKIP_AMBIGUITY_CHECK) &&
//!             repo_settings_get_warn_ambiguous_refs(r) &&
//!             cfg->warn_on_object_refname_ambiguity) {
//!                 refs_found = repo_dwim_ref(r, str, len, &tmp_oid, &real_ref, 0);
//!                 if (refs_found > 0) {
//!                         warning(warn_msg, len, str);
//!                         if (advice_enabled(ADVICE_OBJECT_NAME_WARNING))
//!                                 fprintf(stderr, "%s\n", _(object_name_msg));
//!                 }
//!         }
//!         return 0;
//! }
//! ```
//!
//! A repository containing a ref literally named 40 hex digits is the only way
//! to reach it, so every fixture here creates one. Expectations were captured
//! from stock git 2.55.0 against the same fixture; the wording below is that
//! output verbatim.
//!
//! The four gates are separately observable and each has a test: the ref must
//! exist, `core.warnAmbiguousRefs` (default **true**) must not be false,
//! `advice.objectNameWarning` gates the paragraph but *not* the `warning:` line,
//! and the bulk readers that clear `warn_on_object_refname_ambiguity` stay silent
//! while the same names on argv do not.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A branch name that is 40 hex digits but names no object this repository has —
/// the accident the warning exists to point at.
const ABSENT_HEX: &str = "0123456789012345678901234567890123456789";

/// The paragraph `object_name_msg` holds, verbatim from git 2.55.0
/// (`object-name.c`). Printed with a bare `fprintf(stderr, "%s\n", …)`, so it
/// carries no `hint: ` prefix.
const ADVICE_PARAGRAPH: &str = "\
Git normally never creates a ref that ends with 40 hex characters
because it will be ignored when you just specify 40-hex. These refs
may be created by mistake. For example,

  git switch -c $br $(git rev-parse ...)

where \"$br\" is somehow empty and a 40-hex ref is created. Please
examine these refs and maybe delete them. Turn this message off by
running \"git config set advice.objectNameWarning false\"";

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args, None);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

fn run(repo: &Path, home: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match stdin {
        Some(_) => cmd.stdin(Stdio::piped()),
        None => cmd.stdin(Stdio::null()),
    };
    let mut child = cmd.spawn().unwrap();
    if let Some(text) = stdin {
        use std::io::Write as _;
        child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

/// A repository with one commit and, unless `ambiguous` is false, two refs whose
/// names are 40 hex digits: one naming an object that is not there, and one
/// naming the commit itself. Returns the repo, the home, and HEAD's id.
fn fixture(tag: &str, ambiguous: bool) -> (PathBuf, PathBuf, String) {
    let root = std::env::temp_dir().join(format!("zvcs-objname-ambig-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &home, &["add", "a.txt"]);
    git(&repo, &home, &["commit", "-q", "-m", "first"]);

    let head = String::from_utf8(run(&repo, &home, &["rev-parse", "HEAD"], None).stdout).unwrap();
    let head = head.trim().to_string();
    assert_eq!(head.len(), 40, "fixture assumes sha1");
    if ambiguous {
        git(&repo, &home, &["update-ref", &format!("refs/heads/{ABSENT_HEX}"), "HEAD"]);
        git(&repo, &home, &["update-ref", &format!("refs/heads/{head}"), "HEAD"]);
    }
    (repo, home, head)
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn warning_line(name: &str) -> String {
    format!("warning: refname '{name}' is ambiguous.")
}

fn warnings_in(err: &str, name: &str) -> usize {
    err.lines().filter(|l| *l == warning_line(name)).count()
}

/// The whole message, both lines and paragraph, exactly as stock prints it —
/// including that the paragraph is *not* prefixed `hint:` and that the id git
/// names is the 40 characters, not the operand.
#[test]
fn full_hex_naming_a_ref_warns_with_the_advice_paragraph() {
    let (repo, home, head) = fixture("full", true);

    let out = run(&repo, &home, &["rev-parse", &head], None);
    let err = stderr_of(&out);
    assert!(out.status.success(), "stock exits 0 here; stderr:\n{err}");
    assert_eq!(
        err,
        format!("{}\n{ADVICE_PARAGRAPH}\n", warning_line(&head)),
        "stderr must match stock byte for byte"
    );
    // The full-hex branch answers with the id itself, warning or no warning.
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), head);

    // The rule does not consult the object database, so a ref named 40 hex
    // digits that no object answers to warns just the same.
    let out = run(&repo, &home, &["rev-parse", ABSENT_HEX], None);
    assert_eq!(
        stderr_of(&out),
        format!("{}\n{ADVICE_PARAGRAPH}\n", warning_line(ABSENT_HEX)),
        "an absent object is still a decoded id"
    );
}

/// Gate 4: `refs_found > 0`. Without a ref of that name there is nothing to warn
/// about, which is why an ordinary repository never sees this message.
#[test]
fn no_such_ref_is_silent() {
    let (repo, home, head) = fixture("control", false);

    for spec in [head.as_str(), ABSENT_HEX, "HEAD", "main"] {
        let err = stderr_of(&run(&repo, &home, &["rev-parse", spec], None));
        assert!(!err.contains("is ambiguous"), "`rev-parse {spec}` must be silent:\n{err}");
    }
}

/// Gate 3: `core.warnAmbiguousRefs`. git's default is **true**
/// (`repo_settings_get_warn_ambiguous_refs()` passes 1 as the fallback), so only
/// an explicit false silences it — and it takes the paragraph with it.
#[test]
fn core_warn_ambiguous_refs_false_silences_line_and_paragraph() {
    let (repo, home, head) = fixture("cfgwarn", true);

    let err = stderr_of(&run(
        &repo,
        &home,
        &["-c", "core.warnAmbiguousRefs=false", "rev-parse", &head],
        None,
    ));
    assert_eq!(err, "", "core.warnAmbiguousRefs=false silences the whole branch:\n{err}");

    // Explicitly true is the default, so it behaves like the unset case.
    let err = stderr_of(&run(
        &repo,
        &home,
        &["-c", "core.warnAmbiguousRefs=true", "rev-parse", &head],
        None,
    ));
    assert_eq!(err, format!("{}\n{ADVICE_PARAGRAPH}\n", warning_line(&head)));
}

/// `advice.objectNameWarning` gates only the paragraph. The `warning:` line is
/// not advice and survives — the distinction a caller most easily gets wrong.
#[test]
fn advice_object_name_warning_false_keeps_the_warning_line() {
    let (repo, home, head) = fixture("cfgadvice", true);

    let err = stderr_of(&run(
        &repo,
        &home,
        &["-c", "advice.objectNameWarning=false", "rev-parse", &head],
        None,
    ));
    assert_eq!(err, format!("{}\n", warning_line(&head)), "paragraph gone, line stays:\n{err}");

    // `GIT_ADVICE=0` is `advice_enabled()`'s environment override and reaches
    // this paragraph the same way.
    let mut out = Command::new(BIN);
    let out = out
        .args(["rev-parse", &head])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_ADVICE", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(stderr_of(&out), format!("{}\n", warning_line(&head)));
}

/// One warning per operand, because git reaches `get_oid_basic()` once per
/// operand — two operands warn twice, and a range warns for each endpoint.
#[test]
fn warns_once_per_operand() {
    let (repo, home, head) = fixture("count", true);

    let err = stderr_of(&run(
        &repo,
        &home,
        &["-c", "advice.objectNameWarning=false", "rev-parse", &head, &head],
        None,
    ));
    assert_eq!(warnings_in(&err, &head), 2, "one per operand:\n{err}");

    let range = format!("{head}..{head}");
    let err = stderr_of(&run(
        &repo,
        &home,
        &["-c", "advice.objectNameWarning=false", "rev-parse", &range],
        None,
    ));
    assert_eq!(warnings_in(&err, &head), 2, "`try_difference()` resolves both ends:\n{err}");

    let err = stderr_of(&run(
        &repo,
        &home,
        &["-c", "advice.objectNameWarning=false", "merge-base", &head, &head],
        None,
    ));
    assert_eq!(warnings_in(&err, &head), 2, "two operands, two warnings:\n{err}");
}

/// The commands that take an object name from argv warn — the rule lives in
/// `get_oid_basic()`, not in any one builtin.
#[test]
fn argv_operands_warn_across_commands() {
    let (repo, home, head) = fixture("argv", true);
    let quiet_advice = ["-c", "advice.objectNameWarning=false"];

    let h = head.as_str();
    for args in [
        vec!["cat-file", "-t", h],
        vec!["cat-file", "-p", h],
        vec!["log", "--oneline", h],
        vec!["rev-list", "--count", h],
        vec!["diff", h],
        vec!["ls-tree", h],
        vec!["describe", "--always", h],
        vec!["name-rev", h],
        vec!["branch", "--contains", h],
        vec!["tag", "--points-at", h],
        vec!["for-each-ref", &format!("--merged={head}")],
        vec!["merge-base", "--is-ancestor", h, h],
        vec!["show", "--oneline", "--no-patch", h],
    ] {
        let mut argv: Vec<&str> = quiet_advice.to_vec();
        argv.extend(args.iter().copied());
        let err = stderr_of(&run(&repo, &home, &argv, None));
        assert!(
            err.contains(&warning_line(&head)),
            "`git {}` must warn; stderr:\n{err}",
            args.join(" ")
        );
    }
}

/// The readers that clear `warn_on_object_refname_ambiguity` stay silent for the
/// very same name their argv form warns about. Getting this wrong makes plumbing
/// noisy, which is worse than the silence it replaces.
#[test]
fn bulk_readers_stay_silent() {
    let (repo, home, head) = fixture("bulk", true);
    let line = format!("{head}\n");

    // `batch_objects()` (builtin/cat-file.c).
    for mode in ["--batch", "--batch-check"] {
        let err = stderr_of(&run(&repo, &home, &["cat-file", mode], Some(&line)));
        assert_eq!(err, "", "`cat-file {mode}` must not warn:\n{err}");
    }

    // `read_revisions_from_stdin()` (revision.c), reached by every `--stdin`
    // revision reader.
    for args in [vec!["rev-list", "--stdin"], vec!["log", "--oneline", "--stdin"]] {
        let err = stderr_of(&run(&repo, &home, &args, Some(&line)));
        assert_eq!(err, "", "`git {}` must not warn:\n{err}", args.join(" "));
    }

    // `get_object_list()` (builtin/pack-objects.c).
    let out = run(&repo, &home, &["pack-objects", "--stdout", "--revs"], Some(&line));
    assert!(
        !stderr_of(&out).contains("is ambiguous"),
        "`pack-objects --revs` must not warn:\n{}",
        stderr_of(&out)
    );

    // `update-ref` is silent for the other reason in the same condition:
    // `GET_OID_SKIP_AMBIGUITY_CHECK`.
    let err = stderr_of(&run(&repo, &home, &["update-ref", "refs/heads/tmp", &head], None));
    assert_eq!(err, "", "`update-ref` passes GET_OID_SKIP_AMBIGUITY_CHECK:\n{err}");

    // The same name on argv still warns, which is what makes the four silences
    // above a property of the reader rather than of the repository.
    let err = stderr_of(&run(&repo, &home, &["cat-file", "-t", &head], None));
    assert!(err.contains(&warning_line(&head)), "argv form must still warn:\n{err}");
}

/// A name that is not exactly `hexsz` hex digits never takes this branch, so a
/// 39- or 41-character name, or one with a non-hex character, is silent even
/// when a ref of that name exists.
#[test]
fn only_full_length_hex_takes_the_branch() {
    let (repo, home, _head) = fixture("length", true);
    let short = &ABSENT_HEX[..39];
    let long = format!("{ABSENT_HEX}0");
    for name in [short, long.as_str(), "main"] {
        git(&repo, &home, &["update-ref", &format!("refs/heads/{name}x"), "HEAD"]);
        let err = stderr_of(&run(&repo, &home, &["rev-parse", &format!("{name}x")], None));
        assert!(!err.contains("is ambiguous"), "`{name}x` must not take the branch:\n{err}");
    }
}
