//! Differential corpus cases for the info/attributes/search subsystem:
//! `check-attr`, `check-ignore`, `check-mailmap`, `grep`, `help`, `bugreport`,
//! `diagnose`, `archive`, `get-tar-commit-id`, `verify-commit`, `verify-tag`,
//! and the shared `blame`/`annotate`/`pickaxe` implementation.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! Three properties of the shared fixtures shape what can be asserted here, and
//! are recorded so a reader does not have to rediscover them:
//!
//! * **Dates are pinned.** `env::harden` fixes `GIT_AUTHOR_DATE` and
//!   `GIT_COMMITTER_DATE` to `1700000000 +0000` with `TZ=UTC`, so `blame`'s
//!   human format is byte-comparable for *committed* lines. Lines attributed to
//!   `00000000` ("Not Committed Yet") carry the wall clock instead, so any case
//!   whose output includes one is unmeasurable — those are written with `-s`
//!   (no author/date column) or an `-L` range that lands on a committed line.
//! * **No `.gitattributes`, `.gitignore` or `.mailmap` exists in any shape**, and
//!   a case is one argv against a pristine copy, so it cannot create one. The
//!   `check-*` trio is therefore exercised on its "nothing configured" path,
//!   its quoting/`-z` framing, and its error paths — not on rule matching.
//! * **No binary blob exists in any shape**, so `grep -a`/`-I` are covered only
//!   for their text behavior.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    check_attr(out);
    check_ignore(out);
    check_mailmap(out);
    grep(out);
    help(out);
    bugreport_diagnose(out);
    archive(out);
    verify(out);
    blame(out);
    annotate_pickaxe(out);
}

/// Shorthand for a single-shape case.
fn one(out: &mut Vec<Case>, cmd: &'static str, args: &[&str], shape: Shape) {
    out.push(Case::new(cmd, args, shape));
}

// ---------------------------------------------------------------------------
// check-attr
// ---------------------------------------------------------------------------

/// With no attributes file anywhere, every query answers `unspecified`. That is
/// still the path most scripts hit, and it pins the *report framing* — the
/// `path: attr: value` triple, `-z`'s NUL framing, `-a`'s silence when nothing
/// is set, and the quoting of awkward paths — independently of rule matching.
fn check_attr(out: &mut Vec<Case>) {
    super::read_only("check-attr", &["check-attr", "text", "README.md"], out);
    super::read_only("check-attr", &["check-attr", "-a", "README.md"], out);
    super::read_only("check-attr", &["check-attr", "--all", "README.md"], out);
    super::read_only(
        "check-attr",
        &["check-attr", "text", "eol", "diff", "README.md", "src/lib.rs"],
        out,
    );

    one(out, "check-attr", &["check-attr", "--cached", "-a", "README.md"], Shape::Linear);
    one(out, "check-attr", &["check-attr", "text", "--", "README.md"], Shape::Linear);
    one(out, "check-attr", &["check-attr", "text", "no/such/file.txt"], Shape::Linear);
    // A directory is not a path with attributes; git answers for it anyway.
    one(out, "check-attr", &["check-attr", "-a", "src"], Shape::Linear);
    // stdin is /dev/null under the runner: `--stdin` must terminate, not block.
    one(out, "check-attr", &["check-attr", "--stdin", "text"], Shape::Linear);
    one(out, "check-attr", &["check-attr", "-z", "--stdin", "text"], Shape::Linear);
    one(out, "check-attr", &["check-attr", "--stdin", "text", "README.md"], Shape::Linear);
    one(out, "check-attr", &["check-attr", "--source=HEAD", "-a", "src/lib.rs"], Shape::Branched);
    one(out, "check-attr", &["check-attr", "--source=v0.2.0", "text", "src/lib.rs"], Shape::Branched);

    // Quoting is where path handling diverges: `-z` must emit raw bytes, the
    // default must C-quote the same names.
    let awkward = ["with space.txt", "üñïçødé.txt", "quote\"name.txt", "nested/deep/path.txt"];
    let mut plain = vec!["check-attr", "text"];
    plain.extend_from_slice(&awkward);
    one(out, "check-attr", &plain, Shape::AwkwardPaths);
    let mut nul = vec!["check-attr", "-z", "text"];
    nul.extend_from_slice(&awkward);
    one(out, "check-attr", &nul, Shape::AwkwardPaths);
    one(out, "check-attr", &["check-attr", "-a", "quote\"name.txt"], Shape::AwkwardPaths);

    one(out, "check-attr", &["check-attr", "-a", ".gitmodules", "sub"], Shape::Submodule);
    one(out, "check-attr", &["check-attr", "-a", "conflict.txt"], Shape::Conflicted);

    // ---- error paths ----
    // No pathname at all.
    one(out, "check-attr", &["check-attr", "text"], Shape::Linear);
    // No attribute names and no `-a`.
    one(out, "check-attr", &["check-attr"], Shape::Linear);
    // `-a` and explicit names are mutually exclusive in intent; git accepts and
    // ignores the names, which is itself the behavior to match.
    one(out, "check-attr", &["check-attr", "-a", "text", "README.md"], Shape::Linear);
    one(out, "check-attr", &["check-attr", "--source=nope", "text", "README.md"], Shape::Linear);
}

// ---------------------------------------------------------------------------
// check-ignore
// ---------------------------------------------------------------------------

/// Nothing is ignored in any fixture, so the interesting axis is the *exit
/// code* (1 for "nothing matched") plus `-n`/`-v`'s inverted reporting, which
/// prints a row for non-matching paths and is the only way to get output at all
/// here.
fn check_ignore(out: &mut Vec<Case>) {
    super::read_only("check-ignore", &["check-ignore", "README.md"], out);
    super::read_only("check-ignore", &["check-ignore", "-v", "README.md"], out);
    super::read_only("check-ignore", &["check-ignore", "-n", "-v", "README.md"], out);
    super::read_only("check-ignore", &["check-ignore", "--verbose", "--non-matching", "src/lib.rs"], out);

    one(out, "check-ignore", &["check-ignore", "--no-index", "README.md"], Shape::Linear);
    one(out, "check-ignore", &["check-ignore", "-q", "README.md"], Shape::Linear);
    one(out, "check-ignore", &["check-ignore", "--quiet", "src/lib.rs"], Shape::Linear);
    one(out, "check-ignore", &["check-ignore", "nope.txt"], Shape::Linear);
    one(out, "check-ignore", &["check-ignore", "--stdin"], Shape::Linear);
    one(out, "check-ignore", &["check-ignore", "-z", "--stdin"], Shape::Linear);
    // Empty stdin must produce no records at all — not one record for a line
    // that was never read.
    one(out, "check-ignore", &["check-ignore", "-n", "-v", "--stdin"], Shape::Linear);
    one(out, "check-ignore", &["check-ignore", "-n", "-v", "-z", "--stdin"], Shape::Linear);

    // A tracked path: without `--no-index` git refuses to consider it ignored.
    one(out, "check-ignore", &["check-ignore", "-v", "src/lib.rs"], Shape::Linear);
    one(out, "check-ignore", &["check-ignore", "--no-index", "-v", "-n", "src/lib.rs"], Shape::Linear);
    one(out, "check-ignore", &["check-ignore", "untracked.txt"], Shape::Dirty);
    one(out, "check-ignore", &["check-ignore", "-v", "-n", "untracked.txt"], Shape::Dirty);
    one(out, "check-ignore", &["check-ignore", "-v", "-n", "sub"], Shape::Submodule);

    one(
        out,
        "check-ignore",
        &["check-ignore", "-n", "-v", "--no-index", "quote\"name.txt", "üñïçødé.txt", "with space.txt"],
        Shape::AwkwardPaths,
    );

    // ---- error paths ----
    one(out, "check-ignore", &["check-ignore"], Shape::Linear);
    // `-z` without `--stdin` is rejected outright.
    one(out, "check-ignore", &["check-ignore", "-z", "-v", "-n", "with space.txt"], Shape::AwkwardPaths);
}

// ---------------------------------------------------------------------------
// check-mailmap
// ---------------------------------------------------------------------------

/// No `.mailmap` exists, so every contact echoes back unchanged. That still
/// pins the parser: bare-address input, name-only input, and the `mailmap.file`
/// config path (pointed at a missing file and at a file that is not a mailmap).
fn check_mailmap(out: &mut Vec<Case>) {
    super::read_only("check-mailmap", &["check-mailmap", "A U Thor <author@example.com>"], out);
    one(out, "check-mailmap", &["check-mailmap", "<author@example.com>"], Shape::Linear);
    one(out, "check-mailmap", &["check-mailmap", "nobody"], Shape::Linear);
    one(
        out,
        "check-mailmap",
        &["check-mailmap", "zvcs parity <parity@example.invalid>", "<x@y.z>"],
        Shape::Linear,
    );
    one(out, "check-mailmap", &["check-mailmap", "Üñï çødé <u@example.com>"], Shape::AwkwardPaths);
    one(out, "check-mailmap", &["check-mailmap", "--stdin"], Shape::Linear);

    // `mailmap.file` naming something absent, and something present but not a
    // mailmap — both must be tolerated silently.
    one(
        out,
        "check-mailmap",
        &["-c", "mailmap.file=nope.map", "check-mailmap", "A U Thor <author@example.com>"],
        Shape::Linear,
    );
    one(
        out,
        "check-mailmap",
        &["-c", "mailmap.file=src/lib.rs", "check-mailmap", "A U Thor <author@example.com>"],
        Shape::Linear,
    );
    one(
        out,
        "check-mailmap",
        &["-c", "mailmap.blob=HEAD:README.md", "check-mailmap", "A U Thor <author@example.com>"],
        Shape::Linear,
    );

    // ---- error path ----
    one(out, "check-mailmap", &["check-mailmap"], Shape::Linear);
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

/// The densest block in this module. `grep` had no corpus coverage at all, and
/// its output framing (`path:line`, `path:lineno:line`, `-z`'s NUL joins,
/// `--heading`/`--break` grouping, context markers) is exactly where a
/// reimplementation drifts without anyone noticing.
fn grep(out: &mut Vec<Case>) {
    // Match modes and output framing across every history shape. Dirty deletes
    // `src/lib.rs` and Detached predates `two`, so these also cover "pattern
    // present in some shapes and not others" — including the exit-1 no-match.
    super::read_only("grep", &["grep", "fn"], out);
    super::read_only("grep", &["grep", "-n", "fn"], out);
    super::read_only("grep", &["grep", "-l", "fn"], out);
    super::read_only("grep", &["grep", "-c", "fn"], out);
    super::read_only("grep", &["grep", "--count", "fn"], out);
    super::read_only("grep", &["grep", "--name-only", "fn"], out);
    super::read_only("grep", &["grep", "--full-name", "-n", "fn"], out);
    super::read_only("grep", &["grep", "-i", "FN"], out);
    super::read_only("grep", &["grep", "-w", "fn"], out);
    super::read_only("grep", &["grep", "-v", "fn"], out);
    super::read_only("grep", &["grep", "-h", "-n", "fn"], out);
    super::read_only("grep", &["grep", "-e", "fn"], out);
    super::read_only("grep", &["grep", "-E", "^pub|two"], out);
    super::read_only("grep", &["grep", "-F", "u32"], out);
    super::read_only("grep", &["grep", "-z", "-l", "fn"], out);
    super::read_only("grep", &["grep", "-n", "-z", "fn"], out);
    super::read_only("grep", &["grep", "-L", "fn"], out);
    super::read_only("grep", &["grep", "zzz-no-match"], out);

    // Boolean expression grammar.
    one(out, "grep", &["grep", "-e", "pub", "--and", "-e", "two"], Shape::Branched);
    one(out, "grep", &["grep", "-e", "one", "--or", "-e", "two"], Shape::Branched);
    one(out, "grep", &["grep", "--not", "-e", "two"], Shape::Branched);
    one(out, "grep", &["grep", "--all-match", "-e", "pub", "-e", "two"], Shape::Branched);
    one(
        out,
        "grep",
        &["grep", "(", "-e", "pub", "--or", "-e", "xx", ")", "--and", "-e", "fn"],
        Shape::Branched,
    );
    one(out, "grep", &["grep", "-n", "-e", "ours", "--or", "-e", "theirs"], Shape::Conflicted);

    // Grouping, context, and column reporting.
    one(out, "grep", &["grep", "--heading", "-n", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "--break", "--heading", "-n", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "-A1", "-n", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "-B1", "-n", "two"], Shape::Branched);
    one(out, "grep", &["grep", "-C1", "-n", "two"], Shape::Branched);
    one(out, "grep", &["grep", "--column", "-n", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "-p", "-n", "two"], Shape::Branched);
    one(out, "grep", &["grep", "-W", "two"], Shape::Branched);
    one(out, "grep", &["grep", "--color=always", "-n", "fn"], Shape::Branched);

    // Regex engines and count/limit modifiers.
    one(out, "grep", &["grep", "-P", "p\\w+ fn"], Shape::Branched);
    one(out, "grep", &["grep", "-G", "u32 . 2"], Shape::Branched);
    one(out, "grep", &["grep", "--basic-regexp", "fn one"], Shape::Branched);
    one(out, "grep", &["grep", "-m1", "-n", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "--max-count=1", "-n", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "-c", "-v", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "-q", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "-q", "zzz-no-match"], Shape::Branched);
    one(out, "grep", &["grep", "--threads", "1", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "-I", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "-a", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "--textconv", "-n", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "--no-textconv", "-n", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "--max-depth", "0", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "-r", "fn", "src"], Shape::Branched);
    one(out, "grep", &["grep", "--no-recursive", "fn", "src"], Shape::Branched);

    // Revision arguments: tree search instead of worktree search, and the
    // `<rev>:` prefix stock prepends to every hit.
    one(out, "grep", &["grep", "-n", "fn", "HEAD"], Shape::Branched);
    one(out, "grep", &["grep", "fn", "HEAD", "--", "src/"], Shape::Branched);
    one(out, "grep", &["grep", "-n", "fn", "main", "feature"], Shape::Branched);
    one(out, "grep", &["grep", "-n", "fn", "v0.2.0"], Shape::Branched);
    one(out, "grep", &["grep", "-n", "fn", "HEAD:src"], Shape::Branched);
    one(out, "grep", &["grep", "-n", "side", "HEAD"], Shape::Merged);
    one(out, "grep", &["grep", "--cached", "-n", "fn"], Shape::Branched);

    // Pathspecs, including magic.
    one(out, "grep", &["grep", "-n", "fn", "--", "src/*"], Shape::Branched);
    one(out, "grep", &["grep", "-n", "fn", "--", ":(glob)**/*.rs"], Shape::Branched);
    one(out, "grep", &["grep", "-n", "fn", "--", ":!src/"], Shape::Branched);
    one(out, "grep", &["grep", "-n", "fn", "--", ":(icase)SRC/*"], Shape::Branched);

    // Untracked / index-less searching.
    one(out, "grep", &["grep", "--no-index", "-n", "untracked"], Shape::Dirty);
    one(out, "grep", &["grep", "--untracked", "-n", "untracked"], Shape::Dirty);
    one(out, "grep", &["grep", "--no-index", "-n", "fn", "README.md"], Shape::Branched);

    // Path quoting: default C-quoting versus `-z` raw bytes, on names with a
    // space, a double quote, and multi-byte UTF-8.
    one(out, "grep", &["grep", "-l", "."], Shape::AwkwardPaths);
    one(out, "grep", &["grep", "-z", "-l", "."], Shape::AwkwardPaths);
    one(out, "grep", &["grep", "-n", "."], Shape::AwkwardPaths);
    one(out, "grep", &["grep", "-n", "-z", "."], Shape::AwkwardPaths);
    one(out, "grep", &["grep", "--heading", "-n", "."], Shape::AwkwardPaths);
    one(out, "grep", &["grep", "-n", "unicode"], Shape::AwkwardPaths);
    one(out, "grep", &["grep", "-n", "quote", "--", "quote\"name.txt"], Shape::AwkwardPaths);

    // Submodule descent.
    one(out, "grep", &["grep", "--recurse-submodules", "content"], Shape::Submodule);
    one(out, "grep", &["grep", "--recurse-submodules", "content", "HEAD"], Shape::Submodule);
    one(out, "grep", &["grep", "-l", "--recurse-submodules", "submodule"], Shape::Submodule);
    one(out, "grep", &["grep", "--no-recurse-submodules", "content"], Shape::Submodule);
    one(out, "grep", &["grep", "-n", "path", ".gitmodules"], Shape::Submodule);

    // ---- error paths ----
    one(out, "grep", &["grep"], Shape::Linear);
    one(out, "grep", &["grep", "--exclude-standard", "fn"], Shape::Branched);
    one(out, "grep", &["grep", "-O", "cat", "fn"], Shape::Branched);
}

// ---------------------------------------------------------------------------
// help
// ---------------------------------------------------------------------------

/// `git help` is a compatibility surface for shell completion and for humans.
/// The listing forms are pure data and fully comparable; the viewer forms
/// (`-m`/`-w`/`-i`) shell out and are included for their exit code only.
fn help(out: &mut Vec<Case>) {
    one(out, "help", &["help"], Shape::Linear);
    one(out, "help", &["help", "-g"], Shape::Linear);
    one(out, "help", &["help", "--guides"], Shape::Linear);
    one(out, "help", &["help", "--config"], Shape::Linear);
    one(out, "help", &["help", "--user-interfaces"], Shape::Linear);
    one(out, "help", &["help", "--developer-interfaces"], Shape::Linear);
    one(out, "help", &["help", "--config-for-completion"], Shape::Linear);
    one(out, "help", &["help", "--config-sections-for-completion"], Shape::Linear);
    // `--all` walks git's exec-path *and* `$PATH` for `git-*` helpers, so this
    // pair is sensitive to the PATH the harness inherits. Kept because the
    // dedup rule it exercises — a helper git knows as a builtin must not also be
    // listed as external — is real behavior, not an artifact.
    one(out, "help", &["help", "--all"], Shape::Linear);
    one(out, "help", &["help", "--all", "--no-verbose"], Shape::Linear);
    one(out, "help", &["help", "--all", "--no-external-commands"], Shape::Linear);
    one(out, "help", &["help", "--no-aliases", "--all"], Shape::Linear);
    one(out, "help", &["help", "-w", "status"], Shape::Linear);
    one(out, "help", &["help", "-i", "status"], Shape::Linear);
    one(out, "help", &["help", "nosuchcommandxyz"], Shape::Linear);
}

// ---------------------------------------------------------------------------
// bugreport / diagnose
// ---------------------------------------------------------------------------

/// `bugreport` is comparable when the filename is pinned: it writes to stdout
/// nothing, reports the path on stderr, and the only state change is one
/// untracked file whose *name* the probe sees. `-s <literal>` and `--no-suffix`
/// make that name fixed; the default `-s %Y-%m-%d-%H%M` does not.
///
/// `diagnose` prints a version/platform banner and the mount's free space to
/// stdout, so its stdout is unmeasurable by construction — stock does not
/// reproduce it either. Cases are kept so the bucket is populated from a real
/// measurement rather than from an assumption.
fn bugreport_diagnose(out: &mut Vec<Case>) {
    one(out, "bugreport", &["bugreport", "-s", "fixed"], Shape::Linear);
    one(out, "bugreport", &["bugreport", "--suffix", "fixed"], Shape::Branched);
    one(out, "bugreport", &["bugreport", "--no-suffix"], Shape::Linear);
    one(out, "bugreport", &["bugreport", "-s", "fixed", "-o", "."], Shape::Linear);
    one(out, "bugreport", &["bugreport"], Shape::Linear);
    one(out, "bugreport", &["bugreport", "--diagnose=stats", "-s", "fixed"], Shape::Linear);
    one(out, "bugreport", &["bugreport", "-s", "fixed", "-o", "nosuchdir"], Shape::Linear);

    one(out, "diagnose", &["diagnose", "-s", "fixed"], Shape::Linear);
    one(out, "diagnose", &["diagnose", "--mode=stats", "-s", "fixed"], Shape::Linear);
    one(out, "diagnose", &["diagnose", "--mode=all", "-s", "fixed"], Shape::Linear);
    one(out, "diagnose", &["diagnose", "--mode=nope", "-s", "fixed"], Shape::Linear);
}

// ---------------------------------------------------------------------------
// archive / get-tar-commit-id
// ---------------------------------------------------------------------------

/// `git archive` stamps every entry's mtime from the archived commit, and the
/// fixture's commit dates are pinned, so a tar from a fixed commit is
/// byte-stable — the whole container is comparable, not just the file list.
/// `tgz` is likewise stable because git writes the gzip member with a zeroed
/// mtime field.
fn archive(out: &mut Vec<Case>) {
    super::read_only("archive", &["archive", "--format=tar", "HEAD"], out);
    super::read_only("archive", &["archive", "HEAD"], out);

    one(out, "archive", &["archive", "--list"], Shape::Linear);
    one(out, "archive", &["archive", "--format=tgz", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--format=tar.gz", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--format=zip", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--format=zip", "-0", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--format=tar", "--prefix=p/", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--format=tar", "--prefix=p", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--format=tar", "-v", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--format=tar", "--add-file=README.md", "HEAD"], Shape::Linear);
    one(
        out,
        "archive",
        &["archive", "--format=tar", "--add-virtual-file=x.txt:hello", "HEAD"],
        Shape::Linear,
    );
    one(out, "archive", &["archive", "--worktree-attributes", "--format=tar", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--format=tar", "HEAD", "src"], Shape::Branched);
    one(out, "archive", &["archive", "--format=tar", "HEAD:src"], Shape::Branched);
    one(out, "archive", &["archive", "--format=tar", "v0.2.0"], Shape::Branched);
    one(out, "archive", &["archive", "--format=tar", "HEAD"], Shape::AwkwardPaths);
    one(out, "archive", &["archive", "--format=tar", "HEAD"], Shape::Submodule);
    // `-o` writes a file: exercises the post-command state probe, not stdout.
    one(out, "archive", &["archive", "--format=tar", "-o", "out.tar", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "-o", "out.zip", "HEAD"], Shape::Linear);

    // ---- error paths ----
    one(out, "archive", &["archive"], Shape::Linear);
    one(out, "archive", &["archive", "--format=nope", "HEAD"], Shape::Linear);
    one(out, "archive", &["archive", "--format=tar", "nosuchrev"], Shape::Linear);
    one(out, "archive", &["archive", "--format=tar", "HEAD", "nosuchpath"], Shape::Linear);

    // stdin is /dev/null under the runner, so this is the EOF-before-header
    // path; it is the only form of the command reachable without a pipe.
    one(out, "get-tar-commit-id", &["get-tar-commit-id"], Shape::Linear);
    one(out, "get-tar-commit-id", &["get-tar-commit-id", "extra"], Shape::Linear);
}

// ---------------------------------------------------------------------------
// verify-commit / verify-tag
// ---------------------------------------------------------------------------

/// Nothing in the fixtures is signed, so these cover the *unsigned* verdict —
/// exit 1 with the object still parsed and, under `-v`, printed. That path runs
/// without invoking gpg, which keeps it hermetic.
fn verify(out: &mut Vec<Case>) {
    super::read_only("verify-commit", &["verify-commit", "HEAD"], out);
    one(out, "verify-commit", &["verify-commit", "-v", "HEAD"], Shape::Linear);
    one(out, "verify-commit", &["verify-commit", "--verbose", "HEAD"], Shape::Merged);
    one(out, "verify-commit", &["verify-commit", "--raw", "HEAD"], Shape::Linear);
    one(out, "verify-commit", &["verify-commit", "HEAD", "HEAD~1"], Shape::Branched);
    one(out, "verify-commit", &["verify-commit", "nosuchrev"], Shape::Linear);
    one(out, "verify-commit", &["verify-commit"], Shape::Linear);
    // A tag object handed to verify-commit: wrong object type.
    one(out, "verify-commit", &["verify-commit", "v0.2.0"], Shape::Branched);

    // Lightweight tag: not a tag *object*, so verification cannot even start.
    one(out, "verify-tag", &["verify-tag", "v0.1.0"], Shape::Branched);
    // Annotated but unsigned.
    one(out, "verify-tag", &["verify-tag", "v0.2.0"], Shape::Branched);
    one(out, "verify-tag", &["verify-tag", "-v", "v0.2.0"], Shape::Branched);
    one(out, "verify-tag", &["verify-tag", "--raw", "v0.2.0"], Shape::Branched);
    one(out, "verify-tag", &["verify-tag", "--format=%(tag) %(objecttype)", "v0.2.0"], Shape::Branched);
    one(out, "verify-tag", &["verify-tag", "v0.1.0", "v0.2.0"], Shape::Branched);
    one(out, "verify-tag", &["verify-tag", "nosuchtag"], Shape::Branched);
    one(out, "verify-tag", &["verify-tag"], Shape::Branched);
}

// ---------------------------------------------------------------------------
// blame
// ---------------------------------------------------------------------------

/// Committed lines carry the pinned author date, so the human format is
/// byte-comparable. Anything blamed to `00000000` carries the wall clock, so
/// the Dirty and Conflicted cases below use `-s` or an `-L` range that lands on
/// a committed line — otherwise stock cannot reproduce its own output and the
/// case measures nothing.
fn blame(out: &mut Vec<Case>) {
    // Rename/copy detection: `-C` escalates through three levels, each widening
    // the search from the same commit, to the parent's other files, to the whole
    // history.
    one(out, "blame", &["blame", "-M", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-M3", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-C", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-C5", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-C", "-C", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-C", "-C", "-C", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-M", "-C", "-C", "main.txt"], Shape::Merged);

    // Ignore-revs: `--ignore-rev HEAD` must reattribute line 2 to the root
    // commit rather than to HEAD, which is a visible, date-free change.
    one(out, "blame", &["blame", "--ignore-rev", "HEAD", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--ignore-revs-file", "/dev/null", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--ignore-revs-file", "nope.txt", "src/lib.rs"], Shape::Branched);
    one(
        out,
        "blame",
        &["-c", "blame.ignoreRevsFile=/dev/null", "blame", "src/lib.rs"],
        Shape::Branched,
    );
    one(
        out,
        "blame",
        &["-c", "blame.markUnblamableLines=true", "blame", "--ignore-rev", "HEAD", "src/lib.rs"],
        Shape::Branched,
    );
    one(
        out,
        "blame",
        &["-c", "blame.markIgnoredLines=true", "blame", "--ignore-rev", "HEAD", "src/lib.rs"],
        Shape::Branched,
    );

    // Coloring. `--color-by-age` emits SGR even under NO_COLOR because the flag
    // is explicit, so these compare escape sequences too.
    one(out, "blame", &["blame", "--color-lines", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--color-by-age", "src/lib.rs"], Shape::Branched);
    one(
        out,
        "blame",
        &["-c", "blame.coloring=highlightRecent", "blame", "--color-lines", "src/lib.rs"],
        Shape::Branched,
    );
    one(
        out,
        "blame",
        &["-c", "blame.coloring=repeatedLines", "blame", "src/lib.rs"],
        Shape::Branched,
    );

    // Machine-readable formats.
    one(out, "blame", &["blame", "--incremental", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--line-porcelain", "src/lib.rs"], Shape::Branched);
    super::read_only("blame", &["blame", "--incremental", "README.md"], out);

    // `blame.*` config keys that change the default rendering.
    one(out, "blame", &["-c", "blame.showEmail=true", "blame", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["-c", "blame.showRoot=true", "blame", "-f", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["-c", "blame.blankBoundary=true", "blame", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["-c", "blame.date=iso", "blame", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["-c", "blame.date=raw", "blame", "src/lib.rs"], Shape::Branched);

    // Line ranges and column selection.
    one(out, "blame", &["blame", "-L", "2,2", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-L", "1,+1", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-L", "/two/,+1", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-L", "9,9", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-L", "1,9", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-s", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-e", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-w", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-t", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-n", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-b", "--root", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-c", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "-f", "-n", "-l", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--date=short", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--date=raw", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--abbrev=12", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--no-abbrev", "src/lib.rs"], Shape::Branched);

    // Traversal control.
    one(out, "blame", &["blame", "HEAD~1", "--", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--contents", "README.md", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--first-parent", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--first-parent", "main.txt"], Shape::Merged);
    one(out, "blame", &["blame", "--reverse", "HEAD~1..HEAD", "--", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--indent-heuristic", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--minimal", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--show-stats", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--score-debug", "-C", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--progress", "src/lib.rs"], Shape::Branched);
    one(out, "blame", &["blame", "--no-progress", "src/lib.rs"], Shape::Branched);

    // Mid-merge: the conflicted worktree file is blamed against both merge
    // parents, so line 4 belongs to MERGE_HEAD's commit, not to nobody. `-s`
    // and the `-L4,4` range keep the wall clock out of stock's output.
    one(out, "blame", &["blame", "-s", "conflict.txt"], Shape::Conflicted);
    one(out, "blame", &["blame", "-s", "-M", "conflict.txt"], Shape::Conflicted);
    one(out, "blame", &["blame", "-L", "4,4", "conflict.txt"], Shape::Conflicted);
    one(out, "blame", &["blame", "-s", "README.md"], Shape::Dirty);

    // Path handling and other shapes.
    one(out, "blame", &["blame", "quote\"name.txt"], Shape::AwkwardPaths);
    one(out, "blame", &["blame", "-p", "üñïçødé.txt"], Shape::AwkwardPaths);
    one(out, "blame", &["blame", "--", "with space.txt"], Shape::AwkwardPaths);
    one(out, "blame", &["blame", "-f", "main.txt"], Shape::Merged);
    one(out, "blame", &["blame", ".gitmodules"], Shape::Submodule);

    // ---- error paths ----
    one(out, "blame", &["blame", "nosuch.txt"], Shape::Branched);
    // A directory is not blamable; the diagnostic and exit code are the surface.
    one(out, "blame", &["blame", "src"], Shape::Branched);
    one(out, "blame", &["blame"], Shape::Branched);
    one(out, "blame", &["blame", "-h"], Shape::Branched);
    one(out, "blame", &["blame", "-L", "0,0", "src/lib.rs"], Shape::Branched);
}

// ---------------------------------------------------------------------------
// annotate / pickaxe
// ---------------------------------------------------------------------------

/// `annotate` and `pickaxe` are the same engine as `blame` behind different
/// argv[0] defaults — `annotate` implies the tab-separated compat format. Both
/// are exercised against the same shapes so a divergence between the three
/// entry points shows up as a failure rather than as an untested assumption.
fn annotate_pickaxe(out: &mut Vec<Case>) {
    super::read_only("annotate", &["annotate", "README.md"], out);
    one(out, "annotate", &["annotate", "src/lib.rs"], Shape::Branched);
    one(out, "annotate", &["annotate", "-L1,1", "src/lib.rs"], Shape::Branched);
    one(out, "annotate", &["annotate", "--porcelain", "src/lib.rs"], Shape::Branched);
    one(out, "annotate", &["annotate", "-e", "-f", "src/lib.rs"], Shape::Branched);
    one(out, "annotate", &["annotate", "-s", "src/lib.rs"], Shape::Branched);
    one(out, "annotate", &["annotate", "-t", "src/lib.rs"], Shape::Branched);
    one(out, "annotate", &["annotate", "-c", "src/lib.rs"], Shape::Branched);
    one(out, "annotate", &["annotate", "-n", "-l", "src/lib.rs"], Shape::Branched);
    one(out, "annotate", &["annotate", "-M", "-C", "src/lib.rs"], Shape::Branched);
    one(out, "annotate", &["annotate", "--show-stats", "src/lib.rs"], Shape::Branched);
    one(out, "annotate", &["annotate", "with space.txt"], Shape::AwkwardPaths);
    // Mid-merge, restricted to committed lines so the comparison is date-free:
    // line 2 is "ours", line 4 is "theirs" from the other merge parent.
    one(out, "annotate", &["annotate", "-L2,2", "conflict.txt"], Shape::Conflicted);
    one(out, "annotate", &["annotate", "-L4,4", "conflict.txt"], Shape::Conflicted);
    one(out, "annotate", &["annotate", "nosuch.txt"], Shape::Branched);

    super::read_only("pickaxe", &["pickaxe", "README.md"], out);
    one(out, "pickaxe", &["pickaxe", "src/lib.rs"], Shape::Branched);
    one(out, "pickaxe", &["pickaxe", "-s", "src/lib.rs"], Shape::Branched);
    one(out, "pickaxe", &["pickaxe", "--incremental", "src/lib.rs"], Shape::Branched);
    one(out, "pickaxe", &["pickaxe", "-L2,2", "src/lib.rs"], Shape::Branched);
    one(out, "pickaxe", &["pickaxe", "-M", "-C", "-C", "src/lib.rs"], Shape::Branched);
    one(out, "pickaxe", &["pickaxe", "-L", "4,4", "conflict.txt"], Shape::Conflicted);
    one(out, "pickaxe", &["pickaxe", "-s", "conflict.txt"], Shape::Conflicted);
    one(out, "pickaxe", &["pickaxe", "nosuch.txt"], Shape::Branched);
}
