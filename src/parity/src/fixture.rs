//! Deterministic fixture repositories, built once with *stock* git and then
//! copied per case.
//!
//! Stock git is the builder on purpose: the fixture is the shared premise of a
//! differential run, so it must not depend on the implementation under test.
//! Each shape isolates a class of repository state that porcelain has to read
//! correctly — history, refs, index/worktree divergence, conflicts, and the
//! encoding edge cases that break naive path handling.
//!
//! A case is one argv against a pristine copy, with no pre-step: whatever the
//! command needs on disk has to be in the shape already. That is what the
//! second group of shapes is for. `Attributes`, `Renamed`, `Whitespace`,
//! `Packed`, `Patches` and `Sparse` each carry an input class the first eight
//! could not express — configured rules, a rename, a whitespace-only change, a
//! pack with deltas, a patch file, a sparse worktree — and each one moves the
//! parity number *down* on arrival, because it reaches code the corpus was
//! previously unable to run.

use crate::env;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// A repository shape. Every corpus case names the shape it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Shape {
    /// Single commit, clean worktree. The floor case.
    Linear,
    /// Several commits, two branches, a lightweight and an annotated tag.
    Branched,
    /// A real merge commit with two parents.
    Merged,
    /// Staged, unstaged, and untracked changes coexisting.
    Dirty,
    /// Mid-conflict merge: index has stage 1/2/3 entries.
    Conflicted,
    /// Detached HEAD, as `git submodule update` leaves things.
    Detached,
    /// Unicode, spaces, and quote-worthy bytes in tracked paths.
    AwkwardPaths,
    /// A parent repo with one real submodule — zvcs's stated target topology.
    Submodule,
    /// `.gitattributes`, `.gitignore` and `.mailmap` carrying rules that match
    /// tracked paths, plus commits by the identities the mailmap rewrites.
    ///
    /// Without this, `check-attr`, `check-ignore` and `check-mailmap` are only
    /// ever asked about a repository that configures nothing, so their scores
    /// measure argument parsing and never rule matching.
    Attributes,
    /// A pure rename, a rename-with-edit at a known similarity, a copy, and a
    /// rewrite — one per commit.
    ///
    /// `-M`/`-C`/`-B`/`--find-renames` are pinned for output shape across the
    /// diff family but no shape contained a rename, so detection quality itself
    /// was unmeasured.
    Renamed,
    /// Commits whose only change is whitespace: indentation, trailing blanks,
    /// and line endings — plus one commit mixing a real edit with whitespace
    /// churn, and an unstaged whitespace-only edit in the worktree.
    ///
    /// `-w`/`-b`/`--ignore-*` had nothing to ignore in any other shape.
    Whitespace,
    /// Delta-bearing packs, loose duplicates of packed objects, and pack files
    /// tracked in the worktree at stable paths.
    ///
    /// Makes `verify-pack`, `index-pack` and `prune-packed` reachable on their
    /// real paths rather than only on their error paths, and gives clone/repack
    /// work a pack that actually contains deltas.
    Packed,
    /// Patch, mailbox and quilt-series files on disk: valid, corrupt,
    /// context-only, whitespace-damaging, and binary.
    ///
    /// A case is one argv against a pristine copy and cannot create a file
    /// first, so `apply`, `am` and `quiltimport` had no input to consume.
    Patches,
    /// Cone-mode sparse checkout with one directory excluded from the worktree.
    Sparse,
    /// Branches that `main` can merge — some by fast-forward, some three-way —
    /// over a worktree carrying unstaged edits and an untracked file.
    ///
    /// `Dirty` has dirt but nothing to merge and `Branched` has a branch but a
    /// clean tree, so which paths a merge may write over was unmeasured: a
    /// blanket "is anything dirty" refusal scored the same as git's per-path
    /// one. The dirt is placed deliberately — one edit on a path the branches
    /// rewrite, one on a path none of them touch, and an untracked file exactly
    /// where two of them want to write.
    MergeableDirty,
    /// [`Shape::MergeableDirty`]'s history with a *staged* change instead, on a
    /// path no branch touches.
    ///
    /// The index-vs-`HEAD` gate is the half a fast-forward skips: git refuses a
    /// three-way merge over this and fast-forwards over it happily, and nothing
    /// else in the corpus separates the two.
    MergeableStaged,
    /// Stash entries that already exist, over a worktree that has more to stash.
    ///
    /// Three entries with different insides — an unstaged-only one, one carrying
    /// an untracked file, and one carrying both staged and unstaged work — plus a
    /// current worktree holding a staged change, an unstaged change, an untracked
    /// file and an ignored one. Without pre-existing entries, `stash list/show/
    /// pop/apply/drop/branch` could only ever be measured on their empty-stack
    /// error path, and the flags that decide *what* gets stashed
    /// (`-u`/`-a`/`-k`/`-S`/`--`) had nothing to sort.
    Stashed,
    /// A tracking branch behind a real remote, over a dirty worktree.
    ///
    /// The remote is a bare repository *inside* the fixture (`.remote.git`, hidden
    /// from status through `info/exclude`) reached by a relative URL, so it
    /// survives the per-case copy and every case gets its own. `main` is three
    /// commits behind `origin/main` and fast-forwardable; `div` has moved on both
    /// sides. The worktree keeps an unstaged edit to a file the remote never
    /// touches (so a fast-forward must still succeed) and one to a file `div`
    /// rewrites (so that merge must refuse per path) — the two halves of the
    /// dirty-pull question that shipped broken twice.
    BehindRemote,
    /// A second, *linked* worktree of the same repository: `wt/` beside the main
    /// worktree, with its administrative directory at `.git/worktrees/wt`.
    ///
    /// The one repository layout in which `--git-dir` and `--git-common-dir`
    /// answer differently, and the only one where `HEAD` is read from somewhere
    /// other than the common directory — `wt` is on its own branch, so a
    /// discovery path that resolves the common `HEAD` reports the wrong branch
    /// while every other shape hides the mistake. No existing shape can express
    /// it: a linked worktree has to be created by `worktree add`, which a case
    /// (one argv against a pristine copy) cannot do, and adding one to an
    /// existing shape would change what every case already using that shape
    /// sees.
    ///
    /// Two construction details are load-bearing and are explained where they
    /// are done in [`build`]: the worktree is registered with relative paths so
    /// the per-case copy points at itself rather than at the template, and its
    /// index is rewritten by `read-tree` so the shape hashes the same at two
    /// build locations.
    Worktree,
    /// A merge with four parents, with one branch left unmerged beside it.
    ///
    /// `--graph` draws a merge past the second parent with rows no two-parent
    /// merge produces — the commit row's `*---.` reach, the `|\ \ \` post-merge
    /// row, and the expansion rows that open space around a merge that is not the
    /// rightmost column. [`Shape::Merged`] is the only shape carrying a merge at
    /// all and it has exactly two parents, so every one of those rows was
    /// unmeasured by the corpus.
    ///
    /// No existing shape can express it: an octopus needs a commit with three or
    /// more parents and no shape has one (`fixture.rs` runs `merge` twice in the
    /// whole file, both two-way), and a case is one argv against a pristine copy
    /// so it cannot create the merge itself. Adding the merge to `Merged` would
    /// change what every case already on that shape sees.
    ///
    /// `oct-side` forks before the merge and is never merged, so `--all` keeps a
    /// lane to the octopus's right and the expansion rows are reached; without it
    /// the merge is the last column and git skips them.
    Octopus,
    /// Two directory trees to diff *against each other*, the two degenerate
    /// halves of that comparison, and a repository config carrying a
    /// non-default `core.abbrev`. Everything lives under `ni/`.
    ///
    /// `diff --no-index` reads both sides off the filesystem and never opens an
    /// object store, so nothing a repository *contains* can stand in for its
    /// input — and a case is one argv against a pristine copy, so it cannot
    /// write the trees first.
    ///
    /// Three queue shapes, because the blob ids `--raw` prints are a function of
    /// which one it is. `diffcore_rename()` returns before its hashing pass
    /// unless the queue holds both a source and a destination
    /// (diffcore-rename.c:1461-1462), and in no-index mode that pass is the only
    /// thing that ever fills an id in. So `da`/`db` — a modified file, a
    /// left-only file and a right-only file — print real ids for the delete and
    /// the add, while `addonly_a`/`addonly_b` and `delonly_a`/`delonly_b` are
    /// add-only and delete-only and legitimately keep zeros. A fixture carrying
    /// only the first of the three would score a port that hashes
    /// unconditionally as correct.
    ///
    /// `core.abbrev = 10` sits in the repository config so the width those ids
    /// are printed at is a *configured* value rather than the built-in default.
    /// Every other shape is small enough that git's `auto` width and the
    /// hard-coded 7 coincide, which is exactly what let an implementation that
    /// ignored the setting pass.
    ///
    /// The two empty directories are empty on purpose: an add-only queue needs a
    /// side with nothing in it, and git cannot track an empty directory, so they
    /// survive only because the per-case copy is a directory walk rather than a
    /// checkout.
    NoIndexTrees,
    /// A tracked path whose on-disk name is *decomposed*: `e` followed by
    /// U+0301 COMBINING ACUTE ACCENT, not the single code point `é`.
    ///
    /// macOS hands decomposed names out of `readdir()` and, through shell
    /// completion, into `argv`; git composes both back before anything compares
    /// them (`compat/precompose_utf8.c`, gated on `core.precomposeunicode`). No
    /// other shape carries a combining mark at all — [`Shape::AwkwardPaths`]
    /// writes `üñïçødé.txt` in the composed form — so neither the conversion nor
    /// its absence had a fixture.
    ///
    /// Portable by construction rather than by luck. The conversion is
    /// `#ifdef PRECOMPOSE_UNICODE` in git (`git-compat-util.h:167-179` supplies
    /// pass-through inlines otherwise, and `config.mak.uname:156` defines the
    /// macro inside the `ifeq ($(uname_S),Darwin)` block alone) and
    /// `cfg(target_os = "macos")` in the
    /// port (`extensions/src/precompose.rs:46,202`). On Linux both sides leave
    /// the bytes alone and agree on the decomposed answer, exactly as they agree
    /// on the composed one on macOS.
    ///
    /// One file is tracked and edited in the worktree and one is untracked, so
    /// `status` has to name the path from both directions — through the index
    /// and through the directory walk.
    DecomposedPaths,
    /// A repository that has hooks installed, and a subdirectory to run from.
    ///
    /// No shape carried a hook, so every case ran against a repository where
    /// `.git/hooks` was empty — and the mere *existence* of a hook is enough to
    /// change what several verbs do. It hid a total failure: committing from a
    /// subdirectory of a repository with any hook at all exited 1 with
    /// `No such file or directory`, because the hook's path and working
    /// directory were resolved relative to a cwd that had already moved. A
    /// `pre-commit` that does nothing but `exit 0` reproduces it, so this shape
    /// needs no clever hook to be worth its build cost — it needs only to have
    /// one, and a directory to be inside.
    Hooked,
    /// Three root commits in one repository: `main`'s, and two orphan branches
    /// that share no ancestor with it or with each other.
    ///
    /// Every other shape descends from the one `initial` commit
    /// (`edfab1b71619a22120a8da1a3d85d68e0200290a` in all of them), so a pair of
    /// revisions with **no** merge base could not be named at all. That made a
    /// whole family unreachable: `merge-base` returning exit 1 with no output,
    /// `merge`/`pull` refusing with `refusing to merge unrelated histories` and
    /// then being told `--allow-unrelated-histories`, `rev-list --not` over
    /// disjoint graphs, `format-patch` across roots, and
    /// `rev-list --max-parents=0` finding more than one root.
    ///
    /// The two orphans differ in what they collide with, because the allowed
    /// merge has two outcomes and one shape has to reach both: `alien` shares no
    /// path with `main`, so `merge --allow-unrelated-histories alien` is clean;
    /// `alien-clash` carries its own `README.md`, so the same merge is an
    /// add/add conflict on a path that has no common ancestor to diff against.
    Unrelated,
    /// A criss-cross: two branches that each merged the other, so their two
    /// merge bases are incomparable.
    ///
    /// No other shape has one, which left three things unmeasurable.
    /// `merge-base --all` could never return more than one id, so an
    /// implementation that stops at the first base scored the same as one that
    /// enumerates them; `merge-base --independent` had nothing to prune; and the
    /// recursive strategy's virtual-merge-base path — merging the bases with
    /// each other to build the base it then merges against, the most intricate
    /// path in `merge-ort` — was never entered.
    ///
    /// `clash.txt` is what forces that path to *show*: the two bases disagree on
    /// it (`a` against `b`), so the virtual base is itself a conflicted merge and
    /// stage 1 of the outer conflict holds a blob that exists in no commit —
    /// stock writes conflict markers there. A port that picks one of the two
    /// bases instead leaves `a`, `b` or `base` in stage 1 and is caught by the
    /// state probe even though its stdout matches. `cc.txt` merges cleanly
    /// through the same virtual base, and `calm.txt` is untouched by anything, so
    /// one merge exercises all three outcomes.
    CrissCross,
    /// The same patch on both sides of a fork, applied by `cherry-pick` rather
    /// than shared through history.
    ///
    /// No fixture carried one commit's patch id twice, so `cherry`'s `-` marker,
    /// `rev-list --cherry-mark`'s `=` class and `--cherry-pick`'s omission were
    /// all unreachable — every case could only ever produce `+`, `<` and `>` —
    /// and `rebase`'s `skipped previously applied commit` path was never taken.
    ///
    /// `topic` forks at `cherry: seed`, commits once so the cherry-pick lands on
    /// a different parent (without that the copy is byte-identical to the
    /// original and the two branches share the commit rather than duplicating
    /// its patch), picks `main`'s `cherry: shared patch`, then commits again. So
    /// each side holds one commit the other does not, plus one whose patch id
    /// both have.
    Cherry,
    /// Symlink entries and zero-byte blobs, tracked, untracked, and inside a
    /// patch.
    ///
    /// No shape wrote either. Mode `120000` never appeared in `ls-files
    /// --stage`, so `checkout`, `archive` and `apply` were only ever asked about
    /// regular files, and `cat-file --follow-symlinks` had nothing to follow —
    /// its four answers (a resolved blob, `dangling`, a `symlink` that leaves the
    /// tree, and resolution *through* a symlinked directory) are one shape's
    /// worth of fixture apart from unreachable. The empty blob
    /// (`e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`) is a constant of the hash
    /// function that no fixture had ever stored, so `--batch-check` never saw a
    /// zero-length object and `apply` never created one.
    ///
    /// Every symlink target is relative. An absolute one would bake the build
    /// directory into a tracked blob and the shape would stop being reproducible
    /// at a second location — `link-escape` points *out* of the worktree by
    /// relative path, which is what makes the out-of-tree answer reachable
    /// without naming a machine.
    ///
    /// `sym-pending` carries the changes `patches/symlink.patch` describes, so
    /// the patch applies to `main`'s tree: a new symlink, a new empty file, and a
    /// regular file replaced by a symlink (`T` in `--raw`). A case is one argv
    /// against a pristine copy and cannot write a patch first, which is the same
    /// reason [`Shape::Patches`] exists.
    Symlinks,
    /// A written commit-graph, with one commit deliberately left out of it.
    ///
    /// `fixture.rs` never ran `commit-graph write`, so `core.commitGraph` chose
    /// between two code paths over the same objects and could not change an
    /// answer — the setting was measurable only as an argument-parsing question.
    /// `commit-graph verify`, `--changed-paths` (the Bloom filters `log -- path`
    /// consults) and the generation-number traversal had no file to read.
    ///
    /// `cg-late` is committed *after* the write, so the graph is valid but
    /// incomplete: a traversal has to fall back to reading that commit's object
    /// and to mixing graph-supplied generation numbers with computed ones, which
    /// a graph covering every commit never asks of it. `cg-side` is merged and
    /// `cg-loose` is not, so the graph holds a merge and a fork.
    ///
    /// The file is byte-identical between two builds: it is a function of the
    /// object ids and the write options alone.
    CommitGraph,
    /// A repository with four things wrong with it, none of which stops git
    /// from operating.
    ///
    /// `fsck`, `gc`, `prune` and `rev-parse --verify` ran only on healthy
    /// repositories, so "what does the port do when the repository is damaged"
    /// had no fixture at all — and the answers are not uniform, which is what
    /// makes them worth pinning: `rev-parse --verify` *succeeds* on a ref
    /// pointing at a missing object and prints the id, while `show-ref` and
    /// `for-each-ref` fail with exit 128; `rev-parse --verify` on a dangling
    /// symref warns and fails; `branch --list` prints one of the two broken refs
    /// and hides the other. A port that treats "broken" as one condition gets
    /// several of those wrong.
    ///
    /// The four:
    ///
    /// * `refs/heads/dangling` — a well-formed id no object has.
    /// * `refs/heads/broken-symref` — a symref to a branch that does not exist.
    /// * a loose object file whose contents are not a zlib stream, at a path
    ///   whose name is a valid object id.
    /// * an empty line in `.git/objects/info/alternates`.
    ///
    /// The alternates entry is empty rather than a path that does not exist, and
    /// that is a constraint rather than a preference: a missing alternate makes
    /// git print the *absolute* path of the directory it could not find on
    /// stderr, and the two sides of a case run in two different copies, so the
    /// diagnostic would differ by construction and the case would measure the
    /// harness. An empty entry exercises the same parser and stays silent.
    Damaged,
}

impl Shape {
    pub const ALL: &'static [Shape] = &[
        Shape::Linear,
        Shape::Branched,
        Shape::Merged,
        Shape::Dirty,
        Shape::Conflicted,
        Shape::Detached,
        Shape::AwkwardPaths,
        Shape::Submodule,
        Shape::Attributes,
        Shape::Renamed,
        Shape::Whitespace,
        Shape::Packed,
        Shape::Patches,
        Shape::Sparse,
        Shape::MergeableDirty,
        Shape::MergeableStaged,
        Shape::Stashed,
        Shape::BehindRemote,
        Shape::Worktree,
        Shape::Octopus,
        Shape::NoIndexTrees,
        Shape::DecomposedPaths,
        Shape::Hooked,
        Shape::Unrelated,
        Shape::CrissCross,
        Shape::Cherry,
        Shape::Symlinks,
        Shape::CommitGraph,
        Shape::Damaged,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Shape::Linear => "linear",
            Shape::Branched => "branched",
            Shape::Merged => "merged",
            Shape::Dirty => "dirty",
            Shape::Conflicted => "conflicted",
            Shape::Detached => "detached",
            Shape::AwkwardPaths => "awkward-paths",
            Shape::Submodule => "submodule",
            Shape::Attributes => "attributes",
            Shape::Renamed => "renamed",
            Shape::Whitespace => "whitespace",
            Shape::Packed => "packed",
            Shape::Patches => "patches",
            Shape::Sparse => "sparse",
            Shape::MergeableDirty => "mergeable-dirty",
            Shape::MergeableStaged => "mergeable-staged",
            Shape::Stashed => "stashed",
            Shape::BehindRemote => "behind-remote",
            Shape::Worktree => "worktree",
            Shape::Octopus => "octopus",
            Shape::NoIndexTrees => "no-index-trees",
            Shape::DecomposedPaths => "decomposed-paths",
            Shape::Hooked => "hooked",
            Shape::Unrelated => "unrelated",
            Shape::CrissCross => "criss-cross",
            Shape::Cherry => "cherry",
            Shape::Symlinks => "symlinks",
            Shape::CommitGraph => "commit-graph",
            Shape::Damaged => "damaged",
        }
    }
}

/// Run stock git in `dir`, failing loudly on non-zero exit.
///
/// Fixture construction has no tolerance for partial success: a half-built
/// premise would silently weaken every case that uses it.
fn git(dir: &Path, home: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = crate::stock::command()?;
    env::harden(&mut cmd, home);
    cmd.current_dir(dir).args(args);
    let out = cmd
        .output()
        .with_context(|| format!("spawn stock git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "fixture: stock git {args:?} in {} failed ({})\n{}",
            dir.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn write(dir: &Path, rel: &str, body: &str) -> Result<()> {
    write_bytes(dir, rel, body.as_bytes())
}

fn write_bytes(dir: &Path, rel: &str, body: &[u8]) -> Result<()> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))
}

/// `n` numbered lines. Generated rather than literal so similarity indices are
/// arithmetic rather than eyeballed: editing `k` of `n` lines is a `k/n` change.
fn numbered(prefix: &str, n: usize) -> String {
    numbered_with_edits(prefix, n, &[])
}

/// `numbered`, with the listed 1-based lines rewritten so they no longer match.
fn numbered_with_edits(prefix: &str, n: usize, edited: &[usize]) -> String {
    (1..=n)
        .map(|i| {
            if edited.contains(&i) {
                format!("{prefix} line {i} edited\n")
            } else {
                format!("{prefix} line {i}\n")
            }
        })
        .collect()
}

/// The single `*.pack` under `.git/objects/pack`, which `repack -ad` has just
/// left as the only one. Fails loudly rather than guessing: every consumer of
/// this path needs the pack to exist.
fn sole_pack(dir: &Path) -> Result<PathBuf> {
    let pack_dir = dir.join(".git").join("objects").join("pack");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&pack_dir)
        .with_context(|| format!("read {}", pack_dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pack"))
        .collect();
    found.sort();
    match found.len() {
        1 => Ok(found.remove(0)),
        n => bail!("fixture: expected exactly one pack in {}, found {n}", pack_dir.display()),
    }
}

/// Build `shape` at `dir`. `home` is the hermetic HOME for the build commands.
pub fn build(shape: Shape, dir: &Path, home: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    // `-b main` pins the initial branch so the fixture does not inherit the
    // host's `init.defaultBranch` — another way the machine leaks in.
    git(dir, home, &["init", "-q", "-b", "main"])?;

    // A case runs against a *copy* of the template, and a copy gets fresh inodes
    // and creation times no matter how carefully it is made. Git's default stat
    // comparison includes both, so every copied fixture looked stat-dirty to the
    // commands that check the index without refreshing it first — `git apply
    // --index` and, through it, `git quiltimport` — which failed with
    // `does not match index` on *both* sides and so scored as agreement while
    // measuring nothing. `minimal` drops inode and ctime from the comparison;
    // `copy_tree` carries the mtime across so the remaining fields still match.
    // This compensates for an artifact of the harness, not for anything the
    // implementation under test does.
    git(dir, home, &["config", "core.checkStat", "minimal"])?;

    write(dir, "README.md", "# fixture\n")?;
    write(dir, "src/lib.rs", "pub fn one() -> u32 { 1 }\n")?;
    git(dir, home, &["add", "."])?;
    git(dir, home, &["commit", "-q", "-m", "initial"])?;

    match shape {
        Shape::Linear => {}

        Shape::Branched => {
            write(dir, "src/lib.rs", "pub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\n")?;
            git(dir, home, &["commit", "-qam", "add two"])?;
            git(dir, home, &["tag", "v0.1.0"])?;
            git(dir, home, &["tag", "-a", "v0.2.0", "-m", "annotated"])?;
            git(dir, home, &["branch", "feature"])?;
            git(dir, home, &["checkout", "-q", "feature"])?;
            write(dir, "feature.txt", "feature work\n")?;
            git(dir, home, &["add", "feature.txt"])?;
            git(dir, home, &["commit", "-qm", "feature commit"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
        }

        Shape::Merged => {
            git(dir, home, &["checkout", "-q", "-b", "side"])?;
            write(dir, "side.txt", "side\n")?;
            git(dir, home, &["add", "side.txt"])?;
            git(dir, home, &["commit", "-qm", "side commit"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            write(dir, "main.txt", "main\n")?;
            git(dir, home, &["add", "main.txt"])?;
            git(dir, home, &["commit", "-qm", "main commit"])?;
            git(dir, home, &["merge", "--no-ff", "-m", "merge side", "side"])?;
        }

        Shape::Dirty => {
            // One of each: staged, unstaged, untracked, and a deletion.
            write(dir, "staged.txt", "staged\n")?;
            git(dir, home, &["add", "staged.txt"])?;
            write(dir, "README.md", "# fixture\nmodified, not staged\n")?;
            write(dir, "untracked.txt", "untracked\n")?;
            std::fs::remove_file(dir.join("src/lib.rs"))?;
        }

        Shape::Conflicted => {
            git(dir, home, &["checkout", "-q", "-b", "theirs"])?;
            write(dir, "conflict.txt", "theirs\n")?;
            git(dir, home, &["add", "conflict.txt"])?;
            git(dir, home, &["commit", "-qm", "theirs"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            write(dir, "conflict.txt", "ours\n")?;
            git(dir, home, &["add", "conflict.txt"])?;
            git(dir, home, &["commit", "-qm", "ours"])?;
            // Expected to exit non-zero — that *is* the state being built.
            let mut cmd = crate::stock::command()?;
            env::harden(&mut cmd, home);
            cmd.current_dir(dir).args(["merge", "theirs"]);
            let out = cmd.output()?;
            if out.status.success() {
                bail!("fixture: conflicted merge unexpectedly succeeded");
            }
        }

        Shape::Detached => {
            write(dir, "second.txt", "second\n")?;
            git(dir, home, &["add", "second.txt"])?;
            git(dir, home, &["commit", "-qm", "second"])?;
            git(dir, home, &["checkout", "-q", "--detach", "HEAD~1"])?;
        }

        Shape::AwkwardPaths => {
            write(dir, "with space.txt", "space\n")?;
            write(dir, "üñïçødé.txt", "unicode\n")?;
            write(dir, "quote\"name.txt", "quote\n")?;
            write(dir, "nested/deep/path.txt", "deep\n")?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-qm", "awkward paths"])?;
        }

        Shape::Submodule => {
            // A real submodule needs a real upstream; build one beside the parent.
            let upstream = dir.join("..").join(format!("{}-upstream", dir.file_name().unwrap().to_string_lossy()));
            std::fs::create_dir_all(&upstream)?;
            let upstream = upstream.canonicalize()?;
            git(&upstream, home, &["init", "-q", "-b", "main"])?;
            write(&upstream, "mod.txt", "submodule content\n")?;
            git(&upstream, home, &["add", "."])?;
            git(&upstream, home, &["commit", "-qm", "submodule initial"])?;

            git(
                dir,
                home,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "-q",
                    upstream.to_str().context("upstream path not utf-8")?,
                    "sub",
                ],
            )?;
            git(dir, home, &["commit", "-qm", "add submodule"])?;
        }

        Shape::Attributes => {
            // Tracked paths first: every rule below matches something real, so
            // `check-attr`/`check-ignore` are asked about the matching path set
            // and not only about the "nothing configured" answer.
            write(dir, "src/tabs.rs", "fn indented() {\n\tlet x = 1;\n}\n")?;
            write(dir, "docs/manual.md", "# manual\n\nprose\n")?;
            write(dir, "vendor/generated.js", "// generated, do not edit\n")?;
            write(dir, "sub/nested.txt", "nested\n")?;
            write(dir, "logs/keep.log", "tracked even though *.log is ignored\n")?;
            write_bytes(dir, "assets/logo.bin", &binary_blob(0))?;

            write(
                dir,
                ".gitattributes",
                // Later rules win, so the `binary` macro has to follow `text=auto`.
                "* text=auto\n\
                 *.rs text eol=lf whitespace=tab-in-indent,trailing-space\n\
                 *.md diff=markdown export-ignore\n\
                 *.log -diff\n\
                 vendor/** linguist-generated -diff -merge\n\
                 assets/*.bin binary\n\
                 sub/nested.txt merge=union\n\
                 missing-attr.txt !text\n",
            )?;
            // A nested file overrides the root for its own subtree — precedence
            // is the part of attribute lookup a single file cannot exercise.
            write(dir, "sub/.gitattributes", "nested.txt -diff eol=crlf\n*.txt text\n")?;
            // `.git/info/attributes` outranks both, and lives outside the worktree.
            write(dir, ".git/info/attributes", "*.info ident\ninfo-only.txt text\n")?;

            write(
                dir,
                ".gitignore",
                "*.log\n!important.log\nbuild/\n/notes.tmp\n**/deep-ignored/\n*.o\n",
            )?;
            write(dir, "sub/.gitignore", "!*.log\nlocal-*\n")?;
            write(dir, ".git/info/exclude", "excluded-by-info.txt\n")?;

            write(
                dir,
                ".mailmap",
                "Proper Name <proper@example.invalid> <old@example.invalid>\n\
                 Proper Name <proper@example.invalid> Alias Name <alias@example.invalid>\n\
                 <canonical@example.invalid> Typo Name <typo@example.invalid>\n\
                 Solo Name <solo@example.invalid>\n",
            )?;

            git(dir, home, &["add", "-A"])?;
            // Ignored by its own rule, so it needs `-f` — and being tracked is
            // exactly what makes it interesting to `check-ignore`.
            git(dir, home, &["add", "-f", "logs/keep.log"])?;
            git(dir, home, &["commit", "-qm", "attributes: rules and subjects"])?;

            // Commits by the identities `.mailmap` rewrites. `--author` beats the
            // hardened environment for the author field; the committer stays
            // pinned, which is what keeps the ids reproducible.
            write(dir, "sub/nested.txt", "nested\nsecond line\n")?;
            git(
                dir,
                home,
                &["commit", "-qam", "attributes: by old address", "--author=Old Name <old@example.invalid>"],
            )?;
            write(dir, "docs/manual.md", "# manual\n\nprose\nmore prose\n")?;
            git(
                dir,
                home,
                &["commit", "-qam", "attributes: by alias", "--author=Alias Name <alias@example.invalid>"],
            )?;
            write(dir, "src/tabs.rs", "fn indented() {\n\tlet x = 2;\n}\n")?;
            git(
                dir,
                home,
                &["commit", "-qam", "attributes: by typo", "--author=Typo Name <typo@example.invalid>"],
            )?;

            // Untracked subjects for the ignore rules, written after the commit
            // so `add -A` above could not swallow them.
            write(dir, "build/output.o", "ignored by two rules\n")?;
            write(dir, "logs/debug.log", "ignored\n")?;
            write(dir, "important.log", "un-ignored by the negation\n")?;
            write(dir, "notes.tmp", "ignored, anchored to the root\n")?;
            write(dir, "sub/deep-ignored/thing.txt", "ignored by the ** rule\n")?;
            write(dir, "sub/local-scratch.txt", "ignored by the nested rule\n")?;
            write(dir, "excluded-by-info.txt", "ignored by .git/info/exclude\n")?;
            write(dir, "tracked-looking.txt", "not ignored at all\n")?;
        }

        Shape::Renamed => {
            write(dir, "orig/alpha.txt", &numbered("alpha", 40))?;
            write(dir, "orig/beta.txt", &numbered("beta", 40))?;
            write(dir, "orig/gamma.txt", &numbered("gamma", 40))?;
            write(dir, "orig/delta.txt", &numbered("delta", 40))?;
            git(dir, home, &["add", "orig"])?;
            git(dir, home, &["commit", "-qm", "renames: seed"])?;

            // 100% similarity: content identical, path changed.
            std::fs::create_dir_all(dir.join("moved"))?;
            git(dir, home, &["mv", "orig/alpha.txt", "moved/alpha.txt"])?;
            git(dir, home, &["commit", "-qm", "renames: pure rename"])?;

            // 8 of 40 lines rewritten, which stock scores as `R072` — under
            // `-M90%`, over `-M50%`. The similarity index is what separates a
            // correct threshold implementation from one that ignores the
            // threshold, so it has to be a known value rather than whatever an
            // ad-hoc edit happens to produce.
            git(dir, home, &["mv", "orig/beta.txt", "moved/beta.txt"])?;
            write(
                dir,
                "moved/beta.txt",
                &numbered_with_edits("beta", 40, &[3, 7, 11, 15, 19, 23, 27, 31]),
            )?;
            git(dir, home, &["commit", "-qam", "renames: rename with edit"])?;

            // A copy whose source is modified in the same commit, which is what
            // plain `-C` (without `--find-copies-harder`) is able to see.
            write(dir, "copies/gamma.txt", &numbered("gamma", 40))?;
            write(dir, "orig/gamma.txt", &numbered_with_edits("gamma", 40, &[5, 10]))?;
            git(dir, home, &["add", "-A"])?;
            git(dir, home, &["commit", "-qm", "renames: copy with modified source"])?;

            // Same path, entirely different content: the rewrite `-B` splits.
            write(dir, "orig/delta.txt", &numbered("rewritten", 40))?;
            git(dir, home, &["commit", "-qam", "renames: rewrite in place"])?;
        }

        Shape::Whitespace => {
            write(dir, "ws/indent.c", WS_TABS)?;
            write(dir, "ws/eol.txt", "alpha\r\nbeta\r\ngamma\r\n")?;
            git(dir, home, &["add", "ws"])?;
            git(dir, home, &["commit", "-qm", "whitespace: seed"])?;

            write(dir, "ws/indent.c", WS_SPACES)?;
            git(dir, home, &["commit", "-qam", "whitespace: tabs to spaces"])?;

            write(dir, "ws/indent.c", WS_TRAILING)?;
            git(dir, home, &["commit", "-qam", "whitespace: trailing blanks"])?;

            write(dir, "ws/eol.txt", "alpha\nbeta\ngamma\n")?;
            git(dir, home, &["commit", "-qam", "whitespace: crlf to lf"])?;

            // One real edit surrounded by whitespace churn. `-w` has to drop the
            // churn and keep the edit — and take its context from the *post*
            // image, which is the class of bug no other shape can reach.
            write(dir, "ws/indent.c", WS_MIXED)?;
            git(dir, home, &["commit", "-qam", "whitespace: one edit amid churn"])?;

            // Unstaged and whitespace-only, so bare `git diff` — the most-run
            // diff of all — has something for `-w`/`-b` to ignore.
            write(dir, "ws/indent.c", WS_REINDENTED)?;
        }

        Shape::Packed => {
            // Seven revisions of one 400-line file: successive blobs share most
            // of their content, which is what gives pack-objects deltas to find.
            for rev in 0..7usize {
                let edits: Vec<usize> = (1..=rev).map(|k| k * 37).collect();
                write(dir, "big.txt", &numbered_with_edits("payload", 400, &edits))?;
                if rev == 0 {
                    git(dir, home, &["add", "big.txt"])?;
                }
                let msg = format!("packed: revision {rev}");
                git(dir, home, &["commit", "-qam", &msg])?;
            }

            // `pack.threads=1` pins delta selection. The threaded search splits
            // the object window by thread count, so without this the pack bytes
            // depend on the builder's core count and the shape stops being
            // reproducible on another machine.
            git(dir, home, &["-c", "pack.threads=1", "repack", "-adq"])?;

            // A pack file name embeds the pack's own checksum, so no case can
            // name one in argv without being rewritten whenever the fixture
            // changes by a byte. Stable copies in the worktree fix that, and put
            // `index-pack`'s output where the state probe can see it.
            let pack = sole_pack(dir)?;
            std::fs::create_dir_all(dir.join("packs"))?;
            std::fs::copy(&pack, dir.join("packs/sample.pack"))?;
            std::fs::copy(pack.with_extension("idx"), dir.join("packs/sample.idx"))?;
            std::fs::copy(&pack, dir.join("packs/unindexed.pack"))?;
            git(dir, home, &["add", "packs"])?;
            git(dir, home, &["commit", "-qm", "packed: pack files at stable paths"])?;

            // A second pack, *without* `-d`, so loose copies of packed objects
            // survive and `prune-packed` has real work rather than a no-op.
            git(dir, home, &["-c", "pack.threads=1", "repack", "-q"])?;

            // An object no ref reaches, for `prune`, `fsck --unreachable` and
            // `count-objects -v`. The reflog is expired because it would
            // otherwise keep the commit reachable and hide the state.
            write(dir, "orphan.txt", "unreachable once the reset lands\n")?;
            git(dir, home, &["add", "orphan.txt"])?;
            git(dir, home, &["commit", "-qm", "packed: soon unreachable"])?;
            git(dir, home, &["reset", "-q", "--hard", "HEAD~1"])?;
            git(dir, home, &["reflog", "expire", "--expire=all", "--all"])?;
        }

        Shape::Patches => {
            write(dir, "app/main.c", MAIN_C_BASE)?;
            write_bytes(dir, "app/data.bin", &binary_blob(1))?;
            git(dir, home, &["add", "app"])?;
            git(dir, home, &["commit", "-qm", "patches: seed"])?;

            // The changes the patches carry live on a side branch, so main's
            // tree stays the pre-image every patch applies to and no object is
            // left dangling.
            git(dir, home, &["checkout", "-q", "-b", "pending"])?;
            write(dir, "app/main.c", MAIN_C_ONE)?;
            git(dir, home, &["commit", "-qam", "patches: add subtract"])?;
            write(dir, "app/main.c", MAIN_C_TWO)?;
            write_bytes(dir, "app/data.bin", &binary_blob(2))?;
            git(dir, home, &["commit", "-qam", "patches: bump version and data"])?;
            git(dir, home, &["checkout", "-q", "main"])?;

            // `--no-signature` drops git's own version number from the trailer,
            // which would otherwise put the builder's git build into tracked
            // content and make the fixture non-reproducible across machines.
            let mbox = git(
                dir,
                home,
                &["format-patch", "--no-signature", "--binary", "--stdout", "main..pending"],
            )?;
            write(dir, "mail/series.mbox", &mbox)?;
            let single = git(
                dir,
                home,
                &["format-patch", "--no-signature", "--stdout", "-1", "pending~1"],
            )?;
            write(dir, "mail/one.eml", &single)?;

            let valid = git(dir, home, &["diff", "main", "pending~1", "--", "app/main.c"])?;
            write(dir, "patches/valid.patch", &valid)?;
            let second = git(dir, home, &["diff", "pending~1", "pending", "--", "app/main.c"])?;
            let binary = git(dir, home, &["diff", "--binary", "main", "pending", "--", "app/data.bin"])?;
            write(dir, "patches/binary.patch", &binary)?;
            write(dir, "patches/corrupt.patch", CORRUPT_PATCH)?;
            write(dir, "patches/context-only.patch", CONTEXT_ONLY_PATCH)?;
            write(dir, "patches/whitespace.patch", WHITESPACE_PATCH)?;
            write(dir, "patches/offset.patch", OFFSET_PATCH)?;

            // quilt keeps its patch order in a `series` file beside the patches.
            // The `From:`/`Subject:` headers are what stop `quiltimport` from
            // stopping to ask for an author, which it does by reading stdin —
            // and a case that blocks on stdin measures nothing.
            write(dir, "quilt/series", "0001-first.patch\n0002-second.patch\n")?;
            write(
                dir,
                "quilt/0001-first.patch",
                &format!(
                    "From: {name} <{email}>\nSubject: first quilt patch\n\nAdds subtract().\n\n{valid}",
                    name = env::AUTHOR_NAME,
                    email = env::AUTHOR_EMAIL,
                ),
            )?;
            write(
                dir,
                "quilt/0002-second.patch",
                &format!(
                    "From: {name} <{email}>\nSubject: second quilt patch\n\nBumps VERSION.\n\n{second}",
                    name = env::AUTHOR_NAME,
                    email = env::AUTHOR_EMAIL,
                ),
            )?;

            git(dir, home, &["add", "mail", "patches", "quilt"])?;
            git(dir, home, &["commit", "-qm", "patches: fixtures"])?;
        }

        Shape::Sparse => {
            write(dir, "inside/keep.txt", "kept by the cone\n")?;
            write(dir, "inside/nested/also.txt", "kept, nested\n")?;
            write(dir, "outside/drop.txt", "excluded from the worktree\n")?;
            write(dir, "outside/nested/deep.txt", "excluded, nested\n")?;
            write(dir, "root.txt", "root files stay in a cone checkout\n")?;
            git(dir, home, &["add", "-A"])?;
            git(dir, home, &["commit", "-qm", "sparse: seed"])?;

            git(dir, home, &["sparse-checkout", "init", "--cone"])?;
            git(dir, home, &["sparse-checkout", "set", "inside"])?;

            // An untracked file inside the excluded cone. `status`, `clean` and
            // `add` each have to decide what a sparse-excluded path means, and
            // `rm` on the *tracked* excluded path is a fixed bug this pins.
            write(dir, "outside/stray.txt", "untracked, inside the excluded cone\n")?;
        }

        Shape::Stashed => {
            // Three commits give the entries something to sit on top of.
            write(dir, "counter.txt", "1\n")?;
            write(dir, "notes.txt", "notes\n")?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-q", "-m", "add counter and notes"])?;

            // Entry @{2}: unstaged only.
            write(dir, "counter.txt", "1\nstashed-unstaged\n")?;
            git(dir, home, &["stash", "push", "-m", "unstaged only"])?;
            // Entry @{1}: carries an untracked file, which only `-u` picks up.
            write(dir, "extra.txt", "untracked, stashed with -u\n")?;
            git(dir, home, &["stash", "push", "-u", "-m", "with untracked"])?;
            // Entry @{0}: staged *and* unstaged work, so `--index` has something
            // to restore and `--keep-index` something to keep.
            write(dir, "notes.txt", "notes\nstaged\n")?;
            git(dir, home, &["add", "notes.txt"])?;
            write(dir, "notes.txt", "notes\nstaged\nunstaged\n")?;
            git(dir, home, &["stash", "push", "-m", "staged and unstaged"])?;

            // And a current worktree with one of each, so a fresh `stash push`
            // has all four kinds of content to decide about.
            write(dir, ".gitignore", "ignored.txt\n")?;
            git(dir, home, &["add", ".gitignore"])?;
            git(dir, home, &["commit", "-q", "-m", "ignore ignored.txt"])?;
            write(dir, "counter.txt", "1\nworktree-unstaged\n")?;
            write(dir, "notes.txt", "notes\nworktree-staged\n")?;
            git(dir, home, &["add", "notes.txt"])?;
            write(dir, "notes.txt", "notes\nworktree-staged\nthen-unstaged\n")?;
            write(dir, "fresh.txt", "untracked in the worktree\n")?;
            write(dir, "ignored.txt", "ignored in the worktree\n")?;
        }

        Shape::BehindRemote => {
            write(dir, "shared.txt", "shared, the remote rewrites this\n")?;
            write(dir, "mine.txt", "mine, the remote never touches this\n")?;
            write(dir, "clash.txt", "clash, div rewrites this\n")?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-q", "-m", "add shared, mine and clash"])?;

            // The remote lives inside the fixture so the per-case copy carries it,
            // and is reached by a relative URL so the copy's own one is used.
            git(dir, home, &["init", "-q", "--bare", ".remote.git"])?;
            git(dir, home, &["remote", "add", "origin", "./.remote.git"])?;
            git(dir, home, &["push", "-q", "origin", "main"])?;
            git(dir, home, &["branch", "--set-upstream-to=origin/main", "main"])?;

            // Advance the remote's `main` by three commits, none of them touching
            // `mine.txt`, so a fast-forward over a dirty `mine.txt` must succeed.
            git(dir, home, &["checkout", "-q", "-b", "upstream-work"])?;
            for n in ["2", "3", "4"] {
                write(dir, "shared.txt", &format!("shared, the remote rewrites this\nupstream {n}\n"))?;
                git(dir, home, &["commit", "-qam", &format!("upstream {n}")])?;
            }
            git(dir, home, &["push", "-q", "origin", "upstream-work:main"])?;

            // `div` diverges: the remote rewrites `clash.txt`, the local side adds
            // its own commit, so a pull has to merge and then refuse on the path
            // the worktree is holding dirty.
            git(dir, home, &["checkout", "-q", "-b", "div", "main"])?;
            write(dir, "clash.txt", "clash, div rewrites this\nremote side\n")?;
            git(dir, home, &["commit", "-qam", "div on the remote"])?;
            git(dir, home, &["push", "-q", "origin", "div"])?;
            git(dir, home, &["reset", "-q", "--hard", "HEAD~1"])?;
            write(dir, "notes-div.txt", "local side of div\n")?;
            git(dir, home, &["add", "notes-div.txt"])?;
            git(dir, home, &["commit", "-q", "-m", "div locally"])?;
            git(dir, home, &["branch", "--set-upstream-to=origin/div", "div"])?;

            // Back on `main`, three commits behind, with the remote's own refs
            // fetched so `pull` has a tracking ref to compare against.
            git(dir, home, &["checkout", "-q", "main"])?;
            git(dir, home, &["branch", "-D", "upstream-work"])?;
            git(dir, home, &["fetch", "-q", "origin"])?;

            // The bare remote is not part of the tree under test.
            write(dir, ".git/info/exclude", ".remote.git/\n")?;

            // Dirty in two ways that matter: a file the remote never touches, and
            // one that `div` rewrites.
            write(dir, "mine.txt", "mine, the remote never touches this\nlocal edit\n")?;
            write(dir, "clash.txt", "clash, div rewrites this\nlocal edit\n")?;
        }

        Shape::MergeableDirty => {
            mergeable_history(dir, home)?;
            // `hot.txt` is rewritten by `ff-hot`/`div-hot`, so a merge of those
            // has to refuse; `keep.txt` is rewritten by nothing, so a merge of
            // anything has to carry it through; `squat.txt` sits untracked
            // exactly where `ff-squat`/`div-squat` want to write.
            write(dir, "hot.txt", "hot, edited in the worktree\n")?;
            write(dir, "keep.txt", "keep, edited in the worktree\n")?;
            write(dir, "squat.txt", "untracked squatter\n")?;
        }

        Shape::MergeableStaged => {
            mergeable_history(dir, home)?;
            // Staged, and on a path no branch below touches — so no checkout
            // could overwrite it. A fast-forward carries it through; a strategy
            // refuses the whole merge over it anyway.
            write(dir, "keep.txt", "keep, staged\n")?;
            git(dir, home, &["add", "keep.txt"])?;
        }

        Shape::Worktree => {
            // The linked worktree lives inside the fixture root so the per-case
            // copy carries it, and is hidden from the main worktree's status the
            // same way `BehindRemote` hides its bare remote.
            write(dir, ".git/info/exclude", "wt/\n")?;

            // `--relative-paths` is what makes the shape copyable at all. By
            // default `worktree add` records the *absolute* path of the worktree
            // in `.git/worktrees/wt/gitdir` and of the git dir in `wt/.git`, so
            // both copies of the fixture would point back at the template: the
            // two sides would share one repository and the comparison would
            // measure nothing. With relative paths each copy points at itself.
            git(dir, home, &["worktree", "add", "--relative-paths", "-q", "-b", "linked", "wt"])?;

            // `worktree add` checks the files out and records their `stat` data
            // in `.git/worktrees/wt/index`. That data is inode and mtime, so two
            // builds of the shape would differ there and the shape would stop
            // being reproducible — `shapes_build_reproducibly` exempts the main
            // `.git/index` for exactly this reason, and a second index is not
            // covered by that exemption. `read-tree` rewrites the same entries
            // from the tree with the stat fields zeroed, which is a state git
            // writes itself (any `read-tree` without `-u` does) and refreshes on
            // first use, so the worktree stays a normal checked-out one.
            git(&dir.join("wt"), home, &["read-tree", "HEAD"])?;
        }

        Shape::Octopus => {
            // Three branches off the base, each touching one path of its own so
            // the octopus merges cleanly and needs no strategy of substance.
            for branch in ["oct-a", "oct-b", "oct-c"] {
                git(dir, home, &["checkout", "-q", "-b", branch, "main"])?;
                write(dir, &format!("{branch}.txt"), &format!("{branch}\n"))?;
                git(dir, home, &["add", &format!("{branch}.txt")])?;
                git(dir, home, &["commit", "-qm", &format!("{branch} commit")])?;
            }

            // Forked before the merge and never merged, so it survives as a lane
            // beside the octopus under `--all`.
            git(dir, home, &["checkout", "-q", "-b", "oct-side", "main"])?;
            write(dir, "oct-side.txt", "oct-side\n")?;
            git(dir, home, &["add", "oct-side.txt"])?;
            git(dir, home, &["commit", "-qm", "oct-side commit"])?;

            // `main` moves on first, so the merge's first parent is not the base
            // and the graph has a lane to collapse under the octopus as well.
            git(dir, home, &["checkout", "-q", "main"])?;
            write(dir, "trunk.txt", "trunk\n")?;
            git(dir, home, &["add", "trunk.txt"])?;
            git(dir, home, &["commit", "-qm", "main moves on"])?;
            git(
                dir,
                home,
                &["merge", "-q", "--no-ff", "-m", "octopus merge", "oct-a", "oct-b", "oct-c"],
            )?;
        }

        Shape::NoIndexTrees => {
            // The queue with both a source and a destination: one path changed
            // on both sides, one only on the left, one only on the right.
            write(dir, "ni/da/common.txt", "common one\ncommon two\ncommon three\n")?;
            write(dir, "ni/db/common.txt", "common one\ncommon two changed\ncommon three\n")?;
            write(dir, "ni/da/left.txt", "only on the left\n")?;
            write(dir, "ni/db/right.txt", "only on the right\n")?;
            // The two degenerate queues. Their other half is an empty directory,
            // which is the only way to make a comparison that is purely an add
            // or purely a delete.
            write(dir, "ni/addonly_b/added.txt", "added on the right\n")?;
            write(dir, "ni/delonly_a/gone.txt", "gone from the right\n")?;
            std::fs::create_dir_all(dir.join("ni").join("addonly_a"))?;
            std::fs::create_dir_all(dir.join("ni").join("delonly_b"))?;
            // Two plain files, for the cases that want a single modified pair
            // and nothing else in the queue.
            write(dir, "ni/a.txt", "alpha\nbeta\ngamma\n")?;
            write(dir, "ni/b.txt", "alpha\nBETA\ngamma\n")?;

            // Tracked, so the state probe's `status` stays quiet and the shape
            // reports only what a case did. The empty directories cannot be
            // tracked and are invisible to `status` for the same reason.
            git(dir, home, &["add", "ni"])?;
            git(dir, home, &["commit", "-qm", "no-index: subject trees"])?;

            // Read only by the cases that run *inside* this repository; the ones
            // that run outside it never see this file. Ten is deliberately not
            // 7 and not what `auto` would pick for a repository this small.
            git(dir, home, &["config", "core.abbrev", "10"])?;
        }

        Shape::DecomposedPaths => {
            write(dir, NFD_TRACKED, "decomposed\n")?;
            git(dir, home, &["add", "--", NFD_TRACKED])?;
            git(dir, home, &["commit", "-qm", "nfd: a decomposed path"])?;
            // Dirty through the index, and dirty through the directory walk:
            // `status` has to name the same decomposed path from both.
            write(dir, NFD_TRACKED, "decomposed\nworktree edit\n")?;
            write(dir, NFD_UNTRACKED, "untracked, decomposed\n")?;
        }

        Shape::Hooked => {
            // A subdirectory with tracked content, so a case carrying `cwd` runs
            // from inside the repository rather than at its root. That
            // combination — a hook present, and a working directory below the
            // top level — is the one that failed outright.
            write(dir, "sub/nested.txt", "nested\n")?;
            write(dir, "top.txt", "top\n")?;
            git(dir, home, &["add", "sub/nested.txt", "top.txt"])?;
            git(dir, home, &["commit", "-qm", "hooked: a subdirectory to run from"])?;

            // Deliberately hooks that do NOT invoke git. Each side of a case runs
            // its own binary, and a hook naming one by path would make the other
            // side execute it too — the fixture would then measure one binary
            // through the other and call the agreement parity. `exit 0` and a
            // plain write need no binary at all, and the defect this shape exists
            // for reproduces with exactly that.
            //
            // `$GIT_INDEX_FILE` is echoed rather than used: git points a
            // `pre-commit` hook at the real index, at `index.lock`, or at
            // `next-index-<pid>.lock` depending on the commit mode
            // (builtin/commit.c:468, :493, :554), and a hook that records which
            // one it was given turns that into something a case can compare.
            let hooks = dir.join(".git/hooks");
            std::fs::create_dir_all(&hooks)?;
            for (name, body) in [
                (
                    "pre-commit",
                    "#!/bin/sh\nprintf 'pre-commit %s\\n' \"${GIT_INDEX_FILE##*/}\" > hook-ran.txt\nexit 0\n",
                ),
                // Appends a trailer to every commit message, so a case can see
                // whether the hook's edit survived — and, for the verbs that pass
                // `--no-verify`, whether it was correctly skipped.
                (
                    "commit-msg",
                    "#!/bin/sh\nprintf 'hooked-trailer\\n' >> \"$1\"\nexit 0\n",
                ),
            ] {
                let path = hooks.join(name);
                std::fs::write(&path, body)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
                }
            }
        }

        Shape::Unrelated => {
            write(dir, "src/lib.rs", "pub fn one() -> u32 { 1 }\npub fn two() -> u32 { 2 }\n")?;
            git(dir, home, &["commit", "-qam", "unrelated: main moves"])?;

            // Shares no path with `main`, so the allowed merge is clean and the
            // resulting tree is the union of two roots.
            orphan(dir, home, "alien", &["README.md", "src/lib.rs"])?;
            write(dir, "alien.txt", "alien root\n")?;
            git(dir, home, &["add", "alien.txt"])?;
            git(dir, home, &["commit", "-qm", "alien root"])?;
            write(dir, "alien.txt", "alien root\nalien second\n")?;
            git(dir, home, &["commit", "-qam", "alien second"])?;
            // A tag on the far root, so `describe`, `format-patch` and the
            // `--contains` queries can name it without an id.
            git(dir, home, &["tag", "alien-tip"])?;

            // Collides with `main` on `README.md`. An add/add conflict between
            // unrelated roots has no common ancestor to diff against, which is a
            // different path through the strategy than the add/add conflicts
            // every other shape can produce.
            orphan(dir, home, "alien-clash", &["alien.txt"])?;
            write(dir, "README.md", "# alien fixture\n\nsame path, no common ancestor\n")?;
            git(dir, home, &["add", "README.md"])?;
            git(dir, home, &["commit", "-qm", "alien clash root"])?;

            git(dir, home, &["checkout", "-q", "main"])?;
        }

        Shape::CrissCross => {
            write(dir, "cc.txt", &numbered("cc", 12))?;
            write(dir, "clash.txt", "base\n")?;
            write(dir, "calm.txt", "calm\n")?;
            git(dir, home, &["add", "."])?;
            git(dir, home, &["commit", "-qm", "criss-cross: base"])?;

            // The two future merge bases. They disagree on `clash.txt`, which is
            // what makes the virtual base a conflicted merge rather than a
            // second copy of `base`.
            git(dir, home, &["checkout", "-q", "-b", "cc-a", "main"])?;
            write(dir, "cc.txt", &numbered_with_edits("cc", 12, &[2]))?;
            write(dir, "clash.txt", "a\n")?;
            git(dir, home, &["commit", "-qam", "criss-cross: a"])?;

            git(dir, home, &["checkout", "-q", "-b", "cc-b", "main"])?;
            write(dir, "cc.txt", &numbered_with_edits("cc", 12, &[11]))?;
            write(dir, "clash.txt", "b\n")?;
            git(dir, home, &["commit", "-qam", "criss-cross: b"])?;

            // Each side merges the other and resolves `clash.txt` its own way,
            // so the two tips disagree there again and the outer merge has to
            // reach for a base.
            for (tip, from, other, keep, edit) in [
                ("cc-left", "cc-a", "cc-b", "a\n", 5usize),
                ("cc-right", "cc-b", "cc-a", "b\n", 8usize),
            ] {
                git(dir, home, &["checkout", "-q", "-b", tip, from])?;
                git_conflicting(dir, home, &["merge", "--no-commit", other])?;
                write(dir, "clash.txt", keep)?;
                git(dir, home, &["add", "clash.txt"])?;
                let msg = format!("criss-cross: {tip} merge");
                git(dir, home, &["commit", "-qm", &msg])?;
                // One more commit per side on a path that merges cleanly, so the
                // criss-cross merge is not *only* a conflict.
                write(dir, "cc.txt", &numbered_with_edits("cc", 12, &[2, edit, 11]))?;
                let msg = format!("criss-cross: {tip} tip");
                git(dir, home, &["commit", "-qam", &msg])?;
            }

            // HEAD stays on `cc-left`: a case is one argv and cannot check out
            // first, so the branch the criss-cross is merged *from* has to be the
            // one already checked out.
            git(dir, home, &["checkout", "-q", "cc-left"])?;
        }

        Shape::Cherry => {
            write(dir, "app.txt", &numbered("app", 10))?;
            git(dir, home, &["add", "app.txt"])?;
            git(dir, home, &["commit", "-qm", "cherry: seed"])?;
            git(dir, home, &["branch", "topic"])?;

            write(dir, "app.txt", &numbered_with_edits("app", 10, &[3]))?;
            git(dir, home, &["commit", "-qam", "cherry: shared patch"])?;
            write(dir, "app.txt", &numbered_with_edits("app", 10, &[3, 8]))?;
            git(dir, home, &["commit", "-qam", "cherry: upstream only"])?;

            git(dir, home, &["checkout", "-q", "topic"])?;
            // Committed *before* the pick on purpose. Every input to a commit id
            // is pinned here except the parent, so picking onto the fork point
            // would reproduce the original commit byte for byte and the two
            // branches would share it instead of holding two commits with one
            // patch id.
            write(dir, "topic-base.txt", "topic\n")?;
            git(dir, home, &["add", "topic-base.txt"])?;
            git(dir, home, &["commit", "-qm", "cherry: topic base"])?;
            git(dir, home, &["cherry-pick", "main~1"])?;
            write(dir, "app.txt", &numbered_with_edits("app", 10, &[3, 10]))?;
            git(dir, home, &["commit", "-qam", "cherry: topic only"])?;
        }

        Shape::Symlinks => {
            write(dir, "empty.txt", "")?;
            write(dir, "dir/empty-nested.txt", "")?;
            write(dir, "dir/target.txt", "target content\n")?;
            // One per answer `cat-file --follow-symlinks` has: a blob, a blob
            // reached through a symlinked directory, `dangling`, `symlink` for a
            // target outside the tree, and a link to a link.
            symlink(dir, "link-to-file", "README.md")?;
            symlink(dir, "link-to-dir", "dir")?;
            symlink(dir, "link-broken", "no/such/target")?;
            symlink(dir, "link-escape", "../outside.txt")?;
            symlink(dir, "link-to-link", "link-to-file")?;
            symlink(dir, "dir/link-up", "../dir/target.txt")?;
            // Retargeted in the worktree below, so one symlink is dirty and the
            // rest stay clean.
            symlink(dir, "link-wt", "README.md")?;
            git(dir, home, &["add", "-A"])?;
            git(dir, home, &["commit", "-qm", "symlinks: seed"])?;

            // The patch's subject changes live on a side branch so `main`'s tree
            // stays the pre-image the patch applies to, as in [`Shape::Patches`].
            git(dir, home, &["checkout", "-q", "-b", "sym-pending"])?;
            symlink(dir, "later-link", "empty.txt")?;
            write(dir, "later-empty.txt", "")?;
            std::fs::remove_file(dir.join("dir/target.txt"))?;
            symlink(dir, "dir/target.txt", "../empty.txt")?;
            git(dir, home, &["add", "-A"])?;
            git(dir, home, &["commit", "-qm", "symlinks: pending changes"])?;
            git(dir, home, &["checkout", "-q", "main"])?;

            let patch = git(dir, home, &["diff", "main", "sym-pending"])?;
            write(dir, "patches/symlink.patch", &patch)?;
            git(dir, home, &["add", "patches"])?;
            git(dir, home, &["commit", "-qm", "symlinks: a patch that adds one"])?;

            // Dirty through the index and dirty through the directory walk: a
            // retargeted symlink and an untracked one, plus an untracked empty
            // file for the `add`/`status`/`clean` side of the question.
            std::fs::remove_file(dir.join("link-wt"))?;
            symlink(dir, "link-wt", "src/lib.rs")?;
            symlink(dir, "stray-link", "README.md")?;
            write(dir, "stray-empty.txt", "")?;
        }

        Shape::CommitGraph => {
            for n in 1..=5usize {
                write(dir, "cg.txt", &numbered("cg", n))?;
                if n == 1 {
                    git(dir, home, &["add", "cg.txt"])?;
                }
                let msg = format!("commit-graph: {n}");
                git(dir, home, &["commit", "-qam", &msg])?;
            }

            // A fork that is never merged and a branch that is, so the graph
            // holds both a merge commit and a tip off the main chain.
            git(dir, home, &["checkout", "-q", "-b", "cg-loose", "main~3"])?;
            write(dir, "cg-loose.txt", "loose\n")?;
            git(dir, home, &["add", "cg-loose.txt"])?;
            git(dir, home, &["commit", "-qm", "commit-graph: loose fork"])?;

            git(dir, home, &["checkout", "-q", "-b", "cg-side", "main"])?;
            write(dir, "cg-side.txt", "side\n")?;
            git(dir, home, &["add", "cg-side.txt"])?;
            git(dir, home, &["commit", "-qm", "commit-graph: side"])?;
            git(dir, home, &["checkout", "-q", "main"])?;
            git(dir, home, &["merge", "-q", "--no-ff", "-m", "commit-graph: merge side", "cg-side"])?;

            // `--changed-paths` writes the Bloom filters `log -- <path>` reads;
            // without them the flag is a write option with no reader.
            git(dir, home, &["commit-graph", "write", "--reachable", "--changed-paths"])?;

            // After the write, so the graph is valid and incomplete.
            write(dir, "cg-late.txt", "committed after the graph was written\n")?;
            git(dir, home, &["add", "cg-late.txt"])?;
            git(dir, home, &["commit", "-qm", "commit-graph: after the write"])?;
        }

        Shape::Damaged => {
            write(dir, "second.txt", "second\n")?;
            git(dir, home, &["add", "second.txt"])?;
            git(dir, home, &["commit", "-qm", "damaged: a second commit"])?;

            // Written as files rather than through `update-ref`, which refuses
            // both: git will not create a ref to an object it cannot find, and
            // will not point a symref at a branch that does not exist. The
            // damage this shape exists for is precisely the state git's own
            // plumbing declines to produce.
            write(dir, ".git/refs/heads/dangling", &format!("{MISSING_OBJECT}\n"))?;
            write(dir, ".git/refs/heads/broken-symref", "ref: refs/heads/does-not-exist\n")?;

            // A loose object whose name is a valid id and whose contents are not
            // a zlib stream. `cat-file --batch-all-objects` still lists it — as
            // `missing` — so the object *set* stays enumerable and the damage is
            // in the read.
            write(
                dir,
                &format!(".git/objects/{}/{}", &CORRUPT_OBJECT[..2], &CORRUPT_OBJECT[2..]),
                "this is not a zlib stream\n",
            )?;

            // An empty entry. See the shape's doc comment for why it is empty
            // rather than a path that does not exist.
            write(dir, ".git/objects/info/alternates", "\n")?;
        }
    }
    Ok(())
}

/// The id `refs/heads/dangling` points at in [`Shape::Damaged`]: well-formed,
/// and belonging to no object. A literal rather than a hash of anything, so it
/// cannot accidentally become an object the fixture also stores.
pub const MISSING_OBJECT: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
/// The id of [`Shape::Damaged`]'s corrupt loose object, chosen the same way and
/// for the same reason.
pub const CORRUPT_OBJECT: &str = "ab1234567890123456789012345678901234abcd";

/// Start an unborn branch and clear the previous history out of the index and
/// the worktree, so the next commit is a *root* rather than a child.
///
/// `git checkout --orphan` moves HEAD alone: the index and worktree are left
/// holding the branch that was checked out, and committing them would make a
/// root whose tree is the old one. `carried` names the files that survive the
/// index reset and have to be removed by hand.
fn orphan(dir: &Path, home: &Path, name: &str, carried: &[&str]) -> Result<()> {
    git(dir, home, &["checkout", "-q", "--orphan", name])?;
    git(dir, home, &["rm", "-r", "-q", "--cached", "."])?;
    for path in carried {
        std::fs::remove_file(dir.join(path))?;
    }
    Ok(())
}

/// Run stock git for a step whose *failure* is the state being built, and fail
/// loudly if it succeeds.
///
/// A conflicted merge exits non-zero, so [`git`] cannot run one — and a step
/// that silently stopped conflicting would leave a shape that still builds and
/// no longer carries what its cases need.
fn git_conflicting(dir: &Path, home: &Path, args: &[&str]) -> Result<()> {
    let mut cmd = crate::stock::command()?;
    env::harden(&mut cmd, home);
    cmd.current_dir(dir).args(args);
    let out = cmd.output().with_context(|| format!("spawn stock git {args:?}"))?;
    if out.status.success() {
        bail!("fixture: stock git {args:?} in {} was expected to conflict", dir.display());
    }
    Ok(())
}

/// Create a symlink at `rel` pointing at `target`.
///
/// Targets are always relative: an absolute one would put the build directory
/// into a tracked blob, and the shape would hash differently at a second build
/// location — which is exactly what `shapes_build_reproducibly` fails on.
#[cfg(unix)]
fn symlink(dir: &Path, rel: &str, target: &str) -> Result<()> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(target, &path)
        .with_context(|| format!("symlink {} -> {target}", path.display()))
}

/// Symlink fixtures need a filesystem that has symlinks. Reported rather than
/// silently degraded: a shape built without its symlinks would still build, and
/// every case using it would then measure nothing.
#[cfg(not(unix))]
fn symlink(_dir: &Path, rel: &str, target: &str) -> Result<()> {
    bail!("fixture: symlink {rel} -> {target} needs a unix filesystem")
}

/// `é.txt`, decomposed: `e` + U+0301, the form macOS stores and hands back.
///
/// Written as an escape rather than as a literal so the bytes are unambiguous
/// in the source — an editor that normalises on save would otherwise turn this
/// shape into a duplicate of [`Shape::AwkwardPaths`] without anything noticing.
pub const NFD_TRACKED: &str = "e\u{301}.txt";
/// The same combining mark on an untracked path.
pub const NFD_UNTRACKED: &str = "e\u{301}-new.txt";

/// The history the two mergeable shapes share.
///
/// A branch cannot be both fast-forwardable from `main` and diverged from it,
/// and the two take different paths through git's gates — `unpack_trees()`
/// alone versus the strategy's index check first — so both kinds are built and
/// each case picks by branch name:
///
/// * `div-*` fork before `main`'s last commit, so merging one is a three-way.
/// * `ff-*` fork after it, so merging one is a fast-forward.
///
/// Each branch touches exactly one path, named for what the shape's dirt does
/// to it: `cold` is left alone by the worktree, `hot` is edited there, `squat`
/// is added by the branch and squatted on by an untracked file, `other` exists
/// so two heads can be merged at once as an octopus.
fn mergeable_history(dir: &Path, home: &Path) -> Result<()> {
    for (path, body) in [
        ("keep.txt", "keep\n"),
        ("hot.txt", "hot\n"),
        ("cold.txt", "cold\n"),
        ("trunk.txt", "trunk\n"),
    ] {
        write(dir, path, body)?;
    }
    git(dir, home, &["add", "."])?;
    git(dir, home, &["commit", "-qm", "mergeable base"])?;

    for (branch, path, body) in [
        ("div-cold", "cold.txt", "cold, from div\n"),
        ("div-hot", "hot.txt", "hot, from div\n"),
        ("div-squat", "squat.txt", "squat, from div\n"),
        ("div-other", "other.txt", "other, from div\n"),
    ] {
        git(dir, home, &["checkout", "-q", "-b", branch, "main"])?;
        write(dir, path, body)?;
        git(dir, home, &["add", path])?;
        git(dir, home, &["commit", "-qm", branch])?;
    }

    // `main` moves last among the shared commits, which is what leaves it
    // diverged from every `div-*` above and an ancestor of every `ff-*` below.
    git(dir, home, &["checkout", "-q", "main"])?;
    write(dir, "trunk.txt", "trunk, moved\n")?;
    git(dir, home, &["commit", "-qam", "main moves"])?;

    for (branch, path, body) in [
        ("ff-cold", "cold.txt", "cold, from ff\n"),
        ("ff-hot", "hot.txt", "hot, from ff\n"),
        ("ff-squat", "squat.txt", "squat, from ff\n"),
    ] {
        git(dir, home, &["checkout", "-q", "-b", branch, "main"])?;
        write(dir, path, body)?;
        git(dir, home, &["add", path])?;
        git(dir, home, &["commit", "-qm", branch])?;
    }
    git(dir, home, &["checkout", "-q", "main"])?;
    Ok(())
}

/// A deterministic non-text blob. `variant` changes every byte, so two revisions
/// of the same file are unmistakably binary *and* unmistakably different.
fn binary_blob(variant: u8) -> Vec<u8> {
    (0..1024u32)
        .map(|i| ((i * 7 + variant as u32 * 13) % 251) as u8)
        .collect()
}

/// Tab-indented seed for [`Shape::Whitespace`].
const WS_TABS: &str = "int main(void)\n{\n\tint total = 0;\n\tfor (int i = 0; i < 10; i++) {\n\t\ttotal += i;\n\t}\n\treturn total;\n}\n";
/// The same lines, tabs expanded to four spaces. Nothing else changes.
const WS_SPACES: &str = "int main(void)\n{\n    int total = 0;\n    for (int i = 0; i < 10; i++) {\n        total += i;\n    }\n    return total;\n}\n";
/// The space-indented form with trailing blanks appended to three lines.
const WS_TRAILING: &str = "int main(void)\n{\n    int total = 0;   \n    for (int i = 0; i < 10; i++) {\t\n        total += i;\n    }\n    return total; \n}\n";
/// One real edit (`+= i` becomes `+= i * 2`) with whitespace churn on every
/// other line. `-w` must report exactly the one edit.
const WS_MIXED: &str = "int main(void)\n{\n  int total = 0;\n  for (int i = 0; i < 10; i++) {\n      total += i * 2;\n  }\n  return total;\n}\n";
/// The unstaged, whitespace-only worktree edit: same tokens, eight-space indent.
const WS_REINDENTED: &str = "int main(void)\n{\n        int total = 0;\n        for (int i = 0; i < 10; i++) {\n                total += i * 2;\n        }\n        return total;\n}\n";

/// Pre-image every patch in [`Shape::Patches`] applies to.
const MAIN_C_BASE: &str = "static const int VERSION = 1;\n\nint add(int a, int b)\n{\n\treturn a + b;\n}\n\nint main(void)\n{\n\treturn add(1, 2);\n}\n";
/// First patch's post-image: a function added.
const MAIN_C_ONE: &str = "static const int VERSION = 1;\n\nint add(int a, int b)\n{\n\treturn a + b;\n}\n\nint subtract(int a, int b)\n{\n\treturn a - b;\n}\n\nint main(void)\n{\n\treturn add(1, 2) + subtract(4, 3);\n}\n";
/// Second patch's post-image: a constant bumped.
const MAIN_C_TWO: &str = "static const int VERSION = 2;\n\nint add(int a, int b)\n{\n\treturn a + b;\n}\n\nint subtract(int a, int b)\n{\n\treturn a - b;\n}\n\nint main(void)\n{\n\treturn add(1, 2) + subtract(4, 3);\n}\n";

/// Patch-shaped but corrupt: the hunk header promises seven pre-image lines and
/// supplies one. `apply --check`, with or without `--cached`, has to reject it —
/// accepting it at exit 0 is a regression that shipped once and could not be
/// pinned because no fixture carried a patch.
const CORRUPT_PATCH: &str = "diff --git a/app/main.c b/app/main.c\nindex 1111111..2222222 100644\n--- a/app/main.c\n+++ b/app/main.c\n@@ -1,7 +1,9 @@\n static const int VERSION = 1;\n+int corrupt(void);\n";

/// A hunk of pure context, changing nothing. Stock rejects it as corrupt
/// (`error: corrupt patch at ...:8`, exit 128); the case pins that agreement,
/// which a parser that silently accepts a no-op hunk would break.
const CONTEXT_ONLY_PATCH: &str = "diff --git a/app/main.c b/app/main.c\n--- a/app/main.c\n+++ b/app/main.c\n@@ -1,3 +1,3 @@\n static const int VERSION = 1;\n \n int add(int a, int b)\n";

/// The same change `valid.patch` carries, with a hunk header that points three
/// lines too early, so applying it takes the offset search rather than a
/// literal line-number match (stock: `Hunk #1 succeeded at 5 (offset 3 lines)`).
///
/// The header says line 2, not line 1, deliberately: `git apply` treats a hunk
/// starting at line 1 as anchored to the start of the file and refuses to
/// search at all, so a `-1,7` header would test rejection instead of search.
const OFFSET_PATCH: &str = "diff --git a/app/main.c b/app/main.c\n--- a/app/main.c\n+++ b/app/main.c\n@@ -2,7 +2,12 @@\n \treturn a + b;\n }\n \n+int subtract(int a, int b)\n+{\n+\treturn a - b;\n+}\n+\n int main(void)\n {\n-\treturn add(1, 2);\n+\treturn add(1, 2) + subtract(4, 3);\n }\n";

/// Adds a line with trailing blanks and one indented with a space before a tab,
/// so `--whitespace=warn|error|fix` each have something to act on.
const WHITESPACE_PATCH: &str = "diff --git a/app/main.c b/app/main.c\n--- a/app/main.c\n+++ b/app/main.c\n@@ -1,4 +1,6 @@\n static const int VERSION = 1;\n+int trailing(void);  \n+ \tint indented(void);\n \n int add(int a, int b)\n {\n";

/// Recursive copy used to clone a prebuilt template per case. Copying beats
/// rebuilding: fixture construction is the slowest part of a run, and every
/// case needs a pristine repo.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&from)?;
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else if meta.is_symlink() {
            let target = std::fs::read_link(&from)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &to)?;
            #[cfg(not(unix))]
            std::fs::copy(&from, &to)?;
        } else {
            copy_file(&from, &to, &meta)?;
        }
    }
    Ok(())
}

/// Copy one file, carrying its modification time across.
///
/// The index stores each entry's `stat` data and git compares it before
/// trusting the entry. Inode and creation time cannot survive a copy, which is
/// why the fixtures set `core.checkStat=minimal`; mtime and size can survive,
/// and this is what makes them.
fn copy_file(from: &Path, to: &Path, meta: &std::fs::Metadata) -> Result<()> {
    std::fs::copy(from, to).with_context(|| format!("copy {}", from.display()))?;
    let Ok(mtime) = meta.modified() else { return Ok(()) };

    // Loose objects and packs are copied read-only, and a read-only handle
    // cannot carry a timestamp update; widen the mode for the call, then put it
    // back so the copy is a faithful one.
    let perms = meta.permissions();
    if perms.readonly() {
        let mut writable = perms.clone();
        writable.set_readonly(false);
        std::fs::set_permissions(to, writable)?;
    }
    if let Ok(f) = std::fs::File::options().write(true).open(to) {
        let _ = f.set_modified(mtime);
    }
    if perms.readonly() {
        std::fs::set_permissions(to, perms)?;
    }
    Ok(())
}

/// Prebuilt template directories, one per shape.
pub struct Templates {
    root: PathBuf,
    pub home: PathBuf,
}

impl Templates {
    /// Build every shape once under `root`.
    pub fn build_all(root: &Path) -> Result<Self> {
        let home = root.join("home");
        std::fs::create_dir_all(&home)?;
        let templates = root.join("templates");
        std::fs::create_dir_all(&templates)?;
        for &shape in Shape::ALL {
            let dir = templates.join(shape.name());
            if dir.exists() {
                continue;
            }
            build(shape, &dir, &home)
                .with_context(|| format!("building fixture shape {}", shape.name()))?;
        }
        Ok(Self { root: templates, home })
    }

    /// Materialize a pristine copy of `shape` at `dst`.
    pub fn instantiate(&self, shape: Shape, dst: &Path) -> Result<()> {
        copy_tree(&self.root.join(shape.name()), dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a over file bytes. The hash only has to distinguish, not resist
    /// attack, and this keeps the test free of a dependency.
    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    /// Hash every file in the shape, path-relative and sorted.
    ///
    /// `.git/index` is the one exclusion: it stores `stat` data (inode, device,
    /// mtime) for each entry, which is filesystem state rather than repository
    /// state and cannot match between two directories. The index's *logical*
    /// contents are covered by the `ls-files -v` probe in [`digest`] instead.
    ///
    /// Nothing is normalized. A shape that records its own absolute path
    /// anywhere will hash differently at the two build locations, and failing
    /// on that is the point.
    fn hash_tree(root: &Path, rel: &Path, out: &mut Vec<String>) -> Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(root.join(rel))?
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        entries.sort();
        for name in entries {
            let child = rel.join(&name);
            if child == Path::new(".git/index") {
                continue;
            }
            let abs = root.join(&child);
            let meta = std::fs::symlink_metadata(&abs)?;
            if meta.is_dir() {
                out.push(format!("d {}", child.display()));
                hash_tree(root, &child, out)?;
            } else if meta.is_symlink() {
                out.push(format!("l {} -> {}", child.display(), std::fs::read_link(&abs)?.display()));
            } else {
                let bytes = std::fs::read(&abs)?;
                out.push(format!("f {} {:016x} {}", child.display(), fnv1a(&bytes), bytes.len()));
            }
        }
        Ok(())
    }

    /// One probe's answer: its stdout, and the exit code it left.
    ///
    /// The code is part of the answer because a probe is allowed to *fail* here.
    /// [`Shape::Damaged`] carries a ref pointing at a missing object, and
    /// `for-each-ref` exits 128 rather than printing it, so a probe runner that
    /// insisted on success could not ask any question at all about a repository
    /// with something wrong in it. Recording the code keeps the check strict in
    /// the direction that matters: a probe that succeeds in one build and fails
    /// in the other is still a difference, and the failing probe's stdout is
    /// still compared. stderr is deliberately left out — it names paths, and the
    /// two builds are at two different ones.
    fn probe(dir: &Path, home: &Path, args: &[&str]) -> Result<String> {
        let mut cmd = crate::stock::command()?;
        env::harden(&mut cmd, home);
        cmd.current_dir(dir).args(args);
        let out = cmd.output().with_context(|| format!("spawn stock git {args:?}"))?;
        Ok(format!(
            "exit {}\n{}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout)
        ))
    }

    /// Repository state as stock git reports it, plus the on-disk bytes.
    fn digest(dir: &Path, home: &Path) -> Result<String> {
        let mut s = String::new();
        for args in [
            &["for-each-ref", "--format=%(refname) %(objecttype) %(objectname)"][..],
            &["ls-files", "-v", "--full-name"][..],
            &["cat-file", "--batch-check", "--batch-all-objects"][..],
            &["status", "--porcelain=v1", "--untracked-files=all"][..],
        ] {
            let mut lines: Vec<String> =
                probe(dir, home, args)?.lines().map(str::to_string).collect();
            // `--batch-all-objects` walks packs and loose storage in an order the
            // filesystem influences; the object *set* is what must be stable.
            lines.sort();
            s.push_str(&format!("# {}\n{}\n", args.join(" "), lines.join("\n")));
        }
        let mut files = Vec::new();
        hash_tree(dir, Path::new(""), &mut files)?;
        s.push_str("# tree\n");
        s.push_str(&files.join("\n"));
        Ok(s)
    }

    /// Every shape must build to the same bytes twice.
    ///
    /// A fixture that varies between two builds makes every case that uses it
    /// unmeasurable, and the harness would report that as an implementation
    /// difference rather than as its own defect.
    ///
    /// `Submodule` is excluded by construction, not by convenience: it records
    /// the absolute path of its upstream in `.gitmodules` and `.git/config`, so
    /// two copies at different paths are *supposed* to differ.
    ///
    /// The builder needs a stock git to run at all, and [`crate::stock::git`]
    /// refuses one older than the version the port targets — a machine without a
    /// current git cannot answer this question, so the test says so rather than
    /// reporting a determinism failure it did not measure. Every path that
    /// *reports a number* still refuses outright.
    #[test]
    fn shapes_build_reproducibly() {
        if let Err(why) = crate::stock::git() {
            eprintln!("skipping: no git to build fixtures with — {why}");
            return;
        }
        let root = std::env::temp_dir().join(format!("zvcs-fixture-determinism-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();

        for &shape in Shape::ALL {
            if shape == Shape::Submodule {
                continue;
            }
            let a = root.join(format!("a/{}", shape.name()));
            let b = root.join(format!("b/{}", shape.name()));
            build(shape, &a, &home).unwrap_or_else(|e| panic!("build {} (a): {e:#}", shape.name()));
            build(shape, &b, &home).unwrap_or_else(|e| panic!("build {} (b): {e:#}", shape.name()));
            let da = digest(&a, &home).unwrap();
            let db = digest(&b, &home).unwrap();
            if da != db {
                let first = da
                    .lines()
                    .zip(db.lines())
                    .find(|(x, y)| x != y)
                    .map(|(x, y)| format!("\n  a: {x}\n  b: {y}"))
                    .unwrap_or_else(|| "\n  (line counts differ)".to_string());
                panic!("shape {} is not reproducible:{first}", shape.name());
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
