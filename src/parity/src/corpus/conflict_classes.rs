//! The conflict *classes* the merge engine has to tell apart, on the one shape
//! that can build them: [`Shape::MergeMatrix`].
//!
//! Every other merge module in this corpus asks what the engine does with a
//! *content* disagreement. `merge_ort.rs` ran a census over every commit
//! reachable from every ref of all 43 shapes and reported the reason: the whole
//! corpus held **two deletions and two renames**, all four on
//! [`Shape::Renamed`], which is strictly linear and so cannot be merged at all;
//! **no tree in any commit of any shape contained a `100755` blob**; **no path
//! was a blob in one commit and a tree in another**; and the single typechange
//! (`dir/target.txt` on [`Shape::Symlinks`]) is changed by one side only, so
//! there is nothing to disagree with. modify/delete, rename/rename, directory
//! rename, mode-only, file-becomes-directory and symlink-versus-file were
//! therefore not "untested" — they were unbuildable.
//!
//! [`Shape::MergeMatrix`] builds fourteen tips off one base, paired so that a
//! case can ask about rename/rename without also answering modify/delete.
//! `main` sits at the base commit of every pair.
//!
//! # How this divides territory with the merge modules already here
//!
//! The split is by *shape*, which makes it total and checkable: **every case in
//! this module runs on [`Shape::MergeMatrix`], and no case anywhere else does.**
//!
//! * [`super::merge_ort`] owns `git merge`'s option table, its message
//!   machinery and the `-s ort` spelling, on the shapes that have a content
//!   conflict. Its header records the five defects it could not express; four
//!   of them are pinned here — the three `merge.directoryRenames` rows in
//!   [`directory_rename_configuration`], and the file/directory and
//!   symlink/file classes in [`merge_tree_over_every_class`], which the port
//!   has since learnt to report and which these cases now hold in place.
//! * [`super::merge_strategies`] owns *which backend* the trees go to on
//!   [`Shape::CrissCross`]; here the same `-s`/`-X` grammar is asked over inputs
//!   that make the backends *disagree with each other* — `-s resolve` does no
//!   rename detection, so it completes a rebase `ort` refuses.
//! * [`super::merge_family`] owns the three-way text merge itself
//!   (`merge-file`, `merge-index`, the ll-merge driver). Nothing here produces a
//!   content hunk except [`forced_base_and_conflict_style`], which needs one to
//!   make `merge.conflictStyle` observable at all.
//! * [`super::patch_equivalence`] owns `merge-tree` on
//!   [`Shape::Renamed`]/[`Shape::CrissCross`]/[`Shape::Branched`], including a
//!   `merge.conflictStyle` sweep *without* `--merge-base` and a `--merge-base`
//!   sweep *without* `merge.conflictStyle`. The crossing of the two is neither
//!   module's, and is where the `|||||||` label defect
//!   [`forced_base_and_conflict_style`] records was found.
//! * [`super::merge_dirty`] owns the dirty-worktree gates;
//!   [`super::rebase_engine`] owns `rebase`'s own option table;
//!   [`super::rerere_engine`] owns `rerere.*` over a merge;
//!   [`super::sequences`] owns everything needing a second invocation.
//!
//! # Which classes this shape reaches, and the one it still does not
//!
//! | class | reachable | how |
//! |---|---|---|
//! | modify/delete | yes | `mm-mod` / `mm-del` on `mm/md.txt` |
//! | rename/rename, exact | yes | `mm-ren-a` / `mm-ren-b` on `mm/rr.txt` |
//! | rename/rename, **inexact** | yes | `mm-ren-a` / `mm-ren-edit`, scored `R055` |
//! | directory rename | yes | `mm-dir` / `mm-add` on `mm/old/` |
//! | mode-only | yes | `mm-mode` — the corpus's first `100755` blob |
//! | file becomes directory | yes | `mm-fd` / `mm-file` on `mm/fd` |
//! | symlink vs file | yes | `mm-reg` / `mm-link` on `mm/slink` |
//! | **rename/delete** | yes | `mm-ren-a` / `mm-ren-del` on `mm/rr.txt` |
//! | **add at a rename's destination** | yes | `mm-ren-a` / `mm-ren-add` on `mm/rr-a.txt` |
//! | **mode/mode** | **no** | see below |
//!
//! Six of those rows were reachable when this module was written. The other
//! three — the inexact rename, rename/delete, and the add at a rename's
//! destination — were not: the shape renamed two paths and deleted a third,
//! all disjoint,
//! and nothing added at any rename's destination. `fixture.rs` closed all three
//! with tips that act on `mm/rr.txt`, the path `mm-ren-a` and `mm-ren-b` already
//! rename — `mm-ren-del` deletes it, `mm-ren-add` adds an unrelated file at
//! `mm/rr-a.txt` (the *destination* of `mm-ren-a`'s rename), and `mm-ren-edit`
//! renames it to `mm/rr-e.txt` while rewriting four of its ten lines. Two of the
//! three change what this module can claim rather than only adding a row:
//!
//! * **The collision at a rename's destination is `add/add`, not `rename/add`.**
//!   Measured, not inferred from the shape of the inputs: `merge-ort.c` applies
//!   the rename first and then finds two independent additions at one path, so
//!   the report is `CONFLICT (add/add): Merge conflict in mm/rr-a.txt` with
//!   stages 2 and 3 and no stage 1. It is also the only conflicting pair here
//!   that `-X ours`/`-X theirs` can resolve, which is what turns the claim in
//!   [`strategy_options_over_every_class`] from one about the *flags* into one
//!   about the *classes*. See [`add_at_a_rename_destination`].
//! * **The inexact rename is what makes the similarity options live.** An exact
//!   rename is paired by object id before any score is computed, so
//!   `-X find-renames=` and `-X rename-threshold=` could previously only be
//!   inert controls here. `mm-ren-edit` scores `R055`: a rename at the default
//!   and at 55%, not one at 56%. The threshold now flips the answer in both
//!   directions, and the port's largest defect on this shape is in reading it —
//!   [`similarity_threshold_over_the_inexact_rename`].
//!
//! A **mode/mode** conflict — two sides setting two *different* modes on one
//! path — stays unreachable, and not for want of another branch. A regular file
//! has exactly two modes, `100644` and `100755`, so "two sides disagree about
//! the mode" is `mm-mode` against a tip that leaves the bit alone, which is a
//! mode change on one side only and merges clean. The third spelling of a mode
//! change is a typechange, and `mm-reg`/`mm-link` already covers that.
//!
//! # What `git merge` itself can and cannot do here, and why the verbs differ
//!
//! `main` is the *base* of every pair, so it is an ancestor of every tip and
//! a one-argument `git merge` can only fast-forward. A genuine two-head ort
//! merge from a single invocation is therefore not available on this shape, and
//! that is a real limit, not an oversight. What *is* available:
//!
//! * `git merge <a> <b>` — two heads, which `builtin/merge.c` sends to the
//!   **octopus** strategy. Stock fast-forwards to the first and then merges the
//!   second through `git merge-index git-merge-one-file`, a backend with no
//!   rename detection and no typechange handling at all. See
//!   [`merge_over_more_than_two_heads`].
//! * `git rebase <a> <b>` — checks out `b` and replays it onto `a`, one ort
//!   merge per commit, with `HEAD` at the *other* side of the pair.
//! * `git cherry-pick <a> <b>` — the same two trees, reached by picking `a` onto
//!   `main` (a clean fast-forward-equivalent commit) and then `b` onto that.
//! * `git merge-tree --write-tree <a> <b>` — the engine with no worktree and no
//!   commit, which is the only verb that can also *re-point the base*.
//!
//! `git stash apply` is **not** reachable: the shape carries no stash, and
//! `stash apply <commit>` refuses a commit that is not stash-like, so the verb
//! needs a second invocation and belongs to [`super::sequences`]. `git revert`
//! reaches the engine but cannot be made to *conflict* here — every `mm-*`
//! commit's parent is `main`, so reverting one onto `main` is always the
//! identity merge. Its cases below are clean-path pins over inputs (a `100755`
//! blob, a symlink typechange, a directory rename) the engine had never been
//! handed.
//!
//! # What the module finds
//!
//! 231 cases, **183 matching (79.2%)**, measured with
//! `--only merge,merge-tree,cherry-pick,revert,rebase --verbose` against
//! `target/debug/git` with `/usr/bin/git` (2.50.1) as the second oracle:
//!
//! | verb | cases | match | parity |
//! |---|---|---|---|
//! | `merge-tree` | 120 | 113 | 94.2% |
//! | `cherry-pick` | 38 | 17 | 44.7% |
//! | `rebase` | 35 | 17 | 48.6% |
//! | `merge` | 24 | 22 | 91.7% |
//! | `revert` | 14 | 14 | 100% |
//!
//! **All 48 failures are corroborated by the second oracle** — 2.50.1 gave
//! stock 2.55.0's answer byte for byte, so every one of them is the port's
//! difference and not a version difference. `unsupported`, `version-skew`,
//! `gits-disagree`, `interop-diff` and `zvcs-flaky` are **0** on this shape.
//!
//! The 85 cases the three new tips add account for 20 of the 48; the other 28
//! are exactly the failing set this module had before those tips existed.
//! Growing `mm/rr.txt` from three lines to ten to make a similarity score
//! land between thresholds moved every object id in the shape and changed **no
//! verdict**: this module, unmodified, was run against the previous
//! `fixture.rs` and the same `target/debug/git`, and produced the same 28
//! failing ids.
//!
//! They are **six** distinct defects, not 48. The rows below count the cases
//! each is *visible in* and they overlap on purpose — a
//! `rebase -c merge.directoryRenames=conflict` carries two of them — so the
//! column sums to more than 48:
//!
//! | defect | cases | verdict |
//! |---|---|---|
//! | `cherry-pick` collapses every class to `CONFLICT (content): Merge conflict in <path>` — rename/rename, rename/delete, file/directory and distinct-types alike; its `rebase` twin names all four correctly | 17 | `cherry-pick` |
//! | the `# Conflicts:` list in `MERGE_MSG` (and `rebase-merge/message`) names only *one* of the unmerged paths where stock names all of them, on an index whose stages agree entry for entry | 15 | `rebase`, `[STATE-DIFF]` |
//! | `-X find-renames=<n>`/`-X rename-threshold=<n>` loses an un-suffixed integer under `merge-tree` and `cherry-pick`, and reads it correctly under `rebase` | 7 | 6 `merge-tree`, 1 `cherry-pick` |
//! | `merge.directoryRenames` *is* read now — `false` is honoured everywhere — but the sequencer and rebase paths render the wrong report: `conflict`/`bogus` print `CONFLICT (add/add): Merge conflict in mm/new` for stock's `CONFLICT (file location): …`, and `true` drops the `Path updated:` line | 9 | 3 `cherry-pick`, 5 `rebase`, 1 `merge-tree` (`--quiet`, exit 1 against stock's 0) |
//! | `rebase -s resolve` runs ort anyway: stock's `resolve` backend has no rename detection, so it finishes two rebases the port stops with a conflict, and reports the third differently | 3 | `rebase` |
//! | `git merge` over two heads leaves no `REUC` (resolve-undo) extension in the index where stock records one | 2 | `merge`, `[STATE-DIFF]` |
//!
//! Two of the six are invisible to stdout entirely and exist only because the
//! runner probes the post-state: the `# Conflicts:` list and the missing `REUC`
//! extension. Both survived hand comparison of stdout, exit code, unmerged
//! stages and refs, because they are in none of those.
//!
//! Six defects this module used to record are **gone**, and their cases are
//! kept as the pins that say so: the extra `rebase: checkout <branch>` HEAD
//! reflog entry, the omitted `CONFLICT (file/directory)` line, the symlink
//! stages stacked at one path with the base tree returned, the `[UNSUPPORTED]`
//! refusal of the directory-rename class under `--messages`, the dropped
//! `--merge-base=` label on the `\|\|\|\|\|\|\|` line, and
//! `merge.renameLimit=nonsense` exiting 1 instead of 128.
//!
//! # Determinism
//!
//! Many of these commit, so their object ids are part of what is compared.
//! **Every one of the 231 cases was run stock-against-stock** —
//! `--bin /opt/homebrew/bin/git --only merge,merge-tree,cherry-pick,rebase,revert`,
//! which puts git 2.55.0 on both sides of the comparison in two independent
//! `cp -Rp` copies of the shape under [`crate::env::harden`] and judges them on
//! everything the differential run judges: stdout, exit code, the full state
//! probe and the interop probe. That run is `1072/1072 matched (100%)` with a
//! single exclusion, `branched::rebase::rebase --ignore-date HEAD~1`, which is
//! another module's case on another shape and is excluded because it reads a
//! clock. Nothing on [`Shape::MergeMatrix`] is nondeterministic, and nothing on
//! it is excluded.
//!
//! `cp -Rp` is deliberate, for the reason [`super::merge_ort`] gives:
//! [`crate::fixture::copy_tree`] carries mtimes across and the shapes set
//! `core.checkStat=minimal`, so a copy that dropped the timestamps would make
//! the trivial in-index path in `builtin/merge.c` fail on both sides.
//!
//! Nothing here reads a clock, a random source or an absolute path — the argvs
//! are literals, the two stdin payloads are `&'static [u8]`, and the only
//! configuration touched is `merge.*`/`diff.*`.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    merge_tree_over_every_class(out);
    directory_rename_configuration(out);
    cherry_pick_over_every_class(out);
    rebase_over_every_class(out);
    revert_over_every_class(out);
    merge_over_more_than_two_heads(out);
    strategy_options_over_every_class(out);
    forced_base_and_conflict_style(out);
    rename_delete_over_every_verb(out);
    add_at_a_rename_destination(out);
    inexact_rename_over_every_verb(out);
    similarity_threshold_over_the_inexact_rename(out);
}

/// Push one case per argv against [`Shape::MergeMatrix`].
fn each(cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for args in argvs {
        out.push(Case::new(cmd, args, Shape::MergeMatrix));
    }
}

// ---------------------------------------------------------------------------
// merge-tree: the engine with no worktree and no commit
// ---------------------------------------------------------------------------

/// Each of the six pairs handed straight to `merge-tree --write-tree`.
///
/// This is the narrowest possible view of the engine: no index to update, no
/// worktree to write, no commit to name — just the merged tree, the unmerged
/// stages, and the conflict report. A divergence here is the engine's, not the
/// verb's, which is why the same pairs are then asked again through
/// `cherry-pick` and `rebase` below.
///
/// All six agreed when last measured, which is a change: three of them —
/// `mm-dir mm-add`, `mm-fd mm-file` and `mm-reg mm-link` — were the module's
/// three worst `merge-tree` divergences, and the cases are kept as the pins
/// that say the engine now answers them. `mm-fd mm-file` records the file side
/// at `mm/fd~mm-file` with both conflict lines, and `mm-reg mm-link` renames one
/// of the two types rather than stacking three stages at one path and returning
/// the base tree.
///
/// `--messages` is not redundant with the default. Stock prints the report
/// either way; the port used to take a different path under the explicit flag
/// and refuse the directory-rename class outright, so the two spellings measure
/// different things even when they now agree.
///
/// `--name-only`, `--no-messages`, `-z` and `--quiet` each drop a different part
/// of the record, which is what separates "the report is wrong" from "the tree
/// is wrong". **`--quiet` on `mm-dir mm-add` is the one that still diverges**,
/// and only in the exit code: stock exits **0** there while the same merge
/// without the flag exits 1 — a directory-rename conflict is reported but does
/// not make `--quiet` fail — and the port exits **1**. Nothing else about the
/// two runs differs, which is why the flag needs a case of its own.
///
/// The last three argvs are the near misses named in the module header:
/// `mm-ren-a mm-del` renames and deletes *different* paths, and `mm-dir mm-del`
/// crosses a directory rename with an unrelated deletion. Neither is a
/// rename/delete conflict, and both agreeing is what makes the claim in the
/// header checkable rather than asserted.
fn merge_tree_over_every_class(out: &mut Vec<Case>) {
    each(
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "mm-mod", "mm-del"],
            &["merge-tree", "--write-tree", "--messages", "mm-mod", "mm-del"],
            &["merge-tree", "--write-tree", "--name-only", "mm-mod", "mm-del"],
            &["merge-tree", "--write-tree", "mm-ren-a", "mm-ren-b"],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-a", "mm-ren-b"],
            &["merge-tree", "--write-tree", "--name-only", "mm-ren-a", "mm-ren-b"],
            &["merge-tree", "--write-tree", "mm-dir", "mm-add"],
            &["merge-tree", "--write-tree", "--messages", "mm-dir", "mm-add"],
            &["merge-tree", "--write-tree", "--name-only", "mm-dir", "mm-add"],
            &["merge-tree", "--write-tree", "--quiet", "mm-dir", "mm-add"],
            &["merge-tree", "--write-tree", "-z", "mm-dir", "mm-add"],
            &["merge-tree", "--write-tree", "mm-mode", "mm-mod"],
            &["merge-tree", "--write-tree", "--messages", "mm-mode", "mm-mod"],
            &["merge-tree", "--write-tree", "mm-mode", "mm-fd"],
            &["merge-tree", "--write-tree", "mm-fd", "mm-file"],
            &["merge-tree", "--write-tree", "--messages", "mm-fd", "mm-file"],
            &["merge-tree", "--write-tree", "--name-only", "mm-fd", "mm-file"],
            &["merge-tree", "--write-tree", "mm-reg", "mm-link"],
            &["merge-tree", "--write-tree", "--messages", "mm-reg", "mm-link"],
            &["merge-tree", "--write-tree", "--no-messages", "mm-reg", "mm-link"],
            &["merge-tree", "--write-tree", "-z", "mm-reg", "mm-link"],
            &["merge-tree", "--trivial-merge", "main", "mm-mod", "mm-del"],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-a", "mm-del"],
            &["merge-tree", "--write-tree", "--messages", "mm-dir", "mm-del"],
        ],
        out,
    );

    // `--stdin` is one process answering two merges, and the leading status
    // column is the part only this mode has: stock prints `0` for the
    // directory-rename record — it reports the conflict in the message field
    // and still calls the merge complete — where the same merge as an ordinary
    // invocation exits 1. The port used to print `1` there and lose the
    // record's conflicted-file block; both agree now, and the case is what says
    // so.
    out.push(Case::with_stdin(
        "merge-tree",
        &["merge-tree", "--stdin"],
        Shape::MergeMatrix,
        b"mm-dir mm-add\nmm-reg mm-link\n",
    ));
    out.push(Case::with_stdin(
        "merge-tree",
        &["merge-tree", "--stdin", "-z"],
        Shape::MergeMatrix,
        b"mm-fd mm-file\nmm-mod mm-del\n",
    ));
}

// ---------------------------------------------------------------------------
// `merge.directoryRenames`: read now, reported wrong by two of the three verbs
// ---------------------------------------------------------------------------

/// The three settings of `merge.directoryRenames`, through every verb that
/// reaches the engine, plus the controls that agree.
///
/// **The defect this group exists to pin.** `mm-dir` renames `mm/old/` to
/// `mm/new/`; `mm-add` adds `mm/old/c.txt` into the old name. Stock's answer is
/// a function of the key, and the port's is too **under `merge-tree`** — all
/// four values agree there, including the tree, which is a change from when
/// this group was written and the port behaved as `true` whatever the key said.
/// What is left is a reporting defect confined to the two verbs that carry an
/// index. Measured by hand on the shape, stock 2.55.0 and git 2.50.1 agreeing:
///
/// | setting | stock | port under `cherry-pick`/`rebase` |
/// |---|---|---|
/// | `false` | `mm/old/c.txt` kept, exit 0 | same, byte for byte |
/// | `conflict` (default) | `CONFLICT (file location): mm/old/c.txt added in b606dbf … suggesting it should perhaps be moved to mm/new/c.txt.`, exit 1 | `CONFLICT (add/add): Merge conflict in mm/new`, exit 1 |
/// | `true` | `Path updated: … moving it to mm/new/c.txt.`, exit 0 | the line **missing**, same commit otherwise |
/// | `bogus` | rejected the same way `conflict` is reported | as `conflict` above |
///
/// The `false` row is what makes the finding specific rather than "the key is
/// ignored": it is honoured, and the same merge under the same key prints a
/// class that is not the class git found. The port names `mm/new`, a
/// *directory*, as the conflicted path; stock names `mm/new/c.txt`.
///
/// **The controls are the point of the group, not decoration.** `-X
/// no-renames`, `merge.renames=false` and `diff.renames=false` each turn the
/// same detection off, and the port then agrees with stock byte for byte under
/// all three verbs — measured, not assumed. That localises what is left to the
/// report and not to the detection.
///
/// `merge.renames=true` is the affirmative spelling rather than a control; it
/// leaves detection on, so the merge is the default one and the case lands on
/// the `--messages` refusal with the rest.
///
/// `bogus` is here because an unparsable value is the one input that separates
/// "read and misapplied" from "never read": stock rejects it the same way it
/// rejects it under `merge`, and a port that never looks the key up cannot.
///
/// Two entries are delivered from [`ConfigScope::Repo`] rather than `-c`. The
/// key is read by `merge-ort.c` through the ordinary config sequence, so if the
/// port had a command-line-only reader those two would be the ones that showed
/// it; they fail identically to their `-c` twins, which says the gap is in the
/// consumer and not in the delivery.
fn directory_rename_configuration(out: &mut Vec<Case>) {
    for value in ["false", "conflict", "true", "bogus"] {
        for args in [
            &["merge-tree", "--write-tree", "--messages", "mm-dir", "mm-add"][..],
            &["cherry-pick", "mm-dir", "mm-add"][..],
            &["rebase", "mm-dir", "mm-add"][..],
        ] {
            let cmd = match args[0] {
                "merge-tree" => "merge-tree",
                "cherry-pick" => "cherry-pick",
                _ => "rebase",
            };
            out.push(
                Case::new(cmd, args, Shape::MergeMatrix)
                    .with_config(&[("merge.directoryRenames", value)]),
            );
        }
    }

    // The octopus path reaches the same detection: stock's `git-merge-one-file`
    // backend has none, so it keeps `mm/old/c.txt` whatever the key says, and
    // the port moves it.
    for args in [
        &["merge", "--no-commit", "mm-dir", "mm-add"][..],
        &["merge", "--squash", "mm-dir", "mm-add"][..],
    ] {
        out.push(Case::new("merge", args, Shape::MergeMatrix));
        out.push(
            Case::new("merge", args, Shape::MergeMatrix)
                .with_config(&[("merge.directoryRenames", "false")]),
        );
    }

    // Delivered from `.git/config` instead of the command line.
    for (cmd, args) in [
        ("merge-tree", &["merge-tree", "--write-tree", "--messages", "mm-dir", "mm-add"][..]),
        ("cherry-pick", &["cherry-pick", "mm-dir", "mm-add"][..]),
    ] {
        out.push(Case::new(cmd, args, Shape::MergeMatrix).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Repo, "merge.directoryRenames", "false"),
        ]));
    }

    // The controls: three other ways to switch the same detection off, all of
    // which the port honours.
    each(
        "merge-tree",
        &[&["merge-tree", "--write-tree", "--messages", "-X", "no-renames", "mm-dir", "mm-add"]],
        out,
    );
    each("cherry-pick", &[&["cherry-pick", "-X", "no-renames", "mm-dir", "mm-add"]], out);
    each("rebase", &[&["rebase", "-X", "no-renames", "mm-dir", "mm-add"]], out);
    for (key, value) in [
        ("merge.renames", "false"),
        ("merge.renames", "true"),
        ("diff.renames", "false"),
    ] {
        out.push(
            Case::new(
                "merge-tree",
                &["merge-tree", "--write-tree", "--messages", "mm-dir", "mm-add"],
                Shape::MergeMatrix,
            )
            .with_config(&[(key, value)]),
        );
    }
    out.push(
        Case::new("cherry-pick", &["cherry-pick", "mm-dir", "mm-add"], Shape::MergeMatrix)
            .with_config(&[("merge.renames", "false")]),
    );
}

// ---------------------------------------------------------------------------
// cherry-pick: the engine reached from the sequencer
// ---------------------------------------------------------------------------

/// Every class again, picked onto `main` two commits at a time.
///
/// `main` is the base of every pair, so `cherry-pick <a> <b>` applies `a`
/// cleanly — the commit it writes has `a`'s tree — and then merges `b`'s change
/// against it. That is the same three trees `merge-tree <a> <b>` sees, reached
/// through `sequencer.c` with an index and a worktree behind it, and the answers
/// are **not** the same. `merge-tree` agrees with stock on every class the shape
/// builds; `cherry-pick` collapses all of them to
/// `CONFLICT (content): Merge conflict in <path>` — rename/rename,
/// rename/delete, file/directory and distinct types alike — where stock names
/// the class, and it does so on a path that is sometimes in neither side's
/// tree (`mm/rr.txt` for a rename/delete stock reports at `mm/rr-a.txt`).
/// Both spellings of each pair are here because the
/// conflicted path is named after the side it came from, so reversing the order
/// changes the bytes under test (`mm/slink~HEAD` versus
/// `mm/slink~e0e7093 (merge-matrix: slink becomes a file)`) rather than
/// repeating them.
///
/// `-s resolve` is the discriminator this group contributes that no other does:
/// the port honours it here — stock and port both print `Trying simple merge.`
/// and complete the rename/rename pick — and ignores it under `rebase`. A
/// single module measuring only one of the two verbs would have called that
/// option supported.
///
/// `-n` keeps the pick in the index without committing, which is what makes the
/// *stages* the whole of the answer; on `mm-fd mm-file` stock leaves two
/// conflict lines and the port leaves one.
fn cherry_pick_over_every_class(out: &mut Vec<Case>) {
    each(
        "cherry-pick",
        &[
            &["cherry-pick", "mm-mod", "mm-del"],
            &["cherry-pick", "-n", "mm-mod", "mm-del"],
            &["cherry-pick", "mm-ren-a", "mm-ren-b"],
            &["cherry-pick", "mm-ren-b", "mm-ren-a"],
            &["cherry-pick", "mm-dir", "mm-add"],
            &["cherry-pick", "mm-fd", "mm-file"],
            &["cherry-pick", "-n", "mm-fd", "mm-file"],
            &["cherry-pick", "mm-file", "mm-fd"],
            &["cherry-pick", "mm-reg", "mm-link"],
            &["cherry-pick", "mm-link", "mm-reg"],
            &["cherry-pick", "mm-mode", "mm-mod"],
            &["cherry-pick", "mm-mode", "mm-fd"],
            &["cherry-pick", "--strategy=resolve", "mm-ren-a", "mm-ren-b"],
            &["cherry-pick", "-s", "resolve", "mm-fd", "mm-file"],
            &["cherry-pick", "-s", "resolve", "mm-reg", "mm-link"],
            &["cherry-pick", "-X", "ours", "mm-mod", "mm-del"],
            &["cherry-pick", "-X", "theirs", "mm-reg", "mm-link"],
            &["cherry-pick", "-X", "no-renames", "mm-ren-a", "mm-ren-b"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// rebase: the engine reached with HEAD on the other side of the pair
// ---------------------------------------------------------------------------

/// `rebase <a> <b>` checks out `b` and replays its one commit onto `a`.
///
/// Structurally the same merge as the matching `cherry-pick`, and deliberately
/// duplicated, because the port does not implement them the same way. Two
/// disagreements between the two verbs are visible only by having both:
///
/// * **rename/rename.** `rebase mm-ren-a mm-ren-b` reproduces stock's
///   `CONFLICT (rename/rename)` exactly; `cherry-pick mm-ren-a mm-ren-b` does
///   not. The engine can spell the class — the sequencer path does not ask it
///   to.
/// * **`-s resolve`.** `rebase -s resolve mm-ren-a mm-ren-b` is a *stock
///   success* — `resolve` does no rename detection, so both files land and the
///   rebase completes at exit 0 — and a **port failure**: it runs ort anyway,
///   raises rename/rename and stops the rebase. That is a rebase the port
///   refuses to finish and stock finishes, on an option the same port honours
///   under `cherry-pick`.
///
/// `--onto mm-dir main mm-add` is the three-argument form reaching the same
/// directory-rename detection through a different argument parse;
/// `--merge` names the backend the default already uses, which is what makes a
/// port that treats it as an unknown option visible.
///
/// **A defect no stdout comparison finds, and every conflicting case here
/// carries it.** The runner's state probe reads the files a stopped rebase
/// leaves behind, and the port's `# Conflicts:` list is short. On
/// `rebase mm-ren-a mm-ren-b` the two indexes hold the same three unmerged
/// entries, entry for entry, and stock's `.git/MERGE_MSG` (and its copy at
/// `.git/rebase-merge/message`) is
///
/// ```text
/// merge-matrix: rename rr.txt to rr-b.txt
///
/// # Conflicts:
/// #	mm/rr-a.txt
/// #	mm/rr-b.txt
/// #	mm/rr.txt
/// ```
///
/// where the port lists `mm/rr-a.txt` alone. It is the sole cause of most of
/// the failures in this group, and it survives every comparison of stdout,
/// exit code, unmerged stages and refs because it is in none of them. It is
/// visible on `cherry-pick` too, where it is masked by the class-name defect in
/// [`cherry_pick_over_every_class`]; `rebase` is where it is the *only* thing
/// wrong, and that is what makes it a finding rather than a symptom.
///
/// An earlier defect this group carried — an extra `rebase: checkout <branch>`
/// HEAD reflog entry the port wrote before `rebase (start):` — is gone; the two
/// reflogs now agree entry for entry.
fn rebase_over_every_class(out: &mut Vec<Case>) {
    each(
        "rebase",
        &[
            &["rebase", "mm-mod", "mm-del"],
            &["rebase", "mm-del", "mm-mod"],
            &["rebase", "mm-ren-a", "mm-ren-b"],
            &["rebase", "mm-ren-b", "mm-ren-a"],
            &["rebase", "mm-dir", "mm-add"],
            &["rebase", "mm-fd", "mm-file"],
            &["rebase", "mm-file", "mm-fd"],
            &["rebase", "mm-reg", "mm-link"],
            &["rebase", "mm-link", "mm-reg"],
            &["rebase", "mm-mode", "mm-mod"],
            &["rebase", "mm-mode", "mm-reg"],
            &["rebase", "--onto", "mm-dir", "main", "mm-add"],
            &["rebase", "--merge", "mm-mod", "mm-del"],
            &["rebase", "-s", "resolve", "mm-ren-a", "mm-ren-b"],
            &["rebase", "-s", "resolve", "mm-fd", "mm-file"],
            &["rebase", "-X", "ours", "mm-mod", "mm-del"],
            &["rebase", "-X", "theirs", "mm-reg", "mm-link"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// revert: the engine on inputs it had never been handed
// ---------------------------------------------------------------------------

/// Reverts over the six classes, all of which are **clean** — and the module
/// header says why they have to be: every `mm-*` commit's parent is `main`, so
/// reverting one while `HEAD` is at `main` merges a tree against itself and can
/// never conflict. No amount of argument juggling changes that, so a conflicting
/// revert on this shape is not available and is not faked.
///
/// They are here anyway because the *inputs* are new to this verb even when the
/// outcome is not: before [`Shape::MergeMatrix`] no revert in the corpus had
/// ever undone a `100755` mode bit, a symlink-to-regular-file typechange, a
/// file-to-directory change or a whole-directory rename, and each of those is a
/// path through `merge-ort.c` that a content-only fixture cannot enter. All
/// agree today; the value is that a regression in any of them now has somewhere
/// to be caught.
fn revert_over_every_class(out: &mut Vec<Case>) {
    each(
        "revert",
        &[
            &["revert", "--no-edit", "mm-mod"],
            &["revert", "--no-edit", "mm-del"],
            &["revert", "--no-edit", "mm-mode"],
            &["revert", "--no-edit", "-n", "mm-dir"],
            &["revert", "--no-edit", "-n", "mm-add"],
            &["revert", "--no-edit", "-n", "mm-fd"],
            &["revert", "--no-edit", "-n", "mm-file"],
            &["revert", "--no-edit", "-n", "mm-reg"],
            &["revert", "--no-edit", "-n", "mm-link"],
            &["revert", "--no-edit", "-X", "theirs", "-n", "mm-dir"],
            &["revert", "--no-edit", "-n", "mm-ren-a", "mm-ren-b"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// `git merge` itself: two heads, which means the octopus backend
// ---------------------------------------------------------------------------

/// `git merge <a> <b>` on a `HEAD` that is the base of both.
///
/// This is the only spelling of `git merge` that reaches a conflict on this
/// shape (see the module header), and it does so through a backend the rest of
/// the corpus barely touches. Stock fast-forwards to `<a>` and then hands the
/// second head to `git merge-index git-merge-one-file`, which has **no rename
/// detection, no directory-rename detection and no typechange handling**. The
/// port used to answer every pair with merge-ort instead and disagree on nearly
/// all of them; it now reproduces the octopus backend, down to
/// `Simple merge did not work, trying automatic merge.`, `git-merge-one-file`'s
/// own `ERROR: … Not handling case …` on stderr and stock's exit code. The
/// pairs are kept because that agreement is the thing worth pinning: a port
/// that quietly upgraded the two-head path to ort would commit a *different
/// tree* here — on `mm-ren-a mm-ren-b` stock commits both `mm/rr-a.txt` and
/// `mm/rr-b.txt` at exit 0, where ort raises rename/rename and stops.
///
/// **What is left is in the index rather than in the report.** `merge mm-fd
/// mm-file` and its `-s octopus` twin agree on stdout, exit code, refs and
/// worktree, and the port's index carries no `REUC` extension where stock's
/// records `mm/fd/inside.txt=0|100644:d9930cba…|0` — 920 index bytes against
/// 864. It is a record the port never writes rather than one it writes
/// differently, and no stdout comparison can see it; it was found by the
/// runner's index probe.
///
/// The three `-s` cases are the refusals, and they are `strict` because the
/// refusal *is* the whole behaviour: `ort`, `resolve` and `recursive` each
/// handle exactly two trees, so a third head is
/// `error: Not handling anything other than two heads merge.` at exit 2. All
/// three agree byte for byte today, which is what makes them worth pinning —
/// they are the boundary the octopus cases sit just outside of.
///
/// The `--no-ff -m merged mm-dir` group is three spellings of one merge whose
/// *summary* is the thing under test: `--stat` renders
/// `mm/{old => new}/a.txt | 0` with `rename mm/{old => new}/a.txt (100%)`,
/// `--summary` renders the rename lines alone, and `--no-stat` renders neither.
/// The port used to emit four independent create/delete lines instead of two
/// renames, and to leave the emptied `mm/old/` behind in the worktree after the
/// directory rename; both are gone, and these are the cases that would catch
/// either coming back — the second only because the state probe walks the
/// worktree, where an empty directory is invisible to git itself.
fn merge_over_more_than_two_heads(out: &mut Vec<Case>) {
    each(
        "merge",
        &[
            &["merge", "mm-mod", "mm-del"],
            &["merge", "mm-ren-a", "mm-ren-b"],
            &["merge", "mm-dir", "mm-add"],
            &["merge", "mm-fd", "mm-file"],
            &["merge", "mm-reg", "mm-link"],
            &["merge", "mm-mode", "mm-mod"],
            &["merge", "-s", "octopus", "mm-fd", "mm-file"],
            &["merge", "--squash", "mm-mod", "mm-del"],
            &["merge", "-X", "ours", "mm-mod", "mm-del"],
            &["merge", "--no-ff", "-m", "merged", "mm-dir"],
            &["merge", "--no-ff", "-m", "merged", "mm-mode"],
            &["merge", "--stat", "--no-ff", "-m", "merged", "mm-dir"],
            &["merge", "--no-stat", "--no-ff", "-m", "merged", "mm-dir"],
            &["merge", "--summary", "--no-ff", "-m", "merged", "mm-dir"],
        ],
        out,
    );
    for args in [
        &["merge", "-s", "ort", "mm-mod", "mm-del"][..],
        &["merge", "-s", "resolve", "mm-mod", "mm-del"][..],
        &["merge", "-s", "recursive", "mm-ren-a", "mm-ren-b"][..],
    ] {
        out.push(Case::strict("merge", args, Shape::MergeMatrix));
    }
}

// ---------------------------------------------------------------------------
// The `-X` grammar and the rename-detection keys, over these classes
// ---------------------------------------------------------------------------

/// `-X` and the `merge.rename*` keys asked of inputs where the answer is a
/// *class*, not a hunk.
///
/// Two findings are recorded here rather than guessed at, because they bound
/// what this shape can measure:
///
/// * **`-X ours`/`-X theirs` change nothing on any of the five conflicting
///   pairs below.** Both are content-level resolutions in `merge-ort.c`; a
///   modify/delete, a rename/rename, a distinct-types or a file/directory
///   conflict is not a content conflict, so the flag is parsed and then has
///   nothing to apply to. Every `-X ours`/`-X theirs` case below therefore
///   answers *exactly as its unadorned twin does* — which is the finding: a
///   port that let `-X theirs` swallow a modify/delete would show up here and
///   does not. The claim is about the classes, not about the flags, and
///   [`add_at_a_rename_destination`] is the control that proves it: on an
///   add/add the same two flags resolve the merge at exit 0 and write two
///   different trees.
/// * **`-X find-renames=`/`-X rename-threshold=`/`merge.renameLimit` cannot
///   flip a rename on *these* pairs.** Every rename in them is exact — `git mv`
///   with no edit, so a `100%` similarity match — and exact renames are found
///   by the pairing pass before any similarity score or limit is consulted.
///   The four cases that set them agree with their unadorned twins on both
///   sides. That is what makes them the control for
///   [`similarity_threshold_over_the_inexact_rename`], where the same options
///   are asked of a rename that has to be *scored* and the port loses one
///   spelling of the value: a fix that only made these four pass would have
///   fixed nothing.
///
/// `merge.renameLimit=nonsense` is the one value that is not inert:
/// `fatal: bad numeric config value 'nonsense' for 'merge.renamelimit': invalid
/// unit` at exit **128**, which the port now reproduces — it used to exit 1.
/// `merge_ort` records the same validation gap under `merge`; this is the
/// `merge-tree` half, a different entry point into `git_config`.
fn strategy_options_over_every_class(out: &mut Vec<Case>) {
    for option in ["ours", "theirs"] {
        for pair in [
            ["mm-mod", "mm-del"],
            ["mm-ren-a", "mm-ren-b"],
            ["mm-dir", "mm-add"],
            ["mm-fd", "mm-file"],
            ["mm-reg", "mm-link"],
        ] {
            out.push(Case::new(
                "merge-tree",
                &[
                    "merge-tree",
                    "--write-tree",
                    "--messages",
                    "-X",
                    option,
                    pair[0],
                    pair[1],
                ],
                Shape::MergeMatrix,
            ));
        }
    }
    each(
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "--messages", "-X", "no-renames", "mm-ren-a", "mm-ren-b"],
            &["merge-tree", "--write-tree", "--messages", "-X", "no-renames", "mm-fd", "mm-file"],
            &["merge-tree", "--write-tree", "--messages", "-X", "no-renames", "mm-reg", "mm-link"],
            &["merge-tree", "--write-tree", "--messages", "-X", "find-renames=90%", "mm-dir", "mm-add"],
            &["merge-tree", "--write-tree", "--messages", "-X", "find-renames=90%", "mm-ren-a", "mm-ren-b"],
            &["merge-tree", "--write-tree", "--messages", "-X", "rename-threshold=25", "mm-dir", "mm-add"],
            &["merge-tree", "--write-tree", "--messages", "-X", "rename-threshold=25", "mm-ren-a", "mm-ren-b"],
        ],
        out,
    );
    for (key, value) in [
        ("merge.renameLimit", "1"),
        ("merge.renameLimit", "nonsense"),
        ("merge.renames", "false"),
    ] {
        out.push(
            Case::new(
                "merge-tree",
                &["merge-tree", "--write-tree", "--messages", "mm-ren-a", "mm-ren-b"],
                Shape::MergeMatrix,
            )
            .with_config(&[(key, value)]),
        );
    }
}

// ---------------------------------------------------------------------------
// A forced base, and the only content conflict this shape can produce
// ---------------------------------------------------------------------------

/// `merge-tree --merge-base=` pointed at one of the tips, which was the only
/// way to get a **content** conflict out of the six original pairs — and so the
/// only way to make `merge.conflictStyle` mean anything here.
///
/// None of those six disagrees about the *bytes* of a file: `mm/rr.txt` is
/// moved and never edited, `mm/md.txt` is edited on one side and removed on the
/// other, and the rest are type or mode changes. Re-pointing the base changes
/// that. With `--merge-base=mm-fd`, where `mm/fd` is a directory and so has no
/// blob at all, `mm-file` and `main` both *add* `mm/fd` with different content
/// and stock answers `CONFLICT (add/add): Merge conflict in mm/fd` over a real
/// conflicted blob. (`mm-ren-add` has since given the shape a second content
/// conflict that needs no forced base at all — see
/// [`add_at_a_rename_destination`] — which is what makes the two spellings
/// separable rather than one dimension measured twice.)
///
/// **The defect that used to reach.** With `merge.conflictStyle=diff3` (and
/// identically `zdiff3`) stock labels the base section with the name it was
/// given on the command line, and the port left the label empty:
///
/// ```text
/// stock: <<<<<<< mm-file\nstill a file, edited\n||||||| mm-fd\n=======\n…
/// port:  <<<<<<< mm-file\nstill a file, edited\n|||||||\n=======\n…
/// ```
///
/// so the merged trees differed while the `merge` style agreed byte for byte.
/// It was specific to a base named on the command line: the port got
/// `||||||| 03c866d` right for a computed base then and does now, and the two
/// spellings agree today. [`super::patch_equivalence`] sweeps
/// `merge.conflictStyle` without `--merge-base` and sweeps `--merge-base`
/// without `merge.conflictStyle`; the crossing is neither module's, which is
/// why the defect survived both and why these cases stay.
///
/// The `--merge-base=mm-reg` pair is the same trick over a **symlink**: the
/// base is a regular file, both sides are symlinks with different targets, and
/// git will not merge symlink content, so the report is
/// `CONFLICT (content): Merge conflict in mm/slink` with all three stages
/// recorded and no marker block to render. `--merge-base=mm-mode` puts a
/// `100755` blob in stage 1, which no case in the corpus had done.
fn forced_base_and_conflict_style(out: &mut Vec<Case>) {
    each(
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "--messages", "--merge-base=mm-fd", "mm-file", "main"],
            &["merge-tree", "--write-tree", "--messages", "--merge-base=mm-reg", "mm-link", "main"],
            &["merge-tree", "--write-tree", "--messages", "--merge-base=mm-mode", "mm-reg", "mm-link"],
            &["merge-tree", "--write-tree", "--messages", "--merge-base=main", "mm-fd", "mm-file"],
            &["merge-tree", "--write-tree", "--messages", "--no-merge-base", "mm-fd", "mm-file"],
            &["merge-tree", "--write-tree", "--name-only", "--merge-base=mm-fd", "mm-file", "main"],
        ],
        out,
    );
    for style in ["merge", "diff3", "zdiff3"] {
        for args in [
            &["merge-tree", "--write-tree", "--messages", "--merge-base=mm-fd", "mm-file", "main"][..],
            &["merge-tree", "--write-tree", "--messages", "--merge-base=mm-reg", "mm-link", "main"][..],
        ] {
            out.push(
                Case::new("merge-tree", args, Shape::MergeMatrix)
                    .with_config(&[("merge.conflictStyle", style)]),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// rename/delete: one side renames `P`, the other deletes it
// ---------------------------------------------------------------------------

/// `mm-ren-del` deletes `mm/rr.txt`, which `mm-ren-a` and `mm-ren-b` rename.
///
/// The class the module header used to record as unreachable. The old shape
/// renamed two paths and deleted a third, all disjoint, so the pairing could
/// only ever produce rename/rename or an unrelated deletion; the tip added to
/// [`Shape::MergeMatrix`] deletes a path that is *already* renamed by two other
/// tips, which is the whole of what the class needs. Stock's answer, measured:
///
/// ```text
/// f964a3a9e78e9c2cde983fc571e818f32e31ce32
/// 100644 f9a9686a62283ad7aecdf804cae37ec4f0d3a02a 1	mm/rr-a.txt
/// 100644 f9a9686a62283ad7aecdf804cae37ec4f0d3a02a 2	mm/rr-a.txt
///
/// CONFLICT (rename/delete): mm/rr.txt renamed to mm/rr-a.txt in mm-ren-a, but deleted in mm-ren-del.
/// ```
///
/// Stages 1 and 2 both at the *destination* and no stage 3 is the shape of the
/// class: the deletion has no side to record, and the base entry is carried
/// forward under the new name. The port reproduces it byte for byte through
/// `merge-tree`, `merge` and default `rebase` — including the `# Conflicts:`
/// list, which has only one path to get right here — and does not through
/// `cherry-pick`, which is the same split the module already records for
/// rename/rename and file/directory, now measured on a third class.
///
/// `-X ours` and `-X theirs` are here for the reason the header gives: a
/// rename/delete is not a content disagreement, so both flags parse and then
/// have nothing to apply to, and stock still reports the conflict under each.
/// `-X no-renames` is the control that turns the class off entirely — with no
/// rename detected the merge is "one side deleted a file the other did not
/// touch", which resolves clean at exit 0.
///
/// `rebase -s resolve mm-ren-a mm-ren-del` is the one divergence this group
/// contributes that no other case in the module has: `resolve` does no rename
/// detection, so stock sees `mm-ren-del`'s patch as already applied —
/// `dropping bb51a921… merge-matrix: delete rr.txt -- patch contents already
/// upstream` — and finishes the rebase at exit 0. The port runs ort anyway,
/// raises rename/delete and stops. It is the twin of the `-s resolve`
/// rename/rename failure in [`rebase_over_every_class`], on a class that did
/// not exist when that one was written.
fn rename_delete_over_every_verb(out: &mut Vec<Case>) {
    each(
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "mm-ren-a", "mm-ren-del"],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-a", "mm-ren-del"],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-del", "mm-ren-a"],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-b", "mm-ren-del"],
            &["merge-tree", "--write-tree", "--name-only", "mm-ren-a", "mm-ren-del"],
            &["merge-tree", "--write-tree", "--quiet", "mm-ren-a", "mm-ren-del"],
            &["merge-tree", "--write-tree", "--messages", "-X", "ours", "mm-ren-a", "mm-ren-del"],
            &["merge-tree", "--write-tree", "--messages", "-X", "theirs", "mm-ren-a", "mm-ren-del"],
            &[
                "merge-tree",
                "--write-tree",
                "--messages",
                "-X",
                "no-renames",
                "mm-ren-a",
                "mm-ren-del",
            ],
            &["merge-tree", "--trivial-merge", "main", "mm-ren-a", "mm-ren-del"],
        ],
        out,
    );
    each(
        "cherry-pick",
        &[
            &["cherry-pick", "mm-ren-a", "mm-ren-del"],
            &["cherry-pick", "mm-ren-del", "mm-ren-a"],
            &["cherry-pick", "-n", "mm-ren-a", "mm-ren-del"],
        ],
        out,
    );
    each(
        "rebase",
        &[
            &["rebase", "mm-ren-a", "mm-ren-del"],
            &["rebase", "mm-ren-del", "mm-ren-a"],
            &["rebase", "--merge", "mm-ren-a", "mm-ren-del"],
            &["rebase", "-s", "resolve", "mm-ren-a", "mm-ren-del"],
        ],
        out,
    );
    // The octopus backend has no rename detection, so `git-merge-one-file` sees
    // a file deleted on one side and untouched on the other and commits the
    // deletion: stock prints `Merge made by the 'octopus' strategy.` with
    // `rename mm/{rr.txt => rr-a.txt} (100%)` in the diffstat and exits 0 where
    // ort would have conflicted. The port agrees, which is the finding — the
    // two-head path does *not* silently upgrade itself to ort here.
    each("merge", &[&["merge", "mm-ren-a", "mm-ren-del"]], out);
    each("revert", &[&["revert", "--no-edit", "-n", "mm-ren-del"]], out);
}

// ---------------------------------------------------------------------------
// A collision at a rename's destination — which stock calls add/add
// ---------------------------------------------------------------------------

/// `mm-ren-add` adds an unrelated file at `mm/rr-a.txt`, the path `mm-ren-a`
/// renames `mm/rr.txt` to.
///
/// **This is add/add, not rename/add, and the distinction is measured rather
/// than assumed.** `merge-ort.c` resolves the rename first and then finds two
/// independent additions at one path, so the report is
/// `CONFLICT (add/add): Merge conflict in mm/rr-a.txt` over a real conflicted
/// blob with stages 2 and 3 — no stage 1, because the destination has no base
/// version. A reader expecting `rename/add` from the shape of the inputs would
/// be reading a class git does not print here.
///
/// **It is also the first pair on this shape where `-X ours` and `-X theirs`
/// do something.** Every other conflicting pair the module builds is a class,
/// not a hunk, so both flags are inert (see
/// [`strategy_options_over_every_class`]). An add/add *is* a content
/// disagreement: stock resolves it at exit 0 and writes two different trees —
/// `f964a3a9…` for `-X ours` (the renamed content wins) and `6505f80f…` for
/// `-X theirs` (the independent add wins). The port writes the same two. That
/// turns the header's "both flags are inert here" from a claim about the flags
/// into a claim about the *classes*, which is what it always should have been.
///
/// The `merge.conflictStyle` rows are the other thing this pair unlocks. The
/// module header used to say the only content conflict on this shape needed
/// `--merge-base=` to build; an add/add over a *computed* base is a second one,
/// and it renders a marker block:
///
/// ```text
/// <<<<<<< mm-ren-a
/// rr line 1
/// …
/// rr line 10
/// ||||||| 03c866d
/// =======
/// an unrelated file at the rename's destination
/// >>>>>>> mm-ren-add
/// ```
///
/// with the base label present and abbreviated, and the port writes the same
/// tree (`036fb029…`) under both `diff3` and `zdiff3`. That is the positive
/// control for the label defect [`forced_base_and_conflict_style`] documents:
/// the port's `|||||||` line is right for a computed base and was wrong only
/// for an explicitly named one.
///
/// `mm-ren-b mm-ren-add` is the negative control — a rename to `mm/rr-b.txt`
/// and an add at `mm/rr-a.txt` do not collide, and both sides exit 0 — which is
/// what makes "the destination is what collides" checkable rather than
/// asserted.
///
/// `--merge-base=mm-ren-add` is the same tip used as a *base* instead: with
/// `mm/rr-a.txt` already present in stage 1, `main` deletes it and `mm-ren-a`
/// keeps it, so the class turns into modify/delete on a path no other case in
/// the module reaches that way.
fn add_at_a_rename_destination(out: &mut Vec<Case>) {
    each(
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "mm-ren-a", "mm-ren-add"],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-a", "mm-ren-add"],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-add", "mm-ren-a"],
            &["merge-tree", "--write-tree", "-z", "mm-ren-a", "mm-ren-add"],
            &["merge-tree", "--write-tree", "--quiet", "mm-ren-a", "mm-ren-add"],
            &["merge-tree", "--write-tree", "--messages", "-X", "ours", "mm-ren-a", "mm-ren-add"],
            &["merge-tree", "--write-tree", "--messages", "-X", "theirs", "mm-ren-a", "mm-ren-add"],
            &[
                "merge-tree",
                "--write-tree",
                "--messages",
                "-X",
                "no-renames",
                "mm-ren-a",
                "mm-ren-add",
            ],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-b", "mm-ren-add"],
            &[
                "merge-tree",
                "--write-tree",
                "--messages",
                "--merge-base=mm-ren-add",
                "mm-ren-a",
                "main",
            ],
        ],
        out,
    );
    for style in ["merge", "diff3", "zdiff3"] {
        out.push(
            Case::new(
                "merge-tree",
                &["merge-tree", "--write-tree", "--messages", "mm-ren-a", "mm-ren-add"],
                Shape::MergeMatrix,
            )
            .with_config(&[("merge.conflictStyle", style)]),
        );
    }
    each(
        "cherry-pick",
        &[
            &["cherry-pick", "mm-ren-a", "mm-ren-add"],
            &["cherry-pick", "mm-ren-add", "mm-ren-a"],
            &["cherry-pick", "-X", "ours", "mm-ren-a", "mm-ren-add"],
        ],
        out,
    );
    each(
        "rebase",
        &[
            &["rebase", "mm-ren-a", "mm-ren-add"],
            &["rebase", "-X", "theirs", "mm-ren-a", "mm-ren-add"],
            &["rebase", "mm-ren-b", "mm-ren-add"],
        ],
        out,
    );
    each("merge", &[&["merge", "mm-ren-a", "mm-ren-add"]], out);
    each("revert", &[&["revert", "--no-edit", "-n", "mm-ren-add"]], out);
}

// ---------------------------------------------------------------------------
// The inexact rename, and the option family it makes live
// ---------------------------------------------------------------------------

/// `mm-ren-edit` renames `mm/rr.txt` to `mm/rr-e.txt` *and* rewrites four of
/// its ten lines, which stock scores `R055`.
///
/// Every other rename on this shape is a bare `git mv`, and an exact rename is
/// paired by oid before any similarity score is computed — which is why
/// [`strategy_options_over_every_class`] could only keep `-X find-renames=` and
/// `-X rename-threshold=` as inert negative controls. A rename that has to be
/// *scored* to be found is what makes them live, and
/// [`similarity_threshold_over_the_inexact_rename`] does the sweep.
///
/// The pairs here are the class report at the default threshold:
///
/// * `mm-ren-a mm-ren-edit` — rename/rename, the same class the exact pair
///   produces, reached through the scoring path instead of the pairing path.
/// * `mm-ren-edit mm-ren-del` — the only pair on this shape that reports **two**
///   conflicts from one merge: `CONFLICT (rename/delete)` for `mm/rr.txt`
///   renamed to `mm/rr-e.txt` and then `CONFLICT (modify/delete)` for
///   `mm/rr-e.txt` itself, because the renamed content is also edited. A single
///   conflict per merge had been true of every case in this module.
/// * `mm-ren-edit mm-ren-add` and `cherry-pick mm-ren-add mm-ren-edit` — clean,
///   at exit 0: the edit lands at `mm/rr-e.txt` and the add at `mm/rr-a.txt`,
///   two paths that do not collide.
///
/// `cherry-pick` mislabels all of them as `CONFLICT (content)` exactly as it
/// does the exact rename/rename, and `rebase` names every class correctly and
/// records the same stages as stock — the verb-level split, re-measured on
/// inexact input. The two conflicting `rebase` cases still fail on the short
/// `# Conflicts:` list [`rebase_over_every_class`] documents, which is a
/// different defect and is why they are worth having: it says the class report
/// and the message body are written from different sources.
fn inexact_rename_over_every_verb(out: &mut Vec<Case>) {
    each(
        "merge-tree",
        &[
            &["merge-tree", "--write-tree", "--messages", "mm-ren-a", "mm-ren-edit"],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-edit", "mm-ren-b"],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-edit", "mm-ren-del"],
            &["merge-tree", "--write-tree", "--messages", "mm-ren-edit", "mm-ren-add"],
            &["merge-tree", "--write-tree", "--name-only", "mm-ren-a", "mm-ren-edit"],
        ],
        out,
    );
    each(
        "cherry-pick",
        &[
            &["cherry-pick", "mm-ren-a", "mm-ren-edit"],
            &["cherry-pick", "mm-ren-edit", "mm-ren-a"],
            &["cherry-pick", "mm-ren-edit", "mm-ren-del"],
            &["cherry-pick", "mm-ren-add", "mm-ren-edit"],
        ],
        out,
    );
    each(
        "rebase",
        &[
            &["rebase", "mm-ren-a", "mm-ren-edit"],
            &["rebase", "mm-ren-edit", "mm-ren-a"],
            &["rebase", "mm-ren-edit", "mm-ren-del"],
        ],
        out,
    );
    each("merge", &[&["merge", "mm-ren-a", "mm-ren-edit"]], out);
    each("revert", &[&["revert", "--no-edit", "-n", "mm-ren-edit"]], out);
}

/// The similarity threshold swept across `R055` in both directions.
///
/// **The defect this group exists to pin.** git reads the value of
/// `-X find-renames=`/`-X rename-threshold=` with `parse_rename_score`, which
/// divides the digits by ten to their own count: `40` is 40%, `5` is **50%**,
/// `055` is 5.5%, and a trailing `%` or an embedded `.` overrides the scaling.
/// Measured against stock on `mm-ren-a mm-ren-edit`, whose one rename scores 55:
///
/// | value | stock reads it as | stock | port under `merge-tree`/`cherry-pick` |
/// |---|---|---|---|
/// | *(none)* | 50% | rename/rename | rename/rename |
/// | `40`, `50`, `55` | 40/50/55% | rename/rename | **rename/delete** |
/// | `5` | 50% | rename/rename | **rename/delete** |
/// | `055` | 5.5% | rename/rename | **rename/delete** |
/// | `6`, `56`, `60`, `80` | 60/56/60/80% | rename/delete | rename/delete |
/// | `40%`, `55%` | 40/55% | rename/rename | rename/rename |
/// | `56%`, `80%` | 56/80% | rename/delete | rename/delete |
/// | `0.4` / `0.6` | 40% / 60% | rename/rename / rename/delete | same |
///
/// So the port **does** track the threshold, and its boundary is stock's — the
/// answer flips between `55%` and `56%` on both sides, in both directions. What
/// it does not do is apply git's digit scaling: every un-suffixed integer in
/// the table behaves as a threshold no similarity can reach, so the inexact
/// rename is lost under it whether the number is 5 or 55. It is a parse defect
/// in one spelling of one option, not a missing feature, and the `%` and
/// decimal rows are what make that distinction rather than reporting "renames
/// are not detected".
///
/// **It is also verb-dependent, which no single-verb sweep would have found.**
/// `rebase -X find-renames=40` and `rebase -X find-renames=60` both reproduce
/// stock's report and stock's unmerged stages — rename/rename and rename/delete
/// respectively — so `rebase` reads the bare integer correctly while
/// `merge-tree` and `cherry-pick` do not: the same option, the same value,
/// three code paths, two of them wrong. (The two `rebase` cases that stop with
/// a conflict still *fail*, on the short `# Conflicts:` list that every
/// conflicting `rebase` here carries — see [`rebase_over_every_class`]. Their
/// stdout, exit code and stages agree, which is what the threshold claim rests
/// on, and `rebase -X find-renames=60` matches outright because that answer has
/// only one unmerged path to list.) The
/// `cherry-pick` rows are read off the *stages* rather than the message,
/// because that verb mislabels the class anyway: at `40` the port records
/// stages 1 and 2 at `mm/rr-a.txt` (the rename/delete layout) and at `40%` it
/// records stages 1/2/3 at `mm/rr.txt`, `mm/rr-a.txt` and `mm/rr-e.txt` (the
/// rename/rename layout).
///
/// `-X find-renames=80 mm-ren-a mm-ren-b` is the control that keeps the defect
/// specific: the *exact* rename is still found under the broken spelling,
/// because pairing by oid happens before any score is consulted. A fix that
/// only made this case pass would have fixed nothing.
///
/// The `merge.*`/`diff.*` rows agree throughout and bound the finding on the
/// other side: `merge.renames=false` and `diff.renames=false` each drop the
/// inexact rename to a clean exit 0 on both sides, `merge.renames=true` leaves
/// the default, `merge.renameLimit=1` does not disable detection for a single
/// pair, and `merge.renameLimit=nonsense` is `fatal: bad numeric config value`
/// at exit 128 on both — the validation gap the module header used to record
/// here is closed.
fn similarity_threshold_over_the_inexact_rename(out: &mut Vec<Case>) {
    for value in [
        "40", "50", "55", "56", "60", "80", "5", "6", "055", "40%", "55%", "56%", "80%", "0.4",
        "0.6",
    ] {
        out.push(Case::new(
            "merge-tree",
            &[
                "merge-tree",
                "--write-tree",
                "--messages",
                "-X",
                &format!("find-renames={value}"),
                "mm-ren-a",
                "mm-ren-edit",
            ],
            Shape::MergeMatrix,
        ));
    }
    for value in ["40", "60", "55%"] {
        out.push(Case::new(
            "merge-tree",
            &[
                "merge-tree",
                "--write-tree",
                "--messages",
                "-X",
                &format!("rename-threshold={value}"),
                "mm-ren-a",
                "mm-ren-edit",
            ],
            Shape::MergeMatrix,
        ));
    }
    each(
        "merge-tree",
        &[
            &[
                "merge-tree",
                "--write-tree",
                "--messages",
                "-X",
                "no-renames",
                "mm-ren-a",
                "mm-ren-edit",
            ],
            &[
                "merge-tree",
                "--write-tree",
                "--messages",
                "-X",
                "find-renames=80",
                "mm-ren-a",
                "mm-ren-b",
            ],
        ],
        out,
    );
    for (key, value) in [
        ("merge.renames", "false"),
        ("merge.renames", "true"),
        ("diff.renames", "false"),
        ("merge.renameLimit", "1"),
        ("merge.renameLimit", "nonsense"),
    ] {
        out.push(
            Case::new(
                "merge-tree",
                &["merge-tree", "--write-tree", "--messages", "mm-ren-a", "mm-ren-edit"],
                Shape::MergeMatrix,
            )
            .with_config(&[(key, value)]),
        );
    }
    // The same option through the two verbs that carry an index: `cherry-pick`
    // loses the bare integer exactly as `merge-tree` does, and `rebase` does
    // not.
    for value in ["40", "40%", "60%"] {
        out.push(Case::new(
            "cherry-pick",
            &["cherry-pick", "-X", &format!("find-renames={value}"), "mm-ren-a", "mm-ren-edit"],
            Shape::MergeMatrix,
        ));
    }
    for value in ["40", "60", "40%"] {
        out.push(Case::new(
            "rebase",
            &["rebase", "-X", &format!("find-renames={value}"), "mm-ren-a", "mm-ren-edit"],
            Shape::MergeMatrix,
        ));
    }
}
