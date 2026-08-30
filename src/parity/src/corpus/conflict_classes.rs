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
//! [`Shape::MergeMatrix`] builds six independent branch pairs, one per class,
//! deliberately separate so a case can ask about rename/rename without also
//! answering modify/delete. `main` sits at the base commit of all six.
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
//!   symlink/file omissions in [`merge_tree_over_every_class`].
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
//!   module's and is where the label defect below lives.
//! * [`super::merge_dirty`] owns the dirty-worktree gates;
//!   [`super::rebase_engine`] owns `rebase`'s own option table;
//!   [`super::rerere_engine`] owns `rerere.*` over a merge;
//!   [`super::sequences`] owns everything needing a second invocation.
//!
//! # Which of the eight classes this shape reaches, and which it still does not
//!
//! | class | reachable | how |
//! |---|---|---|
//! | modify/delete | yes | `mm-mod` / `mm-del` on `mm/md.txt` |
//! | rename/rename | yes | `mm-ren-a` / `mm-ren-b` on `mm/rr.txt` |
//! | directory rename | yes | `mm-dir` / `mm-add` on `mm/old/` |
//! | mode-only | yes | `mm-mode` — the corpus's first `100755` blob |
//! | file becomes directory | yes | `mm-fd` / `mm-file` on `mm/fd` |
//! | symlink vs file | yes | `mm-reg` / `mm-link` on `mm/slink` |
//! | **rename/delete** | **no** | see below |
//! | **rename/add** | **no** | see below |
//!
//! **rename/delete** needs one side to rename `P` while the other deletes `P`.
//! The shape renames exactly two paths — `mm/rr.txt` (by `mm-ren-a`/`mm-ren-b`)
//! and `mm/old/{a,b}.txt` (by `mm-dir`) — and deletes exactly one, `mm/md.txt`
//! (by `mm-del`); the two sets are disjoint and no re-pointing of the base with
//! `merge-tree --merge-base=` closes the gap, because every candidate turns the
//! second side's edit into a *rename* too and lands back on rename/rename.
//! **rename/add** needs one side to rename `P` to `Q` while the other adds `Q`
//! independently; nothing in the shape adds `mm/rr-a.txt`, `mm/rr-b.txt` or
//! `mm/new/*`. Both need another branch in `fixture.rs`, which a corpus module
//! cannot add — recorded here rather than substituted for with something
//! easier. A **mode/mode** conflict (two sides setting two different modes on
//! one path) is unreachable for the same reason: only `mm-mode` touches a mode.
//!
//! # What `git merge` itself can and cannot do here, and why the verbs differ
//!
//! `main` is the *base* of all six pairs, so it is an ancestor of every tip and
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
//! 146 cases, **59 matching (40.4%)**, measured with
//! `--only merge,merge-tree,cherry-pick,revert,rebase --verbose` against
//! `target/debug/git` with `/usr/bin/git` (2.50.1) as the second oracle:
//!
//! | verb | cases | match | parity |
//! |---|---|---|---|
//! | `merge-tree` | 67 | 32 | 47.8% |
//! | `cherry-pick` | 25 | 11 | 44.0% |
//! | `rebase` | 22 | 0 | 0.0% |
//! | `merge` | 21 | 5 | 23.8% |
//! | `revert` | 11 | 11 | 100% |
//!
//! **76 of the 87 failures are corroborated by the second oracle** — 2.50.1
//! gave stock 2.55.0's answer byte for byte, so they are the port's difference
//! and not a version difference. `version-skew` and `gits-disagree` are **0**
//! on this shape (the 15 `gits-disagree` cases in that run are all on other
//! shapes). The remaining 11 are the `[UNSUPPORTED]` verdicts below, which the
//! harness does not put to a second git because the port has already said it
//! does not implement the path. `zvcs-flaky` is 0 across the whole run.
//!
//! They are **nine** distinct defects, not 87. The rows below count the cases
//! each defect is *visible in*, and they overlap — a `rebase mm-reg mm-link`
//! carries both the symlink defect and the reflog one, so the column sums to
//! more than 87 on purpose:
//!
//! | defect | cases | verdict |
//! |---|---|---|
//! | `merge.directoryRenames` never read: `false` moves the file anyway, `conflict` does not conflict, `true` drops the `Path updated:` line | 19 | 3 `merge-tree`, 6 `cherry-pick`, 6 `rebase`, 4 `merge` |
//! | `merge-tree --messages` refuses the directory-rename class outright (`conflict at mm/new is a class whose git message text is not ported`) | 11 | `[UNSUPPORTED]` |
//! | `rebase` writes an extra `rebase: checkout <branch>` HEAD reflog entry before `rebase (start):`, which stock does not write — present on **all 22** `rebase` cases, sole cause on 12 | 12 | `[STATE-DIFF]` |
//! | `CONFLICT (file/directory): directory in the way of …` omitted | 14 | 8 `merge-tree`, 3 `cherry-pick`, 3 `rebase` |
//! | symlink-vs-file: stages stacked at the original path instead of `<path>~<side>`, and `merge-tree`'s merged tree is the **base** tree | 14 | 8 `merge-tree`, 3 `cherry-pick`, 3 `rebase` |
//! | `git merge` over two heads runs ort where stock runs `git-merge-one-file` | 8 | `merge` |
//! | `cherry-pick` reports rename/rename and file/directory as `CONFLICT (content)` — its `rebase` twin gets both right | 5 | `cherry-pick` |
//! | `merge --stat`/`--summary` diffstat does not detect the rename | 3 | `merge` |
//! | one each: `merge.renameLimit=nonsense` not validated (exit 1 against stock's 128), `merge.conflictStyle=diff3`/`zdiff3` drops the `--merge-base=` label from the `\|\|\|\|\|\|\|` line, an emptied `mm/old/` left in the worktree after a directory-rename merge | 4 | 3 `merge-tree`, 1 `merge` |
//!
//! Three change what is *written* rather than what is printed, and each is
//! called out where its cases are defined: `merge.directoryRenames` (the port
//! commits a tree neither git would write), the symlink-vs-file tree (the port
//! returns the pre-merge tree), and the emptied directory. A fourth, the
//! `rebase` reflog entry, is invisible to stdout entirely and was found only by
//! the runner's state probe — no hand comparison of stdout, index, refs and
//! objects had caught it.
//!
//! # Determinism
//!
//! Many of these commit, so their object ids are part of what is compared.
//! **All 104 distinct argvs this module uses were each run twice against stock
//! 2.55.0**, in two `cp -Rp` copies of the shape under
//! [`crate::env::harden`], and the two runs compared on exit code, stdout,
//! stderr, `ls-files --stage`, `for-each-ref`, `cat-file --batch-check
//! --batch-all-objects`, `log --all --format='%H %T %P %s'` and the set of
//! files under `.git`. All 104 agreed; none was nondeterministic. (The count is
//! of argvs, not cases: the configuration variants reuse an argv already in the
//! set and cannot introduce a clock the bare argv does not have.)
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
/// Three of the six diverge, and all three were reproduced by hand:
///
/// * `mm-dir mm-add` — stock exits **1** with `CONFLICT (file location)` and a
///   stage-3 entry at `mm/new/c.txt`; the port exits **0** with a clean tree.
///   See [`directory_rename_configuration`].
/// * `mm-fd mm-file` — the port omits stock's
///   `CONFLICT (file/directory): directory in the way of mm/fd from mm-file;
///   moving it to mm/fd~mm-file instead.`
/// * `mm-reg mm-link` — stock records the file side at `mm/slink~mm-reg` and
///   writes tree `3fc920d6…`, whose `mm/slink` is `mm-link`'s symlink
///   (`561da849…`). The port stacks all three stages at `mm/slink`, prints the
///   same `renamed one of them so each can be recorded somewhere` message
///   without renaming anything, and writes `6036719e…` — the **base** tree,
///   whose `mm/slink` is `09d56094…`, the pre-merge target that neither side
///   kept. The message is right and the tree is neither side's answer.
///
/// `--messages` is not redundant with the default. Stock prints the report
/// either way, but the port takes a different path under the explicit flag: on
/// `mm-dir mm-add` it stops with
/// `zvcs: merge-tree: conflict at mm/new is a class whose git message text is
/// not ported`, so the flag turns a silent wrong answer into a stated refusal.
/// Both are worth having — one measures the answer, the other the admission.
///
/// `--name-only`, `--no-messages`, `-z` and `--quiet` each drop a different part
/// of the record, which is what separates "the report is wrong" from "the tree
/// is wrong". `--quiet` on `mm-dir mm-add` is the odd one: stock exits **0**
/// there while the same merge without it exits 1, and the port agrees — a stock
/// behaviour the corpus had no case for.
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
    // directory-rename record (it reports the conflict in the message field and
    // still calls the merge complete) and the port prints `1`, on top of losing
    // the record's whole conflicted-file block.
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
// `merge.directoryRenames`: a key the port never reads
// ---------------------------------------------------------------------------

/// The three settings of `merge.directoryRenames`, through every verb that
/// reaches the engine, plus the controls that agree.
///
/// **The defect this group exists to pin.** `mm-dir` renames `mm/old/` to
/// `mm/new/`; `mm-add` adds `mm/old/c.txt` into the old name. Stock's answer is
/// a function of the key. The port's is not — it behaves as `true` in all
/// three settings, which is wrong two different ways and silently right the
/// third. Measured by hand on the shape, stock 2.55.0 and git 2.50.1 agreeing:
///
/// | setting | stock | port |
/// |---|---|---|
/// | `false` | tree `65c46d8e…`, `mm/old/c.txt` kept, exit 0 | tree `82dbce05…`, file **moved** to `mm/new/c.txt`, exit 0 |
/// | `conflict` (default) | tree `82dbce05…`, stage 3 at `mm/new/c.txt`, `CONFLICT (file location)`, exit **1** | tree `82dbce05…`, no conflict, exit **0** |
/// | `true` | tree `82dbce05…`, exit 0, `Path updated: …` on the pick paths | same tree and status, `Path updated:` line **missing** |
///
/// The `false` row is the one that loses work: through `cherry-pick` the port
/// commits `04c99f0d…` where stock commits `4dcef09a…`, a tree neither git
/// would write for that configuration.
///
/// **The controls are the point of the group, not decoration.** If the three
/// failures stood alone a reader could conclude the port ignores every merge
/// configuration key, and the fix would be aimed at the wrong place. It does
/// not. `-X no-renames`, `merge.renames=false` and `diff.renames=false` each
/// turn the same detection off, and on `merge-tree` and `cherry-pick` the port
/// then agrees with stock byte for byte — measured, not assumed: those four
/// cases match. That localises the defect to `merge.directoryRenames` alone.
/// (`rebase -X no-renames mm-dir mm-add` is the fifth control and it does
/// *not* match, for an unrelated reason: every `rebase` case in this module
/// carries the extra HEAD reflog entry described in
/// [`rebase_over_every_class`]. Its stdout, index and refs agree.)
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
/// `main` is the base of all six pairs, so `cherry-pick <a> <b>` applies `a`
/// cleanly — the commit it writes has `a`'s tree — and then merges `b`'s change
/// against it. That is the same three trees `merge-tree <a> <b>` sees, reached
/// through `sequencer.c` with an index and a worktree behind it, and the answers
/// are **not** the same. On rename/rename, `merge-tree` agrees with stock while
/// `cherry-pick` reports `CONFLICT (content): Merge conflict in mm/rr-a.txt`
/// where stock reports
/// `CONFLICT (rename/rename): mm/rr.txt renamed to mm/rr-a.txt in HEAD and to
/// mm/rr-b.txt in d4a2413`. Both spellings of each pair are here because the
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
/// **A defect no stdout comparison finds, and every case here carries it.** The
/// runner's state probe reads the `HEAD` reflog, and the port writes one entry
/// stock does not. On `rebase mm-mode mm-reg`, stock's trail is
///
/// ```text
/// 0421e7a9 5ca8368f  rebase (start): checkout mm-mode
/// 5ca8368f 0c4f6522  rebase (pick): merge-matrix: slink becomes a file
/// 0c4f6522 0c4f6522  rebase (finish): returning to refs/heads/mm-reg
/// ```
///
/// and the port's is the same three preceded by
/// `0421e7a9 e0e7093f  rebase: checkout mm-reg` — the checkout of the branch
/// being rebased, recorded as if it were a user action. It is the sole cause of
/// 12 of the 22 failures in this group and rides along on the other 10, and it
/// survived every hand comparison of stdout, index, refs and objects because it
/// is in none of them.
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
/// port answers all six pairs with merge-ort instead, so the two disagree on
/// nearly everything, and in two of the six the disagreement changes what is
/// committed rather than what is printed:
///
/// * `merge mm-ren-a mm-ren-b` — stock **succeeds**, commits
///   `Merge branches 'mm-ren-a' and 'mm-ren-b'` with both `mm/rr-a.txt` and
///   `mm/rr-b.txt` present, and exits 0. The port raises
///   `CONFLICT (rename/rename)`, leaves three unmerged stages and exits 1.
/// * `merge mm-dir mm-add` — both commit; stock's tree keeps `mm/old/c.txt`
///   (`65c46d8e…`) and the port's moves it to `mm/new/c.txt` (`82dbce05…`).
///
/// The other four differ in the report only: stock prints
/// `Simple merge did not work, trying automatic merge.` followed by
/// `git-merge-one-file`'s own `ERROR: … Not handling case …` /
/// `ERROR: … Not merging symbolic link changes.` and `fatal: merge program
/// failed`, and the port prints an ort-style `CONFLICT (…)` line.
///
/// The three `-s` cases are the refusals, and they are `strict` because the
/// refusal *is* the whole behaviour: `ort`, `resolve` and `recursive` each
/// handle exactly two trees, so a third head is
/// `error: Not handling anything other than two heads merge.` at exit 2. All
/// three agree byte for byte today, which is what makes them worth pinning —
/// they are the boundary the octopus cases sit just outside of.
///
/// `--no-stat --no-ff -m merged mm-dir` looks like the boring control and is
/// not: stdout, index and refs all agree, and the *worktree* does not. After
/// the directory rename stock has removed `mm/old/`, and the port leaves the
/// emptied directory behind — visible only because the state probe walks the
/// worktree (`mm/old -: <dir>` on the port's side of the diff and nowhere on
/// stock's).
///
/// `--stat`/`--summary` after `--no-ff mm-dir` is a separate defect from
/// everything above and is why those three argvs are here: the merge itself
/// agrees, and the **diffstat** does not. Stock renders
/// `mm/{old => new}/a.txt | 0` plus `rename mm/{old => new}/a.txt (100%)`; the
/// port renders four independent create/delete lines and a
/// `2 insertions(+), 2 deletions(-)` total. `--no-stat` agrees, which places the
/// defect in the rename detection of the summary rather than in the merge.
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
/// * **`-X ours`/`-X theirs` change nothing on any of the five conflicting pairs.** Both are
///   content-level resolutions in `merge-ort.c`; a modify/delete, a
///   rename/rename, a distinct-types or a file/directory conflict is not a
///   content conflict, so the flag is parsed and then has nothing to apply to.
///   Every `-X ours`/`-X theirs` case below therefore diverges *exactly as its
///   unadorned twin does* — which is the finding: a port that let `-X theirs`
///   swallow a modify/delete would show up here and does not.
/// * **`-X find-renames=`/`-X rename-threshold=`/`merge.renameLimit` cannot
///   flip a rename on this shape.** Every rename it builds is exact — `git mv`
///   with no edit, so a `100%` similarity match — and exact renames are found
///   by the pairing pass before any similarity score or limit is consulted.
///   The four cases that set them agree with their unadorned twins on both
///   sides. They are kept as the negative control for the *next* claim: it is
///   `-X no-renames` and `merge.renames=false` that turn detection off, and
///   both do, which is what makes the `merge.directoryRenames` failures above
///   specific.
///
/// `merge.renameLimit=nonsense` is the one value that is not inert:
/// `fatal: bad numeric config value 'nonsense' for 'merge.renamelimit': invalid
/// unit` at exit **128** in stock, exit 1 in the port. `merge_ort` records the
/// same validation gap under `merge`; this is the `merge-tree` half, which is a
/// different entry point into `git_config` and was unmeasured.
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

/// `merge-tree --merge-base=` pointed at one of the six tips, which is the only
/// way to get a **content** conflict out of [`Shape::MergeMatrix`] — and the
/// only way to make `merge.conflictStyle` mean anything here.
///
/// None of the six pairs disagrees about the *bytes* of a file: `mm/rr.txt` is
/// moved and never edited, `mm/md.txt` is edited on one side and removed on the
/// other, and the rest are type or mode changes. Re-pointing the base changes
/// that. With `--merge-base=mm-fd`, where `mm/fd` is a directory and so has no
/// blob at all, `mm-file` and `main` both *add* `mm/fd` with different content
/// and stock answers `CONFLICT (add/add): Merge conflict in mm/fd` over a real
/// conflicted blob.
///
/// **The defect that reaches.** With `merge.conflictStyle=diff3` (and
/// identically `zdiff3`) stock labels the base section with the name it was
/// given on the command line and the port leaves the label empty:
///
/// ```text
/// stock: <<<<<<< mm-file\nstill a file, edited\n||||||| mm-fd\n=======\n…
/// port:  <<<<<<< mm-file\nstill a file, edited\n|||||||\n=======\n…
/// ```
///
/// so the merged trees differ (`2e406707…` against `aed4424f…`) while the
/// `merge` style agrees byte for byte. It is not a property of the conflict
/// class: reproduced in a two-line scratch repository, the port gets
/// `||||||| 5e67bc4048` right for a computed base and for an add/add over a
/// computed base, and drops the label only when `--merge-base=` names the base
/// explicitly. [`super::patch_equivalence`] sweeps `merge.conflictStyle`
/// without `--merge-base` and sweeps `--merge-base` without
/// `merge.conflictStyle`; the crossing is neither module's, which is why the
/// defect survived both.
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
