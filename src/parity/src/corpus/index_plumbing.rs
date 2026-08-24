//! Differential corpus cases for the **index plumbing**: the layer every
//! porcelain verb in the port sits on.
//!
//! `ls-files` reads the index, `update-index` writes it, `read-tree` replaces it
//! from one to three trees, `checkout-index` pushes it back into the working
//! tree, and `status --porcelain=v2` / `diff-index` / `diff-files` are the three
//! renderings of "how does the index differ from HEAD and from the disk". Every
//! case here is compared against stock git for stdout, exit code and
//! post-command repository state.
//!
//! # What this module adds over the corpus it extends
//!
//! `corpus/worktree_index.rs` covers `update-index`'s flag surface and
//! `checkout-index`'s create/overwrite rules on the shapes it could reach with
//! stdin nailed shut; `corpus/plumbing_objects.rs` covers `read-tree`'s
//! one-tree, two-tree and three-tree forms against `Linear`/`Branched`/`Dirty`;
//! `corpus/shape_reach.rs` covers `ls-files` on `Sparse` and `Attributes` and
//! `status --porcelain=v2` on `Sparse`; `corpus.rs` covers bare `ls-files`,
//! `--stage` and `--full-name` across the read shapes. None of them is repeated
//! here. What is added is the part of the layer those passes leave open:
//!
//! * `ls-files`'s **selection algebra** — `-c`/`-o`/`-m`/`-d`/`-k`/`-i`/`-u`
//!   combined, and `--deduplicate`, which is only observable on an index that
//!   holds two entries for one path.
//! * `ls-files`'s **output shapes** — `--format`, `--abbrev`, `--eol`,
//!   `--with-tree`, `--recurse-submodules`, and `--full-name` from a
//!   subdirectory.
//! * `update-index --index-info` and `--stdin`, whose entire input is stdin.
//!   `Case::with_stdin` reaches them; nothing did before.
//! * `read-tree`'s **refusals**, which are the specification of the two-tree and
//!   three-tree merge: an entry not up to date, an untracked file in the way, an
//!   unmerged index, and the three mutually exclusive mode flags.
//! * `status --porcelain=v2`'s header lines (`# branch.*`, `# stash`) and its
//!   `u` records, which v1 does not have at all.
//!
//! # What the state probe cannot see, and what is used instead
//!
//! `runner::probe_state` reads the index back with `ls-files --stage`. That
//! shows mode, object id, stage and path — and nothing else. Four properties an
//! index carries are invisible to it, and each case whose whole point is one of
//! them says so where it is written:
//!
//! * **The index version and its extensions** (TREE, REUC, UNTR, EOIE, IEOT,
//!   link/split-index). Reached instead through `runner::probe_interop`, which
//!   copies the index aside and asks *both* binaries to `write-tree` from it: an
//!   index the port writes that stock cannot parse fails there rather than
//!   silently passing. That is why the `index.version` / `--index-version` /
//!   `--split-index` cases below are worth running even though their `ls-files
//!   --stage` output is identical to a run without them.
//! * **The `skip-worktree` and `assume-unchanged` bits.** `ls-files -v` prints
//!   them as `S` and as a lowercase tag, so the cases that set or clear a bit are
//!   paired with a `-v`/`-t` reader on the shape that already carries one
//!   (`Sparse`).
//! * **The `fsmonitor` valid bit.** `ls-files -f` lowercases the tag for an entry
//!   the fsmonitor has declared clean; with no fsmonitor configured every entry
//!   stays uppercase, so `-f` here pins the *default* rendering only.
//! * **The stat data.** `ls-files --debug` prints it — and prints `ctime`, `dev`
//!   and `ino`, which differ between the two sides' fixture copies by
//!   construction. A `--debug` case would fail for both binaries and measure the
//!   filesystem, so `--debug` is deliberately absent. `checkout-index --temp`
//!   is absent for the same class of reason: it names its output
//!   `.merge_file_XXXXXX` from `mkstemp`, so its stdout is different on every
//!   run of *either* binary.
//!
//! # Fixture constraints that bound this corpus
//!
//! A case is one argv against a pristine copy, so nothing here can set up state
//! first. Two consequences worth stating rather than leaving to be rediscovered:
//!
//! * **No fixture contains a three-way merge with a content conflict reachable
//!   from `read-tree`.** In `Shape::MergeableDirty` no two branches touch the same
//!   path, and `Shape::Conflicted`'s index is already unmerged so `read-tree -m`
//!   refuses outright. Stage 1/2/3 entries are therefore *created* here by
//!   `update-index --index-info`, which can write all three stages of one path
//!   from a stdin literal, and *consumed* on `Shape::Conflicted`.
//! * **Object ids in argv or on stdin must be constants of the hash function.**
//!   Only the empty blob, the empty tree and the all-zero oid qualify; a
//!   fixture's own blob id is a fact about that fixture. Everything else is named
//!   by a rev the fixture resolves.
//!
//! Citations below are to git 2.55.0: `builtin/ls-files.c`,
//! `builtin/update-index.c`, `builtin/read-tree.c`, `builtin/checkout-index.c`,
//! `unpack-trees.c` and `wt-status.c`.

use crate::fixture::Shape;
use crate::runner::Case;

/// The empty blob. In no fixture's object store, which is what makes it useful:
/// an implementation that resolves it from the store rather than recognising it
/// as a hash constant fails on the `--cacheinfo` and `--index-info` cases below.
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

/// The empty tree, for the one `--cacheinfo` case that names a mode `040000`
/// entry.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// A second index file inside the repository. `{repo}` is
/// [`crate::runner::REPO_PLACEHOLDER`], replaced with the running side's own
/// fixture root, so no absolute path is ever written into a case.
///
/// Under this the real `.git/index` must come back untouched from the state
/// probe while the alternate index absorbs the write — which is precisely the
/// invariant a port that ignores `GIT_INDEX_FILE` breaks, silently, on every
/// script that uses the variable to stage without disturbing the user.
const ALT_INDEX: &[(&str, &str)] = &[("GIT_INDEX_FILE", "{repo}/.git/alt-index")];

// ---------------------------------------------------------------------------
// stdin payloads
//
// Every byte is a literal in this file, so a case replays from its id. NULs are
// written `\x00` rather than as an octal escape: `\0` followed by a digit is a
// three-digit octal escape in most languages that could produce these bytes, and
// writing `\0100644` produces a backspace and `644`, not a NUL and `100644`.
// ---------------------------------------------------------------------------

/// One stage-0 entry naming the empty blob, in `--index-info`'s two-column form
/// (`<mode> SP <oid> TAB <path>`).
const II_ONE: &[u8] = b"100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tzero.txt\n";

/// All three merge stages of one path. `builtin/update-index.c` accepts a stage
/// column between the oid and the tab, and this is the only way a case that is
/// one argv against a pristine copy can *produce* an unmerged index — which is
/// why the `Linear` shape is used: the resulting stages are the case's own work
/// and not the fixture's.
const II_STAGES: &[u8] = b"100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 1\tstaged.txt\n\
                           100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 2\tstaged.txt\n\
                           100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 3\tstaged.txt\n";

/// `ls-tree`'s own output form, `<mode> SP <type> SP <oid> TAB <path>`, which
/// `--index-info` accepts so that `ls-tree | update-index --index-info` works.
const II_LSTREE: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\ttreeform.txt\n";

/// Mode `0` removes the path. The oid column is ignored, so the all-zero oid is
/// the honest thing to write there.
const II_REMOVE: &[u8] = b"0 0000000000000000000000000000000000000000\tREADME.md\n";

/// Two entries, NUL-terminated — what `-z` expects. A reader that splits on LF
/// swallows the whole payload as one record and invents a path with a tab in it.
const II_Z: &[u8] = b"100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tz1.txt\x00\
                      100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tz2.txt\x00";

/// A mode that is not octal at all: `fatal: malformed index info`.
const II_BAD_MODE: &[u8] = b"xyz e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tbad.txt\n";

/// A space where the tab between oid and path belongs. The path is not optional
/// and the separator is not "whitespace"; this is the same refusal.
const II_NO_TAB: &[u8] = b"100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 bad.txt\n";

/// `--stdin`'s input is plain paths, one per line — a different grammar from
/// `--index-info` on the same option-parsing path.
const PATHS_LF: &[u8] = b"untracked.txt\n";

/// The same idea for the two tracked paths, so `--verbose` has two lines to
/// print and `checkout-index --stdin` has two files to write.
const PATHS_TWO_LF: &[u8] = b"README.md\nsrc/lib.rs\n";

/// NUL-separated paths for `-z --stdin`.
const PATHS_Z: &[u8] = b"README.md\x00src/lib.rs\x00";

/// One path to remove, NUL-terminated.
const PATH_ONE_Z: &[u8] = b"src/lib.rs\x00";

/// A stdin-fed case whose stderr is compared byte for byte too.
///
/// [`Case::strict`] and [`Case::with_stdin`] are separate constructors and
/// neither composes with the other, so the two facts are combined here rather
/// than by adding a third constructor to `runner.rs`.
fn strict_stdin(
    cmd: &'static str,
    args: &[&str],
    shape: Shape,
    stdin: &'static [u8],
) -> Case {
    Case { compare_stderr: true, ..Case::with_stdin(cmd, args, shape, stdin) }
}

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    ls_files_selection(out);
    ls_files_rendering(out);
    ls_files_exclusion(out);
    ls_files_refusals(out);
    update_index_stdin(out);
    update_index_placement(out);
    update_index_format(out);
    read_tree_merge(out);
    read_tree_refusals(out);
    checkout_index_depth(out);
    status_v2(out);
    diff_staging(out);
}

/// `ls-files`'s selection algebra: which of `-c`/`-o`/`-m`/`-d`/`-k`/`-i`/`-u`
/// an entry falls into, and what happens when several are asked for at once.
///
/// `builtin/ls-files.c:show_files` walks the index once and the directory once,
/// and each entry is emitted by whichever flags match — so a port that
/// implements the flags as independent queries and concatenates the results gets
/// the *order* wrong even when every path is right. `-c -o -m -d -t` on `Dirty`
/// is the case that says so: git prints the directory walk first, then the index
/// walk, and one path appears three times under three different tags.
///
/// `--deduplicate` is only observable on an index holding two entries for one
/// path, so it runs on `Conflicted`, where `conflict.txt` is at stages 2 and 3.
fn ls_files_selection(out: &mut Vec<Case>) {
    out.push(Case::new("ls-files", &["ls-files", "-t"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "-v"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "-m"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "-d"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "-k"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "-o", "-k"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "-c", "-o", "-m", "-d", "-t"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "--no-cached", "-m"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "-m", "-d", "-t", "-z"], Shape::Dirty));

    // The unmerged half. `-t` tags every stage `M`; `-s -t` is the only
    // invocation that shows the tag and the stage column together.
    out.push(Case::new("ls-files", &["ls-files", "-t"], Shape::Conflicted));
    out.push(Case::new("ls-files", &["ls-files", "-s", "-t"], Shape::Conflicted));
    out.push(Case::new("ls-files", &["ls-files", "-u", "-z"], Shape::Conflicted));
    out.push(Case::new("ls-files", &["ls-files", "-c"], Shape::Conflicted));
    out.push(Case::new("ls-files", &["ls-files", "--deduplicate"], Shape::Conflicted));
    out.push(Case::new("ls-files", &["ls-files", "--deduplicate", "-c", "-m"], Shape::Conflicted));
    out.push(Case::new("ls-files", &["ls-files", "--resolve-undo"], Shape::Conflicted));

    // Skip-worktree entries are absent from disk. They are *not* deleted and not
    // modified: `-d`/`-m` must stay empty, which is the half a port that reads
    // the bit but does not honour it gets wrong. `shape_reach.rs` runs
    // `--deleted` alone; the pairing with `-m` and with `-k` is here.
    out.push(Case::new("ls-files", &["ls-files", "-m", "-d"], Shape::Sparse));
    out.push(Case::new("ls-files", &["ls-files", "-k"], Shape::Sparse));
    out.push(Case::new("ls-files", &["ls-files", "-f"], Shape::Sparse));
    // The fsmonitor bit has no fixture — nothing configures a monitor — so `-f`
    // pins the default rendering (every tag uppercase) and nothing more.
    out.push(Case::new("ls-files", &["ls-files", "-f"], Shape::Linear));
}

/// `ls-files`'s output shapes: how an entry is *rendered* once it is selected.
///
/// `--format` is a separate emitter in `builtin/ls-files.c`
/// (`show_ce_fmt`/`expand_objectsize`), not a wrapper over the default one, and
/// it refuses to coexist with `-s`/`-o`/`-k`/`-t`/`--resolve-undo`/
/// `--deduplicate`/`--eol`. A port that treats `--format` as a printf over the
/// existing output path gets both halves wrong at once.
fn ls_files_rendering(out: &mut Vec<Case>) {
    out.push(Case::new("ls-files", &["ls-files", "--format=%(objectname) %(path)"], Shape::AwkwardPaths));
    out.push(Case::new("ls-files", &["ls-files", "--format=%(objectmode) %(objecttype) %(path)"], Shape::Linear));
    out.push(Case::new("ls-files", &["ls-files", "--format=%(objectsize)"], Shape::Linear));
    out.push(Case::new("ls-files", &["ls-files", "--format=%(stage) %(path)"], Shape::Conflicted));
    out.push(Case::new(
        "ls-files",
        &["ls-files", "--format=%(eolinfo:index) %(eolinfo:worktree) %(eolattr) %(path)"],
        Shape::Attributes,
    ));
    out.push(Case::new("ls-files", &["ls-files", "--format=%(objectname)"], Shape::Sparse));

    // Abbreviation. `--abbrev=1` is clamped to `MINIMUM_ABBREV` (4), which is
    // the corner an implementation that passes the number straight through
    // misses.
    out.push(Case::new("ls-files", &["ls-files", "--abbrev=8", "-s"], Shape::Linear));
    out.push(Case::new("ls-files", &["ls-files", "--abbrev", "-s"], Shape::Linear));
    out.push(Case::new("ls-files", &["ls-files", "--abbrev=1", "-s"], Shape::Linear));
    out.push(Case::new("ls-files", &["ls-files", "--abbrev=40", "-s"], Shape::Branched));
    out.push(Case::new("ls-files", &["-c", "core.abbrev=12", "ls-files", "-s"], Shape::Linear));

    // `--eol` on the shapes where the three columns can disagree. `Attributes`
    // is covered in `shape_reach.rs`; `Dirty` (a path deleted from the worktree
    // reports `w/`) and `Whitespace` (a CRLF file normalized on check-in) are
    // not.
    out.push(Case::new("ls-files", &["ls-files", "--eol"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "--eol"], Shape::Whitespace));
    out.push(Case::new("ls-files", &["ls-files", "--eol", "-z"], Shape::Dirty));

    // `--with-tree` re-adds paths that were removed from the index since the
    // named tree, which is how `git status` renders a staged deletion.
    out.push(Case::new("ls-files", &["ls-files", "--with-tree=feature"], Shape::Branched));
    out.push(Case::new("ls-files", &["ls-files", "--with-tree=v0.1.0", "-t"], Shape::Branched));
    out.push(Case::new("ls-files", &["ls-files", "--with-tree=HEAD", "-t"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "--with-tree=HEAD^"], Shape::Branched));

    // A gitlink is one index entry; `--recurse-submodules` spawns the submodule's
    // own `ls-files` and re-prefixes every path it reports.
    out.push(Case::new("ls-files", &["ls-files", "--recurse-submodules"], Shape::Submodule));
    out.push(Case::new("ls-files", &["ls-files", "--recurse-submodules", "-s"], Shape::Submodule));

    // Path rendering from a subdirectory: relative by default, repository-rooted
    // under `--full-name`.
    out.push(Case::new("ls-files", &["ls-files"], Shape::Linear).in_dir("src"));
    out.push(Case::new("ls-files", &["ls-files", "--full-name"], Shape::Linear).in_dir("src"));
    out.push(Case::new("ls-files", &["ls-files", "--full-name", "-s"], Shape::AwkwardPaths).in_dir("src"));
    out.push(Case::new("ls-files", &["ls-files", "-o", "--exclude-standard"], Shape::Attributes).in_dir("sub"));

    // A decomposed path, which macOS hands out of `readdir()` and git composes
    // before it reaches the index. `-s` prints the index's own bytes.
    out.push(Case::new("ls-files", &["ls-files", "-s"], Shape::DecomposedPaths));
    out.push(Case::new("ls-files", &["ls-files", "-m", "-o", "-t"], Shape::DecomposedPaths));

    // Reading an index that is not the repository's. The state probe reads
    // `.git/index`, so this case's assertion is that the *real* index came back
    // untouched while the command answered from the empty one.
    out.push(Case::new("ls-files", &["ls-files", "-s"], Shape::Linear).with_env(ALT_INDEX));
    out.push(Case::new("ls-files", &["ls-files", "-t", "-o"], Shape::Dirty).with_env(ALT_INDEX));
}

/// The exclusion machinery `ls-files` shares with `add` and `status`: where the
/// patterns come from and which of `-o`/`-i` they filter.
///
/// `-i` without `--exclude-standard` (or any `-x`/`-X`) lists nothing, because
/// there are no patterns to be ignored *by*; `-i --exclude-standard -c` is the
/// one invocation that reports a **tracked** path whose own rule matches it
/// (`logs/keep.log`, added with `-f`). Both are contracts scripts read.
fn ls_files_exclusion(out: &mut Vec<Case>) {
    out.push(Case::new("ls-files", &["ls-files", "-i", "--exclude-standard", "-c"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "-i", "--exclude-standard", "-c", "-o"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "-i", "--exclude=*.md"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "-o", "--exclude=*.log", "--exclude-standard"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "-o", "--exclude-from=.gitignore"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "-o", "--exclude-per-directory=.gitignore"], Shape::Attributes));

    // `--directory` collapses an untracked directory to its name. Without
    // `--exclude-standard` the ignored directories are in the walk, so `build/`
    // and `sub/deep-ignored/` collapse and everything else stays a file — which
    // is what separates "collapse any directory" from git's "collapse a
    // directory containing no tracked path".
    out.push(Case::new("ls-files", &["ls-files", "-o", "--directory"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "-o", "--directory", "--no-empty-directory"], Shape::Attributes));
    out.push(Case::new("ls-files", &["ls-files", "-o", "--directory"], Shape::Dirty));
    out.push(Case::new("ls-files", &["ls-files", "-o", "--directory", "--exclude-standard"], Shape::Sparse));
    out.push(Case::new("ls-files", &["ls-files", "-o", "-i", "--exclude-standard", "--directory"], Shape::Attributes));
}

/// `ls-files`'s refusals. Two of them, and both matter to scripts:
/// `--error-unmatch` is how a script asks "is this path tracked", and the
/// `--format` incompatibility is the one place `ls-files` rejects an argument
/// combination rather than quietly picking a winner.
fn ls_files_refusals(out: &mut Vec<Case>) {
    // Exit 1 with the diagnostic on stderr, and the matching paths still printed
    // on stdout — git does not abort the walk at the first miss.
    out.push(Case::strict("ls-files", &["ls-files", "--error-unmatch", "untracked.txt"], Shape::Dirty));
    out.push(Case::strict(
        "ls-files",
        &["ls-files", "--error-unmatch", "README.md", "src/lib.rs", "nosuch.txt"],
        Shape::Linear,
    ));
    out.push(Case::new("ls-files", &["ls-files", "--error-unmatch", "README.md"], Shape::Dirty));
    out.push(Case::strict("ls-files", &["ls-files", "--error-unmatch", "outside/drop.txt"], Shape::Sparse));

    // `fatal: bad ls-files format: %(bogus)`, exit 128.
    out.push(Case::strict("ls-files", &["ls-files", "--format=%(bogus)"], Shape::Linear));
    // Rejected with a `usage:` block, which the harness's standing policy keeps
    // out of byte comparison — so this one is deliberately not `strict`.
    out.push(Case::new("ls-files", &["ls-files", "--format=%(objecttype)", "-s"], Shape::Linear));
}

/// `update-index`'s stdin modes. Nothing reached them before `Case::with_stdin`:
/// `--index-info` and `--stdin` take their whole input there, so every case in
/// `worktree_index.rs` measured the immediate-EOF path.
///
/// `--index-info` is the only route in this harness to an index carrying stage
/// 1/2/3 entries that the *case* produced rather than the fixture — see the
/// module note on why no fixture offers a `read-tree`-reachable content
/// conflict.
fn update_index_stdin(out: &mut Vec<Case>) {
    out.push(Case::with_stdin("update-index", &["update-index", "--index-info"], Shape::Linear, II_ONE));
    out.push(Case::with_stdin("update-index", &["update-index", "--index-info"], Shape::Linear, II_STAGES));
    out.push(Case::with_stdin("update-index", &["update-index", "--index-info"], Shape::Linear, II_LSTREE));
    out.push(Case::with_stdin("update-index", &["update-index", "--index-info"], Shape::Linear, II_REMOVE));
    out.push(Case::with_stdin("update-index", &["update-index", "-z", "--index-info"], Shape::Linear, II_Z));
    out.push(Case::with_stdin("update-index", &["update-index", "--index-info"], Shape::Conflicted, II_ONE));
    out.push(Case::with_stdin("update-index", &["update-index", "--index-info"], Shape::AwkwardPaths, II_STAGES));

    // `fatal: malformed index info <record>`, exit 128, index untouched. A port
    // that accepts either of these writes an index git will not read.
    out.push(strict_stdin("update-index", &["update-index", "--index-info"], Shape::Linear, II_BAD_MODE));
    out.push(strict_stdin("update-index", &["update-index", "--index-info"], Shape::Linear, II_NO_TAB));

    // `--stdin` takes bare paths, not records. Same option-parsing path, a
    // different grammar behind it.
    out.push(Case::with_stdin("update-index", &["update-index", "--add", "--stdin"], Shape::Dirty, PATHS_LF));
    out.push(Case::with_stdin("update-index", &["update-index", "--verbose", "--stdin"], Shape::Dirty, PATHS_TWO_LF));
    out.push(Case::with_stdin("update-index", &["update-index", "--stdin"], Shape::Dirty, PATHS_TWO_LF));
    out.push(Case::with_stdin("update-index", &["update-index", "-z", "--stdin"], Shape::Dirty, PATHS_Z));
    out.push(Case::with_stdin(
        "update-index",
        &["update-index", "--force-remove", "-z", "--stdin"],
        Shape::Dirty,
        PATH_ONE_Z,
    ));
    out.push(Case::with_stdin(
        "update-index",
        &["update-index", "--skip-worktree", "--stdin"],
        Shape::Dirty,
        PATHS_TWO_LF,
    ));
    out.push(Case::with_stdin(
        "update-index",
        &["update-index", "--assume-unchanged", "-z", "--stdin"],
        Shape::Dirty,
        PATHS_Z,
    ));
}

/// Where an entry is allowed to land: the file-vs-directory rule, and the two
/// per-entry bits read back through `ls-files -v`.
///
/// `builtin/update-index.c:add_cacheinfo` refuses a path that collides with an
/// existing directory — or that turns an existing file into one — unless
/// `--replace` is given (`error: '<path>' appears as both a file and as a
/// directory`). That is the rule which keeps an index from describing a tree
/// git cannot serialize, and it is invisible to any test that only adds fresh
/// paths.
fn update_index_placement(out: &mut Vec<Case>) {
    let file_over_dir = format!("100644,{EMPTY_BLOB},src");
    let dir_over_file = format!("100644,{EMPTY_BLOB},README.md/sub");
    out.push(Case::new("update-index", &["update-index", "--add", "--replace", "--cacheinfo", &file_over_dir], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--add", "--replace", "--cacheinfo", &dir_over_file], Shape::Linear));
    // Three diagnostics and `fatal: git update-index: --cacheinfo cannot add
    // <path>`, exit 128, index unchanged.
    out.push(Case::strict("update-index", &["update-index", "--add", "--cacheinfo", &file_over_dir], Shape::Linear));
    out.push(Case::strict("update-index", &["update-index", "--add", "--cacheinfo", &dir_over_file], Shape::Linear));

    // A mode that is not one of the five a tree may carry. git canonicalizes the
    // permission bits rather than refusing: `100777` becomes `100755`. An
    // implementation that validates the whole 16 bits rejects a mode git accepts.
    let odd_mode = format!("100777,{EMPTY_BLOB},odd.txt");
    out.push(Case::new("update-index", &["update-index", "--add", "--cacheinfo", &odd_mode], Shape::Linear));
    let mode_664 = format!("100664,{EMPTY_BLOB},group.txt");
    out.push(Case::new("update-index", &["update-index", "--add", "--cacheinfo", &mode_664], Shape::Linear));
    // Mode `040000` in the index is a directory entry, which is only legal in a
    // sparse index. git accepts the write and then warns when reading it back —
    // so the state probe, not the exit code, is the assertion here.
    let tree_entry = format!("040000,{EMPTY_TREE},subtree");
    out.push(Case::new("update-index", &["update-index", "--add", "--cacheinfo", &tree_entry], Shape::Linear));

    // The per-entry bits, paired with a reader that shows them. `ls-files
    // --stage` cannot; `-v` prints `S` for skip-worktree and lowercases the tag
    // for assume-unchanged.
    out.push(Case::new("update-index", &["update-index", "--no-skip-worktree", "outside/nested/deep.txt"], Shape::Sparse));
    out.push(Case::new("update-index", &["update-index", "--skip-worktree", "root.txt"], Shape::Sparse));
    out.push(Case::new("update-index", &["update-index", "--assume-unchanged", "root.txt"], Shape::Sparse));
    out.push(Case::new("update-index", &["update-index", "--chmod=+x", "conflict.txt"], Shape::Conflicted));

    // Refresh against a sparse index: the skip-worktree paths are absent from
    // disk and must not be reported as needing an update, which is the whole
    // point of the bit.
    out.push(Case::new("update-index", &["update-index", "--refresh"], Shape::Sparse));
    out.push(Case::new("update-index", &["update-index", "--really-refresh"], Shape::Sparse));
    out.push(Case::new("update-index", &["update-index", "-q", "--refresh"], Shape::Sparse));
    out.push(Case::new("update-index", &["update-index", "--refresh", "--ignore-missing"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--unmerged", "-q", "--refresh"], Shape::Conflicted));
    out.push(Case::new("update-index", &["update-index", "--refresh"], Shape::DecomposedPaths));

    // Writing somewhere other than `.git/index`. The state probe reads the real
    // index, so agreement here means the write went to the alternate file on
    // both sides.
    out.push(Case::new("update-index", &["update-index", "--add", "README.md"], Shape::Dirty).with_env(ALT_INDEX));
    out.push(Case::with_stdin("update-index", &["update-index", "--index-info"], Shape::Linear, II_ONE).with_env(ALT_INDEX));
}

/// The index *format* switches. Their whole effect is on bytes `ls-files
/// --stage` does not show, so these are here for `runner::probe_interop`, which
/// hands the resulting index to stock git's own `write-tree`: an index the port
/// writes in a version or with an extension stock cannot parse fails there.
///
/// `index.version` is also the one setting whose value git silently clamps —
/// `read_index_extension`/`verify_hdr` accept 2, 3 and 4 only — so an
/// out-of-range value is a behaviour, not an error.
fn update_index_format(out: &mut Vec<Case>) {
    let add = format!("100644,{EMPTY_BLOB},fmt.txt");
    for version in ["2", "3", "4"] {
        out.push(
            Case::new("update-index", &["update-index", "--add", "--cacheinfo", &add], Shape::Linear)
                .with_config(&[("index.version", version)]),
        );
    }
    out.push(Case::new("update-index", &["update-index", "--index-version", "3"], Shape::Dirty));
    out.push(Case::new("update-index", &["update-index", "--index-version", "9"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--show-index-version"], Shape::Conflicted));
    out.push(
        Case::new("update-index", &["update-index", "--add", "--cacheinfo", &add], Shape::Linear)
            .with_config(&[("index.skipHash", "true")]),
    );
    out.push(
        Case::new("update-index", &["update-index", "--split-index"], Shape::Dirty)
            .with_config(&[("core.splitIndex", "true")]),
    );
    // `--untracked-cache` itself is deliberately absent: it writes a UNTR
    // extension whose contents stock git does not reproduce between two runs of
    // *itself*, so `worktree_index.rs`'s bare case is already excluded from the
    // denominator as `StockNondeterministic` and a second spelling of it would
    // add another excluded case and no measurement. `--test-untracked-cache`
    // only probes the filesystem and prints its verdict, so it is comparable.
    out.push(
        Case::new("update-index", &["update-index", "--test-untracked-cache"], Shape::Dirty)
            .with_config(&[("core.untrackedCache", "true")]),
    );
    out.push(Case::new("update-index", &["update-index", "--fsmonitor-valid", "README.md"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--no-fsmonitor-valid", "README.md"], Shape::Linear));

    // Stat comparison. The fixture pins `core.checkStat=minimal` to compensate
    // for the per-case copy (see `fixture::build`); these two put the setting
    // back where it is a *behaviour* rather than a harness artifact, on the shape
    // whose answer is decided by content — `Dirty` really has modified files, so
    // both sides must report them whatever the stat fields say.
    out.push(
        Case::new("update-index", &["update-index", "--refresh"], Shape::Dirty)
            .with_config(&[("core.checkStat", "default")]),
    );
    out.push(
        Case::new("update-index", &["update-index", "--refresh"], Shape::Dirty)
            .with_config(&[("core.trustctime", "false")]),
    );
    out.push(
        Case::new("update-index", &["update-index", "--refresh"], Shape::Dirty)
            .with_config(&[("core.fileMode", "false")]),
    );
    out.push(
        Case::new("ls-files", &["ls-files", "-s"], Shape::Linear)
            .with_config(&[("core.preloadIndex", "true"), ("index.threads", "1")]),
    );
    out.push(
        Case::new("ls-files", &["ls-files", "-t", "-m"], Shape::Dirty)
            .with_config(&[("core.preloadIndex", "false"), ("core.fscache", "false")]),
    );
}

/// `read-tree`'s merge modes, on the one shape built to separate them.
///
/// `Shape::MergeableDirty` holds three kinds of dirt on purpose: `hot.txt` is
/// edited in the worktree *and* rewritten by `ff-hot`/`div-hot`, `keep.txt` is
/// edited and rewritten by nothing, and `squat.txt` is an untracked file sitting
/// exactly where `ff-squat`/`div-squat` want to write. `unpack-trees.c`
/// (`verify_uptodate`, `verify_absent`) decides per path, so a blanket "is
/// anything dirty" refusal and git's per-path one produce the same answer on
/// every other shape and different answers here.
fn read_tree_merge(out: &mut Vec<Case>) {
    // Two-tree fast-forward. `ff-cold` touches only a path the worktree is not
    // holding, so it must succeed and write `cold.txt` to disk.
    out.push(Case::new("read-tree", &["read-tree", "-m", "-u", "HEAD", "ff-cold"], Shape::MergeableDirty));
    out.push(Case::new("read-tree", &["read-tree", "-m", "HEAD", "ff-cold"], Shape::MergeableDirty));
    out.push(Case::new("read-tree", &["read-tree", "-m", "-u", "-v", "HEAD", "ff-cold"], Shape::MergeableDirty));
    out.push(Case::new("read-tree", &["read-tree", "-i", "-m", "HEAD", "ff-hot"], Shape::MergeableDirty));

    // Three-tree merge with a real base. `HEAD~1` is the commit both sides fork
    // from; `div-other` adds a path neither the base nor `main` has.
    out.push(Case::new("read-tree", &["read-tree", "-m", "HEAD~1", "HEAD", "div-other"], Shape::MergeableDirty));
    out.push(Case::new("read-tree", &["read-tree", "-m", "-u", "HEAD~1", "HEAD", "div-other"], Shape::MergeableDirty));
    out.push(Case::new("read-tree", &["read-tree", "-m", "--aggressive", "HEAD~1", "HEAD", "div-other"], Shape::MergeableDirty));
    out.push(Case::new("read-tree", &["read-tree", "-m", "--trivial", "HEAD~1", "HEAD", "div-other"], Shape::MergeableDirty));
    out.push(Case::new("read-tree", &["read-tree", "-m", "HEAD~1", "HEAD", "div-cold"], Shape::MergeableDirty));
    out.push(Case::new("read-tree", &["read-tree", "-m", "--aggressive", "HEAD", "div-cold"], Shape::MergeableDirty));

    // `--dry-run` must decide exactly what the real run would and write nothing.
    // The state probe is the assertion: agreement means neither side moved the
    // index.
    out.push(Case::new("read-tree", &["read-tree", "--dry-run", "-m", "-u", "HEAD", "ff-cold"], Shape::MergeableDirty));
    out.push(Case::new("read-tree", &["read-tree", "-v", "--dry-run", "-m", "-u", "HEAD", "ff-hot"], Shape::MergeableDirty));
    out.push(Case::new("read-tree", &["read-tree", "--dry-run", "--reset", "-u", "HEAD~1"], Shape::MergeableDirty));

    // More than two trees without `-m` is not an error: git overlays them, later
    // trees winning. A port that rejects a second tree outside `-m` breaks
    // `git-merge-one-file`-era scripts.
    out.push(Case::new("read-tree", &["read-tree", "HEAD", "feature"], Shape::Branched));
    out.push(Case::new("read-tree", &["read-tree", "HEAD^", "HEAD", "feature"], Shape::Branched));

    // `--index-output` writes the result somewhere else; the repository-relative
    // path keeps the case free of an absolute one. The real index must survive.
    out.push(Case::new("read-tree", &["read-tree", "--index-output=.git/alt-index", "HEAD"], Shape::Dirty));
    out.push(Case::new("read-tree", &["read-tree", "--index-output=.git/alt-index", "--reset", "HEAD"], Shape::Conflicted));
    out.push(Case::new("read-tree", &["read-tree", "HEAD"], Shape::Linear).with_env(ALT_INDEX));
    out.push(Case::new("read-tree", &["read-tree", "--empty"], Shape::Dirty).with_env(ALT_INDEX));

    // A sparse index. `--no-sparse-checkout` tells `unpack-trees` to ignore the
    // patterns and materialize everything, which is the flag `git checkout`
    // passes when it is leaving a sparse checkout behind.
    out.push(Case::new("read-tree", &["read-tree", "--reset", "-u", "HEAD"], Shape::Sparse));
    out.push(Case::new("read-tree", &["read-tree", "-m", "-u", "--no-sparse-checkout", "HEAD"], Shape::Sparse));
    out.push(Case::new("read-tree", &["read-tree", "--prefix=extra/", "HEAD"], Shape::Sparse));
    out.push(
        Case::new("read-tree", &["read-tree", "--reset", "-u", "HEAD"], Shape::Sparse)
            .with_config(&[("core.sparseCheckoutCone", "false")]),
    );

    // `--prefix` grafts a tree under a directory. Onto an *occupied* prefix git
    // does not refuse — it interleaves, so `src/` ends up holding both the
    // original `lib.rs` and a nested copy of the whole tree. That is surprising
    // enough that a port is likely to "fix" it.
    out.push(Case::new("read-tree", &["read-tree", "--prefix=src/", "HEAD"], Shape::Linear));
    out.push(Case::new("read-tree", &["read-tree", "--prefix=nested/deep/", "HEAD"], Shape::AwkwardPaths));
    out.push(Case::new("read-tree", &["read-tree", "-u", "--prefix=graft/", "HEAD"], Shape::Linear));
    out.push(Case::new("read-tree", &["read-tree", "--prefix=sub/", "HEAD"], Shape::Submodule));
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Linear)
            .with_config(&[("index.version", "4")]),
    );
}

/// `read-tree`'s refusals. For a two- or three-tree merge these *are* the
/// specification: each one is a state git will not let the index reach, and a
/// port that accepts any of them silently destroys work.
fn read_tree_refusals(out: &mut Vec<Case>) {
    // `error: Entry 'hot.txt' not uptodate. Cannot merge.` — the worktree edit
    // would be lost. `verify_uptodate`, unpack-trees.c.
    out.push(Case::strict("read-tree", &["read-tree", "-m", "-u", "HEAD", "ff-hot"], Shape::MergeableDirty));
    out.push(Case::strict("read-tree", &["read-tree", "-m", "-u", "HEAD~1", "HEAD", "div-hot"], Shape::MergeableDirty));

    // `error: Untracked working tree file 'squat.txt' would be overwritten by
    // merge.` — `verify_absent`. A different check from the one above, and the
    // one a "refuse if the tree is dirty at all" implementation never reaches.
    out.push(Case::strict("read-tree", &["read-tree", "-m", "-u", "HEAD", "ff-squat"], Shape::MergeableDirty));

    // `error: Entry 'cold.txt' would be overwritten by merge. Cannot merge.` —
    // the index disagrees with every tree named, so there is no side to keep.
    out.push(Case::strict("read-tree", &["read-tree", "-m", "HEAD~1", "div-cold", "ff-cold"], Shape::MergeableDirty));

    // `fatal: You need to resolve your current index first` — an unmerged index
    // cannot be the input to another merge.
    out.push(Case::strict("read-tree", &["read-tree", "-m", "HEAD"], Shape::Conflicted));
    out.push(Case::strict("read-tree", &["read-tree", "-m", "-u", "HEAD^", "HEAD", "theirs"], Shape::Conflicted));

    // The three mode flags are mutually exclusive:
    // `fatal: Which one? -m, --reset, or --prefix?`
    out.push(Case::strict("read-tree", &["read-tree", "-m", "--prefix=x/", "HEAD", "feature"], Shape::Branched));
    out.push(Case::strict("read-tree", &["read-tree", "--reset", "--prefix=x/", "HEAD"], Shape::Linear));

    // `fatal: -u is meaningless without -m, --reset, or --prefix`
    out.push(Case::strict("read-tree", &["read-tree", "-u", "HEAD", "feature"], Shape::Branched));
}

/// `checkout-index`: the index-to-worktree writer, on the two shapes whose index
/// is not a plain stage-0 list.
///
/// It is deliberately *not* a checkout: `builtin/checkout-index.c` writes every
/// entry it is asked for regardless of the skip-worktree bit, and refuses to
/// overwrite an existing file without `-f`. Both are contracts, and both are
/// easy to get wrong in the direction of "be helpful".
fn checkout_index_depth(out: &mut Vec<Case>) {
    out.push(Case::with_stdin("checkout-index", &["checkout-index", "-z", "--stdin", "-f"], Shape::Dirty, PATHS_Z));
    out.push(Case::with_stdin("checkout-index", &["checkout-index", "--stdin", "-f"], Shape::Dirty, PATHS_TWO_LF));
    out.push(Case::with_stdin("checkout-index", &["checkout-index", "--stdin", "-f", "-q"], Shape::Dirty, PATHS_TWO_LF));
    out.push(Case::with_stdin(
        "checkout-index",
        &["checkout-index", "--stdin", "-f", "--prefix=copy/"],
        Shape::Dirty,
        PATHS_TWO_LF,
    ));
    out.push(Case::with_stdin("checkout-index", &["checkout-index", "-z", "-n", "--stdin", "-f"], Shape::Dirty, PATHS_Z));
    // A NUL-separated payload fed to the *newline* reader. Not a typo: git reads
    // the whole 21 bytes as one record and then hands it to `prefix_path()`,
    // which is a C-string call — so the path silently truncates at the first NUL
    // and only `README.md` is checked out, at exit 0. An implementation that
    // keeps the record as raw bytes looks up a path with a NUL in it, does not
    // find it, and reports `is not in the cache`.
    out.push(Case::with_stdin("checkout-index", &["checkout-index", "-n", "--stdin", "-f"], Shape::Dirty, PATHS_Z));
    // `README.md already exists, no checkout`, exit 1, on stderr — the refusal
    // that makes `-f` mean something.
    out.push(strict_stdin("checkout-index", &["checkout-index", "--stdin"], Shape::Dirty, PATHS_TWO_LF));

    // Per-stage checkout on an unmerged index. `--stage=1` selects a stage that
    // does not exist for `conflict.txt` (the merge base is an add/add), so it
    // writes nothing for that path and everything for the rest.
    out.push(Case::new("checkout-index", &["checkout-index", "-f", "--stage=1", "-a"], Shape::Conflicted));
    out.push(Case::new("checkout-index", &["checkout-index", "-f", "--stage=2", "-a"], Shape::Conflicted));
    out.push(Case::new("checkout-index", &["checkout-index", "-f", "--stage=3", "-a"], Shape::Conflicted));
    out.push(Case::new("checkout-index", &["checkout-index", "-f", "--stage=3", "conflict.txt"], Shape::Conflicted));
    out.push(Case::new("checkout-index", &["checkout-index", "--stage=2", "--prefix=st/", "-a"], Shape::Conflicted));

    // A sparse index. Every entry is written, skip-worktree included — the file
    // reappears on disk and the entry's stat data is refreshed, which the state
    // probe sees as a `status` change.
    out.push(Case::new("checkout-index", &["checkout-index", "-a"], Shape::Sparse));
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "-f"], Shape::Sparse));
    out.push(Case::new("checkout-index", &["checkout-index", "-f", "outside/drop.txt"], Shape::Sparse));
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "--prefix=out/"], Shape::Sparse));
    out.push(Case::new("checkout-index", &["checkout-index", "-u", "-a", "-f"], Shape::Sparse));

    // Awkward and decomposed paths through the same writer.
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "-f", "--prefix=copy/"], Shape::DecomposedPaths));
    out.push(Case::new("checkout-index", &["checkout-index", "-f", "üñïçødé.txt"], Shape::AwkwardPaths));
    out.push(Case::new("checkout-index", &["checkout-index", "-f", "-a"], Shape::Submodule));

    // Reading a different index: with `GIT_INDEX_FILE` pointing at a file that
    // does not exist the index is empty, so `-a` has nothing to write and must
    // still exit 0.
    out.push(Case::new("checkout-index", &["checkout-index", "-a", "-f"], Shape::Dirty).with_env(ALT_INDEX));
}

/// `status --porcelain=v2`. The corpus has 80-odd `status` cases and almost all
/// of them are v1, which shares no output code with v2: `wt-status.c`'s
/// `wt_porcelain_v2_print` emits `1`/`2`/`u`/`?`/`!` records with an XY pair, a
/// submodule field, three modes and two object ids, plus `# branch.*` and
/// `# stash` headers v1 has no equivalent of.
fn status_v2(out: &mut Vec<Case>) {
    out.push(Case::new("status", &["status", "--porcelain=v2"], Shape::Dirty));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::Dirty));
    out.push(Case::new("status", &["status", "--porcelain=v2", "-z"], Shape::Dirty));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch", "-z"], Shape::Dirty));
    out.push(Case::new("status", &["status", "--porcelain=v2", "-uno"], Shape::Dirty));
    out.push(Case::new("status", &["status", "--porcelain=v2", "-uall"], Shape::Dirty));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::Detached));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::Merged));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::Worktree));

    // The `u` record: six fields git does not print anywhere else, including the
    // three stage modes and the three stage object ids on one line.
    out.push(Case::new("status", &["status", "--porcelain=v2"], Shape::Conflicted));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::Conflicted));
    out.push(Case::new("status", &["status", "--porcelain=v2", "-z"], Shape::Conflicted));

    // `# branch.ab`. `--no-ahead-behind` replaces the counts with `+? -?` rather
    // than dropping the line, which is the shape a port is likely to get wrong.
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::BehindRemote));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch", "--no-ahead-behind"], Shape::BehindRemote));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch", "--ahead-behind"], Shape::BehindRemote));
    out.push(
        Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::BehindRemote)
            .with_config(&[("status.aheadBehind", "false")]),
    );

    // `# stash <n>`, which only appears under `--show-stash` and is independent
    // of `--branch`.
    out.push(Case::new("status", &["status", "--porcelain=v2", "--show-stash"], Shape::Stashed));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch", "--show-stash"], Shape::Stashed));
    out.push(
        Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::Stashed)
            .with_config(&[("status.showStash", "true")]),
    );
    out.push(Case::new("status", &["status", "--porcelain=v2", "--no-show-stash"], Shape::Stashed));

    // The `!` record and the two `--ignored` modes that differ in whether an
    // ignored directory is collapsed.
    out.push(Case::new("status", &["status", "--porcelain=v2", "--ignored=matching"], Shape::Attributes));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--ignored=traditional"], Shape::Attributes));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--ignored=no", "-uall"], Shape::Attributes));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--ignored=matching", "-z"], Shape::Attributes));

    // The submodule field. `N...` is "not a submodule"; a gitlink entry gets
    // `S` plus three flags, and `--ignore-submodules` decides whether the
    // submodule is inspected at all.
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch"], Shape::Submodule));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--ignore-submodules=none"], Shape::Submodule));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--ignore-submodules=all"], Shape::Submodule));

    // Rename detection. No fixture carries a *pending* rename — `Shape::Renamed`
    // has its renames in history and a clean worktree — so these pin the flag
    // parsing and the `2` record's absence, not detection quality. Recorded
    // rather than left implicit: a `2` record is unmeasured by this corpus.
    out.push(Case::new("status", &["status", "--porcelain=v2", "--renames"], Shape::Dirty));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--no-renames"], Shape::Dirty));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--find-renames=25%"], Shape::Dirty));
    out.push(
        Case::new("status", &["status", "--porcelain=v2"], Shape::Dirty)
            .with_config(&[("status.renames", "copies")]),
    );

    // Awkward, decomposed and sparse paths through the v2 quoting rules.
    out.push(Case::new("status", &["status", "--porcelain=v2", "-uall"], Shape::AwkwardPaths));
    out.push(Case::new("status", &["status", "--porcelain=v2", "-uall"], Shape::DecomposedPaths));
    out.push(Case::new("status", &["status", "--porcelain=v2", "--branch", "-uall"], Shape::Sparse));
    out.push(
        Case::new("status", &["status", "--porcelain=v2", "-uall"], Shape::AwkwardPaths)
            .with_config(&[("core.quotePath", "false")]),
    );
}

/// `diff-index` and `diff-files` where what they measure is the *index*, not the
/// diff engine — the half `corpus/diff_family.rs` leaves open because every one
/// of its cases runs on `Shape::Dirty`.
///
/// The sharpest of these is `diff-index --cached` on an unmerged path: with
/// `--cached` git reports `U` and a null destination id, and without it the same
/// path is an ordinary `M` against the worktree file. A port that renders
/// unmerged entries the same way in both modes is wrong in exactly one of them.
fn diff_staging(out: &mut Vec<Case>) {
    out.push(Case::new("diff-index", &["diff-index", "--raw", "HEAD"], Shape::Conflicted));
    out.push(Case::new("diff-index", &["diff-index", "--raw", "--cached", "HEAD"], Shape::Conflicted));
    out.push(Case::new("diff-index", &["diff-index", "--name-status", "--cached", "HEAD"], Shape::Conflicted));
    out.push(Case::new("diff-index", &["diff-index", "--cached", "-z", "HEAD"], Shape::Conflicted));

    // A staged change on a path no branch touches: `--cached` and the worktree
    // comparison must answer identically, because the worktree matches the index.
    out.push(Case::new("diff-index", &["diff-index", "--cached", "--name-status", "HEAD"], Shape::MergeableStaged));
    out.push(Case::new("diff-index", &["diff-index", "--name-status", "HEAD"], Shape::MergeableStaged));
    out.push(Case::new("diff-index", &["diff-index", "--cached", "--exit-code", "HEAD"], Shape::MergeableStaged));
    out.push(Case::new("diff-files", &["diff-files", "--exit-code"], Shape::MergeableStaged));

    // Skip-worktree entries have no file on disk. Neither command may report
    // them as deleted; a port that stats the path and finds ENOENT does.
    out.push(Case::new("diff-index", &["diff-index", "HEAD"], Shape::Sparse));
    out.push(Case::new("diff-index", &["diff-index", "--cached", "HEAD"], Shape::Sparse));
    out.push(Case::new("diff-index", &["diff-index", "--raw", "--exit-code", "HEAD"], Shape::Sparse));
    out.push(Case::new("diff-files", &["diff-files"], Shape::Sparse));
    out.push(Case::new("diff-files", &["diff-files", "--raw", "--exit-code"], Shape::Sparse));

    // A decomposed path, quoted out of the index rather than out of the walk.
    out.push(Case::new("diff-index", &["diff-index", "--name-status", "HEAD"], Shape::DecomposedPaths));
    out.push(Case::new("diff-files", &["diff-files", "--name-status"], Shape::DecomposedPaths));

    // Reading a different index. With `GIT_INDEX_FILE` unset-to-missing the
    // index is empty, so every tracked path reads as deleted — the one
    // invocation that proves the comparison is against the index and not against
    // HEAD.
    out.push(Case::new("diff-index", &["diff-index", "--cached", "--name-status", "HEAD"], Shape::Linear).with_env(ALT_INDEX));
    out.push(Case::new("diff-files", &["diff-files", "--name-status"], Shape::Linear).with_env(ALT_INDEX));
}
