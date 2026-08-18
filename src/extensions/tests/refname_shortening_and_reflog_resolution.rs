//! Three rules that decide how a ref name is *shortened* and how a reflog operand
//! is *resolved*, each of which zvcs previously answered with a prefix strip or a
//! gitoxide ref lookup rather than with git's own algorithm.
//!
//! 1. `refs_shorten_unambiguous_ref()` (`refs.c:1625-1686`) walks
//!    `ref_rev_parse_rules` from the most specific rule down and accepts the first
//!    candidate no *other* rule could expand into an existing ref. Two things a
//!    prefix strip cannot express: the last rule carries a `/HEAD` **suffix**, and
//!    an ambiguous candidate is rejected. `strict` differs per caller —
//!    `%(refname:short)` and `rev-parse --abbrev-ref` pass
//!    `core.warnAmbiguousRefs`, the reflog walker's `%gd` passes 0.
//!
//! 2. `get_oid_basic()`'s reflog branch (`object-name.c:742-822`) resolves
//!    `<ref>@{…}` through `repo_dwim_log()`, which has no ambiguity rule at all —
//!    it takes the first `ref_rev_parse_rules` spelling that both resolves *and*
//!    has a log. `dup@{0}` therefore answers even though plain `dup` is ambiguous.
//!
//! 3. Both dwims insist the name resolves to an object, so an unborn HEAD with a
//!    stale `logs/HEAD` is a fatal, not a listing — and `reflog delete` on it is
//!    `error: no reflog for …` with the log left untouched.
//!
//! Every expectation below was measured against stock git 2.55.0
//! (`/opt/homebrew/bin/git`) on the same fixture before being written down.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

struct Fx {
    repo: PathBuf,
    home: PathBuf,
}

/// Two commits, then the names that make each rule observable:
///
/// * `dup` as both `refs/heads/dup` and `refs/tags/dup` — the ambiguous pair;
/// * `refs/remotes/origin/HEAD` — the only name the `refs/remotes/%.*s/HEAD` rule
///   can shorten, and the one a prefix strip gets wrong;
/// * `refs/remotes/tri/HEAD` alongside `refs/tags/tri`, so the `/HEAD` rule's
///   candidate (`tri`) is itself ambiguous and the shortening has to fall back;
/// * a branch named `ORIG_HEAD` while `$GIT_DIR/ORIG_HEAD` exists — rule 0, the
///   bare candidate, which a set built from `refs/` alone cannot see.
fn fixture(tag: &str) -> Fx {
    let root = std::env::temp_dir().join(format!("zvcs-refname-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    for i in 1..=2 {
        std::fs::write(repo.join("f.txt"), format!("line {i}\n")).unwrap();
        git(&repo, &home, &["add", "f.txt"]);
        git(&repo, &home, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    git(&repo, &home, &["branch", "dup"]);
    git(&repo, &home, &["tag", "dup"]);
    git(&repo, &home, &["branch", "tri"]);
    git(&repo, &home, &["tag", "tri"]);
    git(&repo, &home, &["branch", "ORIG_HEAD"]);
    git(&repo, &home, &["update-ref", "ORIG_HEAD", "HEAD"]);
    git(&repo, &home, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&repo, &home, &["update-ref", "refs/remotes/origin/HEAD", "HEAD"]);
    git(&repo, &home, &["update-ref", "refs/remotes/tri/HEAD", "HEAD"]);

    Fx { repo, home }
}

/// `git init`, one commit, then HEAD pointed at a branch that does not exist.
/// `logs/HEAD` survives with real entries while `HEAD` resolves to nothing, which
/// is the shape that separates "read the log file" from "resolve the operand".
fn unborn_fixture(tag: &str) -> Fx {
    let root = std::env::temp_dir().join(format!("zvcs-refname-unborn-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), "one\n").unwrap();
    git(&repo, &home, &["add", "f.txt"]);
    git(&repo, &home, &["commit", "-q", "-m", "one"]);
    git(&repo, &home, &["branch", "other"]);
    git(&repo, &home, &["symbolic-ref", "HEAD", "refs/heads/nope"]);

    let log = repo.join(".git/logs/HEAD");
    assert!(log.is_file(), "fixture needs a surviving logs/HEAD");
    Fx { repo, home }
}

// ---------------------------------------------------------------------------
// 1. shorten_unambiguous_ref
// ---------------------------------------------------------------------------

/// The `refs/remotes/%.*s/HEAD` rule. A prefix strip yields `origin/HEAD`; git
/// yields `origin`, in every consumer and at both strictness levels.
#[test]
fn remotes_head_shortens_to_the_remote_name() {
    let fx = fixture("remotes-head");
    let out = run(&fx.repo, &fx.home, &["rev-parse", "--abbrev-ref", "refs/remotes/origin/HEAD"]);
    assert_eq!(stdout(&out), "origin\n");
    let out = run(&fx.repo, &fx.home, &["rev-parse", "--abbrev-ref=loose", "refs/remotes/origin/HEAD"]);
    assert_eq!(stdout(&out), "origin\n");
    let out = run(
        &fx.repo,
        &fx.home,
        &["for-each-ref", "--format=%(refname:short)", "refs/remotes/origin/HEAD"],
    );
    assert_eq!(stdout(&out), "origin\n");
    // `%gd` is the non-strict caller and lands on the same answer here.
    let out = run(
        &fx.repo,
        &fx.home,
        &["reflog", "show", "--format=%gd", "refs/remotes/origin/HEAD"],
    );
    assert_eq!(stdout(&out), "origin@{0}\n");
}

/// The candidate produced by the `/HEAD` rule can itself be ambiguous, and then
/// the loop keeps going: `refs/remotes/tri/HEAD` cannot become `tri` because
/// `refs/tags/tri` exists, so it stays `tri/HEAD` via the plain `refs/remotes/`
/// rule. This is the case a "strip the last component" shortcut gets wrong in the
/// opposite direction from the one above.
#[test]
fn an_ambiguous_remote_head_candidate_falls_back() {
    let fx = fixture("remotes-head-ambiguous");
    let out = run(&fx.repo, &fx.home, &["rev-parse", "--abbrev-ref", "refs/remotes/tri/HEAD"]);
    assert_eq!(stdout(&out), "tri/HEAD\n");
    let out = run(
        &fx.repo,
        &fx.home,
        &["for-each-ref", "--format=%(refname:short)", "refs/remotes/tri/HEAD"],
    );
    assert_eq!(stdout(&out), "tri/HEAD\n");
}

/// `strict` decides whether a candidate may collide with a *more* specific rule.
/// `%(refname:short)` and `--abbrev-ref` default to `core.warnAmbiguousRefs`
/// (true), so `refs/tags/dup` keeps a component; `--abbrev-ref=loose` is the same
/// call with `strict = 0` and does not.
#[test]
fn strictness_decides_whether_an_ambiguous_tag_keeps_a_component() {
    let fx = fixture("strict");
    let out = run(&fx.repo, &fx.home, &["rev-parse", "--abbrev-ref", "refs/tags/dup"]);
    assert_eq!(stdout(&out), "tags/dup\n");
    let out = run(&fx.repo, &fx.home, &["rev-parse", "--abbrev-ref=strict", "refs/tags/dup"]);
    assert_eq!(stdout(&out), "tags/dup\n");
    let out = run(&fx.repo, &fx.home, &["rev-parse", "--abbrev-ref=loose", "refs/tags/dup"]);
    assert_eq!(stdout(&out), "dup\n");
    // The branch half keeps its component at either strictness: `refs/tags/dup`
    // sits *before* `refs/heads/` in the rule list, so even the non-strict scan
    // sees it.
    let out = run(&fx.repo, &fx.home, &["rev-parse", "--abbrev-ref=loose", "refs/heads/dup"]);
    assert_eq!(stdout(&out), "heads/dup\n");
    let out = run(
        &fx.repo,
        &fx.home,
        &["for-each-ref", "--format=%(refname:short)", "refs/tags/dup", "refs/heads/dup"],
    );
    assert_eq!(stdout(&out), "heads/dup\ntags/dup\n");
}

/// Rule 0 is the bare candidate, and it is tested against the *ref store*, not
/// against the `refs/` enumeration: `$GIT_DIR/ORIG_HEAD` is what stops
/// `refs/heads/ORIG_HEAD` from shortening to `ORIG_HEAD`.
#[test]
fn a_root_ref_makes_the_bare_candidate_ambiguous() {
    let fx = fixture("root-ref");
    let out = run(&fx.repo, &fx.home, &["rev-parse", "--abbrev-ref", "refs/heads/ORIG_HEAD"]);
    assert_eq!(stdout(&out), "heads/ORIG_HEAD\n");
    let out = run(
        &fx.repo,
        &fx.home,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads/ORIG_HEAD"],
    );
    assert_eq!(stdout(&out), "heads/ORIG_HEAD\n");
}

/// `show_rev()`'s `default: /* ambiguous */` arm: when `repo_dwim_ref()` finds
/// more than one spelling there is no single full name to report, so git prints
/// `error: refname '<name>' is ambiguous` and **nothing** on stdout, at exit 0.
/// Naming the first match instead is a wrong answer at a successful exit — the
/// worst failure shape there is. It is not the ambiguity *warning*: `-q`
/// silences that one and leaves this.
#[test]
fn an_ambiguous_name_has_no_single_full_name_to_print() {
    let fx = fixture("ambiguous-abbrev");
    for flag in ["--abbrev-ref", "--abbrev-ref=loose", "--symbolic-full-name"] {
        let out = run(&fx.repo, &fx.home, &["rev-parse", flag, "dup"]);
        assert_eq!(code(&out), 0, "{flag}");
        assert_eq!(stdout(&out), "", "{flag}");
        assert_eq!(
            stderr(&out),
            "warning: refname 'dup' is ambiguous.\nerror: refname 'dup' is ambiguous\n",
            "{flag}"
        );
    }
    // The `error:` survives `-q`; only the warning above it is suppressed.
    let out = run(&fx.repo, &fx.home, &["rev-parse", "-q", "--abbrev-ref", "dup"]);
    assert_eq!(stderr(&out), "error: refname 'dup' is ambiguous\n");
    assert_eq!(stdout(&out), "");
    // An unambiguous operand later in argv still prints.
    let out = run(&fx.repo, &fx.home, &["rev-parse", "--abbrev-ref", "dup", "main"]);
    assert_eq!(stdout(&out), "main\n");
}

/// `grab_values()` reaches `grab_signature()` only from its `case OBJ_COMMIT:`
/// arm, so a tag object — signed or not — leaves every `%(signature:…)` atom at
/// its empty initial value. Reporting `N`/`undefined` there was a fabricated
/// verdict about an object git never even inspects.
///
/// The fixture writes the tag object by hand so the case needs no key, no agent
/// and no `gpg` binary: what matters is that the object carries a signature
/// block, not that the signature verifies.
#[test]
fn signature_atoms_are_empty_on_a_tag_object() {
    let fx = fixture("signature-tag");
    let commit = stdout(&run(&fx.repo, &fx.home, &["rev-parse", "HEAD"]));
    let body = format!(
        "object {}type commit\ntag sigtag\ntagger A <a@example.invalid> 1112911993 -0700\n\n         signed tag message\n-----BEGIN PGP SIGNATURE-----\n\naGVsbG8K\n         -----END PGP SIGNATURE-----\n",
        commit
    );
    let obj = fx.repo.join("tagobj.txt");
    std::fs::write(&obj, body).unwrap();
    let out = run(&fx.repo, &fx.home, &["hash-object", "-t", "tag", "-w", "tagobj.txt"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let tag = stdout(&out).trim().to_string();
    std::fs::remove_file(&obj).unwrap();
    git(&fx.repo, &fx.home, &["update-ref", "refs/tags/sigtag", &tag]);

    let fmt = "%(signature:grade)|%(signature:trustlevel)|%(signature:signer)|%(signature)";
    let out = run(&fx.repo, &fx.home, &["for-each-ref", &format!("--format={fmt}"), "refs/tags/sigtag"]);
    assert_eq!(stdout(&out), "|||\n", "stderr: {}", stderr(&out));

    // An unsigned *commit* does report the initial verdict — `check_commit_signature`
    // runs for it, and leaves `result = 'N'`, `trust_level = TRUST_UNDEFINED`.
    let out = run(&fx.repo, &fx.home, &["for-each-ref", &format!("--format={fmt}"), "refs/heads/main"]);
    assert_eq!(stdout(&out), "N|undefined||\n", "stderr: {}", stderr(&out));

    // …and the deref form reaches the peeled commit, so it is unaffected.
    let out = run(
        &fx.repo,
        &fx.home,
        &["for-each-ref", "--format=%(*signature:grade)", "refs/tags/sigtag"],
    );
    assert_eq!(stdout(&out), "N\n", "stderr: {}", stderr(&out));
}

/// `--abbrev-ref` takes an optional value and rejects anything but the two modes,
/// before it looks at the operand.
#[test]
fn abbrev_ref_rejects_an_unknown_mode() {
    let fx = fixture("abbrev-mode");
    let out = run(&fx.repo, &fx.home, &["rev-parse", "--abbrev-ref=bogus", "HEAD"]);
    assert_eq!(code(&out), 128);
    assert_eq!(stderr(&out), "fatal: unknown mode for --abbrev-ref: bogus\n");
    assert_eq!(stdout(&out), "");
}

// ---------------------------------------------------------------------------
// 2. repo_dwim_log for an ambiguous name
// ---------------------------------------------------------------------------

/// `dup` is a branch *and* a tag, so an ordinary ref lookup is ambiguous — but the
/// reflog path never does one. `repo_dwim_log()` takes the first rule that both
/// resolves and has a log, and only `refs/heads/dup` has one.
#[test]
fn an_ambiguous_name_resolves_through_dwim_log() {
    let fx = fixture("dwim-log");
    let want = stdout(&run(&fx.repo, &fx.home, &["rev-parse", "refs/heads/dup"]));

    let out = run(&fx.repo, &fx.home, &["rev-parse", "dup@{0}"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), want);

    // A date selector reaches the same lookup.
    let out = run(&fx.repo, &fx.home, &["rev-parse", "dup@{now}"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), want);

    // …and so does the reflog reader, which prints the operand as typed.
    let out = run(&fx.repo, &fx.home, &["reflog", "show", "--format=%gd", "dup@{0}"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "dup@{0}\n");
}

/// `read_complete_reflog()`'s fallback chain (`reflog-walk.c:68-103`) is
/// `<ref>`, the symref target, `refs/<ref>`, `refs/heads/<ref>` — and notably not
/// `refs/tags/`. A bare `dup` therefore lists the *branch's* log while the tag of
/// the same name is what makes the name ambiguous.
#[test]
fn a_bare_ambiguous_name_lists_the_branch_log() {
    let fx = fixture("complete-reflog");
    let out = run(&fx.repo, &fx.home, &["reflog", "show", "--format=%gD", "dup"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    // The name printed is the operand, not the ref the entries came from.
    assert_eq!(stdout(&out), "dup@{0}\n");
    assert_eq!(stderr(&out), "warning: refname 'dup' is ambiguous.\n");
}

/// The ambiguity warning for a reflog operand counts *logs*, not refs, and names
/// the operand with its selector cut off. `dup` has one log (the tag has none) and
/// so is silent; `tri` has two (`refs/heads/tri` and `refs/remotes/tri/HEAD`) and
/// warns.
#[test]
fn the_reflog_ambiguity_warning_counts_logs() {
    let fx = fixture("log-count");
    let out = run(&fx.repo, &fx.home, &["rev-parse", "dup@{0}"]);
    assert_eq!(stderr(&out), "");
    let out = run(&fx.repo, &fx.home, &["rev-parse", "tri@{0}"]);
    assert_eq!(stderr(&out), "warning: refname 'tri' is ambiguous.\n");
    assert_eq!(code(&out), 0);
}

/// `reflog delete` uses the same `repo_dwim_log()`, reports the operand *as typed*
/// when it finds nothing, and keeps going through the remaining operands.
#[test]
fn reflog_delete_dwims_and_continues_past_a_failure() {
    let fx = fixture("delete");
    let log = fx.repo.join(".git/logs/refs/heads/dup");
    let before = std::fs::read_to_string(&log).unwrap();
    assert_eq!(before.lines().count(), 1, "fixture assumes one entry");

    let out = run(&fx.repo, &fx.home, &["reflog", "delete", "nosuch@{0}", "dup@{0}"]);
    assert_eq!(code(&out), 255);
    assert_eq!(stderr(&out), "error: no reflog for 'nosuch@{0}'\n");
    // The second operand still ran: `dup@{0}` is the branch's only entry.
    assert_eq!(std::fs::read(&log).unwrap(), b"");
}

/// `--verbose` is not decoration: `should_expire_reflog_ent_verbose()` prints one
/// line per entry, in oldest-first order, and `--dry-run` changes the verb rather
/// than silencing it. Accepting the flag and printing nothing was a silent
/// divergence at exit 0.
#[test]
fn reflog_delete_verbose_reports_every_entry() {
    let fx = fixture("verbose");
    let out = run(&fx.repo, &fx.home, &["reflog", "delete", "-n", "--verbose", "main@{0}"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "keep commit (initial): c1\nwould prune commit: c2\n");

    let out = run(&fx.repo, &fx.home, &["reflog", "delete", "--verbose", "main@{0}"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "keep commit (initial): c1\nprune commit: c2\n");
}

/// `--updateref` points the ref at the newest surviving entry, and the files
/// backend does it below the transaction layer — so the log gains no entry of its
/// own. The bug this pins wrote the log line *instead of* moving the ref.
#[test]
fn reflog_delete_updateref_moves_the_ref_without_logging() {
    let fx = fixture("updateref");
    let first = stdout(&run(&fx.repo, &fx.home, &["rev-parse", "main~1"]));
    let log = fx.repo.join(".git/logs/refs/heads/main");
    let before = std::fs::read_to_string(&log).unwrap();
    assert_eq!(before.lines().count(), 2);

    let out = run(&fx.repo, &fx.home, &["reflog", "delete", "--updateref", "main@{0}"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&run(&fx.repo, &fx.home, &["rev-parse", "refs/heads/main"])), first);
    let after = std::fs::read_to_string(&log).unwrap();
    assert_eq!(after.lines().count(), 1, "log gained an entry: {after:?}");
    assert!(after.starts_with(&before[..before.find('\n').unwrap() + 1]));
}

// ---------------------------------------------------------------------------
// 3. an unborn HEAD with a stale log
// ---------------------------------------------------------------------------

/// `add_reflog_for_walk()` is only reached for an operand `setup_revisions()`
/// already turned into a commit, so a `logs/HEAD` that outlived its branch is not
/// enough: the walk never starts.
#[test]
fn reflog_show_on_an_unborn_head_is_fatal() {
    let fx = unborn_fixture("show");
    let log_before = std::fs::read(fx.repo.join(".git/logs/HEAD")).unwrap();
    assert!(!log_before.is_empty(), "fixture needs a non-empty stale log");

    for args in [
        vec!["reflog", "show", "HEAD"],
        vec!["reflog", "HEAD"],
        vec!["rev-parse", "HEAD@{0}"],
    ] {
        let out = run(&fx.repo, &fx.home, &args);
        assert_eq!(code(&out), 128, "{args:?} stdout: {}", stdout(&out));
        assert!(
            stderr(&out).starts_with("fatal: ambiguous argument '"),
            "{args:?} stderr: {}",
            stderr(&out)
        );
    }
    assert_eq!(std::fs::read(fx.repo.join(".git/logs/HEAD")).unwrap(), log_before);
}

/// The sibling verbs answer about the log *file* and are unaffected — `reflog
/// exists HEAD` is true, `reflog list` names it — while `reflog delete` goes
/// through `repo_dwim_log()` and fails, leaving the log byte-identical. Truncating
/// it was the worst of these bugs: correct-looking exit 0, destroyed data.
#[test]
fn the_sibling_reflog_verbs_agree_about_an_unborn_head() {
    let fx = unborn_fixture("siblings");
    let path = fx.repo.join(".git/logs/HEAD");
    let before = std::fs::read(&path).unwrap();

    let out = run(&fx.repo, &fx.home, &["reflog", "exists", "HEAD"]);
    assert_eq!(code(&out), 0);

    let out = run(&fx.repo, &fx.home, &["reflog", "list"]);
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).lines().any(|l| l == "HEAD"), "{}", stdout(&out));

    let out = run(&fx.repo, &fx.home, &["reflog", "delete", "HEAD@{0}"]);
    assert_eq!(code(&out), 255);
    assert_eq!(stderr(&out), "error: no reflog for 'HEAD@{0}'\n");
    assert_eq!(std::fs::read(&path).unwrap(), before, "the stale log was modified");
}

/// A bare `@{…}` is HEAD's own log, and the name it prints is the ref HEAD
/// resolved to — not the operand. Rejecting `at == 0` made the whole spelling
/// silently produce nothing.
#[test]
fn a_bare_selector_reads_the_current_branch_log() {
    let fx = fixture("bare-selector");
    let out = run(&fx.repo, &fx.home, &["reflog", "show", "--format=%gD", "@{0}"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "refs/heads/main@{0}\nrefs/heads/main@{1}\n");
}

/// An all-digit selector large enough to be a unix timestamp is a *date*, not an
/// ordinal (`object-name.c:772-774`), so it warns about how far the log goes back
/// instead of dying about entry count.
#[test]
fn a_large_numeric_selector_is_an_epoch_not_an_ordinal() {
    let fx = fixture("epoch");
    let out = run(&fx.repo, &fx.home, &["reflog", "show", "main@{100000000}"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).starts_with("warning: log for 'main' only goes back to "),
        "stderr: {}",
        stderr(&out)
    );
    // The day of the month is unpadded, as `DATE_MODE(RFC2822)` writes it.
    assert!(stderr(&out).contains(" 14 Nov 2023 "), "stderr: {}", stderr(&out));

    // A small one is still an ordinal.
    let out = run(&fx.repo, &fx.home, &["reflog", "show", "main@{9}"]);
    assert_eq!(code(&out), 128);
    assert_eq!(stderr(&out), "fatal: log for 'main' only has 2 entries\n");
}
