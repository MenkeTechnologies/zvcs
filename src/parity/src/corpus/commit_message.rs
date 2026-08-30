//! Differential corpus cases for how a commit **message** is assembled — the
//! bytes that end up in the commit object, which is the one thing a commit is
//! permanently judged by.
//!
//! Every case here is one invocation compared against stock git for stdout,
//! exit code and the post-command state digest. The digest carries
//! `rev-parse HEAD` and `cat-file --batch-check --batch-all-objects`, so the
//! commit *id* is compared — and an id is a function of the message bytes. A
//! message that differs from stock's by one trailing space, one `\r`, or one
//! absent final newline produces a different id, which is exactly the class of
//! defect that is invisible in `git log` and fatal to two repositories ever
//! converging.
//!
//! # How this divides territory with the modules that were read first
//!
//! Read in full before a line of this was written, and each named here with
//! what it owns:
//!
//! * **`commit_family.rs`** — the nearest neighbour, and the one this module is
//!   carved out of. It owns the *inventory*: each message source once (`-m`
//!   twice, `-F <file>`, `-F -`, `-C`, `-c`, `--squash=`, `--fixup=`,
//!   `--fixup=amend:`, `--fixup=reword:`), the five `--cleanup` modes over one
//!   messy `-m` message, `commit.cleanup`, `core.commentChar=;`,
//!   `core.commentString=//`, `commit.template` as *configuration*,
//!   `commit.status`, `commit.verbose`, one and two `--trailer`s,
//!   `trailer.<token>.key`/`ifExists`/`separators`, `i18n.commitEncoding`, and
//!   the whole of staging, `--amend`, the report modes, hooks, the merge gate
//!   and `commit-tree`'s parent list. This module owns what happens when those
//!   sources **combine**, and what happens when the message carries bytes the
//!   inventory rows do not: `CR`, a trailing space, a missing final newline, a
//!   `NUL`, a scissors line reached through an editor.
//! * **`text_plumbing.rs`** — `interpret-trailers` and `stripspace` as *verbs*,
//!   with their own argv and their own stdin. This module owns the same two
//!   engines as they are reached *inside* `commit`, where the caller cannot pass
//!   `--trim-empty` or `--strip-comments` and the mode is chosen by `--cleanup`
//!   and by whether an editor ran.
//! * **`hooks_identity.rs`** — `--author=` in every spelling, accepted and
//!   refused, plus `--committer=`, `core.hooksPath` and the identity
//!   resolution order. **`--author` is therefore absent from this module
//!   entirely**, including the three spellings where the port diverges; they
//!   are that module's rows to report. `--date=` is *not* covered there, or
//!   anywhere else in the corpus, and is owned here.
//! * **`stateful_side_files.rs`** — `.git/MERGE_MSG`, `SQUASH_MSG` and the rest
//!   of the operation-state files as *files*. Nothing here writes one.
//! * **`mail_series.rs`** / `am_deep.rs` — `format-patch` rendering and `am
//!   --signoff` over a mailbox that already carries the trailer. The sign-off
//!   rows here are `commit`'s own, over messages supplied on the command line
//!   and on stdin.
//! * **`log_format.rs`** — `%B`/`%s`/`%b`/`%f`/`%N` and the `--date=` *display*
//!   modes, over the commits a fixture already has.
//! * **`config_reads.rs`** — `core.commentChar`/`core.commentString` as values
//!   read back by `git config`. Here they decide which lines of a message
//!   survive.
//! * **`sequences.rs`** — multi-step workflows against one repository.
//! * **`exit_codes.rs`** — `commit --author=nobrackets` and the conflicted
//!   `commit` refusal, both `Case::strict`.
//!
//! # What no case here can measure, and why
//!
//! **The round trip out.** `log --format=%B`, `cat-file commit`, `show -s` and
//! `format-patch` over a commit *this corpus just wrote* would be the
//! byte-exact contract stated as one assertion. A [`Case`] is one argv against
//! a pristine copy, so writing the commit and reading it back is two
//! invocations and needs a [`crate::runner::Sequence`], which this module's
//! registration (`cases(&mut Vec<Case>)`) cannot express. The substitute is
//! exact rather than approximate: the state digest hashes the object store and
//! `HEAD`, so the message bytes are asserted through the commit id, which is
//! strictly finer than any rendering of them — two messages with the same id
//! are the same bytes.
//!
//! **Two `--date` spellings, excluded as unmeasurable after being run.** Both
//! were run twice against stock 2.55.0 in identical fresh copies of
//! `Shape::Dirty` and compared on `rev-parse HEAD`, per the rule that a case
//! must be deterministic on stock *alone* before it is worth comparing sides:
//!
//! * `--date=now` — the wall clock, by definition. Two stock runs a second
//!   apart gave `b7436c3bae…` and `30e07e3fdc…`.
//! * `--date=2017-07-14` — a date with no time. Git fills the time of day in
//!   from the wall clock, so two stock runs gave `0a98e8e7c4…` and
//!   `6848db1bc0…`. It looks absolute and is not. The four ids above are not
//!   reproducible values to check against; they are two pairs that disagree,
//!   and a fresh pair of runs produces two new ids that disagree again.
//!
//! `--amend --reset-author` is measurable and is already `commit_family.rs`'s
//! row: `env::harden` pins `GIT_AUTHOR_DATE`, and reset-author takes the author
//! date from there rather than from the clock. Confirmed directly rather than
//! inferred — `commit --amend --no-edit --reset-author` with **no** `--date` at
//! all, on `Shape::Linear`, rewrites `edfab1b716…` into `edfab1b716…`: the
//! replacement is byte-identical to the commit it replaces, twice, in two runs
//! a second apart. It could only be identical if the replaced author line
//! carried the pinned date, and it could only be identical *twice* if no clock
//! were read.
//!
//! `-S`/`--gpg-sign` is unusable here for the reason `commit_family.rs` gives:
//! the hermetic `HOME` has no key, so both sides only ever reach the refusal.
//!
//! # What the port does differently, reproduced by hand
//!
//! Every row below was re-run outside the harness, stock and port side by side
//! in fresh copies of the same shape under `env::harden`'s environment, and the
//! commit objects compared with `od -c`. The first is the one this module was
//! written to find: the only row where both sides exit 0, print the same
//! summary line, and commit different objects whose messages differ by
//! whitespace alone.
//!
//! * **`--cleanup=verbatim -F -` over a message with no final newline** — the
//!   difference is one byte and both sides exit 0 with a plausible
//!   `[main …] no trailing newline` summary. Stock's object ends
//!   `… n e w l i n e` (`756a48e511…`); the port's ends `… n e w l i n e \n`
//!   (`0f4e1882a4…`), which is byte for byte the object stock produces under
//!   the *default* cleanup. The port's `verbatim` does not suppress the
//!   terminator. Nothing renders this: `git log` shows the same subject on both
//!   sides, and two repositories that should converge never do.
//! * **`--cleanup=verbatim -F -` over a message that is whitespace and nothing
//!   else** — stock writes the six bytes and lands `edcecd4d3e…`; the port
//!   answers `Aborting commit due to empty commit message.` and exits 1 with
//!   `HEAD` unmoved. The emptiness test runs before cleanup rather than after
//!   it. `commit-tree` over the identical payload writes `432765fcc2…` on
//!   **both** sides, so the writer is right and the test is in the wrong place.
//! * **A `NUL` in the message** — stock refuses
//!   (`error: a NUL byte in commit log message not allowed.` then
//!   `fatal: failed to write commit object`, exit 128); the port exits 0 and
//!   writes `23a106de4d…`, whose message really does contain the `\0`. The same
//!   payload through `commit-tree` is refused by *both* sides with the one
//!   `error:` line and exit 1 — so the check exists in one of the port's two
//!   commit writers and not in the other, which is exactly what pairing the two
//!   verbs over one payload was for.
//! * **`-C <annotated tag>`** — stock peels the tag and reuses the commit's
//!   message, landing `fd02a19099…` (`add two`). The port reads the tag object
//!   itself: `error: object d7277ea…is a tag, not a commit` /
//!   `fatal: could not lookup commit 'v0.2.0'`, exit 128. The lightweight tag
//!   row, which needs no peeling, agrees on both sides.
//! * **`-F` on something unreadable** — stock exits **128** with
//!   `fatal: could not read log file '<path>': <strerror>`; the port exits
//!   **1** with ``zvcs: commit: could not read message file `<path>`:
//!   <strerror> (os error <n>)``. Both spellings, missing file and directory.
//! * **`trailer.<token>.cmd`** — stock runs the command and commits
//!   `Acked-by: tester A` (`e4faf827b7…`). The port refuses:
//!   `zvcs: commit: unsupported config trailer.<key-alias>.cmd/.command (needs
//!   shell execution)`, exit 1, preceded by two
//!   `warning: more than one trailer.ack.key` / `…cmd` lines that are wrong on
//!   their own terms — one of each key was set.
//! * **`--date=not a date`** — the one row here where the two sides agree on
//!   *everything a user sees* and still leave different repositories. Identical
//!   stderr (`fatal: invalid date format: not a date`) and exit 128, but stock
//!   has already written the index tree `5f9e862135…` (105 bytes) and a
//!   cache-tree extension into `.git/index` (325 bytes) before it parses the
//!   date, while the port validates first and writes neither (index 306 bytes).
//!   `pull --cleanup=bogus` in `pull_family.rs` is the same ordering question
//!   answered the other way round, with the port doing the extra work.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

// ---------------------------------------------------------------------------
// Message payloads delivered on stdin
// ---------------------------------------------------------------------------
//
// `&'static [u8]` literals rather than files, because a case that reads the
// filesystem for its input is not reproducible — and because the bytes this
// module is about (a lone `CR`, a trailing space, an absent final newline, a
// `NUL`) cannot be written as a tracked fixture path without a shape that has
// one, and a case cannot add a shape.

/// CRLF line endings throughout. `verbatim` keeps every `\r`; every other mode
/// treats it as trailing whitespace and drops it, which is a different commit id
/// over identical input.
const MSG_CRLF: &[u8] = b"subject\r\n\r\nbody with cr\r\n";
/// Leading blank lines, a run of blank lines in the middle, and a run at the
/// end. `verbatim` keeps all three runs; `whitespace` and `strip` drop the
/// leading and trailing ones and collapse the interior run to a single blank.
const MSG_BLANKS: &[u8] = b"\n\n\nsubject after blanks\n\n\nbody\n\n\n";
/// Whitespace and nothing else — six bytes. Empty to every mode that trims, and
/// *not* empty to `verbatim`, which is the whole point of the pair below. Stock
/// commits those six bytes under `verbatim`; the port refuses to, and its
/// `commit-tree` writes them, so the check is in the wrong place rather than
/// missing from the writer.
const MSG_WS_ONLY: &[u8] = b"   \n\t\n";
/// No trailing newline. `verbatim` writes the message without one, so the commit
/// object's last byte is `e`; every other mode terminates the line.
const MSG_NO_NEWLINE: &[u8] = b"no trailing newline";
/// Comment lines and nothing else. Survives intact when no editor ran (the
/// default cleanup for a supplied message is `whitespace`, which keeps
/// comments), and becomes empty when one did.
const MSG_COMMENT_ONLY: &[u8] = b"# only a comment\n# another\n";
/// Zero bytes.
const MSG_EMPTY: &[u8] = b"";
/// A `NUL` in the subject. Stock refuses to write the object at all, from both
/// `commit` (`error: a NUL byte in commit log message not allowed.` then
/// `fatal: failed to write commit object`, exit 128) and `commit-tree` (the
/// `error:` line alone, exit 1). The port refuses it from `commit-tree` and
/// **writes it** from `commit`.
const MSG_NUL: &[u8] = b"sub\0ject\nbody\n";
/// Multi-byte UTF-8 in both the subject and the body, so `i18n.commitEncoding`
/// has something whose bytes are worth *not* transcoding.
const MSG_UTF8: &[u8] = "résumé café\n\nbody → arrow\n".as_bytes();
/// Git's scissors line with text on both sides of it. Cut only when the message
/// went through an editor *and* the mode is `scissors`; kept in every other
/// combination.
const MSG_SCISSORS: &[u8] =
    b"subject\n\nkeep this\n# ------------------------ >8 ------------------------\ncut this away\n";
/// A comment line, a body line with trailing spaces, and trailing blank lines —
/// the three things the modes disagree about, in one payload delivered through
/// a *file* (stdin) rather than through `-m`, which is what makes the
/// editor/no-editor split in `default` visible.
const MSG_MESSY: &[u8] = b"subject\n\n# a comment\nbody   \n\n\n";
/// Trailing whitespace on the **subject** line and a tab at the end of the body
/// — the position `MSG_MESSY` does not reach, since its trailing spaces are on a
/// body line and a subject is the one line every renderer shows. `verbatim`
/// keeps both (`d7c67ccc69…`); `whitespace`, `strip` and `default` all trim to
/// `subject\n\nbody\n` and share one id (`e96753c76b…`).
const MSG_TRAILING_WS: &[u8] = b"subject   \n\nbody\t\n";

/// A fixed instant, spelled six ways. Chosen inside git's supported range, with
/// an explicit zone in every spelling that has one, so nothing here resolves
/// against the wall clock or against `TZ`.
const EPOCH: &str = "1500000000";

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    sources_combined(out);
    cleanup_bytes(out);
    scissors_and_editor(out);
    trailer_placement(out);
    comment_syntax(out);
    author_date(out);
    commit_tree_bytes(out);
}

fn commit(out: &mut Vec<Case>, shape: Shape, args: &[&str]) {
    out.push(Case::new("commit", args, shape));
}

/// A refusal, where the message on stderr is the behaviour being measured.
fn refusal(out: &mut Vec<Case>, shape: Shape, args: &[&str]) {
    out.push(Case::strict("commit", args, shape));
}

/// A message delivered on stdin.
fn piped(out: &mut Vec<Case>, shape: Shape, args: &[&str], stdin: &'static [u8]) {
    out.push(Case::with_stdin("commit", args, shape, stdin));
}

/// A refusal over a message delivered on stdin.
fn piped_refusal(out: &mut Vec<Case>, shape: Shape, args: &[&str], stdin: &'static [u8]) {
    out.push(Case { compare_stderr: true, ..Case::with_stdin("commit", args, shape, stdin) });
}

// ---------------------------------------------------------------------------
// Sources that combine
// ---------------------------------------------------------------------------

/// What happens when two message sources are *both* honoured.
///
/// `commit_family.rs` pins each source once and pins the pairs git **refuses**
/// (`-m` with `-F`, `--squash` with `--fixup`). The pairs git accepts are a
/// different question and a larger one: `--squash=` and `--fixup=` generate a
/// subject and then take a body from `-m`, `--template=` loses to `-m` and beats
/// `commit.template`, and `-C`/`-c` differ from each other in whether the editor
/// runs at all. Every row below produces a distinct commit id, so a port that
/// picked one source and dropped the other fails on the id and not merely on a
/// missing line.
fn sources_combined(out: &mut Vec<Case>) {
    // `--squash=`/`--fixup=` write `squash! <subject>` / `fixup! <subject>` and
    // then append `-m` as the body, separated by a blank line. Neither is
    // "last wins" and neither is a refusal.
    commit(out, Shape::Dirty, &["commit", "--squash=HEAD", "-m", "extra"]);
    commit(out, Shape::Dirty, &["commit", "--fixup=HEAD", "-m", "extra"]);

    // The same two generators with no `-m`, under `--cleanup=verbatim`. The
    // editor is `true`, so whatever git *prepared* is what gets committed —
    // which is how the prepared buffer becomes observable at all. `--squash=`
    // leaves an empty body below the generated subject; `--fixup=amend:` copies
    // the borrowed commit's whole message in below it. Both then carry the
    // status block git appends for the editor, so the two ids differ from their
    // cleaned-up counterparts in `commit_family.rs` and from each other.
    commit(out, Shape::Dirty, &["commit", "--squash=HEAD", "--cleanup=verbatim"]);
    commit(out, Shape::Dirty, &["commit", "--fixup=amend:HEAD", "--cleanup=verbatim"]);

    // `--template=` as a *flag*. `commit_family.rs` reaches the template through
    // `commit.template` only, so the command-line spelling — and its precedence
    // over the configured one — has never been exercised. `README.md` is the
    // template because a case cannot write a file first.
    commit(out, Shape::Dirty, &["commit", "--template=README.md", "--cleanup=verbatim"]);
    // The flag outranks the config: a port that read the config first would
    // commit `src/lib.rs`'s text and land on a different id.
    out.push(
        Case::new("commit", &["commit", "--template=README.md", "--cleanup=verbatim"], Shape::Dirty)
            .with_config(&[("commit.template", "src/lib.rs")]),
    );
    // ...and loses to `-m`, which suppresses the editor entirely, so the
    // template is never read and the message is the argument alone.
    commit(out, Shape::Dirty, &["commit", "--template=README.md", "-m", "subj"]);

    // `-C <tag>`. Both spellings have to peel to the commit and take *its*
    // message: an annotated tag is an object with a message of its own, and
    // reusing that one instead would be silently plausible. Stock lands
    // `fd02a19099…` for both. The port peels the lightweight tag and not the
    // annotated one, which is the first row's divergence.
    commit(out, Shape::Branched, &["commit", "--allow-empty", "-C", "v0.2.0"]);
    commit(out, Shape::Branched, &["commit", "--allow-empty", "-C", "v0.1.0"]);

    // `-C` with `--reset-author`, one of the three places `--reset-author` is
    // legal — and a **negative** pin rather than the difference it looks like.
    // `-C` borrows the author identity along with the message, `env::harden`
    // pins that identity to the same values the borrowed commit already
    // carries, and so the replacement replaces nothing: this row, a plain
    // `-C HEAD` and the `--cleanup=verbatim` row below all land on
    // `ad8247e1ed…`. What it measures is that `--reset-author` is honoured and
    // consults the pinned environment rather than the clock; a port that took
    // the author date from the wall clock would fail it every second.
    commit(out, Shape::Dirty, &["commit", "-C", "HEAD", "--reset-author"]);
    // `-C` takes the message without an editor and `-c` runs one over it, so
    // under `verbatim` the two diverge by the whole status block —
    // `ad8247e1ed…` against `9ab7dceeec…`. `-c`'s buffer also carries a
    // `# Date:` line, which git writes whenever the author date was supplied by
    // the borrowed commit at all: both dates here are `1700000000 +0000` and the
    // line still appears, so it is not a difference between them.
    commit(out, Shape::Dirty, &["commit", "-C", "HEAD", "--cleanup=verbatim"]);
    commit(out, Shape::Dirty, &["commit", "-c", "HEAD", "--cleanup=verbatim"]);

    // `-F` on something that cannot be read. Two `strerror` cases rather than
    // one, because a port that special-cases "not found" still has to answer for
    // a directory, and git reports both through the same
    // `could not read log file '%s': %s`.
    refusal(out, Shape::Dirty, &["commit", "-F", "nosuchmsg.txt"]);
    refusal(out, Shape::Dirty, &["commit", "-F", "src"]);
}

// ---------------------------------------------------------------------------
// Cleanup, crossed with the bytes it is cleaning
// ---------------------------------------------------------------------------

/// `--cleanup` over messages carrying the bytes the modes actually disagree
/// about.
///
/// `commit_family.rs` sweeps the five modes over one `-m` message holding a
/// comment line, a trailing-space line and trailing blanks. That leaves the
/// byte-level half unmeasured, and it is the half where a difference is
/// invisible: a `\r` that should have been dropped, a trailing space that should
/// have survived, a final newline that should not have been added. None of those
/// change a single rendered character of `git log`; all of them change the
/// commit id, so two repositories that should converge never do.
///
/// Every payload arrives on stdin as a byte literal, which is the only way to
/// state these messages exactly — a `-m` argument cannot carry a `NUL`, and no
/// shape tracks a file whose content ends without a newline.
fn cleanup_bytes(out: &mut Vec<Case>) {
    // CRLF across the modes. `verbatim` keeps every `\r`; `whitespace`, `strip`
    // and `default` all treat it as trailing whitespace and drop it, so three of
    // the four rows share an id and the fourth does not. A port that never looks
    // at `\r` passes `verbatim` and fails the rest; one that always strips it
    // fails only `verbatim`.
    for mode in ["verbatim", "whitespace", "strip", "default"] {
        let flag = format!("--cleanup={mode}");
        piped(out, Shape::Dirty, &["commit", &flag, "-F", "-"], MSG_CRLF);
    }

    // Leading, interior and trailing blank-line runs. `whitespace` and `strip`
    // agree here — both drop the outer runs and collapse the interior one to a
    // single blank — so the assertion is that they agree *and* that `verbatim`
    // keeps all nine lines.
    for mode in ["verbatim", "whitespace", "strip"] {
        let flag = format!("--cleanup={mode}");
        piped(out, Shape::Dirty, &["commit", &flag, "-F", "-"], MSG_BLANKS);
    }

    // A message that is whitespace and nothing else. Under the default it is
    // empty and the commit is refused; under `verbatim` it is six bytes and the
    // commit is written with them (`edcecd4d3e…`). The pair is the sharpest
    // statement of what `verbatim` means: the emptiness test runs on the message
    // *after* cleanup, so a port that trims before testing refuses a commit
    // stock writes — which is what the port does.
    piped_refusal(out, Shape::Dirty, &["commit", "-F", "-"], MSG_WS_ONLY);
    piped(out, Shape::Dirty, &["commit", "--cleanup=verbatim", "-F", "-"], MSG_WS_ONLY);
    // ...and `--allow-empty-message` lets the trimmed version through, which is
    // a third id again: a commit whose message is zero bytes.
    piped(
        out,
        Shape::Dirty,
        &["commit", "--allow-empty-message", "--cleanup=strip", "-F", "-"],
        MSG_WS_ONLY,
    );

    // A message with no final newline. Under `verbatim` the object ends in `e`;
    // under the default git terminates the line. This is the whole difference
    // between the two ids — one byte, in a position no renderer shows, and it
    // is where the port diverges (`756a48e511…` against `0f4e1882a4…`).
    piped(out, Shape::Dirty, &["commit", "--cleanup=verbatim", "-F", "-"], MSG_NO_NEWLINE);
    piped(out, Shape::Dirty, &["commit", "-F", "-"], MSG_NO_NEWLINE);
    // The two controls that localise that byte, both of which the port passes.
    //
    // `-m` under the same mode: git terminates an argument-supplied message
    // before cleanup ever sees it, so `verbatim` has no final newline to
    // suppress and the port agrees (`5404311323…`). The defect is reachable
    // only when the message arrives from a file.
    commit(out, Shape::Dirty, &["commit", "--cleanup=verbatim", "-m", "one line"]);
    // Trailing spaces on an argument-supplied subject, which `verbatim` *does*
    // have to keep — so the port's `verbatim` is not simply ignored, it is
    // wrong about the terminator specifically.
    commit(out, Shape::Dirty, &["commit", "--cleanup=verbatim", "-m", "subj   "]);
    // Adding a trailer re-terminates the message whatever the mode says: `-s`
    // over the unterminated payload lands `5999315849…` under `verbatim` and
    // under the default alike. That is why the byte above survives in practice
    // — every signed-off commit takes the other path.
    piped(out, Shape::Dirty, &["commit", "-s", "--cleanup=verbatim", "-F", "-"], MSG_NO_NEWLINE);
    piped(out, Shape::Dirty, &["commit", "-s", "-F", "-"], MSG_NO_NEWLINE);

    // Trailing whitespace on the subject line, across the four modes. Three of
    // them collapse to `subject\n\nbody\n`; `verbatim` keeps `subject   ` and
    // the tab. The port agrees on all four, which is what makes the
    // final-newline row above a defect in one rule rather than in the mode.
    for mode in ["verbatim", "whitespace", "strip", "default"] {
        let flag = format!("--cleanup={mode}");
        piped(out, Shape::Dirty, &["commit", &flag, "-F", "-"], MSG_TRAILING_WS);
    }

    // Comment lines with no editor: the default cleanup for a *supplied*
    // message is `whitespace`, not `strip`, so both lines survive and the commit
    // has a message made entirely of comments.
    piped(out, Shape::Dirty, &["commit", "-F", "-"], MSG_COMMENT_ONLY);

    // Zero bytes: refused, and accepted under `--allow-empty-message`. The
    // accepted row lands on the same id as the trimmed whitespace-only row
    // above, which is what makes "empty" one state rather than two.
    piped_refusal(out, Shape::Dirty, &["commit", "-F", "-"], MSG_EMPTY);
    piped(out, Shape::Dirty, &["commit", "--allow-empty-message", "-F", "-"], MSG_EMPTY);

    // A `NUL` in the message. Git refuses to write the object —
    // `error: a NUL byte in commit log message not allowed.` then
    // `fatal: failed to write commit object` — and the refusal is the contract:
    // an accepted `NUL` is a commit object whose header parse ends early for
    // every reader that meets it afterwards.
    piped_refusal(out, Shape::Dirty, &["commit", "-F", "-"], MSG_NUL);

    // Multi-byte UTF-8 through the cleanup path, with and without the header
    // that claims it is something else. `i18n.commitEncoding` must change the
    // header and *not* the message bytes, so the two rows differ by exactly the
    // `encoding` line and a port that transcoded would fail the second while
    // passing the first.
    piped(out, Shape::Dirty, &["commit", "-F", "-"], MSG_UTF8);
    out.push(
        Case::with_stdin("commit", &["commit", "-F", "-"], Shape::Dirty, MSG_UTF8)
            .with_config(&[("i18n.commitEncoding", "ISO-8859-1")]),
    );
}

// ---------------------------------------------------------------------------
// The modes only an editor buffer reaches
// ---------------------------------------------------------------------------

/// `--cleanup=scissors`, `default`'s two meanings, and what `-v` puts in the
/// buffer.
///
/// `commit_family.rs` records that a `-m` message is never cut at the scissors
/// and pins the pair that proves it. The other half — the case where git
/// **does** cut — needs the message to have gone through an editor, and
/// `env::harden` pins `GIT_EDITOR=true`, which accepts the prepared buffer
/// unchanged. `-e` is therefore not a dead end but the way in: it makes the
/// buffer the message, so the scissors truncation, the editor-only meaning of
/// `default`, and the diff `-v` appends all become commit ids.
fn scissors_and_editor(out: &mut Vec<Case>) {
    // The three answers to one payload, all distinct:
    //   -e + scissors  -> cut at the line, `cut this away` gone
    //   -e + default   -> the scissors line is a comment and goes; the text
    //                     below it stays
    //   scissors alone -> no editor ran, so nothing is cut and the comment line
    //                     survives too (`scissors` does not strip comments)
    piped(out, Shape::Dirty, &["commit", "-e", "--cleanup=scissors", "-F", "-"], MSG_SCISSORS);
    piped(out, Shape::Dirty, &["commit", "-e", "-F", "-"], MSG_SCISSORS);
    piped(out, Shape::Dirty, &["commit", "--cleanup=scissors", "-F", "-"], MSG_SCISSORS);
    // The same cut, chosen from configuration rather than from argv.
    out.push(
        Case::with_stdin("commit", &["commit", "-e", "-F", "-"], Shape::Dirty, MSG_SCISSORS)
            .with_scoped_config(vec![ConfigEntry::set(
                ConfigScope::Repo,
                "commit.cleanup",
                "scissors",
            )]),
    );

    // `default` is two modes wearing one name: `whitespace` for a message
    // supplied on the command line or in a file, `strip` for one that went
    // through an editor. One payload, one flag apart, two ids — a port that
    // hard-codes either meaning fails exactly one of these.
    piped(out, Shape::Dirty, &["commit", "-F", "-"], MSG_MESSY);
    piped(out, Shape::Dirty, &["commit", "-e", "--no-status", "-F", "-"], MSG_MESSY);

    // Comment-only through an editor: `strip` empties it, so the commit is
    // refused — and `--allow-empty-message` then writes the empty message. The
    // pair separates "the message became empty" from "there was no message".
    piped_refusal(
        out,
        Shape::Dirty,
        &["commit", "-e", "--no-status", "-F", "-"],
        MSG_COMMENT_ONLY,
    );
    piped(
        out,
        Shape::Dirty,
        &["commit", "-e", "--no-status", "--allow-empty-message", "-F", "-"],
        MSG_COMMENT_ONLY,
    );

    // The anchor for the three rows below: the prepared buffer with nothing
    // added to it, committed verbatim. `b9205de8ad…`, whose message is
    // `buffer`, a blank line, and the fifteen comment lines of the status
    // block. Without this row in the corpus the equivalence the `-v` rows
    // assert has nothing to be equal *to*.
    commit(out, Shape::Dirty, &["commit", "-m", "buffer", "-e", "--cleanup=verbatim"]);
    // `--status`/`--no-status` as *flags*. `commit_family.rs` reaches the same
    // decision through `commit.status` and only alongside a template; these are
    // the argv spelling, and under `verbatim` the difference is the whole
    // fifteen-line status block, so this id (`d02fc7280e…`) is far from the
    // anchor's.
    commit(out, Shape::Dirty, &["commit", "-m", "buffer", "-e", "--cleanup=verbatim", "--no-status"]);
    // `-v` and `-vv` append a diff below a cut line that git removes *whatever*
    // the cleanup mode says. Both land on the anchor's `b9205de8ad…` — measured,
    // not assumed — so a port that let the patch through would commit a message
    // containing one while still printing a plausible summary line.
    commit(out, Shape::Dirty, &["commit", "-m", "buffer", "-e", "--cleanup=verbatim", "-v"]);
    commit(out, Shape::Dirty, &["commit", "-m", "buffer", "-e", "--cleanup=verbatim", "-v", "-v"]);
}

// ---------------------------------------------------------------------------
// Trailers: where the block goes
// ---------------------------------------------------------------------------

/// `-s` and `--trailer` against a message that *already has* a trailer block.
///
/// `commit_family.rs` adds one trailer and then two to a bare `-m x`, where
/// there is nothing to place them relative to. Placement is the part that is
/// wrong in practice: whether a duplicate is suppressed, whether a blank line is
/// inserted first, and whether a new trailer joins the existing block or starts
/// a paragraph of its own. All three are decided by
/// `trailer.c:process_trailers`, and all three change the commit id.
fn trailer_placement(out: &mut Vec<Case>) {
    // The identity `env::harden` pins, spelled out, so `-s` over a message that
    // already ends in exactly this line has a duplicate to suppress.
    const SIGNED: &str = "subject\n\nSigned-off-by: zvcs parity <parity@example.invalid>";
    const PROSE: &str = "subject\n\nbody prose";
    const FOREIGN: &str = "subject\n\nAcked-by: A U Thor <a@example.invalid>";

    // Three placements, three ids:
    //   already signed -> unchanged, no second line
    //   prose ending   -> a blank line, then the trailer
    //   foreign block  -> appended *into* the block, no blank line
    commit(out, Shape::Dirty, &["commit", "-s", "-m", SIGNED]);
    commit(out, Shape::Dirty, &["commit", "-s", "-m", PROSE]);
    commit(out, Shape::Dirty, &["commit", "-s", "-m", FOREIGN]);
    // `--no-signoff` after `-s`: the later flag wins, so this is the message
    // alone.
    commit(out, Shape::Dirty, &["commit", "-s", "--no-signoff", "-m", "subj"]);
    // `format.signOff` is `format-patch`'s setting and `commit` has none of its
    // own — verified against `git help -c`, which lists `format.signOff` and no
    // `commit.signoff`. A port that honoured it here would add a trailer stock
    // does not, so this row is a negative pin and lands on the same id as a
    // bare `-m subj`.
    out.push(
        Case::new("commit", &["commit", "-m", "subj"], Shape::Dirty)
            .with_config(&[("format.signOff", "true")]),
    );

    // `trailer.where`, `trailer.ifexists` and `trailer.ifmissing` — the three
    // knobs `commit_family.rs` does not reach (it sets `trailer.<token>.key` and
    // `ifExists=addIfDifferent` only). Each is measured against a message that
    // already has a block, because none of them means anything against a message
    // that does not.
    out.push(
        Case::new(
            "commit",
            &[
                "commit",
                "--trailer",
                "Acked-by: A U Thor <a@example.invalid>",
                "-m",
                "subject\n\nReviewed-by: R Viewer <r@example.invalid>",
            ],
            Shape::Dirty,
        )
        .with_config(&[("trailer.where", "start")]),
    );
    out.push(
        Case::new(
            "commit",
            &[
                "commit",
                "--trailer",
                "Acked-by: B <b@example.invalid>",
                "-m",
                "subject\n\nAcked-by: A U Thor <a@example.invalid>",
            ],
            Shape::Dirty,
        )
        .with_config(&[("trailer.ifexists", "replace")]),
    );
    // `ifmissing=doNothing` suppresses the trailer entirely, so this is the
    // message alone — the one row here whose *absence* of output is the answer.
    out.push(
        Case::new(
            "commit",
            &["commit", "--trailer", "Acked-by: A <a@example.invalid>", "-m", "subject"],
            Shape::Dirty,
        )
        .with_config(&[("trailer.ifmissing", "doNothing")]),
    );

    // The two argument forms `commit_family.rs` does not spell: a bare token
    // with no separator at all (git writes `Acked-by:` with an empty value), and
    // the `token=value` form, which is accepted without `trailer.separators`
    // being widened.
    commit(out, Shape::Dirty, &["commit", "-m", "subj", "--trailer", "Acked-by"]);
    commit(
        out,
        Shape::Dirty,
        &["commit", "-m", "subj", "--trailer", "Acked-by=A U Thor <a@example.invalid>"],
    );

    // `trailer.<token>.cmd` runs a command and uses its output as the value,
    // with the `--trailer` argument passed as `$1`. `echo` is the command
    // because it is a shell builtin with no filesystem, no clock and no
    // environment in its answer, so the produced trailer — `Acked-by: tester A`
    // — is a constant.
    out.push(
        Case::new("commit", &["commit", "-m", "subj", "--trailer", "ack: A"], Shape::Dirty)
            .with_config(&[("trailer.ack.key", "Acked-by"), ("trailer.ack.cmd", "echo tester")]),
    );

    // A trailer added by an amend, over a message that has none. The trailer
    // machinery runs on the *borrowed* message, not on a fresh one.
    commit(
        out,
        Shape::Dirty,
        &["commit", "--amend", "--no-edit", "--trailer", "Acked-by: A <a@example.invalid>"],
    );
}

// ---------------------------------------------------------------------------
// What counts as a comment
// ---------------------------------------------------------------------------

/// `core.commentChar` and `core.commentString` past the single-ASCII-character
/// case.
///
/// `commit_family.rs` sets each once, to `;` and to `//`, both ASCII and both
/// valid. The interesting values are the ones at the edges of the parser: a
/// multi-byte character (accepted by 2.55.0 — `core.commentChar` has taken a
/// string since the `commentString` work landed, so "Char" is a name rather
/// than a constraint), both keys set at once, and the two values git rejects.
/// The last two are `strict` because the diagnostic is a two-line
/// `error:`/`fatal:` pair naming which key failed and why.
fn comment_syntax(out: &mut Vec<Case>) {
    /// A message with a `※` comment line and a `#` comment line, so which of
    /// the two disappears identifies which value took effect.
    const MSG_MULTIBYTE: &str = "subj\n\n※ comment\n# hash";

    for key in ["core.commentChar", "core.commentString"] {
        out.push(
            Case::new("commit", &["commit", "--cleanup=strip", "-m", MSG_MULTIBYTE], Shape::Dirty)
                .with_config(&[(key, "※")]),
        );
    }

    // Both keys set. They are one setting under two names, so the later source
    // wins and the `;` line survives while the `//` line goes — a port holding
    // two independent fields would strip both and land on a third id.
    out.push(
        Case::new(
            "commit",
            &["commit", "--cleanup=strip", "-m", "subj\n\n; semi\n// slash\n# hash"],
            Shape::Dirty,
        )
        .with_config(&[("core.commentChar", ";"), ("core.commentString", "//")]),
    );

    // The two rejected values, both refused before the index is touched:
    // `core.commentchar must have at least one character` and
    // `core.commentstring cannot contain newline`, each followed by
    // `fatal: unable to parse ... from command-line config`.
    out.push(
        Case::strict("commit", &["commit", "-m", "subj"], Shape::Dirty)
            .with_config(&[("core.commentChar", "")]),
    );
    out.push(
        Case::strict("commit", &["commit", "-m", "subj"], Shape::Dirty)
            .with_config(&[("core.commentString", "a\nb")]),
    );
}

// ---------------------------------------------------------------------------
// `--date=`
// ---------------------------------------------------------------------------

/// Every `--date=` spelling that names an *absolute* instant.
///
/// Unreached by the whole corpus until now. `commit_family.rs` records `--date`
/// as unusable on the grounds that both dates are pinned and a case that set one
/// would put a clock back into the comparison; that is right about the *relative*
/// spellings and wrong about the absolute ones. `--date=@1500000000 +0000`
/// names one instant on every machine, in every zone, in every second — it
/// overrides the pinned `GIT_AUTHOR_DATE` with another constant, which is
/// additive in exactly the way `env::harden` requires and puts no clock
/// anywhere.
///
/// The equivalence class is the assertion. Five of the six spellings below name
/// the same instant in the same zone, so all five must produce **one** commit
/// id; the sixth carries `+0530` and must produce a different one, because git
/// stores the zone as written rather than normalising it. A port that parsed the
/// spellings independently would have to get all five right to pass, and a port
/// that normalised the zone to UTC would pass the five and fail the sixth.
///
/// The two spellings that consult the wall clock are named in the module header
/// and are absent here — both were run twice against stock and disagreed with
/// themselves.
fn author_date(out: &mut Vec<Case>) {
    let raw_utc = format!("--date=@{EPOCH} +0000");
    let bare_at = format!("--date=@{EPOCH}");
    let raw_tz = format!("--date={EPOCH} +0530");
    for spelling in [
        raw_utc.as_str(),
        bare_at.as_str(),
        "--date=2017-07-14T02:40:00+00:00",
        "--date=Fri, 14 Jul 2017 02:40:00 +0000",
        "--date=Fri Jul 14 02:40:00 2017 +0000",
        // The odd one out: same instant, a zone that is not UTC. Git records
        // `+0530` verbatim, so this is a different object from the five above.
        raw_tz.as_str(),
    ] {
        commit(out, Shape::Dirty, &["commit", "-m", "dated", spelling]);
    }

    // A spelling git cannot parse: `fatal: invalid date format: not a date`,
    // exit 128, nothing written.
    refusal(out, Shape::Dirty, &["commit", "-m", "dated", "--date=not a date"]);

    // `--date` under `--amend`, which is where it is actually reached in
    // practice — and beside `--reset-author`, which replaces the author with the
    // committer and would drop the supplied date if it ran in the wrong order.
    // Both rows come back identical across two stock runs, which is what
    // establishes that `--reset-author` reads the pinned `GIT_AUTHOR_DATE`
    // rather than the clock.
    commit(out, Shape::Linear, &["commit", "--amend", "--no-edit", &raw_utc]);
    commit(out, Shape::Linear, &["commit", "--amend", "--no-edit", "--reset-author", &raw_utc]);
}

// ---------------------------------------------------------------------------
// `commit-tree`: the same bytes with no cleanup at all
// ---------------------------------------------------------------------------

/// The control group for everything above.
///
/// `commit-tree` writes the message it is given, byte for byte, with no cleanup
/// stage and no editor — it does not even append a final newline. So the same
/// payloads that separate the cleanup modes in `commit` must **all** pass
/// through unchanged here, and the pairing is what localises a defect. Both
/// halves of that turned out to be real: the port's `commit-tree` keeps a `NUL`
/// out while its `commit` lets one in, and its `commit-tree` writes a
/// whitespace-only message while its `commit --cleanup=verbatim` refuses one.
/// Neither could be said by a single-verb case.
///
/// `commit_family.rs` owns `commit-tree`'s tree spellings, parent list,
/// `-m`/stdin equivalence and `i18n.commitEncoding`; none of its payloads
/// carries a byte that any of these four turn on.
fn commit_tree_bytes(out: &mut Vec<Case>) {
    fn ct(out: &mut Vec<Case>, args: &[&str], stdin: &'static [u8]) {
        out.push(Case::with_stdin("commit-tree", args, Shape::Linear, stdin));
    }

    // No final newline: the object's last byte is `e`, and the id says so.
    ct(out, &["commit-tree", "HEAD^{tree}"], MSG_NO_NEWLINE);
    // Every `\r` survives, where `commit`'s default cleanup drops them all.
    ct(out, &["commit-tree", "HEAD^{tree}"], MSG_CRLF);
    // Comment line, trailing spaces and trailing blanks, all kept.
    ct(out, &["commit-tree", "HEAD^{tree}"], MSG_MESSY);
    // Whitespace and nothing else, which `commit-tree` has no emptiness test to
    // apply: both sides write `432765fcc2…`. Paired with the `commit
    // --cleanup=verbatim` row above, which the port refuses, this says the
    // port's object writer is right and its emptiness test runs too early.
    ct(out, &["commit-tree", "HEAD^{tree}"], MSG_WS_ONLY);
    // The one payload `commit-tree` refuses, with the same `error:` line
    // `commit` uses and exit 1 rather than 128 — and with no
    // `fatal: failed to write commit object` after it, which `commit` adds.
    // **Both sides refuse here**, while the port's `commit` accepts the same
    // bytes, so the pairing localises the missing check to one of the port's
    // two commit writers.
    out.push(Case {
        compare_stderr: true,
        ..Case::with_stdin("commit-tree", &["commit-tree", "HEAD^{tree}"], Shape::Linear, MSG_NUL)
    });
}
