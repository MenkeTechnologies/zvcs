//! Differential corpus cases for the commit_family subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! Covers `commit` and `commit-tree`: the message sources, the cleanup modes,
//! the staging modes (`-a`, a pathspec, `--only`, `--include`), `--amend`, the
//! dry-run/status output modes, the hook interaction, and the configuration
//! `commit` reads before it decides any of it.
//!
//! # Why a commit's *bytes* are asserted here and not only its stdout
//!
//! `probe_state` reads `cat-file --batch-check --batch-all-objects` and
//! `rev-parse HEAD` back with stock git, and a commit object is its bytes: the
//! tree, the parent list, the `encoding` header, and the message after cleanup.
//! Any of those differing changes the commit id, which changes both the
//! `[<branch> <abbrev>]` line on stdout and two lines of the state digest. That
//! is what makes a message-shaping case — a cleanup mode, a comment character, a
//! trailer, a template — a real assertion rather than an exit-code check: two
//! cleanup modes over one `-m` argument produce two different object ids.
//!
//! `env::harden` pins author and committer identity and both dates, so those
//! ids are byte-reproducible across sides and runs.
//!
//! # Fixture constraints these cases work around
//!
//! A case is one argv against a pristine copy of a shape, so nothing here can
//! write a file first. Three consequences:
//!
//! * A `-F <file>` message must name a file the shape already tracks
//!   (`README.md`, `src/lib.rs`), or come in on stdin.
//! * `commit.template` must likewise name a tracked file, so the template
//!   content is `README.md`'s two lines rather than something written for the
//!   purpose. Under the default cleanup git then refuses with `Aborting commit;
//!   you did not edit the message.` — the template is offered to `GIT_EDITOR`,
//!   which `env::harden` pins to `true`, so it comes back unedited. Reaching the
//!   *committing* half of the template path therefore needs
//!   `--cleanup=verbatim`, which suppresses the did-not-edit check.
//! * `--date` is unusable: both dates are pinned, and a case that set one would
//!   put a clock back into the comparison.
//!
//! `-S`/`--gpg-sign` is unusable for the same class of reason: no key exists in
//! the hermetic `HOME`, so both sides would only ever reach the "no secret key"
//! refusal.
//!
//! `core.commentChar=auto` is deliberately absent: git 2.55 warns that the value
//! is deprecated and git 2.50 does not, and the two releases pick different
//! comment characters for the same message, so such a case's expected value is
//! the installed git's version rather than anything about the port.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

/// Tree of the shapes' shared initial commit (`README.md` + `src/lib.rs`, both
/// written by `fixture::build` before the first `commit`). Content-addressed, so
/// it is the same id in every shape and on every machine; used to spell a tree
/// to `commit-tree` as a raw object name rather than as a rev.
const BASE_TREE: &str = "e0e1a776261f58b1c8741e3747adde42edd1a859";
/// The empty tree. Git resolves this id without it being in the object store, so
/// a `commit-tree` over it is a commit with no files at all.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// A message carrying everything the cleanup modes disagree about: a comment
/// line, a line with trailing whitespace, and trailing blank lines. `verbatim`
/// keeps all three, `whitespace` drops the trailing blanks and the trailing
/// spaces, `strip` additionally drops the comment line.
const MSG_MESSY: &str = "subject\n\n# a comment line\nbody with trailing   \n\n\n";
/// A message containing git's scissors line. `--cleanup=scissors` cuts an
/// *editor's* buffer at this line; a `-m` message is not an editor buffer, so
/// both scissors and verbatim keep it — the pair below pins that they agree.
const MSG_SCISSORS: &str =
    "subject\n\n# ------------------------ >8 ------------------------\ncut this away\n";
/// Comment lines under two different comment characters, so `core.commentChar`
/// and `core.commentString` each decide which of the two is stripped.
const MSG_SEMI: &str = "subject\n\n; semi comment\n# hash line\n";
const MSG_SLASH: &str = "subject\n\n// slash comment\n# hash line\n";

/// Message payload delivered on stdin.
const STDIN_MSG: &[u8] = b"msg body\nsecond line\n";
/// One pathspec, newline-terminated, for `--pathspec-from-file=-`.
const STDIN_PATHSPEC: &[u8] = b"README.md\n";
/// The same pathspec NUL-terminated, for `--pathspec-file-nul`.
const STDIN_PATHSPEC_NUL: &[u8] = b"README.md\0";

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    message_sources(out);
    cleanup_and_comments(out);
    template_and_status_config(out);
    trailers_and_encoding(out);
    staging_modes(out);
    amend(out);
    output_modes(out);
    hooks(out);
    merge_state(out);
    commit_tree(out);
}

fn commit(out: &mut Vec<Case>, shape: Shape, args: &[&str]) {
    out.push(Case::new("commit", args, shape));
}

/// A refusal, where the message on stderr is the behaviour being measured.
fn refusal(out: &mut Vec<Case>, shape: Shape, args: &[&str]) {
    out.push(Case::strict("commit", args, shape));
}

/// Where the message comes from.
///
/// Each of these produces a *different* commit object over the same tree, so the
/// commit id on stdout and in the state digest is the assertion. `Dirty` is the
/// shape throughout: it has one staged path, so a plain `commit` succeeds and
/// the message is the only thing varying.
fn message_sources(out: &mut Vec<Case>) {
    // Repeated `-m` is paragraph concatenation, not last-wins: git joins the
    // arguments with a blank line between them.
    commit(out, Shape::Dirty, &["commit", "-m", "one", "-m", "two"]);

    // `-F <file>` and its stdin form. A file message is taken as-is under the
    // default cleanup, so `README.md`'s two lines become subject + body.
    commit(out, Shape::Dirty, &["commit", "-F", "README.md"]);
    out.push(Case::with_stdin("commit", &["commit", "-F", "-"], Shape::Dirty, STDIN_MSG));

    // Reusing an existing commit's message. `-C` takes the message and the
    // *author identity* without an editor; `-c` runs the editor over it
    // (`GIT_EDITOR=true` accepts). Both carry the reused author, which is why
    // git prints the extra `Date:` line — the author date no longer matches the
    // committer date.
    commit(out, Shape::Dirty, &["commit", "-C", "HEAD"]);
    commit(out, Shape::Dirty, &["commit", "-c", "HEAD"]);

    // The autosquash message generators. Each writes a fixed prefix plus the
    // named commit's subject: `squash! `, `fixup! `, `amend! `. `--fixup=amend:`
    // additionally opens the editor over the borrowed message, and
    // `--fixup=reword:` forces a paths-limited commit that commits no changes at
    // all — its tree is the parent's.
    commit(out, Shape::Dirty, &["commit", "--squash=HEAD"]);
    commit(out, Shape::Dirty, &["commit", "--fixup=HEAD"]);
    commit(out, Shape::Dirty, &["commit", "--fixup=amend:HEAD"]);
    commit(out, Shape::Dirty, &["commit", "--fixup=reword:HEAD"]);

    // Author override, and the sign-off/trailer machinery that appends to the
    // message body rather than replacing it.
    commit(
        out,
        Shape::Dirty,
        &["commit", "-m", "x", "--author=Other Name <other@example.invalid>"],
    );
    commit(out, Shape::Dirty, &["commit", "-m", "x", "-s"]);
    commit(
        out,
        Shape::Dirty,
        &["commit", "-m", "x", "--signoff", "--trailer", "Acked-by: A U Thor <a@example.invalid>"],
    );
    // `-e` forces the editor even with `-m`. Under `GIT_EDITOR=true` the buffer
    // comes back unchanged, so the difference is only visible through cleanup:
    // the editor path appends the status as comments, and `verbatim` keeps them.
    commit(out, Shape::Dirty, &["commit", "-m", "x", "-e", "--cleanup=verbatim"]);

    // Two message sources at once. Git rejects the combination in
    // `parse_and_validate_options()` before it reads either one.
    refusal(out, Shape::Dirty, &["commit", "-m", "x", "-F", "README.md"]);
    refusal(out, Shape::Dirty, &["commit", "--squash=HEAD", "--fixup=HEAD"]);
    // The same class of check, but with an argument that cannot be resolved:
    // git answers the `-m`/`-C` incompatibility *before* looking `nosuchref` up,
    // so the diagnostic names the options and not the ref.
    refusal(out, Shape::Dirty, &["commit", "-C", "nosuchref", "-m", "x"]);
    // A `-C` argument that resolves to a tree rather than a commit: two lines on
    // stderr (`error:` then `fatal:`), which a single-line refusal cannot
    // reproduce.
    refusal(out, Shape::Dirty, &["commit", "-C", "HEAD^{tree}"]);

    // An empty message is refused with a `1`, not a `128` — git has already
    // written the tree by the time it checks, which is visible in the state
    // digest as an object the repository gained without a commit pointing at it.
    refusal(out, Shape::Dirty, &["commit", "-m", ""]);
    commit(out, Shape::Dirty, &["commit", "--allow-empty-message", "-m", ""]);
}

/// `--cleanup` and the comment character, which together decide which bytes of
/// the message survive into the object.
///
/// The five modes over one message produce four distinct commit ids: `verbatim`
/// keeps everything, `strip` drops the comment line and the trailing blanks, and
/// `whitespace`/`default`/`scissors` agree with each other on a `-m` message. A
/// port that implemented cleanup as a single "trim it" would collapse all five
/// into one id, which every one of these rows would then catch.
fn cleanup_and_comments(out: &mut Vec<Case>) {
    for mode in ["verbatim", "strip", "whitespace", "default", "scissors"] {
        let flag = format!("--cleanup={mode}");
        commit(out, Shape::Dirty, &["commit", &flag, "-m", MSG_MESSY]);
    }

    // The scissors line, under the two modes that could plausibly cut at it.
    // Neither does: git truncates at the scissors only when the message came
    // through the editor, so a `-m` message keeps the line and the text below
    // it, and both ids are equal. A port that cut unconditionally would produce
    // a shorter message and a different id here.
    commit(out, Shape::Dirty, &["commit", "--cleanup=scissors", "-m", MSG_SCISSORS]);
    commit(out, Shape::Dirty, &["commit", "--cleanup=verbatim", "-m", MSG_SCISSORS]);

    // Which lines are comments is configuration. With `core.commentChar=;` the
    // `;` line is stripped and the `#` line survives; `core.commentString=//`
    // does the same for `//`. Both land on the same commit id as each other,
    // because in both the surviving body is one line — so these two rows are
    // only meaningful together with the default-comment-char rows above, where
    // it is the `#` line that goes.
    out.push(
        Case::new("commit", &["commit", "--cleanup=strip", "-m", MSG_SEMI], Shape::Dirty)
            .with_config(&[("core.commentChar", ";")]),
    );
    out.push(
        Case::new("commit", &["commit", "--cleanup=strip", "-m", MSG_SLASH], Shape::Dirty)
            .with_config(&[("core.commentString", "//")]),
    );

    // `commit.cleanup` is the same decision made from configuration, and it has
    // to be read from the repository's own config file and not only from `-c`:
    // this row delivers it through `.git/config`, where a port that consults
    // only the command-line parameter list sees nothing.
    out.push(
        Case::new("commit", &["commit", "-m", MSG_MESSY], Shape::Dirty).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::Repo, "commit.cleanup", "verbatim"),
        ]),
    );
    // `--cleanup` on the command line outranks `commit.cleanup`.
    out.push(
        Case::new("commit", &["commit", "--cleanup=strip", "-m", MSG_MESSY], Shape::Dirty)
            .with_config(&[("commit.cleanup", "verbatim")]),
    );
}

/// `commit.template`, `commit.status`, `commit.verbose` and
/// `status.showUntrackedFiles` — the settings that shape the buffer git hands
/// the editor.
///
/// Normally that buffer is invisible: the comments are stripped on the way back
/// out, so every setting collapses to the same commit. `--cleanup=verbatim`
/// keeps the buffer intact, which turns each setting into a different commit id
/// and makes it measurable at all.
fn template_and_status_config(out: &mut Vec<Case>) {
    const TEMPLATE: &[(&str, &str)] = &[("commit.template", "README.md")];

    // The template, unedited. `GIT_EDITOR` is `true`, so the buffer comes back
    // byte-identical and git refuses with `Aborting commit; you did not edit the
    // message.` — the check that exists precisely for this.
    out.push(Case::strict("commit", &["commit"], Shape::Dirty).with_config(TEMPLATE));
    // `--cleanup=verbatim` suppresses that check, so the template becomes the
    // message — together with the status comments git appended below it.
    out.push(
        Case::new("commit", &["commit", "--cleanup=verbatim"], Shape::Dirty).with_config(TEMPLATE),
    );
    // `commit.status=false` suppresses that appended status, so the message is
    // the template alone and the commit id moves. This pair is the whole
    // measurement of `commit.status`: under any other cleanup mode the two are
    // indistinguishable.
    out.push(
        Case::new("commit", &["commit", "--cleanup=verbatim"], Shape::Dirty)
            .with_config(&[("commit.template", "README.md"), ("commit.status", "false")]),
    );
    // `commit.verbose=true` appends the diff below a cut line. The cut line and
    // everything after it are removed regardless of cleanup mode, so this must
    // land on the same id as the plain template row above — a port that let the
    // diff through would produce a commit whose message contains a patch.
    out.push(
        Case::new("commit", &["commit", "--cleanup=verbatim"], Shape::Dirty)
            .with_config(&[("commit.template", "README.md"), ("commit.verbose", "true")]),
    );
    // Delivered from the repository's config file rather than from `-c`.
    out.push(
        Case::new("commit", &["commit", "--cleanup=verbatim"], Shape::Dirty).with_scoped_config(
            vec![ConfigEntry::set(ConfigScope::Repo, "commit.template", "README.md")],
        ),
    );
    // A template that is not there. Git reports the path with `strerror` and
    // exits 128 before touching the index.
    out.push(
        Case::strict("commit", &["commit"], Shape::Dirty)
            .with_config(&[("commit.template", "nosuchfile")]),
    );

    // No template and no `-m`: the buffer is status comments only, so the
    // message after cleanup is empty and git aborts — a different refusal from
    // the did-not-edit one above.
    refusal(out, Shape::Dirty, &["commit"]);

    // `status.showUntrackedFiles` is read by the status `commit` prints after
    // committing, and replaces the untracked block with a one-line note.
    out.push(
        Case::new("commit", &["commit", "-m", "x", "--long"], Shape::Dirty)
            .with_config(&[("status.showUntrackedFiles", "no")]),
    );
}

/// `--trailer` and `i18n.commitEncoding`: the two settings that change the
/// commit object's *shape* rather than its prose.
fn trailers_and_encoding(out: &mut Vec<Case>) {
    // One trailer, then two, so the interpreter has to place a second one in the
    // same block rather than starting a new paragraph.
    commit(
        out,
        Shape::Dirty,
        &["commit", "-m", "x", "--trailer", "Acked-by: A U Thor <a@example.invalid>"],
    );
    commit(
        out,
        Shape::Dirty,
        &[
            "commit",
            "-m",
            "x",
            "--trailer",
            "Acked-by: A U Thor <a@example.invalid>",
            "--trailer",
            "Reviewed-by: R Viewer <r@example.invalid>",
        ],
    );
    // `trailer.<token>.key` expands a short token to a full key, and `ifExists`
    // decides what a repeat of it does.
    out.push(
        Case::new(
            "commit",
            &[
                "commit",
                "-m",
                "x",
                "--trailer",
                "ack: A U Thor <a@example.invalid>",
                "--trailer",
                "ack: A U Thor <a@example.invalid>",
            ],
            Shape::Dirty,
        )
        .with_config(&[
            ("trailer.ack.key", "Acked-by"),
            ("trailer.ack.ifExists", "addIfDifferent"),
        ]),
    );
    // `trailer.separators` widens what counts as the `key<sep>value` split, so
    // `=` becomes a separator alongside `:`.
    out.push(
        Case::new(
            "commit",
            &["commit", "-m", "x", "--trailer", "Acked-by=A U Thor <a@example.invalid>"],
            Shape::Dirty,
        )
        .with_config(&[("trailer.separators", ":=")]),
    );

    // `i18n.commitEncoding` adds an `encoding` header to the object, which
    // changes the commit id even though every other byte is the same. UTF-8 is
    // the one value git writes *no* header for, so the two rows are only
    // meaningful as a pair: a port that always emits the header and one that
    // never does each pass exactly one of them.
    out.push(
        Case::new("commit", &["commit", "--allow-empty", "-m", "enc"], Shape::Linear)
            .with_config(&[("i18n.commitEncoding", "ISO-8859-1")]),
    );
    out.push(
        Case::new("commit", &["commit", "--allow-empty", "-m", "enc"], Shape::Linear)
            .with_config(&[("i18n.commitEncoding", "UTF-8")]),
    );
}

/// What gets committed: the index as it stands, the worktree (`-a`), or the
/// paths a pathspec names (`--only`/`--include`).
///
/// `Dirty` is the shape that can tell them apart — one staged addition, one
/// unstaged modification, one unstaged deletion, one untracked file — so each
/// mode commits a visibly different tree.
fn staging_modes(out: &mut Vec<Case>) {
    // A pathspec implies `--only`: the commit contains that path's *worktree*
    // content and nothing else, and the staged `staged.txt` is left staged.
    commit(out, Shape::Dirty, &["commit", "-m", "x", "--", "README.md"]);
    commit(out, Shape::Dirty, &["commit", "-m", "x", "--only", "README.md"]);
    // `--include` is the other half: the named path *plus* whatever was already
    // staged, so this commit has two changes where `--only` has one.
    commit(out, Shape::Dirty, &["commit", "-m", "x", "--include", "README.md"]);
    // A pathspec naming a deleted path commits the deletion; a pathspec naming a
    // directory expands to what is under it.
    commit(out, Shape::Dirty, &["commit", "-m", "x", "--", "src/lib.rs"]);
    commit(out, Shape::Dirty, &["commit", "-m", "x", "--", "src"]);
    // `-a` reaches all three tracked changes at once; the untracked file is in
    // none of them.
    commit(out, Shape::Dirty, &["commit", "-am", "x"]);

    // Pathspecs read from a file rather than from argv, in both terminator
    // flavours.
    out.push(Case::with_stdin(
        "commit",
        &["commit", "-m", "x", "--pathspec-from-file=-"],
        Shape::Dirty,
        STDIN_PATHSPEC,
    ));
    out.push(Case::with_stdin(
        "commit",
        &["commit", "-m", "x", "--pathspec-from-file=-", "--pathspec-file-nul"],
        Shape::Dirty,
        STDIN_PATHSPEC_NUL,
    ));
    // A pathspec file whose lines are not paths: git reports *every* line that
    // matched nothing, so the count of `error:` lines is the behaviour.
    refusal(out, Shape::Dirty, &["commit", "-m", "x", "--pathspec-from-file=README.md"]);

    // `-a` with a pathspec is refused outright — the two say different things
    // about which paths are involved.
    refusal(out, Shape::Dirty, &["commit", "-a", "-m", "x", "--", "README.md"]);
    // A pathspec matching nothing git tracks, from both directions: a path that
    // exists but is untracked, and a path that does not exist at all. Both are
    // `error:` and exit 1, not a fatal.
    refusal(out, Shape::Dirty, &["commit", "-m", "x", "--", "untracked.txt"]);
    refusal(out, Shape::Dirty, &["commit", "-m", "x", "--", "nosuch.txt"]);

    // A staged change over a clean worktree: `-m` and `--only` on the staged
    // path commit the same tree.
    commit(out, Shape::MergeableStaged, &["commit", "-m", "x"]);
    commit(out, Shape::MergeableStaged, &["commit", "-m", "x", "--only", "keep.txt"]);
    // A pathspec naming a tracked path with no change on it. Git falls through
    // to `run_status()` and reports what *is* there — the staged `keep.txt` —
    // under the "no changes added to commit" heading, at exit 1.
    refusal(out, Shape::MergeableStaged, &["commit", "-m", "x", "--", "cold.txt"]);

    // A path whose name carries a combining mark. The pathspec has to survive
    // precomposition on the way in, and the same commit has to come out of `-a`
    // and out of naming the path.
    commit(out, Shape::DecomposedPaths, &["commit", "-am", "x"]);
    commit(out, Shape::DecomposedPaths, &["commit", "-m", "x", "--", crate::fixture::NFD_TRACKED]);

    // A cone-mode sparse checkout, where `outside/` is tracked but absent from
    // the worktree. Its index entries carry `skip-worktree`, and every one of
    // these must leave them alone: `-a` stages worktree changes and a
    // skip-worktree path *has* no worktree file, so treating "absent" as
    // "deleted" would commit the removal of half the repository.
    commit(out, Shape::Sparse, &["commit", "-am", "x"]);
    commit(out, Shape::Sparse, &["commit", "-m", "x", "--", "outside/drop.txt"]);
    commit(out, Shape::Sparse, &["commit", "-m", "x", "--", "inside/keep.txt"]);
}

/// `--amend`: the mode that replaces `HEAD` instead of extending it.
///
/// The parent list is the thing to get wrong — an amended merge keeps every
/// parent it had — and `probe_state`'s `rev-parse HEAD` plus the object listing
/// is where that shows.
fn amend(out: &mut Vec<Case>) {
    // The root commit has no parent, and amending it is legal: the result is
    // still a root commit. `--no-edit` keeps the message, so the only change is
    // the committer, and with both dates pinned the id comes back *identical* to
    // the original — which is the sharpest form of the assertion.
    commit(out, Shape::Linear, &["commit", "--amend", "-m", "amended root"]);
    commit(out, Shape::Linear, &["commit", "--amend", "--no-edit"]);

    // Amending with work in the index: the staged path is folded into the
    // replacement commit, and `-a` folds the unstaged ones in too.
    commit(out, Shape::Dirty, &["commit", "--amend", "--no-edit"]);
    commit(out, Shape::Dirty, &["commit", "--amend", "-m", "amended with staged"]);
    commit(out, Shape::Dirty, &["commit", "--amend", "-am", "amend all"]);
    // `--only`/`--include` under `--amend`: one takes the named path's worktree
    // content on top of `HEAD`'s tree, the other adds the index on top of that.
    commit(out, Shape::Dirty, &["commit", "--amend", "--only", "--no-edit", "--", "README.md"]);
    commit(out, Shape::Dirty, &["commit", "--amend", "--include", "--no-edit", "--", "README.md"]);
    // `--reset-author` is legal here (and beside `-C`/`-c`, and nowhere else),
    // and removes the `Date:` line git prints when author and committer differ.
    commit(out, Shape::Dirty, &["commit", "--amend", "--reset-author", "--no-edit"]);
        // The autosquash generator under `--amend`: the borrowed subject is `HEAD`'s,
    // and `HEAD` is what is being replaced.
    commit(out, Shape::Dirty, &["commit", "--amend", "--fixup=HEAD"]);
    commit(out, Shape::Dirty, &["commit", "--amend", "--allow-empty-message", "-m", ""]);

    // Amending a merge. Both parents have to survive, and the octopus has four —
    // a parent list rebuilt from `HEAD^` alone would silently lose three of them
    // and still print a plausible summary line.
    commit(out, Shape::Merged, &["commit", "--amend", "--no-edit"]);
    commit(out, Shape::Octopus, &["commit", "--amend", "--no-edit"]);

    // Amending with no branch to move: `HEAD` itself is rewritten and the
    // summary line says `detached HEAD` rather than a branch name.
    commit(out, Shape::Detached, &["commit", "--amend", "-m", "amended detached"]);

    // Refusals. A merge in progress cannot be amended — there is no single
    // commit to replace — and `--reset-author` contradicts `--author`.
    refusal(out, Shape::Conflicted, &["commit", "--amend", "--no-edit"]);
    refusal(
        out,
        Shape::Dirty,
        &[
            "commit",
            "--amend",
            "--reset-author",
            "--no-edit",
            "--author=Other Name <other@example.invalid>",
        ],
    );
}

/// The reporting modes: what `commit` prints, and what `--dry-run` does instead
/// of committing.
///
/// `--dry-run` is not a pure read: for `-a` and for a pathspec commit git builds
/// the tree it *would* commit, so the object store gains a tree the repository
/// has no reference to. That shows in the state digest, and it is a real
/// behaviour rather than an artefact — the two rows below are the ones that pin
/// it.
fn output_modes(out: &mut Vec<Case>) {
    commit(out, Shape::Dirty, &["commit", "--dry-run"]);
    commit(out, Shape::Dirty, &["commit", "--dry-run", "--short"]);
    commit(out, Shape::Dirty, &["commit", "--dry-run", "--short", "-z"]);
    // `-uno` drops the untracked block the default report prints.
    commit(out, Shape::Dirty, &["commit", "--dry-run", "--short", "-uno"]);
    // The dry runs that build a tree: `-a` and a pathspec.
    commit(out, Shape::Dirty, &["commit", "--dry-run", "-a"]);
    commit(out, Shape::Dirty, &["commit", "--dry-run", "--", "README.md"]);
    // A dry run of an amend reports against `HEAD`'s parent, so every tracked
    // file reads as a new file.
    commit(out, Shape::Dirty, &["commit", "--dry-run", "--amend"]);

    // The same output flags on a commit that actually happens. `--short` and
    // `--porcelain` replace the summary line with a status report, `-q`
    // suppresses it, and `--long` puts the full report after it.
    commit(out, Shape::Dirty, &["commit", "-q", "-m", "x"]);
    commit(out, Shape::Dirty, &["commit", "--porcelain", "-m", "x"]);
    commit(out, Shape::Dirty, &["commit", "--long", "-m", "x"]);
    commit(out, Shape::Dirty, &["commit", "-v", "-m", "x"]);

    // Reports over the shapes whose paths are the hard part: a combining mark
    // that has to be quoted the way stock quotes it, a sparse checkout whose
    // absent files must not be reported as deletions, a gitlink, and an
    // unmerged entry (whose presence is what makes the dry run exit non-zero).
    commit(out, Shape::DecomposedPaths, &["commit", "--dry-run", "--short"]);
    commit(out, Shape::Sparse, &["commit", "--dry-run", "--short"]);
    commit(out, Shape::Submodule, &["commit", "--dry-run", "--long"]);
    commit(out, Shape::Conflicted, &["commit", "--dry-run", "--short"]);

    // `core.abbrev` decides the width of the id in the `[<branch> <id>]` line.
    // `NoIndexTrees` is the only shape that configures it (to 10), so it is the
    // only place a hard-coded 7 is visible — and it is visible on the most-read
    // line `commit` prints.
    commit(out, Shape::NoIndexTrees, &["commit", "--allow-empty", "-m", "x"]);
    commit(out, Shape::NoIndexTrees, &["commit", "--allow-empty", "--amend", "-m", "x"]);
}

/// The `Hooked` shape, where `pre-commit` and `commit-msg` both run.
///
/// The `commit-msg` hook appends a line to the message, so whether it ran is
/// visible in the commit id — that is what makes `--no-verify` measurable rather
/// than merely accepted. The `pre-commit` hook writes `hook-ran.txt`, so whether
/// it ran shows up as an untracked file in the status probe.
fn hooks(out: &mut Vec<Case>) {
    commit(out, Shape::Hooked, &["commit", "--allow-empty", "-m", "hooked"]);
    // `-n` is `--no-verify`; it must skip both hooks, so the message keeps the
    // exact bytes passed and `hook-ran.txt` is absent.
    commit(out, Shape::Hooked, &["commit", "--allow-empty", "-n", "-m", "hooked"]);
    // From a subdirectory. A hook is spawned with the top level as its working
    // directory no matter where the command was run from, so `hook-ran.txt`
    // lands at the root either way.
    out.push(
        Case::new("commit", &["commit", "--allow-empty", "-n", "-m", "hooked"], Shape::Hooked)
            .in_dir("sub"),
    );
    // Cleanup runs *after* `commit-msg`, so the appended line is subject to it —
    // verbatim and default must still agree here because the hook's line is a
    // plain one.
    commit(out, Shape::Hooked, &["commit", "--allow-empty", "-m", "hooked", "--cleanup=verbatim"]);
    // An amend runs the hooks too, so the replacement commit also carries the
    // appended line.
    commit(out, Shape::Hooked, &["commit", "--allow-empty", "--amend", "--no-edit"]);
    // A partial commit points `pre-commit` at a *different* index file
    // (`next-index-<pid>.lock` rather than `.git/index`), which is why the hook
    // records `$GIT_INDEX_FILE`. What it records is per-process and is not
    // compared; that the commit still happens, and still carries the hook's
    // message line, is.
    commit(out, Shape::Hooked, &["commit", "--allow-empty", "-m", "hooked", "-o", "--", "top.txt"]);
    // A dry run runs no hooks at all, and ignores `--allow-empty` while it is at
    // it: the tree is clean, so it reports "nothing to commit" and exits 1.
    commit(out, Shape::Hooked, &["commit", "--dry-run", "--allow-empty", "-m", "hooked"]);
}

/// Committing while a merge is in progress.
///
/// The index carries stage 1/2/3 entries for `conflict.txt`. Which invocations
/// git refuses and which it lets through is not a single "are we merging" gate:
/// a plain commit is refused because entries are unmerged, a *pathspec* commit
/// is refused for a different reason and with a different message, and anything
/// that stages the path first — `-a`, `--include <path>` — resolves the conflict
/// and commits the merge.
fn merge_state(out: &mut Vec<Case>) {
    // `refresh_cache_or_die()` lists every unmerged path on stdout and then
    // dies: three lines of hints on stderr, one path on stdout, exit 128.
    refusal(out, Shape::Conflicted, &["commit", "-m", "resolved"]);
    // `--no-verify` and `--allow-empty` do not exempt anything from that gate.
    refusal(out, Shape::Conflicted, &["commit", "-m", "resolved", "--no-verify"]);
    refusal(out, Shape::Conflicted, &["commit", "--allow-empty", "-m", "resolved"]);
    // A partial commit during a merge is refused earlier and by name: a merge
    // commit records the whole index, so a paths-limited one is meaningless.
    refusal(out, Shape::Conflicted, &["commit", "-m", "resolved", "--", "conflict.txt"]);
    // `-a` stages the conflicted file's worktree content — markers and all —
    // which *resolves* it, and the merge commit is then written with both
    // parents. This is the row that separates "refuse while MERGE_HEAD exists"
    // from git's actual rule.
    commit(out, Shape::Conflicted, &["commit", "-am", "resolved"]);
    // `--include <path>` reaches the same place by the other route: the path is
    // added to the index first, so nothing is unmerged by the time the gate runs.
    commit(out, Shape::Conflicted, &["commit", "-m", "resolved", "-i", "--", "conflict.txt"]);
}

/// `commit-tree`: the plumbing that writes a commit object and nothing else.
///
/// Its whole output is the new object's id, so every case here asserts the
/// object's bytes directly — a parent in the wrong order, a message missing its
/// trailing newline, or a dropped `encoding` header all change the id.
fn commit_tree(out: &mut Vec<Case>) {
    fn ct(out: &mut Vec<Case>, shape: Shape, args: &[&str]) {
        out.push(Case::new("commit-tree", args, shape));
    }

    // The message on stdin, in both spellings — bare (no `-m`, no `-F`) and
    // `-F -`. They have to produce the same object.
    out.push(Case::with_stdin(
        "commit-tree",
        &["commit-tree", "HEAD^{tree}"],
        Shape::Linear,
        STDIN_MSG,
    ));
    out.push(Case::with_stdin(
        "commit-tree",
        &["commit-tree", "-F", "-", "HEAD^{tree}"],
        Shape::Linear,
        STDIN_MSG,
    ));
    // Repeated `-m`: paragraphs joined by a blank line, same as `commit`.
    ct(out, Shape::Linear, &["commit-tree", "-m", "one", "-m", "two", "-m", "three", "HEAD^{tree}"]);
    // How the tree is spelled: a peel, a raw object name, the `<rev>:` path
    // syntax, and a subtree that is no commit's root tree.
    ct(out, Shape::Linear, &["commit-tree", "HEAD^{tree}", "-m", "peeled"]);
    ct(out, Shape::Linear, &["commit-tree", BASE_TREE, "-m", "raw-oid"]);
    ct(out, Shape::Linear, &["commit-tree", "HEAD:", "-m", "tree-via-path"]);
    ct(out, Shape::Linear, &["commit-tree", "HEAD:src", "-m", "subtree"]);
    // The empty tree, which git resolves without it being in the object store.
    ct(out, Shape::Linear, &["commit-tree", EMPTY_TREE, "-m", "empty-tree"]);

    // An empty message file: a commit whose message is zero bytes, which
    // `commit` refuses and `commit-tree` writes without comment.
    ct(out, Shape::Linear, &["commit-tree", "-F", "/dev/null", "HEAD^{tree}"]);
    // `i18n.commitEncoding` reaches `commit-tree` too, and the header it adds
    // changes the id. Paired deliberately with the `commit` rows above: the two
    // verbs write the same header from the same setting, so a port that has it
    // in one and not the other fails exactly one of the pair.
    out.push(
        Case::new("commit-tree", &["commit-tree", "-m", "enc", "HEAD^{tree}"], Shape::Linear)
            .with_config(&[("i18n.commitEncoding", "ISO-8859-1")]),
    );

    // Parents. One, two, three and four, in the order given — the parent list is
    // ordered and a set would lose that. `Octopus` is the only shape with a
    // commit that has more than two parents to name.
    ct(out, Shape::Merged, &["commit-tree", "-p", "HEAD", "-m", "child", "HEAD^{tree}"]);
    ct(
        out,
        Shape::Branched,
        &["commit-tree", "-p", "main", "-p", "feature", "-m", "two", "HEAD^{tree}"],
    );
    ct(
        out,
        Shape::Octopus,
        &[
            "commit-tree", "-p", "HEAD^1", "-p", "HEAD^2", "-p", "HEAD^3", "-m", "three",
            "HEAD^{tree}",
        ],
    );
    ct(
        out,
        Shape::Octopus,
        &[
            "commit-tree", "-p", "oct-a", "-p", "oct-b", "-p", "oct-c", "-p", "oct-side", "-m",
            "four", "HEAD^{tree}",
        ],
    );
    // A repeated parent is dropped with a warning on stderr and the commit is
    // still written, so the object has one parent and the command exits 0.
    out.push(Case::strict(
        "commit-tree",
        &["commit-tree", "-p", "HEAD", "-p", "HEAD", "-m", "dup", "HEAD^{tree}"],
        Shape::Linear,
    ));

    // Refusals. A tree where a commit belongs, an annotated tag as a parent
    // (`commit-tree` does not peel it — it wants a commit), a path where an
    // object name belongs, and two trees where one belongs.
    out.push(Case::strict(
        "commit-tree",
        &["commit-tree", "-p", "HEAD^{tree}", "-m", "x", "HEAD^{tree}"],
        Shape::Linear,
    ));
    out.push(Case::strict(
        "commit-tree",
        &["commit-tree", "-p", "v0.2.0", "-m", "x", "HEAD^{tree}"],
        Shape::Branched,
    ));
    out.push(Case::strict("commit-tree", &["commit-tree", "-m", "x", "README.md"], Shape::Linear));
    out.push(Case::strict(
        "commit-tree",
        &["commit-tree", "-m", "x", "HEAD^{tree}", "extra"],
        Shape::Linear,
    ));
}
