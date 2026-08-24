//! Differential corpus cases for the three input conventions **every** git verb
//! shares: pathspec magic, NUL-separated (`-z`) I/O, and path encoding.
//!
//! Every case here is compared against stock git for stdout, exit code and
//! post-command repository state.
//!
//! # Why a cross-cutting module
//!
//! In git these three are one implementation reached through thirty doors.
//! `pathspec.c:parse_pathspec()` is called by every builtin that takes a path
//! argument, and each caller passes a *magic mask* saying which of the eleven
//! magic words it tolerates; `dir.c` walks the worktree against the result;
//! `quote.c:quote_c_style()` decides whether a name is printed raw or escaped,
//! and is skipped wholesale in `-z` modes. A port does not inherit that
//! sharing. It re-implements the parse per verb, and the corpus's per-verb
//! modules — which ask each verb about its own flags — score a verb that never
//! calls the parser exactly as well as one that does, because with no magic in
//! the argv there is nothing to get wrong.
//!
//! So the axis here is *the same spelling through many verbs*, not *many
//! spellings through one verb*. `:(glob)README.md` is put through five doors
//! below on purpose: `ls-files` accepts it, `ls-tree` dies
//! `pathspec magic not supported by this command: 'glob'`, `update-index` dies
//! `Unable to process path`, `checkout-index` looks it up as a literal cache
//! entry and exits 1, and `check-attr` answers attributes *for a file named
//! `:(glob)README.md`*. Five different right answers for one argument, and no
//! per-verb module compares them against each other.
//!
//! # Territory
//!
//! The per-verb modules keep their own pathspec cases and none are duplicated
//! here — `add_rm_mv_clean.rs` owns `add`/`rm`/`clean`'s exclusions,
//! `reset_family.rs` owns `reset`/`checkout`'s magic and their
//! `--pathspec-from-file` refusals, `switch_restore.rs` owns `restore`'s,
//! `info_attrs.rs` owns `grep`'s three, `archive_export.rs` owns
//! `archive HEAD src :!src/lib.rs`, and `stdin_plumbing.rs` owns the *payload
//! parsers* of `mktree`, `diff-pairs` and `hash-object`. What is left to this
//! module, and is what it contains:
//!
//! * every magic word crossed with every verb that reaches the parser, so the
//!   verbs that must *refuse* a word are measured beside the ones that accept
//!   it;
//! * the magic words no module reaches at all — `:(attr:…)`, `:(literal)`,
//!   `:(top)` from a subdirectory, the two-word combinations
//!   (`:(literal,icase)`, `:(exclude,glob)`, `:(attr:text,glob)`), and the
//!   malformed spellings (`:(nosuch)`, `:(glob`, `:(attr:)`);
//! * the four global pathspec options, and the four `GIT_*_PATHSPECS`
//!   environment variables that mean the same thing through a different door;
//! * the `-z` axis fed *and* produced, on the shapes whose names need quoting,
//!   so a reader that splits NUL input on `\n` and a writer that quotes inside
//!   `-z` are both caught;
//! * `core.quotePath` and `core.precomposeunicode` in both settings, on the
//!   verbs that print a path rather than on one verb.
//!
//! # macOS-specific behaviour, stated rather than assumed
//!
//! [`Shape::DecomposedPaths`] carries `e` + U+0301 on disk. git converts it to
//! the composed `é` before anything compares it, but only where
//! `PRECOMPOSE_UNICODE` is defined — `config.mak.uname` defines it inside the
//! Darwin block alone — and the port gates the same conversion on
//! `cfg(target_os = "macos")`. So on macOS `ls-files` prints
//! `"\303\251.txt"` and `core.precomposeunicode=false` makes it print
//! `"e\314\201.txt"`; on Linux both sides leave the bytes alone and the config
//! is inert. Every case in [`encoding`] that names that shape is therefore
//! measuring a *conversion* on macOS and a *pass-through* on Linux — the two
//! sides agree either way, which is what makes the case portable, but a
//! divergence found there is a macOS divergence and should be read as one.
//!
//! No fixture carries a path with a byte that is not valid UTF-8:
//! `Shape::AwkwardPaths` writes `with space.txt`, `üñïçødé.txt`,
//! `quote"name.txt` and `nested/deep/path.txt`, all of which are well-formed
//! UTF-8, and `fixture.rs` writes every name through `&str`. So the
//! `quote_c_style()` path that escapes a lone `\200` is unreachable from this
//! corpus and is not claimed to be covered; what *is* covered is the high-byte
//! escaping of legal UTF-8 (`\303\274…`), the `"` escape, and the space that
//! needs no escape but does need the surrounding quotes to be absent.
//!
//! # The payload trap
//!
//! Rust's `\<newline>` string continuation eats the next line's leading
//! whitespace. In this corpus that silently deleted the leading space from
//! every diff context line in a set of patch payloads, turning `apply` success
//! cases into rejection cases that still "passed" because both sides rejected
//! them. Every payload literal below is therefore written **flush-left with
//! real newlines**, never continued; a load-bearing leading space is spelled
//! `\x20`. The bytes are checked by matching `--list-cases`'s
//! `stdin[<len>B/<hash>]` against a file written and hashed by hand.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    magic_reach(out);
    magic_glob(out);
    magic_icase(out);
    magic_literal(out);
    magic_exclude(out);
    magic_attr(out);
    magic_top(out);
    magic_spelling(out);
    magic_no_match(out);
    pathspec_globals(out);
    nul_produced(out);
    nul_consumed(out);
    pathspec_from_file(out);
    encoding(out);
}

/// Shorthand for a single-shape case.
fn one(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], shape: Shape) {
    out.push(Case::new(cmd, args, shape));
}

/// Shorthand for a case fed `stdin` whose stderr is compared too.
fn strict_stdin(
    out: &mut Vec<Case>,
    cmd: &'static str,
    args: &[&str],
    shape: Shape,
    input: &'static [u8],
) {
    out.push(Case { compare_stderr: true, ..Case::with_stdin(cmd, args, shape, input) });
}

/// Shorthand for a case whose stderr is compared too.
fn strict(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], shape: Shape) {
    out.push(Case::strict(cmd, args, shape));
}

// ---------------------------------------------------------------------------
// Which verbs reach the pathspec parser at all
// ---------------------------------------------------------------------------

/// One spelling, every door. `:(glob)README.md` is a *pathspec* to `ls-files`,
/// an *unsupported magic* to `ls-tree` and `check-ignore`, a *filename* to
/// `check-attr`, a *cache lookup key* to `checkout-index`, and a *worktree path
/// to stat* to `update-index`. `pathspec.c:parse_pathspec()` is what separates
/// them: each builtin passes a magic mask, and the literal-path shortcut that
/// `check-attr` and `checkout-index` take instead of calling the parser is not
/// the same thing as an empty mask.
///
/// Measured against stock git 2.55.0, all on `Shape::AwkwardPaths`:
///
/// | argv | stock |
/// |---|---|
/// | `ls-tree -r HEAD -- :(glob)README.md` | `fatal: … pathspec magic not supported by this command: 'glob'`, rc 128 |
/// | `ls-tree -r HEAD -- :(literal)README.md` | the `README.md` row, rc 0 |
/// | `update-index --refresh -- :(glob)README.md` | `error: … does not exist and --remove not passed` + `fatal: Unable to process path …`, rc 128 |
/// | `checkout-index -n -- :(glob)README.md` | `git checkout-index: … is not in the cache`, rc 1 |
/// | `check-attr text -- :(glob)**/*.txt` | `:(glob)**/*.txt: text: unspecified`, rc 0 |
///
/// A port that routes every path argument through one permissive parser passes
/// the `ls-files` half of this table and fails all five of the others — and
/// fails `check-attr`'s *silently*, where the wrong answer is a successful
/// looking line rather than an error.
fn magic_reach(out: &mut Vec<Case>) {
    // `ls-tree` accepts only the two magic words that do not change matching:
    // `literal` and `top`. Everything else is a refusal naming the word, and
    // `exclude` additionally names its `!` mnemonic.
    for magic in [":(glob)", ":(icase)", ":(exclude)", ":(attr:text)"] {
        strict(out, "ls-tree", &["ls-tree", "-r", "HEAD", "--", &format!("{magic}README.md")], Shape::AwkwardPaths);
    }
    one(out, "ls-tree", &["ls-tree", "-r", "HEAD", "--", ":(literal)README.md"], Shape::AwkwardPaths);
    // A literal name that also needs quoting on the way out: the magic prefix is
    // stripped from the *input* and the `"` is escaped in the *output*, and the
    // two are independent decisions made in different files.
    one(out, "ls-tree", &["ls-tree", "-r", "HEAD", "--", ":(literal)quote\"name.txt"], Shape::AwkwardPaths);

    // `update-index` never parses magic: `builtin/update-index.c` stats the
    // argument as a worktree path, so the prefix lands in the diagnostic.
    strict(out, "update-index", &["update-index", "--refresh", "--", ":(glob)README.md"], Shape::AwkwardPaths);
    strict(out, "update-index", &["update-index", "--refresh", "--", ":(literal)README.md"], Shape::AwkwardPaths);

    // `checkout-index` looks the argument up in the index by name, exits 1 (not
    // 128) when it is absent, and says so with a `git checkout-index:` prefix
    // that is not git's usual `fatal:`.
    strict(out, "checkout-index", &["checkout-index", "-n", "--", ":(glob)README.md"], Shape::AwkwardPaths);
    one(out, "checkout-index", &["checkout-index", "-n", "--", "README.md"], Shape::AwkwardPaths);

    // `check-attr` takes pathnames, not pathspecs, and answers *about a file
    // with that name*. rc 0 and a plausible line, so this is the one door where
    // getting it wrong produces no error at all.
    one(out, "check-attr", &["check-attr", "text", "--", ":(glob)**/*.txt"], Shape::AwkwardPaths);
    one(out, "check-attr", &["check-attr", "text", "--", ":!README.md"], Shape::Attributes);

    // `check-ignore` sits next to it and does the opposite: it *does* parse, and
    // refuses the words its mask excludes.
    strict(out, "check-ignore", &["check-ignore", "-n", "-v", "--", ":!README.md"], Shape::AwkwardPaths);
    strict(out, "check-ignore", &["check-ignore", "-n", "-v", "--", ":(glob)**/*.log"], Shape::Attributes);

    // `mv`'s source is resolved before any pathspec would be: `builtin/mv.c`
    // needs a real name to rename, so the magic reaches the `bad source`
    // diagnostic with its prefix still attached.
    strict(out, "mv", &["mv", "-n", "--", ":(glob)**/*.txt", "src"], Shape::AwkwardPaths);
}

// ---------------------------------------------------------------------------
// :(glob)
// ---------------------------------------------------------------------------

/// `:(glob)` swaps fnmatch for `wildmatch()` with `WM_PATHNAME`
/// (`pathspec.c` sets `PATHSPEC_GLOB`, `dir.c:match_pathname` passes the flag
/// through), and the whole observable difference is that `*` stops crossing `/`
/// while `**` starts to. So the pair that matters is not "does `:(glob)` work"
/// but "does the *same* pattern mean two different things with and without it".
///
/// Stock git 2.55.0 on `Shape::AwkwardPaths`:
///
/// | pathspec | matches |
/// |---|---|
/// | `*.txt` | `nested/deep/path.txt`, `quote"name.txt`, `with space.txt`, `üñïçødé.txt` |
/// | `:(glob)*.txt` | the three root files only — `*` no longer crosses `/` |
/// | `:(glob)**/*.txt` | all four again — `**` restores the crossing |
///
/// A port that implements `:(glob)` by stripping the prefix and matching as
/// before agrees on the first and third rows and disagrees on the second, which
/// is the row no per-verb module writes.
fn magic_glob(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    one(out, "ls-files", &["ls-files", "--", ":(glob)*.txt"], aw);
    one(out, "ls-files", &["ls-files", "--", ":(glob)**/*.txt"], aw);

    // The same two rows through the verbs that reach the parser from a different
    // entry point: a revision walk, a tree-to-tree diff, a content search, a
    // worktree walk, and an export.
    one(out, "log", &["log", "--oneline", "--name-only", "--", "*.txt"], aw);
    one(out, "log", &["log", "--oneline", "--name-only", "--", ":(glob)*.txt"], aw);
    one(out, "diff-tree", &["diff-tree", "-r", "--root", "--name-only", "HEAD", "--", ":(glob)*.txt"], aw);
    one(out, "grep", &["grep", "-n", "deep", "--", ":(glob)nested/**"], aw);
    one(out, "archive", &["archive", "--format=tar", "HEAD", ":(glob)**/*.txt"], aw);

    // A dirty tree, so the index side and the worktree side of one walk are both
    // filtered: `staged.txt` is index-only, `untracked.txt` is walk-only.
    one(out, "status", &["status", "--porcelain", "--", ":(glob)*.txt"], Shape::Dirty);

    // Shapes where the glob has something structural to select: a sparse cone's
    // excluded half, and a directory of symlinks.
    one(out, "ls-files", &["ls-files", "--", ":(glob)outside/**"], Shape::Sparse);
}

// ---------------------------------------------------------------------------
// :(icase)
// ---------------------------------------------------------------------------

/// `:(icase)` sets `PATHSPEC_ICASE`, which `dir.c` turns into `WM_CASEFOLD` on
/// the wildmatch *and* `strncasecmp` on the literal prefix comparison. Two
/// halves, and a port that folds only the wildmatch passes every pattern case
/// and fails every exact-name one — so both spellings are written here.
///
/// The names are chosen so the fold is unambiguous. `README.MD` differs in one
/// letter, `WITH SPACE.TXT` in every letter *and* carries a space, and
/// `NESTED/DEEP/PATH.TXT` folds across three path components — the last is what
/// catches an implementation that folds only the final component. The plain
/// `README.MD` with no magic is kept beside them so the baseline (git matches
/// nothing) is pinned too.
fn magic_icase(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    one(out, "ls-files", &["ls-files", "--", "README.MD"], aw);
    one(out, "ls-files", &["ls-files", "--", ":(icase)README.MD"], aw);
    one(out, "ls-files", &["ls-files", "--", ":(icase)NESTED/DEEP/PATH.TXT"], aw);
    one(out, "log", &["log", "--oneline", "--name-only", "--", ":(icase)WITH SPACE.TXT"], aw);
    one(out, "grep", &["grep", "-l", "space", "--", ":(icase)WITH SPACE.TXT"], aw);
    // The *directory* half of the walk, named in the wrong case: a tracked
    // subtree and an untracked one.
    one(out, "ls-files", &["ls-files", "--", ":(icase)OUTSIDE/"], Shape::Sparse);
    one(out, "clean", &["clean", "-ndx", "--", ":(icase)BUILD/"], Shape::Attributes);
}

// ---------------------------------------------------------------------------
// :(literal)
// ---------------------------------------------------------------------------

/// `:(literal)` is the only magic word that *removes* a capability, and it is
/// the one a port is most likely to implement as a no-op prefix strip — which
/// looks right on every pathspec that has no wildcard in it, that is, on every
/// pathspec anyone writes by hand.
///
/// `Shape::AwkwardPaths` has no tracked name containing `*`, so the difference
/// shows from the other side: `*.txt` matches four files and
/// `:(literal)*.txt` must match **none**, because no file is named `*.txt`.
/// Stock git 2.55.0 answers rc 0 with empty stdout for `ls-files`, and
/// `error: pathspec ':(literal)*.txt' did not match any file(s) known to git`
/// at rc 1 under `--error-unmatch` — with the magic prefix reproduced verbatim
/// in the diagnostic, which is itself a thing to get wrong.
fn magic_literal(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    one(out, "ls-files", &["ls-files", "--", ":(literal)README.md"], aw);
    one(out, "ls-files", &["ls-files", "--", ":(literal)*.txt"], aw);
    strict(out, "ls-files", &["ls-files", "--error-unmatch", "--", ":(literal)*.txt"], aw);
    // `literal` and `icase` are not mutually exclusive: the fold applies to a
    // name comparison rather than to a pattern.
    one(out, "ls-files", &["ls-files", "--", ":(literal,icase)README.MD"], aw);
    // A name that is glob-significant *and* real, so the two modes disagree in
    // the direction where literal matches more, not less.
    one(out, "ls-files", &["ls-files", "--", ":(literal)quote\"name.txt"], aw);
}

// ---------------------------------------------------------------------------
// :(exclude) / :!
// ---------------------------------------------------------------------------

/// A negative pathspec is not a filter applied after matching — `dir.c` keeps
/// the negatives in a separate list and consults them only once a positive has
/// matched, with the rule that **a list holding only negatives matches
/// everything else**. Two consequences a port gets wrong independently:
///
/// * `archive --format=tar HEAD ':!src/lib.rs'` with no positive at all is
///   stock git rc 0, archiving every path but that one — not
///   `fatal: pathspec 'src' did not match any files`;
/// * `ls-files --error-unmatch -- ':!README.md'` is rc 0, because the "must
///   match something" requirement applies to positives only.
///
/// Both spellings are exercised — `:(exclude)` and its `:!` mnemonic — because
/// they are two entries in `pathspec.c`'s magic table and a port can implement
/// one and not the other. `:(exclude,glob)` is here because a negative carries
/// its own match flags, and a port that reuses the positive list's flags for the
/// negatives answers it differently.
fn magic_exclude(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    // Negative-only.
    one(out, "ls-files", &["ls-files", "--", ":(exclude)src/lib.rs"], aw);
    one(out, "ls-files", &["ls-files", "--", ":(exclude,glob)**/*.txt"], aw);
    one(out, "ls-files", &["ls-files", "--error-unmatch", "--", ":!README.md"], aw);
    one(out, "archive", &["archive", "--format=tar", "HEAD", ":!src/lib.rs"], aw);

    // Positive plus negative, through the other doors.
    one(out, "ls-files", &["ls-files", "--", ".", ":!nested/deep/path.txt"], aw);
    one(out, "log", &["log", "--oneline", "--name-only", "--", ".", ":!nested/"], aw);
    one(out, "grep", &["grep", "-l", ".", "--", ".", ":!src/"], aw);
    one(out, "status", &["status", "--porcelain", "--", ".", ":!untracked.txt"], Shape::Dirty);
}

// ---------------------------------------------------------------------------
// A pathspec that matches nothing
// ---------------------------------------------------------------------------

/// The same unmatched name through nine verbs produces five different outcomes,
/// and every one of them is a contract a script depends on. Stock git 2.55.0 on
/// `Shape::AwkwardPaths`, argument `nosuchfile`:
///
/// | verb | stderr | rc |
/// |---|---|---|
/// | `add -n` / `rm -n` / `archive` | `fatal: pathspec 'nosuchfile' did not match any files` | 128 |
/// | `ls-files --error-unmatch` | `error: pathspec … did not match any file(s) known to git` + `Did you forget to 'git add'?` | 1 |
/// | `grep -l .` | — | 1 |
/// | `checkout-index -n` | `git checkout-index: nosuchfile is not in the cache` | 1 |
/// | `log` / `status` / `ls-files` | — | 0 |
///
/// Three distinct exit codes and three distinct message shapes, all with empty
/// stdout. A port that picks one convention and applies it everywhere breaks
/// `git ls-files --error-unmatch "$f"` and `git log -- "$f"` in opposite
/// directions.
fn magic_no_match(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    strict(out, "add", &["add", "-n", "--", "nosuchfile"], aw);
    strict(out, "rm", &["rm", "-n", "--", "nosuchfile"], aw);
    strict(out, "archive", &["archive", "--format=tar", "HEAD", "nosuchfile"], aw);
    strict(out, "ls-files", &["ls-files", "--error-unmatch", "--", "nosuchfile"], aw);
    strict(out, "checkout-index", &["checkout-index", "-n", "--", "nosuchfile"], aw);
    one(out, "grep", &["grep", "-l", ".", "--", "nosuchfile"], aw);
    one(out, "log", &["log", "--oneline", "--", "nosuchfile"], aw);
    one(out, "status", &["status", "--porcelain", "--", "nosuchfile"], aw);
    one(out, "ls-files", &["ls-files", "--", "nosuchfile"], aw);
}

// ---------------------------------------------------------------------------
// :(attr:<name>)
// ---------------------------------------------------------------------------

/// The one magic word that makes the pathspec parser depend on a *second*
/// subsystem: `pathspec.c` builds an attribute query and `dir.c` runs it through
/// `attr.c` for every candidate path. No corpus module reaches it —
/// `info_attrs.rs` asks `check-attr` about attributes and `add_rm_mv_clean.rs`
/// asks `add` about pathspecs, and `:(attr:…)` is the case that needs both.
///
/// `Shape::Attributes` is the only shape with rules. Stock git 2.55.0,
/// `ls-files`:
///
/// | pathspec | matches |
/// |---|---|
/// | `:(attr:text)` | `src/lib.rs`, `src/tabs.rs`, `sub/nested.txt` — *set*, not `auto` |
/// | `:(attr:text=auto)` | the nine paths the root `* text=auto` line still covers |
/// | `:(attr:merge=union)` | `sub/nested.txt` alone |
/// | `:(attr:linguist-generated)` | `vendor/generated.js` alone |
/// | `:(attr:!text)` | nothing — no path leaves `text` unspecified |
/// | `:(attr:)` | `fatal: attr spec must not be empty`, rc 128 |
///
/// The distinction between `attr:text` (set) and `attr:text=auto` (a value) is
/// the one a port collapses first, and it decides five of these rows.
fn magic_attr(out: &mut Vec<Case>) {
    let at = Shape::Attributes;
    for spec in [
        ":(attr:text)",
        ":(attr:text=auto)",
        ":(attr:merge=union)",
        ":(attr:linguist-generated)",
        ":(attr:!text)",
    ] {
        one(out, "ls-files", &["ls-files", "--", spec], at);
    }
    strict(out, "ls-files", &["ls-files", "--", ":(attr:)"], at);
    // The attribute query crossed with the pattern flags: `attr` narrows the set
    // `glob` produced.
    one(out, "ls-files", &["ls-files", "--", ":(attr:text,glob)**/*.rs"], at);
    // The same query through the other doors: a content search, a revision walk,
    // and a worktree walk — the last over *untracked* files, whose attributes
    // are read from the same rules but whose paths never touch the index.
    one(out, "grep", &["grep", "-l", "x", "--", ":(attr:text)"], at);
    one(out, "log", &["log", "--oneline", "--name-only", "-3", "--", ":(attr:text)"], at);
    one(out, "clean", &["clean", "-ndx", "--", ":(attr:text)"], at);
}

// ---------------------------------------------------------------------------
// :(top) / :/ — and what a subdirectory does to the printed name
// ---------------------------------------------------------------------------

/// `:(top)` (short spelling `:/`) anchors the pathspec at the working tree root
/// instead of at the current directory. Every case here runs `.in_dir(…)`,
/// because from the top level the word is a no-op and proves nothing.
///
/// The second half of the group is what a subdirectory does to the *output*.
/// Stock git 2.55.0, run from `src/` on `Shape::AwkwardPaths`:
///
/// ```text
/// $ git ls-files -- :(top)
/// ../README.md
/// ../nested/deep/path.txt
/// "../quote\"name.txt"
/// lib.rs
/// ../with space.txt
/// "../\303\274\303\261\303\257\303\247\303\270d\303\251.txt"
/// ```
///
/// Three separate facts in that listing: the names are made relative to the cwd
/// (`../`), the sort is on the *repository* name and not on the printed one
/// (`lib.rs` sorts between `quote"…` and `with space…`), and `quote.c` quotes
/// the whole relative name so the `../` lands *inside* the quotes. A port that
/// prefixes `../` after quoting produces `../"quote\"name.txt"` and fails only
/// this case. `--full-name` turns the first fact off and, with it, the third.
///
/// From two levels down the prefix is `../../`, which is the case that catches a
/// hard-coded single `..`. `diff-tree`'s names stay repository-relative whatever
/// the cwd is, which is the counterexample that makes the rows above a decision
/// rather than a universal.
fn magic_top(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    out.push(Case::new("ls-files", &["ls-files", "--", ":(top)"], aw).in_dir("src"));
    out.push(Case::new("ls-files", &["ls-files", "--", ":/"], aw).in_dir("src"));
    out.push(Case::new("ls-files", &["ls-files", "--full-name", "--", ":(top)"], aw).in_dir("src"));
    out.push(Case::new("ls-files", &["ls-files", "--", ":(top)nested/deep/path.txt"], aw).in_dir("src"));
    // No `:(top)`: the same walk confined to the cwd, so the group shows what
    // the word changed rather than only what it produced.
    out.push(Case::new("ls-files", &["ls-files", "--", ":(icase)LIB.RS"], aw).in_dir("src"));
    // Two levels down: the relative prefix has to be `../../`.
    out.push(Case::new("ls-files", &["ls-files", "--", ":(top)src"], aw).in_dir("nested/deep"));
    out.push(Case::new("grep", &["grep", "-l", ".", "--", ":(top)"], aw).in_dir("src"));
    out.push(Case::new("ls-tree", &["ls-tree", "-r", "HEAD", "--", ":(top)nested"], aw).in_dir("src"));
    out.push(
        Case::new("diff-tree", &["diff-tree", "-r", "--root", "--name-only", "HEAD", "--", ":(top)"], aw)
            .in_dir("src"),
    );
    // `check-ignore` parses the magic but prints `pathspec.items[i].original` —
    // the argument as typed, prefix and all — beside the rule that matched the
    // *resolved* path.
    out.push(
        Case::new("check-ignore", &["check-ignore", "-n", "-v", "--", ":(top)../build/output.o"], Shape::Attributes)
            .in_dir("sub"),
    );
}

// ---------------------------------------------------------------------------
// How the pathspec is spelled, before any matching happens
// ---------------------------------------------------------------------------

/// `pathspec.c:prefix_pathspec()` runs before `dir.c` sees anything, and makes
/// four decisions no matching code can undo: it splits the magic from the path,
/// it strips a trailing `/`, it normalizes the magic-only forms, and it decides
/// where option parsing stops.
///
/// Stock git 2.55.0:
///
/// | argv | result |
/// |---|---|
/// | `ls-files -- ':(nosuch)README.md'` | `fatal: Invalid pathspec magic 'nosuch' in ':(nosuch)README.md'`, rc 128 |
/// | `ls-files -- ':(glob'` | `fatal: Missing ')' at the end of pathspec magic in ':(glob'`, rc 128 |
/// | `ls-files -- ':()README.md'` | `README.md` — empty magic is legal and inert |
/// | `ls-files -- 'src/'` | `src/lib.rs` — the trailing slash is stripped |
/// | `add -f -- 'build/'` | index entry named `build/output.o`, slash gone |
/// | `add -n -f -- 'build/'` with no `build/` | `fatal: pathspec 'build/' did not match any files` — the slash survives into the *message* |
/// | `ls-files -- '-README.md'` | rc 0, empty — after `--`, a leading `-` is a path |
/// | `ls-files '-README.md'` | ``error: unknown switch `R'`` plus a usage block |
///
/// The last pair is the one that matters to a caller: everything after `--` is a
/// path even when it starts with a dash, and a port that keeps parsing options
/// past the separator refuses or deletes the wrong file. The usage-block case is
/// deliberately not `strict` — a `parse_options()` dump tracks git's own option
/// table and the harness treats it as prose rather than as contract.
fn magic_spelling(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    strict(out, "ls-files", &["ls-files", "--", ":(nosuch)README.md"], aw);
    strict(out, "ls-files", &["ls-files", "--", ":(glob"], aw);
    one(out, "ls-files", &["ls-files", "--", ":()README.md"], aw);
    one(out, "ls-files", &["ls-files", "--", "src/"], aw);

    // The trailing slash on the verb that writes the name into the index. The
    // dry run pins what is reported; the real one pins what is *stored*, which
    // is where a port that keeps the slash shows up — in `ls-files` and in
    // `probe_index_meta`, not in stdout.
    let at = Shape::Attributes;
    one(out, "add", &["add", "-n", "-f", "--", "build/"], at);
    one(out, "add", &["add", "-f", "--", "build/"], at);
    one(out, "add", &["add", "-f", "--", "sub/deep-ignored/"], at);
    // The slash on a pathspec that matches nothing: it survives into the
    // diagnostic verbatim, so the message is not built from the stripped form.
    strict(out, "add", &["add", "-n", "-f", "--", "build/"], Shape::Sparse);

    // A leading `-`, on both sides of the separator.
    one(out, "ls-files", &["ls-files", "--", "-README.md"], aw);
    one(out, "ls-files", &["ls-files", "-README.md"], aw);
    // `--` where the verb also takes revisions: once as the only separator, and
    // once with a second `--` that is itself a pathspec.
    one(out, "log", &["log", "--oneline", "--", "--", "README.md"], aw);
}

// ---------------------------------------------------------------------------
// The four global settings, through both doors
// ---------------------------------------------------------------------------

/// `--literal-pathspecs`, `--glob-pathspecs`, `--noglob-pathspecs` and
/// `--icase-pathspecs` set a default magic that applies to *every* pathspec the
/// process parses, and each has an environment twin —
/// `GIT_LITERAL_PATHSPECS`, `GIT_GLOB_PATHSPECS`, `GIT_NOGLOB_PATHSPECS`,
/// `GIT_ICASE_PATHSPECS` — read by `pathspec.c`'s environment pass. The corpus
/// crosses the three non-`icase` options with `ls-files` once
/// (`corpus.rs::config_and_globals`); nothing anywhere reads the environment
/// twins, and nothing checks the three rules that make them more than aliases.
///
/// Stock git 2.55.0 on `Shape::AwkwardPaths`, one pathspec `*.txt`:
///
/// | setting | matches |
/// |---|---|
/// | none | all four `.txt` paths |
/// | `--glob-pathspecs` / `GIT_GLOB_PATHSPECS=1` | the three root ones — `*` stops at `/` |
/// | `--noglob-pathspecs` / `GIT_NOGLOB_PATHSPECS=1` | nothing — the `*` is a literal character |
/// | `--literal-pathspecs` / `GIT_LITERAL_PATHSPECS=1` | nothing |
/// | `--icase-pathspecs` / `GIT_ICASE_PATHSPECS=1` | all four |
/// | `GIT_LITERAL_PATHSPECS=0` | all four — the value is read as a boolean, not tested for presence |
///
/// The three rules:
///
/// * **`0` means off.** A port that tests `getenv(…) != NULL` turns
///   `GIT_LITERAL_PATHSPECS=0` into literal matching and drops every glob its
///   caller wrote.
/// * **Explicit magic outranks the global.** `GIT_NOGLOB_PATHSPECS=1` with an
///   explicit `:(glob)*.txt` still globs; `--literal-pathspecs` is the one
///   exception and swallows the `:(glob)` prefix into the filename.
/// * **`literal` excludes the others.** `--glob-pathspecs --literal-pathspecs`
///   is `fatal: global 'literal' pathspec setting is incompatible with all
///   other global pathspec settings`, rc 128.
fn pathspec_globals(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    for opt in ["--literal-pathspecs", "--glob-pathspecs", "--noglob-pathspecs", "--icase-pathspecs"] {
        out.push(Case::new("ls-files", &["ls-files", "--", "*.txt"], aw).with_globals(&[&[opt]]));
    }
    for var in [
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
    ] {
        out.push(Case::new("ls-files", &["ls-files", "--", "*.txt"], aw).with_env(&[(var, "1")]));
    }
    // `0` is false, not "present": the case that catches a presence test.
    out.push(Case::new("ls-files", &["ls-files", "--", "*.txt"], aw).with_env(&[("GIT_LITERAL_PATHSPECS", "0")]));

    // Explicit magic against the global default, in both directions.
    out.push(
        Case::new("ls-files", &["ls-files", "--", ":(glob)*.txt"], aw)
            .with_globals(&[&["--literal-pathspecs"]]),
    );
    out.push(
        Case::new("ls-files", &["ls-files", "--", ":(glob)*.txt"], aw)
            .with_env(&[("GIT_NOGLOB_PATHSPECS", "1")]),
    );

    // The exclusion rule.
    out.push(
        Case::strict("ls-files", &["ls-files", "--", "*.txt"], aw)
            .with_globals(&[&["--glob-pathspecs"], &["--literal-pathspecs"]]),
    );

    // The same global through other verbs, because it is a *process* setting and
    // a port that implements it inside one verb's option parser passes only that
    // verb.
    out.push(
        Case::new("log", &["log", "--oneline", "--name-only", "--", "*.txt"], aw)
            .with_globals(&[&["--literal-pathspecs"]]),
    );
    out.push(
        Case::new("grep", &["grep", "-l", ".", "--", "SRC/LIB.RS"], aw)
            .with_globals(&[&["--icase-pathspecs"]]),
    );
    out.push(
        Case::new("diff-tree", &["diff-tree", "-r", "--root", "--name-only", "HEAD", "--", "*.txt"], aw)
            .with_globals(&[&["--glob-pathspecs"]]),
    );
}

// ---------------------------------------------------------------------------
// -z on the way out
// ---------------------------------------------------------------------------

/// `-z` is not a formatting preference. It changes two things at once: the
/// record terminator becomes NUL, and `quote.c`'s C-style quoting is *skipped
/// entirely* — `write_name_quoted()` takes the `line_terminator == '\0'` branch
/// and writes the path's bytes raw. So a name that prints as
/// `"\303\274\303\261\303\257\303\247\303\270d\303\251.txt"` in the default
/// mode is fourteen bytes of UTF-8 under `-z`, and a name with a `"` in it
/// loses both the escape and the surrounding quotes.
///
/// Stock git 2.55.0, `Shape::AwkwardPaths` (`^@` written for NUL):
///
/// ```text
/// $ git ls-files
/// README.md
/// nested/deep/path.txt
/// "quote\"name.txt"
/// src/lib.rs
/// with space.txt
/// "\303\274\303\261\303\257\303\247\303\270d\303\251.txt"
/// $ git ls-files -z
/// README.md^@nested/deep/path.txt^@quote"name.txt^@src/lib.rs^@with space.txt^@üñïçødé.txt^@
/// ```
///
/// Note the trailing NUL: `-z` *terminates* records, it does not separate them,
/// so a separator implementation is one byte short and every case here catches
/// that. The corpus's existing `-z` cases sit inside the per-verb modules and
/// each pins one flag combination; what this group adds is the plain form of
/// each verb's `-z` on the shapes whose names need quoting, so the
/// quoting-suppression half is measured rather than assumed. Each `-z` case that
/// has a non-`-z` twin is written with it, so the pair is a comparison rather
/// than one absolute answer.
fn nul_produced(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    one(out, "ls-files", &["ls-files", "-z"], aw);
    one(out, "ls-tree", &["ls-tree", "-z", "HEAD"], aw);
    one(out, "ls-tree", &["ls-tree", "HEAD"], aw);
    one(out, "diff", &["diff", "--name-status", "-z", "HEAD~1", "HEAD"], aw);
    one(out, "diff", &["diff", "--name-status", "HEAD~1", "HEAD"], aw);
    one(out, "log", &["log", "--oneline", "--name-only", "-z"], aw);
    one(out, "grep", &["grep", "-z", "-n", "deep"], aw);
    one(out, "status", &["status", "--porcelain", "-z"], Shape::Dirty);
    // The decomposed shape: macOS composes the name before printing it, so the
    // raw bytes `-z` emits are the *composed* ones. On Linux both sides emit the
    // decomposed bytes and still agree — see the module doc.
    one(out, "ls-files", &["ls-files", "-z"], Shape::DecomposedPaths);
    one(out, "ls-files", &["ls-files"], Shape::DecomposedPaths);
    one(out, "status", &["status", "--porcelain", "-z"], Shape::DecomposedPaths);
}

// ---------------------------------------------------------------------------
// -z on the way in
// ---------------------------------------------------------------------------
//
// Payload literals. Written flush-left with real newlines: see the module doc's
// note on `\<newline>` continuation. Byte counts are stated so a reader can
// check them against `--list-cases`'s `stdin[<len>B/<hash>]` without running
// anything.

/// Three words, NUL-terminated. 20 bytes.
const ABC_NUL: &[u8] = b"alpha\0bravo\0charlie\0";

/// The same three words, LF-terminated. 20 bytes — the same length, which is
/// what makes the pair a clean separator test.
const ABC_LF: &[u8] = b"alpha\nbravo\ncharlie\n";

/// Two tracked paths, one carrying a space, NUL-terminated. 26 bytes.
const SPACED_NUL: &[u8] = b"with space.txt\0src/lib.rs\0";

/// The same two paths, LF-terminated. 26 bytes.
const SPACED_LF: &[u8] = b"with space.txt\nsrc/lib.rs\n";

/// Three tracked paths of `Shape::AwkwardPaths`, NUL-terminated, one of them
/// carrying a raw `"`. 51 bytes.
const AWKWARD_NUL: &[u8] = b"with space.txt\0quote\"name.txt\0nested/deep/path.txt\0";

/// The same three, LF-terminated. 51 bytes.
const AWKWARD_LF: &[u8] = b"with space.txt\nquote\"name.txt\nnested/deep/path.txt\n";

/// One path in `quote.c`'s C-style quoting, LF-terminated — the form the
/// non-`-z` readers are required to *unquote*. 18 bytes.
const QUOTED_LF: &[u8] = b"\"quote\\\"name.txt\"\n";

/// The same path with no quoting at all, NUL-terminated — the form the `-z`
/// readers are required to take verbatim. 15 bytes.
const RAW_QUOTE_NUL: &[u8] = b"quote\"name.txt\0";

/// Two tracked paths of the base fixture, NUL-terminated. 21 bytes.
const TWO_NUL: &[u8] = b"README.md\0src/lib.rs\0";

/// The same two, LF-terminated. 21 bytes.
const TWO_LF: &[u8] = b"README.md\nsrc/lib.rs\n";

/// Every reader here takes a *record* terminator, and the failure this group
/// exists to catch is a reader that ignores which one it was told to use. Not a
/// hypothetical: three commands shipped with it, and all three fail the same
/// way — a NUL-separated payload handed to a reader that splits on `\n` is one
/// record whose first NUL ends it, so **the first path is processed and the rest
/// are silently dropped, at rc 0**.
///
/// Stock git 2.55.0, measured:
///
/// | command | payload | stdout |
/// |---|---|---|
/// | `column --mode=plain` | `alpha\0bravo\0charlie\0` | `alpha` |
/// | `column --mode=plain` | `alpha\nbravo\ncharlie\n` | `alpha`, `bravo`, `charlie` |
/// | `hash-object --stdin-paths` | `README.md\nsrc/lib.rs\n` | two oids |
/// | `hash-object --stdin-paths` | the three awkward paths, NUL-terminated | one oid |
/// | `update-index --verbose --stdin` | `with space.txt\0src/lib.rs\0` | `add 'with space.txt'` — one path |
/// | `update-index --verbose -z --stdin` | the same bytes | two `add` lines |
///
/// The mirror direction is louder and therefore easier: an LF payload handed to
/// a `-z` reader is one record *containing* the newlines, and git reports the
/// whole blob as one impossible path —
/// `fatal: Unable to process path with space.txt⏎src/lib.rs⏎` for
/// `update-index -z --stdin`, and `git checkout-index: with space.txt⏎src/lib.rs⏎
/// is not in the cache` at rc 1 for `checkout-index -z --stdin`.
///
/// The second axis is quoting, which is tied to the separator by definition: a
/// non-`-z` reader passes each record through `unquote_c_style()` and a `-z`
/// reader does not. So `"quote\"name.txt"` on stdin means the file
/// `quote"name.txt` to `update-index --stdin`, and means a name containing
/// literal quote characters to `update-index -z --stdin`.
///
/// `index_plumbing.rs` owns the *matched* pairs for `update-index` and
/// `checkout-index`; every case below is either a mismatch, a quoting decision,
/// or a command with no `-z` at all.
fn nul_consumed(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    let li = Shape::Linear;

    // `column` reads whatever it is given and lays it out. It has no `-z`, which
    // is the point: there is exactly one right answer for a NUL payload, and it
    // is "one record".
    out.push(Case::with_stdin("column", &["column", "--mode=plain"], li, ABC_NUL));
    out.push(Case::with_stdin("column", &["column", "--mode=plain"], li, ABC_LF));
    out.push(Case::with_stdin("column", &["column", "--mode=column", "--width=40"], li, ABC_LF));
    out.push(Case::with_stdin("column", &["column", "--mode=plain"], li, AWKWARD_NUL));

    // `hash-object --stdin-paths` likewise has no `-z`. It opens each record as
    // a file, so a truncated record is a wrong *object id*, not an error.
    out.push(Case::with_stdin("hash-object", &["hash-object", "--stdin-paths"], li, TWO_LF));
    out.push(Case::with_stdin("hash-object", &["hash-object", "--stdin-paths"], aw, AWKWARD_NUL));
    out.push(Case::with_stdin("hash-object", &["hash-object", "--stdin-paths"], aw, AWKWARD_LF));

    // The two readers that *do* have `-z`, mismatched both ways.
    out.push(Case::with_stdin("update-index", &["update-index", "--verbose", "--stdin"], aw, SPACED_NUL));
    out.push(Case::with_stdin("update-index", &["update-index", "--verbose", "-z", "--stdin"], aw, SPACED_NUL));
    strict_stdin(out, "update-index", &["update-index", "-z", "--stdin"], aw, SPACED_LF);
    out.push(Case::with_stdin("checkout-index", &["checkout-index", "--stdin", "-n", "-f"], aw, SPACED_NUL));
    strict_stdin(out, "checkout-index", &["checkout-index", "-z", "--stdin", "-f"], aw, SPACED_LF);

    // Quoting, which travels with the separator.
    out.push(Case::with_stdin("update-index", &["update-index", "--verbose", "--stdin"], aw, QUOTED_LF));
    out.push(Case::with_stdin("update-index", &["update-index", "--verbose", "-z", "--stdin"], aw, RAW_QUOTE_NUL));
    out.push(Case::with_stdin("checkout-index", &["checkout-index", "--stdin", "-n", "-f"], aw, QUOTED_LF));

    // `check-attr` and `check-ignore` read paths on stdin too, and quote their
    // *output* by the same rule. The mismatched combinations are the ones no
    // module has: fed LF, `-z` reports one path whose name contains newlines;
    // fed NUL, the non-`-z` reader answers about the first path only.
    let at = Shape::Attributes;
    out.push(Case::with_stdin("check-attr", &["check-attr", "-z", "--stdin", "text"], at, AWKWARD_NUL));
    out.push(Case::with_stdin("check-attr", &["check-attr", "-z", "--stdin", "text"], at, AWKWARD_LF));
    out.push(Case::with_stdin("check-attr", &["check-attr", "--stdin", "text"], at, AWKWARD_NUL));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "-z", "--stdin", "-n", "-v"], at, AWKWARD_NUL));
    out.push(Case::with_stdin("check-ignore", &["check-ignore", "--stdin", "-n", "-v"], at, AWKWARD_NUL));
}

// ---------------------------------------------------------------------------
// --pathspec-from-file / --pathspec-file-nul
// ---------------------------------------------------------------------------

/// One pathspec carrying magic, LF-terminated. 16 bytes.
const MAGIC_LF: &[u8] = b":(glob)**/*.txt\n";

/// The same pathspec NUL-terminated. 16 bytes.
const MAGIC_NUL: &[u8] = b":(glob)**/*.txt\0";

/// `--pathspec-from-file` is the third separator decision in git, and a separate
/// implementation from the `-z` readers above: `pathspec.c`'s file reader splits
/// on `\n` or on `\0` depending on `--pathspec-file-nul` and — in the `\n` case
/// only — passes each record through `unquote_c_style()`. Exactly seven verbs
/// have the pair (`add`, `rm`, `reset`, `restore`, `checkout`, `commit`,
/// `stash push`) and no other verb has it at all.
///
/// The corpus already feeds each of those verbs a matched payload once. Missing,
/// and what this group is, are the three things that are properties of the
/// *reader* rather than of the verb:
///
/// * **The mismatched pairs.** Stock git 2.55.0, `rm -n --pathspec-from-file=-`
///   on `Shape::AwkwardPaths`:
///
///   | flag | payload | result |
///   |---|---|---|
///   | none | `…\0…\0…\0` | `rm 'with space.txt'` — one path, rc 0 |
///   | `--pathspec-file-nul` | `…\0…\0…\0` | three `rm` lines, rc 0 |
///   | none | `…\n…\n…\n` | three `rm` lines, rc 0 |
///   | `--pathspec-file-nul` | `…\n…\n…\n` | `fatal: pathspec 'with space.txt⏎quote"name.txt⏎nested/deep/path.txt⏎' did not match any files`, rc 128 |
///
///   The first row is the dangerous one: a script that pipes `ls-files -z` into
///   `rm --pathspec-from-file=-` removes the first file and reports success.
///
/// * **The unquoting.** `"quote\"name.txt"` in the file means the path
///   `quote"name.txt` under `\n`; the same name unquoted with a NUL terminator
///   means it under `--pathspec-file-nul`.
///
/// * **Magic inside the file.** A record is a full pathspec, not a literal path:
///   `:(glob)**/*.txt` selects four paths through `rm`. Under
///   `--pathspec-file-nul` the same bytes with their trailing `\n` still
///   attached are a pathspec that matches nothing, and the `fatal:` reproduces
///   the newline — which is how a port that trims whitespace "helpfully" is
///   caught. `restore` shows the same record landing in the *index*: rc 0 with
///   `\n`, and `error: pathspec ':(glob)**/*.txt⏎' did not match any file(s)
///   known to git` at rc 1 with the flag.
fn pathspec_from_file(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;

    // The four-row table, on `rm -n` because it names what it would remove.
    out.push(Case::with_stdin("rm", &["rm", "-n", "--pathspec-from-file=-"], aw, AWKWARD_NUL));
    out.push(Case::with_stdin(
        "rm",
        &["rm", "-n", "--pathspec-from-file=-", "--pathspec-file-nul"],
        aw,
        AWKWARD_NUL,
    ));
    out.push(Case::with_stdin("rm", &["rm", "-n", "--pathspec-from-file=-"], aw, AWKWARD_LF));
    strict_stdin(out, "rm", &["rm", "-n", "--pathspec-from-file=-", "--pathspec-file-nul"], aw, AWKWARD_LF);

    // The unquoting axis.
    out.push(Case::with_stdin("rm", &["rm", "-n", "--pathspec-from-file=-"], aw, QUOTED_LF));
    out.push(Case::with_stdin(
        "rm",
        &["rm", "-n", "--pathspec-from-file=-", "--pathspec-file-nul"],
        aw,
        RAW_QUOTE_NUL,
    ));

    // Magic inside the file, matched and mismatched.
    out.push(Case::with_stdin("rm", &["rm", "-n", "--pathspec-from-file=-"], aw, MAGIC_LF));
    out.push(Case::with_stdin(
        "rm",
        &["rm", "-n", "--pathspec-from-file=-", "--pathspec-file-nul"],
        aw,
        MAGIC_NUL,
    ));

    // The same reader through the other verbs that have it, with payloads whose
    // effect lands in the *state* probe rather than in stdout: which paths the
    // index kept, and what the stash took.
    out.push(Case::with_stdin(
        "restore",
        &["restore", "--source=HEAD~1", "--staged", "--pathspec-from-file=-"],
        aw,
        MAGIC_LF,
    ));
    strict_stdin(
        out,
        "restore",
        &["restore", "--source=HEAD~1", "--staged", "--pathspec-from-file=-", "--pathspec-file-nul"],
        aw,
        MAGIC_LF,
    );
    out.push(Case::with_stdin("stash", &["stash", "push", "--pathspec-from-file=-"], Shape::Dirty, TWO_NUL));
    out.push(Case::with_stdin(
        "stash",
        &["stash", "push", "--pathspec-from-file=-", "--pathspec-file-nul"],
        Shape::Dirty,
        TWO_NUL,
    ));
}

// ---------------------------------------------------------------------------
// Encoding: core.quotePath and core.precomposeunicode
// ---------------------------------------------------------------------------

/// Two settings that change the bytes of a printed path without changing which
/// path was selected. Both are read once per process and applied by every verb
/// that prints a name, so a port that implements them per verb reports one
/// repository two ways.
///
/// **`core.quotePath`** gates `quote.c:quote_c_style()`'s treatment of bytes
/// above 0x7f *only*. Stock git 2.55.0, `ls-tree -r HEAD` on
/// `Shape::AwkwardPaths`:
///
/// | setting | `üñïçødé.txt` | `quote"name.txt` |
/// |---|---|---|
/// | `true` (default) | `"\303\274\303\261\303\257\303\247\303\270d\303\251.txt"` | `"quote\"name.txt"` |
/// | `false` | `üñïçødé.txt` | `"quote\"name.txt"` |
///
/// The second column is the half a port gets wrong: `quotePath=false` does not
/// turn quoting *off*, it turns the high-byte escape off. A `"` is still escaped
/// and the name is still wrapped, because the wrapping is what makes the escape
/// parseable. In a `-z` mode the setting is inert in both directions, because
/// there is no quoting left to configure — which is why both values are written
/// against `ls-files -z` too.
///
/// **`core.precomposeunicode`** is macOS-only in both implementations (see the
/// module doc). With it off, git stops composing the names `readdir()` hands it,
/// and `Shape::DecomposedPaths` becomes a repository with *two* files where it
/// had one — the index's composed `é.txt` and the directory's decomposed
/// `e`+U+0301. Stock git 2.55.0 on macOS:
///
/// ```text
/// $ git -c core.precomposeunicode=true  status --porcelain
///  M "\303\251.txt"
/// ?? "\303\251-new.txt"
/// $ git -c core.precomposeunicode=false status --porcelain
///  M "\303\251.txt"
/// ?? "e\314\201-new.txt"
/// ?? "e\314\201.txt"
/// ```
///
/// The tracked path is still reported modified — the index entry is composed and
/// is compared against an `lstat` of the name it holds — while the same file
/// *also* appears untracked under its on-disk spelling, and `clean -nd` would
/// then remove it. On Linux both settings are inert and the two blocks coincide.
/// `add_rm_mv_clean.rs` owns the `add` side of this shape; these are the
/// read-only verbs and the pathspec side, which no module asks.
fn encoding(out: &mut Vec<Case>) {
    let aw = Shape::AwkwardPaths;
    for value in ["true", "false"] {
        out.push(Case::new("ls-tree", &["ls-tree", "-r", "HEAD"], aw).with_config(&[("core.quotePath", value)]));
        // Inert by definition: both values must produce the same bytes as each
        // other and as the unset default.
        out.push(Case::new("ls-files", &["ls-files", "-z"], aw).with_config(&[("core.quotePath", value)]));
    }
    // The stdin-driven pair prints paths it was *given* rather than paths it
    // found, and quotes them by the same rule.
    out.push(
        Case::with_stdin("check-attr", &["check-attr", "--stdin", "text"], Shape::Attributes, AWKWARD_LF)
            .with_config(&[("core.quotePath", "false")]),
    );
    out.push(
        Case::with_stdin("check-ignore", &["check-ignore", "--stdin", "-n", "-v"], Shape::Attributes, AWKWARD_LF)
            .with_config(&[("core.quotePath", "false")]),
    );

    let nfd = Shape::DecomposedPaths;
    for value in ["true", "false"] {
        out.push(Case::new("status", &["status", "--porcelain"], nfd).with_config(&[("core.precomposeunicode", value)]));
        out.push(Case::new("clean", &["clean", "-nd"], nfd).with_config(&[("core.precomposeunicode", value)]));
        out.push(Case::new("ls-files", &["ls-files", "-o"], nfd).with_config(&[("core.precomposeunicode", value)]));
        out.push(Case::new("ls-tree", &["ls-tree", "-r", "HEAD"], nfd).with_config(&[("core.precomposeunicode", value)]));
    }
    // The two spellings as *pathspecs* rather than as output: with the
    // conversion on, both select the one tracked path; with it off, only the
    // composed one does.
    out.push(Case::new("ls-files", &["ls-files", "--", "\u{e9}.txt"], nfd));
    out.push(Case::new("ls-files", &["ls-files", "--", "e\u{301}.txt"], nfd));
    out.push(
        Case::new("ls-files", &["ls-files", "--", "e\u{301}.txt"], nfd)
            .with_config(&[("core.precomposeunicode", "false")]),
    );
}
