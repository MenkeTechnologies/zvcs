//! Differential corpus cases for the stateful_side_files subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! The six verbs whose behaviour lives in files *beside* the object store:
//! `rerere`, `replace`, `notes`, `bisect`, `worktree` and `filter-branch`.
//! Every one of them is a verb a port can implement as a stub and still score
//! well on a logical probe, because the thing they write is not a commit, a tree
//! or a blob — it is a cache directory, a ref namespace nothing else reads, a
//! root state file, or an administrative directory for a second working tree.
//!
//! # Which side files the probe actually reads, and how that shaped the cases
//!
//! `runner::probe_state` is the whole assertion for a verb that prints nothing,
//! so what it reads decides what can be measured. Enumerated from the source
//! rather than assumed:
//!
//! * **`probe_rr_cache` (runner.rs:2609)** walks `.git/rr-cache/**` and compares
//!   every file **byte for byte**. So the preimage a `rerere` record writes is a
//!   first-class assertion — but only on a shape that is *mid-conflict*, because
//!   nothing else fills that directory. [`Shape::Conflicted`] is the only shape
//!   with stage 1/2/3 entries in the index, so it is the only shape on which
//!   `rerere` can record anything at all. The [`Shape::CrissCross`] rows below
//!   are deliberately the opposite assertion: its tip is checked out clean, so a
//!   port that creates `rr-cache` eagerly — the easy way to make `rerere status`
//!   "work" — is caught there and nowhere else.
//! * **`OP_STATE_FILES` (runner.rs:2380)** reads the contents of `MERGE_RR` and
//!   of all nine `BISECT_*` root files (`BISECT_ANCESTORS_OK`,
//!   `BISECT_EXPECTED_REV`, `BISECT_FIRST_PARENT`, `BISECT_HEAD`, `BISECT_LOG`,
//!   `BISECT_NAMES`, `BISECT_RUN`, `BISECT_START`, `BISECT_TERMS`). That is what
//!   makes a single `bisect` invocation worth writing: `bisect start` and
//!   `bisect replay` leave their whole session in those files, and the probe
//!   compares the bytes, not the presence.
//! * **`for-each-ref`** covers `refs/replace/*`, `refs/notes/*` and
//!   `refs/bisect/*` in the main worktree, which is what makes `replace` and
//!   `notes` measurable at all — their entire visible effect is one ref and the
//!   objects under it, and `cat-file --batch-all-objects` sees those.
//! * **`config --list --local`** covers `branch.<name>.*` written by
//!   `worktree add --track`, and `extensions.worktreeConfig`.
//!
//! ## What the probe cannot see, recorded rather than left to be inferred
//!
//! * **`.git/worktrees/<id>/`** is read by nothing. `OP_STATE_DIRS`
//!   (runner.rs:2420) lists `NOTES_MERGE_WORKTREE`, `rebase-apply`,
//!   `rebase-merge` and `sequencer` — not `worktrees`. So the `locked` file
//!   `worktree lock` writes, the `gitdir` file `repair` rewrites, and the
//!   per-worktree `HEAD`/`index` are all invisible to the state comparison.
//!   Only `git_fingerprint` notices they moved, and that opens the interop gate
//!   without comparing their content. Every `lock`/`unlock`/`repair` case below
//!   is therefore an **exit-code and stderr** assertion, and is written
//!   `Case::strict` where the message is the whole answer. Where the state
//!   matters the case is chosen so a *listing* reveals it instead — `worktree
//!   list --porcelain` prints `locked` and `detached` from those same files.
//! * **A locked worktree cannot be set up.** No shape ships one and a case is
//!   one argv, so `worktree remove` on a locked tree — git's own documented
//!   refusal in `builtin/worktree.c` — is unreachable from here. The
//!   corresponding refusals that *are* reachable stand in: `remove .` on the
//!   main tree, `lock .` on the main tree, `unlock` on a tree that is not
//!   locked.
//! * **`bisect` state written from a linked worktree** lands in
//!   `.git/worktrees/wt/BISECT_*`, and `refs/bisect/*` is a per-worktree ref
//!   namespace, so neither `probe_op_state` nor `for-each-ref` at the fixture
//!   root sees it. The one `.in_dir("wt")` bisect case below is scored on its
//!   stdout and exit code alone.
//! * **`--no-replace-objects` / `GIT_NO_REPLACE_OBJECTS` have nothing to
//!   suppress.** No shape carries a `refs/replace/*` ref and a case cannot
//!   create one before the invocation being measured, so the flag's *effect* is
//!   not reachable; the two rows below pin that it parses and does not perturb
//!   the write path. The same argument retires `notes.displayRef`,
//!   `notes.rewriteRef` and `log --notes=`: they only change how an *existing*
//!   note is displayed or copied, and a pristine fixture has none.
//! * **`notes.rewrite.<cmd>` / `notes.rewriteMode`** are consulted by
//!   `notes copy --for-rewrite=<cmd>` (`builtin/notes.c`), which is reachable
//!   — it reads its `<old> <new>` pairs from stdin — but with no source note to
//!   copy the two settings can only be measured as parse and early-exit.
//!
//! # Fixture constraints
//!
//! * **One argv against a pristine copy.** Multi-step workflows belong to
//!   `corpus::sequences`; nothing here may depend on a previous invocation.
//!   That is why `rerere` is only ever measured on its record and report paths
//!   and never on its *replay* path, and why the `worktree add`-then-`remove`
//!   pairing lives in `sequences.rs:1324` rather than here.
//! * **No literal object ids.** Revision names are used everywhere, including
//!   inside the `bisect replay` payloads. That is not only tidiness: a replay
//!   log line `git bisect good <rev>` becomes the ref `refs/bisect/good-<rev>`
//!   verbatim, so a rev containing `~` or `^` makes stock reject its own ref
//!   name (`refusing to update ref with bad name`). Branch names are used for
//!   the `good`/`old` side for exactly that reason.
//! * **`Shape::Worktree` hides `wt/` from status** via `.git/info/exclude`
//!   (fixture.rs:998), so a `worktree remove wt` leaves no trace in
//!   `status --porcelain -uall`. A `worktree add <new>` or `worktree move wt
//!   <new>` does, because the new path is not excluded — which is why the
//!   `move` case targets `wt2` and not some path inside `wt`.
//! * **`filter-branch` is in `SLEEP_ALLOWANCE` (runner.rs:1495)** because the
//!   script sleeps ten seconds printing its deprecation banner. Every case here
//!   sets `FILTER_BRANCH_SQUELCH_WARNING=1` through `Case::with_env`, which
//!   removes the sleep on both sides symmetrically and is not one of the
//!   variables `env::harden` pins. The count is still kept to six, because the
//!   script also prints `(N seconds passed…)` on stdout and stock does not
//!   always reproduce its own output.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// [`Case::strict`] with a stdin payload.
///
/// The two constructors do not compose — each builds a whole `Case` — and
/// several rows here need both halves at once: a refusal whose message *is* the
/// answer, driven by input no fixture can hold. Written here rather than as a
/// seventh constructor in `runner.rs` because this is the only module that wants
/// the combination.
fn strict_stdin(cmd: &'static str, args: &[&str], shape: Shape, stdin: &'static [u8]) -> Case {
    Case { compare_stderr: true, ..Case::with_stdin(cmd, args, shape, stdin) }
}

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    rerere(out);
    replace(out);
    notes(out);
    bisect(out);
    worktree(out);
    filter_branch(out);
}

// ---------------------------------------------------------------------------
// rerere
// ---------------------------------------------------------------------------

/// `rerere`: the gate that decides whether anything is written at all, and the
/// bytes that get written when it opens.
///
/// `corpus::merge_family::rerere` already covers the five verbs with the feature
/// off, and the same five with `-c rerere.enabled=true` on the command line.
/// What is added here is the *gate itself* — `rerere.c`'s `is_rerere_enabled` reads
/// `rerere.enabled` as a tri-state (unset, false, true) and falls back to "the
/// `rr-cache` directory exists" when it is unset — plus the two settings that
/// change what the record and the collect do, and the one shape where recording
/// must **not** happen.
///
/// Every enabled-and-conflicted row asserts through `probe_rr_cache`
/// (runner.rs:2609), which compares `.git/rr-cache/<hash>/preimage` byte for
/// byte, and through the `MERGE_RR` line of `probe_op_state`, which holds the
/// `<hash>\tconflict.txt` mapping. A port that prints
/// `Recorded preimage for 'conflict.txt'` and writes neither file scores a
/// state diff rather than a match — before this the only rows that could catch
/// that were the five on the command-line scope.
fn rerere(out: &mut Vec<Case>) {
    // The tri-state. `false` is not the same code path as unset: unset falls
    // through to the directory-existence fallback, `false` short-circuits.
    // `1` is the same value spelled as a numeric bool, which `git_config_bool`
    // accepts and a hand-rolled string comparison against "true" does not.
    out.push(Case::new("rerere", &["-c", "rerere.enabled=false", "rerere"], Shape::Conflicted));
    out.push(Case::new("rerere", &["-c", "rerere.enabled=1", "rerere"], Shape::Conflicted));
    out.push(Case::new("rerere", &["-c", "rerere.enabled=0", "rerere", "status"], Shape::Conflicted));

    // The gate delivered from a *file* rather than from argv. `-c` and
    // `.git/config` reach `is_rerere_enabled` through different scopes, and a
    // port that only honours the command line matches every existing row and
    // fails these two.
    let repo_on = || vec![ConfigEntry::set(ConfigScope::Repo, "rerere.enabled", "true")];
    out.push(Case::new("rerere", &["rerere"], Shape::Conflicted).with_scoped_config(repo_on()));
    out.push(Case::new("rerere", &["rerere", "status"], Shape::Conflicted).with_scoped_config(repo_on()));
    out.push(Case::new("rerere", &["rerere"], Shape::Conflicted).with_scoped_config(vec![
        ConfigEntry::set(ConfigScope::Global, "rerere.enabled", "true"),
    ]));
    // Repo says off, command line says on: last scope wins, so this records.
    out.push(
        Case::new("rerere", &["-c", "rerere.enabled=true", "rerere"], Shape::Conflicted)
            .with_scoped_config(vec![ConfigEntry::set(ConfigScope::Repo, "rerere.enabled", "false")]),
    );

    // `rerere.autoUpdate` without `rerere.enabled` must record nothing:
    // autoupdate governs whether a *resolved* path is staged, not whether the
    // feature is on. A port that treats any `rerere.*` key as an enable diverges
    // on the first of these and matches the second.
    out.push(Case::new("rerere", &["-c", "rerere.autoupdate=true", "rerere"], Shape::Conflicted));
    out.push(Case::new(
        "rerere",
        &["-c", "rerere.enabled=true", "-c", "rerere.autoupdate=true", "rerere"],
        Shape::Conflicted,
    ));
    out.push(Case::new(
        "rerere",
        &["-c", "rerere.enabled=true", "-c", "rerere.autoupdate=false", "rerere", "remaining"],
        Shape::Conflicted,
    ));

    // `forget` writes `MERGE_RR` even when it has nothing to forget, and its
    // diagnostic is the whole answer — hence `strict`. The pathspec forms are
    // separate rows because `rerere forget` runs its argument through
    // `parse_pathspec` (`builtin/rerere.c`): a literal path, `.`, and the
    // deprecated no-pathspec form each take a different branch.
    out.push(Case::strict(
        "rerere",
        &["-c", "rerere.enabled=true", "rerere", "forget", "."],
        Shape::Conflicted,
    ));
    out.push(Case::strict("rerere", &["-c", "rerere.enabled=true", "rerere", "forget"], Shape::Conflicted));
    out.push(Case::new(
        "rerere",
        &["-c", "rerere.enabled=true", "rerere", "forget", "nosuch.txt"],
        Shape::Conflicted,
    ));
    out.push(Case::new(
        "rerere",
        &["-c", "rerere.enabled=true", "rerere", "forget", "--", "conflict.txt"],
        Shape::Conflicted,
    ));

    // `gc` reads `gc.rerereResolved` and `gc.rerereUnresolved` as day counts
    // (`builtin/rerere.c`). Zero means "expire everything now", so the two
    // rows differ in whether a cache entry would survive — on an empty cache
    // both are silent successes, and the assertion is that neither one creates
    // the directory it was about to prune.
    out.push(Case::new(
        "rerere",
        &["-c", "rerere.enabled=true", "-c", "gc.rerereresolved=0", "-c", "gc.rerereunresolved=0", "rerere", "gc"],
        Shape::Conflicted,
    ));
    out.push(Case::new(
        "rerere",
        &["-c", "rerere.enabled=true", "-c", "gc.rerereresolved=90", "-c", "gc.rerereunresolved=30", "rerere", "gc"],
        Shape::Conflicted,
    ));

    // `CrissCross` is checked out clean at `cc-left`, so there is no conflict to
    // record and `MERGE_RR` does not exist. These rows assert the *negative*:
    // enabled or not, nothing under `.git/rr-cache` may appear. Verified against
    // stock 2.55.0 — all four are silent, exit 0, and leave the tree untouched.
    for verb in ["status", "diff", "remaining"] {
        out.push(Case::new("rerere", &["-c", "rerere.enabled=true", "rerere", verb], Shape::CrissCross));
    }
    out.push(Case::new("rerere", &["-c", "rerere.enabled=true", "rerere"], Shape::CrissCross));
}

// ---------------------------------------------------------------------------
// replace
// ---------------------------------------------------------------------------

/// `replace`: one ref under `refs/replace/`, sometimes one new commit object,
/// and a type check nobody exercises.
///
/// `corpus::history_rewrite::replace` covers `--list`, `-l`, `--format=long`,
/// the two-argument form, `-f`, three `--graft` forms, `--edit`,
/// `--convert-graft-file` and three error paths. What is added here is the rest
/// of `builtin/replace.c`: the two remaining `--format` values and the invalid
/// one, `-l`'s pattern argument, `--raw`, the object-type gate
/// (`replace_object_oid` in `builtin/replace.c`, which refuses to replace a commit
/// with a tag), the parent-list rewrites a `--graft` produces on shapes with
/// more than one parent, and the self-replacement that makes stock's own
/// `for-each-ref` fail afterwards.
///
/// Every row asserts through `for-each-ref` (the new `refs/replace/<oid>` ref)
/// and `cat-file --batch-check --batch-all-objects` (the rewritten commit
/// `--graft` synthesizes). The commit id a `--graft` produces is a function of
/// the pinned identity and date, so it is a legitimate assertion target.
fn replace(out: &mut Vec<Case>) {
    // The format vocabulary. `long` with `--list` is already covered; these are
    // the two that are not, plus the value that is rejected.
    out.push(Case::new("replace", &["replace", "--format=short", "--list"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "--format=medium", "--list"], Shape::Branched));
    out.push(Case::strict("replace", &["replace", "--format=bogus", "--list"], Shape::Branched));
    // `-l <pattern>`: the argument is a ref *glob* matched against
    // `refs/replace/*`, not an object name. Both of these match nothing, which
    // is the point — a port that treats the pattern as a rev fails to parse it.
    out.push(Case::new("replace", &["replace", "-l", "main"], Shape::Branched));
    // A repository with a dangling ref and a corrupt loose object: listing
    // replacements must not walk into either.
    out.push(Case::new("replace", &["replace", "-l"], Shape::Damaged));

    // The type gate, both directions. An annotated tag and the commit it points
    // at are different object types, and the message names both types — so the
    // whole answer is the diagnostic and these are strict.
    out.push(Case::strict("replace", &["replace", "HEAD", "v0.2.0"], Shape::Branched));
    out.push(Case::strict("replace", &["replace", "v0.2.0", "HEAD"], Shape::Branched));

    // `--edit` with `GIT_EDITOR=true`: the editor leaves the buffer untouched,
    // so git refuses with "new object is the same as the old one". `--raw`
    // changes what is written *into* that buffer (the object's raw bytes rather
    // than the pretty-printed form) and must reach the same refusal.
    out.push(Case::strict("replace", &["replace", "--raw", "--edit", "HEAD"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "--edit", "HEAD~1"], Shape::Branched));

    // `--graft` rewrites the parent list, so each of these synthesizes a *new
    // commit object* as well as the ref. The interesting axis is how many
    // parents the original had and how many the graft leaves:
    //   Merged   2 -> 0, and 1 -> 1 with the parents swapped;
    //   Octopus  4 -> 0 (four parent lines removed from one commit);
    //   Branched 1 -> 2 (a parent list grows).
    out.push(Case::new("replace", &["replace", "--graft", "HEAD"], Shape::Octopus));
    out.push(Case::new("replace", &["replace", "--graft", "HEAD", "main~1", "feature"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "--graft", "HEAD^2", "HEAD^1"], Shape::Merged));
    // A duplicate parent in the argument list: git keeps both.
    out.push(Case::new("replace", &["replace", "--graft", "HEAD", "HEAD~1", "HEAD~1"], Shape::Branched));
    // The graft that changes nothing, so the refusal is reached instead.
    out.push(Case::strict("replace", &["replace", "--graft", "HEAD~1"], Shape::Branched));
    // `--graft` on a *tag*: the argument is peeled to the commit, and the ref
    // that appears is named after the commit, not the tag.
    out.push(Case::new("replace", &["replace", "--graft", "v0.2.0"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "-f", "--graft", "HEAD"], Shape::Merged));

    // `<object> <replacement>` in the direction the existing corpus does not
    // take, and the degenerate self-replacement. The latter is deliberate: it
    // leaves a repository where stock git's *own* `for-each-ref` dies with
    // `replace depth too high`, so the state digest is a row of `<err>` markers
    // on both sides — which is only a match if the port also created the ref.
    out.push(Case::new("replace", &["replace", "HEAD~1", "HEAD"], Shape::Branched));
    out.push(Case::new("replace", &["replace", "HEAD", "HEAD"], Shape::Branched));

    // `--convert-graft-file` with no `.git/info/grafts` to convert, on a second
    // shape, and `-d` on a name that resolves to nothing versus `-d` with no
    // argument at all — the first is a ref-resolution failure (exit 1), the
    // second a usage error (exit 129), and a port that collapses them diverges.
    out.push(Case::strict("replace", &["replace", "-d", "nosuch"], Shape::Branched));
    out.push(Case::strict("replace", &["replace", "-d"], Shape::Branched));

    // `--no-replace-objects` and its environment twin. No shape carries a
    // replacement, so these cannot show the flag *suppressing* anything; what
    // they pin is that neither spelling perturbs the write path — the ref and
    // the synthesized commit must be identical to the rows above.
    out.push(
        Case::new("replace", &["replace", "--graft", "HEAD"], Shape::Merged)
            .with_globals(&[&["--no-replace-objects"]]),
    );
    out.push(
        Case::new("replace", &["replace", "--graft", "HEAD"], Shape::Merged)
            .with_env(&[("GIT_NO_REPLACE_OBJECTS", "1")]),
    );
}

// ---------------------------------------------------------------------------
// notes
// ---------------------------------------------------------------------------

/// A note payload delivered on stdin, with a paragraph break in it so
/// `--stripspace`'s blank-line collapsing has something to act on.
const NOTE_STDIN: &[u8] = b"note from stdin\n\n\nsecond paragraph\n\n";
/// A payload that is only whitespace: `notes add -F -` treats an empty message
/// as "remove the note", which is a different branch from a non-empty one.
const NOTE_BLANK_STDIN: &[u8] = b"\n \n\t\n";
/// One `<from> <to>` pair for `notes copy --stdin` and `--for-rewrite`, spelled
/// with revision names so no object id is baked into the corpus.
const COPY_PAIR_STDIN: &[u8] = b"HEAD~1 HEAD\n";
/// A line that is not a pair of revisions, for the parse-failure branch.
const COPY_GARBAGE_STDIN: &[u8] = b"garbage line\n";
/// One revision per line, for `notes remove --stdin`.
const REMOVE_STDIN: &[u8] = b"HEAD\n";

/// `notes`: `refs/notes/<ref>`, the tree under it, and the four places the ref
/// name can come from.
///
/// `corpus::history_rewrite::notes` covers `add`/`append`/`copy`/`list`/
/// `get-ref`/`prune`/`edit`/`show`/`remove`, `-m`, `-f`, `-C`, `--ref=` and six
/// error paths. What is added here is everything that decides *what bytes the
/// note blob holds* and *which ref it lands on* — the two questions a port that
/// only handles `-m` never has to answer:
///
///  * **Message assembly.** `--separator`, `--no-separator`, `--stripspace`,
///    `--no-stripspace` and `--allow-empty` are all handled in
///    `builtin/notes.c`'s `parse_msg_arg` / `concat_messages`, and each one changes
///    the blob content and therefore its id. `cat-file --batch-all-objects` in
///    `probe_state` sees the id; nothing prints it.
///  * **`-F -`.** The file argument `-` means stdin, which the harness can now
///    supply. This is the only way to put a multi-paragraph or whitespace-only
///    payload into a note from a single invocation.
///  * **Ref selection.** `--ref=`, `core.notesRef` and `GIT_NOTES_REF` are three
///    scopes for one answer, and `notes get-ref` prints it. The precedence row
///    sets the environment *and* the option, which is the only row that
///    separates a port that reads them in git's order from one that reads them
///    in any order.
///  * **stdin-driven bulk verbs.** `copy --stdin`, `copy --for-rewrite=<cmd>`
///    and `remove --stdin` each parse a stream of revisions;
///    `--for-rewrite` additionally consults `notes.rewrite.<cmd>` and
///    `notes.rewriteMode`.
fn notes(out: &mut Vec<Case>) {
    // ---- message assembly: same verb, different blob ----
    out.push(Case::new("notes", &["notes", "add", "--allow-empty", "HEAD"], Shape::Linear));
    // An empty `-m` without `--allow-empty` still writes a blob: git only treats
    // an *editor-supplied* empty message as a removal.
    out.push(Case::new("notes", &["notes", "add", "-m", "", "HEAD"], Shape::Branched));
    // Whitespace handling, the two directions. `--stripspace` trims trailing
    // blanks and collapses runs of blank lines; `--no-stripspace` stores the
    // bytes as given, including the missing trailing newline.
    out.push(Case::new("notes", &["notes", "add", "--no-stripspace", "-m", "a  ", "HEAD"], Shape::Branched));
    out.push(Case::new("notes", &["notes", "add", "--stripspace", "-m", "  a  ", "HEAD"], Shape::Branched));
    // The paragraph separator between repeated `-m` arguments: default (blank
    // line), explicit string, explicitly empty, and suppressed.
    out.push(Case::new(
        "notes",
        &["notes", "add", "-m", "one", "-m", "two", "--separator=+++", "HEAD"],
        Shape::Branched,
    ));
    out.push(Case::new("notes", &["notes", "add", "--separator=", "-m", "one", "-m", "two", "HEAD"], Shape::Branched));
    out.push(Case::new(
        "notes",
        &["notes", "append", "--no-separator", "-m", "one", "-m", "two", "HEAD"],
        Shape::Branched,
    ));

    // ---- `-F -`: the payload arrives on stdin ----
    out.push(Case::with_stdin("notes", &["notes", "add", "-F", "-", "HEAD"], Shape::Linear, NOTE_STDIN));
    out.push(Case::with_stdin(
        "notes",
        &["notes", "add", "--no-stripspace", "-F", "-", "HEAD"],
        Shape::Branched,
        NOTE_STDIN,
    ));
    // A whitespace-only payload strips down to nothing, which git reports as a
    // removal even though there was no note — the diagnostic is the answer.
    out.push(strict_stdin("notes", &["notes", "add", "-F", "-", "HEAD"], Shape::Linear, NOTE_BLANK_STDIN));
    out.push(Case::with_stdin(
        "notes",
        &["notes", "add", "--allow-empty", "-F", "-", "HEAD"],
        Shape::Linear,
        NOTE_BLANK_STDIN,
    ));
    // `-F` naming a file that is not there, so the read failure is reached
    // rather than the stdin path.
    out.push(Case::strict("notes", &["notes", "add", "-F", "no-such-file", "HEAD"], Shape::Linear));

    // ---- which ref the note lands on ----
    out.push(Case::new("notes", &["-c", "core.notesRef=refs/notes/cfg", "notes", "add", "-m", "viaconfig", "HEAD"], Shape::Branched));
    out.push(
        Case::new("notes", &["notes", "add", "-m", "viaenv", "HEAD"], Shape::Branched)
            .with_env(&[("GIT_NOTES_REF", "refs/notes/env")]),
    );
    out.push(
        Case::new("notes", &["notes", "get-ref"], Shape::Branched)
            .with_env(&[("GIT_NOTES_REF", "refs/notes/env")]),
    );
    // Precedence: `--ref` outranks the environment, which outranks
    // `core.notesRef`. One row, three answers, and only one of them is right.
    out.push(
        Case::new(
            "notes",
            &["-c", "core.notesRef=refs/notes/cfg", "notes", "--ref=refs/notes/opt", "add", "-m", "both", "HEAD"],
            Shape::Branched,
        )
        .with_env(&[("GIT_NOTES_REF", "refs/notes/env")]),
    );
    out.push(
        Case::new("notes", &["-c", "core.notesRef=refs/notes/cfg", "notes", "get-ref"], Shape::Branched)
            .with_env(&[("GIT_NOTES_REF", "refs/notes/env")]),
    );
    // The ref name from a file scope rather than from argv.
    out.push(
        Case::new("notes", &["notes", "add", "-m", "fromfile", "HEAD"], Shape::Branched)
            .with_scoped_config(vec![ConfigEntry::set(ConfigScope::Repo, "core.notesRef", "refs/notes/fromfile")]),
    );
    // A short name is qualified into `refs/notes/`; an already-qualified name is
    // left alone. `--ref=custom` is covered elsewhere, so this is the *other*
    // half — a name that is a full ref but not under `refs/notes`.
    out.push(Case::new("notes", &["notes", "--ref=refs/heads/main", "list"], Shape::Branched));
    out.push(Case::strict("notes", &["notes", "--ref=refs/heads/main", "show", "HEAD"], Shape::Branched));

    // ---- stdin-driven bulk verbs ----
    out.push(Case::with_stdin("notes", &["notes", "copy", "--stdin"], Shape::Branched, COPY_PAIR_STDIN));
    out.push(strict_stdin("notes", &["notes", "copy", "--stdin"], Shape::Branched, COPY_GARBAGE_STDIN));
    out.push(Case::with_stdin("notes", &["notes", "copy", "--for-rewrite=amend"], Shape::Branched, COPY_PAIR_STDIN));
    out.push(Case::with_stdin(
        "notes",
        &["-c", "notes.rewrite.amend=false", "notes", "copy", "--for-rewrite=amend"],
        Shape::Branched,
        COPY_PAIR_STDIN,
    ));
    out.push(strict_stdin("notes", &["notes", "remove", "--stdin"], Shape::Branched, REMOVE_STDIN));
    out.push(strict_stdin(
        "notes",
        &["notes", "remove", "--ignore-missing", "--stdin"],
        Shape::Branched,
        REMOVE_STDIN,
    ));
    // Several objects on the command line: one diagnostic per object, and a
    // non-zero exit even though the verb "succeeded" for neither.
    out.push(Case::strict("notes", &["notes", "remove", "HEAD", "HEAD~1"], Shape::Branched));

    // ---- merge, and the state it would leave ----
    // `NOTES_MERGE_REF`, `NOTES_MERGE_PARTIAL` and `NOTES_MERGE_WORKTREE/` are
    // all in `OP_STATE_FILES`/`OP_STATE_DIRS`, so a merge that leaves any of
    // them behind is compared. None of these reaches a conflict — that needs two
    // notes refs and belongs to `sequences.rs:1431` — so what they pin is that
    // the refusal leaves *nothing* behind.
    out.push(Case::strict("notes", &["notes", "merge", "nosuchref"], Shape::Branched));
    out.push(Case::strict("notes", &["notes", "merge", "-s", "ours", "nosuchref"], Shape::Branched));
    out.push(Case::strict("notes", &["notes", "merge", "--strategy=theirs", "refs/notes/commits"], Shape::Branched));
    out.push(Case::strict("notes", &["notes", "merge", "--commit"], Shape::Branched));
    out.push(Case::strict("notes", &["notes", "merge", "-s", "no-such-strategy", "other"], Shape::Branched));

    // ---- reuse, and the type check behind it ----
    // `-c`/`-C` want a *blob*; a commit-ish is rejected by
    // `builtin/notes.c`'s `copy_obj_to_fd`. `-C` on a commit is already covered on
    // `Linear`; this is the `-c` (reuse-and-edit) half, which takes the editor
    // path before the type check.
    out.push(Case::strict("notes", &["notes", "add", "-c", "HEAD", "HEAD"], Shape::Branched));
    // Too many operands: `add` takes at most one object.
    out.push(Case::strict("notes", &["notes", "add", "-m", "x", "HEAD~1", "HEAD"], Shape::Branched));
    out.push(Case::strict("notes", &["notes", "list", "HEAD~1"], Shape::Branched));
    out.push(Case::new("notes", &["notes", "prune", "-v"], Shape::Branched));

    // The notes ref lives in the *common* directory, so a note written from a
    // linked worktree lands where the root's `for-each-ref` probe can see it.
    // That is the one thing about `notes` a linked worktree can change, and a
    // port that resolves `refs/notes/commits` against the per-worktree ref
    // store writes it somewhere the probe reports as missing.
    out.push(Case::new("notes", &["notes", "add", "-m", "fromwt", "HEAD"], Shape::Worktree).in_dir("wt"));
}

// ---------------------------------------------------------------------------
// bisect
// ---------------------------------------------------------------------------

/// A replay log that drives a whole session from one invocation: `start`, a
/// `bad` and a `good`. Branch names rather than object ids on both sides —
/// `git bisect good <rev>` becomes `refs/bisect/good-<rev>` verbatim, so a rev
/// with `~` or `^` in it makes git refuse its own ref name.
const REPLAY_OCTOPUS: &[u8] = b"git bisect start\ngit bisect bad main\ngit bisect good oct-a\n";
/// The same, on a fork whose good side is not an ancestor of the bad one, so
/// the answer is `a merge base must be tested` rather than a midpoint.
const REPLAY_CHERRY: &[u8] = b"git bisect start\ngit bisect bad main\ngit bisect good topic\n";
/// Custom terms carried through the replay: `BISECT_TERMS` must come back
/// holding `new`/`old`, not the defaults.
const REPLAY_TERMS: &[u8] =
    b"git bisect start --term-old=old --term-new=new\ngit bisect new main\ngit bisect old cg-loose\n";
/// A log that starts a session and stops: the half-open state, with
/// `BISECT_START` and `BISECT_TERMS` written and no `refs/bisect/*` yet.
const REPLAY_START_ONLY: &[u8] = b"git bisect start\n";
/// A log whose `good` operand contains `~`. Stock writes `BISECT_START`,
/// `BISECT_TERMS`, `BISECT_LOG` and `refs/bisect/bad`, then fails on the ref
/// name — a partial session, which is exactly the state a port is most likely
/// to get wrong by rolling back or by never writing at all.
const REPLAY_BAD_REFNAME: &[u8] = b"git bisect start\ngit bisect bad main\ngit bisect good main~2\n";

/// `bisect`: eight root state files, a per-worktree ref namespace, and a log
/// that can be replayed.
///
/// `corpus::misc_commands` already covers `start` in ten forms plus the bare
/// verbs. What is added here is the half that leaves *content* behind rather
/// than a status line, which is what `OP_STATE_FILES` (runner.rs:2380) compares:
/// `BISECT_START`, `BISECT_TERMS`, `BISECT_LOG`, `BISECT_NAMES`,
/// `BISECT_FIRST_PARENT` and `BISECT_EXPECTED_REV` are all read back byte for
/// byte, and `refs/bisect/bad` and `refs/bisect/good-<rev>` land in
/// `for-each-ref`.
///
/// `replay` is the centrepiece because it is the only verb that can build a
/// *whole* session from one invocation. `builtin/bisect.c`'s `bisect_replay` reads
/// the file line by line and re-dispatches each `git bisect …` line internally,
/// so one case exercises `start`, term parsing, `bisect_state` and the
/// `bisect_next` that follows. Its input arrives on `/dev/stdin`, which is the
/// harness's stdin pipe: the argument is a machine-independent literal, and
/// `bisect replay -` is *not* a synonym — stock rejects it, which is its own
/// row below.
fn bisect(out: &mut Vec<Case>) {
    // ---- replay: a whole session per invocation ----
    out.push(Case::with_stdin("bisect", &["bisect", "replay", "/dev/stdin"], Shape::Octopus, REPLAY_OCTOPUS));
    out.push(Case::with_stdin("bisect", &["bisect", "replay", "/dev/stdin"], Shape::Cherry, REPLAY_CHERRY));
    out.push(Case::with_stdin("bisect", &["bisect", "replay", "/dev/stdin"], Shape::CommitGraph, REPLAY_TERMS));
    out.push(Case::with_stdin("bisect", &["bisect", "replay", "/dev/stdin"], Shape::Branched, REPLAY_START_ONLY));
    out.push(Case::with_stdin("bisect", &["bisect", "replay", "/dev/stdin"], Shape::CrissCross, REPLAY_OCTOPUS));
    out.push(strict_stdin("bisect", &["bisect", "replay", "/dev/stdin"], Shape::CommitGraph, REPLAY_BAD_REFNAME));
    // `-` is a filename to `bisect replay`, not a synonym for stdin. The
    // diagnostic is the whole answer.
    out.push(strict_stdin("bisect", &["bisect", "replay", "-"], Shape::Branched, REPLAY_OCTOPUS));

    // ---- start: the term vocabulary and the traversal switches ----
    // The separate-argument spelling of the term options, which takes a
    // different `parse_options` branch from the `--term-old=` form the existing
    // corpus uses. Both must land the same two lines in `BISECT_TERMS`.
    out.push(Case::new(
        "bisect",
        &["bisect", "start", "--term-old", "old", "--term-new", "new", "main", "main~2"],
        Shape::CommitGraph,
    ));
    // Only one term renamed: the other keeps its default, which is the case a
    // port that stores the pair as one unit gets wrong.
    out.push(Case::new("bisect", &["bisect", "start", "--term-new=new", "main", "main~2"], Shape::CommitGraph));
    // `--first-parent` writes `BISECT_FIRST_PARENT` and changes which commit is
    // picked; an octopus merge is where the two answers differ most.
    out.push(Case::new("bisect", &["bisect", "start", "--first-parent", "main", "main~2"], Shape::Octopus));
    // `--no-checkout` writes `BISECT_HEAD` instead of moving `HEAD`, so the two
    // rows differ in which state file exists and in `rev-parse HEAD`.
    out.push(Case::new("bisect", &["bisect", "start", "--no-checkout", "main", "main~4"], Shape::CommitGraph));
    out.push(Case::new(
        "bisect",
        &["bisect", "start", "--no-checkout", "--term-old=old", "--term-new=new", "main", "main~4"],
        Shape::CommitGraph,
    ));
    // A pathspec after `--` is stored in `BISECT_NAMES` and narrows the
    // traversal; the existing corpus has one such row on `Branched`, where the
    // history is two commits deep and the narrowing cannot change the answer.
    out.push(Case::new("bisect", &["bisect", "start", "main", "main~4", "--", "cg.txt"], Shape::CommitGraph));
    out.push(Case::new("bisect", &["bisect", "start", "main", "main~4", "--", "src"], Shape::CommitGraph));
    // Two roots with no merge base at all, and a criss-cross where the merge
    // base is not unique — the two topologies `bisect_next` handles specially.
    out.push(Case::new("bisect", &["bisect", "start", "main", "alien-tip"], Shape::Unrelated));
    out.push(Case::new("bisect", &["bisect", "start", "cc-left", "cc-right"], Shape::CrissCross));
    // The same commit on both sides: a refusal, and one that must leave the
    // session it had already started behind.
    out.push(Case::strict("bisect", &["bisect", "start", "main", "main"], Shape::CommitGraph));

    // ---- the refusals before a session exists ----
    // Every one of these has to leave `.git` untouched: a port that writes
    // `BISECT_START` and *then* notices there is no session diverges on the
    // op-state digest even though its exit code matches.
    for verb in ["good", "bad", "skip", "next", "terms"] {
        out.push(Case::strict("bisect", &["bisect", verb], Shape::CommitGraph));
    }
    out.push(Case::strict("bisect", &["bisect", "good", "main"], Shape::CommitGraph));
    // `visualize` must not launch anything. Verified against stock 2.55.0 under
    // `env::harden` (no `DISPLAY`, `TERM=dumb`): exit 1, and not one byte on
    // either stream. A port that shells out to `gitk` or to a pager here would
    // hang, which is the verdict the harness's timeout exists to name.
    out.push(Case::strict("bisect", &["bisect", "visualize"], Shape::CommitGraph));
    out.push(Case::strict("bisect", &["bisect", "view"], Shape::CommitGraph));
    // `run` with no session: the command must never be executed. `false` rather
    // than `true` so a port that runs it anyway is distinguishable by exit code.
    out.push(Case::strict("bisect", &["bisect", "run", "false"], Shape::CommitGraph));
    out.push(Case::strict("bisect", &["bisect", "reset"], Shape::CommitGraph));
    out.push(Case::strict("bisect", &["bisect", "replay", "no-such-log"], Shape::CommitGraph));

    // From a linked worktree. The session would land in
    // `.git/worktrees/wt/BISECT_*` and `refs/bisect/*` is per-worktree, so
    // neither `probe_op_state` nor `for-each-ref` at the fixture root can see
    // it — this row is scored on stdout and exit code alone, and is here
    // because a port that writes bisect state into the *common* directory is
    // caught by the root probe reporting state that stock does not write.
    out.push(Case::new("bisect", &["bisect", "start"], Shape::Worktree).in_dir("wt"));
    out.push(Case::strict("bisect", &["bisect", "log"], Shape::Worktree).in_dir("wt"));
}

// ---------------------------------------------------------------------------
// worktree
// ---------------------------------------------------------------------------

/// `worktree`: a second checkout, its administrative directory, and a listing
/// that is the only window onto either.
///
/// `corpus::worktree_index::worktree` covers `list` in four forms on `Linear`,
/// five `add` forms, `prune`, `repair` and eight refusals — all of them against
/// shapes with **no linked worktree at all**. That is the gap this closes:
/// [`Shape::Worktree`] carries a real second checkout with its own admin
/// directory at `.git/worktrees/wt` (fixture.rs:994), registered with relative
/// paths so the per-case copy points at itself, and every verb that *operates on
/// an existing linked worktree* — `lock`, `unlock`, `move`, `remove`, `repair
/// <name>`, and `list` with something to list — was unreachable before it.
///
/// Where the answer lives, per verb:
///
///  * `list` prints it, and the paths it prints are normalized to `<REPO>` by
///    `runner::normalize`, so the listing is comparable byte for byte.
///  * `add` writes a branch ref (`for-each-ref`), a checkout the root's
///    `status --porcelain -uall` reports as one untracked directory, and — with
///    `--track` — a `branch.<name>.remote`/`.merge` pair that
///    `config --list --local` reads back.
///  * `move` is visible the same way: the destination is not in
///    `.git/info/exclude`, so it appears in `status` while the vacated `wt/`
///    does not.
///  * `lock`, `unlock`, `repair` and `remove` write only under
///    `.git/worktrees/`, which no probe reads (see the module comment). Those
///    rows are exit-code and stderr assertions and are `strict` where the
///    diagnostic is the whole answer.
fn worktree(out: &mut Vec<Case>) {
    // ---- list, with something to list ----
    out.push(Case::new("worktree", &["worktree", "list"], Shape::Worktree));
    out.push(Case::new("worktree", &["worktree", "list", "--porcelain"], Shape::Worktree));
    out.push(Case::new("worktree", &["worktree", "list", "-v"], Shape::Worktree));
    out.push(Case::new("worktree", &["worktree", "list", "--porcelain", "-z"], Shape::Worktree));
    // From inside the linked worktree, and from inside its administrative
    // directory. The listing must be identical from all three vantage points —
    // it is a property of the common directory, not of the cwd — and a port that
    // resolves the worktree set relative to `--git-dir` rather than to
    // `--git-common-dir` gets one entry instead of two from the second row.
    out.push(Case::new("worktree", &["worktree", "list"], Shape::Worktree).in_dir("wt"));
    out.push(Case::new("worktree", &["worktree", "list", "--porcelain"], Shape::Worktree).in_dir("wt"));
    out.push(Case::new("worktree", &["worktree", "list"], Shape::Worktree).in_dir(".git/worktrees/wt"));
    // A repository with a dangling ref, a broken symref and a corrupt loose
    // object: `worktree list` resolves `HEAD` and must not walk into any of them.
    out.push(Case::new("worktree", &["worktree", "list"], Shape::Damaged));

    // ---- add: the flags that change what is written ----
    // `--lock` and `--reason` write `.git/worktrees/<id>/locked`; the file is not
    // probed, so what these two rows pin is the *rest* — the branch ref, the
    // checkout, and the fact that neither flag suppresses them.
    out.push(Case::new("worktree", &["worktree", "add", "--lock", "wtl"], Shape::Linear));
    // `--no-checkout` writes the admin directory and the ref but leaves the new
    // worktree empty; `--checkout` is the explicit default. The two differ in
    // what `status -uall` reports at the root.
    out.push(Case::new("worktree", &["worktree", "add", "--no-checkout", "wtn"], Shape::Linear));
    // `-B` on a branch that already exists resets it — a ref *move*, not a
    // create, and the only `add` form that changes an existing ref's value.
    out.push(Case::new("worktree", &["worktree", "add", "-B", "feature", "wtb"], Shape::Branched));
    // `--orphan` creates an *unborn* branch: the admin directory's `HEAD` names
    // a ref that does not exist, so `for-each-ref` reports no new ref at all.
    out.push(Case::new("worktree", &["worktree", "add", "--orphan", "wto"], Shape::Linear));
    // `--relative-paths` decides whether `.git/worktrees/<id>/gitdir` and the
    // new worktree's `.git` file hold a relative or an absolute path. Neither is
    // probed directly, but an absolute one bakes this side's fixture root into
    // a file — so the pair is here as the shape of the two writes, and the
    // fixture itself depends on the relative form working (fixture.rs:1006).
    out.push(Case::new("worktree", &["worktree", "add", "--relative-paths", "wtr"], Shape::Linear));
    // `--track` writes `branch.tb.remote` and `branch.tb.merge` into
    // `.git/config`, which `probe_state`'s `config --list --local` reads back —
    // the one `add` flag whose effect is a *config* write.
    out.push(Case::new("worktree", &["worktree", "add", "--track", "-b", "tb", "wtt", "origin/div"], Shape::BehindRemote));
    // `worktree.guessRemote` only fires when the path's basename names a branch
    // that exists *only* on a remote, and no shape has one — so these two pin
    // that the setting does not perturb the ordinary DWIM, in both directions.
    out.push(Case::new("worktree", &["-c", "worktree.guessRemote=true", "worktree", "add", "wtg"], Shape::BehindRemote));
    // A second linked worktree beside the first, and one added *from* inside the
    // first — the path is resolved against the cwd, not against the repository
    // root, so `../wt2` and `wt2` must land in the same place.
    out.push(Case::new("worktree", &["worktree", "add", "wt2"], Shape::Worktree));
    out.push(Case::new("worktree", &["worktree", "add", "../wt2"], Shape::Worktree).in_dir("wt"));
    // `-f` checks out a branch that is already checked out somewhere else, which
    // is a refusal without it.
    out.push(Case::new("worktree", &["worktree", "add", "-f", "wtf", "main"], Shape::Linear));

    // ---- add: refusals that matter ----
    out.push(Case::strict("worktree", &["worktree", "add", "wt"], Shape::Worktree));
    out.push(Case::strict("worktree", &["worktree", "add", "-b", "main", "wtx"], Shape::Linear));
    out.push(Case::strict("worktree", &["worktree", "add", "wtx", "nosuchbranch"], Shape::Linear));
    out.push(Case::strict("worktree", &["worktree", "add", "."], Shape::Linear));
    out.push(Case::strict("worktree", &["worktree", "add", "wtx", "main"], Shape::Linear));
    out.push(Case::strict("worktree", &["worktree", "add", "--orphan", "--detach", "wto"], Shape::Linear));

    // ---- lock / unlock ----
    // The state these write is invisible to the probe (module comment), so what
    // is measured is the diagnostic and the exit code — and, for the successful
    // pair, that nothing *else* moved.
    out.push(Case::new("worktree", &["worktree", "lock", "wt"], Shape::Worktree));
    out.push(Case::strict("worktree", &["worktree", "unlock", "wt"], Shape::Worktree));
    out.push(Case::strict("worktree", &["worktree", "lock", "."], Shape::Worktree));

    // ---- move / remove ----
    // `move` is visible: `wt2` is not in `.git/info/exclude`, so the root's
    // `status -uall` gains an untracked directory and loses nothing.
    out.push(Case::new("worktree", &["worktree", "move", "wt", "wt2"], Shape::Worktree));
    out.push(Case::strict("worktree", &["worktree", "move", "wt", "."], Shape::Worktree));
    // `remove` deletes both the checkout and the admin directory. The checkout
    // is excluded from status, so the assertion is the exit code plus the
    // interop probe — `git_fingerprint` sees `.git/worktrees/wt` disappear and
    // opens the gate, and stock is then asked to re-read both repositories.
    out.push(Case::new("worktree", &["worktree", "remove", "wt"], Shape::Worktree));
    out.push(Case::strict("worktree", &["worktree", "remove", "."], Shape::Worktree));

    // ---- prune / repair ----
    // `prune` on a repository whose one linked worktree is healthy must remove
    // nothing whatever the expiry says; `--expire=now` is the row that catches a
    // port pruning a live worktree.
    out.push(Case::new("worktree", &["worktree", "prune", "-v"], Shape::Worktree));
    out.push(Case::new("worktree", &["worktree", "prune", "--expire=now", "-v"], Shape::Worktree));
    out.push(Case::strict("worktree", &["worktree", "prune", "--expire=bogus"], Shape::Linear));
    // `repair` on the shape whose worktree is registered with *relative* paths
    // reports the mismatch it found; the path in that message is normalized.
    out.push(Case::new("worktree", &["worktree", "repair"], Shape::Worktree));
    out.push(Case::new("worktree", &["worktree", "repair", "wt"], Shape::Worktree));

    // ---- the configuration that changes what a worktree *is* ----
    // `extensions.worktreeConfig` moves part of the configuration into
    // `.git/config.worktree`; `ConfigScope::Worktree` writes both files, so the
    // extension is on for real rather than asserted by a `-c` that git ignores.
    out.push(
        Case::new("worktree", &["worktree", "list", "--porcelain"], Shape::Worktree).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Worktree, "wt.k", "fromworktree"),
        ]),
    );
    // `safe.bareRepository=explicit` makes git refuse a bare repository reached
    // by discovery. A normal worktree is not one, so `list` must still answer —
    // a port that applies the check to every repository fails here.
    out.push(Case::new("worktree", &["-c", "safe.bareRepository=explicit", "worktree", "list"], Shape::Worktree));
}

// ---------------------------------------------------------------------------
// filter-branch
// ---------------------------------------------------------------------------

/// Squelch the ten-second deprecation banner. Additive — `env::harden` does not
/// pin it (`env::is_pinned`) — and delivered to both sides, so it removes the
/// `SLEEP_ALLOWANCE` (runner.rs:1495) cost from these rows without changing what
/// the script does.
const SQUELCH: &[(&str, &str)] = &[("FILTER_BRANCH_SQUELCH_WARNING", "1")];

/// `filter-branch`: the filters that need no shell of their own beyond a
/// harmless command.
///
/// Kept to four rows on purpose, and that number is measured rather than
/// guessed. The script prints
/// `Rewrite <sha> (i/n) (N seconds passed, remaining N predicted)` to stdout
/// where `N` is wall-clock elapsed time, so stock does not reliably reproduce
/// its own output: a first pass with six rows had two of them — both rewriting
/// three commits across `-- --all` — land in the harness's `Nondeterministic`
/// bucket, where they are excluded from the parity denominator and measure
/// nothing while still costing four stock invocations each. Those two were
/// dropped. The four left rewrite at most two commits apiece or do not rewrite
/// at all, which lowers the odds without removing them: a two-commit rewrite
/// still straddles a second occasionally, and when it does the row is excluded
/// rather than scored. Four is the count at which that is an acceptable trade;
/// the same wall clock is why `corpus::history_rewrite` keeps its own eight. `corpus::history_rewrite` already carries
/// eight rows; these add the combinations it does not have — an
/// `--index-filter` that actually removes a path, an `-- --all` rewrite of a
/// merge, the no-`-f` success, and the dirty-tree refusal.
fn filter_branch(out: &mut Vec<Case>) {
    // An index filter that removes a tracked path from every commit: two
    // rewrites, new ids on both, and `refs/original/refs/heads/main` left behind
    // holding the old tip. `--ignore-unmatch` keeps `git rm` quiet on the commit
    // where the path is absent, which is what makes the filter usable at all.
    out.push(
        Case::new(
            "filter-branch",
            &["filter-branch", "-f", "--index-filter", "git rm --cached --ignore-unmatch README.md", "HEAD"],
            Shape::Branched,
        )
        .with_env(SQUELCH),
    );
    // `--subdirectory-filter` across every ref of a merge: both branch tips
    // collapse onto the same rewritten root, because `src/` is identical on
    // both sides of the merge.
    out.push(
        Case::new(
            "filter-branch",
            &["filter-branch", "-f", "--subdirectory-filter", "src", "--", "--all"],
            Shape::Merged,
        )
        .with_env(SQUELCH),
    );
    // No `-f`, and no `refs/original/` in the way: this *succeeds*. The force
    // flag guards against clobbering a previous backup, not against rewriting —
    // a port that demands `-f` unconditionally diverges here.
    out.push(
        Case::new("filter-branch", &["filter-branch", "--msg-filter", "cat", "--", "--all"], Shape::Branched)
            .with_env(SQUELCH),
    );
    // The refusal that *is* reachable from a pristine fixture: a dirty worktree.
    // `git-filter-branch.sh` runs `require_clean_work_tree` before it touches a
    // ref, so both messages — unstaged and staged — come out together and
    // nothing is rewritten.
    out.push(
        Case::strict("filter-branch", &["filter-branch", "-f", "--msg-filter", "cat", "HEAD"], Shape::Dirty)
            .with_env(SQUELCH),
    );
}
