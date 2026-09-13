//! `git merge-file` as a **program**: its exit-code contract, its option
//! grammar, and the byte-level edges of the three files it is handed — plus the
//! two plumbing verbs that drive it from the index, `merge-index` and
//! `merge-one-file`.
//!
//! # How this divides territory with the modules already here
//!
//! Ten modules were read before a case was written here. What each owns, and
//! what is left over:
//!
//! * [`super::merge_family`] is the only other module that runs `merge-file` at
//!   all (`merge-index` and `merge-one-file` have two more owners, below), and
//!   it owns the **merged text**:
//!   the `ZEALOUS_ALNUM` driver's hunk grouping over the `.git/hooks/*.sample`
//!   scripts, the three conflict styles (`--diff3`, `--zdiff3`,
//!   `merge.conflictStyle`), `--ours`/`--theirs`/`--union` as *renderings*, the
//!   `.git/MERGE_MODE` empty-input triple, awkward path names as labels, and
//!   three error paths (a missing operand, an unresolvable `--object-id`, one
//!   operand). Its `merge-index` cases are the argument protocol (`echo` as the
//!   merge program, `-a`, `--`, `-o`, `-o -q`) and `git-merge-one-file` as the
//!   real pairing. **Nothing there measures the exit status as a number**: every
//!   one of its `merge-file` cases is a one-hunk or one-file-wide conflict, so
//!   the corpus only ever saw 0 and 1, and no case pins that N conflicts means
//!   exit N. This module is the exit code, the option grammar around it
//!   (`-L` arity, `--marker-size`, `--diff-algorithm`, `--object-id` as an
//!   output mode), and the input bytes that are not text-shaped.
//! * [`super::merge_strategies`] owns `git merge`'s backends — and, in its
//!   `one_file_driver`, **`merge-one-file`'s seven-argument dispatch**: the
//!   symlink-on-theirs refusal, the two `Not handling case` forms, the
//!   resolving merge, and the plain add. Read that function before adding a
//!   `merge-one-file` case; three written here were deleted as duplicates of
//!   it. What survives in [`merge_one_file_argv_protocol`] is the three arms it
//!   does not reach.
//! * [`super::stdin_plumbing`] owns the `merge-index` argv walk that
//!   `merge_family` left: a bare path with no `--`, `-q` without `-o`, an
//!   unknown path after `--`, and the bare failing merge program
//!   (`merge-index false -a`). The `-q` half of that failure is here.
//! * [`super::merge_ort`] and [`super::merge_dirty`] own `git merge`'s option
//!   table and its dirty-worktree gates. Neither runs a plumbing merge verb.
//! * [`super::conflict_classes`] owns the conflict *classes* (rename/rename,
//!   modify/delete, mode-only, symlink-versus-file) on [`Shape::MergeMatrix`]
//!   through `merge` and `merge-tree`. The two classes reached here —
//!   [`merge_one_file_argv_protocol`]'s mode mismatch and its symlink mode — are
//!   reached by *spelling the stage triple in argv*, which is `merge-one-file`'s
//!   interface and not a tree state; no case here uses `MergeMatrix`.
//! * [`super::patch_equivalence`] owns `merge-tree` in both its modes. No case
//!   here runs `merge-tree`.
//! * [`super::attributes_filters`] owns `merge.<driver>.driver` — an attribute
//!   that *replaces* the text merge with a command. Nothing here configures a
//!   driver; `merge-file` never consults `.gitattributes` in the first place,
//!   which is the point of [`no_worktree_conversion`].
//! * [`super::eol_conversion`] owns `core.autocrlf`/`core.eol`/`core.safecrlf`
//!   as a *conversion*, on the commands that perform one. The single
//!   `core.autocrlf` case here asserts the opposite fact — that `merge-file`
//!   performs **no** conversion, so the key changes nothing — and it is the only
//!   case in this module carrying configuration.
//! * [`super::no_index_diff`] owns `diff --no-index` over [`Shape::NoIndexTrees`]
//!   and grew that shape's binary, missing-final-newline and whitespace pairs.
//!   The same files are read here by a different program; no id collides,
//!   because every case here names `merge-file` and every case there names
//!   `diff`.
//!
//! # The contract this module exists for
//!
//! `builtin/merge-file.c` returns the **number of conflicts** as the process
//! exit status, capped at 127, and 0 for a clean merge. Measured on stock
//! 2.55.0 against the fixtures, every rung of that ladder:
//!
//! | invocation (abbreviated)                                   | exit |
//! |------------------------------------------------------------|------|
//! | `-p ni/b.txt ni/a.txt ni/a.txt` (one side changed)          | 0    |
//! | `-p ni/b.txt ni/a.txt ni/b.txt` (both sides change alike)   | 0    |
//! | `-p ni/a.txt ni/a.txt ni/a.txt` (all three identical)       | 0    |
//! | `-p ni/b.txt ni/a.txt ni/ws_a.txt` (one conflict hunk)      | 1    |
//! | `-p update pre-commit pre-rebase` (two conflict hunks)      | 2    |
//! | the same triple with `-q`                                   | 2    |
//! | the same triple with `--ours` / `--theirs` / `--union`      | 0    |
//! | a binary operand                                            | 255  |
//! | a missing or unreadable operand                             | 255  |
//! | a usage error (`-L` ×4, `--marker-size=abc`, bad algorithm) | 129  |
//! | `--object-id` naming a tree                                 | 128  |
//!
//! The `--ours`/`--theirs`/`--union` row is the sharpest of them: the same three
//! files exit 2 without the flag and 0 with it, because a favoured resolution is
//! not a conflict. A port that returns "1 if anything conflicted" scores
//! identically to git on every case the corpus had before this module.
//!
//! **The 127 cap is not reachable and no case pretends to reach it.** It needs
//! 128 separate conflict hunks in one merge, and that needs two sides that
//! disagree in 128 regions separated by regions where they agree. Three sweeps
//! against stock 2.55.0 say the fixtures cannot supply one:
//!
//!  * all **2184** ordered triples of the fourteen `.git/hooks/*.sample`
//!    scripts — the only near-identical multi-line family in any shape — of
//!    which four exit 2 and the rest exit 1 or 0;
//!  * all **21924** ordered triples of the 29 files in [`Shape::Renamed`],
//!    worktree and git directory together, none of which exits above 2;
//!  * all **50616** ordered triples of the 38 files in [`Shape::Patches`] — the
//!    shape carrying the patches, mailboxes and quilt series, which are the
//!    longest and most self-similar text in any fixture — none above 2 either.
//!
//! The one family that *looks* like it should scale — the 400-line `big.txt`
//! revisions on [`Shape::Packed`] — cannot conflict at all, by construction
//! rather than by measurement: `numbered_with_edits` rewrites a chosen line to
//! one fixed string, so any line two revisions both touch, they touch
//! identically, and every disagreement is one-sided. Reaching 127 would take a
//! shape built for it, and a corpus module cannot add a shape.
//!
//! # `-p` versus writing back, decided per case
//!
//! `merge-file` writes its result into the **first path operand** unless `-p`
//! is given, which is a worktree mutation the state digest sees. The split here
//! is deliberate and is not `merge_family`'s:
//!
//! * **Every case whose operands live under `.git/hooks/` uses `-p`.**
//!   `probe_worktree_content` (runner.rs) does not walk `.git`, so a result
//!   written into a hook sample is invisible to every probe and the case would
//!   assert nothing but an exit code.
//! * **The cases in [`write_back_into_file1`] deliberately do not use `-p`**, and
//!   they write into *tracked* files (`ni/a.txt`, `ni/eol_a.txt`). Those bytes
//!   are asserted twice over — `status --porcelain` reports the modification and
//!   `probe_worktree_content` compares the file byte for byte. `merge_family`'s
//!   header says a write-back case is "kept for its exit code" because that
//!   probe did not exist when it was written; it does now, so the write-back
//!   path is a first-class content surface and is measured as one here.
//! * **`--object-id` without `-p` is a third output mode**, not a write-back: it
//!   prints the merged blob's id on stdout, and writes that blob into the object
//!   store when the result is one the repository does not already have. Both
//!   halves are measured — the digest's `cat-file --batch-check
//!   --batch-all-objects` probe grew by one line for the conflicted merge and by
//!   none for the two clean ones (26 objects to 27, and 26 to 26). See
//!   [`object_id_mode`].
//!
//! # What is measured by hand and cannot be measured here
//!
//! * A **content conflict produced through `merge-one-file`** is
//!   nondeterministic on stock git alone: `git-merge-one-file` unpacks its
//!   stages to `.merge_file_XXXXXX` temp files and hands those names to
//!   `merge-file` as labels, so the markers it writes carry a random suffix.
//!   The harness already reports `merge_family`'s one such case as
//!   `[NONDETERMINISTIC]` and excludes it. Every `merge-one-file` case here was
//!   checked for that leak and none of them writes a temp name: they either
//!   refuse before the merge or resolve without markers.
//! * **stderr** is compared only where the message *is* the behaviour — the
//!   input-error refusals, the usage errors, and the `-q` pairs that differ in
//!   nothing else. `-q` does not change stdout or the exit status of any
//!   invocation; it silences `error: Cannot merge binary files: …` and
//!   `error: Could not stat …`, so without a strict pair `-q` would be
//!   unmeasurable.

use crate::fixture::Shape;
use crate::runner::Case;

/// Hook samples used as multi-line merge inputs. Five of the fourteen, chosen
/// for two properties nothing in the worktree of any shape has.
///
/// `update`/`pre-commit`/`pre-rebase` is one of only **four** ordered triples,
/// out of the 2184 the fourteen scripts can form, that conflict in more than one
/// place (exit 2). The four are that one, its reverse, and the pair that puts
/// `sendemail-validate` where `update` stands — every one of them with
/// `pre-commit` as the base, which is what supplies the single agreeing region
/// between the two conflicts. (Two is also the ceiling everywhere else: the one
/// non-hook triple found that reaches it,
/// `mail/one.eml mail/series.mbox patches/whitespace.patch` on
/// [`Shape::Patches`], gets there the same way and no further. See the module
/// header for the sweeps.)
///
/// `pre-commit`/`push-to-checkout`/`pre-receive` is a triple on which the four
/// `--diff-algorithm` values produce three different merges — see
/// [`diff_algorithm`] for the measured checksums.
///
/// Spelled here rather than imported from [`super::merge_family`] because that
/// module's constants are private to it; the paths themselves exist in every
/// shape, since `git init` writes them.
const UPDATE: &str = ".git/hooks/update.sample";
const PRE_COMMIT: &str = ".git/hooks/pre-commit.sample";
const PRE_REBASE: &str = ".git/hooks/pre-rebase.sample";
const PUSH_TO_CHECKOUT: &str = ".git/hooks/push-to-checkout.sample";
const PRE_RECEIVE: &str = ".git/hooks/pre-receive.sample";

/// The empty blob's object id — a constant of the hash algorithm rather than of
/// any fixture, so naming it in argv ties no case to the fixture's bytes.
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
/// The null object id, which `--object-id` accepts and treats as empty rather
/// than refusing. Measured on stock 2.55.0; see [`object_id_mode`].
const NULL_OID: &str = "0000000000000000000000000000000000000000";

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    exit_code_is_the_conflict_count(out);
    labels_are_positional(out);
    marker_size(out);
    content_edges(out);
    object_id_mode(out);
    write_back_into_file1(out);
    diff_algorithm(out);
    no_worktree_conversion(out);
    merge_index_program_protocol(out);
    merge_one_file_argv_protocol(out);
}

/// The exit status **is** the conflict count. Every rung of the ladder in the
/// module header, on inputs whose count was measured on stock 2.55.0.
fn exit_code_is_the_conflict_count(out: &mut Vec<Case>) {
    // Exit 0, three different ways of being clean: only one side moved, both
    // sides made the same move, and nothing moved at all. A port that reports a
    // conflict for "both sides changed" fails the second; one that compares
    // ours against theirs instead of each against the base fails the third.
    for args in [
        &["merge-file", "-p", "ni/b.txt", "ni/a.txt", "ni/a.txt"],
        &["merge-file", "-p", "ni/b.txt", "ni/a.txt", "ni/b.txt"],
        &["merge-file", "-p", "ni/a.txt", "ni/a.txt", "ni/a.txt"],
    ] {
        out.push(Case::new("merge-file", args, Shape::NoIndexTrees));
    }

    // Exit 1: one conflict hunk. Two different input pairs so the count is not
    // an artifact of one file's shape — three lines that all disagree, and a
    // ten-line C file against a three-line text file.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "ni/b.txt", "ni/a.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "ni/fn_b.c", "ni/fn_a.c", "ni/a.txt"],
        Shape::NoIndexTrees,
    ));

    // Exit 2: one of the four triples in any fixture that conflict in two
    // separate regions (see [`UPDATE`]). `-q` is on the same inputs on purpose —
    // it changes stderr and nothing else, so the pair pins that quiet does not
    // quieten the status.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", UPDATE, PRE_COMMIT, PRE_REBASE],
        Shape::Linear,
    ));
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "-q", UPDATE, PRE_COMMIT, PRE_REBASE],
        Shape::Linear,
    ));

    // Back to 0 on the *same* inputs, because a favoured resolution is not a
    // conflict. This is the pair that separates "count the conflicts" from
    // "did anything conflict".
    for favor in ["--ours", "--theirs", "--union"] {
        out.push(Case::new(
            "merge-file",
            &["merge-file", "-p", favor, UPDATE, PRE_COMMIT, PRE_REBASE],
            Shape::Linear,
        ));
    }
    // Two favours on one command line: the last one wins (measured: `--ours
    // --theirs` yields theirs' lines, and the reverse yields ours').
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "--ours", "--theirs", UPDATE, PRE_COMMIT, PRE_REBASE],
        Shape::Linear,
    ));
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "--theirs", "--ours", UPDATE, PRE_COMMIT, PRE_REBASE],
        Shape::Linear,
    ));

    // 255: an input git will not merge. Both are strict pairs with their `-q`
    // twin, because `-q` suppresses exactly these two messages and changes
    // neither stdout nor the status — without the pair it is unmeasurable.
    for args in [
        &["merge-file", "-p", "ni/bin_a.bin", "ni/bin_b.bin", "ni/bin_a.bin"][..],
        &["merge-file", "-p", "-q", "ni/bin_a.bin", "ni/bin_b.bin", "ni/bin_a.bin"][..],
        &["merge-file", "-p", "ni/da", "ni/a.txt", "ni/b.txt"][..],
        &["merge-file", "-p", "-q", "ni/da", "ni/a.txt", "ni/b.txt"][..],
    ] {
        out.push(Case::strict("merge-file", args, Shape::NoIndexTrees));
    }
    // Binary refuses even when all three sides are byte-identical: the check is
    // "is there a NUL", not "do these differ".
    out.push(Case::strict(
        "merge-file",
        &["merge-file", "-p", "ni/bin_a.bin", "ni/bin_a.bin", "ni/bin_a.bin"],
        Shape::NoIndexTrees,
    ));

    // 129: the option parser refused before any file was opened. Two and four
    // operands — `merge_family` covers one operand, and the arity is a range
    // rather than a minimum.
    out.push(Case::strict(
        "merge-file",
        &["merge-file", "-p", "ni/a.txt", "ni/b.txt"],
        Shape::NoIndexTrees,
    ));
    out.push(Case::strict(
        "merge-file",
        &["merge-file", "-p", "ni/a.txt", "ni/b.txt", "ni/ws_a.txt", "ni/ws_b.txt"],
        Shape::NoIndexTrees,
    ));
}

/// `-L` is positional and capped at three. What happens with fewer, with an
/// empty one, and with a fourth.
fn labels_are_positional(out: &mut Vec<Case>) {
    // One label replaces file1's and leaves file2's as the path; two leave
    // file2's. Measured on stock: the missing labels fall back to the operand
    // spelling, not to a placeholder.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "-L", "mine", "ni/b.txt", "ni/a.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "-L", "mine", "-L", "base", "ni/b.txt", "ni/a.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
    // Under `--diff3` the *second* label is printed on the `|||||||` line, so a
    // one- or two-label invocation is the only way to see which label the
    // ancestor block falls back to.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "--diff3", "-L", "mine", "ni/b.txt", "ni/a.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
    out.push(Case::new(
        "merge-file",
        &[
            "merge-file", "-p", "--diff3", "-L", "mine", "-L", "orig",
            "ni/b.txt", "ni/a.txt", "ni/ws_a.txt",
        ],
        Shape::NoIndexTrees,
    ));
    // An empty label: the marker line is `<<<<<<<` with no trailing space
    // (verified with `xxd`: `3c3c3c3c3c3c3c0a`). A port that formats the line as
    // `"{markers} {label}"` unconditionally leaves a trailing blank and diverges
    // on a byte no human would see.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "-L", "", "ni/b.txt", "ni/a.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
    // A fourth label is a usage error, not a silently ignored argument.
    out.push(Case::strict(
        "merge-file",
        &[
            "merge-file", "-p", "-L", "a", "-L", "b", "-L", "c", "-L", "d",
            "ni/b.txt", "ni/a.txt", "ni/ws_a.txt",
        ],
        Shape::NoIndexTrees,
    ));
    // Labels do *not* reach the binary-file refusal, which names the path even
    // when a label was given for it.
    out.push(Case::strict(
        "merge-file",
        &[
            "merge-file", "-p", "-L", "mine", "-L", "base", "-L", "yours",
            "ni/bin_a.bin", "ni/bin_b.bin", "ni/bin_a.bin",
        ],
        Shape::NoIndexTrees,
    ));
}

/// `--marker-size` decides how wide every marker line is, and what it does with
/// the values that are not a width.
fn marker_size(out: &mut Vec<Case>) {
    // 0 and a negative both fall back to the built-in 7 rather than producing
    // an empty or absent marker — the two values most likely to be special-cased
    // wrongly. 1 and 2 are the widths at which `=` and `==` stop looking like
    // markers at all, and are what a port that hard-codes seven characters gets
    // wrong.
    for size in ["0", "1", "2", "-3", "200"] {
        out.push(Case::new(
            "merge-file",
            &[
                "merge-file", "-p", &format!("--marker-size={size}"),
                "ni/b.txt", "ni/a.txt", "ni/ws_a.txt",
            ],
            Shape::NoIndexTrees,
        ));
    }
    // `k` is git's own integer suffix (parse-options' `OPT_INTEGER` with a
    // magnitude suffix), so this is 1024 markers wide and not an error.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "--marker-size=1k", "ni/b.txt", "ni/a.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
    // The separate-argument spelling, and the `|||||||` marker scaling with the
    // rest under `--diff3`.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "--marker-size", "3", "ni/b.txt", "ni/a.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
    out.push(Case::new(
        "merge-file",
        &[
            "merge-file", "-p", "--diff3", "--marker-size=1",
            "ni/b.txt", "ni/a.txt", "ni/ws_a.txt",
        ],
        Shape::NoIndexTrees,
    ));
    // Not a number: the refusal is the behaviour.
    out.push(Case::strict(
        "merge-file",
        &["merge-file", "-p", "--marker-size=abc", "ni/b.txt", "ni/a.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
}

/// The input bytes that are not three tidy text files: a missing final newline,
/// CRLF on one side only, and an empty side named as an object id.
fn content_edges(out: &mut Vec<Case>) {
    // `ni/eol_b.txt` is `ni/eol_a.txt` without its final newline — the pair
    // `no_index_diff` grew the shape for, read here by the other program that
    // has to decide what a line without a terminator is.
    //
    // Clean in both directions: the side that dropped the newline wins when it
    // is the only one that moved, and the side that added it wins when both did.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "ni/eol_b.txt", "ni/eol_a.txt", "ni/eol_a.txt"],
        Shape::NoIndexTrees,
    ));
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "ni/eol_a.txt", "ni/eol_b.txt", "ni/eol_a.txt"],
        Shape::NoIndexTrees,
    ));
    // Conflicting, with the terminator-less line inside the markers. Verified
    // with `xxd`: git inserts the newline the input did not have, so the
    // `=======` line starts at column 0 — `…6c617374206c696e650a3d3d3d3d3d3d3d`.
    // Both orders, because the fix-up has to happen for whichever side is last
    // as well as for the one in the middle.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "ni/eol_b.txt", "ni/eol_a.txt", "ni/a.txt"],
        Shape::NoIndexTrees,
    ));
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "ni/a.txt", "ni/eol_a.txt", "ni/eol_b.txt"],
        Shape::NoIndexTrees,
    ));

    // CRLF on one side only. No shape has a CRLF file in the worktree —
    // `ws/eol.txt` was committed CRLF and rewritten LF four commits later — so
    // the only way to name both forms in one invocation is `--object-id` over
    // the two blobs. Verified with `xxd`: the CR survives the merge when the
    // CRLF side is the one that moved (`616c7068610d0a…`), and does not when
    // both sides converted it away.
    out.push(Case::new(
        "merge-file",
        &[
            "merge-file", "-p", "--object-id",
            "HEAD~4:ws/eol.txt", "HEAD:ws/eol.txt", "HEAD:ws/eol.txt",
        ],
        Shape::Whitespace,
    ));
    out.push(Case::new(
        "merge-file",
        &[
            "merge-file", "-p", "--object-id",
            "HEAD:ws/eol.txt", "HEAD~4:ws/eol.txt", "HEAD~4:ws/eol.txt",
        ],
        Shape::Whitespace,
    ));
    // CRLF inside conflict markers: the marker lines are LF-terminated while the
    // content around them keeps its CRLF, which is a mixture no other case has.
    out.push(Case::new(
        "merge-file",
        &[
            "merge-file", "-p", "--object-id",
            "HEAD~4:ws/eol.txt", "HEAD:ws/eol.txt", "HEAD:ws/indent.c",
        ],
        Shape::Whitespace,
    ));

    // An empty side, named two ways that are not a file: the empty blob's id,
    // and the null oid — which `--object-id` accepts as empty rather than
    // refusing, measured on stock 2.55.0. `merge_family` reaches the empty input
    // only through `.git/MERGE_MODE`, a zero-byte *file*.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "--object-id", EMPTY_BLOB, "HEAD:ws/indent.c", "HEAD~3:ws/indent.c"],
        Shape::Whitespace,
    ));
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "--object-id", NULL_OID, "HEAD:ws/indent.c", "HEAD~3:ws/indent.c"],
        Shape::Whitespace,
    ));
}

/// `--object-id` is two changes at once: the operands are objects, and without
/// `-p` the *result* is an object too.
fn object_id_mode(out: &mut Vec<Case>) {
    // No `-p`: stdout is the merged blob's id. Nothing in the corpus ran
    // `--object-id` without `-p`.
    //
    // These two are the *clean* half, where the result is a blob the repository
    // already has — the CRLF side taken whole, and a merge of one blob with
    // itself. Measured on stock 2.55.0 with
    // `cat-file --batch-check --batch-all-objects | wc -l`: 26 objects before
    // and 26 after, and the id printed is the operand's own
    // (`b4ec4d1f…` = `HEAD~4:ws/eol.txt`, `4359683e…` = `HEAD:ws/indent.c`). So
    // the assertion is stdout plus the absence of a new object, which is the
    // half a port that hashes unconditionally and writes anyway would fail.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "--object-id", "HEAD~4:ws/eol.txt", "HEAD:ws/eol.txt", "HEAD:ws/eol.txt"],
        Shape::Whitespace,
    ));
    out.push(Case::new(
        "merge-file",
        &[
            "merge-file", "--object-id",
            "HEAD:ws/indent.c", "HEAD:ws/indent.c", "HEAD:ws/indent.c",
        ],
        Shape::Whitespace,
    ));
    // The other half: a conflicted merge in id mode writes a blob that did not
    // exist — 26 objects to 27, `3cfbbe71…` — so the markers inside it are
    // asserted by the digest's `cat-file --batch-all-objects` probe as well as
    // by the id on stdout. The exit code is still the conflict count.
    out.push(Case::new(
        "merge-file",
        &[
            "merge-file", "--object-id",
            "HEAD:ws/indent.c", "HEAD~3:ws/indent.c", "HEAD~1:ws/indent.c",
        ],
        Shape::Whitespace,
    ));
    // `--no-object-id` turns the mode back off, so the same operands are paths
    // again. A port that treats `--object-id` as a latch diverges here.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", "--object-id", "--no-object-id", "ni/b.txt", "ni/a.txt", "ni/b.txt"],
        Shape::NoIndexTrees,
    ));
    // An object that exists and is not a blob: `fatal: unable to read blob
    // object <oid>`, exit 128 — a different refusal from the one an unresolvable
    // name gets, which `merge_family` already covers with `deadbeef`.
    out.push(Case::strict(
        "merge-file",
        &[
            "merge-file", "-p", "--object-id", "HEAD^{tree}",
            "HEAD:ws/indent.c", "HEAD~3:ws/indent.c",
        ],
        Shape::Whitespace,
    ));
    // A path where an object id was expected: `error: object '…' does not
    // exist`, exit 255. The same spelling is a perfectly good operand without
    // `--object-id`, so this is the mode's own failure and not a bad argument.
    out.push(Case::strict(
        "merge-file",
        &[
            "merge-file", "-p", "--object-id", "ws/indent.c",
            "HEAD:ws/indent.c", "HEAD~3:ws/indent.c",
        ],
        Shape::Whitespace,
    ));
}

/// Without `-p` the merged text is written back into the first operand. Every
/// case here writes into a **tracked** file, so the bytes are compared by
/// `probe_worktree_content` and the modification is reported by `status`.
fn write_back_into_file1(out: &mut Vec<Case>) {
    // A conflicted result on disk: markers, labels and all, in a file `status`
    // now calls modified.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "ni/a.txt", "ni/b.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
    // The same with `--diff3`, so the ancestor block is on disk too and the two
    // cases differ in the file's bytes rather than only in its mtime.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "--diff3", "ni/a.txt", "ni/b.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
    // `--no-stdout` is the long spelling of "write it back", and has to survive
    // being given explicitly rather than by omission. Their side is the only one
    // that moved, so the destination ends up holding `ni/fn_b.c`'s bytes —
    // `status` reports `M ni/fn_a.c` and the content probe carries the rest.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "--no-stdout", "ni/fn_a.c", "ni/fn_a.c", "ni/fn_b.c"],
        Shape::NoIndexTrees,
    ));
    // A clean write-back that *removes* the file's final newline: theirs is the
    // side without one, so `ni/eol_a.txt` ends at `…6c696e65` with no `0a`
    // (verified with `xxd`). The mutation is one byte and nothing but a
    // byte-for-byte worktree comparison can see it.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-q", "ni/eol_a.txt", "ni/eol_a.txt", "ni/eol_b.txt"],
        Shape::NoIndexTrees,
    ));
    // Writing back into a file that is also one of the other two operands: the
    // result must be computed before the destination is truncated.
    out.push(Case::new(
        "merge-file",
        &["merge-file", "ni/a.txt", "ni/a.txt", "ni/b.txt"],
        Shape::NoIndexTrees,
    ));
}

/// `--diff-algorithm` on inputs where the four values do not agree.
fn diff_algorithm(out: &mut Vec<Case>) {
    // Measured on stock 2.55.0 over this triple: the default and `myers` and
    // `patience` produce one merge (cksum 878552741, 2268 bytes), `minimal`
    // another (1856641777, 2266) and `histogram` a third (1005077504, 2269).
    // Three distinct results from four values is what makes the option
    // observable at all — on the `ws/indent.c` revisions all four agree, so a
    // port that ignores the flag entirely would pass there.
    let triple = [PRE_COMMIT, PUSH_TO_CHECKOUT, PRE_RECEIVE];
    out.push(Case::new(
        "merge-file",
        &["merge-file", "-p", triple[0], triple[1], triple[2]],
        Shape::Linear,
    ));
    for alg in ["myers", "minimal", "patience", "histogram"] {
        out.push(Case::new(
            "merge-file",
            &[
                "merge-file", "-p", &format!("--diff-algorithm={alg}"),
                triple[0], triple[1], triple[2],
            ],
            Shape::Linear,
        ));
    }
    // An unknown algorithm, and the separate-argument spelling eating the next
    // operand — both land on the same refusal, which names the four accepted
    // values.
    out.push(Case::strict(
        "merge-file",
        &["merge-file", "-p", "--diff-algorithm=bogus", "ni/b.txt", "ni/a.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
    out.push(Case::strict(
        "merge-file",
        &["merge-file", "-p", "--diff-algorithm", "ni/b.txt", "ni/a.txt", "ni/ws_a.txt"],
        Shape::NoIndexTrees,
    ));
}

/// `merge-file` reads its operands raw: no attribute lookup, no eol filter, no
/// `core.autocrlf`.
///
/// The assertion is that the configuration changes **nothing** — the CRLF blob
/// merges to CRLF with `core.autocrlf=true` exactly as it does without it
/// (measured on stock 2.55.0, byte-identical output). A port that routes these
/// reads through its checkout conversion would quietly strip the CRs here and
/// nowhere else.
fn no_worktree_conversion(out: &mut Vec<Case>) {
    for value in ["true", "input"] {
        out.push(
            Case::new(
                "merge-file",
                &[
                    "merge-file", "-p", "--object-id",
                    "HEAD~4:ws/eol.txt", "HEAD:ws/eol.txt", "HEAD:ws/eol.txt",
                ],
                Shape::Whitespace,
            )
            .with_config(&[("core.autocrlf", value)]),
        );
    }
}

/// `merge-index` runs a program per unmerged path. What it does with that
/// program's **exit status**, and which paths it will accept.
fn merge_index_program_protocol(out: &mut Vec<Case>) {
    // A program that succeeds, on the same unmerged entry the failing one runs
    // against. `merge_family` drives `echo` and `git-merge-one-file`, both of
    // which succeed but also *print*, so "the program ran and said nothing"
    // had no case.
    out.push(Case::new("merge-index", &["merge-index", "true", "-a"], Shape::Conflicted));
    // The bare failing program — `fatal: merge program failed`, exit 128 — is
    // [`super::stdin_plumbing`]'s (`merge-index false -a`, strict). What is left
    // is what `-q` does to it: demotes the fatal to a silent exit 1. The pair
    // below is the rest of that contract — same failing program, a different
    // exit depending on the flags.
    out.push(Case::strict("merge-index", &["merge-index", "-q", "false", "-a"], Shape::Conflicted));
    out.push(Case::strict(
        "merge-index",
        &["merge-index", "-o", "-q", "false", "-a"],
        Shape::Conflicted,
    ));

    // A path that is *merged*: accepted, and the program is never run — visible
    // because `echo` would otherwise have printed the stage triple.
    out.push(Case::new("merge-index", &["merge-index", "echo", "README.md"], Shape::Conflicted));
    // A path that is not in the index at all: `not in the cache`, exit 128.
    out.push(Case::strict(
        "merge-index",
        &["merge-index", "echo", "no-such-path.txt"],
        Shape::Conflicted,
    ));
    // `-a` *and* a path: `-a` runs first over every unmerged entry, then the
    // stray operand is still validated and still fatal. The stdout from the
    // first half is kept, which is what makes the ordering observable.
    out.push(Case::strict("merge-index", &["merge-index", "echo", "-a", "extra"], Shape::Conflicted));
}

/// The three arms of `merge-one-file`'s seven-argument dispatch that
/// [`super::merge_strategies`]'s `one_file_driver` does not reach: an add/add
/// whose sides disagree about the **mode**, the same with a symlink mode on
/// *our* side, and an argv too short to dispatch on at all.
///
/// Both are checked not to write a `.merge_file_XXXXXX` label into the worktree
/// — see the module header. The mode-mismatch arm is safe for the reason the
/// permissions branch *with* a base is not: with an empty orig the script never
/// gets a third file to unpack, so it refuses with no temp name in hand.
/// Verified by running each twice against a fresh fixture and by the harness's
/// own stock-versus-stock pass, which reports neither as `NONDETERMINISTIC`.
///
/// The stage triple is named with revision syntax rather than raw ids so no case
/// is pinned to the fixture's bytes.
fn merge_one_file_argv_protocol(out: &mut Vec<Case>) {
    // Add/add — no base — with the two sides claiming different modes. Both a
    // content conflict and a permission conflict are reported, and the index is
    // left unmerged.
    out.push(Case::new(
        "merge-one-file",
        &[
            "merge-one-file", "", "main:conflict.txt", "theirs:conflict.txt",
            "conflict.txt", "", "100644", "100755",
        ],
        Shape::Conflicted,
    ));
    // A symlink mode on **our** side, with no base. [`super::merge_strategies`]
    // owns the mirror image — a real base and `120000` on theirs — and the two
    // reach the refusal through different arms of the script's
    // `case "${1:-.}${2:-.}${3:-.}"` dispatch, which is why both are worth
    // having: the add/add arm has to notice the mode before it decides there is
    // nothing to merge.
    out.push(Case::new(
        "merge-one-file",
        &[
            "merge-one-file", "", "main:conflict.txt", "theirs:conflict.txt",
            "conflict.txt", "", "120000", "100644",
        ],
        Shape::Conflicted,
    ));
    // Four arguments: the usage message, which the script prints twice — once
    // for the arity check and once for the trailing explanation.
    out.push(Case::new(
        "merge-one-file",
        &[
            "merge-one-file", "main^:README.md", "main:conflict.txt", "theirs:conflict.txt",
            "conflict.txt",
        ],
        Shape::Conflicted,
    ));
}
