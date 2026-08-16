//! Which `(tree count, --prefix, -m, --reset)` combinations `read-tree` accepts,
//! and what each one does.
//!
//! `--prefix` is not a mode. `cmd_read_tree` sets `stage = opts.merge = 1` for it
//! exactly as it does for `-m` and `--reset` (builtin/read-tree.c:202-206), and then
//! the *tree count alone* picks the unpack function (builtin/read-tree.c:237-259):
//!
//! | trees | `--prefix` absent | `--prefix` present |
//! |-------|-------------------|--------------------|
//! | 0     | plain read (empties the index, with a deprecation warning) | `you must specify at least one tree to merge` |
//! | 1     | plain read / `oneway_merge` under `-m`,`--reset` | `bind_merge` |
//! | 2     | plain union read / `twoway_merge` under `-m`,`--reset` | `twoway_merge` |
//! | 3+    | plain union read / `threeway_merge` under `-m`,`--reset` | `threeway_merge` |
//! | 9+    | `I cannot read more than 8 trees` | same |
//!
//! `opts.prefix` is read once outside that switch — to choose `bind_merge` over
//! `oneway_merge` at one tree. Everywhere else it is only the traversal base
//! (`setup_traverse_info`, tree-walk.c:192-205), so a two- or three-tree merge with
//! `--prefix=<p>` is the ordinary two- or three-tree merge with every tree path read
//! at `<p>/<path>` and every index entry outside `<p>/` reaching the merge function
//! with all tree slots absent.
//!
//! `-m`, `--reset` and `--prefix` remain mutually exclusive, which is the only thing
//! the "Which one?" check enforces — not a tree count.
//!
//! Every expectation here was measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A prefix whose value is a single TAB: not a directory name anyone would type,
/// and the shape that broke the port — the fuzzer mutates `--prefix=sub/` to it.
const TAB: &str = "--prefix=\t";

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
    /// Three tags over two branches, chosen so the two-tree cases can be driven
    /// into each of `twoway_merge`'s arms without touching the index:
    ///
    /// * `base`   — `a.txt`, `b.txt`
    /// * `ours`   — `base` plus `c.txt` (an add-only step, so `base`→`ours` merges)
    /// * `theirs` — `base` with `b.txt` rewritten (a carry-forward conflict)
    ///
    /// `HEAD` is `ours`, so the index holds `a.txt`, `b.txt`, `c.txt`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rtprefix-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("a.txt", "a0\n");
        f.write("b.txt", "b0\n");
        f.git(&["add", "a.txt", "b.txt"]);
        f.git(&["commit", "-q", "-m", "c0"]);
        f.git(&["tag", "base"]);
        f.write("c.txt", "c0\n");
        f.git(&["add", "c.txt"]);
        f.git(&["commit", "-q", "-m", "c1"]);
        f.git(&["tag", "ours"]);
        f.git(&["checkout", "-q", "-b", "feature", "base"]);
        f.write("b.txt", "b1\n");
        f.git(&["add", "b.txt"]);
        f.git(&["commit", "-q", "-m", "c2"]);
        f.git(&["tag", "theirs"]);
        f.git(&["checkout", "-q", "main"]);
        f
    }

    fn write(&self, path: &str, body: &str) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }

    fn git(&self, args: &[&str]) {
        let out = self.run(args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// `read-tree` with `args`, as `(exit code, stderr)`.
    fn read_tree(&self, args: &[&str]) -> (i32, String) {
        let mut argv = vec!["read-tree"];
        argv.extend_from_slice(args);
        let out = self.run(&argv);
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Every index path, in index order. Read with `-z` so a path holding a TAB
    /// arrives raw instead of through `ls-files`' C-quoting.
    fn paths(&self) -> Vec<String> {
        self.stage_lines().into_iter().map(|(_, p)| p).collect()
    }

    /// `(oid, path)` for every index entry, in index order.
    fn stage_lines(&self) -> Vec<(String, String)> {
        let out = self.run(&["ls-files", "-s", "-z"]);
        assert!(out.status.success(), "`ls-files -s -z` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|r| !r.is_empty())
            .map(|r| {
                let (meta, path) = r.split_once('\t').expect("ls-files -s separates with a TAB");
                (meta.split(' ').nth(1).expect("<mode> <oid> <stage>").to_string(), path.to_string())
            })
            .collect()
    }

    fn oid_of(&self, path: &str) -> String {
        self.stage_lines()
            .into_iter()
            .find(|(_, p)| p == path)
            .unwrap_or_else(|| panic!("no index entry for {path:?}"))
            .0
    }
}

// ---------------------------------------------------------------------------
// One tree: `bind_merge`, and what the prefix value itself means
// ---------------------------------------------------------------------------

#[test]
fn one_tree_binds_under_the_prefix() {
    let f = Fixture::new("bind");
    assert_eq!(f.read_tree(&["--prefix=sub/", "ours"]), (0, String::new()));
    assert_eq!(f.paths(), ["a.txt", "b.txt", "c.txt", "sub/a.txt", "sub/b.txt", "sub/c.txt"]);
}

/// `setup_traverse_info()` drops one trailing `/` and `make_traverse_path()` puts
/// one back, so a prefix without the separator names the same directory.
#[test]
fn a_prefix_without_a_trailing_slash_gains_one() {
    let f = Fixture::new("noslash");
    assert_eq!(f.read_tree(&["--prefix=sub", "ours"]), (0, String::new()));
    assert_eq!(f.paths(), ["a.txt", "b.txt", "c.txt", "sub/a.txt", "sub/b.txt", "sub/c.txt"]);
}

/// The separator is inserted whatever the prefix says — a TAB is a directory name
/// like any other, and it sorts ahead of every letter.
#[test]
fn a_tab_prefix_is_a_directory_name() {
    let f = Fixture::new("tab-bind");
    assert_eq!(f.read_tree(&[TAB, "ours"]), (0, String::new()));
    assert_eq!(f.paths(), ["\t/a.txt", "\t/b.txt", "\t/c.txt", "a.txt", "b.txt", "c.txt"]);
}

/// An empty prefix leaves `info->pathlen` at 0, so no separator is inserted at
/// all and the tree binds over the index at its own paths — which overlaps.
#[test]
fn an_empty_prefix_overlaps_the_index() {
    let f = Fixture::new("empty-prefix");
    let (code, err) = f.read_tree(&["--prefix=", "ours"]);
    assert_eq!(code, 128);
    assert_eq!(err, "error: Entry 'a.txt' overlaps with 'a.txt'.  Cannot bind.\n");
    assert_eq!(f.paths(), ["a.txt", "b.txt", "c.txt"], "the index is left alone");
}

/// "Prefix should not start with a directory separator" sits between the mode
/// conflict and the lock (builtin/read-tree.c:181-183), so it outranks the
/// unresolvable tree-ish that would otherwise be diagnosed first.
#[test]
fn an_absolute_prefix_is_refused_before_the_tree_is_resolved() {
    let f = Fixture::new("abs-prefix");
    let (code, err) = f.read_tree(&["--prefix=/abs", "does-not-exist"]);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: Invalid prefix, prefix cannot start with '/'\n");
    assert_eq!(f.paths(), ["a.txt", "b.txt", "c.txt"]);
}

// ---------------------------------------------------------------------------
// Two trees with `--prefix`: `twoway_merge` over the prefixed paths
// ---------------------------------------------------------------------------

/// The regression this file exists for. Two trees under `--prefix` is not a bind
/// merge and is not an error: it is the ordinary two-tree merge, read under the
/// prefix. `base`→`ours` only adds `c.txt`, so `sub/c.txt` is `merged_entry`'d in
/// while `sub/a.txt` and `sub/b.txt` (unchanged between the trees) contribute
/// nothing, and every index entry carries forward untouched.
#[test]
fn two_trees_with_a_prefix_run_the_twoway_merge() {
    let f = Fixture::new("two-add");
    assert_eq!(f.read_tree(&["--prefix=sub/", "base", "ours"]), (0, String::new()));
    assert_eq!(f.paths(), ["a.txt", "b.txt", "c.txt", "sub/c.txt"]);
    assert_eq!(f.oid_of("sub/c.txt"), f.oid_of("c.txt"), "the new tree's blob, bound under sub/");
}

#[test]
fn two_trees_with_a_prefix_and_u_write_the_prefixed_path() {
    let f = Fixture::new("two-add-u");
    assert_eq!(f.read_tree(&["--prefix=sub/", "-u", "base", "ours"]), (0, String::new()));
    assert_eq!(f.paths(), ["a.txt", "b.txt", "c.txt", "sub/c.txt"]);
    assert_eq!(std::fs::read_to_string(f.work.join("sub/c.txt")).unwrap(), "c0\n");
}

/// `base`→`theirs` rewrites `b.txt`, and nothing in the index sits at `sub/b.txt`
/// to carry it forward, so `twoway_merge` refuses the whole read.
#[test]
fn two_trees_with_a_prefix_reject_a_carry_forward_conflict() {
    let f = Fixture::new("two-conflict");
    let (code, err) = f.read_tree(&["--prefix=sub/", "base", "theirs"]);
    assert_eq!(code, 128);
    assert_eq!(err, "error: Entry 'sub/b.txt' would be overwritten by merge. Cannot merge.\n");
    assert_eq!(f.paths(), ["a.txt", "b.txt", "c.txt"], "the index is left alone");
}

/// The refusal names the *prefixed* path, so an odd prefix has to survive into
/// the message rather than being normalised out of it.
#[test]
fn a_tab_prefix_reaches_the_two_tree_refusal() {
    let f = Fixture::new("tab-conflict");
    let (code, err) = f.read_tree(&[TAB, "base", "theirs"]);
    assert_eq!(code, 128);
    assert_eq!(err, "error: Entry '\t/b.txt' would be overwritten by merge. Cannot merge.\n");
}

/// The identical-trees case the fuzzer found: every prefixed path is unchanged
/// between the two trees, so the merge contributes nothing and the index survives
/// as it was. Exit 0 with no output — which is what made the old refusal visible.
#[test]
fn two_identical_trees_with_a_prefix_leave_the_index_alone() {
    let f = Fixture::new("two-same");
    assert_eq!(f.read_tree(&[TAB, "ours", "ours"]), (0, String::new()));
    assert_eq!(f.paths(), ["a.txt", "b.txt", "c.txt"]);
}

// ---------------------------------------------------------------------------
// Three trees with `--prefix`: `threeway_merge` over the prefixed paths
// ---------------------------------------------------------------------------

/// With the index emptied there is nothing outside the prefix to object, so the
/// three-way merge resolves each prefixed path and installs it.
#[test]
fn three_trees_with_a_prefix_run_the_threeway_merge() {
    let f = Fixture::new("three");
    f.git(&["read-tree", "--empty"]);
    assert_eq!(f.read_tree(&["--prefix=sub/", "base", "ours", "theirs"]), (0, String::new()));
    assert_eq!(f.paths(), ["sub/a.txt", "sub/b.txt", "sub/c.txt"]);
}

/// With entries outside the prefix, `threeway_merge` sees an index slot whose head
/// slot is absent and refuses — the first path in sort order, `a.txt`, not one of
/// the prefixed ones.
#[test]
fn three_trees_with_a_prefix_refuse_an_index_outside_it() {
    let f = Fixture::new("three-idx");
    let (code, err) = f.read_tree(&["--prefix=sub/", "base", "ours", "theirs"]);
    assert_eq!(code, 128);
    assert_eq!(err, "error: Entry 'a.txt' would be overwritten by merge. Cannot merge.\n");
    assert_eq!(f.paths(), ["a.txt", "b.txt", "c.txt"]);
}

// ---------------------------------------------------------------------------
// What the tree count and the mode flags really gate
// ---------------------------------------------------------------------------

/// The one thing "Which one?" enforces: the three modes are mutually exclusive.
/// It says nothing about how many trees each may take.
#[test]
fn prefix_and_m_are_still_a_mode_conflict() {
    let f = Fixture::new("mode-clash");
    let (code, err) = f.read_tree(&["-m", "--prefix=sub/", "ours"]);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: Which one? -m, --reset, or --prefix?\n");
}

/// `--prefix` sets `opts.merge`, so the zero-tree case is the merge refusal
/// rather than the plain read's deprecation warning.
#[test]
fn prefix_with_no_tree_is_a_merge_refusal() {
    let f = Fixture::new("no-tree");
    let (code, err) = f.read_tree(&["--prefix=sub/"]);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: you must specify at least one tree to merge\n");
}

/// `list_tree()` refuses the ninth tree (`MAX_UNPACK_TREES`, unpack-trees.h:10),
/// and does so after that argument's *name* resolved — so an unresolvable ninth
/// is diagnosed as a bad object name instead.
#[test]
fn eight_trees_read_and_nine_do_not() {
    let f = Fixture::new("cap");
    let eight = ["base"; 8];
    assert_eq!(f.read_tree(&eight), (0, String::new()));
    assert_eq!(f.paths(), ["a.txt", "b.txt"]);

    let nine = ["base"; 9];
    let (code, err) = f.read_tree(&nine);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: I cannot read more than 8 trees\n");

    let mut bad = eight.to_vec();
    bad.push("does-not-exist");
    let (code, err) = f.read_tree(&bad);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: Not a valid object name does-not-exist\n");
}

/// A plain read takes any number of trees with no merge at all — later trees win
/// per path. The guard that the prefix work did not leak into the non-prefix path.
#[test]
fn a_plain_read_of_three_trees_is_still_a_union() {
    let f = Fixture::new("union");
    assert_eq!(f.read_tree(&["base", "ours", "theirs"]), (0, String::new()));
    assert_eq!(f.paths(), ["a.txt", "b.txt", "c.txt"]);
    // `theirs` is last, so its `b.txt` wins; `c.txt` survives from `ours`.
    assert_eq!(f.oid_of("b.txt"), f.run(&["rev-parse", "theirs:b.txt"]).stdout_trimmed());
}

/// `exclude_per_directory_cb` (builtin/read-tree.c:57-71) dies on both counts
/// during the option scan: without `-u`, and with any name but `.gitignore`.
#[test]
fn exclude_per_directory_must_name_gitignore() {
    let f = Fixture::new("epd");
    let (code, err) = f.read_tree(&["-m", "-u", "--exclude-per-directory=x", "ours"]);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: --exclude-per-directory argument must be .gitignore\n");

    let (code, err) = f.read_tree(&["-m", "--exclude-per-directory=.gitignore", "ours"]);
    assert_eq!(code, 128);
    assert_eq!(err, "fatal: --exclude-per-directory is meaningless unless -u\n");

    assert_eq!(
        f.read_tree(&["-m", "-u", "--exclude-per-directory=.gitignore", "ours"]),
        (0, String::new())
    );
}

/// `Output::stdout` as a trimmed `String`, for the one-line plumbing reads above.
trait StdoutTrimmed {
    fn stdout_trimmed(&self) -> String;
}

impl StdoutTrimmed for Output {
    fn stdout_trimmed(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}
