//! Repository *discovery*: which repository git decides it is in, before it
//! does anything else.
//!
//! Every other case in the corpus runs at the fixture's worktree root under one
//! fixed environment, so `setup.c:setup_git_directory_gently_1` — the function
//! that answers "which git dir, which worktree, which prefix" — was only ever
//! asked its easiest question. The answer is a function of the *working
//! directory* (inside `.git`, inside a linked worktree, inside a bare
//! repository, inside a submodule) and of three environment variables
//! (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_CEILING_DIRECTORIES`), and neither was
//! reachable from a case. That blind spot shipped three bugs — commands failed
//! outright from inside `.git`, every command in a bare repository's
//! *subdirectory* aborted the process, and `GIT_DIR` was ignored by nearly every
//! call site — and a regression in any of them would be silent again.
//!
//! Each block below is one *situation*: a directory, sometimes with an
//! environment, in which git reaches a different conclusion. The commands run in
//! it are chosen per situation rather than crossed with it, because most of the
//! grid is meaningless: `--show-toplevel` in a bare repository is one error, not
//! fifteen, and a directory whose every answer is identical to another
//! directory's is a duplicate rather than a case. Two rules were applied:
//!
//!  * **A column earns its place by the answer it gives here differing from the
//!    answer it gives in the situations already covered.** The full set of
//!    twelve is spelled out only where every one of them is distinctive —
//!    inside `.git`, in a linked worktree, in a bare repository, in a submodule
//!    checkout — and trimmed to the differing answers elsewhere.
//!  * **A situation whose whole column set duplicates another's is dropped.**
//!    `.git/objects` answers identically to `.git/refs/heads` in all twelve
//!    (both are "some directory below the git dir"), and the superproject's own
//!    `.git` answers identically to a plain repository's, so neither is here.
//!    They were measured against stock 2.55.0 before being dropped, not assumed
//!    to be duplicates from reading the C.
//!
//! Two situations are deliberately **not** curated, because they differ for
//! reasons that are not discovery:
//!
//!  * `GIT_DIR=.` inside `.git` with `status`. The worktree is then `.git`
//!    itself, so stock lists its own transient `index.lock` as an untracked
//!    file — it is observing an artifact of its own run, and no implementation
//!    can agree with it except by accident of timing. Every other command in
//!    that situation is curated below.
//!  * `status` over a directory it cannot read. That difference reproduces at a
//!    plain worktree root and has nothing to do with which repository was
//!    found.
//!
//! Two combinations were once missing for a third reason — they were live
//! divergences, and a case that fails is not a pin — and both are now here:
//!
//!  * `GIT_CEILING_DIRECTORIES` naming a **proper ancestor** of the starting
//!    directory that is also the repository's own root. Stock stopped the upward
//!    search before it examined that directory and died `not a git repository`
//!    while the port examined it and found the repository; the port now stops
//!    there too, and [`ceilings`] pins all three outcomes together.
//!  * `rev-parse --show-cdup` where there is no worktree. Stock prints zero
//!    bytes and exits 0, the port printed an empty line; it now prints nothing
//!    either, so [`CDUP`] is part of the full column set everywhere.

use crate::fixture::Shape;
use crate::runner::Case;

// ---------------------------------------------------------------------------
// The columns: one argv each, named by the discovery fact it reports.
// ---------------------------------------------------------------------------

/// The git dir, as git chooses to spell it — relative when it can be, absolute
/// otherwise, and that choice is itself part of the answer.
const GIT_DIR: &[&str] = &["rev-parse", "--git-dir"];
/// The *common* dir, which only differs from the git dir in a linked worktree.
const COMMON_DIR: &[&str] = &["rev-parse", "--git-common-dir"];
const IS_BARE: &[&str] = &["rev-parse", "--is-bare-repository"];
const INSIDE_GIT_DIR: &[&str] = &["rev-parse", "--is-inside-git-dir"];
const INSIDE_WORK_TREE: &[&str] = &["rev-parse", "--is-inside-work-tree"];
const TOPLEVEL: &[&str] = &["rev-parse", "--show-toplevel"];
const PREFIX: &[&str] = &["rev-parse", "--show-prefix"];
/// The `../` climb back to the top of the worktree — and where there is no
/// worktree, the one query that prints *nothing at all*, not even the newline
/// every other query ends with. Measured byte for byte with `od -c` in each
/// situation that has no worktree rather than by eye, since an empty line and no
/// line look alike in a terminal.
const CDUP: &[&str] = &["rev-parse", "--show-cdup"];
const ABS_GIT_DIR: &[&str] = &["rev-parse", "--absolute-git-dir"];
/// Reading history at all, from wherever git decided it is.
const LOG: &[&str] = &["log", "--oneline"];
/// The same, naming a branch: the bare fixture's `HEAD` is unborn, so a bare
/// `log` there measures the unborn-branch error rather than object reading.
const LOG_MAIN: &[&str] = &["log", "--oneline", "main"];
/// The porcelain most sensitive to the worktree half of the answer.
const STATUS: &[&str] = &["status", "--porcelain"];
/// `HEAD` itself, which a linked worktree keeps in its own admin directory.
const SYMREF: &[&str] = &["symbolic-ref", "HEAD"];

/// Every column, for the situations where each one is distinctive.
const FULL: &[&[&str]] = &[
    GIT_DIR,
    COMMON_DIR,
    IS_BARE,
    INSIDE_GIT_DIR,
    INSIDE_WORK_TREE,
    TOPLEVEL,
    PREFIX,
    CDUP,
    ABS_GIT_DIR,
    LOG,
    STATUS,
    SYMREF,
];

// ---------------------------------------------------------------------------
// The environments. `{repo}` is `runner::REPO_PLACEHOLDER`, replaced with the
// running side's own fixture root — the two copies live at different paths, so
// a literal one would name the other side's repository.
// ---------------------------------------------------------------------------

/// `GIT_DIR=.` evaluated from inside `.git`: the git dir *and* the worktree.
const GIT_DIR_DOT: &[(&str, &str)] = &[("GIT_DIR", ".")];
/// An absolute `GIT_DIR`, so discovery is skipped and the worktree becomes
/// whatever directory the command happens to run in.
const GIT_DIR_ABS: &[(&str, &str)] = &[("GIT_DIR", "{repo}/.git")];
/// A relative `GIT_DIR` naming the repository the search would have found
/// anyway — the same repository by a different route.
const GIT_DIR_REL: &[(&str, &str)] = &[("GIT_DIR", ".git")];
/// A ceiling naming the repository root, which is the directory the search
/// starts in for one of the rows that uses it and a proper ancestor of it for
/// the other — the distinction that decides whether the ceiling applies.
const CEILING_START: &[(&str, &str)] = &[("GIT_CEILING_DIRECTORIES", "{repo}")];
/// A ceiling that matches nothing on the way up.
const CEILING_MISS: &[(&str, &str)] = &[("GIT_CEILING_DIRECTORIES", "{repo}/no-such-dir")];

/// One situation: where the commands run, and under what environment.
struct Situation {
    shape: Shape,
    /// Relative to the fixture root; `None` is the root itself.
    cwd: Option<&'static str>,
    env: &'static [(&'static str, &'static str)],
}

impl Situation {
    fn at(shape: Shape, cwd: &'static str) -> Self {
        Self { shape, cwd: Some(cwd), env: &[] }
    }

    fn root(shape: Shape) -> Self {
        Self { shape, cwd: None, env: &[] }
    }

    fn with(mut self, env: &'static [(&'static str, &'static str)]) -> Self {
        self.env = env;
        self
    }

    /// Push one case per argv, all in this situation.
    fn run(&self, argvs: &[&'static [&'static str]], out: &mut Vec<Case>) {
        for args in argvs {
            let mut case = Case::new(args[0], args, self.shape);
            if let Some(cwd) = self.cwd {
                case = case.in_dir(cwd);
            }
            if !self.env.is_empty() {
                case = case.with_env(self.env);
            }
            out.push(case);
        }
    }
}

/// Append this module's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    worktree_root(out);
    inside_git_dir(out);
    linked_worktree(out);
    bare(out);
    explicit_git_dir(out);
    submodule(out);
    ceilings(out);
}

/// The ordinary answers, from the worktree root and from a subdirectory of it.
///
/// The root row is the floor the rest of the grid is read against, and six of
/// its twelve answers had never been asked for: the corpus already pins
/// `--git-dir`, `--show-toplevel` and `--is-inside-work-tree` there, so only the
/// other six are added rather than duplicating three case ids.
///
/// The subdirectory row is where the *prefix* half of discovery exists at all:
/// `--show-prefix` and `--show-cdup` are empty strings everywhere else in this
/// file, and the git dir stops being spellable as `.git`. `Dirty` carries it so
/// `status` has something to print — including a deletion inside the very
/// directory the command runs from, which is what makes the path rendering
/// (relative to the root, not to the cwd) measurable.
fn worktree_root(out: &mut Vec<Case>) {
    Situation::root(Shape::Linear).run(
        &[COMMON_DIR, IS_BARE, INSIDE_GIT_DIR, PREFIX, CDUP, ABS_GIT_DIR],
        out,
    );
    Situation::at(Shape::Dirty, "src")
        .run(&[GIT_DIR, TOPLEVEL, PREFIX, CDUP, ABS_GIT_DIR, STATUS], out);
}

/// Inside the git directory, where there is no worktree to be in.
///
/// The full set at `.git` itself: every answer differs from the worktree root's
/// — `--git-dir` collapses to `.`, `--is-inside-git-dir` flips, `--show-toplevel`
/// and `status` become errors, and `log` still has to work. This is the
/// situation that shipped broken.
///
/// One directory below, only three answers move (the git dir can no longer be
/// spelled relatively), so the deeper row is trimmed to those plus `log` as
/// evidence the repository is still usable from down there.
fn inside_git_dir(out: &mut Vec<Case>) {
    Situation::at(Shape::Linear, ".git").run(FULL, out);
    Situation::at(Shape::Linear, ".git/refs/heads")
        .run(&[GIT_DIR, COMMON_DIR, ABS_GIT_DIR, LOG], out);
}

/// A linked worktree, from inside it and from its administrative directory.
///
/// The only layout where `--git-dir` and `--git-common-dir` disagree, and the
/// only one where `HEAD` lives outside the common directory: `wt` is checked out
/// on `linked` while the main worktree is on `main`, so `symbolic-ref HEAD`
/// separates an implementation that reads the per-worktree `HEAD` from one that
/// reads the common one. Both rows are on [`Shape::Worktree`], which exists for
/// this and is used by nothing else.
///
/// The admin directory row is trimmed to the answers that are not already
/// pinned by the plain `.git` row above: it is *inside* a git dir like that one,
/// but the git dir it is inside is the worktree's, so the common dir, the
/// absolute git dir and `HEAD` all differ.
fn linked_worktree(out: &mut Vec<Case>) {
    Situation::at(Shape::Worktree, "wt").run(FULL, out);
    Situation::at(Shape::Worktree, ".git/worktrees/wt").run(
        &[GIT_DIR, COMMON_DIR, ABS_GIT_DIR, INSIDE_GIT_DIR, TOPLEVEL, SYMREF, LOG],
        out,
    );
}

/// A bare repository, at its root and in a subdirectory of it.
///
/// [`Shape::BehindRemote`] already contains one: `.remote.git`, the bare remote
/// it pushes to, which lives inside the fixture and is copied with it. No new
/// shape is needed, and using the existing one keeps the bare rows on a
/// repository that has real history to read.
///
/// The subdirectory row is the one that used to abort the process (exit 101), so
/// it is measured on ten columns rather than the three that strictly differ from
/// the root row: a crash is not specific to which question was asked, and the
/// regression this guards is "any command, one level down". Its `log` names
/// `main` because the bare repository's `HEAD` is unborn — `git init --bare`
/// left it on `master` and nothing ever wrote that ref — so a bare `log` would
/// measure the unborn-branch error twice instead of reading objects once.
fn bare(out: &mut Vec<Case>) {
    let full_with_named_log: Vec<&'static [&'static str]> =
        FULL.iter().map(|a| if *a == LOG { LOG_MAIN } else { *a }).collect();
    Situation::at(Shape::BehindRemote, ".remote.git").run(&full_with_named_log, out);
    Situation::at(Shape::BehindRemote, ".remote.git/refs").run(
        &[
            GIT_DIR,
            COMMON_DIR,
            IS_BARE,
            INSIDE_GIT_DIR,
            INSIDE_WORK_TREE,
            ABS_GIT_DIR,
            TOPLEVEL,
            STATUS,
            LOG_MAIN,
            SYMREF,
        ],
        out,
    );
}

/// `GIT_DIR` in its three spellings, each of which sends discovery down a
/// different branch of `setup_git_directory_gently_1`.
///
/// Reachable at all only because a case can now carry environment: `GIT_DIR`
/// went ignored by all but a handful of the port's call sites, and nothing in
/// the corpus could see it.
///
///  * `GIT_DIR=.` from inside `.git` makes the git directory its own worktree —
///    `--is-inside-work-tree` becomes true *and* `--is-inside-git-dir` stays
///    true, the only situation here where both are. `status` is omitted: see the
///    module header.
///  * An absolute `GIT_DIR` from a directory that is not a repository skips
///    discovery entirely and adopts the current directory as the worktree, so
///    `--show-toplevel` names a directory with no repository in it and `status`
///    reports every tracked file as deleted. The directory is created by the
///    runner on both sides, identically.
///  * A relative `GIT_DIR=.git` at the root names the repository the search
///    would have found anyway. Every answer matches the plain root row except
///    the one that must not: git prints the variable's own spelling back.
fn explicit_git_dir(out: &mut Vec<Case>) {
    Situation::at(Shape::Linear, ".git").with(GIT_DIR_DOT).run(
        &[
            GIT_DIR,
            COMMON_DIR,
            ABS_GIT_DIR,
            INSIDE_GIT_DIR,
            INSIDE_WORK_TREE,
            TOPLEVEL,
            PREFIX,
            CDUP,
            LOG,
            SYMREF,
        ],
        out,
    );
    Situation::at(Shape::Linear, "no-repo-here").with(GIT_DIR_ABS).run(
        &[GIT_DIR, ABS_GIT_DIR, TOPLEVEL, INSIDE_WORK_TREE, INSIDE_GIT_DIR, LOG, SYMREF, STATUS],
        out,
    );
    Situation::root(Shape::Linear).with(GIT_DIR_REL).run(
        &[GIT_DIR, ABS_GIT_DIR, TOPLEVEL, INSIDE_GIT_DIR, INSIDE_WORK_TREE, STATUS, LOG],
        out,
    );
}

/// A submodule, from its checkout and from its git directory.
///
/// The checkout is the `.git`-file indirection: `sub/.git` is a file naming
/// `../.git/modules/sub`, so an implementation that only knows about `.git`
/// directories finds the superproject instead — the full set is pinned because
/// every one of the twelve then answers about the wrong repository.
///
/// The git directory is the reverse indirection, `core.worktree`, and it is the
/// one place in this file where `--show-toplevel` succeeds while
/// `--is-inside-git-dir` is true, and where `--show-cdup` prints an absolute
/// path. `status` works there too, against the worktree the config points back
/// at.
fn submodule(out: &mut Vec<Case>) {
    Situation::at(Shape::Submodule, "sub").run(FULL, out);
    Situation::at(Shape::Submodule, ".git/modules/sub").run(
        &[
            GIT_DIR,
            COMMON_DIR,
            ABS_GIT_DIR,
            INSIDE_GIT_DIR,
            INSIDE_WORK_TREE,
            TOPLEVEL,
            CDUP,
            STATUS,
            LOG,
            SYMREF,
        ],
        out,
    );
}

/// `GIT_CEILING_DIRECTORIES`: the one form that stops the search, and the two
/// that must *not*.
///
/// The ceiling is the first directory git refuses to look in, so the same
/// `GIT_CEILING_DIRECTORIES={repo}` means opposite things one directory apart:
/// from `src` the repository at `{repo}` is behind the ceiling and nothing is
/// found, while from `{repo}` itself the search never walks up at all and finds
/// it immediately — git compares a ceiling only against the directories it would
/// walk *up* into (`longest_ancestor_length()` requires the ceiling to be a
/// *proper* ancestor). A ceiling matching nothing on the way up changes nothing.
///
/// All three are pinned together because they are each other's over-correction:
/// stopping as soon as any ceiling is seen breaks the second, and searching the
/// ceiling itself breaks the first.
///
/// The failing row is one column rather than three. The ceiling is decided
/// before any query runs, so every column collapses to the same
/// `not a git repository` — and `log` cannot be one of them, because the port
/// renders a failed discovery as `zvcs: log: …` with exit 1 rather than git's
/// `fatal:` with 128 in *any* directory that has no repository, ceiling or not.
/// That is a divergence of its own and not this file's to pin.
fn ceilings(out: &mut Vec<Case>) {
    Situation::at(Shape::Linear, "src").with(CEILING_START).run(&[GIT_DIR], out);
    Situation::root(Shape::Linear).with(CEILING_START).run(&[GIT_DIR, LOG], out);
    Situation::at(Shape::Linear, "src").with(CEILING_MISS).run(&[GIT_DIR, TOPLEVEL, PREFIX], out);
}
