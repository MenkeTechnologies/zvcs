//! A repository that has **more than one working tree**: the administrative
//! files that make that true, the ref and config namespaces that are per-tree
//! rather than per-repository, and the gate every verb has to consult before it
//! moves a branch some other tree is standing on.
//!
//! # How this divides territory with the seven files that already say `worktree`
//!
//! `worktree` is the most-covered verb in the corpus, so the boundary matters
//! more here than the breadth does. What each of the others owns:
//!
//! * **`worktree_index.rs`** — the nearest neighbour. `list` in four forms,
//!   five `add` forms, `prune`, `repair` and eight refusals, all on shapes with
//!   **no linked worktree at all** ([`Shape::Linear`], [`Shape::Branched`],
//!   [`Shape::Submodule`], [`Shape::Conflicted`]). It is the "what does this
//!   verb do to a repository that has only one working tree" file.
//! * **`stateful_side_files.rs`** — the same verbs against
//!   [`Shape::Worktree`], which has one. It owns `list` from three vantage
//!   points, the `add` flags that decide *what is written* (`--lock`,
//!   `--no-checkout`, `-B`, `--orphan`, `--relative-paths`, `--track`,
//!   `worktree.guessRemote`, `-f`), plain `lock`/`unlock`/`move`/`remove`/
//!   `repair`/`prune --expire=now`, and five `add` refusals.
//! * **`fixture_gaps2.rs`** — [`Shape::WorktreeLocked`]: the lock protocol and
//!   the prunable registration. `remove`/`unlock`/`lock`/`prune`/`repair`/
//!   `move` against a tree that is locked, one that is open and one whose
//!   directory is gone, plus `branch -D`, `checkout` and `switch` on the
//!   branches those trees hold.
//! * **`sequences.rs`** — multi-step: `add`→`move`→`remove`→`prune`,
//!   `add`→`lock`→refuse→`unlock`, and a worktree that is committed in.
//! * **`sparse_family.rs`** — one row, `worktree add wt2` on
//!   [`Shape::Sparse`], for what `add` copies into the new tree's
//!   `config.worktree`.
//! * **`exit_codes.rs`** — four exit-code rows (`add wt main`, bare `add`,
//!   `remove nosuchwt`, `lock .`).
//! * **`misc_commands.rs`** — `worktree <sub> -h` for all eight subcommands.
//!
//! **What is left, and is what this file is:** the parts of "a repository can
//! have more than one working tree" that are *not* the `worktree` verb.
//!
//!  1. **`refs/worktree/*`** — the per-worktree ref namespace. Zero cases in
//!     the corpus mention it. `update-ref refs/worktree/pin` writes
//!     `.git/refs/worktree/pin` from the main tree and
//!     `.git/worktrees/wt/refs/worktree/pin` from the linked one, and the two
//!     are read back by two *different* probes (`probe_state`'s `for-each-ref`
//!     and `probe_worktrees`'s file walk), so a port that puts the ref in the
//!     common directory from inside a linked tree is caught in both digests at
//!     once.
//!  2. **`rev-parse --git-path`** — zero cases in the corpus. It is the one
//!     query whose answer *is* the split between the per-worktree directory and
//!     the common one, per path: `HEAD` and `index` resolve into
//!     `.git/worktrees/wt/`, `config` and `refs/heads/main` into `.git/`.
//!  3. **`config --worktree` from inside a linked worktree.** `config_cmd.rs`
//!     owns the *reads* (`--get wt.k` in `wt/`) and the refusal when the
//!     extension is off; it writes only on [`Shape::Linear`], where "the
//!     worktree config" and "the main config.worktree" are the same file. From
//!     inside `wt/` with the extension on they are two different files, and the
//!     one that must be written is `.git/worktrees/wt/config.worktree`.
//!  4. **The other-worktree gate on the verbs that are not `switch`.**
//!     `switch_restore.rs` owns `switch linked` / `switch main`; nothing owns
//!     `checkout`, `checkout -B`, `switch -C`, `rebase <upstream> <branch>`,
//!     `branch -f`, or `worktree add <branch>` where the holder is a **linked**
//!     tree rather than the main one. See the corruption note below.
//!  5. **`HEAD@{n}` and the reflog inside a linked worktree.**
//!     `maintenance_repack.rs` owns `reflog expire` on [`Shape::Worktree`] from
//!     the root; nothing reads `.git/worktrees/wt/logs/HEAD` through the
//!     revision syntax that names it.
//!  6. **`add` flags and path forms nobody passes** — `--no-track`,
//!     `--guess-remote`, `--no-guess-remote`, explicit `--checkout`,
//!     `--lock --reason` together, `--orphan -b`, and paths that are a
//!     tracked directory, a directory that must be created, a subdirectory of
//!     the worktree, or inside `.git` itself.
//!  7. **`list`'s two argument errors**, `repair` on a path that is not a
//!     worktree, and `prune --expire` with the two *keyword* expiries.
//!
//! # Determinism: what this territory prints that cannot be compared
//!
//! Almost everything here names an absolute path, and that is survivable
//! because `runner::normalize` masks each side's own fixture root to `<REPO>`
//! in stdout, stderr **and** the state digest (`runner.rs:5780`). So
//! `worktree list`'s paths, the `already used by worktree at '…'` refusals,
//! `--git-path`'s absolute answers from inside a linked tree, and the absolute
//! `gitdir` that a non-`--relative-paths` `add` writes are all comparable, and
//! the refusals are `strict` here rather than exit-code-only.
//!
//! Two things are genuinely unusable and no case below reaches them:
//!
//!  * **`worktree prune --expire=<date>` with a real date.** The registration's
//!     mtime is wall-clock, so a threshold between "now" and "the fixture was
//!     built" is a race. Only the keyword expiries (`all`, `never`) are used;
//!     `--expire=now` is already owned by `stateful_side_files.rs`.
//!  * **A worktree registered outside the fixture root.** `link_note` refuses
//!     to read through a link that leaves the fixture and `mask_paths` has no
//!     token for a third root, so every path below stays inside the copy.
//!
//! And one thing is unmeasurable for a reason that is about the *shapes*, not
//! the harness: **no shape has two worktrees at different commits.**
//! [`Shape::Worktree`]'s `main` and `linked` both point at the initial commit,
//! and all four of [`Shape::WorktreeLocked`]'s refs point at its second one. So
//! `merge linked`, `rebase linked` and `reset --hard linked` from the main tree
//! are all no-ops on stock (`Already up to date.`, exit 0) and measure only
//! that the port also does nothing. They are omitted rather than written as
//! decoration; separating "read the other tree's HEAD" from "read my own"
//! through those three verbs needs a shape this file may not add.
//!
//! `.git/worktrees/<id>/index` is compared by **length only**
//! (`runner::probe_worktrees`), because it carries `ctime`/`ino`/`dev`. Every
//! other administrative file — `gitdir`, `commondir`, `HEAD`, `ORIG_HEAD`,
//! `locked`, `config.worktree`, `logs/HEAD`, `refs/worktree/*` — is compared
//! byte for byte, which is what makes groups 1, 3 and the `lock --reason` rows
//! measurable at all.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    per_worktree_refs(out);
    git_path(out);
    worktree_config(out);
    add_flags_and_paths(out);
    other_worktree_gate(out);
    administration(out);
}

/// `extensions.worktreeConfig` delivered for real: `ConfigScope::Worktree`
/// writes `.git/config.worktree` **and** the gate in `.git/config`, so the
/// extension is on rather than asserted by a `-c` git would ignore.
///
/// `wt.k` is set in both the repository scope and the worktree scope so the two
/// files hold different values — which is what separates "read the right file"
/// from "read a file".
fn worktree_extension() -> Vec<ConfigEntry> {
    vec![
        ConfigEntry::set(ConfigScope::Repo, "wt.k", "fromlocal"),
        ConfigEntry::set(ConfigScope::Worktree, "wt.k", "fromworktree"),
    ]
}

// ---------------------------------------------------------------------------
// 1. The per-worktree ref namespace
// ---------------------------------------------------------------------------

/// `refs/worktree/*`, and the reflog a linked worktree keeps for its own `HEAD`.
///
/// # The gap
///
/// `refs/worktree/` is the one ref prefix git resolves against the *current*
/// working tree instead of the common directory, and no case in the corpus
/// names it. Verified on stock 2.55.0 against a rebuilt [`Shape::Worktree`]:
///
/// ```text
/// $ git -C wt update-ref refs/worktree/pin HEAD
/// $ find .git/worktrees -type f
/// .git/worktrees/wt/refs/worktree/pin      <- here
/// $ git for-each-ref --format='%(refname)'  # from the root
/// refs/heads/linked
/// refs/heads/main                           <- and not here
///
/// $ git update-ref refs/worktree/pin HEAD   # from the root instead
/// $ ls .git/refs/worktree/pin               # -> .git/refs/worktree/pin
/// $ git -C wt rev-parse refs/worktree/pin
/// fatal: ambiguous argument 'refs/worktree/pin': unknown revision …
/// ```
///
/// Both destinations are probed, by two different probes, so the pair pins the
/// split in both directions: a port that always writes the common directory
/// fails row two on `probe_worktrees`, and one that always writes the
/// per-worktree directory fails row one on `probe_state`'s `for-each-ref`.
///
/// `--include-root-refs` from inside the linked tree is the read side of the
/// same split: stock lists `ORIG_HEAD` there and not at the root, because
/// `ORIG_HEAD` is per-worktree too and only `wt` has one.
/// `ref_storage.rs:972` asks this from the root; the vantage point is the whole
/// difference.
fn per_worktree_refs(out: &mut Vec<Case>) {
    out.push(Case::new("update-ref", &["update-ref", "refs/worktree/pin", "HEAD"], Shape::Worktree));
    out.push(
        Case::new("update-ref", &["update-ref", "refs/worktree/pin", "HEAD"], Shape::Worktree)
            .in_dir("wt"),
    );
    // With a reflog: the log lands beside the ref, under the *worktree's* logs
    // directory, so it is a second file in the same per-tree namespace.
    out.push(
        Case::new(
            "update-ref",
            &["update-ref", "--create-reflog", "refs/worktree/pin", "HEAD"],
            Shape::Worktree,
        )
        .in_dir("wt"),
    );
    // Deleting one that was never created: the refusal has to come from the
    // per-worktree namespace, not from the common one.
    out.push(
        Case::strict("update-ref", &["update-ref", "-d", "refs/worktree/pin"], Shape::Worktree)
            .in_dir("wt"),
    );

    // The read side. `%(worktreepath)` is absolute and masked to `<REPO>`.
    out.push(
        Case::new(
            "for-each-ref",
            &["for-each-ref", "--include-root-refs", "--format=%(refname)|%(objecttype)|%(worktreepath)"],
            Shape::Worktree,
        )
        .in_dir("wt"),
    );

    // The linked worktree's own `HEAD` reflog, reached through the syntax that
    // names it. `.git/worktrees/wt/logs/HEAD` has two entries — the checkout
    // `worktree add` made and the `read-tree` the fixture used to zero the
    // index's stat data — so `HEAD@{0}` and `HEAD@{1}` both resolve and
    // `HEAD@{2}` does not, which is the boundary an implementation reading the
    // *common* log would get wrong in both directions.
    out.push(Case::new("reflog", &["reflog", "show", "HEAD"], Shape::Worktree).in_dir("wt"));
    out.push(Case::new("reflog", &["reflog", "show", "HEAD@{0}"], Shape::Worktree).in_dir("wt"));
    out.push(
        Case::new("rev-parse", &["rev-parse", "HEAD@{0}", "HEAD@{1}"], Shape::Worktree).in_dir("wt"),
    );
    out.push(Case::strict("rev-parse", &["rev-parse", "HEAD@{2}"], Shape::Worktree).in_dir("wt"));
    out.push(Case::new("log", &["log", "-g", "--oneline", "HEAD"], Shape::Worktree).in_dir("wt"));
}

// ---------------------------------------------------------------------------
// 2. rev-parse --git-path
// ---------------------------------------------------------------------------

/// `rev-parse --git-path <name>`: the query that resolves one path at a time
/// against the split directory.
///
/// # The gap
///
/// No case in the corpus passes `--git-path`. `discovery.rs` asks for
/// `--git-dir` and `--git-common-dir`, which report the two *directories*; what
/// they cannot report is git's per-path routing table, and the routing is where
/// the mistakes are — `HEAD` and `index` are per-worktree, `config` and
/// `refs/heads/*` are common, and a port with one rule for all of them answers
/// half the rows right whichever rule it picked.
///
/// Measured on stock 2.55.0 over [`Shape::Worktree`], with the fixture root
/// masked as the runner masks it:
///
/// ```text
/// cwd = repo root          cwd = wt                        cwd = .git/worktrees/wt
/// .git/HEAD                <REPO>/.git/worktrees/wt/HEAD   HEAD
/// .git/index               <REPO>/.git/worktrees/wt/index  index
/// .git/config              <REPO>/.git/config              <REPO>/.git/config
/// .git/logs/HEAD           <REPO>/.git/worktrees/wt/logs/HEAD  logs/HEAD
/// ```
///
/// Three different spellings of the same routing — relative where git can,
/// absolute where it must — so the rows also pin the *rendering* choice, which
/// is the half of `--git-path` that `--git-dir` alone never exercises.
fn git_path(out: &mut Vec<Case>) {
    const CORE: &[&str] = &[
        "rev-parse",
        "--git-path",
        "HEAD",
        "--git-path",
        "index",
        "--git-path",
        "config",
        "--git-path",
        "logs/HEAD",
    ];
    out.push(Case::new("rev-parse", CORE, Shape::Worktree));
    out.push(Case::new("rev-parse", CORE, Shape::Worktree).in_dir("wt"));
    out.push(Case::new("rev-parse", CORE, Shape::Worktree).in_dir(".git/worktrees/wt"));

    // Three names that do not exist as files. `--git-path` is a router, not a
    // stat: it answers for a path whether or not anything is there, and
    // `config.worktree` in particular routes per-worktree while `config` does
    // not — the same pair of names, opposite answers.
    out.push(
        Case::new(
            "rev-parse",
            &[
                "rev-parse",
                "--git-path",
                "config.worktree",
                "--git-path",
                "objects/info/alternates",
                "--git-path",
                "shallow",
            ],
            Shape::Worktree,
        )
        .in_dir("wt"),
    );

    // The three files that *make* a linked worktree one, asked for by name from
    // inside a locked tree — where `locked` is a file that actually exists.
    out.push(
        Case::new(
            "rev-parse",
            &["rev-parse", "--git-path", "locked", "--git-path", "gitdir", "--git-path", "commondir"],
            Shape::WorktreeLocked,
        )
        .in_dir("wt"),
    );
}

// ---------------------------------------------------------------------------
// 3. config --worktree from inside a linked worktree
// ---------------------------------------------------------------------------

/// `config --worktree` where "the worktree" is a *linked* one.
///
/// # The gap
///
/// `config_cmd.rs` owns this key's reads — `--get wt.k` and `--show-scope` in
/// `wt/`, and the `--worktree --list` refusal when `extensions.worktreeConfig`
/// is off. Its one *write* row is on [`Shape::Linear`], where the repository has
/// a single working tree and `--worktree` therefore targets
/// `.git/config.worktree`, the same file the scope installed. From inside a
/// linked tree those are two different files and only one of them is right.
///
/// Measured on stock 2.55.0, extension on, `.git/config.worktree` holding
/// `wt.k = fromworktree`:
///
/// ```text
/// $ git -C wt config --worktree --list
/// fatal: unable to read config file
///   '<REPO>/.git/worktrees/wt/config.worktree': No such file or directory   (exit 128)
/// $ git -C wt config --worktree wtnew.k v
/// $ cat .git/worktrees/wt/config.worktree
/// [wtnew]
///         k = v
/// $ cat .git/config.worktree          # untouched
/// [wt]
///         k = "fromworktree"
/// ```
///
/// The refusal is the sharpest row of the three: a port that resolves
/// `--worktree` off the common directory prints `wt.k=fromworktree` and exits 0
/// where stock dies, and it is `strict` because the path in that message is
/// masked to `<REPO>` like every other path here.
///
/// The write is visible in the digest because `probe_worktrees` reads every
/// file under `.git/worktrees/` byte for byte — `config.worktree` included —
/// while `probe_state`'s `config --list --local` would show a misdirected write
/// into `.git/config` instead. Whichever way the port is wrong, one of the two
/// sections moves.
fn worktree_config(out: &mut Vec<Case>) {
    out.push(
        Case::strict("config", &["config", "--worktree", "--list"], Shape::Worktree)
            .with_scoped_config(worktree_extension())
            .in_dir("wt"),
    );
    out.push(
        Case::new("config", &["config", "--worktree", "wtnew.k", "v"], Shape::Worktree)
            .with_scoped_config(worktree_extension())
            .in_dir("wt"),
    );
    // Unsetting a key the linked tree does not have, while the *main* tree's
    // `config.worktree` does: stock exits 5 and writes nothing. A port reading
    // the common file finds the key and removes it.
    out.push(
        Case::strict("config", &["config", "--worktree", "--unset", "wt.k"], Shape::Worktree)
            .with_scoped_config(worktree_extension())
            .in_dir("wt"),
    );
    // The main working tree of a repository that has more than one: `--worktree`
    // means `.git/config.worktree` here, and this is the row that separates
    // "always the common file" from "always the current tree's file" — the two
    // wrong rules that each pass one of the rows above.
    out.push(
        Case::new("config", &["config", "--worktree", "wtnew.k", "v"], Shape::Worktree)
            .with_scoped_config(worktree_extension()),
    );
}

// ---------------------------------------------------------------------------
// 4. worktree add: flags and path forms
// ---------------------------------------------------------------------------

/// The `add` options and destinations that no other file passes.
///
/// `stateful_side_files.rs` owns the flags that change *what is written*
/// (`--lock` alone, `--no-checkout`, `-B`, `--orphan` alone,
/// `--relative-paths`, `--track`, `-f`) and `worktree_index.rs` owns the plain
/// forms. What is left is the tracking negations, the remote guess in both
/// directions, the explicit default, the two flags that only mean something
/// *together*, and four destinations that exercise the path handling rather
/// than the registration.
fn add_flags_and_paths(out: &mut Vec<Case>) {
    // `--no-track` suppresses the `branch.nt.*` pair that `--track` writes, so
    // the assertion is a `config --list --local` with *nothing* added to it.
    out.push(Case::new("worktree", &["worktree", "add", "--no-track", "-b", "nt", "wtnt"], Shape::Worktree));
    // The flag spelling of `worktree.guessRemote`, which
    // `stateful_side_files.rs` reaches only through `-c`. No shape has a branch
    // that exists solely on a remote, so what both rows pin is that neither
    // direction perturbs the ordinary DWIM — a port that treats
    // `--guess-remote` as "always detach" or "always track" fails one of them.
    out.push(Case::new("worktree", &["worktree", "add", "--guess-remote", "wtgr"], Shape::Worktree));
    out.push(Case::new("worktree", &["worktree", "add", "--no-guess-remote", "wtng"], Shape::Worktree));
    // `--checkout` is the documented default said out loud, with `-q` to
    // suppress the progress lines: the state must match a bare `add` exactly.
    out.push(Case::new("worktree", &["worktree", "add", "--checkout", "-q", "wtco"], Shape::Worktree));
    // `--lock` *with* a reason: the pair writes a `locked` file with content
    // where `--lock` alone writes an empty one, and `probe_worktrees` compares
    // that content byte for byte.
    out.push(Case::new(
        "worktree",
        &["worktree", "add", "--lock", "--reason", "held-by-the-case", "wtlr"],
        Shape::Worktree,
    ));
    // `--orphan` with an explicit branch name, which is a different argument
    // path from `--orphan` alone (that one derives the name from the basename).
    out.push(Case::new("worktree", &["worktree", "add", "--orphan", "-b", "orphb", "wto"], Shape::Worktree));
    // `--orphan` and a commit-ish are mutually exclusive; the parse must refuse
    // before anything is created.
    out.push(Case::strict("worktree", &["worktree", "add", "--orphan", "wto", "HEAD"], Shape::Worktree));

    // ---- destinations ----
    // A tracked directory that already exists and is not empty: refused, and
    // `src/lib.rs` has to still be there afterwards.
    out.push(Case::strict("worktree", &["worktree", "add", "src"], Shape::Worktree));
    // Two levels of directory that do not exist yet: `add` creates them.
    out.push(Case::new("worktree", &["worktree", "add", "nosuch/deep/dir"], Shape::Worktree));
    // Inside a tracked directory of the worktree itself, which is legal and
    // makes the new tree show up as untracked content under `src/`.
    out.push(Case::new("worktree", &["worktree", "add", "src/inner"], Shape::Worktree));
    // Inside the git directory. Stock allows it — the check is on the
    // *worktree* path, not on where it lands — and the registration under
    // `.git/worktrees/inside/` is what the probe then reads back.
    out.push(Case::new("worktree", &["worktree", "add", ".git/inside"], Shape::Worktree));
}

// ---------------------------------------------------------------------------
// 5. The other-worktree gate
// ---------------------------------------------------------------------------

/// A branch checked out in another working tree may not be checked out, force
/// created, force updated, rebased onto or handed to a new worktree.
///
/// # Why this is the group that matters
///
/// This is the only class of defect here that *corrupts*. Two working trees
/// sharing one branch leaves the second one's index and files describing a
/// commit its `HEAD` no longer names, and git prevents it in five separate
/// places — `die_if_checked_out()` for the checkout family,
/// `branch_checked_out()` for `branch`, `add_worktree()`'s own check for
/// `worktree add`. `switch_restore.rs` owns two of the entry points
/// (`switch linked`, `switch main` from inside `wt/`) and nothing owns the
/// rest.
///
/// Every one of these is `strict`, and that is a correction of a claim
/// `switch_restore.rs` records: it says the refusal "cannot match by
/// construction" because the message embeds the other tree's absolute path.
/// `runner::normalize` masks stderr against the side's own fixture root
/// (`runner.rs:5780`), and the other worktree is inside that root, so it does
/// match. Measured on stock 2.55.0 over [`Shape::Worktree`]:
///
/// ```text
/// git checkout linked      fatal: 'linked' is already used by worktree at '<REPO>/wt'
/// git -C wt checkout main  fatal: 'main' is already used by worktree at '<REPO>'
/// git checkout -B linked   fatal: 'linked' is already used by worktree at '<REPO>/wt'
/// git switch -C linked     fatal: 'linked' is already used by worktree at '<REPO>/wt'
/// git rebase main linked   fatal: 'linked' is already used by worktree at '<REPO>/wt'
/// git branch -f linked HEAD
///     fatal: cannot force update the branch 'linked' used by worktree at '<REPO>/wt'
/// git worktree add wtx linked
///     fatal: 'linked' is already used by worktree at '<REPO>/wt'
/// ```
///
/// All exit 128. Comparing the message is not decoration for this group: the
/// *reason* is the answer, and a port that refuses for the wrong reason (a
/// dirty tree, an unknown ref) has not implemented the gate.
///
/// The two legal rows are the other half. `--ignore-other-worktrees` and
/// `--detach` both peel past the gate, and a port that refuses them has
/// over-applied the check — which is the failure mode that looks like a pass on
/// every row above.
fn other_worktree_gate(out: &mut Vec<Case>) {
    // `worktree add` where the holder is a *linked* tree rather than the main
    // one: `add_worktree()` walks `.git/worktrees/*/HEAD`, a different loop from
    // the one that finds the main `HEAD`. `stateful_side_files.rs` covers
    // `add wtx main` on Linear, which only exercises the second loop.
    out.push(Case::strict("worktree", &["worktree", "add", "wtx", "linked"], Shape::Worktree));
    // Detaching at that same branch is legal: the new tree's HEAD names a
    // commit, not the ref, so nothing is shared.
    out.push(Case::new("worktree", &["worktree", "add", "--detach", "wtx", "linked"], Shape::Worktree));

    out.push(Case::strict("checkout", &["checkout", "linked"], Shape::Worktree));
    out.push(Case::strict("checkout", &["checkout", "main"], Shape::Worktree).in_dir("wt"));
    // Force-*create* over a held branch. `-B`/`-C` reset the ref, so they reach
    // the gate through the branch-creation path rather than the checkout path.
    out.push(Case::strict("checkout", &["checkout", "-B", "linked"], Shape::Worktree));
    out.push(Case::strict("switch", &["switch", "-C", "linked"], Shape::Worktree));
    // The escape hatch on `checkout`, which `switch_restore.rs` only pins for
    // `switch`.
    out.push(Case::new("checkout", &["checkout", "--ignore-other-worktrees", "linked"], Shape::Worktree));
    // `rebase <upstream> <branch>` checks `<branch>` out first, so a verb that
    // is not in the checkout family reaches the same gate.
    out.push(Case::strict("rebase", &["rebase", "main", "linked"], Shape::Worktree));
    // Force-updating the ref without checking it out anywhere. `branch -d`/`-D`
    // on the same ref are owned by `branch_remote.rs`; `-f` is the third gate in
    // `builtin/branch.c` and is unowned.
    out.push(Case::strict("branch", &["branch", "-f", "linked", "HEAD"], Shape::Worktree));
}

// ---------------------------------------------------------------------------
// 6. Administration: list, lock, move, prune, repair
// ---------------------------------------------------------------------------

/// The administrative verbs, on the corners the two owning files leave open.
///
/// Argument errors first — `list` has two mutually-exclusive pairs and neither
/// is exercised anywhere — then three vantage points and three states that only
/// [`Shape::WorktreeLocked`] can supply.
fn administration(out: &mut Vec<Case>) {
    // ---- list: the argument contract ----
    // Verified on stock 2.55.0, both exit 128:
    //   worktree list --porcelain -v
    //     fatal: options '--verbose' and '--porcelain' cannot be used together
    //   worktree list -z
    //     fatal: the option '-z' requires '--porcelain'
    out.push(Case::strict("worktree", &["worktree", "list", "--porcelain", "-v"], Shape::Worktree));
    out.push(Case::strict("worktree", &["worktree", "list", "-z"], Shape::Worktree));

    // ---- list: vantage points nobody stands at ----
    // From inside the administrative directory of the worktree whose own
    // *checkout* is gone. Discovery has to succeed from a git directory whose
    // `gitdir` file points nowhere, and the listing still has to name all four
    // trees with `wt-gone` marked prunable.
    out.push(
        Case::new("worktree", &["worktree", "list", "--porcelain"], Shape::WorktreeLocked)
            .in_dir(".git/worktrees/wt-gone"),
    );
    // `-v` from inside a linked tree that is neither the locked one nor the
    // gone one: the annotations are properties of the registry, not of where
    // the command runs, so all four rows and both annotations must still print.
    out.push(Case::new("worktree", &["worktree", "list", "-v"], Shape::WorktreeLocked).in_dir("wt-open"));

    // ---- lock ----
    // `lock --reason` on a tree that is not locked: `stateful_side_files.rs`
    // covers bare `lock wt`, which writes an *empty* `locked` file. The reason
    // is content, and `probe_worktrees` compares it.
    out.push(Case::new(
        "worktree",
        &["worktree", "lock", "--reason", "held-by-the-case", "wt"],
        Shape::Worktree,
    ));
    // Locking a tree that is already locked: stock refuses and echoes the
    // *existing* reason, so the message proves it read the file rather than
    // just noticing it exists.
    //   fatal: 'wt' is already locked, reason: held by the fixture
    out.push(Case::strict(
        "worktree",
        &["worktree", "lock", "--reason", "held-by-the-case", "wt"],
        Shape::WorktreeLocked,
    ));
    // `.` from inside a linked worktree resolves to *that* worktree and
    // succeeds, where `.` at the root names the main tree and is refused
    // (`stateful_side_files.rs` owns that one). The pair is the cwd-relative
    // resolution.
    out.push(Case::new("worktree", &["worktree", "lock", "."], Shape::Worktree).in_dir("wt"));
    // Unlocking the main working tree, which is the same refusal from the other
    // verb: `fatal: The main working tree cannot be locked or unlocked`.
    out.push(Case::strict("worktree", &["worktree", "unlock", "."], Shape::Worktree));

    // ---- move ----
    // Into a subdirectory of the worktree. The destination is not in
    // `.git/info/exclude`, so `status -uall` gains it, and
    // `.git/worktrees/wt/gitdir` has to be rewritten to the new location — a
    // file the digest compares byte for byte.
    out.push(Case::new("worktree", &["worktree", "move", "wt", "src/moved"], Shape::Worktree));
    // Moving a registration whose checkout is gone. Stock validates the source
    // first and dies naming the missing `.git`:
    //   fatal: validation failed, cannot move working tree:
    //     '<REPO>/wt-gone/.git' does not exist
    out.push(Case::strict("worktree", &["worktree", "move", "wt-gone", "elsewhere"], Shape::WorktreeLocked));

    // ---- prune: the keyword expiries ----
    // `--expire=all` and `--expire=never` are the two thresholds that are not a
    // clock reading, so they are the only ones this file can use (see the module
    // header). They bracket the default: `all` still removes only the
    // registration that is actually broken, and `never` removes nothing at all
    // — which is the row that catches a port ignoring the expiry and pruning on
    // brokenness alone.
    out.push(Case::new("worktree", &["worktree", "prune", "--expire=all", "-v"], Shape::WorktreeLocked));
    out.push(Case::new("worktree", &["worktree", "prune", "--expire=never", "-v"], Shape::WorktreeLocked));

    // ---- remove / repair ----
    // `--force` on the registration whose directory is gone.
    // `fixture_gaps2.rs` covers the unforced form.
    out.push(Case::new("worktree", &["worktree", "remove", "--force", "wt-gone"], Shape::WorktreeLocked));
    // `repair` on a path that is not a worktree at all, and on one that is
    // registered but gone. Both are `error:` on stderr with exit 1, and the
    // distinction between "not a valid path" and a successful repair is the
    // whole contract.
    out.push(Case::strict("worktree", &["worktree", "repair", "nosuchpath"], Shape::Worktree));
    out.push(Case::strict("worktree", &["worktree", "repair", "wt-gone"], Shape::WorktreeLocked));
}
