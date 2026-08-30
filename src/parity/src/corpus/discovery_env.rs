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
//! # How territory divides with the eight adjacent modules
//!
//! * **`discovery.rs`** — the walk, as above. Its `GIT_DIR` cases use the three
//!   spellings that name *the repository the walk would have found anyway*
//!   (`.`, `.git`, `{repo}/.git`); this file's name a **different** repository
//!   from the one under the cwd, which is where the port stops agreeing. Its
//!   ceiling cases are the three that decide whether a ceiling applies at all;
//!   this file adds only the *list* forms (an empty element, two elements, a
//!   relative element) it does not have.
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
//!   `--work-tree` case at all; those are this file's, together with the two
//!   pathspec-magic *conflicts* (`literal` with anything, `glob` with `noglob`)
//!   and the environment spelling of the same four switches.
//! * **`worktree_lifecycle.rs`** — owns `rev-parse --git-path` on the layout
//!   where the routing splits: [`Shape::Worktree`] from three vantage points and
//!   [`Shape::WorktreeLocked`] from inside the locked tree. This file asks the
//!   same query where there is **no** split — a plain repository from a
//!   subdirectory, a bare repository at its root, a submodule checkout — and
//!   under the variables that reroute one path without moving the git directory
//!   (`GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`). No argv and no shape is shared.
//! * **`worktree_index.rs`** — `update-index`, `checkout-index`,
//!   `sparse-checkout`, `clean` and the `worktree` verb on shapes with no linked
//!   worktree. Nothing about where the repository is.
//! * **`config_reads.rs`** — a setting changes what some *other* verb prints,
//!   90-odd `color.*`/`diff.*`/`status.*`/`log.*` keys. It sets no `core.*` key
//!   that moves the repository.
//! * **`config_resolution.rs`** — how a configuration *value* is resolved: the
//!   include graph, the scope stack, the type conversions, the file grammar.
//!   The two files meet only at `safe.directory`, and the split is clean: that
//!   module would own how the *value* is parsed, this one owns what the value
//!   does to whether a repository opens. It sets no `core.worktree`/`core.bare`.
//! * **`submodule_deep.rs` / `submodule_family.rs`** — the submodule as a
//!   *topology*. `submodule_family` owns `GIT_DIR={repo}/.git/modules/sub` (five
//!   readouts) and `GIT_DIR=.git` from inside `sub` (`--git-dir` only);
//!   `submodule_deep` owns `--show-superproject-working-tree` from three
//!   positions and one `GIT_DIR`+`GIT_WORK_TREE` pair naming the submodule.
//!   Neither names the **gitfile** as `GIT_DIR` *from the superproject root*,
//!   which is the combination in [`gitfile_as_git_dir`] where the port operates
//!   on the wrong worktree.
//!
//! # What is new here, in one list
//!
//! No case in the corpus before this file used: `--git-dir` or `--work-tree` as
//! a global option; `core.worktree` or `core.bare` in any scope;
//! `GIT_COMMON_DIR`; `GIT_TEST_ASSUME_DIFFERENT_OWNER`; `safe.directory`;
//! `--path-format` in either value; `GIT_NAMESPACE` on `upload-pack` or
//! `receive-pack`; `GIT_DIR` naming a gitfile by absolute path;
//! `--resolve-git-dir`; or a `GIT_CEILING_DIRECTORIES` value with more than one
//! element.
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
//!    bytes. So `--absolute-git-dir`, `--show-toplevel` and every absolute
//!    `--git-path` answer are comparable, and they are the readouts this file
//!    leans on hardest.
//!  * A case's environment may not contain a literal absolute path
//!    (`runner::apply_case_env` asserts it); `{repo}` is substituted per side.
//!    Every value below that names a path is written that way.
//!  * Every stock answer below was measured **twice, at two roots of different
//!    length**, in a fresh copy each time, and required to be byte-identical
//!    before the case was written. That is what would catch a padding or
//!    truncation that depends on how long the path happens to be.
//!
//! **A case's *configuration* value is not substituted** — it is a literal
//! written into a file — so a setting whose value must be this side's own
//! absolute path cannot be expressed. That rules out exactly one measurement,
//! and it is a live divergence rather than a hypothetical: see [`ownership`].
//! `core.worktree` escapes the rule only because git resolves a relative
//! `core.worktree` against the git directory, so `../src` names the same place
//! `{repo}/src` would.
//!
//! # Two things that cannot be measured at all, and are absent rather than faked
//!
//!  * **`GIT_DISCOVERY_ACROSS_FILESYSTEM`.** It changes the walk only where the
//!    walk crosses a mount point, and a case is one argv against a directory
//!    tree the runner copied — nothing in the harness can create a filesystem
//!    boundary. Measured anyway, both values, from a subdirectory: identical to
//!    not setting it. A case would be vacuous, so there is none.
//!  * **A repository that is genuinely owned by another user.** Both copies are
//!    created by the process running the harness, so `st_uid` is the same on
//!    both sides and equal to the euid. Git's own escape hatch closes the gap
//!    without needing a second user — `GIT_TEST_ASSUME_DIFFERENT_OWNER=1` makes
//!    `ensure_valid_ownership()` take the refusing branch — and [`ownership`]
//!    uses it. What stays out of reach is the ownership *check itself*: these
//!    cases measure what git does once it has decided the repository is
//!    dubious, never how it decides.

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

    // The two layouts where `--git-dir` and `--git-common-dir` disagree, asked
    // in absolute form so the answers are two different directories rather than
    // one directory spelled two ways.
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

/// `--git-dir` and `--work-tree` on the command line, which no case in the
/// corpus has ever passed.
///
/// `globals_layer.rs` covers the option layer and skips both, so the whole
/// command-line half of repository selection was unmeasured — the environment
/// half (`GIT_DIR`, `GIT_WORK_TREE`) was reachable and the option half was not,
/// even though `git.c:handle_options` resolves the option *after* `-C` has
/// already moved the current directory and the two therefore do not commute.
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
/// the wrong repository.
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

/// `--work-tree` / `GIT_WORK_TREE` **without** a `--git-dir` beside it: the
/// combination the documentation warns about and nothing measured.
///
/// Declaring a worktree does not stop the walk — git still finds `.git` by
/// climbing — but it does replace the worktree the walk would have paired it
/// with, and that changes three answers at once. Measured on stock 2.55.0 at the
/// fixture root with `GIT_WORK_TREE=src`:
///
/// ```text
/// --git-dir              .git            (still relative: cwd is not the worktree root,
///                                         but it *is* the git dir's parent)
/// --show-toplevel        <REPO>/src
/// --is-inside-work-tree  false           (the cwd is above the declared worktree)
/// --show-cdup            <REPO>/src      (absolute — the climb cannot be spelled with ../)
/// ```
///
/// Two rows carry the same environment on `status` and `ls-files`, because
/// `rev-parse` reporting the right worktree and the command *using* it are
/// different claims: with the worktree moved to `src`, every tracked path is
/// missing from it and the real `src/lib.rs` is an untracked `lib.rs`.
///
/// The last pair is the one that separates "declared" from "existing": a
/// worktree that is **not there** is not an error to git. `--show-toplevel`
/// prints the path that does not exist and exits 0.
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
    // not move with the worktree, so it prints the same two paths whatever the
    // worktree is. Measured against stock before it was dropped.

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
/// The same setting at the fixture *root* is honoured by both. So the defect is
/// not "the key is ignored" but "the key is dropped once discovery had to climb"
/// — which is why the pair of rows is the case and neither one alone is.
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
    // `core.worktree = ..` is deliberately not here: it resolves against the
    // git directory, so `<REPO>/.git/..` is `<REPO>` and the setting is the
    // identity. Measured against stock before it was dropped rather than
    // reasoned about — a vacuous case looks exactly like a passing one.
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
    out.push(
        Case::new("rev-parse", full, Shape::Linear)
            .in_dir("src")
            .with_globals(&[&["--git-dir=../.git"]])
            .with_scoped_config(cfg("../src")),
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
/// exit 128 — while `log` and `status` are unaffected, because neither needs the
/// worktree to answer.
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
    // `log` and `ls-files` are absent and were measured before being dropped:
    // neither consults the worktree, so both print exactly what they print
    // without the setting. The refusal is the whole observable effect, and
    // `status` is the verb that carries it.

    // `core.bare=true` and an explicit worktree, which is the contradiction git
    // has to resolve: the option wins and the repository has a worktree again.
    out.push(
        Case::new("rev-parse", WHERE, Shape::Linear)
            .with_globals(&[&["--work-tree=."]])
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
/// tracked path is deleted. `status` is read-only and merely reports it, but the
/// same pairing under `add -A` stages the superproject into the submodule's
/// index, and under `clean -fd` deletes the paths listed as `??` — in a
/// repository the user did not name. That is why `status --porcelain` is a case
/// here and not just the `rev-parse` row: the porcelain is what makes the
/// consequence legible rather than the diagnosis.
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
/// `--resolve-git-dir` is the query that asks the indirection directly, and no
/// case in the corpus passes it. Five rows, and the two refusals are different
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
/// fails before anything is read, and only the message separates them.
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
/// The port answers `--git-common-dir` by re-printing the git directory, which
/// is right in every layout where nothing has separated the two and wrong the
/// moment something has. From `src/` the same pair reads `../.git` (stock,
/// unset) / `<REPO>/.git` (stock, set) / `../.git` (port, set) — so the
/// variable moves stock's answer from both vantage points, and the port's from
/// neither.
///
/// The second row is the **gate**, and it is a different fact: `setup.c`'s
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
    // Not on [`Shape::Worktree`]: measured, and there the variable is vacuous.
    // A linked worktree's common dir is already `<REPO>/.git` and already
    // rendered absolutely, so naming it changes no byte of stock's answer — the
    // rows above are the two places where it does.
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
/// GIT_INDEX_FILE=.git/other-index
///   --git-path index               .git/other-index
///   --git-path index.lock          .git/index.lock            (the lock is not the index)
/// ```
///
/// Both variables reroute exactly one branch of the routing table and leave the
/// rest alone, and each case above asks for a rerouted path and an untouched one
/// in the same invocation, so a port with one rule for the whole table cannot
/// pass either row by picking the rule that happens to suit it.
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
        .with_env(&[("GIT_OBJECT_DIRECTORY", ".git/objects")]),
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
/// the git dir it printed for `--git-dir` gets one of the three.
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
/// Four exception forms follow, and the scope is half of what each one measures:
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
///
/// **One exception form is measured and cannot be a case**, and it is a live
/// divergence rather than a hypothetical: `safe.directory = %(prefix)/<abs
/// path>`. `%(prefix)` expands to the empty string on a build without a runtime
/// prefix, so the value is the absolute path and stock 2.55.0 (and 2.50.1)
/// accept the repository; the port refuses it, exit 128, with the dubious-
/// ownership message. It is unwritable here because a configuration value is a
/// literal that the runner never substitutes — `{repo}` works in an environment
/// value and nowhere else — and the two sides' copies live at different paths,
/// so any literal absolute path would name at most one of them. Recorded rather
/// than dropped: the gap is in the harness, not in the port's favour.
fn ownership(out: &mut Vec<Case>) {
    const DUBIOUS: &[(&str, &str)] = &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")];
    let global = |value: &str| ConfigEntry::set(ConfigScope::Global, "safe.directory", value);

    // Refused, from four vantage points and on three verbs — the check runs
    // during setup, so the verb that follows never gets to disagree.
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
}

// ---------------------------------------------------------------------------
// 10. GIT_NAMESPACE
// ---------------------------------------------------------------------------

/// `GIT_NAMESPACE`: where the refs a *served* repository shows are read from.
///
/// The corpus sets it twice, on `ls-remote` (`env_layer.rs`, and
/// `globals_layer.rs` for the `--namespace=` spelling), and both of those files
/// record that the local listings ignore it. Re-measured here across
/// `for-each-ref`, `show-ref`, `branch --list`, `rev-parse <ref>`,
/// `symbolic-ref HEAD`, `log`, `rev-parse --git-path refs/heads/main` and
/// `update-ref` — all eight, on [`Shape::Branched`] — and the finding holds:
/// **nothing local honours it**, including the write. `GIT_NAMESPACE=ns
/// update-ref refs/heads/nsb HEAD` writes `refs/heads/nsb`, not
/// `refs/namespaces/ns/refs/heads/nsb`, on stock 2.55.0, on 2.50.1 and on the
/// port. Those are not cases: a variable that changes no byte is a case that can
/// never fail.
///
/// Two verbs do honour it without a network, and neither has ever been asked:
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
/// server has no objects and would send the whole history.
///
/// `fetch_clone.rs` owns `upload-pack`/`receive-pack --advertise-refs` on
/// [`Shape::BehindRemote`] and [`Shape::Packed`] under five `uploadpack.*`
/// settings and none under a namespace, so no argv here is a second copy of one
/// there.
fn namespace_serving(out: &mut Vec<Case>) {
    for value in ["ns", "a/b", "refs/heads"] {
        let env: &[(&str, &str)] = &[("GIT_NAMESPACE", value)];
        out.push(
            Case::new("upload-pack", &["upload-pack", "--advertise-refs", "."], Shape::Branched)
                .with_env(env),
        );
        out.push(
            Case::new("receive-pack", &["receive-pack", "--advertise-refs", "."], Shape::Branched)
                .with_env(env),
        );
    }
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
/// things neither has, and each is a live divergence:
///
///  * **`0` is off for every one of them, not just for `literal`.** Stock 2.55.0
///    under `GIT_NOGLOB_PATHSPECS=0` lists all four `*.txt` paths, exactly as it
///    does with the variable unset; the port lists none, because it tests the
///    variable for presence. `pathspec_stdin.rs` pins the boolean rule on
///    `GIT_LITERAL_PATHSPECS`, where the port happens to agree — so the rule is
///    pinned on the one variable that does not break it.
///  * **`false` and the empty string are off too**, which a port that special-
///    cases the literal string `0` still gets wrong.
///  * **The other two conflicts.** `literal` is exclusive with *all three*
///    others and `glob` is exclusive with `noglob`; only `glob` + `literal` is
///    pinned. Stock refuses `literal` + `icase` with the same message and
///    `glob` + `noglob` with `fatal: global 'glob' and 'noglob' pathspec
///    settings are incompatible`, both exit 128 and both taken before the
///    pathspec is parsed. The port has no conflict check at all and runs the
///    command, so it fails all three; only one of them is currently asked.
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
/// `discovery.rs` owns the three rows that decide whether a ceiling applies at
/// all — a ceiling that is a proper ancestor of the starting directory, one that
/// *is* the starting directory, and one that matches nothing. What it does not
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
