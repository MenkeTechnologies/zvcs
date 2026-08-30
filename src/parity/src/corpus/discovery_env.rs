//! Repository **location**: the overrides that move it, the readouts that
//! report it, and the gates that decide whether git will open it at all.
//!
//! [`super::discovery`] is the nearest neighbour and the file to read first. It
//! owns *the walk*: from a directory, git climbs until it finds a repository,
//! and the answer is a function of where the walk started (the worktree root, a
//! subdirectory, inside `.git`, inside a linked worktree, inside a bare
//! repository, inside a submodule) and of the three variables that redirect or
//! stop it (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_CEILING_DIRECTORIES`). Its columns
//! are one query each and it spells them at a dozen vantage points.
//!
//! This file owns everything the walk hands off to once it has finished, which
//! is where the port is wrong. Four questions, none of them reachable from a
//! situation-and-a-column grid:
//!
//!  1. **Which directory the answer is *spelled relative to*.** Git does not
//!     echo back the git directory it was given. It re-renders it — relative
//!     when the current directory is the worktree root, absolute when it is not,
//!     and `--git-common-dir` follows `--git-dir`'s spelling rather than having
//!     one of its own. A port that prints the value it was handed agrees at the
//!     worktree root and disagrees everywhere else.
//!  2. **The overrides that do not come from the walk at all**: `core.worktree`
//!     and `core.bare` out of the repository's own config, `GIT_COMMON_DIR`,
//!     `GIT_OBJECT_DIRECTORY`, `GIT_INDEX_FILE`, `--git-dir`/`--work-tree` as
//!     *global options*, and every conflicting combination of them.
//!  3. **The gates.** `GIT_OBJECT_DIRECTORY` and `GIT_COMMON_DIR` are consulted
//!     by `setup.c:is_git_directory()`, so pointing either at nothing stops
//!     `.git` from being a repository; and `ensure_valid_ownership()` refuses a
//!     repository whose directory is owned by somebody else unless
//!     `safe.directory` says otherwise. Both answer "no repository here" about a
//!     repository that is plainly there.
//!  4. **`--git-path`, `--path-format` and the serving-side namespace** — the
//!     readouts that report a location rather than a name.
//!
//! # The five places the port operates on a different repository
//!
//! This is the question the file exists for, and it is not the same question as
//! "which readout disagrees". A spelling difference is a wrong *answer*; the
//! rows below are a wrong *repository*, and four of the five are reachable by a
//! write. Every one was reproduced by hand against stock 2.55.0 and
//! corroborated by git 2.50.1 through the harness's second oracle.
//!
//!  1. **A gitfile as `GIT_DIR`, from the superproject root** — the port pairs
//!     the submodule's git directory with the **superproject's** working tree.
//!     `add -A` under it rewrites the submodule's index from the superproject's
//!     tree. See [`gitfile_as_git_dir`].
//!  2. **`core.worktree` once discovery had to climb** — stock moves the
//!     worktree, the port keeps the one the walk found, so `ls-files` and
//!     `status` answer about different directories. See [`core_worktree`].
//!  3. **`core.worktree` with an explicit `--git-dir`** — the port ignores the
//!     setting outright. Same function.
//!  4. **`GIT_COMMON_DIR` naming somewhere that is not there** — stock refuses
//!     the repository (`is_git_directory()` looks for `refs/`/`objects/` under
//!     the *common* directory); the port ignores the variable and operates on a
//!     repository git declined to open. See [`common_dir`].
//!  5. **A worktree that is not there** — `GIT_WORK_TREE=nosuch` and
//!     `--work-tree=nosuch` are exit 0 to stock, which prints the path it was
//!     given; the port refuses. `core.worktree = nosuch` is the mirror image:
//!     stock refuses with `cannot chdir`, the port carries on against the
//!     worktree the walk found. See [`work_tree_alone`] and [`core_worktree`].
//!
//! The named combinations that turned out **not** to move the repository, each
//! measured rather than assumed: `--git-dir` with `core.bare=true`;
//! `GIT_WORK_TREE` against a bare repository (both pair it correctly, and
//! `status` lists the same six untracked paths on both); `GIT_DIR` naming a
//! linked worktree's admin directory *or* its gitfile (right worktree on both);
//! `GIT_CEILING_DIRECTORIES` in every list form; and `GIT_INDEX_FILE` /
//! `GIT_OBJECT_DIRECTORY` as routing. Four of those still *fail*, on
//! `--git-common-dir` alone — the spelling defect of [`git_dir_option`], not a
//! second repository.
//!
//! # One thing a hand check outside the harness will not reproduce
//!
//! The runner's workdir is `std::env::temp_dir()`, which on macOS is under
//! `/var/folders/…` — a symlink to `/private/var/folders/…`. Stock echoes the
//! `GIT_DIR` it was handed; the port canonicalizes it. So the pair
//! `GIT_DIR={repo}/.remote.git GIT_WORK_TREE={repo}` diverges on
//! `--git-common-dir` in the harness (`<REPO>/.remote.git` against
//! `/private<REPO>/.remote.git`, the `/private` left over because
//! `mask_paths` substitutes the un-canonicalized root first) and does **not**
//! diverge when the same case is replayed by hand under a root that is already
//! canonical. Both spellings name the same directory, so this is not a wrong
//! repository — but it is a real byte difference, it reproduces on repeat, and
//! git 2.50.1 corroborates it. Anything replayed under `/private/tmp` is
//! blind to it.
//!
//! # How territory divides with the adjacent modules
//!
//! * **`discovery.rs`** — the walk, as above. Its `GIT_DIR` cases use the three
//!   spellings that name *the repository the walk would have found anyway*
//!   (`GIT_DIR_DOT` `.`, `GIT_DIR_REL` `.git`, `GIT_DIR_ABS` `{repo}/.git`);
//!   this file's name a **different** repository from the one under the cwd,
//!   which is where the port stops agreeing. Its ceiling cases are the two
//!   single-element values in the corpus (`{repo}`, `{repo}/no-such-dir` — the
//!   only two anywhere, checked by grep across `corpus/`); this file adds the
//!   *list* forms (an empty element, two elements, a relative element).
//! * **`env_layer.rs`** — seven `GIT_*` variables chosen for changing what a
//!   *read* prints: the object sources, `GIT_REPLACE_REF_BASE`,
//!   `GIT_CONFIG_PARAMETERS`, `GIT_ADVICE`, `GIT_PAGER_IN_USE`,
//!   `GIT_TEST_DATE_NOW`, and `GIT_NAMESPACE` on `ls-remote` alone. Its header
//!   records `GIT_COMMON_DIR` as needing "a layout no template builds" and its
//!   namespace group records that the local ref listings ignore the variable.
//!   Both are re-measured here and both hold — see [`common_dir`] for the layout
//!   `GIT_COMMON_DIR` actually needs (none: a plain repository is enough,
//!   because the variable's *own value* is what the readout must print) and
//!   [`namespace_serving`] for the two verbs that do honour it.
//! * **`globals_layer.rs`** — the option layer before the verb. It owns
//!   `--bare`, `-C sub`, `--namespace=` on `ls-remote`, `--config-env`,
//!   `--shallow-file`, `--no-advice`, and the pathspec-magic switches in their
//!   *option* spelling on `ls-files`/`grep`. It has **no** `--git-dir` and no
//!   `--work-tree` case (grep: the two strings appear in that file only inside
//!   comments and as a `rev-parse` *argument*). The two that do exist anywhere
//!   are in `corpus.rs`'s own globals block — `-C src --git-dir=../.git` asking
//!   `rev-parse --git-dir`, and `--git-dir=.git --work-tree=.` asking
//!   `rev-parse --show-toplevel`. Every other combination is this file's,
//!   together with the two pathspec-magic *conflicts* (`literal` with anything,
//!   `glob` with `noglob`) and the environment spelling of the same four
//!   switches.
//! * **`worktree_lifecycle.rs`** — owns `rev-parse --git-path` on the layout
//!   where the routing splits: [`Shape::Worktree`] from three vantage points and
//!   [`Shape::WorktreeLocked`] from inside the locked tree. This file asks the
//!   same query where there is **no** split — a plain repository from a
//!   subdirectory, a bare repository at its root, a submodule checkout — and
//!   under the variables that reroute one path without moving the git directory
//!   (`GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`). No argv is shared with it.
//!   `submodule_family.rs:661` is the one other `--git-path` in the corpus —
//!   `config` alone, from inside `sub` — and this file's submodule row asks
//!   `HEAD`, `config` and `index` together, which is a different argv and a
//!   different question: whether the three land in the same directory.
//! * **`worktree_index.rs`** — `update-index`, `checkout-index`,
//!   `sparse-checkout`, `clean` and the `worktree` verb on shapes with no linked
//!   worktree. Nothing about where the repository is.
//! * **`config_reads.rs`** — a setting changes what some *other* verb prints,
//!   90-odd `color.*`/`diff.*`/`status.*`/`log.*` keys. It sets no `core.*` key
//!   that moves the repository.
//! * **`config_resolution.rs`** — how a configuration *value* is resolved: the
//!   include graph, the scope stack, the type conversions, the file grammar.
//!   The two files do not overlap at all: it sets no `core.worktree`, no
//!   `core.bare` and no `safe.directory` (its only mention of `core.bare` is a
//!   `config --global --get` of a key nothing set). If they ever do meet, the
//!   split is that it owns how a *value* is parsed and this file owns what the
//!   value does to whether a repository opens.
//! * **`submodule_deep.rs` / `submodule_family.rs`** — the submodule as a
//!   *topology*. `submodule_family` owns `GIT_DIR={repo}/.git/modules/sub` (five
//!   readouts), `GIT_DIR=.git` from inside `sub` (`--git-dir` only), and
//!   `rev-parse --resolve-git-dir .git` **from inside `sub`**;
//!   `submodule_deep` owns `--show-superproject-working-tree` from three
//!   positions and one `GIT_DIR`+`GIT_WORK_TREE` pair naming the submodule.
//!   Neither names the **gitfile** as `GIT_DIR` *from the superproject root*,
//!   which is the combination in [`gitfile_as_git_dir`] where the port operates
//!   on — and writes into — the wrong repository.
//!
//! # What is new here, and what is not
//!
//! Checked by grep across `corpus/` and `corpus.rs` rather than asserted. No
//! case in the corpus before this file used: `core.worktree` or `core.bare` set
//! in any scope (`submodule_family.rs` *reads* `core.worktree` back through
//! `config --get`; nothing sets either key); `GIT_COMMON_DIR` (`env_layer.rs`
//! retired it, `sparse_family.rs` only names it in prose);
//! `GIT_TEST_ASSUME_DIFFERENT_OWNER`; `safe.directory`; `--path-format` in
//! either value (`fuzz.rs:2796` samples it, nothing curated); `GIT_NAMESPACE`
//! or `--namespace=` on `upload-pack`/`receive-pack`; `GIT_DIR` naming a
//! **gitfile**; or a `GIT_CEILING_DIRECTORIES` value with more than one element.
//!
//! Two things the first draft of this header called new are **not**, and the
//! rows here are additions to them rather than the first of their kind:
//!
//!  * `--git-dir`/`--work-tree` as a global option — `corpus.rs`'s globals block
//!    has two rows (above). This file has eleven, and asks eight queries where
//!    those ask one.
//!  * `--resolve-git-dir` — `submodule_family.rs:662` passes it, on `.git` from
//!    inside `sub`. This file adds the other four operands, including the two
//!    refusals.
//!
//! The one group here that is *not* new territory is the pathspec-magic
//! switches: `pathspec_stdin.rs` owns them and `globals_layer.rs` owns their
//! option spelling. [`pathspec_magic_env`] is trimmed to the three *questions*
//! neither asks — the `off` spellings on the variables where `off` is not
//! pinned, the boolean rule on a pathspec `icase` can actually see, and the two
//! exclusion refusals that are not pinned at all — and says so.
//!
//! # Determinism: this territory is made of paths
//!
//! Almost every byte compared below is a filesystem path, and the two sides run
//! against copies at different roots (`<workdir>/stock` and `<workdir>/zvcs` —
//! different *lengths*, not just different names). Three things make that
//! survivable, and one thing does not:
//!
//!  * `runner::mask_paths` rewrites each side's own fixture root to `<REPO>` and
//!    the shared home to `<HOME>`, in stdout, stderr and the state digest, on
//!    bytes, and each path in both its symlinked and its canonicalized form
//!    (`runner.rs`, `mask_paths`). So `--absolute-git-dir`, `--show-toplevel`
//!    and every absolute `--git-path` answer are comparable, and they are the
//!    readouts this file leans on hardest.
//!  * A case's environment may not contain a literal absolute path
//!    (`runner::apply_case_env` asserts it); `{repo}` is substituted per side.
//!    Every value below that names a path is written that way.
//!  * Every case in this file was replayed against **stock alone** at two roots
//!    whose lengths differ by 33 bytes (`…/d/a` against
//!    `…/d/bbbbbbbbbb/cccccccccc/dddddddddd/e`), in a fresh build of the shape
//!    each time, and required to render byte-identically after the root was
//!    masked. That is what would catch a padding or truncation that depends on
//!    how long the path happens to be, and it is the check that makes a
//!    `<REPO>`-masked answer worth comparing at all.
//!
//! **The masking is per side, of that side's own root**, which is what makes it
//! honest here: it cannot make two *different* answers equal, because a port
//! answering about the stock copy would print a path that is not the port's own
//! root and would therefore not be masked. What it can hide is a length
//! difference, which is what the two-root replay above is for.
//!
//! **A case's *configuration* value is not substituted** — it is a literal
//! written into a file — so a setting whose value must be this side's own
//! absolute path cannot be expressed. That rules out exactly one measurement,
//! and it is a live divergence rather than a hypothetical: see [`ownership`].
//! `core.worktree` escapes the rule only because git resolves a relative
//! `core.worktree` against the git directory, so `../src` names the same place
//! `{repo}/src` would.
//!
//! # Four things that cannot be measured here, and are absent rather than faked
//!
//!  * **`GIT_DISCOVERY_ACROSS_FILESYSTEM`.** It changes the walk only where the
//!    walk crosses a mount point, and a case is one argv against a directory
//!    tree the runner copied — nothing in the harness can create a filesystem
//!    boundary. Measured anyway, both values, from `src/`: `1`, `0` and unset
//!    give the identical two lines on both binaries. A case would be vacuous,
//!    so there is none.
//!  * **A repository that is genuinely owned by another user.** Both copies are
//!    created by the process running the harness, so `st_uid` is the same on
//!    both sides and equal to the euid. Git's own escape hatch closes the gap
//!    without needing a second user — `GIT_TEST_ASSUME_DIFFERENT_OWNER=1` makes
//!    `ensure_valid_ownership()` take the refusing branch — and [`ownership`]
//!    uses it. What stays out of reach is the ownership *check itself*: these
//!    cases measure what git does once it has decided the repository is
//!    dubious, never how it decides.
//!  * **A namespace that actually holds refs.** `GIT_NAMESPACE=<n>` reads from
//!    `refs/namespaces/<n>/`, no shape builds that hierarchy, and a case is one
//!    argv — so the only namespaced advertisement reachable from a single
//!    invocation is the **empty** one. Every positive row (a ref inside a
//!    namespace being advertised, and stripped back to its un-namespaced name
//!    on the wire) needs two steps and therefore belongs to `sequences.rs`, not
//!    here. See [`namespace_serving`] for what the empty side still separates.
//!  * **`safe.directory = %(prefix)/<absolute path>`.** Measured, and a live
//!    divergence — see [`ownership`] for the transcript — but unwritable,
//!    because the useful value is this side's own absolute path and a
//!    configuration value is a literal the runner never substitutes.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Append this module's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    path_format(out);
    git_dir_option(out);
    work_tree_alone(out);
    core_worktree(out);
    core_bare(out);
    gitfile_as_git_dir(out);
    common_dir(out);
    object_dir_and_index(out);
    git_path_without_a_split(out);
    ownership(out);
    namespace_serving(out);
    pathspec_magic_env(out);
    bad_git_dir(out);
    ceiling_lists(out);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One `rev-parse` reading several location facts in one invocation.
///
/// Several queries per case rather than one, which is the opposite of
/// [`super::discovery`]'s choice and is deliberate: that file varies the
/// *situation* and holds the query fixed, so one query per case is what makes
/// its grid readable. Here the situation is the point and the queries are the
/// instrument, and a port that gets the git directory's spelling wrong gets it
/// wrong in company — the failure is worth reading as one row.
fn rp(out: &mut Vec<Case>, shape: Shape, cwd: Option<&'static str>, args: &[&str]) {
    let mut case = Case::new("rev-parse", args, shape);
    if let Some(cwd) = cwd {
        case = case.in_dir(cwd);
    }
    out.push(case);
}

/// The location facts that move when a worktree is declared: where the git
/// directory is, how it is spelled, where the worktree root is, and whether the
/// current directory counts as being in one.
const WHERE: &[&str] = &[
    "rev-parse",
    "--git-dir",
    "--absolute-git-dir",
    "--git-common-dir",
    "--show-toplevel",
    "--is-inside-work-tree",
    "--is-inside-git-dir",
    "--is-bare-repository",
    "--show-prefix",
];

/// The same, trimmed to the four answers that move on their own, for the rows
/// where the common dir, the bareness and the prefix add nothing.
const SPELLING: &[&str] =
    &["rev-parse", "--git-dir", "--absolute-git-dir", "--show-toplevel", "--is-inside-work-tree"];

// ---------------------------------------------------------------------------
// 1. --path-format
// ---------------------------------------------------------------------------

/// `--path-format=absolute|relative`: the switch that re-renders every
/// path-valued query that *follows* it.
///
/// Zero cases in the corpus pass it (`fuzz.rs` samples it; nothing curated). It
/// is not a flag but a mode, and three properties are what the seven cases
/// below are for:
///
///  * It is **positional**. Measured on stock 2.55.0 from `src/`:
///    `--path-format=absolute --git-dir --path-format=relative --git-dir` prints
///    `<REPO>/.git` and then `../.git` — one argv, two answers, from the same
///    repository. A port that parses it as a flag prints the same line twice.
///  * `--absolute-git-dir` is **immune** to it: under `--path-format=relative`
///    it still prints the absolute path, because it is a different query and not
///    `--git-dir` in a mode.
///  * An unknown value is a refusal (`fatal: unknown argument to --path-format:
///    bogus`, exit 128) taken before any query runs.
///
/// **The port passes all seven.** It is positional on the port too, it leaves
/// `--absolute-git-dir` alone, and it refuses `bogus` with the identical
/// message and exit code. Recorded because a group with no divergence in it is
/// worth being able to tell apart from a group that was never run: these are
/// pins on behaviour that already agrees, and their job is to stay green.
fn path_format(out: &mut Vec<Case>) {
    rp(
        out,
        Shape::Linear,
        Some("src"),
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--show-toplevel",
            "--show-prefix",
            "--show-cdup",
        ],
    );
    rp(
        out,
        Shape::Linear,
        Some("src"),
        &["rev-parse", "--path-format=absolute", "--git-dir", "--path-format=relative", "--git-dir"],
    );
    rp(out, Shape::Linear, None, &["rev-parse", "--path-format=relative", "--absolute-git-dir"]);
    out.push(Case::strict("rev-parse", &["rev-parse", "--path-format=bogus", "--git-dir"], Shape::Linear));

    // The mode applied where the *relative* answer is not the one a reader
    // would predict. Measured on stock 2.55.0, and the port agrees on all
    // three:
    //
    //   Worktree, cwd=wt        <REPO>/.git/worktrees/wt  <REPO>/.git  <REPO>/wt
    //   BehindRemote, cwd=.remote.git   <REPO>/.remote.git  <REPO>/.remote.git
    //   Linear, cwd=.git        <REPO>/.git  (and `--show-cdup` prints nothing)
    //
    // Only the first is a layout where the git directory and the common
    // directory are two different places; the second is the bare repository
    // spelling both of them as itself, and the third is the one vantage point
    // where `--show-cdup` has no line to print at all.
    rp(
        out,
        Shape::Worktree,
        Some("wt"),
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--show-toplevel",
        ],
    );
    rp(
        out,
        Shape::BehindRemote,
        Some(".remote.git"),
        &["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"],
    );
    rp(
        out,
        Shape::Linear,
        Some(".git"),
        &["rev-parse", "--path-format=absolute", "--git-dir", "--show-cdup"],
    );
}

// ---------------------------------------------------------------------------
// 2. --git-dir and --work-tree as global options
// ---------------------------------------------------------------------------

/// `--git-dir` and `--work-tree` on the command line.
///
/// `globals_layer.rs` owns the option layer and has neither; the only two rows
/// anywhere are `corpus.rs`'s (`-C src --git-dir=../.git` asking one query, and
/// `--git-dir=.git --work-tree=.` asking one query). So the command-line half of
/// repository selection was measured at two points and the readouts that move
/// were not asked at either — even though `git.c:handle_options` resolves the
/// option *after* `-C` has already moved the current directory, and the two
/// therefore do not commute.
///
/// Both orders are here for that reason, and they do not merely spell the same
/// answer differently — they disagree about whether there is an answer at all.
/// Measured on stock 2.55.0 at the fixture root:
///
/// ```text
/// -C src --git-dir=../.git   ../.git   <REPO>/.git   <REPO>/src   exit 0
/// --git-dir=.git -C src      fatal: not a git repository: '.git'   exit 128
/// ```
///
/// `--git-dir` is resolved *after* every `-C` has run, so the relative value is
/// read from the directory `-C` moved to and `.git` is not there. A port that
/// resolves the option where it appears in argv gets the second row exit 0 and
/// the wrong repository. The port exits 128 and reaches the right verdict by a
/// different route, and says so in different words —
/// `fatal: not a git repository (or any of the parent directories): .git`,
/// which is why that row is [`Case::strict`].
///
/// The eight-query readout is what the two existing rows do not have, and it is
/// where the port is wrong. `--git-common-dir` **follows `--git-dir`'s own
/// spelling** and does not have one of its own; the port re-renders it
/// absolutely whenever the current directory is not the worktree root. Measured
/// on stock 2.55.0 against the port, `--git-dir` / `--git-common-dir` only:
///
/// ```text
///                             stock                  port
/// --git-dir=.git              .git      .git         .git      .git        (agree)
/// --git-dir=.git --work-tree=src
///                             .git      .git         .git      <REPO>/.git
/// -C src --git-dir=../.git    ../.git   ../.git      ../.git   <REPO>/.git
/// cwd=src, --git-dir=../.git  ../.git   ../.git      ../.git   <REPO>/.git
/// ```
///
/// The first row is the control: option accepted, same repository, same
/// spelling as no option at all. It is the only one of the four the port passes.
fn git_dir_option(out: &mut Vec<Case>) {
    let g = |out: &mut Vec<Case>, globals: &[&[&str]], args: &[&str], cwd: Option<&'static str>| {
        let mut case = Case::new("rev-parse", args, Shape::Linear).with_globals(globals);
        if let Some(cwd) = cwd {
            case = case.in_dir(cwd);
        }
        out.push(case);
    };

    g(out, &[&["--git-dir=.git"]], WHERE, None);
    g(out, &[&["--git-dir=../.git"]], WHERE, Some("src"));
    g(out, &[&["--git-dir=.git"], &["--work-tree=src"]], WHERE, None);
    g(out, &[&["-C", "src"], &["--git-dir=../.git"]], WHERE, None);
    out.push(
        Case::strict("rev-parse", WHERE, Shape::Linear)
            .with_globals(&[&["--git-dir=.git"], &["-C", "src"]]),
    );

    // A `--git-dir` reached from a directory with no repository in it: the
    // option twin of `discovery.rs`'s `GIT_DIR_ABS` environment row. The
    // directory is created by the runner on both sides, and the value is
    // relative because a global option is argv and argv is not substituted —
    // only `Case::env` values carry `{repo}`.
    g(out, &[&["--git-dir=../.git"]], WHERE, Some("no-repo-here"));

    // The bare repository inside the fixture, named as the git directory from
    // *outside* it — with a worktree and without one. Without, `--show-toplevel`
    // is the refusal `this operation must be run in a work tree`; with, the
    // fixture root becomes the worktree of a repository that is not under it.
    out.push(
        Case::new("rev-parse", WHERE, Shape::BehindRemote)
            .with_globals(&[&["--git-dir=.remote.git"], &["--work-tree=."]]),
    );
    // The same pairing through the environment rather than the option layer.
    // Both sides land on the same repository and the same worktree —
    // `--absolute-git-dir` and `--show-toplevel` agree — and `--git-common-dir`
    // still differs, though not in the way the option rows differ: here stock
    // echoes the value it was handed and the port canonicalizes it, which is
    // visible only because the runner's workdir is reached through a symlink.
    // See the module header; a replay under an already-canonical root shows no
    // difference at all on this row.
    //
    // `status` is the second half: the bare repository's index is empty, so
    // every path in the fixture root is untracked to it, which is the
    // observable consequence of having declared a worktree for a repository
    // that has none. That row agrees on both sides, and it is the one that says
    // the port is operating on the repository it was told to.
    //
    //     ?? .remote.git/  ?? README.md  ?? clash.txt  ?? mine.txt
    //     ?? shared.txt    ?? src/
    const BARE_PAIR: &[(&str, &str)] =
        &[("GIT_DIR", "{repo}/.remote.git"), ("GIT_WORK_TREE", "{repo}")];
    out.push(Case::new("rev-parse", WHERE, Shape::BehindRemote).with_env(BARE_PAIR));
    out.push(
        Case::new("status", &["status", "--porcelain"], Shape::BehindRemote).with_env(BARE_PAIR),
    );
    out.push(
        Case::strict("rev-parse", WHERE, Shape::BehindRemote)
            .with_globals(&[&["--git-dir=.remote.git"]]),
    );
    out.push(
        Case::new("rev-parse", &["rev-parse", "--is-bare-repository", "--show-toplevel"], Shape::BehindRemote)
            .with_globals(&[&["--bare"], &["--git-dir=.remote.git"]]),
    );

    // What the declared worktree is actually made of, rather than what
    // `rev-parse` says about it.
    out.push(
        Case::new("status", &["status", "--porcelain"], Shape::Linear)
            .with_globals(&[&["--git-dir=.git"], &["--work-tree=src"]]),
    );
}

// ---------------------------------------------------------------------------
// 3. A worktree with no git directory beside it
// ---------------------------------------------------------------------------

/// `--work-tree` / `GIT_WORK_TREE` **without** a `--git-dir` beside it.
///
/// It is not unreached — `init_family.rs` sets `GIT_WORK_TREE` alone twice on
/// `init`, and `corpus.rs` once on `ls-files -o` inside a bare repository — but
/// never on a repository the walk has *already found*, which is the only
/// arrangement where declaring a worktree replaces one rather than supplying the
/// missing one.
///
/// Declaring a worktree does not stop the walk — git still finds `.git` by
/// climbing — but it does replace the worktree the walk would have paired it
/// with, and that changes three answers at once. Measured on stock 2.55.0 at the
/// fixture root with `GIT_WORK_TREE=src`, against the port:
///
/// ```text
///                        stock                        port
/// --git-dir              .git                         <REPO>/.git
/// --absolute-git-dir     <REPO>/.git                  <REPO>/.git
/// --show-toplevel        <REPO>/src                   <REPO>/src
/// --is-inside-work-tree  false                        false
/// --show-cdup            <REPO>/src                   <REPO>/src
/// ```
///
/// `--git-dir` stays relative on stock because the current directory is the git
/// directory's parent — the declared worktree does not enter into it — and the
/// port renders it absolutely the moment the current directory stops being the
/// worktree root. That is the same rule it gets wrong for `--git-common-dir` in
/// [`git_dir_option`], seen through a second query. `--show-cdup` is absolute on
/// both because the climb is *downward* and cannot be spelled with `../`.
///
/// One row carries the same environment on `status`, because `rev-parse`
/// reporting the right worktree and the command *using* it are different
/// claims: with the worktree moved to `src`, every tracked path is missing from
/// it and the real `src/lib.rs` is an untracked `lib.rs` — ` D README.md`,
/// ` D src/lib.rs`, `?? lib.rs`, on both sides.
///
/// The last pair is the one that separates "declared" from "existing", and it is
/// where the port stops agreeing: a worktree that is **not there** is not an
/// error to git. Stock prints the path that does not exist and exits 0; the port
/// exits 128 with `fatal: this operation must be run in a work tree`, for both
/// the environment and the option spelling.
fn work_tree_alone(out: &mut Vec<Case>) {
    const AT_SRC: &[(&str, &str)] = &[("GIT_WORK_TREE", "{repo}/src")];
    const REL_SRC: &[(&str, &str)] = &[("GIT_WORK_TREE", "src")];
    const MISSING: &[(&str, &str)] = &[("GIT_WORK_TREE", "nosuch")];

    let with = |out: &mut Vec<Case>, cmd: &'static str, args: &[&str], env: &[(&str, &str)]| {
        out.push(Case::new(cmd, args, Shape::Linear).with_env(env));
    };

    let full: &[&str] = &[
        "rev-parse",
        "--git-dir",
        "--absolute-git-dir",
        "--show-toplevel",
        "--is-inside-work-tree",
        "--is-bare-repository",
        "--show-prefix",
        "--show-cdup",
    ];
    with(out, "rev-parse", full, REL_SRC);
    with(out, "rev-parse", full, AT_SRC);
    with(out, "status", &["status", "--porcelain"], AT_SRC);
    // `ls-files` is deliberately absent: it reads the index and the index does
    // not move with the worktree, so it prints the same two paths
    // (`README.md`, `src/lib.rs`) as it does with no worktree declared at all.
    // Measured against stock before it was dropped.

    // The same declared from the command line rather than the environment.
    out.push(Case::new("rev-parse", full, Shape::Linear).with_globals(&[&["--work-tree=src"]]));
    out.push(
        Case::new("status", &["status", "--porcelain"], Shape::Linear)
            .with_globals(&[&["--work-tree=src"]]),
    );

    // Declared *at* the current directory, from inside `src`: the spelling that
    // leaves the cwd inside the declared worktree, so `--show-toplevel` becomes
    // `<REPO>/src` and `--show-prefix` and `--show-cdup` both empty out.
    out.push(
        Case::new("rev-parse", full, Shape::Linear).in_dir("src").with_env(&[("GIT_WORK_TREE", ".")]),
    );
    // `--work-tree=..` from `src` is *not* here: it names the directory the walk
    // would have paired with the repository anyway, so every answer is the one
    // `discovery.rs` already pins for a plain subdirectory. Measured, not assumed.

    // A worktree that does not exist. Stock prints it and exits 0; nothing is
    // stat'ed until something needs the directory.
    with(out, "rev-parse", full, MISSING);
    out.push(Case::new("rev-parse", full, Shape::Linear).with_globals(&[&["--work-tree=nosuch"]]));

    // The git directory pointed at itself as a worktree: `.git` is both, and
    // `--is-inside-git-dir` has to answer about a directory that is also the
    // worktree root.
    //
    // The second row is the one that matters, and it is the file's cleanest
    // reading of the two rules the port has confused. Standing in `.git` with
    // `GIT_DIR=.` and the worktree declared at the fixture root, stock answers:
    //
    //     --git-dir <REPO>/.git   --is-inside-git-dir false   --show-prefix .git/
    //
    // absolute because the cwd is not the worktree root, and *not* inside the
    // git directory because the declared worktree contains it, so `.git/` is a
    // prefix inside a worktree rather than a location inside a repository. The
    // port prints the value it was handed (`.`) and answers `true`.
    with(out, "rev-parse", WHERE, &[("GIT_WORK_TREE", "{repo}/.git")]);
    out.push(
        Case::new("rev-parse", WHERE, Shape::Linear)
            .in_dir(".git")
            .with_env(&[("GIT_DIR", "."), ("GIT_WORK_TREE", "{repo}")]),
    );
}

// ---------------------------------------------------------------------------
// 4. core.worktree
// ---------------------------------------------------------------------------

/// `core.worktree` in the repository's own config: the worktree redirection that
/// arrives from a *file* rather than from the environment or the command line.
///
/// No case in the corpus sets it in any scope. It is the one override that a
/// user does not have to pass — it is simply true of the repository from then
/// on — and it is where the port operates on the wrong directory:
///
/// ```text
/// core.worktree = ../src, cwd = src/, stock 2.55.0 vs the port
///                      stock                     port
/// --show-toplevel      <REPO>/src                <REPO>
/// --show-prefix        (empty)                   src/
/// --show-cdup          (empty)                   ../
/// ls-files             README.md, src/lib.rs     lib.rs
/// status --porcelain    D README.md              (nothing)
///                       D src/lib.rs
///                      ?? lib.rs
/// ```
///
/// At the fixture *root* the setting is honoured by both — same worktree, same
/// `status`. So the defect is not "the key is ignored" but "the key is dropped
/// once discovery had to climb", which is why the pair of rows is the case and
/// neither one alone is. (The root row still fails, on `--git-dir`: stock `.git`
/// against the port's `<REPO>/.git`. That is the spelling defect of
/// [`git_dir_option`] showing through, not a second worktree defect, and the
/// two are told apart by `--show-toplevel` agreeing on that row and not on the
/// one below it.)
///
/// The value is relative on purpose. Git resolves a relative `core.worktree`
/// against the git directory, so `../src` from `.git` is `{repo}/src`; a case's
/// configuration value is a literal that is never substituted, so the absolute
/// spelling would name one side's copy to both. See the module header.
///
/// `core.worktree = nosuch` is the third row and a different kind of answer
/// again: git *chdirs* into the declared worktree during setup, so a value that
/// is not there is `fatal: cannot chdir to 'nosuch': No such file or directory`
/// and exit 128 — not the exit-0 "print the path anyway" that `GIT_WORK_TREE`
/// gets for the same non-existent directory. Two overrides, one meaning, two
/// outcomes.
///
/// The port gets that pair backwards on both halves. Where stock refuses the
/// chdir it carries on, from `src/`, against the worktree the walk found:
/// `<REPO>` as the toplevel, `src/` as the prefix, exit 0. Where stock prints
/// a non-existent `GIT_WORK_TREE` and exits 0, it refuses. On `status` it
/// refuses too, but with the wrong refusal —
/// `fatal: this operation must be run in a work tree` where stock says
/// `fatal: cannot chdir to 'nosuch'` — which is why both `nosuch` rows are
/// [`Case::strict`]: on `status` the exit code agrees and only the message
/// carries the difference.
fn core_worktree(out: &mut Vec<Case>) {
    let cfg = |value: &str| vec![ConfigEntry::set(ConfigScope::Repo, "core.worktree", value)];
    let at = |out: &mut Vec<Case>, cwd: Option<&'static str>, cmd: &'static str, args: &[&str], value: &str| {
        let mut case = Case::new(cmd, args, Shape::Linear).with_scoped_config(cfg(value));
        if let Some(cwd) = cwd {
            case = case.in_dir(cwd);
        }
        out.push(case);
    };

    let full: &[&str] = &[
        "rev-parse",
        "--git-dir",
        "--absolute-git-dir",
        "--show-toplevel",
        "--is-inside-work-tree",
        "--show-prefix",
        "--show-cdup",
    ];

    at(out, None, "rev-parse", full, "../src");
    at(out, Some("src"), "rev-parse", full, "../src");
    at(out, None, "status", &["status", "--porcelain"], "../src");
    at(out, Some("src"), "status", &["status", "--porcelain"], "../src");
    // `ls-files` only from `src`: at the root it prints the whole index either
    // way, because the index does not move with the worktree. From `src` the
    // prefix is what the setting changes, and that is where the port answers
    // about the wrong directory.
    at(out, Some("src"), "ls-files", &["ls-files"], "../src");

    // Which override wins. Both `GIT_WORK_TREE` and `--work-tree` beat
    // `core.worktree`, so the fixture root is the worktree again and
    // `--show-toplevel` prints `<REPO>` rather than `<REPO>/src` — the answer
    // that separates "the setting is honoured" from "the setting is honoured
    // *last*", which the two rows above cannot tell apart.
    //
    // `core.worktree = ..` on its own is deliberately not here: it resolves
    // against the git directory, so `<REPO>/.git/..` is `<REPO>` and the
    // setting is the identity — measured from the root and from `src/`,
    // byte-identical to carrying no setting at all, rather than reasoned about.
    // It earns a row only once an explicit `--git-dir` has moved what the
    // worktree would otherwise have been; see the last case in this function.
    out.push(
        Case::new("rev-parse", full, Shape::Linear)
            .with_env(&[("GIT_WORK_TREE", "{repo}")])
            .with_scoped_config(cfg("../src")),
    );
    out.push(
        Case::new("rev-parse", full, Shape::Linear)
            .with_globals(&[&["--work-tree=."]])
            .with_scoped_config(cfg("../src")),
    );

    // A worktree that is not there: the chdir refusal.
    out.push(
        Case::strict("rev-parse", full, Shape::Linear)
            .in_dir("src")
            .with_scoped_config(cfg("nosuch")),
    );
    out.push(Case::strict("status", &["status", "--porcelain"], Shape::Linear).with_scoped_config(cfg("nosuch")));

    // `core.worktree` with an explicit `--git-dir` in front of it: the option
    // does not cancel the setting, it only changes what the setting is resolved
    // against.
    //
    // The value here is `..` and **not** `../src`, and the difference is the
    // difference between a case and a decoration. With `../src` this row is
    // byte-identical to the same argv carrying no configuration at all —
    // measured, stock and port, all six queries — because `--git-dir=../.git`
    // from `src/` already makes `src/` the worktree, which is what `../src`
    // resolves to. A row that reads the same whether the setting is honoured or
    // ignored cannot fail against a port that ignores it, and that is exactly
    // the port this file is measuring. `..` resolves to the fixture root
    // instead, so the setting has somewhere else to point, and the row
    // separates the two implementations:
    //
    //     stock  <REPO>/.git  <REPO>/.git  <REPO>      true  src/  ../
    //     port   ../.git      <REPO>/.git  <REPO>/src  true  ""    ""
    //
    // `core.worktree = ..` *without* the option is the identity — `<REPO>/.git`
    // resolved against `..` is `<REPO>`, the worktree the walk would have found
    // — and is measured and absent for that reason.
    out.push(
        Case::new("rev-parse", full, Shape::Linear)
            .in_dir("src")
            .with_globals(&[&["--git-dir=../.git"]])
            .with_scoped_config(cfg("..")),
    );
}

// ---------------------------------------------------------------------------
// 5. core.bare
// ---------------------------------------------------------------------------

/// `core.bare = true` in a repository that has a working tree on disk.
///
/// The setting is read by `setup.c` while the repository is being opened, so it
/// is not "a boolean some command consults" — it deletes the worktree from
/// git's model of the repository, and every worktree-shaped answer becomes a
/// refusal. Measured on stock 2.55.0 at the fixture root:
/// `--is-bare-repository` prints `true`, `--git-dir` still prints `.git`, and
/// `--show-toplevel` is `fatal: this operation must be run in a work tree` with
/// exit 128. `log --oneline` and `ls-files` are unaffected — measured, both
/// print exactly what they print without the setting, because neither needs the
/// worktree to answer — and `status --porcelain` is **not**: it takes the same
/// refusal and the same exit 128, which is what makes it the verb that carries
/// the setting's whole observable effect. (An earlier draft of this comment said
/// `status` was unaffected. It is not.)
///
/// The port's defect here is the `--git-dir` spelling rather than the bareness:
/// `--is-bare-repository` and the refusal agree on every row, while the port
/// re-renders `--git-dir` absolutely on the two rows where the current directory
/// is the git directory's parent — `.git` against `<REPO>/.git` — and agrees
/// from `src/`, where stock is absolute too.
///
/// The command-line spelling is **not** here and its absence is measured rather
/// than assumed: `-c core.bare=true` changes nothing at all (stock prints
/// `false` for `--is-bare-repository`), because the parameter config is layered
/// on after the repository has already been opened as non-bare. A case
/// delivering it that way would be vacuous, and the pair of them would look like
/// coverage of one question when only the file spelling asks it.
///
/// That is also why the scope matters more here than the key does, and why this
/// group is not `config_reads.rs`'s: the *same key and value* is inert from
/// `-c` and load-bearing from `.git/config`.
fn core_bare(out: &mut Vec<Case>) {
    let cfg = || vec![ConfigEntry::set(ConfigScope::Repo, "core.bare", "true")];
    let case = |cmd: &'static str, args: &[&str], cwd: Option<&'static str>| {
        let mut c = Case::new(cmd, args, Shape::Linear).with_scoped_config(cfg());
        if let Some(cwd) = cwd {
            c = c.in_dir(cwd);
        }
        c
    };

    out.push(case("rev-parse", &["rev-parse", "--is-bare-repository", "--git-dir", "--absolute-git-dir"], None));
    out.push(
        Case::strict("rev-parse", WHERE, Shape::Linear).with_scoped_config(cfg()),
    );
    out.push(case("rev-parse", &["rev-parse", "--is-bare-repository", "--git-dir"], Some("src")));
    out.push(case("status", &["status", "--porcelain"], None));
    // `log --oneline` and `ls-files` are absent and were measured before being
    // dropped: neither consults the worktree, so both print exactly what they
    // print without the setting.

    // `core.bare=true` and an explicit worktree, which is the contradiction git
    // has to resolve: the option wins and the repository has a worktree again.
    out.push(
        Case::new("rev-parse", WHERE, Shape::Linear)
            .with_globals(&[&["--work-tree=."]])
            .with_scoped_config(cfg()),
    );
    // `core.bare=true` and an explicit `--git-dir`, which is the combination
    // that decides whether the setting is read out of the *named* directory's
    // config or out of whatever the walk would have found. Both read it — both
    // refuse `--show-toplevel` — and the port disagrees on `--git-common-dir`
    // alone (`.git` against `<REPO>/.git`), which is the same spelling defect
    // the option rows in [`git_dir_option`] find and not a second one.
    out.push(
        Case::strict("rev-parse", WHERE, Shape::Linear)
            .with_globals(&[&["--git-dir=.git"]])
            .with_scoped_config(cfg()),
    );
}

// ---------------------------------------------------------------------------
// 6. GIT_DIR naming a gitfile
// ---------------------------------------------------------------------------

/// `GIT_DIR` pointing at a **`.git` file** rather than a `.git` directory.
///
/// A submodule checkout and a linked worktree both keep a one-line
/// `gitdir: <path>` file where the directory would be, and `setup.c`'s
/// `read_gitfile_gently()` follows it. `submodule_family.rs` reaches the
/// indirection from *inside* the submodule (`GIT_DIR=.git` in `sub/`), which is
/// the benign direction: the cwd is already the submodule's worktree, so an
/// implementation that resolves the file and one that does not still agree
/// about which worktree they are in.
///
/// From the **superproject root** they do not, and this is the worst divergence
/// in this file. `GIT_DIR={repo}/sub/.git` there, stock 2.55.0 against the port:
///
/// ```text
///                        stock                        port
/// --git-dir              <REPO>/.git/modules/sub      <REPO>/sub/.git
/// --show-toplevel        <REPO>/sub                   <REPO>
/// --is-inside-work-tree  false                        true
/// status --porcelain     (clean)                       D mod.txt
///                                                     ?? .gitmodules
///                                                     ?? README.md
///                                                     ?? src/
///                                                     ?? sub/
/// ```
///
/// The port pairs the submodule's **index** with the superproject's **working
/// tree**. Every superproject file is untracked to it and the submodule's only
/// tracked path is deleted. `status` is read-only and merely reports it, so
/// `add -A` is here too, and it is the row that turns the diagnosis into a
/// consequence. Run under the same `GIT_DIR`, then asked of the submodule's own
/// git directory afterwards with stock:
///
/// ```text
/// git --git-dir=<REPO>/.git/modules/sub ls-files
///   after stock's add -A     mod.txt
///   after the port's add -A  .gitmodules
///                            README.md
///                            src/lib.rs
///                            sub
/// ```
///
/// The port wrote the superproject's tree into the submodule's index and
/// dropped the submodule's only tracked path, in a repository the invocation
/// never named. `ls-files` is the third row and the control: the *index* is the
/// submodule's on both sides before anything is written (`mod.txt`), so the
/// divergence above is the worktree half of the pairing and not a second wrong
/// git directory.
///
/// The state digest is what carries the `add -A` row — stdout is empty on both
/// sides and both exit 0 — through `probe_modules`, which renders each module
/// index as `entries=<n>` (1 against 4) and lists the module's objects, which
/// the port's write adds to.
///
/// The linked worktree is the same indirection with a different destination, and
/// it separates the two halves of the defect: there the port lands on the
/// *right* worktree (`--show-toplevel` agrees) and still gets the spelling wrong
/// — `--git-dir` prints the gitfile it was handed where stock prints the
/// directory the gitfile names, and `--git-common-dir` prints a bare `.git`
/// where stock prints `<REPO>/.git`. So the wrong-worktree half is specific to
/// the submodule and the wrong-spelling half is not, and both rows are here so
/// neither is read as the other.
///
/// The admin directory named *directly* (`GIT_DIR={repo}/.git/worktrees/wt`)
/// splits it once more: with no gitfile to follow, the port's `--git-dir`
/// agrees, and only `--git-common-dir` is still wrong (`.git` against
/// `<REPO>/.git`). The option spelling `--git-dir=.git/worktrees/wt` is a
/// fourth row saying the same thing from the command line. Together they place
/// the defect: the port resolves the indirection when it has to, and never
/// re-renders the common directory.
///
/// `symbolic-ref HEAD` through the gitfile is the row that says the port is
/// reading the *linked* worktree's HEAD and not the main one — `refs/heads/
/// linked` on both sides. Without it, every row above would be satisfied by a
/// port that silently fell back to `<REPO>/.git`.
///
/// `--resolve-git-dir` is the query that asks the indirection directly.
/// `submodule_family.rs:662` passes it once, on `.git` from inside `sub`. Five
/// rows here, none of them that one, and the two refusals are different
/// diagnostics rather than one:
///
/// ```text
/// sub/.git     <REPO>/.git/modules/sub            exit 0  (followed)
/// .git         .git                               exit 0  (a real directory, echoed)
/// wt/.git      <REPO>/.git/worktrees/wt           exit 0
/// nosuch       fatal: not a gitdir 'nosuch'       exit 128
/// README.md    fatal: invalid gitfile format: README.md   exit 128
/// ```
///
/// The last two are why both are [`Case::strict`]: a file that exists is *read*
/// as a gitfile and fails on its contents, while a path that does not exist
/// fails before anything is read, and only the message separates them. The port
/// gives both the `nosuch` diagnostic — `fatal: not a gitdir 'README.md'` —
/// so without stderr compared the two rows would score as agreement.
fn gitfile_as_git_dir(out: &mut Vec<Case>) {
    let sub_gitfile: &[(&str, &str)] = &[("GIT_DIR", "{repo}/sub/.git")];
    let rel_gitfile: &[(&str, &str)] = &[("GIT_DIR", "sub/.git")];

    for env in [sub_gitfile, rel_gitfile] {
        out.push(Case::new("rev-parse", WHERE, Shape::Submodule).with_env(env));
    }
    out.push(
        Case::new("status", &["status", "--porcelain"], Shape::Submodule).with_env(sub_gitfile),
    );
    out.push(Case::new("ls-files", &["ls-files"], Shape::Submodule).with_env(sub_gitfile));
    // The write. Read-only rows show which worktree the port paired the
    // submodule's git directory with; this one shows what happens when
    // something acts on that pairing. Both sides exit 0 with empty stdout, so
    // the whole case is the post-state.
    out.push(Case::new("add", &["add", "-A"], Shape::Submodule).with_env(sub_gitfile));
    out.push(
        Case::new("rev-parse", SPELLING, Shape::Submodule).in_dir("src").with_env(sub_gitfile),
    );

    // The linked worktree's gitfile, and the admin directory it points at,
    // named from the main worktree's root.
    let wt_gitfile: &[(&str, &str)] = &[("GIT_DIR", "{repo}/wt/.git")];
    let wt_admin: &[(&str, &str)] = &[("GIT_DIR", "{repo}/.git/worktrees/wt")];
    out.push(Case::new("rev-parse", WHERE, Shape::Worktree).with_env(wt_gitfile));
    out.push(Case::new("rev-parse", WHERE, Shape::Worktree).with_env(wt_admin));
    out.push(Case::new("symbolic-ref", &["symbolic-ref", "HEAD"], Shape::Worktree).with_env(wt_gitfile));
    out.push(
        Case::new(
            "rev-parse",
            &["rev-parse", "--git-dir", "--git-common-dir", "--show-toplevel"],
            Shape::Worktree,
        )
        .with_globals(&[&["--git-dir=.git/worktrees/wt"]]),
    );

    // `--resolve-git-dir`, which nothing in the corpus passes.
    for (shape, name) in [
        (Shape::Submodule, "sub/.git"),
        (Shape::Submodule, ".git"),
        (Shape::Worktree, "wt/.git"),
    ] {
        out.push(Case::new("rev-parse", &["rev-parse", "--resolve-git-dir", name], shape));
    }
    out.push(Case::strict("rev-parse", &["rev-parse", "--resolve-git-dir", "nosuch"], Shape::Submodule));
    out.push(Case::strict("rev-parse", &["rev-parse", "--resolve-git-dir", "README.md"], Shape::Submodule));
}

// ---------------------------------------------------------------------------
// 7. GIT_COMMON_DIR
// ---------------------------------------------------------------------------

/// `GIT_COMMON_DIR`: the half of the git directory that is shared between
/// worktrees, named directly.
///
/// `env_layer.rs` retired it as needing "a layout no template builds". Measured
/// again here, that is not what it needs: the variable's own value *is* the
/// answer `--git-common-dir` prints, so a plain repository is enough to see it,
/// and the port does not print it. Stock 2.55.0 at [`Shape::Linear`]'s root with
/// `GIT_COMMON_DIR={repo}/.git`:
///
/// ```text
/// --git-dir          .git             .git          (agree)
/// --git-common-dir   <REPO>/.git      .git          (stock, port)
/// ```
///
/// The port **ignores the variable**: on every row it prints the
/// `--git-common-dir` it would have printed with the variable unset. At the
/// root that is `.git`, which happens to equal the git directory; from `src/`
/// it is `../.git`, which does not. The three columns from `src/` are `../.git`
/// (stock, unset) / `<REPO>/.git` (stock, set) / `../.git` (port, set) — so the
/// variable moves stock's answer from both vantage points and the port's from
/// neither, and the "re-prints the git directory" reading that fits the root row
/// does not survive the `src/` one.
///
/// The last three rows are the **gate**, and it is a different fact: `setup.c`'s
/// `is_git_directory()` looks for `refs/` and `objects/` under the *common*
/// directory, so a `GIT_COMMON_DIR` that is not there stops `.git` from being a
/// repository at all. Stock refuses with
/// `fatal: not a git repository (or any of the parent directories): .git` and
/// exit 128; the port ignores the variable and reports the repository. A port
/// that opens a repository git has refused is the same class of defect as one
/// that opens the wrong one — under a write verb it writes where git would not
/// have.
fn common_dir(out: &mut Vec<Case>) {
    const HERE: &[(&str, &str)] = &[("GIT_COMMON_DIR", "{repo}/.git")];
    const NOWHERE: &[(&str, &str)] = &[("GIT_COMMON_DIR", "{repo}/.git/no-such")];

    let q: &[&str] = &["rev-parse", "--git-dir", "--git-common-dir", "--absolute-git-dir"];
    out.push(Case::new("rev-parse", q, Shape::Linear).with_env(HERE));
    out.push(Case::new("rev-parse", q, Shape::Linear).in_dir("src").with_env(HERE));
    // Not on [`Shape::Worktree`] from inside `wt`: measured, and there the
    // variable is vacuous. A linked worktree's common dir is already
    // `<REPO>/.git` and already rendered absolutely, so naming it changes no
    // byte of stock's answer. From that shape's *root* it does move the answer
    // (`.git` becomes `<REPO>/.git`) — but that is the identical fact the first
    // row above already pins on [`Shape::Linear`], so the row would be a second
    // copy of a question rather than a new one.
    out.push(
        Case::new(
            "rev-parse",
            &["rev-parse", "--git-dir", "--git-common-dir", "--git-path", "HEAD", "--git-path", "config"],
            Shape::Linear,
        )
        .with_env(&[("GIT_DIR", "{repo}/.git"), ("GIT_COMMON_DIR", "{repo}/.git")]),
    );

    // The gate.
    out.push(Case::strict("rev-parse", q, Shape::Linear).with_env(NOWHERE));
    out.push(Case::strict("log", &["log", "--oneline"], Shape::Linear).with_env(NOWHERE));
    out.push(Case::strict("status", &["status", "--porcelain"], Shape::Linear).with_env(NOWHERE));
}

// ---------------------------------------------------------------------------
// 8. GIT_OBJECT_DIRECTORY and GIT_INDEX_FILE, as routing
// ---------------------------------------------------------------------------

/// The two variables that move **one path** out of the git directory without
/// moving the git directory.
///
/// `object_pack.rs` owns what a *missing* `GIT_OBJECT_DIRECTORY` does (it breaks
/// discovery, and the diagnostic is `not a git repository`); `index_plumbing.rs`
/// and `reset_family.rs` own reading and writing a different index through
/// `GIT_INDEX_FILE`. What nothing owns is the routing question — where does git
/// say the path *is* — and `rev-parse --git-path` is the only query that answers
/// it. Measured on stock 2.55.0 at [`Shape::Linear`]'s root:
///
/// ```text
/// GIT_OBJECT_DIRECTORY={repo}/.git/objects
///   --git-path objects/pack        <REPO>/.git/objects/pack   (absolute: the variable's own value)
///   --git-path index               .git/index                 (relative: untouched)
///
/// GIT_OBJECT_DIRECTORY=.git/objects/pack
///   --git-path objects                     .git/objects/pack
///   --git-path objects/info/alternates     .git/objects/pack/info/alternates
///
/// GIT_INDEX_FILE=.git/other-index
///   --git-path index               .git/other-index
///   --git-path index.lock          .git/index.lock            (the lock is not the index)
/// ```
///
/// Both variables reroute exactly one branch of the routing table and leave the
/// rest alone, and each case above asks for a rerouted path and an untouched one
/// in the same invocation, so a port with one rule for the whole table cannot
/// pass either row by picking the rule that happens to suit it.
///
/// The second row's value used to be `.git/objects`, which is where the object
/// store already is, so both queries printed what they print with the variable
/// unset — measured, byte for byte — and the row could not fail against a port
/// that ignored the variable entirely. `.git/objects/pack` is a directory the
/// fixture already has (so `is_git_directory()` still accepts the repository)
/// and is somewhere else, so the row now has an answer of its own.
///
/// All four rows agree between stock 2.55.0 and the port.
fn object_dir_and_index(out: &mut Vec<Case>) {
    out.push(
        Case::new(
            "rev-parse",
            &[
                "rev-parse",
                "--git-path",
                "objects/pack",
                "--git-path",
                "objects/info/packs",
                "--git-path",
                "index",
            ],
            Shape::Linear,
        )
        .with_env(&[("GIT_OBJECT_DIRECTORY", "{repo}/.git/objects")]),
    );
    out.push(
        Case::new(
            "rev-parse",
            &["rev-parse", "--git-path", "objects", "--git-path", "objects/info/alternates"],
            Shape::Linear,
        )
        .with_env(&[("GIT_OBJECT_DIRECTORY", ".git/objects/pack")]),
    );
    out.push(
        Case::new(
            "rev-parse",
            &["rev-parse", "--git-path", "index", "--git-path", "index.lock", "--git-path", "HEAD"],
            Shape::Linear,
        )
        .with_env(&[("GIT_INDEX_FILE", ".git/other-index")]),
    );
    out.push(
        Case::new("rev-parse", &["rev-parse", "--git-path", "index"], Shape::Linear)
            .with_env(&[("GIT_INDEX_FILE", "{repo}/.git/index")]),
    );
}

/// `rev-parse --git-path` where the git directory has **not** been split.
///
/// `worktree_lifecycle.rs` owns the query on [`Shape::Worktree`] and
/// [`Shape::WorktreeLocked`], which is where the routing table has two
/// destinations. The three rows here are the layouts where it has one, and each
/// prints a *spelling* that appears nowhere in that table:
///
/// ```text
/// Linear, cwd = src/          ../.git/HEAD  ../.git/index  ../.git/config  ../.git/logs/HEAD
/// BehindRemote, cwd = .remote.git/    HEAD  index          config          objects
/// Submodule,   cwd = sub/     <REPO>/.git/modules/sub/HEAD  …/config  …/index
/// ```
///
/// Relative to the current directory in the first, bare names in the second
/// (the git directory *is* the current directory), absolute in the third
/// (resolved through the gitfile). A port that renders `--git-path` by joining
/// the git dir it printed for `--git-dir` gets one of the three; the port under
/// test gets all three, matching stock byte for byte on every row, so this
/// group is a pin on agreement rather than a finding. Worth having beside
/// [`git_dir_option`] for exactly that reason: the port's `--git-dir` spelling
/// is wrong in four places and its `--git-path` spelling is right in three, so
/// the two are not one defect.
fn git_path_without_a_split(out: &mut Vec<Case>) {
    out.push(
        Case::new(
            "rev-parse",
            &[
                "rev-parse",
                "--git-path",
                "HEAD",
                "--git-path",
                "index",
                "--git-path",
                "config",
                "--git-path",
                "logs/HEAD",
            ],
            Shape::Linear,
        )
        .in_dir("src"),
    );
    out.push(
        Case::new(
            "rev-parse",
            &[
                "rev-parse",
                "--git-path",
                "HEAD",
                "--git-path",
                "index",
                "--git-path",
                "config",
                "--git-path",
                "objects",
            ],
            Shape::BehindRemote,
        )
        .in_dir(".remote.git"),
    );
    out.push(
        Case::new(
            "rev-parse",
            &["rev-parse", "--git-path", "HEAD", "--git-path", "config", "--git-path", "index"],
            Shape::Submodule,
        )
        .in_dir("sub"),
    );
}

// ---------------------------------------------------------------------------
// 9. Ownership: the repository git refuses to open
// ---------------------------------------------------------------------------

/// `safe.directory` and the dubious-ownership refusal — the one gate that
/// answers "no repository here" about a repository that is present, healthy and
/// found.
///
/// No case in the corpus sets `safe.directory` or reaches
/// `ensure_valid_ownership()`, and the reason it was out of reach is real: the
/// check compares the git directory's `st_uid` against the euid, both fixture
/// copies are created by the process running the harness, and a case cannot
/// `chown`. The escape hatch is git's own —
/// `GIT_TEST_ASSUME_DIFFERENT_OWNER=1` makes `ensure_valid_ownership()` take the
/// refusing branch without any second user existing — and it is not a pin, so a
/// case may set it and both sides see it.
///
/// The refusal is [`Case::strict`] throughout, because the message *is* the
/// behaviour here: stdout is empty on both sides and an exit code alone would
/// not tell "refused for the documented reason" from "refused". Measured
/// byte-identical between stock 2.55.0 and the port, with the fixture root
/// masked as the runner masks it:
///
/// ```text
/// fatal: detected dubious ownership in repository at '<REPO>'
/// To add an exception for this directory, call:
///
/// 	git config --global --add safe.directory <REPO>
/// ```
///
/// Five exception forms follow, and the scope is half of what each one measures:
///
///  * `safe.directory = *` in the **global** file — accepted, and the only
///    exception a case can spell, because the value is written into a file
///    verbatim and the useful spelling is this side's own absolute path.
///  * The same value in **`.git/config`** — still refused. Git reads
///    `safe.directory` from the protected scopes only, on purpose: a repository
///    that could vouch for itself would make the check ornamental. A port that
///    reads the key from wherever it finds it accepts here, and this row is the
///    only thing in the corpus that would catch it.
///  * A global value that matches nothing (`nosuch`) — still refused, so the row
///    above is testing the *match* and not merely the presence of the key. It
///    also carries a second line all three binaries agree on:
///    `warning: safe.directory 'nosuch' not absolute`, printed before the
///    refusal, because a relative exception is rejected on its way in rather
///    than silently failing to match.
///  * `*` followed by an empty value — the documented reset: an empty
///    `safe.directory` clears everything listed before it, so the exception is
///    withdrawn and the repository is refused again. That is a last-value rule
///    with a special case in it, and `-c` cannot express two values of one key.
///  * `/*` in the global file — accepted. This is the *other* wildcard rule: a
///    value ending in `/*` matches the named directory and everything below it,
///    which is a different branch of `ensure_valid_ownership()` from the bare
///    `*` that matches unconditionally. It is the one form of it a case can
///    spell, because `/` is the one absolute directory prefix both sides share,
///    and a port that implements only the bare `*` refuses here. Both agree.
///
/// **One exception form is measured and cannot be a case**, and it is a live
/// divergence rather than a hypothetical: `safe.directory = %(prefix)/<abs
/// path>` — the value written with `%(prefix)/` in front and the absolute path
/// keeping its own leading slash, so the remainder git hands to `system_path()`
/// is itself absolute and comes back unchanged. Stock 2.55.0 accepts the
/// repository; the port refuses it, exit 128, with the dubious-ownership
/// message. It is unwritable here because a configuration value is a literal
/// that the runner never substitutes — `{repo}` works in an environment value
/// and nowhere else — and the two sides' copies live at different paths, so any
/// literal absolute path would name at most one of them.
///
/// The nearby spellings are measured too, and neither is a substitute:
/// `%(prefix)` with the path appended and no slash is refused by *both*
/// (`skip_prefix` does not fire, so the value is not a path at all), and
/// `%(prefix)/*` is refused by both because the remainder `*` is relative and
/// is therefore joined onto this build's runtime prefix. That second one would
/// be writable, and it is deliberately not a case: what it pins is the prefix
/// the *installed stock binary* was compiled with, which is a property of the
/// machine rather than of either implementation. Recorded rather than dropped:
/// the gap is in the harness, not in the port's favour.
fn ownership(out: &mut Vec<Case>) {
    const DUBIOUS: &[(&str, &str)] = &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")];
    let global = |value: &str| ConfigEntry::set(ConfigScope::Global, "safe.directory", value);

    // Refused, from four vantage points and on three verbs — the check runs
    // during setup, so the verb that follows never gets to disagree.
    //
    // The first four rows print the *same bytes*: the message names the
    // repository, not the current directory, so `cwd[src]` does not change it,
    // and `status` and `log` never get far enough to say anything of their own.
    // They are kept because each is a different entry point into
    // `setup_git_directory_gently()` and a port can fail the gate on one verb
    // and not another — the port's `--resolve-git-dir` and `GIT_DIR=README.md`
    // rows elsewhere in this file are exactly that shape of defect. The last
    // two vantage points are not redundant at all: a bare repository names its
    // git directory (`<REPO>/.remote.git`) and a submodule names its worktree
    // (`<REPO>/sub`), so those two messages differ from the first four and from
    // each other.
    out.push(Case::strict("rev-parse", &["rev-parse", "--git-dir"], Shape::Linear).with_env(DUBIOUS));
    out.push(
        Case::strict("rev-parse", &["rev-parse", "--show-toplevel"], Shape::Linear)
            .in_dir("src")
            .with_env(DUBIOUS),
    );
    out.push(Case::strict("status", &["status", "--porcelain"], Shape::Linear).with_env(DUBIOUS));
    out.push(Case::strict("log", &["log", "--oneline"], Shape::Linear).with_env(DUBIOUS));
    out.push(
        Case::strict("rev-parse", &["rev-parse", "--git-dir"], Shape::BehindRemote)
            .in_dir(".remote.git")
            .with_env(DUBIOUS),
    );
    out.push(
        Case::strict("rev-parse", &["rev-parse", "--git-dir"], Shape::Submodule)
            .in_dir("sub")
            .with_env(DUBIOUS),
    );

    // Accepted: the wildcard, from the scope git trusts.
    out.push(
        Case::strict("rev-parse", &["rev-parse", "--git-dir", "--show-toplevel"], Shape::Linear)
            .with_env(DUBIOUS)
            .with_scoped_config(vec![global("*")]),
    );
    out.push(
        Case::strict("status", &["status", "--porcelain"], Shape::Linear)
            .with_env(DUBIOUS)
            .with_scoped_config(vec![global("*")]),
    );
    out.push(
        Case::strict("rev-parse", &["rev-parse", "--show-prefix", "--git-dir"], Shape::Linear)
            .in_dir("src")
            .with_env(DUBIOUS)
            .with_scoped_config(vec![global("*")]),
    );

    // Refused anyway: the repository vouching for itself, a value that matches
    // nothing, and the empty-value reset after a wildcard.
    out.push(
        Case::strict("rev-parse", &["rev-parse", "--git-dir"], Shape::Linear)
            .with_env(DUBIOUS)
            .with_scoped_config(vec![ConfigEntry::set(ConfigScope::Repo, "safe.directory", "*")]),
    );
    out.push(
        Case::strict("rev-parse", &["rev-parse", "--git-dir"], Shape::Linear)
            .with_env(DUBIOUS)
            .with_scoped_config(vec![global("nosuch")]),
    );
    out.push(
        Case::strict("rev-parse", &["rev-parse", "--git-dir"], Shape::Linear)
            .with_env(DUBIOUS)
            .with_scoped_config(vec![global("*"), global("")]),
    );

    // Accepted again, through the trailing-`/*` rule rather than the bare `*`.
    out.push(
        Case::strict("rev-parse", &["rev-parse", "--git-dir"], Shape::Linear)
            .with_env(DUBIOUS)
            .with_scoped_config(vec![global("/*")]),
    );
}

// ---------------------------------------------------------------------------
// 10. GIT_NAMESPACE
// ---------------------------------------------------------------------------

/// `GIT_NAMESPACE`: where the refs a *served* repository shows are read from.
///
/// # What it does on the port, in full
///
/// The corpus sets it on `ls-remote` (`env_layer.rs`, twice) and passes
/// `--namespace=` on `ls-remote` (`globals_layer.rs`) and on
/// `for-each-ref`/`rev-parse --git-dir`/`show-ref` (`corpus.rs`'s globals
/// block). Re-measured here across `for-each-ref`, `show-ref`,
/// `branch --list`, `rev-parse main`, `symbolic-ref HEAD`, `log --oneline -1`,
/// `rev-parse --git-path refs/heads/main` and `update-ref` — all eight, on
/// [`Shape::Branched`], stock against the port — and the finding is:
/// **nothing local honours it**, on either binary, including the write.
/// `GIT_NAMESPACE=ns update-ref refs/heads/nsb HEAD` leaves
/// `.git/refs/heads/nsb` on disk on both sides, not
/// `refs/namespaces/ns/refs/heads/nsb`. None of those eight is a case: a
/// variable that changes no byte is a case that can never fail.
///
/// Two verbs honour it without a network, and neither had ever been asked:
/// `upload-pack --advertise-refs` and `receive-pack --advertise-refs` build
/// their advertisement out of `refs/namespaces/<n>/`, which no fixture has. So
/// under any namespace the advertisement is *empty*, and empty is a specific
/// pkt-line stream rather than nothing:
///
/// ```text
/// upload-pack,  no namespace   0112<oid> HEAD …symref=HEAD:refs/heads/main… + 5 ref lines
/// upload-pack,  GIT_NAMESPACE=ns   0101<null oid> capabilities^{} …           (no ref lines)
/// receive-pack, GIT_NAMESPACE=ns   00ae<oid> .have … + 2 more .have lines
/// ```
///
/// The two verbs answer *differently* to the same variable, and that is the
/// point of pinning both: `upload-pack` has nothing left to say, while
/// `receive-pack` still advertises the objects the repository holds as `.have`
/// lines, because those are not refs and are not namespaced. The port collapses
/// the second onto the first — it drops the `.have` lines and advertises
/// `capabilities^{}` alone — so a namespaced push against it would be told the
/// server has no objects and would send the whole history. That is the whole
/// `GIT_NAMESPACE` divergence: the port matches stock on `upload-pack` under
/// every value, and matches it on nothing under `receive-pack`.
///
/// # The value, and the refusal
///
/// `refs.c:expand_namespace` splits the value on `/` and wraps every component,
/// so `a/b` becomes `refs/namespaces/a/refs/namespaces/b/`, then runs the result
/// through `check_refname_format` and dies if it fails. **The value is
/// therefore validated, and the refusal is the one place a namespace can change
/// an exit code without any namespaced ref existing.** Measured:
///
/// ```text
/// GIT_NAMESPACE=..        stock  fatal: bad git namespace path ".."           exit 128
///                         port   zvcs: upload-pack: bad git namespace path ".."  exit 1
/// GIT_NAMESPACE=x.lock    the same pair, with the value echoed
/// ```
///
/// Two different rules of `check_refname_format` (`..` is the traversal rule,
/// `x.lock` the suffix rule), one message, and a port that reaches the same
/// verdict by a different exit code and a different prefix. Asked on
/// `upload-pack` for both values and on `receive-pack` for one, because the
/// validation is in the shared expansion rather than in either verb.
///
/// A bad namespace is inert everywhere else — measured on `rev-parse`,
/// `status`, `for-each-ref`, `show-ref`, `log` and `update-ref`, all six exit 0
/// with the same output they give unset, on both binaries — which is why no
/// local verb carries the value.
///
/// # What the values cannot separate
///
/// `ns`, `a/b` and `refs/heads` produce **byte-identical** streams on both verbs
/// (checked on stock by digest, all three, both verbs) because none of them has
/// any ref underneath. An earlier draft asked all three; the two extra values
/// were four cases that could not distinguish any implementation from any other,
/// and they are gone. What remains that *is* separating: the empty value, which
/// is not a namespace at all and brings the full advertisement back, so a port
/// that treated any value as "advertise nothing" fails; the option spelling,
/// which reaches the same code through `git.c:handle_options`; the bare
/// repository serving itself, which is the layout a namespace is for; and the
/// two refusals.
///
/// `fetch_clone.rs` owns `upload-pack`/`receive-pack --advertise-refs` on
/// [`Shape::BehindRemote`] and [`Shape::Packed`] under five `uploadpack.*`
/// settings and none under a namespace, so no argv here is a second copy of one
/// there.
///
/// # Reading the second oracle on these rows
///
/// Every failing `receive-pack` row here is classified **`gits-disagree`**
/// rather than `corroborated-defect`, and the classification is an artifact.
/// The advertisement's capability list ends in `agent=git/<version>-Darwin`, so
/// git 2.55.0 and git 2.50.1 never produce the same bytes for any advertisement
/// and the runner's "did the two gits agree" test cannot come back yes. On the
/// substance they agree exactly — both print the same three `.have` lines with
/// the same object ids — and the port prints `capabilities^{}` and no `.have`
/// line to either. The verdict to read is the stdout diff, not the adjudication.
fn namespace_serving(out: &mut Vec<Case>) {
    {
        let env: &[(&str, &str)] = &[("GIT_NAMESPACE", "ns")];
        out.push(
            Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::Branched)
                .with_env(env),
        );
        out.push(
            Case::new("receive-pack", &["receive-pack", "--advertise-refs", "."], Shape::Branched)
                .with_env(env),
        );
    }
    // The values `check_refname_format` rejects, which is the only way a
    // namespace moves an exit code in a repository that has no namespaced refs.
    // [`Case::strict`] throughout: stdout is empty on both sides, so the
    // diagnostic is the entire content of the case.
    for value in ["..", "x.lock"] {
        out.push(
            Case::strict("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::Branched)
                .with_env(&[("GIT_NAMESPACE", value)]),
        );
    }
    out.push(
        Case::strict("receive-pack", &["receive-pack", "--advertise-refs", "."], Shape::Branched)
            .with_env(&[("GIT_NAMESPACE", "..")]),
    );
    // The empty value, which is *not* a namespace: git treats it as unset and
    // the full advertisement comes back. Without this row the three above would
    // pass equally well against a port that treats any value as "advertise
    // nothing".
    out.push(
        Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::Branched)
            .with_env(&[("GIT_NAMESPACE", "")]),
    );
    out.push(
        Case::new("receive-pack", &["receive-pack", "--advertise-refs", "."], Shape::Branched)
            .with_env(&[("GIT_NAMESPACE", "")]),
    );
    // The option spelling, which reaches the same place through
    // `git.c:handle_options` rather than through the environment.
    out.push(
        Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::Branched)
            .with_globals(&[&["--namespace=ns"]]),
    );
    out.push(
        Case::new("receive-pack", &["receive-pack", "--advertise-refs", "."], Shape::Branched)
            .with_globals(&[&["--namespace=ns"]]),
    );
    // A bare repository serving itself, which is the layout a namespace is for.
    out.push(
        Case::new(
            "upload-pack",
            &["upload-pack", "--advertise-refs", "./.remote.git"],
            Shape::BehindRemote,
        )
        .with_env(&[("GIT_NAMESPACE", "ns")]),
    );
    out.push(
        Case::new(
            "receive-pack",
            &["receive-pack", "--advertise-refs", "./.remote.git"],
            Shape::BehindRemote,
        )
        .with_env(&[("GIT_NAMESPACE", "ns")]),
    );
}

// ---------------------------------------------------------------------------
// 11. The pathspec-magic switches, from the environment
// ---------------------------------------------------------------------------

/// The pathspec-magic switches: the *off* spellings and the *conflict*
/// refusals, which is all `pathspec_stdin.rs` leaves.
///
/// That module owns this territory and owns it well: the four environment twins
/// at `=1`, `GIT_LITERAL_PATHSPECS=0`, explicit `:(glob)` magic against a global
/// default in both directions, the `--glob-pathspecs --literal-pathspecs`
/// refusal, and the same globals through `log`, `grep` and `diff-tree`.
/// `globals_layer.rs` owns the option spellings on `ls-files`/`grep`. Three
/// things neither has, and two of the three are live divergences:
///
///  * **`0` is off for every one of them, not just for `literal`** — and the
///    port only implements it for some. Measured on `Shape::AwkwardPaths` with
///    `ls-files -- *.txt`, whose unset answer is all four `.txt` paths:
///
///    ```text
///    GIT_NOGLOB_PATHSPECS = 0 | false | ""   stock: all four   port: none
///    GIT_GLOB_PATHSPECS   = 0 | false | ""   stock: all four   port: all four
///    GIT_LITERAL_PATHSPECS =    false | ""   stock: all four   port: all four
///    ```
///
///    Three rows diverge and six agree, and it is the same rule underneath —
///    the port tests `GIT_NOGLOB_PATHSPECS` for presence and reads the other
///    three as booleans. `pathspec_stdin.rs` pins the boolean rule on
///    `GIT_LITERAL_PATHSPECS`, which is one of the ones the port gets right, so
///    the rule was pinned on a variable that does not break it.
///  * **The boolean rule on a pathspec `icase` can see.** `*.txt` cannot see it
///    — measured, `GIT_ICASE_PATHSPECS` matches all four `.txt` paths at every
///    value including `1`, so an `icase` row over that pathspec is vacuous
///    whatever the port does. `WITH SPACE.TXT` is the pathspec whose case is
///    wrong, so `1` matches `with space.txt` and `0`/`false`/`""` match nothing.
///    All four rows agree; they are here because without them the group would
///    pin the boolean rule on three variables and leave the fourth untested.
///  * **The other two conflicts.** `literal` is exclusive with *all three*
///    others and `glob` is exclusive with `noglob`; only `glob` + `literal` is
///    pinned (`pathspec_stdin.rs:558`, the option spelling). Stock refuses
///    `literal` + `icase` with
///    `fatal: global 'literal' pathspec setting is incompatible with all other
///    global pathspec settings` and `glob` + `noglob` with
///    `fatal: global 'glob' and 'noglob' pathspec settings are incompatible`,
///    both exit 128 and both taken before the pathspec is parsed. **The port has
///    no conflict check at all**: it exits 0 and runs the command, which for
///    `glob` + `noglob` means it lists all four paths as though neither switch
///    were set. All five rows here diverge.
///
/// The environment spelling of the already-pinned conflict is here as well,
/// because the two doors are different code (`git.c:handle_options` against
/// `pathspec.c`'s environment pass) and the port misses both.
fn pathspec_magic_env(out: &mut Vec<Case>) {
    let ls = |out: &mut Vec<Case>, env: &[(&str, &str)]| {
        out.push(
            Case::new("ls-files", &["ls-files", "--", "*.txt"], Shape::AwkwardPaths).with_env(env),
        );
    };

    // Off, in the three spellings of off. `GIT_LITERAL_PATHSPECS=0` is
    // `pathspec_stdin.rs`'s and is not repeated.
    ls(out, &[("GIT_LITERAL_PATHSPECS", "false")]);
    ls(out, &[("GIT_LITERAL_PATHSPECS", "")]);
    for key in ["GIT_GLOB_PATHSPECS", "GIT_NOGLOB_PATHSPECS"] {
        for value in ["0", "false", ""] {
            ls(out, &[(key, value)]);
        }
    }

    // `icase` gets its own pathspec, because `*.txt` cannot see it: measured on
    // stock, `GIT_ICASE_PATHSPECS` matches all four `.txt` paths at every value
    // including `1`, so an `icase` row over that pathspec is vacuous whatever
    // the port does. `WITH SPACE.TXT` is the one pathspec whose case is wrong,
    // so `1` matches `with space.txt` and `0`/`false`/`` match nothing — which
    // is where the boolean rule is visible for this variable too.
    for value in ["1", "0", "false", ""] {
        out.push(
            Case::new("ls-files", &["ls-files", "WITH SPACE.TXT"], Shape::AwkwardPaths)
                .with_env(&[("GIT_ICASE_PATHSPECS", value)]),
        );
    }

    // The refusals: two that nothing asks for, and the environment door to the
    // one that something does.
    out.push(
        Case::strict("ls-files", &["ls-files", "--", "*.txt"], Shape::AwkwardPaths)
            .with_env(&[("GIT_LITERAL_PATHSPECS", "1"), ("GIT_ICASE_PATHSPECS", "1")]),
    );
    out.push(
        Case::strict("ls-files", &["ls-files", "--", "*.txt"], Shape::AwkwardPaths)
            .with_env(&[("GIT_LITERAL_PATHSPECS", "1"), ("GIT_GLOB_PATHSPECS", "1")]),
    );
    out.push(
        Case::strict("ls-files", &["ls-files", "--", "*.txt"], Shape::AwkwardPaths)
            .with_env(&[("GIT_GLOB_PATHSPECS", "1"), ("GIT_NOGLOB_PATHSPECS", "1")]),
    );
    out.push(
        Case::strict("ls-files", &["ls-files", "--", "*.txt"], Shape::AwkwardPaths)
            .with_globals(&[&["--literal-pathspecs"], &["--icase-pathspecs"]]),
    );
    out.push(
        Case::strict("ls-files", &["ls-files", "--", "*.txt"], Shape::AwkwardPaths)
            .with_globals(&[&["--glob-pathspecs"], &["--noglob-pathspecs"]]),
    );
}

// ---------------------------------------------------------------------------
// 12. A GIT_DIR that is not a repository
// ---------------------------------------------------------------------------

/// `GIT_DIR` and `--git-dir` naming something that is not a git directory.
///
/// Discovery is skipped entirely once either is set, so there is no walk to fall
/// back on and the answer is one of three refusals — which one depends on *what*
/// is at the path, not on whether anything is:
///
/// ```text
/// GIT_DIR=nosuch      fatal: not a git repository: 'nosuch'
/// GIT_DIR=src         fatal: not a git repository: 'src'          (a directory, no refs/objects)
/// GIT_DIR=README.md   fatal: invalid gitfile format: README.md    (a file, so it is read as a gitfile)
/// ```
///
/// All three are exit 128 and all three are [`Case::strict`], because the whole
/// content of the case is which diagnostic the path produced: stdout is empty in
/// every one of them, so an exit code alone would collapse the three into one
/// question. `discovery.rs` has no row where an explicit git directory is
/// *invalid* — its three `GIT_DIR` spellings all name the real repository.
///
/// The port collapses all three into one on `rev-parse`, and into a fourth
/// message that names no path:
/// `fatal: not a git repository (or any of the parent directories): .git`,
/// exit 128, for `nosuch`, `src` and `README.md`, environment and option
/// spelling alike — six rows, one answer. It is not a `rev-parse` quirk in one
/// direction only: under the same `GIT_DIR=nosuch`, the port's `status` prints
/// stock's message exactly, and under `GIT_DIR=README.md` its `log` prints
/// `fatal: not a git repository: 'README.md'` — the *`nosuch`* diagnostic
/// applied to a file. So the value's kind is lost on three different paths
/// through the port, in three different ways, and the last two rows are what
/// says the defect is not confined to one verb.
fn bad_git_dir(out: &mut Vec<Case>) {
    for value in ["nosuch", "src", "README.md"] {
        out.push(
            Case::strict("rev-parse", &["rev-parse", "--git-dir", "--show-toplevel"], Shape::Linear)
                .with_env(&[("GIT_DIR", value)]),
        );
        out.push(
            Case::strict("rev-parse", &["rev-parse", "--git-dir", "--show-toplevel"], Shape::Linear)
                .with_globals(&[&[&format!("--git-dir={value}")]]),
        );
    }
    out.push(
        Case::strict("status", &["status", "--porcelain"], Shape::Linear)
            .with_env(&[("GIT_DIR", "nosuch")]),
    );
    out.push(
        Case::strict("log", &["log", "--oneline"], Shape::Linear)
            .with_env(&[("GIT_DIR", "README.md")]),
    );
}

// ---------------------------------------------------------------------------
// 13. GIT_CEILING_DIRECTORIES as a list
// ---------------------------------------------------------------------------

/// The `GIT_CEILING_DIRECTORIES` spellings that are more than one path.
///
/// `discovery.rs` owns the three situations that decide whether a ceiling
/// applies at all (`discovery.rs:370-372`) — a ceiling that is a proper ancestor
/// of the starting directory, one that *is* the starting directory, and one that
/// matches nothing — each spelled across several queries. What it does not
/// have is the fact that the variable is a **`PATH`-style list**, and the list
/// has two rules that a single-value implementation gets wrong:
///
///  * Any element may match; the others are not consulted once one has. So
///    `{repo}/no-such-dir:{repo}` stops the walk exactly as `{repo}` alone does.
///  * A **relative** element changes nothing at all — with
///    `GIT_CEILING_DIRECTORIES=relative-not-absolute` the walk runs to
///    completion and finds the repository — while an **empty** element does not
///    disqualify the elements after it: `:{repo}` still stops the walk, exactly
///    as `{repo}` alone does. Two kinds of unusable element, two different
///    outcomes, and a list parser that drops empties along with relatives gets
///    the second one wrong.
///
/// One more row pins the interaction the two variables have: `GIT_DIR` is
/// resolved before the walk begins, so a ceiling that would have stopped the
/// walk is irrelevant and the repository is found anyway.
///
/// All four agree between stock 2.55.0 and the port — the port parses the list,
/// keeps empty elements and drops relative ones, exactly as git does. Kept as
/// pins: the two `Case::strict` rows are refusals, and a refusal that starts
/// agreeing for the wrong reason is invisible without the message.
fn ceiling_lists(out: &mut Vec<Case>) {
    let from_src = |strict: bool, value: &'static str| {
        let args: &[&str] = &["rev-parse", "--git-dir", "--show-toplevel"];
        let case = if strict {
            Case::strict("rev-parse", args, Shape::Linear)
        } else {
            Case::new("rev-parse", args, Shape::Linear)
        };
        case.in_dir("src").with_env(&[("GIT_CEILING_DIRECTORIES", value)])
    };
    out.push(from_src(true, "{repo}/no-such-dir:{repo}"));
    out.push(from_src(true, ":{repo}"));
    out.push(from_src(false, "relative-not-absolute"));
    out.push(
        Case::new("rev-parse", &["rev-parse", "--git-dir", "--show-toplevel"], Shape::Linear)
            .in_dir("src")
            .with_env(&[("GIT_CEILING_DIRECTORIES", "{repo}"), ("GIT_DIR", "{repo}/.git")]),
    );
}
