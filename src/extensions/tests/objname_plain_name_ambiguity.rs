//! `get_oid_basic()`'s *second* ambiguity warning — the one that fires for a
//! plain name rather than for 40 hex digits (`object-name.c:750-756`):
//!
//! ```c
//! if (!refs_found)
//!         return -1;
//!
//! if (repo_settings_get_warn_ambiguous_refs(r) && !(flags & GET_OID_QUIETLY) &&
//!     (refs_found > 1 ||
//!      !get_short_oid(r, str, len, &tmp_oid, GET_OID_QUIETLY)))
//!         warning(warn_msg, len, str);
//! ```
//!
//! It shares only the message with the full-hex warning above it; the gates are
//! different, and each difference is a case below:
//!
//! * `warn_on_object_refname_ambiguity` is **not** read here, so the four bulk
//!   readers that clear it (`rev-list --stdin`, `cat-file --batch*`,
//!   `pack-objects --revs`, `bundle create --stdin`) still warn about a plain
//!   name while staying silent about a 40-hex ref name.
//! * `GET_OID_SKIP_AMBIGUITY_CHECK` is not read here either, so `update-ref`
//!   warns about a plain name and not about a 40-hex one.
//! * `GET_OID_QUIETLY` *is* read here, and only here.
//!
//! `refs_found` is `repo_dwim_ref()`'s count of matching `ref_rev_parse_rules`
//! spellings; the right-hand disjunct catches the other shape, a single ref whose
//! name is also an unambiguous abbreviated object id. Both are fixtured.
//!
//! Every expectation was measured against stock git 2.55.0
//! (`/opt/homebrew/bin/git`) on the same fixture before being written down.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

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

/// What the fixture built, so a case can name the pieces it needs.
struct Fx {
    repo: PathBuf,
    home: PathBuf,
    /// A 40-hex ref name — the *first* branch's trigger, kept here as the control
    /// that separates the two warnings' gates.
    hex40: String,
    /// A ref name that is 39 hex digits and an unambiguous prefix of `hex40`'s
    /// object: `refs_found == 1`, `get_short_oid()` succeeds, warning due.
    hex39: String,
}

/// Six commits, then:
///
/// * `dup` as both `refs/heads/dup` and `refs/tags/dup` — `refs_found == 2`;
/// * `tri` as `refs/heads/tri`, `refs/tags/tri` and `refs/tri` — `refs_found == 3`;
/// * `solo` as one branch — the unambiguous control;
/// * a branch named 40 hex digits and a branch named the first 39 of them.
///
/// The tag deliberately points further back than the branch, because
/// `ref_rev_parse_rules` puts `refs/tags/` ahead of `refs/heads/`: a case that
/// checks *which* ref won can tell them apart.
fn fixture(tag: &str) -> Fx {
    let root = std::env::temp_dir().join(format!("zvcs-objname-plain-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    for i in 1..=6 {
        std::fs::write(repo.join("f.txt"), format!("line {i}\n")).unwrap();
        git(&repo, &home, &["add", "f.txt"]);
        git(&repo, &home, &["commit", "-q", "-m", &format!("c{i}")]);
    }

    git(&repo, &home, &["branch", "dup", "HEAD~1"]);
    git(&repo, &home, &["tag", "dup", "HEAD~2"]);
    git(&repo, &home, &["branch", "tri", "HEAD~1"]);
    git(&repo, &home, &["tag", "tri", "HEAD~2"]);
    git(&repo, &home, &["update-ref", "refs/tri", "HEAD~2"]);
    git(&repo, &home, &["branch", "solo", "HEAD~1"]);

    let hex40 = String::from_utf8(run(&repo, &home, &["rev-parse", "HEAD~4"], None).stdout).unwrap();
    let hex40 = hex40.trim().to_string();
    assert_eq!(hex40.len(), 40, "fixture assumes sha1");
    let hex39 = hex40[..39].to_string();
    git(&repo, &home, &["branch", &hex40, "HEAD~1"]);
    git(&repo, &home, &["branch", &hex39, "HEAD~1"]);

    Fx { repo, home, hex40, hex39 }
}

fn warning_line(name: &str) -> String {
    format!("warning: refname '{name}' is ambiguous.")
}

/// How many times the warning names `name`, which is the whole measurement: an
/// extra one is as wrong as a missing one.
fn warnings(err: &str, name: &str) -> usize {
    err.lines().filter(|l| *l == warning_line(name)).count()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The two disjuncts, and the control that satisfies neither.
///
/// `dup` and `tri` are `refs_found > 1`; the 39-hex branch is `refs_found == 1`
/// with `!get_short_oid(…)`; `solo` is a single ref that is not hex and stays
/// silent. Nothing here carries the full-hex branch's advice paragraph, which is
/// how the two warnings are told apart on stderr.
#[test]
fn plain_name_matching_more_than_one_ref_warns_once() {
    let fx = fixture("disjuncts");

    for name in ["dup", "tri"] {
        let out = run(&fx.repo, &fx.home, &["rev-parse", name], None);
        assert!(out.status.success(), "stock exits 0 here");
        assert_eq!(
            stderr_of(&out),
            format!("{}\n", warning_line(name)),
            "`rev-parse {name}` is exactly one line, and no advice paragraph"
        );
    }

    let out = run(&fx.repo, &fx.home, &["rev-parse", &fx.hex39], None);
    assert_eq!(
        stderr_of(&out),
        format!("{}\n", warning_line(&fx.hex39)),
        "one ref, but the name is also an unambiguous short oid"
    );

    let out = run(&fx.repo, &fx.home, &["rev-parse", "solo"], None);
    assert_eq!(stderr_of(&out), "", "a single non-hex ref satisfies neither disjunct");
}

/// The name in the message is the one `get_oid_basic()` was handed, not the
/// operand: `get_oid_1()` peels `~<n>`/`^<n>`, `peel_onion()` cuts `^{…}` and
/// `get_oid_with_context_1()` cuts at the `:` — each ending in one call, so each
/// spelling warns once and names the three characters.
#[test]
fn suffixed_operands_warn_once_naming_the_base() {
    let fx = fixture("suffix");

    for spec in ["dup", "dup^", "dup~1", "dup^{commit}", "dup^{}", "dup:f.txt", "dup^0"] {
        let err = stderr_of(&run(&fx.repo, &fx.home, &["rev-parse", spec], None));
        assert_eq!(
            warnings(&err, "dup"),
            1,
            "`rev-parse {spec}` warns once about `dup`; stderr:\n{err}"
        );
    }

    // A range is two operands joined by `||`, so each endpoint warns for itself.
    let err = stderr_of(&run(&fx.repo, &fx.home, &["rev-parse", "dup..tri"], None));
    assert_eq!(warnings(&err, "dup"), 1, "left endpoint:\n{err}");
    assert_eq!(warnings(&err, "tri"), 1, "right endpoint:\n{err}");
}

/// `GET_OID_QUIETLY` gates this warning and not the one above it. `rev-parse` is
/// the builtin that passes it, so `--quiet --verify` is silent for a plain name
/// while a 40-hex ref name still warns.
#[test]
fn quiet_gates_the_plain_warning_but_not_the_full_hex_one() {
    let fx = fixture("quiet");

    let err = stderr_of(&run(&fx.repo, &fx.home, &["rev-parse", "--verify", "dup"], None));
    assert_eq!(warnings(&err, "dup"), 1, "`--verify` alone warns:\n{err}");

    let err =
        stderr_of(&run(&fx.repo, &fx.home, &["rev-parse", "--quiet", "--verify", "dup"], None));
    assert_eq!(err, "", "`GET_OID_QUIETLY` silences the plain-name warning:\n{err}");

    let err = stderr_of(&run(
        &fx.repo,
        &fx.home,
        &["rev-parse", "--quiet", "--verify", &fx.hex40],
        None,
    ));
    assert_eq!(
        warnings(&err, &fx.hex40),
        1,
        "the full-hex branch has no `GET_OID_QUIETLY` test:\n{err}"
    );
}

/// `core.warnAmbiguousRefs` is the one gate the two warnings share, and its
/// default is true.
#[test]
fn core_warn_ambiguous_refs_false_silences_the_plain_warning() {
    let fx = fixture("cfg");

    for args in [
        vec!["-c", "core.warnAmbiguousRefs=false", "rev-parse", "dup"],
        vec!["-c", "core.warnAmbiguousRefs=false", "log", "-1", "--oneline", "dup"],
        vec!["-c", "core.warnAmbiguousRefs=false", "cat-file", "-t", "dup"],
    ] {
        let err = stderr_of(&run(&fx.repo, &fx.home, &args, None));
        assert_eq!(err, "", "`{args:?}` must be silent:\n{err}");
    }

    // `advice.objectNameWarning` belongs to the full-hex branch's paragraph and
    // has nothing to say about this warning.
    let err = stderr_of(&run(
        &fx.repo,
        &fx.home,
        &["-c", "advice.objectNameWarning=false", "rev-parse", "dup"],
        None,
    ));
    assert_eq!(err, format!("{}\n", warning_line("dup")), "advice does not gate the line:\n{err}");
}

/// The verbs that take an object name from argv, one operand each. Stock warns
/// once per resolution, so the count is the assertion — these all resolve `dup`
/// exactly once.
#[test]
fn argv_operands_warn_once_across_the_verb_families() {
    let fx = fixture("verbs");

    let cases: &[&[&str]] = &[
        &["rev-parse", "dup"],
        &["log", "-1", "--oneline", "dup"],
        &["rev-list", "-1", "dup"],
        &["show", "-s", "--oneline", "dup"],
        &["cat-file", "-t", "dup"],
        &["cat-file", "-p", "dup"],
        &["diff-tree", "--name-only", "dup"],
        &["diff-index", "--name-only", "dup"],
        &["merge-base", "dup", "main"],
        &["name-rev", "dup"],
        &["describe", "dup"],
        &["branch", "--contains", "dup"],
        &["tag", "--contains", "dup"],
        &["for-each-ref", "--contains=dup"],
        &["archive", "-o", "/dev/null", "dup"],
        &["fast-export", "dup"],
        &["merge-tree", "dup", "main"],
        &["ls-tree", "dup"],
        &["grep", "-e", "line", "dup"],
        &["verify-tag", "dup"],
        &["shortlog", "dup..main"],
        &["format-patch", "--stdout", "-1", "dup"],
        &["bundle", "create", "b.bundle", "dup"],
        &["reflog", "show", "dup"],
    ];
    for args in cases {
        let err = stderr_of(&run(&fx.repo, &fx.home, args, None));
        assert_eq!(warnings(&err, "dup"), 1, "`git {}`; stderr:\n{err}", args.join(" "));
    }
}

/// The four sites that clear `warn_on_object_refname_ambiguity` around a bulk
/// read. The switch is read inside `get_oid_basic()`'s full-hex branch only, so
/// each of these is silent for a 40-hex ref name and *warns* for a plain one —
/// the pair is the whole point of the case.
#[test]
fn bulk_readers_still_warn_for_a_plain_name() {
    let fx = fixture("bulk");

    let cases: &[(&[&str], &str)] = &[
        (&["rev-list", "--stdin"], "dup\n"),
        (&["log", "--stdin", "--oneline"], "dup\n"),
        (&["cat-file", "--batch-check"], "dup\n"),
        (&["cat-file", "--batch-command"], "info dup\n"),
        (&["pack-objects", "--revs", "--stdout"], "dup\n"),
        (&["bundle", "create", "s.bundle", "--stdin"], "dup\n"),
    ];
    for (args, input) in cases {
        let err = stderr_of(&run(&fx.repo, &fx.home, args, Some(input)));
        assert_eq!(warnings(&err, "dup"), 1, "`git {}` on stdin; stderr:\n{err}", args.join(" "));

        let hex_input = format!("{}\n", fx.hex40);
        let err = stderr_of(&run(&fx.repo, &fx.home, args, Some(&hex_input)));
        assert_eq!(
            warnings(&err, &fx.hex40),
            0,
            "the switch does silence the full-hex branch for `git {}`; stderr:\n{err}",
            args.join(" ")
        );
    }
}

/// `update-ref` passes `GET_OID_SKIP_AMBIGUITY_CHECK`, which is tested in the
/// full-hex branch and nowhere else — so it is silent for a 40-hex ref name and
/// warns for a plain one, once per slot it resolves.
#[test]
fn update_ref_skips_only_the_full_hex_warning() {
    let fx = fixture("updateref");

    let out = run(&fx.repo, &fx.home, &["update-ref", "refs/heads/z", "dup"], None);
    assert_eq!(warnings(&stderr_of(&out), "dup"), 1, "{}", stderr_of(&out));

    let out = run(&fx.repo, &fx.home, &["update-ref", "refs/heads/z2", &fx.hex40], None);
    assert_eq!(
        warnings(&stderr_of(&out), &fx.hex40),
        0,
        "`GET_OID_SKIP_AMBIGUITY_CHECK`:\n{}",
        stderr_of(&out)
    );
}

/// `dwim_branch_start()` (`branch.c:539-594`) resolves the start-point through
/// `repo_get_oid_mb()` and then DWIMs it, and refuses more than one match:
///
/// ```c
/// default:
///         die(_("ambiguous object name: '%s'"), start_name);
/// ```
///
/// `git branch` reaches it once; `checkout -b`/`-B` and `switch -c` reach it
/// after `parse_branchname_arg()` has already resolved the same name, so those
/// warn twice and then die.
#[test]
fn creating_a_branch_from_an_ambiguous_start_point_warns_then_dies() {
    let fx = fixture("create");

    let out = run(&fx.repo, &fx.home, &["branch", "nb", "dup"], None);
    let err = stderr_of(&out);
    assert_eq!(warnings(&err, "dup"), 1, "one resolution:\n{err}");
    assert!(err.contains("fatal: ambiguous object name: 'dup'"), "{err}");
    assert!(!out.status.success());

    for args in [
        vec!["checkout", "-b", "nb2", "dup"],
        vec!["checkout", "-B", "nb3", "dup"],
        vec!["switch", "-c", "nb4", "dup"],
    ] {
        let out = run(&fx.repo, &fx.home, &args, None);
        let err = stderr_of(&out);
        assert_eq!(warnings(&err, "dup"), 2, "`{args:?}` resolves twice:\n{err}");
        assert!(err.contains("fatal: ambiguous object name: 'dup'"), "`{args:?}`:\n{err}");
        assert!(!out.status.success(), "`{args:?}` must fail");
    }
}

/// `get_fork_point()` (`commit.c:1103-1111`) DWIMs its `<ref>` operand itself
/// rather than handing it to `get_oid_basic()`, so that operand earns no warning
/// at all — it is fatal instead. The neighbouring `merge-base` modes, which do go
/// through `get_oid()`, keep theirs.
#[test]
fn merge_base_fork_point_dies_without_warning() {
    let fx = fixture("forkpoint");

    let out = run(&fx.repo, &fx.home, &["merge-base", "--fork-point", "dup", "main"], None);
    let err = stderr_of(&out);
    assert_eq!(warnings(&err, "dup"), 0, "no `get_oid_basic()` on this path:\n{err}");
    assert!(err.contains("fatal: Ambiguous refname: 'dup'"), "{err}");
    assert!(!out.status.success());

    let err = stderr_of(&run(&fx.repo, &fx.home, &["merge-base", "dup", "main"], None));
    assert_eq!(warnings(&err, "dup"), 1, "plain `merge-base` resolves through `get_oid()`:\n{err}");
}

/// `core.warnAmbiguousRefs` reaches further than the warning. `expand_ref()`
/// stops at the first matching rule when it is off:
///
/// ```c
/// if (r) {
///         if (!refs_found++)
///                 *ref = xstrdup(r);
///         if (!repo_settings_get_warn_ambiguous_refs(repo))
///                 break;
/// }
/// ```
///
/// so `refs_found` never exceeds 1, and the callers that turn the count into a
/// `die()` rather than a warning stop dying with it.
#[test]
fn warn_ambiguous_refs_false_caps_refs_found_at_one() {
    let fx = fixture("capped");
    let off = ["-c", "core.warnAmbiguousRefs=false"];

    let out = run(&fx.repo, &fx.home, &[&off[..], &["merge-base", "--fork-point", "dup", "main"]].concat(), None);
    let err = stderr_of(&out);
    assert!(!err.contains("Ambiguous refname"), "one candidate, so no die:\n{err}");

    let out = run(&fx.repo, &fx.home, &[&off[..], &["branch", "nb", "dup"]].concat(), None);
    let err = stderr_of(&out);
    assert_eq!(err, "", "the branch is created:\n{err}");
    assert!(out.status.success());

    let out = run(&fx.repo, &fx.home, &[&off[..], &["checkout", "-b", "nb2", "dup"]].concat(), None);
    let err = stderr_of(&out);
    assert!(!err.contains("ambiguous object name"), "{err}");
    assert!(out.status.success(), "{err}");
}
