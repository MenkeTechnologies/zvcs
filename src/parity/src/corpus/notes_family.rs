//! `git notes`: the second commit graph, the ref that selects it, and the
//! *display* layer that is a different implementation from the write layer.
//!
//! A note is not a field on a commit. Writing one builds a blob, splices it
//! into a tree keyed by the annotated object's hex name, and commits that tree
//! onto `refs/notes/<ref>` — a whole parallel history whose only visible traces
//! are one ref, some objects, and whatever a reader chooses to print. Two
//! implementations can therefore agree on every byte `notes show` prints and
//! still write different trees, different commits, and different refs, and the
//! only thing that notices is the state digest.
//!
//! # How this divides territory with the six modules that got there first
//!
//! There were 125 `notes` cases before this file — 166 invocations once the six
//! sequences are counted step by step — and no module owned them. All six were
//! read in full; what each owns, and what is left:
//!
//! * **`corpus/history_rewrite.rs:350` (`notes`)** — the write verbs on a
//!   *pristine* fixture: `add`/`append`/`copy`/`list`/`show`/`remove`/`prune`/
//!   `edit`/`get-ref` on `Linear` and `Branched`, `-m`, `-f`, `-C <commit>`,
//!   `--ref=custom`, and six error paths. Every one of them starts from a
//!   repository with **no notes**, so it measures the create path and never the
//!   update path.
//! * **`corpus/stateful_side_files.rs:378` (`notes`)** — what decides *which
//!   bytes the blob holds* and *which ref it lands on*: `--separator`,
//!   `--stripspace`, `--allow-empty`, `-F -`, `--ref=` vs `GIT_NOTES_REF` vs
//!   `core.notesRef` precedence, `copy --stdin`, `copy --for-rewrite=amend`
//!   with `notes.rewrite.amend`, `remove --stdin`, and the five `merge`
//!   refusals that leave no `NOTES_MERGE_*` behind. Also `Branched`, so also
//!   the create path.
//! * **`corpus/fixture_gaps2.rs:448` (`notes_and_replace`)** — the read verbs
//!   over [`Shape::NotesReplace`], which is the only fixture that ships notes.
//!   24 `notes` argvs, 10 `log`, 5 `show`, 6 `cat-file`, plus
//!   `notes.displayRef` at three values. It owns `notes merge <ref>` and the
//!   four `-s <strategy>` spellings on that shape.
//! * **`corpus/sequences.rs:1509` + `:3464`** — the multi-step workflows:
//!   add→append→copy→remove, the conflicted merge aborted, the conflicted merge
//!   committed, three refs merged by strategy in turn, and `prune` after `gc`
//!   removed the annotated commit. Sequences are the only thing that can reach
//!   `NOTES_MERGE_*` in one state and act on it in the next.
//! * **`corpus/exit_codes.rs:254,447`** — exactly two: `notes add --nosuchopt`
//!   and `notes copy HEAD HEAD`, both for their exit status.
//! * **`corpus/misc_commands.rs:890`** — `notes <sub> -h` and
//!   `notes <sub> --zzbogus=x` for all ten subcommands, plus two `--help-al`
//!   near-misses. Help text only; no subcommand body runs.
//!
//! Nothing in those six ever ran `ls-tree` or `cat-file` **on a notes ref**,
//! ever asked `rev-list`, `format-patch` or `--show-notes=<ref>` about a note,
//! ever set `notes.rewriteRef`, `notes.rewriteMode`, `GIT_NOTES_REWRITE_REF`,
//! `notes.mergeStrategy` or `notes.<name>.mergeStrategy`, ever annotated a tag,
//! a tree or a blob, ever ran a rewrite-class verb (`commit --amend`, `rebase`)
//! over a repository that has notes, and never merged a ref that is not a notes
//! ref. That is this file.
//!
//! # The fanout, and why there is no case for it
//!
//! A notes tree reorganises itself as it grows: past a threshold the writer
//! splits the flat `<40 hex>` entries into `<2 hex>/<38 hex>` subtrees, and a
//! port that writes a flat tree where stock fans out produces a repository
//! stock reads as having *no note* on the objects it moved. It is the sharpest
//! failure this family has, and **it is not reachable from this harness.**
//!
//! Measured on stock 2.55.0, annotating fresh blobs one at a time in a scratch
//! repository, `ls-tree refs/notes/commits` stays entirely flat up to 75 notes
//! and the first `040000 tree` entry appears at the **76th**:
//!
//! ```text
//! $ for i in $(seq 1 120); do b=$(git hash-object -w --stdin <<<"blob $i")
//!       git notes add -m "n$i" $b; done          # reporting the first fanout
//! first fanout subtree at 76 notes
//! ```
//!
//! A case is one invocation against one fixture, so the ceiling on how many
//! notes it can write is the number of objects the fixture holds.
//! `Shape::NotesReplace` holds 31 (`11 blob`, `10 commit`, `10 tree`, from
//! `cat-file --batch-all-objects`), and the largest shape in the whole fixture
//! set holds 36 (`attributes` and `mergeable-staged`, counted over every
//! template the harness builds). The ceiling is under half the threshold.
//! [`bulk_copy_stdin`] writes the widest notes tree that is reachable — 12
//! entries in a single `notes copy --stdin` — which measures the multi-entry
//! *sort* and the single-commit write, and cannot measure the fanout. A shape
//! carrying a pre-fanned notes ref would fix this; shapes are not this file's
//! to add.
//!
//! # What else is not measurable here, and why
//!
//! * **`notes merge --commit` / `--abort` over a conflict.** Reaching the
//!   `NOTES_MERGE_*` state costs one invocation and acting on it costs a
//!   second. A [`Case`] is one invocation; `sequences.rs:1536,1553,3527` owns
//!   this and is the right place for it.
//! * **`notes edit` with a message the editor actually writes.**
//!   `env::harden` pins `GIT_EDITOR=true`, which cannot be re-pointed by a case
//!   (`env::is_pinned`), so the editor always exits 0 having written nothing.
//!   That is not a dead end — on a note that *exists*, `true` leaves the
//!   prefilled template alone and git re-commits the unchanged note, verified
//!   on stock 2.55.0: `refs/notes/commits` moves `686803…` → `b5ee2250…` while
//!   `notes show HEAD` still prints `default note on HEAD`. [`editor_paths`]
//!   pins that. What stays unreachable is an edit that *changes* the text.
//! * **`GIT_NOTES_REF` on the display side.** It selects the write ref and the
//!   default display ref from the same variable, so a case that sets it cannot
//!   separate the two; `stateful_side_files.rs:427` already owns the write half.
//! * **`cherry-pick` carrying a note.** It does not, on any git: the commands
//!   `notes.rewrite.<command>` is defined for are `amend` and `rebase` and
//!   nothing else, so there is no note-copying behaviour for `cherry-pick` to
//!   agree or disagree about. A case would measure the absence of a feature
//!   rather than the feature, so [`rewrite_refs`] covers the two verbs that do
//!   copy and stops there.
//!
//! # What these cases found, all reproduced by hand against both oracles
//!
//! Five defects, each confirmed identical on stock 2.55.0 and git 2.50.1 and
//! different on the port. Each is stated at the group that measures it; the
//! list is here so a reader knows what this file is currently red for.
//!
//! 1. **`notes merge <non-notes-ref>` is a silent no-op.** Both gits write a
//!    merge commit onto `refs/notes/commits`; the port leaves the ref where it
//!    was, prints nothing, and exits 0. [`merge_strategy_config`].
//! 2. **`notes.rewriteRef` / `GIT_NOTES_REWRITE_REF` are not implemented.**
//!    With either one set, `commit --amend` and `rebase` carry the note onto
//!    the rewritten commit on both gits and carry nothing on the port, with
//!    byte-identical stdout on both sides. [`rewrite_refs`].
//! 3. **`notes.rewriteMode=ignore` writes nothing.** Both gits still commit the
//!    (unchanged) notes tree; the port does not move the ref. [`rewrite_refs`].
//! 4. **A bad `notes.rewriteMode` changes the note text.** Both gits reject the
//!    value and leave the destination's note alone; the port falls back to
//!    concatenating, so `HEAD~1` ends up holding two notes instead of one.
//!    [`rewrite_refs`].
//! 5. **`--show-notes=<ref>` replaces the display list instead of adding to
//!    it.** Both gits print the default note *and* the named one; the port
//!    prints only the named one. `--notes=<ref>` really does replace, and the
//!    port is right about that one — which is why the pair is what measures it.
//!    [`display_selection`]. `rev-list --notes`, `--standard-notes` and
//!    `--no-standard-notes` are simply unknown to the port's parsers, which is
//!    the same group.
//!
//! The four `notes merge -s <strategy>` state failures that predate this file
//! have a one-byte cause, recorded here because six of the new cases inherit
//! it: **the port appends a trailing newline to the notes-merge commit
//! message.** The trees are identical, the parents are identical, the message
//! text is identical, and `git cat-file commit refs/notes/commits | xxd` ends
//! `…refs/notes/commits` on stock and `…refs/notes/commits\n` on the port —
//! 326 bytes against 327, and every downstream id differs.

use crate::fixture::Shape;
use crate::runner::Case;

pub fn cases(out: &mut Vec<Case>) {
    notes_ref_as_history(out);
    display_selection(out);
    display_config(out);
    merge_strategy_config(out);
    rewrite_refs(out);
    write_targets(out);
    editor_paths(out);
    bulk_copy_stdin(out);
}

/// One case per argv, on one shape, under one command name.
fn each(shape: Shape, cmd: &'static str, argvs: &[&[&str]], out: &mut Vec<Case>) {
    for argv in argvs {
        out.push(Case::new(cmd, argv, shape));
    }
}

// ---------------------------------------------------------------------------
// The notes ref read as what it is: a commit graph
// ---------------------------------------------------------------------------

/// `refs/notes/*` asked the questions any other branch would be asked.
///
/// Every other module in the corpus reads a note through `notes show` or `%N`,
/// which route through `notes.c`'s tree walker and answer the same for a flat
/// tree and a fanned-out one. These read the ref *as a ref*: `ls-tree` prints
/// the tree entries verbatim, so the path a note is stored at — the thing a
/// fanout changes and the thing that decides whether stock can find the note
/// afterwards — becomes stdout rather than an implementation detail.
///
/// Verified on stock 2.55.0 against [`Shape::NotesReplace`]: the tree is flat,
/// two entries, keyed by full 40-hex object name, and the ref's tip is an
/// ordinary commit whose message is `Notes added by 'git notes add'` and whose
/// author and committer are the pinned identity — so the whole thing is a
/// constant, not a clock.
///
/// ```text
/// $ git ls-tree refs/notes/commits
/// 100644 blob f7b2374f…  7b6d7d59f80ba49d1f9add363d00a1defcdc738f
/// 100644 blob 2d484ff3…  e882351e84ec33831d9f9af554d2d29b38fcd1db
/// ```
fn notes_ref_as_history(out: &mut Vec<Case>) {
    each(
        Shape::NotesReplace,
        "ls-tree",
        &[
            &["ls-tree", "refs/notes/commits"],
            &["ls-tree", "-r", "--name-only", "refs/notes/review"],
            &["ls-tree", "refs/notes/other"],
            &["ls-tree", "-l", "refs/notes/commits"],
        ],
        out,
    );
    each(
        Shape::NotesReplace,
        "cat-file",
        &[
            &["cat-file", "-p", "refs/notes/commits"],
            &["cat-file", "-p", "refs/notes/commits^{tree}"],
            &["cat-file", "-t", "refs/notes/commits"],
            &["cat-file", "-s", "refs/notes/review"],
        ],
        out,
    );
    // The notes ref walked as history. `notes edit`, `notes merge` and every
    // rewrite-mode below add commits to it, so how many commits deep it is and
    // what each one says is a fact the corpus should be able to state.
    each(
        Shape::NotesReplace,
        "log",
        &[
            &["log", "--oneline", "refs/notes/commits"],
            &["log", "--format=%s", "refs/notes/review"],
        ],
        out,
    );
    out.push(Case::new("show", &["show", "--stat", "refs/notes/commits"], Shape::NotesReplace));
    out.push(Case::new("rev-parse", &["rev-parse", "refs/notes/commits^{tree}"], Shape::NotesReplace));
    // A repository with no notes ref at all: `ls-tree` on it must refuse rather
    // than print an empty tree, which is the failure mode of a reader that
    // treats a missing notes ref as an empty one everywhere.
    out.push(Case::strict("ls-tree", &["ls-tree", "refs/notes/commits"], Shape::Linear));
}

// ---------------------------------------------------------------------------
// The display layer
// ---------------------------------------------------------------------------

/// Which notes a *reader* prints, which is not the same code as which note a
/// writer writes.
///
/// `revision.c` keeps a display list; `--notes=<ref>`, `--show-notes=<ref>`,
/// `--no-notes` and `--standard-notes` each edit that list differently, and the
/// difference between two of them is the whole finding here. Measured on stock
/// 2.55.0 against [`Shape::NotesReplace`], whose `HEAD` carries a note on
/// `refs/notes/commits` *and* one on `refs/notes/other`:
///
/// ```text
/// $ git log -1 --notes=other --format=%N        # replaces the default list
/// other note on HEAD, conflicting
/// $ git log -1 --show-notes=other --format=%N   # ADDS to it
/// default note on HEAD
/// other note on HEAD, conflicting
/// ```
///
/// `fixture_gaps2.rs:493` has the only other `--show-notes` case and spells it
/// `--show-notes=review --oneline`, where the oneline format prints no notes at
/// all — so the additive rule was reachable by nothing.
///
/// `rev-list` is here because it is the *third* consumer of the same option
/// table and the one that refuses: git wires `--notes` into `rev-list`'s parser
/// and then dies `fatal: rev-list does not support display of notes`. Answering
/// that with a usage block is a different exit code for a different reason.
fn display_selection(out: &mut Vec<Case>) {
    each(
        Shape::NotesReplace,
        "log",
        &[
            // The additive spelling against the replacing one, same ref, same
            // format: the pair is the measurement.
            &["log", "-1", "--show-notes=other", "--format=%N"],
            &["log", "-1", "--notes=other", "--format=%N"],
            &["log", "-1", "--notes=other", "--notes", "--format=%N"],
            // The deprecated pair: `--standard-notes` turns the default list
            // on (so with `--notes=review` both refs print), and
            // `--no-standard-notes` turns it off (so nothing prints). Verified
            // on stock 2.55.0; the port knows neither spelling.
            &["log", "-1", "--standard-notes", "--notes=review", "--format=%N"],
            &["log", "-1", "--no-standard-notes", "--format=%N"],
            // Order inside the list: a later `--no-notes` empties it, and a
            // `--notes` after that refills it with the default.
            &["log", "-1", "--notes", "--no-notes", "--format=%N"],
            &["log", "-1", "--notes=review", "--no-notes", "--notes", "--format=%N"],
            // A ref that is not there, and a ref name that is not a ref.
            &["log", "-1", "--notes=nosuchref", "--format=%N"],
            &["log", "-1", "--notes=", "--format=%N"],
            &["log", "-1", "--notes=refs/heads/main", "--format=%N"],
            // `%N` under the two pretty formats that render notes themselves.
            &["log", "-1", "--pretty=raw", "--notes"],
            &["log", "-1", "--pretty=fuller", "--notes"],
            // The walk, not just one commit: the replaced commit is in it.
            &["log", "--notes", "--format=%N%n--", "--all"],
            &["log", "--no-replace-objects", "--notes", "--format=%N", "-1", "HEAD~2"],
        ],
        out,
    );
    each(
        Shape::NotesReplace,
        "show",
        &[
            &["show", "-s", "--show-notes=other", "HEAD"],
            &["show", "-s", "--notes=*", "HEAD"],
            &["show", "-s", "--show-notes", "HEAD"],
            &["show", "--notes", "-s", "refs/notes/commits"],
        ],
        out,
    );
    // `format-patch`'s own renderer: a note becomes an indented `Notes:` block
    // between the commit message and the diffstat, and a named ref titles it
    // `Notes (review):`. `--stdout` so the case writes no files.
    each(
        Shape::NotesReplace,
        "format-patch",
        &[
            &["format-patch", "--notes", "--stdout", "-1"],
            &["format-patch", "--notes=review", "--stdout", "-1"],
            &["format-patch", "--no-notes", "--stdout", "-1"],
            &["format-patch", "--notes", "--stdout", "-2"],
        ],
        out,
    );
    // `rev-list` refuses rather than renders.
    each(
        Shape::NotesReplace,
        "rev-list",
        &[
            &["rev-list", "-1", "--notes", "HEAD"],
            &["rev-list", "-1", "--no-notes", "HEAD"],
            &["rev-list", "--format=%N", "--notes", "-1", "HEAD"],
            // Without `--notes`, `%N` is simply empty and the walk succeeds —
            // the negative control that says the refusal is about the option
            // and not about the placeholder.
            &["rev-list", "-1", "--format=%N", "HEAD"],
        ],
        out,
    );
    // A repository with no notes ref at all. Every display path has to answer
    // "no note" rather than fail to find the ref.
    each(
        Shape::Linear,
        "log",
        &[&["log", "--notes", "--format=%s%n%N"], &["log", "-1", "--notes=commits", "--format=%N"]],
        out,
    );
    out.push(Case::new("show", &["show", "-s", "--notes", "HEAD"], Shape::Linear));
    out.push(Case::new("format-patch", &["format-patch", "--notes", "--stdout", "-1"], Shape::Linear));
}

// ---------------------------------------------------------------------------
// Configuration on the display side
// ---------------------------------------------------------------------------

/// `notes.displayRef` and `core.notesRef` as *files*, and set more than once.
///
/// `fixture_gaps2.rs:500` sets `notes.displayRef` three times, each as a single
/// `-c`. `notes.displayRef` is a **multi-valued** key — git appends every value
/// it sees to the display list (`notes.c:notes_display_config`) — and a single
/// `-c` cannot express that, so a port that keeps only the last value scored
/// the same as one that keeps all of them. Two entries in one repository config
/// file is the shape that separates them, and it is also the only spelling that
/// exercises the file parser rather than the `-c` splitter.
///
/// `core.notesRef` changes the *default* display ref as well as the write ref,
/// so it is the one key that moves both layers at once.
fn display_config(out: &mut Vec<Case>) {
    use crate::runner::{ConfigEntry, ConfigScope};

    // Two values for one key, in one file, in order.
    out.push(
        Case::new("log", &["log", "-1", "--notes", "--format=%N"], Shape::NotesReplace)
            .with_scoped_config(vec![
                ConfigEntry::set(ConfigScope::Repo, "notes.displayRef", "refs/notes/review"),
                ConfigEntry::set(ConfigScope::Repo, "notes.displayRef", "refs/notes/other"),
            ]),
    );
    // The same pair on the command line, which is a different assembler.
    out.push(
        Case::new("log", &["log", "-1", "--notes", "--format=%N"], Shape::NotesReplace).with_config(
            &[
                ("notes.displayRef", "refs/notes/review"),
                ("notes.displayRef", "refs/notes/other"),
            ],
        ),
    );
    // A glob, with no `--notes` on argv at all. `%N` renders the default note
    // whether or not `--notes` was given, and the key *adds* to that list — so
    // stock prints three notes here where a bare `log -1 --format=%N` prints
    // one. Verified on stock 2.55.0:
    //
    //   $ git -c notes.displayRef=refs/notes/* log -1 --format=%N
    //   default note on HEAD
    //   other note on HEAD, conflicting
    //   review note on HEAD
    out.push(
        Case::new("log", &["log", "-1", "--format=%N"], Shape::NotesReplace)
            .with_config(&[("notes.displayRef", "refs/notes/*")]),
    );
    // `--no-notes` beats the configuration; `show` turns notes on by itself, so
    // the same key reaches a second renderer without `--notes`.
    out.push(
        Case::new("log", &["log", "-1", "--no-notes", "--format=%N"], Shape::NotesReplace)
            .with_config(&[("notes.displayRef", "refs/notes/review")]),
    );
    out.push(
        Case::new("show", &["show", "-s", "HEAD"], Shape::NotesReplace)
            .with_config(&[("notes.displayRef", "refs/notes/review")]),
    );
    out.push(
        Case::new("format-patch", &["format-patch", "--notes", "--stdout", "-1"], Shape::NotesReplace)
            .with_config(&[("notes.displayRef", "refs/notes/review")]),
    );
    // `core.notesRef` moves the default on both layers.
    out.push(
        Case::new("log", &["log", "-1", "--notes", "--format=%N"], Shape::NotesReplace)
            .with_config(&[("core.notesRef", "refs/notes/review")]),
    );
    out.push(
        Case::new("notes", &["notes", "list"], Shape::NotesReplace)
            .with_config(&[("core.notesRef", "refs/notes/review")]),
    );
    out.push(
        Case::new("notes", &["notes", "list"], Shape::NotesReplace)
            .with_config(&[("core.notesRef", "refs/notes/nope")]),
    );
}

// ---------------------------------------------------------------------------
// `notes merge`: the strategy from configuration, and a non-notes ref
// ---------------------------------------------------------------------------

/// The two `mergeStrategy` keys, and what happens when the ref being merged is
/// not a notes ref at all.
///
/// `fixture_gaps2.rs:471` owns `notes merge other` and the four `-s <strategy>`
/// spellings on this shape. What it cannot reach is the *other* source of the
/// same decision: `builtin/notes.c:git_config_get_notes_strategy` reads
/// `notes.mergeStrategy` for every merge and `notes.<name>.mergeStrategy` for
/// the ref being merged into, and a port that implements `-s` and ignores both
/// keys falls back to `manual`, conflicts, and exits 1 — which is a loud
/// failure only if something asks.
///
/// `notes merge HEAD` is the odd one and the sharpest. `HEAD` is an ordinary
/// commit, not a notes ref, and stock does not refuse: it merges it and leaves
/// `refs/notes/commits` a **merge commit with `HEAD` as its second parent**,
/// silently, printing nothing and exiting 0. Verified on stock 2.55.0:
///
/// ```text
/// $ git notes merge HEAD ; echo "rc=$?"          # rc=0, no output
/// $ git cat-file -p refs/notes/commits
/// tree 54ee300ee6e35a8cdb404ca8b038abfd87e914b9
/// parent 686803320d5e3588c5fd275ff2f25d94e24ec544
/// parent 7b6d7d59f80ba49d1f9add363d00a1defcdc738f
/// …
/// Merged notes from HEAD into refs/notes/commits
/// ```
///
/// Nothing on stdout distinguishes that from doing nothing at all, so only the
/// state digest can see it.
fn merge_strategy_config(out: &mut Vec<Case>) {
    for strategy in ["ours", "theirs", "union", "cat_sort_uniq", "manual", "bogus"] {
        out.push(
            Case::new("notes", &["notes", "merge", "other"], Shape::NotesReplace)
                .with_config(&[("notes.mergeStrategy", strategy)]),
        );
    }
    // The per-ref key, which names the ref being merged *into* — so it is
    // `notes.commits.*` even though the argument is `other`.
    out.push(
        Case::new("notes", &["notes", "merge", "other"], Shape::NotesReplace)
            .with_config(&[("notes.commits.mergeStrategy", "union")]),
    );
    // Precedence between the two keys, and the option over both.
    out.push(
        Case::new("notes", &["notes", "merge", "other"], Shape::NotesReplace)
            .with_config(&[("notes.mergeStrategy", "ours"), ("notes.commits.mergeStrategy", "theirs")]),
    );
    out.push(
        Case::new("notes", &["notes", "merge", "-s", "union", "other"], Shape::NotesReplace)
            .with_config(&[("notes.mergeStrategy", "ours")]),
    );
    // The per-ref key aimed at a ref that is not the one being merged into: it
    // must not fire.
    out.push(
        Case::new("notes", &["notes", "merge", "other"], Shape::NotesReplace)
            .with_config(&[("notes.other.mergeStrategy", "theirs")]),
    );
    // A ref that is not a notes ref, merged anyway.
    out.push(Case::strict("notes", &["notes", "merge", "HEAD"], Shape::NotesReplace));
    out.push(Case::strict("notes", &["notes", "merge", "-s", "ours", "HEAD"], Shape::NotesReplace));
    // `-s manual` spelled out, which is the default arm reached through the
    // option table rather than through the fallback. `strict` on exactly this
    // one: a conflicted notes merge names the file the user has to edit, and
    // the port names it `./.git/NOTES_MERGE_WORKTREE` where stock names it
    // `.git/NOTES_MERGE_WORKTREE`. That is a path, not prose.
    out.push(Case::strict("notes", &["notes", "merge", "-s", "manual", "other"], Shape::NotesReplace));
    out.push(Case::new("notes", &["notes", "merge", "refs/notes/review"], Shape::NotesReplace));
}

// ---------------------------------------------------------------------------
// Notes carried across a rewrite
// ---------------------------------------------------------------------------

/// One `<from> <to>` pair, spelled as revisions so no object id is baked in.
/// `HEAD` carries a note on `refs/notes/commits` in [`Shape::NotesReplace`] and
/// `HEAD~2` carries none, so the copy has a source and a free destination.
const REWRITE_FRESH: &[u8] = b"HEAD HEAD~2\n";
/// The same, aimed at a destination that **already has** a note — which is the
/// only input under which `notes.rewriteMode` decides anything.
const REWRITE_OCCUPIED: &[u8] = b"HEAD HEAD~1\n";

/// `notes.rewriteRef`, `GIT_NOTES_REWRITE_REF` and `notes.rewriteMode`: whether
/// a note follows the commit it annotates when that commit is rewritten.
///
/// This is the half of the rewrite machinery that no case reached.
/// `stateful_side_files.rs:462` runs `notes copy --for-rewrite=amend` twice,
/// with and without `notes.rewrite.amend`, and both runs are no-ops because
/// **`notes.rewriteRef` has no default** — `notes.c:init_copy_notes_for_rewrite`
/// returns NULL with nothing to copy to, so the enabling key and the disabling
/// key produced the same empty answer. Setting `notes.rewriteRef` is what turns
/// the machinery on, and once it is on the ordinary porcelain verbs run it:
/// `commit --amend` and `rebase` call `copy_note_for_rewrite` themselves.
///
/// Verified on stock 2.55.0 against [`Shape::NotesReplace`] — with the key set,
/// `commit --amend` moves the notes ref; with it unset, it does not:
///
/// ```text
/// $ git -c notes.rewriteRef=refs/notes/review commit --amend -m amended
/// $ git rev-parse refs/notes/review     # 21736b6c…, was ab297060…
/// $ git commit --amend -m amended       # same fixture, no key
/// $ git rev-parse refs/notes/review     # ab297060…, unchanged
/// ```
///
/// Both spellings of "the note is already there" are covered, because
/// `notes.rewriteMode` only decides between them: with a *fresh* destination
/// every mode copies, and with an *occupied* one `overwrite`, `concatenate`,
/// `cat_sort_uniq` give three different blobs while `ignore` leaves the
/// destination's blob alone — and `ignore` still writes a notes commit, whose
/// tree is byte-identical to the one already on the ref.
fn rewrite_refs(out: &mut Vec<Case>) {
    // ---- the porcelain verbs, which run the machinery without being asked ----
    // Unset: the negative control. `commit --amend` must leave every notes ref
    // exactly where it was.
    out.push(Case::new("commit", &["commit", "--amend", "-m", "amended"], Shape::NotesReplace));
    for target in ["refs/notes/commits", "refs/notes/review", "refs/notes/*"] {
        out.push(
            Case::new("commit", &["commit", "--amend", "-m", "amended"], Shape::NotesReplace)
                .with_config(&[("notes.rewriteRef", target)]),
        );
    }
    // The same answer from the environment rather than from config.
    out.push(
        Case::new("commit", &["commit", "--amend", "-m", "amended"], Shape::NotesReplace)
            .with_env(&[("GIT_NOTES_REWRITE_REF", "refs/notes/review")]),
    );
    // `notes.rewrite.<cmd>` gates the whole thing per verb, and `amend` is the
    // verb here — so this pair is the on/off switch with the ref already set.
    out.push(
        Case::new("commit", &["commit", "--amend", "-m", "amended"], Shape::NotesReplace)
            .with_config(&[("notes.rewriteRef", "refs/notes/commits"), ("notes.rewrite.amend", "false")]),
    );
    out.push(
        Case::new("commit", &["commit", "--amend", "-m", "amended"], Shape::NotesReplace)
            .with_config(&[("notes.rewriteRef", "refs/notes/commits"), ("notes.rewriteMode", "ignore")]),
    );
    // `rebase` is the other porcelain caller, and it is a different call site:
    // it copies for every commit it replays rather than for one.
    out.push(Case::new(
        "rebase",
        &["rebase", "--onto", "HEAD~3", "HEAD~2", "main"],
        Shape::NotesReplace,
    ));
    out.push(
        Case::new("rebase", &["rebase", "--onto", "HEAD~3", "HEAD~2", "main"], Shape::NotesReplace)
            .with_config(&[("notes.rewriteRef", "refs/notes/commits")]),
    );
    out.push(
        Case::new("rebase", &["rebase", "--onto", "HEAD~3", "HEAD~2", "main"], Shape::NotesReplace)
            .with_config(&[("notes.rewriteRef", "refs/notes/commits"), ("notes.rewrite.rebase", "false")]),
    );

    // ---- `notes copy --for-rewrite`, driven from stdin ----
    for cmd in ["amend", "rebase"] {
        out.push(
            Case::with_stdin("notes", &["notes", "copy", "--for-rewrite", cmd], Shape::NotesReplace, REWRITE_FRESH)
                .with_config(&[("notes.rewriteRef", "refs/notes/commits")]),
        );
    }
    // The four modes, all against a destination that already has a note, which
    // is the only input that tells them apart.
    for mode in ["overwrite", "concatenate", "cat_sort_uniq", "ignore"] {
        out.push(
            Case::with_stdin(
                "notes",
                &["notes", "copy", "--for-rewrite", "amend"],
                Shape::NotesReplace,
                REWRITE_OCCUPIED,
            )
            .with_config(&[("notes.rewriteRef", "refs/notes/commits"), ("notes.rewriteMode", mode)]),
        );
    }
    // A mode git does not know: stock complains on stderr and carries on at
    // exit 0 having written nothing. Not `strict` — the diagnostic is prose,
    // and what is being pinned is that a bad value is a complaint rather than a
    // fatal, which the exit code and the unchanged state already say.
    out.push(
        Case::with_stdin(
            "notes",
            &["notes", "copy", "--for-rewrite", "amend"],
            Shape::NotesReplace,
            REWRITE_OCCUPIED,
        )
        .with_config(&[("notes.rewriteRef", "refs/notes/commits"), ("notes.rewriteMode", "nosuchmode")]),
    );
    out.push(
        Case::with_stdin("notes", &["notes", "copy", "--for-rewrite", "amend"], Shape::NotesReplace, REWRITE_FRESH)
            .with_config(&[("notes.rewriteRef", "refs/notes/fresh")]),
    );
    // The environment variable on the same verb, so both delivery paths reach
    // `init_copy_notes_for_rewrite` and not only the config one.
    out.push(
        Case::with_stdin("notes", &["notes", "copy", "--for-rewrite", "amend"], Shape::NotesReplace, REWRITE_FRESH)
            .with_env(&[("GIT_NOTES_REWRITE_REF", "refs/notes/commits")]),
    );
    // `GIT_NOTES_REWRITE_MODE`, which `git-config(1)` documents as the override
    // for `notes.rewriteMode` and which nothing sets anywhere.
    out.push(
        Case::with_stdin("notes", &["notes", "copy", "--for-rewrite", "amend"], Shape::NotesReplace, REWRITE_OCCUPIED)
            .with_config(&[("notes.rewriteRef", "refs/notes/commits")])
            .with_env(&[("GIT_NOTES_REWRITE_MODE", "ignore")]),
    );
    // A **colon-separated list** of refs, which is the form `GIT_NOTES_REWRITE_REF`
    // documents and `notes.rewriteRef` does not have — it is the one thing the
    // variable can express that the config key cannot.
    out.push(
        Case::new("commit", &["commit", "--amend", "-m", "amended"], Shape::NotesReplace)
            .with_env(&[("GIT_NOTES_REWRITE_REF", "refs/notes/commits:refs/notes/review")]),
    );
}

// ---------------------------------------------------------------------------
// What a note can be attached to
// ---------------------------------------------------------------------------

/// Annotating objects that are not commits, and refs that are not notes refs.
///
/// `notes` resolves its object argument with `get_oid`, which does **not**
/// peel: `notes add -m x v0.2.0` on [`Shape::Branched`] annotates the *tag
/// object*, not the commit it points at. Verified on stock 2.55.0 — the note
/// lands on `d7277ea9…`, which is `rev-parse v0.2.0`, and `notes list` reports
/// it under that name rather than under the commit's:
///
/// ```text
/// $ git notes add -m x v0.2.0 && git notes list
/// 587be6b4c3f93f93c489c0111bba5596147a26cb d7277ea97518c8631ff11851f616d1ca422aeef0
/// $ git rev-parse v0.2.0
/// d7277ea97518c8631ff11851f616d1ca422aeef0
/// ```
///
/// A port that peels writes the note where nothing looks for it, and `notes
/// show v0.2.0` afterwards finds nothing — a note stock cannot find is worse
/// than a wrong line on stdout, and it is invisible to any case that only ever
/// annotates `HEAD`.
///
/// The `--ref` half is the same question about the *notes* ref: git qualifies a
/// bare name into `refs/notes/<name>` and leaves an already-qualified one
/// alone, whatever it qualifies to. `--ref=refs/heads/main` therefore points
/// the writer at a branch, and `notes add` rewrites that branch to a notes
/// commit. `stateful_side_files.rs:456` reads through that aiming;
/// this writes through it, which is the destructive half.
fn write_targets(out: &mut Vec<Case>) {
    // Objects that are not commits.
    each(
        Shape::Branched,
        "notes",
        &[
            &["notes", "add", "-m", "on-annotated-tag", "v0.2.0"],
            &["notes", "add", "-m", "on-lightweight-tag", "v0.1.0"],
            &["notes", "add", "-m", "on-tree", "HEAD^{tree}"],
            &["notes", "add", "-m", "on-blob", "HEAD:README.md"],
        ],
        out,
    );
    out.push(Case::new("notes", &["notes", "list", "HEAD^{tree}"], Shape::NotesReplace));
    // Reuse from a blob, which is what `-C` and `-c` actually want. Every
    // existing `-C`/`-c` case hands them a commit and measures the refusal;
    // these hand them a blob and measure the copy. `HEAD:README.md` is also a
    // blob this shape has *replaced*, so the copy runs through the replace
    // mechanism too — stock's note on `HEAD~2` afterwards reads
    // `# replaced readme`, the replacement's content, not `# fixture`.
    each(
        Shape::NotesReplace,
        "notes",
        &[
            &["notes", "add", "-C", "HEAD:README.md", "HEAD~2"],
            &["notes", "add", "-c", "HEAD:README.md", "HEAD~2"],
            &["notes", "append", "-C", "HEAD:README.md", "HEAD"],
        ],
        out,
    );
    // A notes ref that is a branch, written through.
    out.push(Case::new("notes", &["notes", "--ref=refs/heads/main", "add", "-m", "clobber", "HEAD"], Shape::NotesReplace));
    out.push(Case::new("notes", &["notes", "--ref=refs/tags/v9", "add", "-m", "t", "HEAD"], Shape::NotesReplace));
    // The non-default ref reached for every verb that takes `--ref`, on a shape
    // where that ref already has content — the update path rather than the
    // create path.
    each(
        Shape::NotesReplace,
        "notes",
        &[
            &["notes", "--ref=review", "add", "-f", "-m", "z", "HEAD"],
            &["notes", "--ref=review", "remove", "HEAD"],
            &["notes", "--ref=other", "show", "HEAD"],
            &["notes", "--ref=review", "prune", "-n"],
            &["notes", "--ref=refs/notes/*", "list"],
            // A ref that does not exist, one verb at a time: each has its own
            // "no notes ref" arm and they do not agree with each other.
            &["notes", "--ref=nope", "list"],
            &["notes", "--ref=nope", "show", "HEAD"],
            &["notes", "--ref=nope", "remove", "HEAD"],
            &["notes", "--ref=nope", "prune"],
            &["notes", "--ref=nope", "get-ref"],
        ],
        out,
    );
    // Update-path variants of the message assembly, all on notes that already
    // exist — `stateful_side_files.rs:380` measures the same options on the
    // create path.
    each(
        Shape::NotesReplace,
        "notes",
        &[
            &["notes", "append", "--separator=---", "-m", "a", "-m", "b", "HEAD"],
            &["notes", "append", "--stripspace", "-m", "  a  ", "HEAD"],
            &["notes", "append", "--no-stripspace", "-m", "b  ", "HEAD"],
            &["notes", "append", "--allow-empty", "HEAD"],
            &["notes", "add", "--allow-empty", "-m", "", "-f", "HEAD"],
            &["notes", "add", "--no-stripspace", "--allow-empty", "-m", "  ", "HEAD~2"],
            &["notes", "copy", "-f", "HEAD~1", "HEAD"],
            &["notes", "copy", "HEAD~1", "HEAD~2"],
            &["notes", "remove", "--ignore-missing", "HEAD~2", "HEAD"],
            &["notes", "prune", "-n", "-v"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// The editor path, which the hermetic environment makes reachable rather than
// unreachable
// ---------------------------------------------------------------------------

/// `notes edit` and `notes add` with no `-m`/`-F`, over notes that exist.
///
/// `env::harden` pins `GIT_EDITOR=true`, and the reflex is to call that a dead
/// end. It is not. `true` exits 0 having left the template file exactly as git
/// wrote it, so the outcome depends entirely on what git *prefilled* it with —
/// and that differs between a note that exists and one that does not:
///
/// * On [`Shape::Linear`], where `HEAD` has no note, the template is empty, the
///   message strips to nothing, and git adds no note. That is the arm
///   `history_rewrite.rs:372` already measures.
/// * On [`Shape::NotesReplace`], where `HEAD` has one, the template holds the
///   existing note, so the "edit" re-commits the same text. Verified on stock
///   2.55.0: `refs/notes/commits` moves `686803…` → `b5ee2250…` and `notes show
///   HEAD` still prints `default note on HEAD`. The ref moves; the note does
///   not. Nothing on stdout says so.
///
/// So the whole editor arm is measurable here, and only the case where the
/// editor changes the text is not.
fn editor_paths(out: &mut Vec<Case>) {
    each(
        Shape::NotesReplace,
        "notes",
        &[
            &["notes", "edit", "HEAD"],
            &["notes", "edit", "--allow-empty", "HEAD"],
            &["notes", "edit", "-m", "x", "HEAD"],
            &["notes", "add", "HEAD"],
            &["notes", "add", "-f", "HEAD"],
            &["notes", "append", "HEAD"],
            // On a commit that has no note on this ref, so the empty-template
            // arm is reached on the same fixture as the prefilled one.
            &["notes", "edit", "HEAD~2"],
            &["notes", "add", "HEAD~2"],
        ],
        out,
    );
}

// ---------------------------------------------------------------------------
// The widest notes tree a single invocation can build
// ---------------------------------------------------------------------------

/// Ten `<from> <to>` pairs, every destination a distinct object of
/// [`Shape::NotesReplace`] named by a rev rather than an id: two commits, three
/// trees and five blobs. `HEAD` is the source for all of them because it is the
/// only object with a note on `refs/notes/commits` besides `HEAD~1`.
const BULK_PAIRS: &[u8] = b"HEAD HEAD~2\nHEAD HEAD~3\nHEAD HEAD^{tree}\nHEAD HEAD~1^{tree}\n\
HEAD HEAD:README.md\nHEAD HEAD:src/lib.rs\nHEAD HEAD:note1.txt\nHEAD HEAD:note2.txt\n\
HEAD HEAD:note3.txt\nHEAD HEAD:src\n";

/// One invocation, twelve notes, one notes commit.
///
/// Every other write case in the corpus adds one note at a time, so the tree
/// writer is only ever asked to splice a single entry into a tree it just read.
/// `notes copy --stdin` is the one verb that hands it a whole batch, and the
/// result is a twelve-entry tree written in a single commit — the widest one
/// this fixture set can produce, and the closest reachable approach to the
/// fanout question the module header records as unmeasurable.
///
/// What it does measure is the ordering. A tree's entries are sorted by name,
/// the names here are object ids, and the ids arrive in the order the stdin
/// lines are read — which is not sorted. Verified on stock 2.55.0: twelve
/// entries, ascending, and every one of them a full 40-hex name with no fanout.
///
/// ```text
/// $ git notes copy --stdin < pairs ; git ls-tree refs/notes/commits | wc -l
/// 12
/// ```
fn bulk_copy_stdin(out: &mut Vec<Case>) {
    out.push(Case::with_stdin("notes", &["notes", "copy", "--stdin"], Shape::NotesReplace, BULK_PAIRS));
    // The same batch with `-f`, so every destination that already has a note is
    // overwritten rather than refused — a different arm of the same loop.
    out.push(Case::with_stdin("notes", &["notes", "copy", "--stdin", "-f"], Shape::NotesReplace, BULK_PAIRS));
    // And read back through `ls-tree` in the state probe rather than on stdout:
    // the digest's `cat-file --batch-all-objects` carries the tree's id, so a
    // port that writes the same twelve notes at different paths fails here even
    // though `notes list` would print the same twelve lines.
}
