//! The **recursion axis**: `--recurse-submodules` and the configuration that
//! turns it on, asked of every verb that claims to carry it, from the
//! superproject.
//!
//! # How this file divides the subsystem with its neighbours
//!
//! Six places already touch submodules, and each owns a different question. What
//! is written here is what none of them asks.
//!
//! | file | owns |
//! |---|---|
//! | `corpus/submodule_family.rs` | the `submodule` / `submodule--helper` **verb** — `status`, `init`, `deinit`, `update`, `summary`, `foreach`, `sync`, `set-url`, `set-branch`, `absorbgitdirs`, `add`; the front-end/helper split; ordinary verbs run **inside** `sub`; `-C sub`; `GIT_DIR` aimed at `.git/modules/sub` |
//! | `corpus/misc_commands.rs:145-262` | the 62 bare `submodule` / `submodule--helper` subcommands and their simplest flags |
//! | `corpus/discovery.rs:317-347` | the twelve-column `rev-parse` discovery grid from `sub` and from `.git/modules/sub` |
//! | `corpus/fixture_gaps3.rs:760-856` | [`Shape::NestedSubmodule`] read-side: `submodule update --init[ --recursive]`, `status`, `sync`, `summary`, `foreach`, `ls-files`, `status`, `config -f .gitmodules`, `diff --submodule=short\|log`, `ls-tree`, `cat-file` |
//! | `corpus/sequences.rs:5478-5535` | the two multi-step nested walks: init-then-recurse, and sync-then-deinit back out |
//! | `corpus/worktree_index.rs`, `corpus/index_plumbing.rs`, `corpus/info_attrs.rs`, `corpus/switch_restore.rs`, `corpus/reset_family.rs`, `corpus/fetch_clone.rs`, `corpus/transport_local.rs` | one or two `--recurse-submodules` rows each, on the verb that file is about: `ls-files`, `grep`, `switch`, `restore`, `reset`, `fetch`, `pull`, `clone` |
//!
//! The gap those leave is the axis itself. `--recurse-submodules` is not one
//! implementation; it is eleven — `clone`, `fetch`, `pull`, `push`, `checkout`,
//! `switch`, `restore`, `reset`, `read-tree`, `grep`, `ls-files`, each with its
//! own option table — switched on by three separate configuration keys
//! (`submodule.recurse`, `fetch.recurseSubmodules`, `push.recurseSubmodules`) that
//! do not agree on their value sets. A port that wires it into `fetch`, `switch`
//! and `grep` — the three the neighbours above happen to cover — and stubs it
//! everywhere else scores full marks today. What this file adds:
//!
//!  * the **verbs that do not have the flag at all** and must refuse it
//!    (`§ flag_that_is_not_there`), which is the only way to catch a port whose
//!    option parser accepts it globally;
//!  * the **value grammar per verb**, which is not uniform: `checkout`, `switch`,
//!    `reset`, `restore`, `read-tree`, `fetch`, `pull` and `push` take a value and
//!    say `fatal: bad recurse-submodules argument`, while `grep` and `ls-files`
//!    are boolean-only and say `error: option 'recurse-submodules' takes no value`
//!    with a *different* exit code (`§ value_grammar`);
//!  * `submodule.recurse` reaching a verb whose flag was never typed, **and its
//!    two documented exceptions** — `clone` and `ls-files` ignore it
//!    (`§ submodule_recurse_config`);
//!  * `push.recurseSubmodules`, including `only`, which is the one value on this
//!    fixture that changes what is written (`§ push_recurse`);
//!  * `--submodule=diff`, which is the only diff spelling that opens the
//!    submodule's *object store* from the superproject (`§ submodule_rendering`);
//!  * `.gitmodules` as a **parsed file** rather than as a premise — a duplicated
//!    name, an entry with no url, an entry naming a path that is not a gitlink,
//!    and a line that is not configuration at all (`§ gitmodules_parsing`);
//!  * two-level recursion through `clone` and through `submodule update --remote`
//!    on [`Shape::NestedSubmodule`] (`§ nested_recursion`).
//!
//! # What the recursion axis cannot reach with the shapes that exist
//!
//! Stated rather than papered over, because a case that measures nothing is
//! worse than no case:
//!
//!  * **No shape has two sibling submodules.** [`Shape::Submodule`] has one
//!    (`sub`), [`Shape::NestedSubmodule`] has one per level (`mid`, `mid/leaf`).
//!    So traversal *order* across siblings is unmeasurable, and so is
//!    `submodule.fetchJobs` / `--jobs` as parallelism: with one clone per level
//!    there is nothing to interleave. The `--jobs 2` and `submodule.fetchJobs=2`
//!    rows below are therefore honest only as "the option is accepted and does
//!    not reorder a one-element list" — they are kept because a port that rejects
//!    the option fails them, and they are *not* presented as parallelism cases.
//!  * **[`Shape::Submodule`]'s submodule is clean and already at the recorded
//!    commit.** `--recurse-submodules` on `checkout`, `switch`, `reset`,
//!    `restore` and `read-tree` therefore has nothing to do: stock recurses into
//!    a submodule that is already correct and prints nothing. Those rows measure
//!    the flag's *acceptance and its effect on the superproject*, not recursion.
//!    Real recursion on this shape exists in exactly one verb — `grep`, whose
//!    `sub/mod.txt:submodule content` line is present with the flag and absent
//!    without it — and that is why `grep` carries the config half of this file.
//!  * **[`Shape::NestedSubmodule`]'s `mid` is registered and not populated.** So
//!    `--recurse-submodules` on the worktree verbs finds nothing there either;
//!    only the two verbs that *populate* — `clone --recurse-submodules` and
//!    `submodule update --init --recursive` — descend two levels, and those are
//!    the nested rows below.
//!  * **A clone's own git directory is not in the digest.**
//!    `runner::collect_worktree` skips any directory named `.git`, and
//!    `probe_modules` reads `<fixture>/.git/modules` and nowhere else. So the
//!    `clone --recurse-submodules … copy` rows compare stdout, stderr, the exit
//!    code, `copy`'s **worktree** files and the superproject's own state — not
//!    `copy/.git/modules/mid/**`, where a recursive clone puts the two module
//!    repositories it created. That is the same blind spot every existing
//!    clone-into-the-fixture case has (`corpus/transport_local.rs:144`,
//!    `corpus/fetch_clone.rs:981`); it is named here rather than left for a reader
//!    to assume the rows cover more than they do. The equivalent question *is*
//!    measured for `submodule update --init --recursive`, which populates
//!    `<fixture>/.git/modules` directly and is read by `probe_modules` in full.
//!  * **A case is one argv against a pristine copy.** So `push
//!    --recurse-submodules=check` cannot be made to *fail* (nothing can move the
//!    gitlink to a commit the submodule's remote lacks first), `deinit` cannot be
//!    made to hit local modifications, and `.gitmodules` cannot be staged-but-not-
//!    committed. `ConfigScope::Modules` **appends** to the tracked `.gitmodules`,
//!    which is why every case in `§ gitmodules_parsing` also shows ` M .gitmodules`
//!    in the state probe; it cannot *remove* the `sub` stanza, so "a gitlink with
//!    no `.gitmodules` entry" stays out of reach and is not faked with a
//!    near-miss.
//!
//! # Output that carries an unmaskable path, and is therefore avoided
//!
//! [`crate::runner::normalize`] masks three paths: the running side's fixture
//! root (`<REPO>`), its `HOME`, and its exec-path. [`Shape::Submodule`]'s upstream
//! is **not** one of them — `fixture.rs:1092` builds it *beside the template*, and
//! the per-case copy inherits the template's absolute path in `.gitmodules`, in
//! `.git/config` and in `.git/modules/sub/config`. It is identical on both sides
//! within a run, so every case still scores correctly; it is not identical across
//! runs, so a failure block quoting it is not reproducible from the report alone.
//!
//! Three families print it and are spelled around here rather than accepted:
//!
//!  * `diff HEAD~1` **without a pathspec** prints the whole `.gitmodules` blob,
//!    url included. Every diff row below is written `-- sub`, which restricts the
//!    output to the gitlink and prints no path at all.
//!  * `config -f .gitmodules --list` and `--get submodule.sub.url` print it
//!    directly. The `.gitmodules`-parsing rows read `submodule.<name>.path`
//!    instead, or a key belonging to a stanza this file appended, both of which
//!    are relative by construction.
//!  * `submodule update --init` on [`Shape::Submodule`] prints nothing, but the
//!    same verb on [`Shape::NestedSubmodule`] prints `Submodule 'mid'
//!    (<REPO>/.mid.git) registered` — masked, because that shape's upstreams are
//!    bare repositories *inside* the fixture with relative urls
//!    (`fixture.rs:2340-2399`). Every nested row is therefore safe where the
//!    equivalent `Submodule` row would not be, and the nested rows are where the
//!    cloning half of this axis lives for exactly that reason.
//!
//! # `protocol.file.allow`
//!
//! No fixture sets it. Git's default is `user`, and `git-submodule.sh:29-30`
//! exports `GIT_PROTOCOL_FROM_USER=0` before dispatching, so the `submodule`
//! front-end is refused where the helper is allowed — the matrix
//! `submodule_family.rs` pins. Every case here that actually transports objects
//! names `protocol.file.allow=always` on its own command line, delivered
//! identically to both sides, so it measures the verb rather than the default.
//!
//! # Determinism
//!
//! Every case below was run twice against stock 2.55.0 **at two different
//! filesystem roots**, with stdout, stderr, exit code and a full
//! content-addressed walk of the resulting worktree and git directory compared
//! after masking. No `.fetchJobs` value above 1 is used for anything but flag
//! acceptance, and no case runs a `foreach` command whose output depends on
//! anything but its arguments.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    flag_that_is_not_there(out);
    value_grammar(out);
    recurse_on_the_worktree_verbs(out);
    submodule_recurse_config(out);
    push_recurse(out);
    submodule_rendering(out);
    ignore_submodules(out);
    gitmodules_parsing(out);
    nested_recursion(out);
    superproject_link(out);
}

/// The fixture's submodule path, and the tracked non-submodule paths the
/// `.gitmodules` rows point a bogus entry at.
const SUB: &str = "sub";

/// `protocol.file.allow=always`, for the rows that really move objects.
const ALLOW: &[(&str, &str)] = &[("protocol.file.allow", "always")];

// ---------------------------------------------------------------------------
// The flag that is not there
// ---------------------------------------------------------------------------

/// Ten verbs that **reject** `--recurse-submodules` in 2.55.0, pinned strictly.
///
/// This is the only group in the corpus that can catch a port whose option
/// parser knows the flag globally. Every other row in this file asks a verb that
/// has the flag whether it does the right thing with it; these ask verbs that do
/// *not* have it whether they say so — and a port that answers "0, did nothing"
/// instead of "129, unknown option" looks better on every summary while shipping
/// a flag that lies.
///
/// The refusals are not uniform, which is what makes them worth ten rows instead
/// of one. Measured against stock 2.55.0, they fall into three shapes:
///
/// | verbs | answer |
/// |---|---|
/// | `archive`, `status`, `add`, `commit`, `stash`, `clean`, `worktree`, `describe` | 129, `error: unknown option 'recurse-submodules'` **and** that command's usage block |
/// | `diff` | 129, the usage block **alone** — the diff option parser prints usage and never names the option |
/// | `log` | 128, `fatal: unrecognized argument: --recurse-submodules`, no usage at all |
///
/// A port with one error path for "unrecognised" gets one of the three shapes
/// right and the other two wrong.
///
/// Strict, because the usage block *is* the answer: it names the options the verb
/// really has, and a port that prints a plausible refusal with the wrong option
/// list has not implemented the verb, it has implemented the refusal.
fn flag_that_is_not_there(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    let rows: &[(&str, &[&str])] = &[
        ("archive", &["archive", "--recurse-submodules", "--format=tar", "HEAD"]),
        ("status", &["status", "--recurse-submodules"]),
        ("diff", &["diff", "--recurse-submodules", "HEAD"]),
        ("log", &["log", "--recurse-submodules", "-1"]),
        ("add", &["add", "--recurse-submodules", "."]),
        ("commit", &["commit", "--recurse-submodules", "-m", "x"]),
        ("stash", &["stash", "--recurse-submodules"]),
        ("clean", &["clean", "--recurse-submodules", "-n"]),
        ("worktree", &["worktree", "--recurse-submodules", "list"]),
        ("describe", &["describe", "--recurse-submodules"]),
    ];
    for (cmd, args) in rows {
        out.push(Case::strict(cmd, args, s));
    }
}

// ---------------------------------------------------------------------------
// The value grammar, which is not one grammar
// ---------------------------------------------------------------------------

/// `--recurse-submodules=<bad>`, per verb, because the answer is three different
/// answers.
///
/// Measured against stock 2.55.0 on this fixture:
///
/// | verb | `--recurse-submodules=bogus` |
/// |---|---|
/// | `checkout`, `switch`, `reset`, `restore`, `read-tree`, `fetch`, `pull`, `push` | 128 `fatal: bad recurse-submodules argument: bogus` |
/// | `grep`, `ls-files` | 129 `error: option 'recurse-submodules' takes no value` |
///
/// The split is `OPT_CALLBACK` against `OPT_BOOL`: the first group registers a
/// value-taking option whose callback rejects the string, the second registers a
/// plain boolean and `parse_options` refuses the `=` before any submodule code
/// runs. A port that models the flag as one option type answers one of these two
/// for all ten, and the ten rows separate the two halves in one pass.
///
/// The two `--ignore-submodules=bogus` rows are the sibling grammar — a
/// *different* option with a *different* enum (`none|untracked|dirty|all`) and a
/// third message, `fatal: bad --ignore-submodules argument: bogus` — and they are
/// here rather than in `diff_family.rs` because what they establish is that the
/// two option families are not the same parser wearing two names.
///
/// All strict: an exit code alone does not distinguish "rejected the value" from
/// "rejected the option".
fn value_grammar(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    let valued: &[(&str, &[&str])] = &[
        ("checkout", &["checkout", "--recurse-submodules=bogus", "main"]),
        ("switch", &["switch", "--recurse-submodules=bogus", "main"]),
        ("reset", &["reset", "--recurse-submodules=bogus", "--hard", "HEAD"]),
        ("restore", &["restore", "--recurse-submodules=bogus", "--source=HEAD", SUB]),
        ("read-tree", &["read-tree", "--recurse-submodules=bogus", "HEAD"]),
        ("fetch", &["fetch", "--recurse-submodules=bogus", "."]),
        ("pull", &["pull", "--recurse-submodules=bogus", ".", "main"]),
        ("push", &["push", "--recurse-submodules=bogus", ".", "main:refs/heads/x"]),
    ];
    for (cmd, args) in valued {
        out.push(Case::strict(cmd, args, s));
    }

    // Boolean-only: the `=` is refused by the option parser, not by a callback.
    out.push(Case::strict("grep", &["grep", "--recurse-submodules=bogus", "content"], s));
    out.push(Case::strict("ls-files", &["ls-files", "--recurse-submodules=bogus"], s));

    // A different option family with a different enum and a third message.
    out.push(Case::strict("diff", &["diff", "--ignore-submodules=bogus", "HEAD~1"], s));
    out.push(Case::strict("status", &["status", "--ignore-submodules=bogus"], s));

    // Two refusals that fire *after* the flag is accepted, from inside the
    // recursion path itself: `ls-files` cannot report an unmatched path across a
    // submodule boundary, and `grep` cannot walk untracked files inside one.
    // Both name the flag in the message, so they are the proof that the flag was
    // parsed and then consulted rather than ignored.
    out.push(Case::strict(
        "ls-files",
        &["ls-files", "--recurse-submodules", "--error-unmatch", SUB],
        s,
    ));
    out.push(Case::strict("grep", &["grep", "--recurse-submodules", "--untracked", "content"], s));
}

// ---------------------------------------------------------------------------
// The worktree verbs, where the flag parses and the submodule is already right
// ---------------------------------------------------------------------------

/// `--recurse-submodules` on the verbs that rewrite the worktree.
///
/// Stated honestly: on [`Shape::Submodule`] the submodule is clean and already at
/// the recorded commit, so stock recurses into it and finds nothing to do. These
/// rows measure that the flag is accepted in every one of its spellings and that
/// the *superproject* half of the command still happens — the branch is created,
/// the index is rewritten, the prefix is applied — not that recursion moved
/// anything. `corpus/reset_family.rs:475-499` is the one place in the corpus
/// where `--recurse-submodules` changes what is left on disk (a `reset --hard
/// HEAD~1` across the commit that added the submodule), and it is not repeated.
///
/// What is new here is coverage of the three verbs no curated case reaches at
/// all: `checkout` (only `switch` was covered, and they are separate
/// implementations in `builtin/checkout.c` sharing one struct), `read-tree`
/// (whose `--recurse-submodules` is `unpack_trees`' own, not the porcelain's),
/// and `push`. `read-tree --recurse-submodules --prefix=x/` is the interesting
/// one: the flag and `--prefix` are simultaneously in play, and the result is
/// visible only in the index the state probe reads.
fn recurse_on_the_worktree_verbs(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    out.push(Case::new("checkout", &["checkout", "--recurse-submodules", "main"], s));
    out.push(Case::new("checkout", &["checkout", "--recurse-submodules=yes", "main"], s));
    out.push(Case::new("checkout", &["checkout", "--recurse-submodules=no", "main"], s));
    out.push(Case::new("checkout", &["checkout", "--no-recurse-submodules", "-b", "topic"], s));
    out.push(Case::new("checkout", &["checkout", "--recurse-submodules", "-b", "topic"], s));

    out.push(Case::new("read-tree", &["read-tree", "--recurse-submodules", "-m", "-u", "HEAD"], s));
    out.push(Case::new("read-tree", &["read-tree", "--no-recurse-submodules", "-m", "-u", "HEAD"], s));
    out.push(Case::new("read-tree", &["read-tree", "--recurse-submodules", "HEAD"], s));
    out.push(Case::new("read-tree", &["read-tree", "--recurse-submodules", "--prefix=x/", "HEAD"], s));

    out.push(Case::new("restore", &["restore", "--recurse-submodules", "--source=HEAD", SUB], s));
    out.push(Case::new("restore", &["restore", "--no-recurse-submodules", "--source=HEAD", SUB], s));
}

// ---------------------------------------------------------------------------
// submodule.recurse, and the two verbs it does not reach
// ---------------------------------------------------------------------------

/// `submodule.recurse`: the flag nobody typed, and the exceptions.
///
/// Documented as "a boolean indicating if commands should enable
/// `--recurse-submodules` by default", applying to every command that has the
/// option **except `clone` and `ls-files`**. Both halves — that it reaches a verb
/// and that it skips those two — are only observable as a *difference between two
/// invocations*, and this fixture supplies exactly one verb where the difference
/// lands on stdout:
///
/// ```text
/// git grep content                            → exit 1, nothing
/// git -c submodule.recurse=true grep content  → exit 0, sub/mod.txt:submodule content
/// ```
///
/// So `grep` is the load-bearing row and the rest are the sweep around it. The
/// two exceptions are pinned as exceptions: `ls-files` under
/// `submodule.recurse=true` must print the four superproject entries with `sub`
/// as a bare gitlink and **not** `sub/mod.txt`, and `clone` under the same
/// setting must leave `copy/mid` empty — the latter on
/// [`Shape::NestedSubmodule`], where "did not descend" is a state a port cannot
/// fake by printing nothing.
///
/// `submodule.recurse` is delivered from two scopes on purpose, which is the
/// distinction [`ConfigScope`] exists for: `-c` hands the reader an already-split
/// `key=value` from the last source in git's precedence order, while
/// `.git/config` hands it a stanza that has to be parsed out of a file. A port
/// that reads the key from `-c` and never looks in the file scores full marks on
/// the command-line row alone.
///
/// `submodule.recurse=bogus` is a *boolean* parse failure —
/// `fatal: bad boolean config value 'bogus' for 'submodule.recurse'` — which is
/// a fourth distinct message beside the three in `§ value_grammar`, and strict
/// for that reason.
fn submodule_recurse_config(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    let on: &[(&str, &str)] = &[("submodule.recurse", "true")];

    // The row where recursion is visible on stdout, and its negative control.
    out.push(Case::new("grep", &["grep", "content"], s));
    out.push(Case::new("grep", &["grep", "content"], s).with_config(on));
    out.push(Case::new("grep", &["grep", "-c", "content"], s).with_config(on));
    out.push(Case::new("grep", &["grep", "-l", "submodule"], s).with_config(on));
    // From `.git/config` rather than from `-c`: a different parser reaching the
    // same key.
    out.push(Case::new("grep", &["grep", "content"], s).with_scoped_config(vec![
        ConfigEntry::set(ConfigScope::Repo, "submodule.recurse", "true"),
    ]));
    // The flag wins over the setting, in both directions.
    out.push(
        Case::new("grep", &["grep", "--recurse-submodules", "content"], s)
            .with_config(&[("submodule.recurse", "false")]),
    );
    out.push(
        Case::new("grep", &["grep", "--no-recurse-submodules", "content"], s).with_config(on),
    );
    out.push(Case::strict("grep", &["grep", "content"], s)
        .with_config(&[("submodule.recurse", "bogus")]));

    // The documented exception: `ls-files` ignores it.
    out.push(Case::new("ls-files", &["ls-files"], s).with_config(on));
    out.push(Case::new("ls-files", &["ls-files", "--stage"], s).with_config(on));

    // The verbs it does reach, where the submodule is already correct and the
    // measurement is that the superproject half still happened.
    out.push(Case::new("checkout", &["checkout", "main"], s).with_config(on));
    out.push(Case::new("checkout", &["checkout", "-b", "topic"], s).with_config(on));
    out.push(Case::new("read-tree", &["read-tree", "-m", "-u", "HEAD"], s).with_config(on));
    out.push(
        Case::new("restore", &["restore", "--source=HEAD", "--staged", "--worktree", SUB], s)
            .with_config(on),
    );
    out.push(Case::new("push", &["push", ".", "main:refs/heads/x"], s).with_config(on));
}

// ---------------------------------------------------------------------------
// push.recurseSubmodules
// ---------------------------------------------------------------------------

/// `push`'s own recursion enum, which is four values and not a boolean.
///
/// `check`, `on-demand`, `only` and `no`, plus `--no-recurse-submodules`. Three
/// of them are no-ops on this fixture — the gitlink's commit is already in the
/// submodule's remote, so `check` passes and `on-demand` has nothing to push —
/// and the fourth is not:
///
/// ```text
/// git push --recurse-submodules=only . main:refs/heads/x
///   → "Everything up-to-date", and refs/heads/x is NOT created
/// ```
///
/// `only` means *push the submodules and not the superproject*, so the
/// superproject ref the argument named is deliberately left unwritten. That is a
/// state difference the runner's `for-each-ref` probe reads, and it is the one
/// row in this group a port cannot pass by treating the enum as a boolean.
///
/// The config spelling is the same enum through `push.recurseSubmodules`, and its
/// rejection message names the key **lower-cased**:
/// `fatal: bad push.recursesubmodules argument: bogus`, not the camel-case the
/// command line used. Strict, because the casing is the whole difference between
/// echoing the key back and reporting the one git actually looked up.
///
/// The last row is the precedence question: `--recurse-submodules=no` on the
/// command line against `push.recurseSubmodules=only` in configuration. The flag
/// wins, so the ref *is* created — the opposite state from the row above it.
fn push_recurse(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    const REF: &str = "main:refs/heads/x";
    for v in ["check", "on-demand", "only", "no"] {
        let flag = format!("--recurse-submodules={v}");
        out.push(Case::new("push", &["push", &flag, ".", REF], s));
        out.push(Case::new("push", &["push", ".", REF], s)
            .with_config(&[("push.recurseSubmodules", v)]));
    }
    out.push(Case::new("push", &["push", "--no-recurse-submodules", ".", REF], s));
    out.push(Case::strict("push", &["push", ".", REF], s)
        .with_config(&[("push.recurseSubmodules", "bogus")]));
    // From `.git/config` rather than `-c`, and then the flag overriding it.
    out.push(Case::new("push", &["push", ".", REF], s).with_scoped_config(vec![
        ConfigEntry::set(ConfigScope::Repo, "push.recurseSubmodules", "only"),
    ]));
    out.push(
        Case::new("push", &["push", "--recurse-submodules=no", ".", REF], s)
            .with_config(&[("push.recurseSubmodules", "only")]),
    );
}

// ---------------------------------------------------------------------------
// --submodule=<format>
// ---------------------------------------------------------------------------

/// How a gitlink change is *rendered*, which is three renderers and one of them
/// opens a second repository.
///
/// Every row is written `-- sub`. Without the pathspec the same command prints
/// the `.gitmodules` blob, whose `url` is the template's absolute path and is not
/// masked (module header); with it the output is the gitlink alone and contains
/// no path at all.
///
/// The three formats against `HEAD~1`, the commit before the submodule existed:
///
/// ```text
/// --submodule=short  → a 160000 hunk: "+Subproject commit 7c9f5d7e…"
/// --submodule=log    → "Submodule sub 0000000...7c9f5d7 (new submodule)"
/// --submodule=diff   → that line, then a full diff of sub/mod.txt
/// ```
///
/// `=diff` is the row that matters. Producing it means opening
/// `.git/modules/sub`, resolving `7c9f5d7e…` there, and diffing its tree against
/// the empty tree — from a process whose repository is the superproject. A port
/// that renders the gitlink from the superproject's index alone produces the
/// first two and cannot produce the third.
///
/// `corpus/fixture_gaps3.rs:835-836` covers `=short` and `=log` on
/// [`Shape::NestedSubmodule`], where the submodule is unpopulated and `=diff`
/// would have nothing to open; `corpus/config_reads.rs:216` covers
/// `diff.submodule` as a value sweep on `diff HEAD~1 HEAD`. Neither reaches
/// `=diff` on a populated submodule, and neither reaches the other four verbs
/// that share the option: `log`, `show`, `diff-tree` and `diff --cached`.
///
/// Two failure spellings, and they are not the same failure.
/// `--submodule=bogus` is an option-parse error —
/// `error: failed to parse --submodule option parameter: 'bogus'`, exit 129.
/// `diff.submodule=bogus` is a *warning*, exit 0, the format falls back to
/// `short` — and stock prints the warning **twice**, once per diff setup pass.
/// Both are strict; the doubled warning in particular is the kind of detail a
/// port normalises away without noticing.
fn submodule_rendering(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    for fmt in ["short", "log", "diff"] {
        let flag = format!("--submodule={fmt}");
        out.push(Case::new("diff", &["diff", &flag, "HEAD~1", "--", SUB], s));
    }
    // The bare spelling, which is `=log`.
    out.push(Case::new("diff", &["diff", "--submodule", "HEAD~1", "--", SUB], s));
    out.push(Case::strict("diff", &["diff", "--submodule=bogus", "HEAD~1", "--", SUB], s));

    // The same renderer reached through four other verbs.
    out.push(Case::new("log", &["log", "-1", "-p", "--submodule=diff", "--", SUB], s));
    out.push(Case::new("log", &["log", "-1", "-p", "--submodule=log", "--", SUB], s));
    out.push(Case::new("show", &["show", "--submodule=diff", "HEAD", "--", SUB], s));
    out.push(Case::new(
        "diff-tree",
        &["diff-tree", "-p", "--submodule=diff", "HEAD~1", "HEAD", "--", SUB],
        s,
    ));
    out.push(Case::new("diff", &["diff", "--cached", "--submodule=diff", "HEAD~1", "--", SUB], s));

    // The config spelling, from the command line and from `.git/config`.
    out.push(
        Case::new("diff", &["diff", "HEAD~1", "--", SUB], s)
            .with_config(&[("diff.submodule", "diff")]),
    );
    out.push(Case::new("diff", &["diff", "HEAD~1", "--", SUB], s).with_scoped_config(vec![
        ConfigEntry::set(ConfigScope::Repo, "diff.submodule", "diff"),
    ]));
    out.push(Case::strict("diff", &["diff", "HEAD~1", "--", SUB], s)
        .with_config(&[("diff.submodule", "bogus")]));
    // The flag beats the setting.
    out.push(
        Case::new("diff", &["diff", "--submodule=short", "HEAD~1", "--", SUB], s)
            .with_config(&[("diff.submodule", "diff")]),
    );
}

// ---------------------------------------------------------------------------
// --ignore-submodules and status.submoduleSummary
// ---------------------------------------------------------------------------

/// The two settings that decide whether a submodule is *reported* at all.
///
/// `--ignore-submodules` has four values and the fixture's submodule is clean, so
/// all four `status --porcelain` rows print the same nothing; against `HEAD~1`,
/// where the gitlink is a genuine addition, `all` — and the bare spelling, which
/// means `all` — suppress the `sub` hunk that `none`, `untracked` and `dirty`
/// print. Those two rows are the whole measurement and the other six are the
/// sweep that makes them legible.
///
/// `status.submoduleSummary` is not a boolean, which is the point of the rows
/// that set it. It is a **count** — the number of submodule commits to summarise
/// — so `true` is accepted, `1` is accepted, and `bogus` fails with
/// `fatal: bad numeric config value 'bogus' for 'status.submodulesummary': invalid unit`,
/// a numeric-parse message rather than a boolean one, with the key lower-cased.
/// Strict, because the exit code alone does not say which of the two parsers
/// rejected it. On a clean fixture the accepted values print no summary section at
/// all, which is stated rather than implied: those rows measure the parse, not the
/// rendering.
fn ignore_submodules(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    for v in ["none", "untracked", "dirty", "all"] {
        let flag = format!("--ignore-submodules={v}");
        out.push(Case::new("diff", &["diff", &flag, "HEAD~1", "--", SUB], s));
        out.push(Case::new("status", &["status", "--porcelain", &flag], s));
    }
    // The bare spelling, which is `=all`.
    out.push(Case::new("diff", &["diff", "--ignore-submodules", "HEAD~1", "--", SUB], s));

    out.push(Case::new("status", &["status", "--long"], s)
        .with_config(&[("status.submoduleSummary", "true")]));
    out.push(Case::new("status", &["status", "--long"], s)
        .with_config(&[("status.submoduleSummary", "1")]));
    out.push(Case::strict("status", &["status", "--long"], s)
        .with_config(&[("status.submoduleSummary", "bogus")]));
    out.push(Case::new("status", &["status", "--long"], s).with_scoped_config(vec![
        ConfigEntry::set(ConfigScope::Repo, "status.submoduleSummary", "true"),
    ]));
}

// ---------------------------------------------------------------------------
// .gitmodules as a file that is parsed
// ---------------------------------------------------------------------------

/// `.gitmodules` treated as input rather than as premise.
///
/// [`ConfigScope::Modules`] appends to the tracked `.gitmodules`
/// (`runner.rs:1584`, `runner.rs:1718`), so a case can add a stanza and nothing
/// else. Every row therefore also shows ` M .gitmodules` in the state probe, on
/// both sides, and none of them can *remove* the `sub` stanza — which is why "a
/// gitlink with no `.gitmodules` entry" is absent from this file rather than
/// approximated.
///
/// Four pathologies, and what each one establishes:
///
///  * **A duplicated name.** A second `[submodule "sub"]` with a second `url`.
///    Git's last-value-wins rule applies inside `.gitmodules` exactly as it does
///    in `.git/config`, so `--get submodule.sub.path` answers once and
///    `--get-all` answers twice — read on `path`, never on `url`, because `url`
///    is the template's absolute path. `submodule sync` then has to pick one, and
///    which one it wrote is visible in `.git/config` through the state probe.
///  * **A stanza whose path is not a gitlink** (`ghost`, which does not exist;
///    and `notlink`, whose path is the tracked file `README.md`). `submodule
///    status` and `submodule init` walk the *index* and never see either — they
///    print exactly what they print without the stanza — while `submodule
///    set-url` walks `.gitmodules` and succeeds on `ghost`, writing a file for a
///    submodule that does not exist. That asymmetry is the finding: the two verbs
///    do not agree on what a submodule is. The `set-url ghost` row without the
///    stanza is the control, and it is `fatal: no submodule mapping found in
///    .gitmodules for path 'ghost'` with exit 128.
///  * **A stanza with no `url`.** `path = src` and nothing else, so the name
///    resolves to a real tracked directory that has no gitlink. `submodule init
///    src` and `submodule update src` both exit 0 in silence.
///  * **A line that is not configuration.** Appended as a raw line, so it lands
///    as line 4 of a three-line file, and stock answers
///    `fatal: bad config line 4 in file <REPO>/.gitmodules` — with the line
///    number, which is a fact only a file has. This is the strongest row in the
///    group because of *who* fails: not just `submodule status`, but
///    `status --porcelain` and `diff --submodule=log`, neither of which mentions
///    submodules in its arguments. A port that parses `.gitmodules` leniently, or
///    only when a `submodule` verb was named, exits 0 where stock exits 128.
///
/// Two more rows read `submodule.<name>.update` and `.branch` out of
/// `.gitmodules` rather than out of `.git/config`. `update = none` produces
/// `Skipping submodule 'sub'` on stderr with exit 0 —
/// `corpus/submodule_family.rs:285` reaches the same message through `-c`, and
/// the point of repeating it here is that the two arrive by different readers:
/// `submodule-config.c` for the tracked file, `config.c` for the command line.
fn gitmodules_parsing(out: &mut Vec<Case>) {
    let s = Shape::Submodule;

    // A duplicated `[submodule "sub"]`.
    let dup = || {
        vec![
            ConfigEntry::set(ConfigScope::Modules, "submodule.sub.path", "sub"),
            ConfigEntry::set(ConfigScope::Modules, "submodule.sub.url", "./dup-upstream"),
        ]
    };
    out.push(Case::new("config", &["config", "-f", ".gitmodules", "--get", "submodule.sub.path"], s)
        .with_scoped_config(dup()));
    out.push(
        Case::new("config", &["config", "-f", ".gitmodules", "--get-all", "submodule.sub.path"], s)
            .with_scoped_config(dup()),
    );
    out.push(Case::new("submodule", &["submodule", "status"], s).with_scoped_config(dup()));
    out.push(Case::new("submodule", &["submodule", "sync"], s).with_scoped_config(dup()));
    out.push(Case::new("submodule", &["submodule", "init"], s).with_scoped_config(dup()));

    // A stanza for a path that is not in the index at all.
    let ghost = || {
        vec![
            ConfigEntry::set(ConfigScope::Modules, "submodule.ghost.path", "ghost"),
            ConfigEntry::set(ConfigScope::Modules, "submodule.ghost.url", "./ghost-upstream"),
        ]
    };
    out.push(Case::new("submodule", &["submodule", "status"], s).with_scoped_config(ghost()));
    out.push(Case::new("submodule", &["submodule", "init"], s).with_scoped_config(ghost()));
    out.push(Case::strict("submodule", &["submodule", "status", "ghost"], s)
        .with_scoped_config(ghost()));
    out.push(Case::strict("submodule", &["submodule", "init", "ghost"], s)
        .with_scoped_config(ghost()));
    out.push(Case::strict("submodule", &["submodule", "sync", "ghost"], s)
        .with_scoped_config(ghost()));
    // The asymmetry: `set-url` reads `.gitmodules`, not the index, and succeeds.
    out.push(Case::new("submodule", &["submodule", "set-url", "ghost", "./y"], s)
        .with_scoped_config(ghost()));
    out.push(Case::new("submodule", &["submodule", "set-branch", "-b", "main", "ghost"], s)
        .with_scoped_config(ghost()));
    // The control, without the stanza.
    out.push(Case::strict("submodule", &["submodule", "set-url", "ghost", "./y"], s));

    // A stanza whose path is a tracked file that is not a gitlink.
    let notlink = || {
        vec![
            ConfigEntry::set(ConfigScope::Modules, "submodule.notlink.path", "README.md"),
            ConfigEntry::set(ConfigScope::Modules, "submodule.notlink.url", "./x"),
        ]
    };
    out.push(Case::new("submodule", &["submodule", "status"], s).with_scoped_config(notlink()));
    out.push(Case::new("submodule", &["submodule", "init", "README.md"], s)
        .with_scoped_config(notlink()));
    out.push(Case::new("submodule", &["submodule", "set-url", "README.md", "./z"], s)
        .with_scoped_config(notlink()));

    // A stanza with a path and no url.
    let nourl =
        || vec![ConfigEntry::set(ConfigScope::Modules, "submodule.nourl.path", "src")];
    out.push(Case::new("submodule", &["submodule", "init", "src"], s).with_scoped_config(nourl()));
    out.push(Case::new("submodule", &["submodule", "update", "src"], s).with_scoped_config(nourl()));
    out.push(Case::new("submodule", &["submodule", "status", "src"], s).with_scoped_config(nourl()));
    out.push(Case::strict("submodule", &["submodule", "set-url", "src", "./w"], s)
        .with_scoped_config(nourl()));

    // A line that is not configuration, reaching three verbs that did not ask
    // about submodules.
    let bad = || vec![ConfigEntry::raw(ConfigScope::Modules, "this is not a config line")];
    out.push(Case::strict("submodule", &["submodule", "status"], s).with_scoped_config(bad()));
    out.push(Case::strict("status", &["status", "--porcelain"], s).with_scoped_config(bad()));
    out.push(Case::strict("diff", &["diff", "--submodule=log", "HEAD~1", "--", SUB], s)
        .with_scoped_config(bad()));
    out.push(Case::strict("submodule", &["submodule", "sync"], s).with_scoped_config(bad()));
    // An unterminated subsection name is the same diagnostic by a different
    // route: the section header parses as far as the quote and then runs out.
    out.push(Case::strict("submodule", &["submodule", "status"], s).with_scoped_config(vec![
        ConfigEntry::raw(ConfigScope::Modules, "[submodule \"unterminated]"),
    ]));

    // Per-submodule policy read out of the tracked file rather than `.git/config`.
    out.push(Case::new("submodule", &["submodule", "update"], s).with_scoped_config(vec![
        ConfigEntry::set(ConfigScope::Modules, "submodule.sub.update", "none"),
    ]));
    out.push(Case::new("submodule", &["submodule", "update"], s).with_scoped_config(vec![
        ConfigEntry::set(ConfigScope::Modules, "submodule.sub.update", "rebase"),
    ]));
    // `with_scoped_config` *replaces* the whole config vector, so the transport
    // policy has to be one of its entries rather than a `with_config` beside it.
    out.push(Case::new("submodule", &["submodule", "update", "--remote"], s).with_scoped_config(
        vec![
            ConfigEntry::set(ConfigScope::Modules, "submodule.sub.branch", "main"),
            ConfigEntry::set(ConfigScope::CommandLine, "protocol.file.allow", "always"),
        ],
    ));
    out.push(Case::new("status", &["status", "--porcelain"], s).with_scoped_config(vec![
        ConfigEntry::set(ConfigScope::Modules, "submodule.sub.ignore", "all"),
    ]));
    out.push(Case::new("diff", &["diff", "HEAD~1", "--", SUB], s).with_scoped_config(vec![
        ConfigEntry::set(ConfigScope::Modules, "submodule.sub.ignore", "all"),
    ]));
}

// ---------------------------------------------------------------------------
// Two levels, and the verbs that actually descend them
// ---------------------------------------------------------------------------

/// Recursion that is recursion: [`Shape::NestedSubmodule`], where `--recursive`
/// and its absence build different repositories.
///
/// `corpus/fixture_gaps3.rs:760-796` already runs `submodule update --init`,
/// `--init --recursive`, `--init --recursive --depth 1`, `--init -- mid`,
/// `--recursive`, `--init --checkout` and `--init --recursive --no-fetch` on this
/// shape, and `corpus/sequences.rs:5490-5535` walks the two levels down and back
/// up in two multi-step sequences. What is *not* there is the other verb that
/// descends — `clone` — and the three configuration keys that stop the descent
/// part way.
///
/// **`clone --recurse-submodules`** is the case this shape was built for and did
/// not have. It clones the superproject, then `mid`, then `mid/leaf`, printing
/// two `Submodule path '<p>': checked out '<oid>'` lines in traversal order.
/// Every path in its output — `<REPO>/copy/mid`, `<REPO>/./.mid.git` — is under
/// the fixture root and is masked, which is precisely what the equivalent case on
/// [`Shape::Submodule`] (`corpus/transport_local.rs:144`,
/// `corpus/fetch_clone.rs:981`) cannot say. Four spellings around it:
/// `--no-recurse-submodules` after `--recurse-submodules` (last wins, no
/// descent), `-j2`, `--remote-submodules`, and `--shallow-submodules` (which
/// emits `warning: --depth is ignored in local clones`, once per module).
///
/// **Two rows are strict, and that is where the depth is actually measured.**
/// The `Submodule path '<p>': checked out` lines on *stdout* carry the full
/// `mid/leaf` for the second level in every implementation that gets the checkout
/// right, so stdout alone does not separate "descended one level correctly" from
/// "descended two". The registration line does, and it is on **stderr**:
///
/// ```text
/// Submodule 'leaf' (<REPO>/./.leaf.git) registered for path 'mid/leaf'
/// ```
///
/// That `mid/` prefix is the running display path a recursive walk has to carry
/// down with it, and it is the only byte in the whole invocation that says
/// whether it did. `clone --recurse-submodules` and
/// `submodule update --init --recursive --remote` are therefore strict; the
/// option-variation rows around them are not, because they would restate the same
/// finding once per flag instead of measuring the flag.
///
/// **`submodule.recurse=true` with a plain `clone`** is the documented exception
/// from `§ submodule_recurse_config`, measured here because only here does "did
/// not descend" leave evidence: `copy/mid` stays an empty directory and
/// `copy/.git/modules` is never created, both of which the state probe reads.
///
/// **The three ways to stop the descent** produce three different stderrs and
/// three different repositories, all with `--init --recursive` asked for:
///
/// ```text
/// submodule.mid.update = none   → "Submodule 'mid' … registered", "Skipping submodule 'mid'"
/// submodule.mid.active = false  → "Submodule 'mid' … registered", and no clone
/// submodule.active     = nothing→ nothing at all: not even registered
/// ```
///
/// A port with one "is this submodule wanted" predicate cannot produce three
/// answers.
///
/// `--jobs 2` and `submodule.fetchJobs=2` are here as flag acceptance and are
/// labelled as such in the module header: one clone per level is not parallelism,
/// and a case that pretended otherwise would be measuring a scheduler that never
/// ran.
fn nested_recursion(out: &mut Vec<Case>) {
    let s = Shape::NestedSubmodule;
    out.push(
        Case::strict("clone", &["clone", "--recurse-submodules", ".", "copy"], s).with_config(ALLOW),
    );
    out.push(
        Case::new("clone", &["clone", "--recurse-submodules", "--no-recurse-submodules", ".", "copy"], s)
            .with_config(ALLOW),
    );
    out.push(
        Case::new("clone", &["clone", "--recurse-submodules", "-j2", ".", "copy"], s)
            .with_config(ALLOW),
    );
    out.push(
        Case::new("clone", &["clone", "--recurse-submodules", "--remote-submodules", ".", "copy"], s)
            .with_config(ALLOW),
    );
    out.push(
        Case::new("clone", &["clone", "--recurse-submodules", "--shallow-submodules", ".", "copy"], s)
            .with_config(ALLOW),
    );
    // The exception: `submodule.recurse` does not reach `clone`.
    out.push(Case::new("clone", &["clone", ".", "copy"], s).with_config(&[
        ("protocol.file.allow", "always"),
        ("submodule.recurse", "true"),
    ]));

    // The remote-tracking descent, which `fixture_gaps3` does not reach.
    out.push(
        Case::strict("submodule", &["submodule", "update", "--init", "--recursive", "--remote"], s)
            .with_config(ALLOW),
    );
    out.push(
        Case::new("submodule", &["submodule", "update", "--init", "--recursive", "--jobs", "2"], s)
            .with_config(ALLOW),
    );
    out.push(
        Case::new("submodule", &["submodule", "update", "--init", "--recursive"], s)
            .with_config(&[("protocol.file.allow", "always"), ("submodule.fetchJobs", "2")]),
    );
    out.push(
        Case::new("submodule", &["submodule", "update", "--init", "--recursive", "--single-branch"], s)
            .with_config(ALLOW),
    );

    // Three ways to stop half way, from three different keys.
    out.push(
        Case::new("submodule", &["submodule", "update", "--init", "--recursive"], s)
            .with_config(&[("protocol.file.allow", "always"), ("submodule.mid.update", "none")]),
    );
    out.push(
        Case::new("submodule", &["submodule", "update", "--init", "--recursive"], s)
            .with_config(&[("protocol.file.allow", "always"), ("submodule.mid.active", "false")]),
    );
    out.push(
        Case::new("submodule", &["submodule", "update", "--init", "--recursive"], s)
            .with_config(&[("protocol.file.allow", "always"), ("submodule.active", "nothing")]),
    );
    // The same key out of the tracked `.gitmodules` instead, which is a different
    // reader and — for `update` — a deliberately restricted one. The transport
    // policy is an entry of the same vector because `with_scoped_config` replaces
    // the config list rather than adding to it.
    out.push(
        Case::new("submodule", &["submodule", "update", "--init", "--recursive"], s)
            .with_scoped_config(vec![
                ConfigEntry::set(ConfigScope::Modules, "submodule.mid.update", "none"),
                ConfigEntry::set(ConfigScope::CommandLine, "protocol.file.allow", "always"),
            ]),
    );
}

// ---------------------------------------------------------------------------
// The link back to the superproject
// ---------------------------------------------------------------------------

/// The reverse pointer, asked in the two spellings `corpus/discovery.rs` and
/// `corpus/submodule_family.rs` leave out.
///
/// `--show-superproject-working-tree` is already pinned from `sub`, from the
/// fixture root and from `.git/modules/sub`. What is not pinned is what happens
/// when the repository was selected by the **environment** rather than by
/// discovery: `GIT_DIR` and `GIT_WORK_TREE` naming the submodule, from inside the
/// submodule. Stock still answers `<REPO>` — the superproject link is read out of
/// the submodule's `config` and its `.git` file is never consulted — so a port
/// that finds the superproject by walking up from the cwd, rather than by asking
/// the selected repository, answers the same thing for the wrong reason and then
/// answers wrongly the moment the two disagree.
///
/// `--show-superproject-working-tree` from `src/` is the third position it can
/// be asked from — inside the superproject but not at its root — and it must
/// still print nothing and exit 0. `--git-common-dir` from `sub` is deliberately
/// **not** here: `corpus/discovery.rs:329` already runs it as part of the twelve
/// columns it asks from `sub`.
fn superproject_link(out: &mut Vec<Case>) {
    let s = Shape::Submodule;
    out.push(
        Case::new("rev-parse", &["rev-parse", "--show-superproject-working-tree"], s)
            .in_dir(SUB)
            .with_env(&[
                ("GIT_DIR", "{repo}/.git/modules/sub"),
                ("GIT_WORK_TREE", "{repo}/sub"),
            ]),
    );
    out.push(
        Case::new("submodule", &["submodule", "status"], s).in_dir(SUB).with_env(&[
            ("GIT_DIR", "{repo}/.git/modules/sub"),
            ("GIT_WORK_TREE", "{repo}/sub"),
        ]),
    );
    out.push(Case::new("rev-parse", &["rev-parse", "--show-superproject-working-tree"], s).in_dir("src"));
}
