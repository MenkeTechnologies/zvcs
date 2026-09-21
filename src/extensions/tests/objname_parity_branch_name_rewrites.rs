//! `repo_interpret_branch_name()` (`object-name.c:1472-1522`) — the rewrite pass
//! `substitute_branch_name()` runs before `repo_dwim_ref()` and `repo_dwim_log()`
//! look anything up — and the one `die()` a `<rev>:<path>` operand has of its own.
//!
//! ```c
//! int repo_interpret_branch_name(struct repository *r, const char *name, int namelen,
//!                                struct strbuf *buf, const struct interpret_branch_name_options *options)
//! {
//!         if (!options->allowed || (options->allowed & INTERPRET_BRANCH_LOCAL)) {
//!                 len = interpret_nth_prior_checkout(r, name, namelen, buf);
//!                 if (!len) {
//!                         return len;
//!                 } else if (len > 0) {
//!                         if (len == namelen)
//!                                 return len;
//!                         else
//!                                 return reinterpret(r, name, namelen, len, buf, options->allowed);
//!                 }
//!         }
//!
//!         for (start = name; (at = memchr(start, '@', namelen - (start - name))); start = at + 1) {
//!                 if (!options->allowed || (options->allowed & INTERPRET_BRANCH_HEAD)) {
//!                         len = interpret_empty_at(name, namelen, at - name, buf);
//!                         if (len > 0)
//!                                 return reinterpret(r, name, namelen, len, buf, options->allowed);
//!                 }
//!                 len = interpret_branch_mark(r, name, namelen, at - name, buf,
//!                                             upstream_mark, branch_get_upstream, options);
//!                 if (len > 0) return len;
//!                 len = interpret_branch_mark(r, name, namelen, at - name, buf,
//!                                             push_mark, branch_get_push, options);
//!                 if (len > 0) return len;
//!         }
//!         return -1;
//! }
//! ```
//!
//! The `reinterpret()` recursion is the part a re-derivation drops: `@{-<n>}` and
//! a bare `@` consume only a *prefix* and hand the spliced name back to the same
//! function, which is why `@{-2}@{u}` names the prior branch's upstream and
//! `@@{u}` names HEAD's. Without it both reached `interpret_branch_mark()` with
//! `@{-2}` / `@` as the branch name and died with `no such branch:`.
//!
//! Every expectation below was captured from stock git 2.55.0 against the same
//! fixture shape and is reproduced verbatim.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Pinned so the reflog timestamps `@{<date>}` and `read_ref_at()` read are the
/// same on every machine.
const DATE: &str = "1700000000 +0000";

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
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
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
    assert!(
        out.status.success(),
        "fixture step `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A repository whose names are deliberately awkward:
///
/// * `dup` is **both** a branch and a tag, so any name that rewrites to it is an
///   ambiguous refname;
/// * HEAD's reflog holds three branch switches, so `@{-1}`/`@{-2}`/`@{-3}` are
///   `two`, `main` and `dup` in that order;
/// * `main` has an upstream (`refs/remotes/origin/main`) and a *different* push
///   destination (`refs/remotes/pushr/landed`), so `@{u}` and `@{push}` cannot be
///   confused for one another;
/// * `refs/remotes/origin/main` has exactly one reflog entry, which makes
///   `@{u}@{1}` one past the end.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("zvcs-objname-rewrite-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "one\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    git(&repo, &home, &["commit", "-q", "-m", "one"]);
    std::fs::write(repo.join("f"), "two\n").unwrap();
    git(&repo, &home, &["commit", "-q", "-am", "two"]);

    // A branch and a tag of the same name: two `ref_rev_parse_rules` spellings,
    // which is `refs_found > 1`.
    git(&repo, &home, &["branch", "dup", "HEAD~1"]);
    git(&repo, &home, &["tag", "dup", "HEAD~1"]);
    git(&repo, &home, &["branch", "two", "HEAD~1"]);

    // An upstream and a push destination that are different refs.
    git(&repo, &home, &["update-ref", "refs/remotes/origin/main", "refs/heads/main"]);
    git(&repo, &home, &["update-ref", "refs/remotes/pushr/landed", "refs/heads/main"]);
    git(&repo, &home, &["config", "branch.main.remote", "origin"]);
    git(&repo, &home, &["config", "branch.main.merge", "refs/heads/main"]);
    git(&repo, &home, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    git(&repo, &home, &["config", "branch.main.pushRemote", "pushr"]);
    git(&repo, &home, &["config", "remote.pushr.push", "refs/heads/main:refs/heads/landed"]);
    git(&repo, &home, &["config", "remote.pushr.fetch", "+refs/heads/*:refs/remotes/pushr/*"]);

    // Three branch switches, so `@{-3}` is the ambiguous `dup`.
    git(&repo, &home, &["checkout", "-q", "dup"]);
    git(&repo, &home, &["checkout", "-q", "main"]);
    git(&repo, &home, &["checkout", "-q", "two"]);
    git(&repo, &home, &["checkout", "-q", "main"]);

    (repo, home)
}

/// The id every `@{u}`-shaped operand below must land on.
fn head_id(repo: &Path, home: &Path) -> String {
    stdout_of(&run(repo, home, &["rev-parse", "main"]))
}

/// `@{-<n>}` consumes the whole operand, so `substitute_branch_name()`
/// substitutes and `repo_dwim_ref()` counts the *rewritten* name's spellings —
/// while `warning(warn_msg, len, str)` still prints the operand as typed.
///
/// Stock 2.55.0, with `dup` both a branch and a tag:
///
/// ```text
/// $ git rev-parse @{-3}
/// warning: refname '@{-3}' is ambiguous.
/// <id>
/// ```
///
/// The warning lives in `get_oid_basic()`, so every verb that resolves an argv
/// operand through `repo_get_oid()` prints it — which is the whole reason it
/// belongs in the shared resolver and not in `rev-parse`.
#[test]
fn nth_prior_checkout_is_dwimmed_before_the_ambiguity_count() {
    let (repo, home) = fixture("nth-prior-ambiguous");
    let dup = stdout_of(&run(&repo, &home, &["rev-parse", "refs/heads/dup"]));

    for args in [
        vec!["rev-parse", "@{-3}"],
        vec!["cat-file", "-t", "@{-3}"],
        vec!["rev-list", "--max-count=1", "@{-3}"],
        vec!["log", "--oneline", "-1", "@{-3}"],
        vec!["describe", "--always", "@{-3}"],
        vec!["merge-base", "HEAD", "@{-3}"],
        vec!["branch", "--contains", "@{-3}"],
        vec!["ls-tree", "--name-only", "@{-3}"],
    ] {
        let out = run(&repo, &home, &args);
        assert!(out.status.success(), "`git {args:?}` must still resolve: {}", stderr_of(&out));
        assert_eq!(
            stderr_of(&out),
            "warning: refname '@{-3}' is ambiguous.\n",
            "`git {args:?}` must warn once, naming the operand as typed"
        );
    }

    // The operand still resolves to the prior branch, warning or no warning.
    assert_eq!(stdout_of(&run(&repo, &home, &["rev-parse", "@{-3}"])), dup);

    // `@{-2}` is `main`, which only one rule matches: no warning at all. Without
    // this half the test would pass for a resolver that warns unconditionally.
    let out = run(&repo, &home, &["rev-parse", "@{-2}"]);
    assert_eq!(stderr_of(&out), "", "`@{{-2}}` names an unambiguous branch");
}

/// `reinterpret()`: `@{-<n>}` consumed a prefix, so the spliced name goes back
/// through the whole interpretation and the mark that follows applies to the
/// prior *branch*.
///
/// Stock 2.55.0: `git rev-parse @{-2}@{u}` answers `main`'s upstream, silently.
#[test]
fn nth_prior_checkout_carries_an_upstream_mark() {
    let (repo, home) = fixture("nth-prior-upstream");
    let want = head_id(&repo, &home);

    for args in [
        vec!["rev-parse", "@{-2}@{u}"],
        vec!["rev-parse", "@{-2}@{upstream}"],
        vec!["rev-list", "--max-count=1", "@{-2}@{u}"],
        vec!["merge-base", "HEAD", "@{-2}@{u}"],
    ] {
        let out = run(&repo, &home, &args);
        assert!(
            out.status.success(),
            "`git {args:?}` must resolve the prior branch's upstream: {}",
            stderr_of(&out)
        );
        assert_eq!(stdout_of(&out), want, "`git {args:?}`");
        assert_eq!(stderr_of(&out), "", "`git {args:?}` says nothing");
    }

    // `cat-file` reaches the same resolver by a different route.
    assert_eq!(stdout_of(&run(&repo, &home, &["cat-file", "-t", "@{-2}@{u}"])), "commit");

    // `@{-1}` is `two`, which has no upstream — the mark still applies to the
    // rewritten name, so the `die()` names *that* branch and not `@{-1}`.
    let out = run(&repo, &home, &["rev-parse", "@{-1}@{u}"]);
    assert!(!out.status.success());
    assert_eq!(stderr_of(&out), "fatal: no upstream configured for branch 'two'\n");
}

/// `interpret_empty_at()`: a bare `@` is `HEAD`, and `@@{…}` is the one other
/// spelling its three tests admit.
///
/// ```c
/// if (len || name[1] == '{') return -1;
/// next = memchr(name + len + 1, '@', namelen - len - 1);
/// if (next && next[1] != '{') return -1;
/// if (!next) next = name + namelen;
/// if (next != name + 1) return -1;
/// ```
#[test]
fn bare_at_is_rewritten_to_head_before_a_mark() {
    let (repo, home) = fixture("empty-at");
    let want = head_id(&repo, &home);

    for spec in ["@@{u}", "@@{upstream}", "@@{U}"] {
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert!(out.status.success(), "`rev-parse {spec}`: {}", stderr_of(&out));
        assert_eq!(stdout_of(&out), want, "`rev-parse {spec}`");
        assert_eq!(stderr_of(&out), "", "`rev-parse {spec}` says nothing");
    }
    assert_eq!(stdout_of(&run(&repo, &home, &["cat-file", "-t", "@@{u}"])), "commit");

    // `next[1] != '{'` — a second `@` that does not open a brace stops the
    // rewrite, so `@x@{u}` is the branch `@x` and not `HEAD`.
    let out = run(&repo, &home, &["rev-parse", "@x@{u}"]);
    assert!(!out.status.success(), "`@x@{{u}}` must not become HEAD");
    assert_eq!(stderr_of(&out), "fatal: no such branch: '@x'\n");
}

/// `--symbolic-full-name` and `--abbrev-ref` report `repo_dwim_ref()`'s `full`,
/// which is the name *after* `substitute_branch_name()` — so the rewrites above
/// have to reach the naming modes too, and `branch_get("HEAD")` there is the
/// checked-out branch just as `branch_get(NULL)` is.
#[test]
fn the_naming_modes_report_the_rewritten_ref() {
    let (repo, home) = fixture("naming");

    for (spec, full, short) in [
        ("@{u}", "refs/remotes/origin/main", "origin/main"),
        ("HEAD@{u}", "refs/remotes/origin/main", "origin/main"),
        ("@@{u}", "refs/remotes/origin/main", "origin/main"),
        ("@{-2}@{u}", "refs/remotes/origin/main", "origin/main"),
        ("@{push}", "refs/remotes/pushr/landed", "pushr/landed"),
        ("HEAD@{push}", "refs/remotes/pushr/landed", "pushr/landed"),
    ] {
        assert_eq!(
            stdout_of(&run(&repo, &home, &["rev-parse", "--symbolic-full-name", spec])),
            full,
            "`rev-parse --symbolic-full-name {spec}`"
        );
        assert_eq!(
            stdout_of(&run(&repo, &home, &["rev-parse", "--abbrev-ref", spec])),
            short,
            "`rev-parse --abbrev-ref {spec}`"
        );
    }
}

/// `interpret_branch_name_options.allowed`: `git check-ref-format --branch`
/// passes `INTERPRET_BRANCH_LOCAL`, and `interpret_empty_at()` is gated on
/// `INTERPRET_BRANCH_HEAD` — so the bare-`@` rewrite does **not** apply there.
///
/// ```c
/// if (!options->allowed || (options->allowed & INTERPRET_BRANCH_HEAD)) {
///         len = interpret_empty_at(name, namelen, at - name, buf);
/// ```
///
/// Stock 2.55.0 answers `git check-ref-format --branch @@{u}` with
/// `fatal: no such branch: '@'` — the `@{u}` mark applied to a branch literally
/// called `@` — while `git rev-parse @@{u}` answers HEAD's upstream. A rewrite
/// that ignored `allowed` would make the two agree, and they must not.
#[test]
fn check_ref_format_branch_does_not_take_the_bare_at_rewrite() {
    let (repo, home) = fixture("allowed");

    let out = run(&repo, &home, &["check-ref-format", "--branch", "@@{u}"]);
    assert!(!out.status.success());
    assert_eq!(stderr_of(&out), "fatal: no such branch: '@'\n");

    // `interpret_nth_prior_checkout()` *is* gated on `INTERPRET_BRANCH_LOCAL`,
    // which this caller passes, so `@{-<n>}` still expands here.
    assert_eq!(stdout_of(&run(&repo, &home, &["check-ref-format", "--branch", "@{-2}"])), "main");
    // …and a branch named `@` is simply itself.
    assert_eq!(stdout_of(&run(&repo, &home, &["check-ref-format", "--branch", "@"])), "@");

    // The same operand through the resolver, where `allowed` is 0 and the
    // rewrite does apply.
    assert_eq!(stdout_of(&run(&repo, &home, &["rev-parse", "@@{u}"])), head_id(&repo, &home));
}

/// `repo_dwim_log()` runs `substitute_branch_name()` too (`refs.c:844`), so the
/// reflog a `<ref>@{<n>}` operand reads is the *rewritten* ref's — while
/// `get_oid_basic()`'s `die()` names the operand's own `str`/`len`:
///
/// ```c
/// die(_("log for '%.*s' only has %d entries"), len, str, co_cnt);
/// ```
///
/// with `len = at`, i.e. everything before the selector. Stock 2.55.0 on a
/// one-entry upstream log: `fatal: log for '@{u}' only has 1 entries`.
#[test]
fn upstream_mark_reflog_reads_the_upstreams_log() {
    let (repo, home) = fixture("upstream-reflog");
    let want = head_id(&repo, &home);

    // In range: the upstream's own current value.
    for spec in ["@{u}@{0}", "main@{u}@{0}", "HEAD@{u}@{0}", "@{upstream}@{0}"] {
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert!(out.status.success(), "`rev-parse {spec}`: {}", stderr_of(&out));
        assert_eq!(stdout_of(&out), want, "`rev-parse {spec}`");
    }

    // One past the end. The message names the operand's ref half, not the
    // upstream it was rewritten to, and it is a `die()` raised below every
    // caller's own "not a valid object name" — so each verb ends on it.
    for (args, named) in [
        (vec!["rev-parse", "@{u}@{1}"], "@{u}"),
        (vec!["rev-parse", "main@{u}@{1}"], "main@{u}"),
        (vec!["cat-file", "-t", "@{u}@{1}"], "@{u}"),
        (vec!["rev-list", "--max-count=1", "@{u}@{1}"], "@{u}"),
        (vec!["merge-base", "HEAD", "main@{u}@{1}"], "main@{u}"),
    ] {
        let out = run(&repo, &home, &args);
        assert!(!out.status.success(), "`git {args:?}` must fail");
        assert_eq!(
            stderr_of(&out),
            format!("fatal: log for '{named}' only has 1 entries\n"),
            "`git {args:?}`"
        );
    }
}

/// The push mark reaches the same `interpret_branch_mark()` with
/// `branch_get_push()` for `get_data`, so it is substituted for lookups exactly
/// as `@{u}` is. The fixture's push destination is a *different* ref from its
/// upstream, so an implementation that quietly answered with the upstream would
/// fail here.
#[test]
fn push_mark_is_substituted_for_the_reflog_lookup() {
    let (repo, home) = fixture("push-reflog");
    let want = head_id(&repo, &home);
    assert_eq!(
        stdout_of(&run(&repo, &home, &["rev-parse", "--symbolic-full-name", "@{push}"])),
        "refs/remotes/pushr/landed",
        "fixture assumes the push destination is not the upstream"
    );

    for spec in ["@{push}@{0}", "main@{push}@{0}", "HEAD@{push}@{0}"] {
        let out = run(&repo, &home, &["rev-parse", spec]);
        assert!(out.status.success(), "`rev-parse {spec}`: {}", stderr_of(&out));
        assert_eq!(stdout_of(&out), want, "`rev-parse {spec}`");
        assert_eq!(stderr_of(&out), "", "`rev-parse {spec}` says nothing");
    }
}

/// `get_oid_with_context_1()`'s `<rev>:<path>` arm has a `die()` of its own for
/// the case where the *rev* half does not resolve:
///
/// ```c
/// if (!get_oid_1(repo, name, len, &tree_oid, sub_flags)) {
///         …
/// } else {
///         if (only_to_die)
///                 die(_("invalid object name '%.*s'."), len, name);
/// }
/// ```
///
/// (`object-name.c:1854-1877`.) `len` is the offset of the splitting colon, so
/// the message names the rev half alone. Every verb that ends in
/// `die_verify_filename()` reaches it through
/// `maybe_die_on_misspelt_object_name()`, which is why the generic
/// `ambiguous argument …` block is wrong for all of them at once.
#[test]
fn rev_path_with_an_unresolvable_rev_names_the_rev() {
    let (repo, home) = fixture("rev-path-die");

    for (args, rev) in [
        (vec!["rev-parse", "nosuchref:f"], "nosuchref"),
        (vec!["rev-parse", "nosuchref:"], "nosuchref"),
        (vec!["log", "--oneline", "-1", "nosuchref:f"], "nosuchref"),
        (vec!["show", "nosuchref:f"], "nosuchref"),
        (vec!["rev-list", "--max-count=1", "nosuchref:f"], "nosuchref"),
    ] {
        let out = run(&repo, &home, &args);
        assert!(!out.status.success(), "`git {args:?}` must fail");
        assert!(
            stderr_of(&out).ends_with(&format!("fatal: invalid object name '{rev}'.\n")),
            "`git {args:?}` stderr was:\n{}",
            stderr_of(&out)
        );
    }

    // A rev that *does* resolve keeps the path-shaped diagnosis, so the arm
    // above cannot have swallowed it.
    let out = run(&repo, &home, &["rev-parse", "main:nosuchfile"]);
    assert!(!out.status.success());
    assert_eq!(stderr_of(&out), "fatal: path 'nosuchfile' does not exist in 'main'\n");

    // And a blob on the left is a resolvable rev, not an invalid object name —
    // git only reaches the `die()` above when `get_oid_1()` itself fails.
    let blob = stdout_of(&run(&repo, &home, &["rev-parse", "main:f"]));
    let out = run(&repo, &home, &["rev-parse", &format!("{blob}:f")]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains(&format!("in '{blob}'")),
        "stderr was:\n{}",
        stderr_of(&out)
    );
}
