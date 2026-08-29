//! `git init`, `git init-db`, and the on-disk format every later command
//! inherits from them.
//!
//! Before this module the corpus asked `init` one question — `linear::init::init`
//! (`corpus.rs`) — plus eight `init-db` invocations in
//! [`crate::corpus::misc_commands`]. That is the whole measurement of the verb
//! that decides a repository's hash algorithm, its ref storage, whether it is
//! bare, which branch `HEAD` names before a single commit exists, and what lands
//! in `config`, `info/exclude`, `description` and `hooks/`. Every one of those
//! choices is permanent for the life of the repository, and `builtin/init-db.c`
//! reaches them through two different code paths — a fresh `init_db()` and a
//! *reinit* over a repository that already exists — which agree on almost
//! nothing: a reinit prints a different sentence, refuses `--object-format` and
//! `--ref-format` when they name anything but the format the repository already
//! has, silently ignores `--initial-branch` with a warning, and still honours
//! `--shared` and `--separate-git-dir`.
//!
//! # What this harness can and cannot see when `init` runs
//!
//! `init` is the one verb in the corpus whose *product* is usually a repository
//! that is not the fixture, and [`crate::runner::probe_state`] is written around
//! the fixture. Three facts were established by reading the probes and by
//! running them against repositories stock git had just created, and they decide
//! which axis is measured where. They are recorded here because a case whose
//! result nothing compares is worse than no case at all — it inflates the
//! denominator.
//!
//!  * **A repository created *beside* the fixture is probed, but not its
//!    config.** `probe_peer`/`other_peers` walk the fixture for any directory
//!    that is a git directory (or holds a `.git` that is), so `init sub`,
//!    `init --bare sub.git` and `init --separate-git-dir=gd sub` are all found.
//!    `peer_section` then compares that repository's **`HEAD` file byte for
//!    byte**, its `for-each-ref`, its object census, its reflogs and stock's
//!    `fsck --strict` — and *not* its `config`. So `--initial-branch` is fully
//!    measured on a nested target and `core.bare` / `extensions.*` /
//!    `core.sharedRepository` are not.
//!  * **A repository created *at* the fixture root is probed completely**,
//!    because `probe_state`'s `config --list --local` runs there. Every
//!    configuration axis is therefore exercised as a **reinit**, which is not a
//!    weaker question than a fresh init: it is the second of the two code paths,
//!    and it is the one a port is more likely to get wrong.
//!  * **`init --bare` at the fixture root writes the whole template payload into
//!    the *worktree*.** Stock 2.55.0 does not refuse it and does not reinitialize
//!    the existing `.git`; it prints `Initialized empty Git repository in
//!    <REPO>/` and lays `HEAD`, `config`, `description`, `info/exclude`,
//!    `hooks/*.sample`, `objects/{info,pack}` and `refs/{heads,tags}` down beside
//!    `README.md`. `collect_worktree` reads every one of those as ordinary
//!    worktree content, with file bytes. That is the only invocation in this
//!    crate through which the hook samples, `description` and `info/exclude` are
//!    compared at all, which is why [`bare_layout_at_root`] exists.
//!
//! # `--object-format=sha256` and `--ref-format=reftable`
//!
//! Both were checked against the probes before any case relied on them, because
//! a repository the harness cannot probe measures nothing.
//!
//!  * **sha256 is fully probeable.** Stock's `for-each-ref`,
//!    `cat-file --batch-check --batch-all-objects` and
//!    `fsck --strict --no-progress --no-dangling` all exit 0 against a freshly
//!    `init --object-format=sha256`'d repository, and its `HEAD` is the ordinary
//!    `ref: refs/heads/master`. Nothing is lost.
//!  * **reftable is probeable only in one of its three placements.** The same
//!    three probes exit 0 against a reftable repository and its `HEAD` holds the
//!    stub `ref: refs/heads/.invalid`, so nothing about the format itself is
//!    unreadable. What is unreproducible is the table's *name*:
//!    `reftable/0x000000000001-0x000000000001-<8 hex>.ref`, whose hex field
//!    changes run to run — two runs of the same stock binary produced `b91e298d`
//!    and `6cbadb3e` — with `tables.list` naming whichever file was written. So:
//!    - `init --ref-format=reftable sub` — **used.** The tables land in
//!      `sub/.git/reftable/`, no probe reaches them, and the run is clean.
//!    - `init --bare --ref-format=reftable sub.git` — **removed after being
//!      measured.** The harness classified it NONDETERMINISTIC, so some probe
//!      does reach a *bare* peer's reftable directory.
//!    - `init --bare --ref-format=reftable` at the fixture root — never written.
//!      The tables land in the worktree, where `collect_worktree` reads every
//!      byte.
//!
//!    So the flag is measured on the nested non-bare form, and at the root where
//!    stock *refuses* it outright.
//!
//! Both formats are also on trial as things the port may simply not have. That is
//! a legitimate finding and it is reported rather than avoided, but it is not
//! allowed to be the *only* thing a case reports, which is why the format axis is
//! crossed with `-b` and with `--bare` rather than run alone.
//!
//! # `--shared` permissions
//!
//! `--shared` does two things: it writes `core.sharedRepository` (and
//! `receive.denyNonFastForwards`) and it `chmod`s the git directory. Only the
//! first is comparable here, and the reason is **not** umask skew — `env::harden`
//! never touches the umask and both binaries are spawned by the same runner
//! process, so both sides create files under one identical mask by construction.
//! It is that **no probe records a mode**. `collect_worktree` reduces a file's
//! mode to the single character `x`/`-`
//! and renders a directory as `<dir>` with no mode at all; `hook_value` does the
//! same for the one hook set it reads. `--shared=group` moves group *read/write*
//! bits and never the execute bit of a file, so the whole permission half of the
//! flag is invisible to this harness and would stay invisible however many cases
//! were added.
//!
//! What *is* measured, and measured completely, is the configuration half plus
//! the word stock puts in its own report: `Reinitialized existing **shared** Git
//! repository`. Every spelling `git_config_perm` accepts — `shared_callback` in
//! `builtin/init-db.c` passes the option's argument straight to it — is crossed
//! against it in [`shared_spellings`], because the spellings do not
//! collapse — measured on stock 2.55.0, `group`/`true`/`1`/bare `--shared` write
//! `1`, `all`/`world`/`everybody` write `2`, `0660` writes `0660`, and
//! `false`/`umask`/`0` write nothing at all and drop the "shared" word.
//!
//! # Fixture constraints obeyed by every case here
//!
//! A case is one argv against a pristine copy, and it may only write inside that
//! copy. So every path operand is repo-relative and every one of them is a name
//! that either does not exist in [`Shape::Linear`] (`sub`, `sub.git`, `gd`,
//! `newgit`, `alt-objs`, `empty`, `a`, `b`) or exists as the fixture built it
//! (`src`, `README.md`, `.git`); `..` and absolute paths appear nowhere, and the
//! environment values
//! that need the fixture root spell it `{repo}`
//! ([`crate::runner::REPO_PLACEHOLDER`]). Directories that must already exist
//! before the command runs are created by `Case::in_dir`, which is what makes
//! "init into an existing empty directory" expressible at all —
//! `runner::case_dir` `mkdir -p`s the case's working directory on both sides
//! identically.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this module's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    operands(out);
    reinit_at_root(out);
    bare_layout_at_root(out);
    bare_targets(out);
    separate_git_dir(out);
    initial_branch(out);
    formats(out);
    shared_spellings(out);
    templates(out);
    environment(out);
    init_db_alias(out);
}

/// Shorthand: one `init` case on [`Shape::Linear`].
fn init(args: &[&str]) -> Case {
    Case::new("init", args, Shape::Linear)
}

/// Shorthand: one `init` case whose refusal is the contract, so stderr counts.
fn init_strict(args: &[&str]) -> Case {
    Case::strict("init", args, Shape::Linear)
}

/// The path operand, in every form the fixture can express — and the working
/// directory, which is the operand's default.
///
/// `builtin/init-db.c:cmd_init_db` treats the operand as a directory to `mkdir`
/// and `chdir` into before anything else happens, so the operand is not one
/// question but four: does the directory not exist yet, does it exist and is it
/// empty, does it exist and hold files, and does the *name* exist as something
/// that is not a directory. Stock answers all four differently — the first three
/// succeed with the same sentence and the fourth is
/// `fatal: cannot mkdir README.md: File exists` with exit 128 — and a port that
/// implements the operand as "create the directory" gets the last one wrong in a
/// way nothing else here would notice.
///
/// The existing-directory rows are reached through `Case::in_dir`, because a
/// case cannot create a directory before its own command runs and `case_dir`
/// creates the working directory on both sides identically. `in_dir("empty")`
/// therefore *is* "an existing empty directory"; `in_dir("src")` is the
/// non-empty one, since the fixture put `src/lib.rs` there.
///
/// `init .git` is not here: the fixture's `.git` is a directory, so the
/// invocation would ask the same question `in_dir(".git") init` already asks,
/// from the other side.
fn operands(out: &mut Vec<Case>) {
    out.push(init(&["init", "sub"]));
    out.push(init(&["init", "./sub"]));
    // Two levels at once: the whole prefix has to be created, not just the leaf.
    out.push(init(&["init", "sub/deeper"]));
    // `--` before the operand. The parser has no options after it, so the only
    // way this differs from the plain form is if the port forwards `--` on.
    out.push(init(&["init", "--", "sub"]));
    // A directory that exists and is not empty.
    out.push(init(&["init", "src"]));
    // An existing *empty* directory, created by the runner, entered rather than
    // named — and the same directory named as `.`, which is a different token
    // through the parser and must reach the same place.
    out.push(init(&["init"]).in_dir("empty"));
    out.push(init(&["init", "."]).in_dir("empty"));
    // From a subdirectory of a repository: the new repository is the
    // subdirectory, not the one discovery would have found.
    out.push(init(&["init"]).in_dir("src"));
    out.push(init(&["init", "sub"]).in_dir("src"));
    // From inside the git directory. Stock creates `.git/.git` and reports it,
    // rather than recognising that it is standing in a repository.
    out.push(init(&["init"]).in_dir(".git"));
    // The operand names an existing regular file.
    out.push(init_strict(&["init", "README.md"]));
    // Two operands: `init` takes at most one, and the refusal is the usage text
    // with exit 129 rather than a `fatal:` with 128.
    out.push(init_strict(&["init", "a", "b"]));
}

/// Reinit: `init` over a repository that already exists.
///
/// This is where the configuration axes live, because `probe_state`'s
/// `config --list --local` runs at the fixture root and nowhere else (see the
/// module header). It is also a genuinely separate branch of
/// `builtin/init-db.c`: `init_db()` sees `reinit` true, prints `Reinitialized
/// existing …` instead of `Initialized empty …`, and `create_default_files()`
/// skips the parts that would overwrite what is there.
///
/// The three rows that are not about a flag are about *which repository* the
/// reinit lands on, and each one is a layout no other row reaches:
///
///  * [`Shape::Detached`] — `HEAD` is a raw object id rather than a symref, and
///    a reinit must leave it exactly so.
///  * [`Shape::Worktree`] `wt` — from inside a linked worktree, stock
///    reinitializes `<REPO>/.git/worktrees/wt/`, not the common directory. That
///    path appears in the report, so an implementation that resolves the common
///    directory prints the wrong one.
///  * [`Shape::BehindRemote`] `.remote.git` — a *bare* repository. Plain `init`
///    there does not reinit it: stock treats the cwd as a worktree and creates
///    `.remote.git/.git`, while `init --bare` there reinitializes the bare
///    repository in place. Two adjacent invocations, opposite outcomes.
///
/// `init --bare` run from inside the fixture's own `.git` is the one reinit that
/// *changes* `core.bare` — measured on stock 2.55.0, `.git/config` comes back
/// with `bare = true` while `logallrefupdates` and `checkstat` survive — so it
/// is the strongest single test that the reinit path writes config through
/// git's own updater rather than by rewriting the file.
fn reinit_at_root(out: &mut Vec<Case>) {
    // `init` alone on Linear is already in `corpus.rs`; these are the spellings
    // that are not.
    out.push(init(&["init", "."]));
    out.push(init(&["init", "-q"]));
    out.push(init(&["init", "--quiet"]));
    // Stock warns `re-init: ignored --initial-branch=topic` and leaves `HEAD` on
    // `main` with no `topic` ref created. A port that honours the flag on reinit
    // moves the branch a repository is checked out on.
    out.push(init(&["init", "-b", "topic"]));
    out.push(init(&["init", "--initial-branch=topic"]));
    out.push(init(&["init", "-b", "topic", "."]));
    out.push(Case::new("init", &["init", "-b", "other"], Shape::Branched));
    // Redundant-but-agreeing format flags: the reinit checks them against what
    // the repository already is and must accept a match.
    out.push(init(&["init", "--object-format=sha1"]));
    out.push(init(&["init", "--ref-format=files"]));
    // The template is consulted on reinit too, for the files that are missing.
    out.push(init(&["init", "--template=src"]));
    // The reinit that flips `core.bare`.
    out.push(init(&["init", "--bare"]).in_dir(".git"));

    out.push(Case::new("init", &["init"], Shape::Detached));
    out.push(Case::new("init", &["init"], Shape::Dirty));
    out.push(Case::new("init", &["init", "-q"], Shape::Branched));
    out.push(Case::new("init", &["init"], Shape::Worktree).in_dir("wt"));
    out.push(Case::new("init", &["init"], Shape::BehindRemote).in_dir(".remote.git"));
    out.push(Case::new("init", &["init", "--bare"], Shape::BehindRemote).in_dir(".remote.git"));
}

/// `--bare` at the fixture root: the only place the template payload is
/// compared.
///
/// Stock 2.55.0 does not refuse `init --bare` inside an existing non-bare
/// worktree and does not treat it as a reinit of the `.git` beside it. It
/// creates a *second*, bare repository whose git directory is the worktree root,
/// prints `Initialized empty Git repository in <REPO>/`, and leaves `.git`
/// untouched — so `HEAD`, `config`, `description`, `info/exclude`, all fourteen
/// `hooks/*.sample` files and the empty `objects/`+`refs/` tree land where
/// `collect_worktree` reads them with their bytes.
///
/// Everything else in this crate compares a hook sample by *length* and only
/// inside `.git/modules` (`runner.rs:hook_value`), and nothing at all compares
/// `description` or `info/exclude`. These rows are that comparison, and they are
/// crossed with the flags whose whole effect is what gets written into that
/// payload: `--shared` (which adds two config keys), `--object-format=sha256`
/// (which raises `core.repositoryformatversion` to 1 and adds
/// `extensions.objectformat`), `--template=src` (which points at a directory
/// holding no `hooks/`, no `info/` and no `description`, so eighteen of the
/// twenty-six paths stock otherwise writes are absent and `lib.rs` is there
/// instead), and `-b` (which is honoured here, unlike on a reinit, because this
/// is a fresh `init_db()`).
///
/// `--ref-format=reftable` is absent by construction — see the module header.
///
/// One property of these rows is permanent and is recorded rather than left to
/// be rediscovered: the payload is version-dependent, so the harness's second
/// oracle reports `gits-disagree` on every one of them. `hooks/commit-msg.sample`
/// is not the same file in git 2.55.0 and in git 2.50.1 — 2.55.0 checks the
/// message for an embedded diff and honours `core.comment{char,string}`, 2.50.1
/// does neither, and the file is 74 lines there against 24 here — while `description`,
/// `info/exclude`, `config` and the other thirteen samples are byte-identical
/// between the releases. That is one file's worth of skew in a payload of
/// twenty-six paths, and it does not make the rows unmeasurable: the port's own
/// difference is every sample, the description, `info/exclude`, and the mode
/// bit, none of which either git disagrees with itself about.
fn bare_layout_at_root(out: &mut Vec<Case>) {
    out.push(init(&["init", "--bare"]));
    out.push(init(&["init", "--bare", "."]));
    out.push(init(&["init", "-q", "--bare"]));
    out.push(init(&["init", "--bare", "-b", "topic"]));
    out.push(init(&["init", "--bare", "--initial-branch=trunk"]));
    out.push(init(&["init", "--bare", "--shared=group"]));
    out.push(init(&["init", "--bare", "--shared=0640"]));
    out.push(init(&["init", "--bare", "--object-format=sha256"]));
    out.push(init(&["init", "--bare", "--ref-format=files"]));
    out.push(init(&["init", "--bare", "--template=src"]));
    // A template directory that does not exist: stock warns on stderr,
    // `templates not found in <REPO>/no-such-dir`, exits 0, and writes **no**
    // payload at all — `HEAD`, `config`, `objects/` and `refs/` and nothing
    // else. The flag replaces the template rather than adding to it, so a
    // missing one is an empty one.
    out.push(init(&["init", "--bare", "--template=no-such-dir"]));
}

/// `--bare` with a directory operand: the bare repository as a *peer*.
///
/// The complement of [`bare_layout_at_root`]. A bare repository named as an
/// operand is what `other_peers` was written to find, so `HEAD`, the ref
/// listing, the object census and stock's `fsck` are all compared against it —
/// and its config is not. So these rows measure the half the root rows cannot
/// (that the thing created is a *repository* stock can read) and skip the half
/// the root rows already have.
///
/// `--ref-format=reftable` is **not** here, and the reason is measured rather
/// than assumed. It was written, run, and removed: the harness classified
/// `init --bare --ref-format=reftable sub.git` NONDETERMINISTIC — stock git did
/// not reproduce its own post-state — and a case excluded from the denominator
/// measures nothing. Two runs of stock 2.55.0 over identical fixtures differ in
/// exactly one place, `sub.git/reftable/0x000000000001-0x000000000001-<8 hex>.ref`
/// and the `tables.list` naming it (`89bff900` against `b82333dd`), so some probe
/// reaches a bare peer's reftable directory where it does not reach a non-bare
/// one's. The non-bare form, `init --ref-format=reftable sub` in [`formats`],
/// runs clean and is where the flag is measured.
fn bare_targets(out: &mut Vec<Case>) {
    out.push(init(&["init", "--bare", "sub.git"]));
    out.push(init(&["init", "--bare", "-q", "sub.git"]));
    out.push(init(&["init", "--bare", "-b", "topic", "sub.git"]));
    out.push(init(&["init", "--bare", "--shared", "sub.git"]));
    out.push(init(&["init", "--bare", "--shared=group", "sub.git"]));
    out.push(init(&["init", "--bare", "--object-format=sha256", "sub.git"]));
    out.push(init(&["init", "--bare", "--ref-format=files", "sub.git"]));
}

/// `--separate-git-dir`: the git directory somewhere other than `<worktree>/.git`.
///
/// The layout is the one `clone --separate-git-dir` already produces in
/// [`crate::corpus::fetch_clone`] and [`crate::corpus::transport_local`], but
/// `init` reaches it through `set_git_dir` in `setup.c` rather than through
/// clone's own copy, and the two spellings the option parser accepts
/// (`--separate-git-dir=gd` and `--separate-git-dir gd`) go down the same path
/// only if the parser is right about the option taking a value.
///
/// It is worth more here than the flag usually is, because it puts the git
/// directory in the *worktree* namespace on both sides of the question:
///
///  * With an operand, `gd/` is a peer the probes read and `sub/.git` becomes a
///    **file** whose bytes — `gitdir: <REPO>/gd` — `collect_worktree` prints.
///    That absolute path is masked to `<REPO>` by `normalize`, so the pointer
///    itself is compared rather than the machine it names.
///  * Without one, it *moves* the fixture's own git directory: `.git` becomes
///    that file and `gd/` holds the real repository, `core.checkStat` and all.
///
/// Three refusals, and all three land in the handful of lines `cmd_init_db` runs
/// straight after `parse_options` — before it has created or entered the operand
/// directory, which is what makes the last one an *ordering* fact rather than a
/// message:
///
///  * `--bare` together with `--separate-git-dir`, rejected beside the option
///    table itself: `options '--separate-git-dir' and '--bare' cannot be used
///    together`, exit 128.
///  * an empty value: a relative `--separate-git-dir` goes through
///    `real_pathdup(real_git_dir, 1)` on the next line, which dies
///    `The empty string is not a valid path`, exit 128.
///  * a value under a directory that does not exist *yet*: the same
///    `real_pathdup` dies `Invalid path '<REPO>/sub': No such file or
///    directory`, exit 128, because `sub` is not created until much later in the
///    function. A port that creates the operand first succeeds where stock
///    fails, and nothing but this row would say so.
fn separate_git_dir(out: &mut Vec<Case>) {
    out.push(init(&["init", "--separate-git-dir=gd", "sub"]));
    out.push(init(&["init", "--separate-git-dir", "gd", "sub"]));
    out.push(init(&["init", "-q", "--separate-git-dir=gd", "sub"]));
    out.push(init(&["init", "--separate-git-dir=gd"]));
    out.push(init(&["init", "--separate-git-dir", "gd"]));
    out.push(init(&["init", "--separate-git-dir=gd", "-b", "topic", "sub"]));
    out.push(init_strict(&["init", "--bare", "--separate-git-dir=gd", "sub"]));
    out.push(init_strict(&["init", "--separate-git-dir=", "sub"]));
    out.push(init_strict(&["init", "--separate-git-dir=sub/gd", "sub"]));
}

/// `-b` / `--initial-branch`: the name `HEAD` points at before any commit.
///
/// Fully measured on a nested target, because `peer_section` reads the new
/// repository's `HEAD` **file** rather than asking git to resolve it — an unborn
/// branch has no ref to enumerate, so `HEAD`'s bytes are the only evidence the
/// name was honoured.
///
/// What is validated is not the name but `refs/heads/<name>`:
/// `builtin/init-db.c` builds that string and calls
/// `check_refname_format(ref, 0)` on it, with no `REFNAME_ALLOW_ONELEVEL`. The
/// accepted names are chosen so that distinction is visible rather than assumed:
///
///  * `topic` — ordinary, and the floor the rest are read against.
///  * `feature/x` — a slash, so the resulting `HEAD` has four components.
///  * `refs/heads/x` — a full refname, which git does **not** strip: stock writes
///    `ref: refs/heads/refs/heads/x`.
///  * `HEAD` — a branch name that collides with the file it is written into.
///  * `@` — accepted, and the row that catches a port validating the bare name.
///    `check_refname_format` rejects a refname that *is* the single character
///    `@`, and `refs/heads/@` is not that refname, so prefixing first is the
///    difference between accepting a name git takes and refusing it.
///
/// The rejected names cover the four rules that matter, each verified against
/// stock 2.55.0 as `fatal: invalid initial branch name: '<name>'` with exit 128:
/// a leading dot, a `..` run, an embedded space, and a trailing slash. The empty
/// string is the fifth and reaches the same message with nothing between the
/// quotes.
fn initial_branch(out: &mut Vec<Case>) {
    for name in ["topic", "main", "feature/x", "refs/heads/x", "HEAD", "@"] {
        out.push(init(&["init", "-b", name, "sub"]));
    }
    out.push(init(&["init", "--initial-branch=topic", "sub"]));
    out.push(init(&["init", "--initial-branch=feature/x", "sub"]));
    for bad in [".bad", "a..b", "has space", "x/"] {
        out.push(init_strict(&["init", "-b", bad, "sub"]));
    }
    out.push(init_strict(&["init", "--initial-branch=", "sub"]));
}

/// `--object-format` and `--ref-format`: the two `extensions.*` a repository can
/// never change afterwards.
///
/// Both flags have three answers and the corpus had none of them for `init`:
/// the format the repository would have had anyway (accepted, and must write no
/// extension), the other real format (accepted on a fresh init, and must write
/// `core.repositoryformatversion = 1` plus the extension), and a name that is
/// neither (refused — but not before the operand directory exists).
///
/// That last clause is a measured ordering fact and it is what the two `bogus`
/// rows are for. `cmd_init_db` creates and enters the operand directory before
/// it validates the format, so stock exits 128 with
/// `fatal: unknown hash algorithm 'bogus'` and still leaves an empty `sub/`
/// behind; the state probe sees it as `sub -: <dir>`. An implementation that
/// validates first refuses just as loudly and leaves nothing, and the only thing
/// that separates the two is a probe that reads the worktree. `--separate-git-dir`
/// is the opposite order — its path check runs first and leaves nothing — so the
/// two families are not interchangeable.
///
/// The reinit rows are the pair that makes the flags mean something. Stock
/// refuses to reinitialize a repository *into* a different format — `attempt to
/// reinitialize repository with different hash` and `… with different reference
/// storage format`, both exit 128 — and that refusal is the only thing standing
/// between a user and a repository whose config claims a hash its objects are
/// not in. A port that accepts the flag and does nothing reports success for a
/// conversion that did not happen.
///
/// The formats are crossed with `-b` and with each other rather than run alone,
/// so a port that lacks a format still has to get the argument parsing, the
/// operand and the branch name right on the same line.
fn formats(out: &mut Vec<Case>) {
    out.push(init(&["init", "--object-format=sha1", "sub"]));
    out.push(init(&["init", "--object-format=sha256", "sub"]));
    out.push(init(&["init", "-b", "topic", "--object-format=sha256", "sub"]));
    out.push(init(&["init", "--ref-format=files", "sub"]));
    out.push(init(&["init", "--ref-format=reftable", "sub"]));
    out.push(init(&["init", "-b", "topic", "--ref-format=reftable", "sub"]));
    out.push(init(&["init", "--object-format=sha256", "--ref-format=reftable", "sub"]));
    out.push(init_strict(&["init", "--object-format=bogus", "sub"]));
    out.push(init_strict(&["init", "--ref-format=bogus", "sub"]));
    // Reinit into a different format: both refusals, at the fixture root.
    out.push(init_strict(&["init", "--object-format=sha256"]));
    out.push(init_strict(&["init", "--ref-format=reftable"]));
}

/// `--shared`, in every spelling `git_parse_shared` accepts.
///
/// Run at the fixture root, because that is the only place
/// `config --list --local` reads what was written (module header). Measured on
/// stock 2.55.0, the twelve accepted spellings collapse onto four outcomes, and
/// the collapse is the reason all twelve are here rather than one per outcome:
///
/// | spelling                          | `core.sharedRepository` | report says |
/// |-----------------------------------|-------------------------|-------------|
/// | `--shared`, `=true`, `=group`, `=1` | `1`                   | `shared`    |
/// | `=all`, `=world`, `=everybody`    | `2`                     | `shared`    |
/// | `=0660`, `=0640`                  | the octal, verbatim     | `shared`    |
/// | `=false`, `=umask`, `=0`          | *not written*           | not shared  |
///
/// `receive.denyNonFastForwards = true` rides along with every spelling that
/// writes the key and with none that does not, so a port that writes the mode
/// and forgets the second key is caught by the same probe.
///
/// The permission half of the flag is not measured, and cannot be — see the
/// module header for why that is a property of the probes and not of the umask.
///
/// `--shared=bogus` is the refusal, and the message is the tell: `bad boolean
/// config value 'bogus' for 'arg'`, exit 128. `arg` is not the option's name —
/// it is the literal key `shared_callback` passes to `git_config_perm`, so the
/// value is parsed by the *config* parser rather than by anything that knows it
/// came from a command line. A hand-written validator produces a different
/// sentence even when it refuses the same input.
fn shared_spellings(out: &mut Vec<Case>) {
    out.push(init(&["init", "--shared"]));
    for spelling in
        ["false", "true", "umask", "group", "all", "world", "everybody", "0660", "0640", "1", "0"]
    {
        out.push(init(&["init", &format!("--shared={spelling}")]));
    }
    out.push(init(&["init", "-q", "--shared=group"]));
    // With an operand: the config is out of the probe's reach, but the report
    // still has to carry the word `shared`, and the peer still has to be a
    // repository stock can read.
    out.push(init(&["init", "--shared=group", "sub"]));
    out.push(init_strict(&["init", "--shared=bogus"]));
    // The same value on the *fresh* path, because the two are different
    // questions: a reinit may reject the argument and then do nothing, and a
    // fresh init has to reject it before it creates `sub`. Stock gives the
    // identical answer to both (exit 128, nothing created), which is what makes
    // one of them redundant only if the other passes.
    out.push(init_strict(&["init", "--shared=bogus", "sub"]));
}

/// `--template`: where the payload comes from.
///
/// The flag *replaces* the template directory rather than adding to it, which is
/// the fact all four rows turn on: whatever the value names is the only source
/// the payload comes from, and if it is not a directory the payload is **empty**.
/// Verified on stock 2.55.0 by counting what landed under the new `.git`:
///
///  * `src` — exists, holds `lib.rs`, and has no `hooks/`, `info/` or
///    `description`. The new repository gets `lib.rs` and nothing else. The same
///    directory [`crate::corpus::fetch_clone`] uses for `clone --template`, and
///    the only one available, since a case cannot create a template directory
///    first.
///  * `no-such-dir` — `warning: templates not found in <REPO>/no-such-dir` on
///    stderr, exit 0, and **zero** hook samples, no `description`, no
///    `info/exclude`. There is no fallback to the compiled-in default.
///  * `README.md` — a file rather than a directory. Same warning, same empty
///    payload.
///  * the empty value — no warning at all, and the same empty payload.
///
/// A port that resolves the template path relative to something other than the
/// current directory writes the wrong payload or none, and no other case in the
/// corpus would see it.
fn templates(out: &mut Vec<Case>) {
    out.push(init(&["init", "--template=src", "sub"]));
    out.push(init(&["init", "--template=no-such-dir", "sub"]));
    out.push(init(&["init", "--template=README.md", "sub"]));
    out.push(init(&["init", "--template=", "sub"]));
    out.push(init(&["init", "--template=src", "--bare", "sub.git"]));
}

/// `init` under the four environment variables that can move it.
///
/// `init` is the one verb for which `GIT_DIR` is not a discovery question but a
/// *destination*: `cmd_init_db` reads `GIT_DIR` and `GIT_WORK_TREE` with
/// `getenv` itself and hands the first to `init_db()` as the directory to
/// create, so `GIT_DIR=newgit git init` at the fixture root does not
/// reinitialize `.git` at all — it creates a new repository at `<REPO>/newgit`
/// with `core.bare = true`. The `bare` is not an accident: with no `--bare` on
/// the command line `cmd_init_db` asks `guess_repository_type(git_dir)`, which
/// answers 0 only for `.` , for the cwd, for `.git` and for `*/.git`, and
/// "often bare … at this point we are just guessing" for everything else —
/// `newgit` is everything else.
/// Adding `GIT_WORK_TREE=.` changes that outcome rather than adding to it: stock
/// then writes `core.bare = false` and a `core.worktree` pointing back at the
/// fixture root. `GIT_DIR=newgit git init sub` moves the whole thing down one
/// level, to `<REPO>/sub/newgit`.
///
/// `GIT_WORK_TREE` *without* `GIT_DIR` is the refusal, and `cmd_init_db` makes
/// it deliberately early — its own comment says "Catch the error early":
/// `GIT_WORK_TREE (or
/// --work-tree=<directory>) not allowed without specifying GIT_DIR (or
/// --git-dir=<directory>)`, exit 128.
///
/// `GIT_OBJECT_DIRECTORY` is the one that produces a repository git itself will
/// not recognise. Stock honours it, so `init sub` under it creates
/// `sub/.git/{HEAD,config,refs,…}` with **no `objects/` directory at all** and
/// puts `objects/info` and `objects/pack` at `sub/alt-objs/` instead — in the
/// worktree, where `collect_worktree` reads them, and outside `looks_like_git_dir`,
/// so the thing that was created is not even a peer. That asymmetry is the whole
/// measurement.
///
/// `GIT_CEILING_DIRECTORIES` naming the fixture root, from `src`, is the control:
/// a ceiling stops the upward *search*, and `init` does not search, so it must
/// change nothing. [`crate::corpus::discovery`] pins the ceiling's effect on
/// commands that do search; this pins its non-effect on the one that does not.
///
/// Every value spells the fixture root as `{repo}` or is relative, per
/// `apply_case_env`'s assertion.
fn environment(out: &mut Vec<Case>) {
    const DIR_NEW: &[(&str, &str)] = &[("GIT_DIR", "newgit")];
    const DIR_DOT_GIT: &[(&str, &str)] = &[("GIT_DIR", ".git")];
    const DIR_ABS: &[(&str, &str)] = &[("GIT_DIR", "{repo}/.git")];
    const DIR_AND_WORK_TREE: &[(&str, &str)] =
        &[("GIT_DIR", "newgit"), ("GIT_WORK_TREE", ".")];
    const WORK_TREE_DOT: &[(&str, &str)] = &[("GIT_WORK_TREE", ".")];
    const WORK_TREE_ROOT: &[(&str, &str)] = &[("GIT_WORK_TREE", "{repo}")];
    const OBJECTS: &[(&str, &str)] = &[("GIT_OBJECT_DIRECTORY", "alt-objs")];
    const CEILING: &[(&str, &str)] = &[("GIT_CEILING_DIRECTORIES", "{repo}")];

    out.push(init(&["init"]).with_env(DIR_NEW));
    out.push(init(&["init", "sub"]).with_env(DIR_NEW));
    out.push(init(&["init", "--bare"]).with_env(DIR_NEW));
    out.push(init(&["init"]).with_env(DIR_AND_WORK_TREE));
    out.push(init(&["init"]).with_env(DIR_DOT_GIT));
    out.push(init(&["init"]).with_env(DIR_ABS));
    out.push(init_strict(&["init"]).with_env(WORK_TREE_DOT));
    out.push(init_strict(&["init", "sub"]).with_env(WORK_TREE_ROOT));
    out.push(init(&["init", "sub"]).with_env(OBJECTS));
    out.push(init(&["init"]).with_env(OBJECTS));
    out.push(init(&["init"]).with_env(CEILING).in_dir("src"));
}

/// `init-db`: the pre-builtin spelling, on the axes `init` is measured on.
///
/// `git.c`'s command table maps `init-db` to `cmd_init_db` — the *same function*
/// `init` resolves to, with no wrapper and no separate option table — so every
/// answer in this file should reappear under the other name. That is a claim
/// about the port's dispatch table rather than about `init`, and it is exactly
/// the kind of claim a port breaks quietly: an alias that reaches a different
/// parser, or one that reaches the same parser with the subcommand name still in
/// `argv`, fails only on the arguments nobody tried.
///
/// So this is a *cross-check*, not a second copy of the corpus: one row per axis
/// that the `init` groups above establish, chosen so that a divergence here and
/// agreement above localises the fault in the alias rather than in `init`.
/// [`crate::corpus::misc_commands`] already owns the bare form, `-q`, `--bare`,
/// `-h`, an unknown flag, `--template=/no/such/template`, `--object-format=bogus`
/// and `-b renamed`; none of those is repeated.
fn init_db_alias(out: &mut Vec<Case>) {
    let db = |args: &[&str]| Case::new("init-db", args, Shape::Linear);
    out.push(db(&["init-db", "sub"]));
    out.push(db(&["init-db", "."]));
    out.push(db(&["init-db", "-q", "sub"]));
    out.push(db(&["init-db", "--bare", "sub.git"]));
    out.push(db(&["init-db", "--bare", "--shared=group"]));
    out.push(db(&["init-db", "--shared=group"]));
    out.push(db(&["init-db", "--shared=all"]));
    out.push(db(&["init-db", "-b", "topic", "sub"]));
    out.push(db(&["init-db", "--initial-branch=topic", "sub"]));
    out.push(db(&["init-db", "--separate-git-dir=gd", "sub"]));
    out.push(db(&["init-db", "--object-format=sha256", "sub"]));
    out.push(db(&["init-db", "--ref-format=reftable", "sub"]));
    out.push(db(&["init-db", "--template=src", "sub"]));
    out.push(db(&["init-db", "src"]));
    out.push(Case::strict("init-db", &["init-db", "README.md"], Shape::Linear));
    out.push(Case::strict("init-db", &["init-db", "--shared=bogus"], Shape::Linear));
    out.push(Case::strict("init-db", &["init-db", "-b", ".bad", "sub"], Shape::Linear));
    out.push(Case::strict("init-db", &["init-db", "--object-format=sha256"], Shape::Linear));
}
