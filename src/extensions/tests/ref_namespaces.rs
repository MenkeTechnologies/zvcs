//! `GIT_NAMESPACE` / `git --namespace=<name>`: which commands are rewritten by a
//! ref namespace, and — the part that is easy to get backwards — which are not.
//!
//! A namespace lets one repository serve several logical ref sets over the wire
//! while sharing an object store: with `GIT_NAMESPACE=ns`, what a peer sees as
//! `refs/heads/main` is stored at `refs/namespaces/ns/refs/heads/main`
//! (`Documentation/gitnamespaces.adoc`).
//!
//! The tempting reading is "a namespace is set, so every ref lookup gets a
//! prefix". Git does not do that. It consults `GIT_NAMESPACE` only in the
//! programs that hand refs to a peer:
//!
//!   * `upload-pack.c:892` — `for_each_namespaced_ref_1()` iterates with
//!     `opts.namespace = get_git_namespace()`; `:1200,1220` write each advertised
//!     name through `strip_namespace()`; `:1090-1107` resolve HEAD with
//!     `refs_head_ref_namespaced()` (`refs.c:1053`).
//!   * `builtin/receive-pack.c` — `update()` builds
//!     `namespaced_name = get_git_namespace() + name` for the ref transaction.
//!   * `http-backend.c:523,569,591,604` — the dumb ref routes.
//!
//! Everywhere else it is ignored, and that is checkable as a negative rather than
//! assumed: the substring `namespace` does not occur even once in
//! `builtin/for-each-ref.c`, `builtin/show-ref.c`, `builtin/rev-parse.c`,
//! `builtin/branch.c`, `builtin/update-ref.c` or `builtin/ls-remote.c`.
//!
//! Two consequences drive most of the tests below, and both are the kind of thing
//! a "prefix everything" implementation gets wrong in opposite directions:
//!
//!   1. With a namespace set and **nothing stored under it**, `for-each-ref` and
//!      friends still list the ordinary refs and still exit 0. Prefixing globally
//!      instead yields an empty listing and
//!      `fatal: The reference 'HEAD' did not exist` — which is exactly the
//!      regression this file guards.
//!   2. With a namespace **populated**, `for-each-ref` reports the
//!      `refs/namespaces/ns/*` refs under their *full, unstripped* names,
//!      alongside the ordinary ones, because to that builtin they are just refs.
//!
//! `ls-remote` looks like a counterexample and is not. `builtin/ls-remote.c` has
//! no namespace code either; it is a client. The namespacing happens inside the
//! `upload-pack` it connects to, which inherits `GIT_NAMESPACE` through the
//! environment like any other child. So `ls-remote` is the one command here whose
//! output *does* change with the namespace, and it changes because of the server,
//! not because of itself.
//!
//! Every expectation below was captured from stock git 2.55.0 in a fixture built
//! byte-identically to [`Fixture::new`] — fixed identity and fixed
//! `1700000000 +0000` timestamps, so the commit id is deterministic and the
//! listings can be pinned literally rather than pattern-matched.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The single commit every fixture builds, at a pinned identity and date. Pinning
/// the id is what lets the ref listings below be compared as exact bytes instead
/// of being reduced to "some 40 hex digits, then the name".
const COMMIT: &str = "174e41a0b3a6254b4af429f297c6d6c4361f5146";

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One commit on `main`, an idle `feature`, and a lightweight tag `v0.1.0` —
    /// deliberately three *ordinary* refs, because the empty-namespace tests are
    /// asserting that these keep showing up when a namespace is set.
    ///
    /// Nothing is created under `refs/namespaces/` here; [`Self::populate`] does
    /// that, so both halves of the feature can be exercised from one fixture
    /// shape.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-refns-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), b"one\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "one"]);
        f.git(&["tag", "v0.1.0"]);
        f.git(&["branch", "feature"]);
        assert_eq!(
            f.run(&["rev-parse", "HEAD"]).0.trim(),
            COMMIT,
            "fixture is not reproducing the pinned commit id; the literal \
             expectations below are keyed to it"
        );
        f
    }

    /// Give the `ns` namespace its own `HEAD`, branch and tag, mirroring what a
    /// `receive-pack` running under `GIT_NAMESPACE=ns` would have written. These
    /// are created *without* a namespace set, by their full names, precisely
    /// because `update-ref` does not namespace (see
    /// [`update_ref_writes_outside_the_namespace`]).
    fn populate(&self) {
        self.git(&["update-ref", "refs/namespaces/ns/refs/heads/main", COMMIT]);
        self.git(&["update-ref", "refs/namespaces/ns/refs/tags/nstag", COMMIT]);
        self.git(&["symbolic-ref", "refs/namespaces/ns/HEAD", "refs/heads/main"]);
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_NAMESPACE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e.co")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e.co")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// stdout, stderr and exit status kept apart: the regression being guarded
    /// showed up as a status change with an empty stdout, so collapsing the three
    /// would hide it.
    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_ns(None, args)
    }

    /// As [`Self::run`], but with `GIT_NAMESPACE` set to `ns` for this call only.
    fn run_ns(&self, ns: Option<&str>, args: &[&str]) -> (String, String, i32) {
        let mut c = self.cmd(args);
        if let Some(ns) = ns {
            c.env("GIT_NAMESPACE", ns);
        }
        let out = c.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// The three ordinary refs, in `for-each-ref` order. Every "the namespace is
/// ignored" assertion below reduces to "this, unchanged".
fn ordinary_for_each_ref() -> String {
    format!(
        "{COMMIT} commit\trefs/heads/feature\n\
         {COMMIT} commit\trefs/heads/main\n\
         {COMMIT} commit\trefs/tags/v0.1.0\n"
    )
}

// ---------------------------------------------------------------------------
// A namespace that holds nothing: the listing commands must not notice it
// ---------------------------------------------------------------------------

/// The parity regression itself. `GIT_NAMESPACE=ns` with no `refs/namespaces/ns/`
/// in existence must leave `for-each-ref` completely alone — same three refs,
/// same exit 0. Namespacing the ref store globally instead makes this print
/// nothing and exit 1, because HEAD resolves through the namespace and is absent.
#[test]
fn for_each_ref_ignores_an_empty_namespace() {
    let f = Fixture::new("fer-empty");
    let expected = ordinary_for_each_ref();
    assert_eq!(
        f.run_ns(Some("ns"), &["for-each-ref"]),
        (expected.clone(), String::new(), 0),
        "an unpopulated namespace must not change for-each-ref"
    );
    // The control: identical to the no-namespace run. If a future change makes
    // *both* wrong in the same way, the pair above alone would still pass.
    assert_eq!(
        f.run(&["for-each-ref"]).0,
        expected,
        "for-each-ref with and without a namespace must agree"
    );
}

/// Same regression through `show-ref`, which the parity corpus flagged
/// alongside `for-each-ref`. `builtin/show-ref.c` contains no namespace code at
/// all, so its output is the plain ref listing regardless.
#[test]
fn show_ref_ignores_an_empty_namespace() {
    let f = Fixture::new("sr-empty");
    let expected = format!(
        "{COMMIT} refs/heads/feature\n\
         {COMMIT} refs/heads/main\n\
         {COMMIT} refs/tags/v0.1.0\n"
    );
    assert_eq!(
        f.run_ns(Some("ns"), &["show-ref"]),
        (expected, String::new(), 0),
        "an unpopulated namespace must not change show-ref"
    );
}

/// `HEAD` is the specific lookup that used to fail: a globally namespaced store
/// resolves it as `refs/namespaces/ns/HEAD`, which does not exist, and the
/// command dies. In git, HEAD is namespaced *only* through the explicitly named
/// `refs_head_ref_namespaced()` (`refs.c:1053`), which none of these three call.
#[test]
fn head_resolution_ignores_an_empty_namespace() {
    let f = Fixture::new("head-empty");
    assert_eq!(
        f.run_ns(Some("ns"), &["rev-parse", "HEAD"]),
        (format!("{COMMIT}\n"), String::new(), 0),
        "rev-parse HEAD must not resolve through the namespace"
    );
    assert_eq!(
        f.run_ns(Some("ns"), &["symbolic-ref", "HEAD"]),
        ("refs/heads/main\n".to_string(), String::new(), 0),
        "symbolic-ref HEAD must not resolve through the namespace"
    );
    assert_eq!(
        f.run_ns(Some("ns"), &["rev-parse", "--symbolic-full-name", "HEAD"]),
        ("refs/heads/main\n".to_string(), String::new(), 0),
        "rev-parse --symbolic-full-name HEAD must not resolve through the namespace"
    );
}

// ---------------------------------------------------------------------------
// A populated namespace: the listing commands still must not notice it
// ---------------------------------------------------------------------------

/// The other direction, and the one a "strip the prefix" implementation gets
/// wrong: with refs actually stored under `refs/namespaces/ns/`, `for-each-ref`
/// reports them under their **full names**, mixed into the ordinary refs in plain
/// name order. It does not strip them and it does not hide the ordinary ones.
#[test]
fn for_each_ref_lists_namespaced_refs_under_their_full_names() {
    let f = Fixture::new("fer-populated");
    f.populate();
    let expected = format!(
        "{COMMIT} commit\trefs/heads/feature\n\
         {COMMIT} commit\trefs/heads/main\n\
         {COMMIT} commit\trefs/namespaces/ns/HEAD\n\
         {COMMIT} commit\trefs/namespaces/ns/refs/heads/main\n\
         {COMMIT} commit\trefs/namespaces/ns/refs/tags/nstag\n\
         {COMMIT} commit\trefs/tags/v0.1.0\n"
    );
    assert_eq!(
        f.run_ns(Some("ns"), &["for-each-ref"]),
        (expected.clone(), String::new(), 0),
        "a populated namespace must be listed unstripped, not filtered to"
    );
    assert_eq!(
        f.run(&["for-each-ref"]).0,
        expected,
        "setting the namespace must make no difference at all here"
    );
}

/// `rev-parse --all` / `--branches` / `--tags` walk the same ref store and are
/// namespace-free in the C for the same reason. `--branches` and `--tags` are
/// worth pinning separately from `--all`: they restrict by prefix, so an
/// implementation that namespaced the *prefix* rather than the store would still
/// pass an `--all` test.
#[test]
fn rev_parse_ref_selectors_ignore_the_namespace() {
    let f = Fixture::new("rp-populated");
    f.populate();
    // Six refs exist, so --all reports six ids; the namespaced ones are included
    // because they are simply refs.
    let all = format!("{COMMIT}\n").repeat(6);
    assert_eq!(
        f.run_ns(Some("ns"), &["rev-parse", "--all"]),
        (all, String::new(), 0),
        "--all covers namespaced refs as ordinary refs"
    );
    // --branches is refs/heads/* only: feature and main. The namespaced
    // refs/namespaces/ns/refs/heads/main does NOT live under refs/heads/.
    assert_eq!(
        f.run_ns(Some("ns"), &["rev-parse", "--branches"]),
        (format!("{COMMIT}\n").repeat(2), String::new(), 0),
        "--branches stays refs/heads/*, unaffected by the namespace"
    );
    // --tags is refs/tags/* only: v0.1.0. Not nstag, which is namespaced.
    assert_eq!(
        f.run_ns(Some("ns"), &["rev-parse", "--tags"]),
        (format!("{COMMIT}\n"), String::new(), 0),
        "--tags stays refs/tags/*, unaffected by the namespace"
    );
}

/// `branch --list` reads `refs/heads/*` directly. Under a namespace it must still
/// show `feature` and `main` with `main` current — not the namespace's single
/// branch, and not nothing.
#[test]
fn branch_list_ignores_the_namespace() {
    let f = Fixture::new("br-populated");
    f.populate();
    let expected = "  feature\n* main\n".to_string();
    assert_eq!(
        f.run_ns(Some("ns"), &["branch", "--list"]),
        (expected.clone(), String::new(), 0),
        "branch --list must not be filtered to the namespace"
    );
    assert_eq!(
        f.run(&["branch", "--list"]).0,
        expected,
        "branch --list with and without a namespace must agree"
    );
}

/// The write half of the boundary, and the sharpest single check in this file:
/// `update-ref` under `GIT_NAMESPACE=ns` writes `refs/heads/written`, **not**
/// `refs/namespaces/ns/refs/heads/written`. Only `receive-pack` prefixes on
/// write, via `update()`'s `get_git_namespace() + name`; `builtin/update-ref.c`
/// has no namespace code.
#[test]
fn update_ref_writes_outside_the_namespace() {
    let f = Fixture::new("ur-write");
    let (_, err, code) = f.run_ns(Some("ns"), &["update-ref", "refs/heads/written", COMMIT]);
    assert_eq!((err.as_str(), code), ("", 0), "update-ref should succeed");
    let (out, _, _) = f.run(&["for-each-ref", "--format=%(refname)"]);
    assert_eq!(
        out,
        "refs/heads/feature\n\
         refs/heads/main\n\
         refs/heads/written\n\
         refs/tags/v0.1.0\n",
        "update-ref must write the literal name, not a namespaced one"
    );
}

/// `GIT_NAMESPACE=` (set but empty) is not a namespace named "" —
/// `environment.c:get_git_namespace()` returns `""` for both the unset and the
/// empty case, so it must behave exactly like no namespace rather than erroring
/// on an invalid refname.
#[test]
fn an_empty_namespace_variable_is_not_a_namespace() {
    let f = Fixture::new("ns-blank");
    assert_eq!(
        f.run_ns(Some(""), &["for-each-ref"]),
        (ordinary_for_each_ref(), String::new(), 0),
        "GIT_NAMESPACE= must behave as unset"
    );
    // The wire side is where an empty value could plausibly blow up, since that
    // is the side that actually expands the namespace into a ref prefix.
    let (_, _, code) = f.run_ns(Some(""), &["ls-remote", "."]);
    assert_eq!(code, 0, "GIT_NAMESPACE= must not break the transport path");
}

// ---------------------------------------------------------------------------
// The wire side: the one place the namespace is real
// ---------------------------------------------------------------------------

/// `ls-remote` is the payoff. It has no namespace code of its own, but it
/// connects to an `upload-pack` that inherits `GIT_NAMESPACE`, and that server
/// does apply it: names come back **stripped** of `refs/namespaces/ns/`, and refs
/// outside the namespace are gone entirely
/// ("git-upload-pack and git-receive-pack will ignore all references outside the
/// specified namespace" — `gitnamespaces.adoc`).
///
/// The doubled `HEAD` is not a typo and is load-bearing: one line is the
/// advertisement's leading `HEAD` from `refs_head_ref_namespaced()`, the other is
/// `refs/namespaces/ns/HEAD` stripped down to `HEAD` by the ref walk. Stock emits
/// both, so pinning it keeps a "tidy up the duplicate" change from silently
/// diverging.
#[test]
fn ls_remote_is_namespaced_because_upload_pack_is() {
    let f = Fixture::new("lsr-populated");
    f.populate();
    assert_eq!(
        f.run_ns(Some("ns"), &["ls-remote", "."]).0,
        format!(
            "{COMMIT}\tHEAD\n\
             {COMMIT}\tHEAD\n\
             {COMMIT}\trefs/heads/main\n\
             {COMMIT}\trefs/tags/nstag\n"
        ),
        "ls-remote must see the namespace's refs, stripped, and nothing else"
    );
    // Without the namespace the same command sees the whole store, namespaced
    // refs included under their full names. This is the contrast that proves the
    // filtering above came from the namespace and not from a hidden-refs rule.
    assert_eq!(
        f.run(&["ls-remote", "."]).0,
        format!(
            "{COMMIT}\tHEAD\n\
             {COMMIT}\trefs/heads/feature\n\
             {COMMIT}\trefs/heads/main\n\
             {COMMIT}\trefs/namespaces/ns/HEAD\n\
             {COMMIT}\trefs/namespaces/ns/refs/heads/main\n\
             {COMMIT}\trefs/namespaces/ns/refs/tags/nstag\n\
             {COMMIT}\trefs/tags/v0.1.0\n"
        ),
        "without a namespace ls-remote sees everything"
    );
}

/// A namespace with nothing under it hides *everything* from the wire — the
/// opposite of what it does to `for-each-ref`. This pair is the clearest
/// statement of the boundary: same repository, same variable, one command
/// unchanged and the other emptied.
#[test]
fn an_empty_namespace_hides_every_ref_from_the_wire() {
    let f = Fixture::new("lsr-empty");
    f.populate();
    let (out, _, code) = f.run_ns(Some("other"), &["ls-remote", "."]);
    assert_eq!(
        (out.as_str(), code),
        ("", 0),
        "a namespace holding no refs advertises no refs, at exit 0"
    );
    // …while the local listing in the very same state is untouched.
    assert_eq!(
        f.run_ns(Some("other"), &["for-each-ref", "--format=%(refname)"]).0,
        "refs/heads/feature\n\
         refs/heads/main\n\
         refs/namespaces/ns/HEAD\n\
         refs/namespaces/ns/refs/heads/main\n\
         refs/namespaces/ns/refs/tags/nstag\n\
         refs/tags/v0.1.0\n",
        "the same empty namespace must leave for-each-ref alone"
    );
}

/// The hierarchical expansion: "`GIT_NAMESPACE=foo/bar` will store refs under
/// `refs/namespaces/foo/refs/namespaces/bar/`" (`gitnamespaces.adoc`). A naive
/// `refs/namespaces/{}/` format string would look for
/// `refs/namespaces/foo/bar/` and find nothing, so this is worth its own case
/// rather than being folded into the single-component test.
#[test]
fn a_slashed_namespace_expands_hierarchically() {
    let f = Fixture::new("ns-nested");
    f.git(&[
        "update-ref",
        "refs/namespaces/foo/refs/namespaces/bar/refs/heads/deep",
        COMMIT,
    ]);
    assert_eq!(
        f.run_ns(Some("foo/bar"), &["ls-remote", "."]).0,
        format!("{COMMIT}\trefs/heads/deep\n"),
        "foo/bar must expand to refs/namespaces/foo/refs/namespaces/bar/"
    );
}

/// `git --namespace=<name>` is documented as equivalent to the environment
/// variable, and `git.c` implements it by setting `GIT_NAMESPACE`. Pinned because
/// the option is parsed in a different place from where the namespace is applied,
/// so the two can drift apart.
#[test]
fn the_namespace_option_matches_the_environment_variable() {
    let f = Fixture::new("ns-option");
    f.populate();
    let via_option = f.run(&["--namespace=ns", "ls-remote", "."]);
    let via_env = f.run_ns(Some("ns"), &["ls-remote", "."]);
    assert_eq!(via_option, via_env, "--namespace=ns must equal GIT_NAMESPACE=ns");
    assert_eq!(
        via_option.0,
        format!(
            "{COMMIT}\tHEAD\n\
             {COMMIT}\tHEAD\n\
             {COMMIT}\trefs/heads/main\n\
             {COMMIT}\trefs/tags/nstag\n"
        ),
        "and must be the namespaced listing, not a no-op"
    );
    // The separated spelling takes the value from the next argv slot.
    assert_eq!(
        f.run(&["--namespace", "ns", "ls-remote", "."]),
        via_env,
        "--namespace ns must equal --namespace=ns"
    );
}
