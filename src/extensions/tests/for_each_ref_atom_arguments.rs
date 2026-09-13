//! What each ref-filter atom does with an argument — accept it, ignore it, or
//! reject it with which words — and *when* an argument is judged.
//!
//! git decides this per atom through the `parser` column of `valid_atom[]`
//! (ref-filter.c:946-993), and several atoms have no parser at all, so the
//! "obvious" rule — refuse what you do not understand — is wrong for them:
//!
//!   * `%(creator:<x>)` is `%(creator)`, while `%(author:<x>)`,
//!     `%(committer:<x>)` and `%(tagger:<x>)` are accepted and render empty
//!     (`grab_person()` skips them by name, ref-filter.c:1745-1749).
//!   * The parsers that refuse every argument say `%(<atom>) does not take
//!     arguments`, naming the atom without its deref `*` (`err_no_arg`).
//!   * `refname`'s counts are `strtol_i()` with their own message; `oid` and
//!     `align` numbers are `strtoul_ui()`; `%(color:…)` prints the colour
//!     parser's own `error:` first.
//!   * A date format is not parsed until a value is filled
//!     (`grab_date()`, ref-filter.c:1692-1696), so it dies per object, after
//!     the lines already written, and `%(authordate:)` still reaches it.
//!   * A deref `*` is refused by nothing: `%(*refname)` gains `^{}` and
//!     `%(*push)` is `missing object`, raised for whichever ref git's sort
//!     fills first.
//!
//! Every expectation was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment, stdout, stderr and
//! exit status compared separately.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const CREATOR: &str = "C O Mitter <committer@example.com> 1700000000 +0000";

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
    /// One commit on `main`, branches `b` and `c` at it, a lightweight tag
    /// `light` and an annotated tag `v1`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fer-args-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.git(&["add", "a"]);
        f.git(&["commit", "-q", "-m", "subject"]);
        f.git(&["branch", "b"]);
        f.git(&["branch", "c"]);
        f.git(&["tag", "light"]);
        f.git(&["tag", "-a", "-m", "msg", "v1"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
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

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn stdout(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!((err.as_str(), code), ("", 0), "`git {args:?}`");
        out
    }

    /// `for-each-ref <pattern> --format=<fmt>`, which must die with `fatal`.
    fn dies(&self, pattern: &str, fmt: &str, fatal: &str) {
        let (out, err, code) = self.run(&["for-each-ref", pattern, &format!("--format={fmt}")]);
        assert_eq!((out.as_str(), err.as_str(), code), ("", format!("fatal: {fatal}\n").as_str(), 128), "{fmt}");
    }

    fn oid(&self, spec: &str) -> String {
        self.stdout(&["rev-parse", spec]).trim().to_string()
    }
}

/// `%(creator)` carries no parser and is filled by `atom_type`, so its argument
/// is ignored — `%(creator:mailmap)` is the unmapped line, not a refusal. The
/// other three person atoms are accepted too but never filled, the empty
/// `:` included. All three listing verbs share the behaviour.
#[test]
fn person_atoms_accept_any_argument_and_only_creator_is_filled() {
    let f = Fixture::new("person");
    let fmt = "--format=[%(creator:mailmap)][%(author:x)][%(author:)][%(committer:mailmap)]";
    let want = format!("[{CREATOR}][][][]\n");
    assert_eq!(f.stdout(&["for-each-ref", "refs/heads/main", fmt]), want);
    assert_eq!(f.stdout(&["branch", "--list", "main", fmt]), want);

    let fmt = "--format=[%(tagger:x)][%(creator:x)]";
    let want = format!("[][{CREATOR}]\n");
    assert_eq!(f.stdout(&["for-each-ref", "refs/tags/v1", fmt]), want);
    assert_eq!(f.stdout(&["tag", "-l", "v1", fmt]), want);
}

/// `err_no_arg()` is handed the atom name as a literal, so the deref `*` is not
/// in the message; atoms with no parser (`flag`, `worktreepath`, `numparent`)
/// ignore an argument outright.
#[test]
fn no_argument_parsers_name_the_atom_and_parserless_atoms_ignore_one() {
    let f = Fixture::new("no-arg");
    let main = "refs/heads/main";
    f.dies(main, "%(objecttype:x)", "%(objecttype) does not take arguments");
    f.dies(main, "%(*objecttype:x)", "%(objecttype) does not take arguments");
    f.dies(main, "%(body:x)", "%(body) does not take arguments");
    f.dies(main, "%(HEAD:x)", "%(HEAD) does not take arguments");

    let bare = f.stdout(&["for-each-ref", main, "--format=%(flag)|%(worktreepath)|%(numparent)"]);
    let with = f.stdout(&["for-each-ref", main, "--format=%(flag:x)|%(worktreepath:x)|%(numparent:x)"]);
    assert_eq!(with, bare);
    // `main` is checked out here and its commit is a root.
    assert!(bare.starts_with("|/") && bare.ends_with("|0\n"), "{bare:?}");
}

/// `refname_atom_parser_internal()`: a count is `strtol_i()` — leading blanks
/// fine, trailing junk or a value past `int` not — and its failure message says
/// `lstrip=` even for `strip=`. An unknown modifier names the atom as written,
/// deref `*` kept, for `%(upstream)` as much as for `%(refname)`.
#[test]
fn refname_counts_are_strtol_i_with_their_own_message() {
    let f = Fixture::new("refname");
    let main = "refs/heads/main";
    f.dies(main, "%(refname:lstrip=x)", "Integer value expected refname:lstrip=x");
    f.dies(main, "%(refname:strip=1x)", "Integer value expected refname:lstrip=1x");
    f.dies(main, "%(refname:rstrip=2147483648)", "Integer value expected refname:rstrip=2147483648");
    f.dies(main, "%(upstream:rstrip=x)", "Integer value expected refname:rstrip=x");
    f.dies(main, "%(*refname:bogus)", "unrecognized %(*refname) argument: bogus");
    f.dies(main, "%(*upstream:bogus)", "unrecognized %(*upstream) argument: bogus");
    assert_eq!(f.stdout(&["for-each-ref", main, "--format=%(refname:lstrip= 1)"]), "heads/main\n");
}

/// No atom refuses the deref `*`: `%(*refname)` and `%(*symref)` append `^{}`
/// (an absent symref is `""`, still suffixed), `%(*HEAD)` is `%(HEAD)`, and the
/// container atoms keep their meaning.
#[test]
fn deref_suffixes_names_and_keeps_containers_working() {
    let f = Fixture::new("deref");
    let got = f.stdout(&[
        "for-each-ref",
        "refs/tags/v1",
        "--format=%(*refname)|%(*symref)|%(*HEAD)|%(*refname:short)",
    ]);
    assert_eq!(got, "refs/tags/v1^{}|^{}| |v1^{}\n");
    let got = f.stdout(&["for-each-ref", "refs/heads/main", "--format=%(*if)%(refname)%(*then)y%(*else)n%(*end)"]);
    assert_eq!(got, "y\n");
}

/// `oid_atom_parser` and `align_atom_parser` read numbers with `strtoul_ui()`,
/// and `align` has three messages of its own plus a `~0U` sentinel that makes
/// an explicit 4294967295 count as no width at all.
#[test]
fn oid_and_align_numbers_are_strtoul_ui() {
    let f = Fixture::new("numbers");
    let main = "refs/heads/main";
    let short = f.stdout(&["for-each-ref", main, "--format=%(objectname:short= 5)"]);
    assert_eq!(short, format!("{}\n", &f.oid("main")[..5]));
    f.dies(
        main,
        "%(objectname:short=4294967296)",
        "positive value expected '4294967296' in %(objectname:short=4294967296)",
    );
    f.dies(main, "%(align:width=x)%(end)", "unrecognized width:x");
    f.dies(main, "%(align:position=x,5)%(end)", "unrecognized position:x");
    f.dies(main, "%(align:left)%(end)", "positive width expected with the %(align) atom");
    f.dies(main, "%(align:4294967295)%(end)", "positive width expected with the %(align) atom");
}

/// `color_parse()` is the non-quiet parser, so its `error:` line comes first.
#[test]
fn a_bad_color_reports_the_parser_error_then_the_atom() {
    let f = Fixture::new("color");
    let (out, err, code) = f.run(&["for-each-ref", "--format=%(color:bogus)"]);
    assert_eq!(out, "");
    assert_eq!(err, "error: invalid color value: bogus\nfatal: unrecognized color: %(color:bogus)\n");
    assert_eq!(code, 128);
}

/// `match_atom_bool_arg()` is the full `git_parse_maybe_bool()`, and
/// `describe:abbrev` tests the sign before full consumption.
#[test]
fn describe_booleans_and_abbrev_follow_git_parsers() {
    let f = Fixture::new("describe");
    let got = f.stdout(&[
        "for-each-ref",
        "refs/heads/main",
        "--format=%(describe:tags=on)|%(describe:tags=2)|%(describe:tags=)",
    ]);
    assert_eq!(got, "v1|v1|v1\n");
    f.dies("refs/heads/main", "%(describe:abbrev=-5x)", "positive value expected describe:abbrev=-5x");
}

/// The date format is parsed when a value is filled: a run whose objects never
/// fill the atom succeeds, an iterated `tag` listing writes the lines before the
/// tag that dies, a sorted listing fills everything before its first line, and
/// the fill happens ahead of a formatting-stack error.
#[test]
fn a_date_format_is_judged_only_when_a_value_is_filled() {
    let f = Fixture::new("date");
    f.dies("refs/heads/main", "%(authordate:)", "unknown date format ");

    let heads = f.stdout(&["for-each-ref", "refs/heads", "--format=%(taggerdate:bogus)%(refname)"]);
    assert_eq!(heads, "refs/heads/b\nrefs/heads/c\nrefs/heads/main\n");

    let (out, err, code) = f.run(&["tag", "-l", "--format=%(refname)%(taggerdate:bogus)"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("refs/tags/light\n", "fatal: unknown date format bogus\n", 128));

    let (out, err, code) = f.run(&[
        "for-each-ref",
        "refs/tags",
        "--sort=objectname",
        "--format=%(refname)%(taggerdate:bogus)",
    ]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "fatal: unknown date format bogus\n", 128));

    f.dies("refs/heads/main", "%(then)%(taggerdate:bogus)", "format: %(then) atom used without a %(if) atom");
    f.dies("refs/tags/v1", "%(then)%(taggerdate:bogus)", "unknown date format bogus");
}

/// `%(*push)` is `missing object` for every ref, so the ref it names is the one
/// filled first: the first iterated for `for-each-ref`, and for `branch` — which
/// always sorts — the left side of `git_qsort_s()`'s first comparison, which on
/// three branches is the second.
#[test]
fn deref_push_names_the_ref_filled_first() {
    let f = Fixture::new("push");
    let oid = f.oid("main");
    f.dies("refs/heads", "%(*push)", &format!("missing object {oid} for refs/heads/b"));
    let (out, err, code) = f.run(&["branch", "--format=%(*push)"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", format!("fatal: missing object {oid} for refs/heads/c\n").as_str(), 128)
    );
}
