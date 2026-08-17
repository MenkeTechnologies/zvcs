//! `git replace` and `git prune` handed a well-formed object id the repository
//! does not have.
//!
//! `get_oid_basic()` (`object-name.c`) resolves a name of exactly `hexsz` hex
//! digits by decoding it, and returns *before* the object database is consulted.
//! So `<40 hex>` naming an absent object is not a resolution failure in git — it
//! is a successful resolution to an object that happens to be missing, and every
//! command carries on to whatever it does next.
//!
//! Resolving through the odb alone (gitoxide's `rev_parse_single()`) collapses
//! the two and makes each of these commands report the wrong thing:
//!
//! ```text
//! git replace <absent> HEAD          type check   ->  '(null)' vs 'commit'
//! git replace HEAD <absent>          ref write    ->  nonexistent object
//! git replace -f <absent> HEAD       succeeds, writing refs/replace/<absent>
//! git replace -d <absent>            replace ref '<absent>' not found
//! git replace --graft HEAD <absent>  could not parse <absent> as a commit
//! git replace --edit <absent>        unable to get object type for <absent>
//! git prune <absent>                 unable to parse object: <absent>
//! ```
//!
//! Every expectation below is the verbatim output of stock git 2.55.0 for the
//! same argv in the same fixture. Each case is paired with a control — a name
//! that does not resolve at all (`nosuchthing`) or a 39-digit hex string, which
//! is one digit short of the rule and therefore falls through to the ordinary
//! revspec parser — so a "fix" that simply reports the missing-object message
//! for *everything* fails just as loudly as the original bug did.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A well-formed SHA-1 object name that no repository here contains.
const ABSENT: &str = "0123456789012345678901234567890123456789";
/// The same, in the case `get_oid_hex()` also accepts and `ObjectId::from_hex`
/// does not.
const ABSENT_UPPER: &str = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
/// One digit short of `hexsz`, so `get_oid_basic()`'s first branch declines it.
const SHORT_39: &str = "012345678901234567890123456789012345678";
/// A name that resolves to nothing at all, by any rule.
const NOSUCH: &str = "nosuchthing";

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    /// The unreachable blob written at setup — `prune` must not remove it while
    /// failing on its arguments.
    dangling: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One commit, plus one loose blob nothing refers to.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-objname-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("r");
        std::fs::create_dir_all(&repo).unwrap();
        let mut f = Fixture { root, repo, dangling: String::new() };

        f.setup(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.repo.join("f"), "hello\n").unwrap();
        f.setup(&["add", "f"]);
        f.setup(&["commit", "-q", "-m", "one"]);

        std::fs::write(f.root.join("blob"), "dangling\n").unwrap();
        let out = f
            .cmd(&["hash-object", "-w", f.root.join("blob").to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "hash-object failed: {out:?}");
        f.dangling = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        assert_eq!(f.dangling.len(), 40, "hash-object printed {:?}", f.dangling);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "zvcs test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "zvcs test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_EDITOR", "true")
            .env("EDITOR", "true")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn setup(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn head(&self) -> String {
        self.run(&["rev-parse", "HEAD"]).1.trim().to_owned()
    }

    /// Every ref as `<name> <value>`, sorted — so a ref written on the wrong
    /// path is visible even when the exit code happens to be right.
    fn refs(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .run(&["for-each-ref", "--format=%(refname) %(objectname)"])
            .1
            .lines()
            .map(str::to_owned)
            .collect();
        v.sort();
        v
    }

    /// Every loose object path under `.git/objects`, sorted.
    fn loose(&self) -> Vec<String> {
        let mut v = Vec::new();
        collect(&self.repo.join(".git/objects"), &self.repo.join(".git/objects"), &mut v);
        v.sort();
        v
    }
}

fn collect(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect(base, &path, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push(rel.display().to_string());
        }
    }
}

// ---------------------------------------------------------------------------
// git replace
// ---------------------------------------------------------------------------

/// The absent id resolves, so `replace_object_oid()` reaches its type check and
/// `type_name(OBJ_BAD)` renders the literal `(null)` git's `%s` of a NULL
/// pointer produces. All three lines come from one `error()`, so only the first
/// carries the prefix.
#[test]
fn replace_absent_object_reaches_the_type_check() {
    let f = Fixture::new("type-check");
    let (code, out, err) = f.run(&["replace", ABSENT, "HEAD"]);
    assert_eq!(
        err,
        format!(
            "error: Objects must be of the same type.\n\
             '{ABSENT}' points to a replaced object of type '(null)'\n\
             while 'HEAD' points to a replacement object of type 'commit'.\n"
        )
    );
    assert_eq!(code, 255, "stdout: {out}");
    assert_eq!(f.refs().len(), 1, "no ref may be written: {:?}", f.refs());
}

/// The same rule on the replacement operand, which reports the two types the
/// other way round.
#[test]
fn replace_absent_replacement_reaches_the_type_check() {
    let f = Fixture::new("type-check-2nd");
    let (code, _, err) = f.run(&["replace", "HEAD", ABSENT]);
    assert_eq!(
        err,
        format!(
            "error: Objects must be of the same type.\n\
             'HEAD' points to a replaced object of type 'commit'\n\
             while '{ABSENT}' points to a replacement object of type '(null)'.\n"
        )
    );
    assert_eq!(code, 255);
}

/// `get_oid_hex()` runs through `hexval()`, which is case-insensitive, and the
/// operand is echoed back in the case it was typed while the id itself is
/// lowercase.
#[test]
fn replace_absent_object_accepts_uppercase_hex() {
    let f = Fixture::new("upper");
    let (code, _, err) = f.run(&["replace", ABSENT_UPPER, "HEAD"]);
    assert_eq!(
        err,
        format!(
            "error: Objects must be of the same type.\n\
             '{ABSENT_UPPER}' points to a replaced object of type '(null)'\n\
             while 'HEAD' points to a replacement object of type 'commit'.\n"
        )
    );
    assert_eq!(code, 255);
}

/// The controls. A name that resolves by no rule, and a hex string one digit
/// short of `hexsz`, are both plain resolution failures — the message the bug
/// used to produce for the absent-but-well-formed id.
#[test]
fn replace_unresolvable_names_still_fail_to_resolve() {
    let f = Fixture::new("control");
    for name in [NOSUCH, SHORT_39] {
        let (code, _, err) = f.run(&["replace", name, "HEAD"]);
        assert_eq!(err, format!("error: failed to resolve '{name}' as a valid ref\n"));
        assert_eq!(code, 255, "{name}");
    }
}

/// `-f` skips the type check, and the object being *replaced* never has to
/// exist: the ref is named after it. This is the case that silently did nothing
/// before, and it is the one that proves the resolver returns the id rather than
/// an error.
#[test]
fn replace_force_writes_a_ref_for_an_absent_object() {
    let f = Fixture::new("force");
    let head = f.head();
    let (code, out, err) = f.run(&["replace", "-f", ABSENT, "HEAD"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
    assert!(
        f.refs().contains(&format!("refs/replace/{ABSENT} {head}")),
        "refs: {:?}",
        f.refs()
    );
    assert_eq!(f.run(&["replace", "-l"]).1, format!("{ABSENT}\n"));
}

/// The replacement, in contrast, must exist: `ref_transaction_update()` refuses
/// to point a ref at an object the repository does not have. Reaching that check
/// at all depends on the absent id resolving first.
#[test]
fn replace_force_refuses_a_nonexistent_replacement() {
    let f = Fixture::new("force-2nd");
    let head = f.head();
    let before = f.refs();
    let (code, _, err) = f.run(&["replace", "-f", "HEAD", ABSENT]);
    assert_eq!(
        err,
        format!("error: trying to write ref 'refs/replace/{head}' with nonexistent object {ABSENT}\n")
    );
    assert_eq!(code, 255);
    assert_eq!(f.refs(), before, "the ref must not have been written");
}

/// `-d` resolves the name and then looks for the ref, so an absent id gets the
/// "not found" report — and the control gets the resolution failure. Both exit
/// 1, which is why the message is the only thing that distinguishes them.
#[test]
fn replace_delete_distinguishes_absent_from_unresolvable() {
    let f = Fixture::new("delete");
    let (code, _, err) = f.run(&["replace", "-d", ABSENT]);
    assert_eq!(err, format!("error: replace ref '{ABSENT}' not found\n"));
    assert_eq!(code, 1);

    let (code, _, err) = f.run(&["replace", "-d", NOSUCH]);
    assert_eq!(err, format!("error: failed to resolve '{NOSUCH}' as a valid ref\n"));
    assert_eq!(code, 1);
}

/// `create_graft()`'s own operand: resolved, then `lookup_commit_reference()`
/// returns NULL, which is `could not parse %s` — without `as a commit`, unlike
/// the parent form below.
#[test]
fn replace_graft_absent_commit_could_not_parse() {
    let f = Fixture::new("graft");
    let (code, _, err) = f.run(&["replace", "--graft", ABSENT]);
    assert_eq!(err, format!("error: could not parse {ABSENT}\n"));
    assert_eq!(code, 255);

    let (code, _, err) = f.run(&["replace", "--graft", NOSUCH]);
    assert_eq!(err, format!("error: not a valid object name: '{NOSUCH}'\n"));
    assert_eq!(code, 255);
}

/// `replace_parents()` reports with `error()` and `create_graft()` propagates
/// its -1, so both parent rejections are 255 — not a `die()` at 128.
#[test]
fn replace_graft_parent_rejections_are_errors_not_fatal() {
    let f = Fixture::new("graft-parent");
    let (code, _, err) = f.run(&["replace", "--graft", "HEAD", ABSENT]);
    assert_eq!(err, format!("error: could not parse {ABSENT} as a commit\n"));
    assert_eq!(code, 255);

    let (code, _, err) = f.run(&["replace", "--graft", "HEAD", NOSUCH]);
    assert_eq!(err, format!("error: not a valid object name: '{NOSUCH}'\n"));
    assert_eq!(code, 255);
}

/// `--edit` resolves, then asks for the type — which is where an absent object
/// stops, before the scratch file is created or the editor is run.
#[test]
fn replace_edit_absent_object_stops_at_the_type_lookup() {
    let f = Fixture::new("edit");
    let (code, _, err) = f.run(&["replace", "--edit", ABSENT]);
    assert_eq!(err, format!("error: unable to get object type for {ABSENT}\n"));
    assert_eq!(code, 255);
    assert!(
        !f.repo.join(".git/REPLACE_EDITOBJ").exists(),
        "the scratch file must not be created"
    );
}

/// `warning(_("could not convert the following graft(s):\n%s"), err.buf)` — the
/// format string's newline plus the one each accumulated line starts with put a
/// blank line before the first failure.
#[test]
fn convert_graft_file_reports_failures_after_a_blank_line() {
    let f = Fixture::new("cgf");
    let head = f.head();
    let line = format!("{ABSENT} {head}");
    std::fs::create_dir_all(f.repo.join(".git/info")).unwrap();
    std::fs::write(f.repo.join(".git/info/grafts"), format!("{line}\n")).unwrap();

    let (code, _, err) = f.run(&["replace", "--convert-graft-file"]);
    assert_eq!(
        err,
        format!(
            "error: could not parse {ABSENT}\n\
             warning: could not convert the following graft(s):\n\
             \n\t{line}\n"
        )
    );
    assert_eq!(code, 1);
    assert!(
        f.repo.join(".git/info/grafts").exists(),
        "a graft file with an unconverted line must survive"
    );
}

// ---------------------------------------------------------------------------
// git prune
// ---------------------------------------------------------------------------

/// `cmd_prune()` splits the two failures across two calls: `repo_get_oid()`
/// decides `unrecognized argument`, and `parse_object_or_die()` — which only
/// runs once the name *has* resolved — decides `unable to parse object`. An
/// absent full-length hex id reaches the second, never the first.
#[test]
fn prune_absent_object_is_unable_to_parse() {
    let f = Fixture::new("prune-absent");
    let before = f.loose();
    let (code, out, err) = f.run(&["prune", ABSENT]);
    assert_eq!(err, format!("fatal: unable to parse object: {ABSENT}\n"));
    assert_eq!((code, out.as_str()), (128, ""));
    // The argument loop runs before anything is unlinked: the unreachable blob
    // is still there, which is the difference between a wrong message and a
    // wrong repository.
    assert_eq!(f.loose(), before, "prune must not have touched the odb");
    assert!(
        f.loose().iter().any(|p| p.replace('/', "") == f.dangling),
        "the dangling blob {} must survive: {:?}",
        f.dangling,
        f.loose()
    );
}

/// The controls: a name that resolves by no rule, and a 39-digit hex string,
/// both stop at `repo_get_oid()` with the other message — and equally leave the
/// object store alone.
#[test]
fn prune_unresolvable_names_are_unrecognized_arguments() {
    for (tag, name) in [("prune-ctl", NOSUCH), ("prune-39", SHORT_39)] {
        let f = Fixture::new(tag);
        let before = f.loose();
        let (code, out, err) = f.run(&["prune", name]);
        assert_eq!(err, format!("fatal: unrecognized argument: {name}\n"));
        assert_eq!((code, out.as_str()), (128, ""), "{name}");
        assert_eq!(f.loose(), before, "prune must not have touched the odb");
    }
}

/// `--dry-run` changes nothing about argument handling — the die happens before
/// the flag could matter — and `--` does not turn the operand into something
/// other than an object name.
#[test]
fn prune_absent_object_fails_the_same_under_dry_run_and_after_dashdash() {
    let f = Fixture::new("prune-variants");
    for args in [vec!["prune", "-n", ABSENT], vec!["prune", "--", ABSENT]] {
        let (code, out, err) = f.run(&args);
        assert_eq!(err, format!("fatal: unable to parse object: {ABSENT}\n"), "{args:?}");
        assert_eq!((code, out.as_str()), (128, ""), "{args:?}");
    }
}

/// And a name that resolves to an object the repository *does* have still works:
/// the guard above must not have turned every operand into a failure. The blob
/// is unreachable from `HEAD`, so this run removes it — which also proves the
/// argument was accepted as a traversal root rather than ignored.
#[test]
fn prune_with_a_real_head_still_prunes() {
    let f = Fixture::new("prune-ok");
    let dangling = f.dangling.clone();
    let (code, out, err) = f.run(&["prune", "HEAD"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
    assert!(
        !f.loose().iter().any(|p| p.replace('/', "") == dangling),
        "the unreachable blob should have been pruned: {:?}",
        f.loose()
    );
    assert_eq!(f.run(&["rev-parse", "HEAD"]).0, 0, "HEAD must still resolve");
}
