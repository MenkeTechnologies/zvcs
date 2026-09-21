//! Four places `git stash` refuses, reports, or restores differently than a
//! naive reading of the subcommand suggests.
//!
//! * `--index` is not "replay the stash's `I` tree". `do_apply_stash()` applies
//!   the stash's staged patch *onto the current index*, records the result, and
//!   then runs `reset_head()` — `git reset --quiet --refresh`, a mixed reset
//!   (builtin/stash.c:678-693). Two consequences are observable:
//!   - the index the merge then meets is `HEAD`'s, so `unclean()`
//!     (merge-ort-wrappers.c:15-28) compares it against `c_tree` and refuses the
//!     whole apply the moment anything at all was staged. A `pop` that refuses
//!     keeps its entry; one that wrongly succeeds deletes it.
//!   - what comes back staged is that recorded tree, not `i_tree`. With `HEAD`
//!     moved on since the stash was taken they are different trees, and staging
//!     `i_tree` rolls the index back over the newer commit.
//! * `-q` reaches the merge, not just the trailing `git status`:
//!   `if (quiet) o.verbosity = 0` (builtin/stash.c:705-706) and
//!   `show_msgs = !!opt->verbosity` (merge-ort-wrappers.c:45).
//! * A valueless option handed `=<value>` is `` option `x' takes no value ``
//!   with no usage block (parse-options.c:138-143), which is a different exit
//!   path from an unknown option.
//! * An unrecognised first word is not a subcommand error: `cmd_stash` hands it
//!   to `push_stash()` with `push_assumed` (builtin/stash.c:2500-2510), and
//!   push's `PARSE_OPT_STOP_AT_NON_OPTION` is what refuses it.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-stashparity-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.write("a.txt", "base\n");
        f.write("b.txt", "b\n");
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.work.join(name)).unwrap()
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
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

    fn status(&self) -> Vec<String> {
        let out = self.cmd(&["status", "--porcelain"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.contains(".zvcs/"))
            .map(str::to_string)
            .collect()
    }

    fn stash_list(&self) -> Vec<String> {
        let out = self.cmd(&["stash", "list"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
    }

    /// A stash holding one *staged* edit to `a.txt`, so `i_tree` differs from
    /// the base and `--index` has something to restore.
    fn stash_a_staged(&self) {
        self.write("a.txt", "base\nstashed\n");
        self.git(&["add", "a.txt"]);
        self.git(&["stash", "push", "-q", "-m", "s"]);
    }
}

/// `pop --index` over a dirty index must refuse *and keep the entry*. The
/// mixed reset inside the `--index` arm leaves the index at `HEAD`, and
/// `unclean()` then names every path `c_tree` disagrees with — space-separated
/// on one indented line, which is the `%s` in merge-ort-wrappers.c:22.
#[test]
fn pop_with_index_refuses_over_staged_work_and_keeps_the_entry() {
    let f = Fixture::new("uncleanpop");
    f.stash_a_staged();
    f.write("b.txt", "b\nstaged\n");
    f.git(&["add", "b.txt"]);

    let (code, _out, err) = f.run(&["stash", "pop", "--index", "-q"]);
    assert_eq!(code, 1, "a refused pop exits 1: {err}");
    assert!(
        err.contains(
            "error: Your local changes to the following files would be overwritten by merge:\n  b.txt\n"
        ),
        "unclean() names the staged path on one indented line: {err:?}"
    );
    assert!(err.contains("Index was not unstashed.\n"), "{err:?}");
    assert_eq!(
        f.stash_list().len(),
        1,
        "a pop that refused must not drop the entry it could not apply"
    );
    // `a.txt` must not have been touched: the refusal happens before the merge.
    assert_eq!(f.read("a.txt"), "base\n", "the worktree must be untouched");
    assert_eq!(
        f.status(),
        vec![" M b.txt".to_string()],
        "reset_head() really ran: the caller's staged edit is back in the worktree only"
    );
}

/// Several staged paths come back on the same line, in path order.
#[test]
fn unclean_lists_every_staged_path_on_one_line() {
    let f = Fixture::new("uncleanmany");
    f.write("c.txt", "c\n");
    f.git(&["add", "c.txt"]);
    f.git(&["commit", "-q", "-m", "c"]);
    f.stash_a_staged();
    f.write("b.txt", "b2\n");
    f.write("c.txt", "c2\n");
    f.git(&["add", "b.txt", "c.txt"]);

    let (code, _out, err) = f.run(&["stash", "apply", "--index", "-q"]);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("would be overwritten by merge:\n  b.txt c.txt\n"),
        "one `%s`, so the paths are space-joined rather than one per line: {err:?}"
    );
}

/// With `HEAD` moved on since the stash was taken, `--index` stages the tree
/// `apply_cached()` produced — the new commit's content with the stash's staged
/// patch on top — not the stash's own `i_tree`, which still names the old
/// content of every path the newer commit touched.
#[test]
fn apply_with_index_stages_the_patched_tree_not_the_stashs_index_tree() {
    let f = Fixture::new("movedhead");
    f.stash_a_staged();
    f.write("b.txt", "b\nsecond\n");
    f.git(&["add", "b.txt"]);
    f.git(&["commit", "-q", "-m", "second"]);

    let (code, out, err) = f.run(&["stash", "apply", "--index", "-q"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        f.status(),
        vec!["M  a.txt".to_string()],
        "only the stashed path may be staged; `b.txt` must stay at the commit that introduced it"
    );
    assert_eq!(f.read("b.txt"), "b\nsecond\n", "the newer commit's content must survive");
}

/// `-q` takes merge-ort's own conflict block with it, not just the trailing
/// `git status`. The conflict itself still lands in the worktree.
#[test]
fn quiet_apply_silences_the_merge_block_but_still_conflicts() {
    /// A stash whose single hunk collides with what `HEAD` since grew.
    fn conflicting(tag: &str) -> Fixture {
        let f = Fixture::new(tag);
        f.write("a.txt", "one\ntwo\nthree\n");
        f.git(&["commit", "-q", "-am", "lines"]);
        f.write("a.txt", "one\nSTASH\nthree\n");
        f.git(&["stash", "push", "-q", "-m", "s"]);
        f.write("a.txt", "one\nHEAD\nthree\n");
        f.git(&["commit", "-q", "-am", "other"]);
        f
    }

    let f = conflicting("quiet");
    let (code, out, err) = f.run(&["stash", "apply", "-q"]);
    assert_eq!(code, 1, "a conflicted apply exits 1");
    assert_eq!(out, "", "nothing on stdout under -q: {out:?}");
    assert_eq!(err, "", "`Auto-merging`/`CONFLICT` are verbosity-gated: {err:?}");
    assert!(
        f.status().iter().any(|l| l.starts_with("UU ")),
        "the conflict must still be recorded: {:?}",
        f.status()
    );

    // The same apply without -q reports the merge it just made silently.
    let f = conflicting("verbose");
    let (code, out, _err) = f.run(&["stash", "apply"]);
    assert_eq!(code, 1);
    assert!(out.contains("CONFLICT (content): Merge conflict in a.txt"), "{out:?}");
}

/// `--<bool>=<value>` is `takes no value`, one line and exit 129 with no usage
/// block — `optname()` quotes the table entry's full name even when an
/// abbreviation was typed, and keeps the `no-` of a negated spelling.
#[test]
fn valueless_options_refuse_an_attached_value_without_printing_usage() {
    let f = Fixture::new("noarg");
    for (sub, typed, named) in [
        ("push", "--include-untracked=only", "include-untracked"),
        ("push", "--incl=x", "include-untracked"),
        ("push", "--no-all=1", "no-all"),
        ("push", "--keep-index=1", "keep-index"),
        ("save", "--all=1", "all"),
        ("save", "--quiet=x", "quiet"),
    ] {
        let (code, out, err) = f.run(&["stash", sub, typed]);
        assert_eq!(code, 129, "`stash {sub} {typed}` must exit 129: {out}{err}");
        assert_eq!(
            err,
            format!("error: option `{named}' takes no value\n"),
            "`stash {sub} {typed}` names the table entry and prints nothing else"
        );
        assert_eq!(out, "", "the refusal is stderr-only: {out:?}");
    }

    // An option that really takes a value is unaffected, and an unknown option
    // still gets the usage block it always got.
    let (code, _out, err) = f.run(&["stash", "push", "--zzbogus"]);
    assert_eq!(code, 129);
    assert!(err.starts_with("error: unknown option `zzbogus'\n"), "{err:?}");
    assert!(err.contains("usage: git stash [push]"), "an unknown option keeps its usage: {err:?}");
}

/// An unrecognised first word is push's refusal, not a subcommand lookup
/// failure — and under `--patch` the very same word is a pathspec.
#[test]
fn an_unknown_first_word_is_refused_by_assumed_push() {
    let f = Fixture::new("assumed");
    let (code, out, err) = f.run(&["stash", "bogus"]);
    assert_eq!(code, 128, "{out}{err}");
    assert_eq!(
        err,
        "fatal: subcommand wasn't specified; 'push' can't be assumed due to unexpected token 'bogus'\n",
        "the word reaches push_stash() with push_assumed set"
    );

    // `--patch` sets `force_assume`, so the same token is taken as a pathspec
    // and the failure is the pathspec's, with git's wording.
    let (code, _out, err) = f.run(&["stash", "-p", "bogus"]);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("did not match any file(s) known to git"),
        "with --patch the token is a pathspec: {err:?}"
    );
}
