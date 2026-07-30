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
use std::process::Command;

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
        }
    }
}

/// Run stock git in `dir`, failing loudly on non-zero exit.
///
/// Fixture construction has no tolerance for partial success: a half-built
/// premise would silently weaken every case that uses it.
fn git(dir: &Path, home: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
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
            let mut cmd = Command::new("git");
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
    }
    Ok(())
}

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

    /// Repository state as stock git reports it, plus the on-disk bytes.
    fn digest(dir: &Path, home: &Path) -> Result<String> {
        let mut s = String::new();
        for probe in [
            &["for-each-ref", "--format=%(refname) %(objecttype) %(objectname)"][..],
            &["ls-files", "-v", "--full-name"][..],
            &["cat-file", "--batch-check", "--batch-all-objects"][..],
            &["status", "--porcelain=v1", "--untracked-files=all"][..],
        ] {
            let mut lines: Vec<String> =
                git(dir, home, probe)?.lines().map(str::to_string).collect();
            // `--batch-all-objects` walks packs and loose storage in an order the
            // filesystem influences; the object *set* is what must be stable.
            lines.sort();
            s.push_str(&format!("# {}\n{}\n", probe.join(" "), lines.join("\n")));
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
    #[test]
    fn shapes_build_reproducibly() {
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
