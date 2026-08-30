//! `git apply` — the patch applier, taken as a language rather than as a flag
//! list.
//!
//! A patch is *input*, and `apply` is the parser and the writer for it. Almost
//! everything that can go wrong is a property of the bytes on stdin — a header
//! git has never seen, a hunk that lies about its own length, a base85 payload,
//! a line ending nobody meant to commit — and none of it is reachable from an
//! argv sweep alone. The corpus already had a broad flag matrix (see the
//! division below); what it did not have was a second dialect, a literal binary
//! payload, a three-way merge that *succeeds*, or a single case in which stock
//! leaves a file half-written on purpose.
//!
//! Every payload here is a `&'static [u8]` literal, byte for byte, because the
//! interesting inputs differ from the boring ones by whitespace: a CR before a
//! LF, a trailing blank, a missing final newline. A payload read off disk or
//! rebuilt by a formatter would lose exactly the bytes being measured.
//!
//! Every `index <old>..<new>` line names a blob the fixture actually holds.
//! Measured directly against a hand-built copy of each shape on stock 2.55.0:
//!
//! ```text
//! Shape::Linear   README.md   9741694d75caeb49d3b7c1f59451c0c56bf6216c
//!                 src/lib.rs  46e89a20198dc3175599f285c8d874fc19439a64
//! Shape::Patches  app/main.c  a43c1d3e88e0731084fc975dff1f94fcf0c9c61c  (main, MAIN_C_BASE)
//!                 app/main.c  c6d9f18ee0b9c19f3e3196f4ccfaf687e2fa435e  (pending~1, MAIN_C_ONE)
//!                 app/main.c  10848bbbb316d91944ff7552aa70e6d8abe649cd  (pending,  MAIN_C_TWO)
//! ```
//!
//! An invented pre-image id is not a harmless placeholder: `--3way` and
//! `--build-fake-ancestor` read it, so a made-up id turns a case about merging
//! into a case about a missing object, and the case then passes while measuring
//! the wrong branch. [`A_FAKE_MISSING_BLOB`] is the one payload that names an id
//! no repository has, and it does so deliberately.
//!
//! # How this divides territory with the nine files that already touch `apply`
//!
//! All nine were read. What each owns, and what is therefore *not* repeated
//! here:
//!
//! * **`corpus/mail_patch.rs`** — `apply` against a **tracked file that is not
//!   a patch** (`README.md`, `src/lib.rs`), plus empty/absent/`/dev/null` input
//!   and `--bogus-flag`. Its module header records why: it was written when the
//!   runner spawned both sides with stdin closed. It owns the whole "input is
//!   not a patch" axis and the usage dump; nothing here feeds a non-patch.
//! * **`corpus/mail_series.rs`** — the largest existing owner, and the one this
//!   file is deliberately adjacent to: 46 stdin-fed `apply` cases over
//!   `P_MULTI`, `P_CREATE`, `P_DEEP`, `P_ZERO`, `P_NOEOL`, `P_RECOUNT`, `P_WS`,
//!   `P_NOAPPLY`, `P_FAKEIDX`, `P_MODE`, `P_RENAME`, `P_SYMLINK`, `P_UNSAFE`
//!   and `P_EMPTY`. It owns the *ordinary* git-format patch under the flag
//!   matrix — `--stat`/`--numstat`/`--summary`, `--index`/`--cached`,
//!   `-p0`/`-p2`/`--directory=`/`--include=`/`--exclude=` one at a time,
//!   `--unidiff-zero`, `--recount`, `--inaccurate-eof`, `-C1`, `--allow-empty`,
//!   all five `--whitespace=` values, `--ignore-space-change`, `-R`, and the
//!   `apply.whitespace`/`apply.ignoreWhitespace` config pair. Its three
//!   `--3way` cases all **fail**, and its one `--reject` case rejects the file's
//!   only hunk. This file starts where that stops: a second dialect, a binary
//!   payload, a three-way that succeeds, and a reject that leaves half a file.
//! * **`corpus/shape_reach.rs`** — 50 cases over the *files* `Shape::Patches`
//!   ships (`patches/valid.patch`, `corrupt`, `context-only`, `whitespace`,
//!   `offset`, `binary`, and the two mailboxes), all as path arguments. It owns
//!   the file-argument form, the offset search, and the fixture's own **delta**
//!   binary patch. Nothing here passes a patch as a path except where the
//!   *working directory* is the subject ([`subdirectory_and_pathspecs`]).
//! * **`corpus/fixture_gaps.rs`** — `apply` over `patches/symlink.patch` on
//!   `Shape::Symlinks`, reading (`--check`/`--stat`/`--numstat`/`--summary`)
//!   and writing (`--index`/`--cached`/`-3`). It owns mode 120000. No payload
//!   here creates a symlink.
//! * **`corpus/fixture_gaps3.rs`** — `apply --stat`/`--check` over
//!   `mail/ok.mbox` on `Shape::AmHooks`, as the no-hook control beside `am`. It
//!   owns "a mailbox handed to `apply`".
//! * **`corpus/exit_codes.rs`** — one case, `apply --check --directory=zz
//!   patches/valid.patch`, for its exit code. It owns `--directory=` pointed at
//!   a name that does not exist.
//! * **`corpus/sequences.rs`** — the multi-step half: `apply --index` then
//!   `commit` then `apply -R`, a `--3way` conflict then a resolution, `--cached`
//!   then `commit`, the symlink patch through a commit, and `apply` used to
//!   *stage* content for the clean/smudge and `ident` filter sequences. It owns
//!   everything whose subject is what happens **after** the apply.
//! * **`corpus/misc_commands.rs`** and **`corpus/stash_deep.rs`** — their
//!   `apply` hits are `git stash apply`, a different verb entirely, plus
//!   `apply -h`. Nothing overlaps.
//!
//! What none of the nine has, and this file adds:
//!
//! | axis | why it was unreachable |
//! |---|---|
//! | a **non-git** patch dialect | every payload in the corpus starts `diff --git` |
//! | a **literal** binary hunk | the only binary patch in any fixture is a `delta` one |
//! | `--3way` that **succeeds** | needs a pre-image blob in the store that is *not* the current file |
//! | `--ours`/`--theirs`/`--union` | never spelled anywhere in the corpus |
//! | `--intent-to-add`/`-N`, `--no-add`, `--allow-overlap`, `--binary`, `--allow-binary-replacement`, `--ignore-whitespace`, `--no-3way`, `--quiet` | never spelled for `apply` anywhere in the corpus |
//! | a patch stock applies **half** of | the one existing `--reject` case rejects the only hunk |
//! | `--build-fake-ancestor` that writes a file | both existing uses are on input that produces none |
//! | `apply` run from a **subdirectory** | no `apply` case sets a cwd |
//!
//! # What is deliberately not measured
//!
//! * **`--unsafe-paths`.** [`A_ABSOLUTE`] climbs one component with a leading
//!   `/` and stock's default `-p1` strips it, so the file lands *inside* the
//!   worktree and the probe sees it. A path that genuinely escapes needs
//!   `--unsafe-paths`, and stock then writes into the case directory's parent,
//!   which the state probe cannot read and which would leak between cases —
//!   the same limit `corpus/mail_series.rs` records for `P_UNSAFE`.
//! * **`--build-fake-ancestor` outside the worktree.** The file name is an argv
//!   token and a case may not name an absolute path, so the index is written to
//!   `fake.idx` at the fixture root, where `probe_worktree_content` hashes its
//!   bytes. That is only sound because `builtin/apply.c` writes the fake
//!   ancestor's entries with the `stat` fields **zeroed** — verified by hand
//!   below — so the file is a function of the patch's ids alone and carries no
//!   clock, inode or device number.
//! * **`--whitespace=fix` on a context line.** Stock warns about the damaged
//!   *context* line and leaves it alone: `fix` rewrites added lines only. The
//!   case pins that pair (a warning with no edit); there is no spelling that
//!   makes stock repair context, so nothing here asks for one.
//! * **The wording of any diagnostic that names an input line.** Eight cases
//!   here reach a message of the form `… at <stdin>:<n>`, and git 2.50.1 writes
//!   the same message as `… (line <n>)` or `… on line <n>`. Those eight are
//!   deliberately **not** strict: with the prose compared, the second oracle
//!   reports `gits-disagree` and no answer can make the case pass, so the case
//!   would measure nothing at all. Their exit code, stdout and post-state are
//!   version-stable and are compared; only the prose is dropped, and only
//!   where two releases of git already disagree about it. Every other refusal
//!   in this file is strict, verified identical on both oracles.

use crate::fixture::Shape;
use crate::runner::Case;

/// One stdin-fed case, stdout + exit code + state.
fn a(cmd: &'static str, args: &[&str], shape: Shape, input: &'static [u8], out: &mut Vec<Case>) {
    out.push(Case::with_stdin(cmd, args, shape, input));
}

/// One stdin-fed case with stderr compared byte for byte, for the refusals whose
/// diagnostic *is* the behaviour — `apply` reports which hunk failed and why,
/// and a port that refuses at a different line has not matched.
fn ax(cmd: &'static str, args: &[&str], shape: Shape, input: &'static [u8], out: &mut Vec<Case>) {
    out.push(Case { compare_stderr: true, ..Case::with_stdin(cmd, args, shape, input) })
}

pub fn cases(out: &mut Vec<Case>) {
    option_grammar(out);
    patch_dialects(out);
    binary_payloads(out);
    three_way(out);
    rejects_and_partials(out);
    structural_headers(out);
    path_rewriting(out);
    subdirectory_and_pathspecs(out);
    intent_to_add_and_no_add(out);
    whitespace_matching(out);
    fake_ancestor(out);
}

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

/// The plain change the whole file's happy path is built on: one line appended
/// to `Shape::Linear`'s `README.md`, spelled as a **git** patch. Present so the
/// dialect cases below have a control that differs from them in nothing but the
/// header.
const A_GIT_FORM: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+traditional unified
";

/// The same change as a **traditional** unified diff: no `diff --git` line, no
/// `index` line, and the `--- `/`+++ ` names carrying the tab-separated
/// timestamps `diff -u` writes.
///
/// Nothing in the corpus had ever fed `apply` a patch that was not git's own,
/// so `parse_traditional_patch` — the branch that has to guess the filename
/// from two `---`/`+++` lines with no header to confirm it — was dead code no
/// case could enter. The timestamps are literal and fixed; a real `diff -u`
/// would put a clock in the payload, which is why this is a literal rather than
/// something the fixture generates.
const A_TRADITIONAL: &[u8] = b"--- a/README.md\t2005-04-07 22:13:13.000000000 -0700
+++ b/README.md\t2005-04-07 22:13:13.000000000 -0700
@@ -1 +1,2 @@
 # fixture
+traditional unified
";

/// A traditional diff with **no `a/`,`b/` prefix at all**, which only `-p0` can
/// name. Stock applies it at `-p0` and cannot see the file at the default
/// `-p1`, so the pair separates a `-p` implementation from one that special
/// cases git's prefixes.
const A_TRADITIONAL_NO_PREFIX: &[u8] = b"--- README.md
+++ README.md
@@ -1 +1,2 @@
 # fixture
+no prefix
";

/// An old-style **context** diff — `*** `/`--- ` names, a `***************`
/// separator, `*** 1 ****`/`--- 1 ----` ranges and `! ` change markers. Git has
/// no parser for it: stock reads the whole thing, finds nothing it recognises,
/// and dies `No valid patches in input` at exit 128.
///
/// Worth a case precisely because it *looks* like a patch. An implementation
/// that scans for `---` and `+++` without checking what precedes them can
/// mistake the second line for a unified header and then apply something.
const A_CONTEXT_DIFF: &[u8] = b"*** a/README.md\t2005-04-07 22:13:13.000000000 -0700
--- b/README.md\t2005-04-07 22:13:13.000000000 -0700
***************
*** 1 ****
! # fixture
--- 1 ----
! # context diff
";

/// Prose, then a trailer, then a blank line, then a git patch. `apply` skips
/// everything before the first header it recognises, which is what lets it read
/// a mail body — and an implementation that requires the first line to be a
/// header refuses input every mail tool produces.
const A_LEADING_JUNK: &[u8] = b"Some prose before the patch.
Signed-off-by: nobody

diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+after leading junk
";

/// [`A_GIT_FORM`] with **every line** of the patch terminated CRLF, as a patch
/// that has been through a Windows editor or a webmail client is.
///
/// Stock does not strip the CRs: the context line reads `# fixture\r`, which is
/// not what the file holds, so it refuses at exit 1. That refusal is the
/// behaviour — a port that trims line endings while parsing would apply this
/// and write a file stock never writes.
const A_CRLF_PATCH: &[u8] = b"diff --git a/README.md b/README.md\r
index 9741694..1314fa4 100644\r
--- a/README.md\r
+++ b/README.md\r
@@ -1 +1,2 @@\r
 # fixture\r
+crlf patch line\r
";

/// A patch whose own lines end LF, adding a line whose **content** ends CR.
/// Stock applies it, reports the CR as a trailing-whitespace error, and writes
/// the CR into the file. The two halves are separable: a port can warn and not
/// write the byte, or write it and not warn.
const A_CRLF_CONTENT: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+carriage return here\r
";

/// A **copy**: `copy from`/`copy to` with a 100% similarity index and no hunk.
///
/// No fixture and no payload in the corpus has ever carried one — `P_RENAME` in
/// `corpus/mail_series.rs` is the rename half, and a rename and a copy take
/// different branches (`gitdiff_copysrc` against `gitdiff_renamesrc`), because a
/// copy must leave the source in place. `--summary` prints
/// ` copy README.md => COPY.md (100%)` and `--index` stages a second entry
/// holding the *same* blob, which is what the state probe sees.
const A_COPY: &[u8] = b"diff --git a/README.md b/COPY.md
similarity index 100%
copy from README.md
copy to COPY.md
";

/// A patch cut off **inside** a hunk: the header promises two pre-image lines
/// and one post-image addition, and the input ends after the first context
/// line. Stock: `error: corrupt patch at <stdin>:7`, exit 128 — and exit 0 with
/// no output under `--recount`, which recounts the hunk from the body it was
/// actually given and so accepts a truncation as a shorter hunk. That pair is
/// the case: an implementation that validates the header before recounting
/// rejects both.
const A_TRUNCATED_HUNK: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1,2 +1,3 @@
 # fixture
";

/// A patch cut off **inside** the header, after `--- ` and before `+++ `. A
/// different failure with a different message — `git diff header lacks filename
/// information at <stdin>:4` — because the parser has not reached a hunk at
/// all. Two truncations rather than one: an implementation that reports both as
/// `corrupt patch` agrees with stock on neither.
const A_TRUNCATED_HEADER: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
";

/// A git header naming a file and describing **no change at all**: no hunk, no
/// mode line, no rename. Stock counts it as no patch (`No valid patches in
/// input`, exit 128) and `--allow-empty` turns that into a silent exit 0.
///
/// `P_EMPTY` in `corpus/mail_series.rs` is zero *bytes*; this is a well-formed
/// header that contributes nothing, which is the other way to reach the same
/// counter and the one a header parser can get wrong.
const A_EMPTY_DIFF: &[u8] = b"diff --git a/README.md b/README.md
";

/// The same file twice in one patch, the second hunk written against the first
/// one's post-image. Stock applies them in order against its in-memory image
/// rather than re-reading the file, so both land.
const A_SAME_FILE_TWICE: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+first
diff --git a/README.md b/README.md
index 1314fa4..2222222 100644
--- a/README.md
+++ b/README.md
@@ -1,2 +1,3 @@
 # fixture
 first
+second
";

/// Two hunks against the same file at the *same* pre-image line, so the second
/// overlaps the first. Stock refuses at exit 1 **with and without**
/// `--allow-overlap` — the flag permits an overlap the applier can still
/// reconcile, not this one — and pinning both spellings is what stops a port
/// from making the flag mean "skip the check".
const A_OVERLAP: &[u8] = b"diff --git a/app/main.c b/app/main.c
index a43c1d3..1111111 100644
--- a/app/main.c
+++ b/app/main.c
@@ -1,4 +1,4 @@
-static const int VERSION = 1;
+static const int VERSION = 5;
 
 int add(int a, int b)
 {
@@ -1,4 +1,4 @@
-static const int VERSION = 1;
+static const int VERSION = 6;
 
 int add(int a, int b)
 {
";

/// A mode change to a mode that is not a mode. Stock rejects it while still
/// parsing the header — `error: invalid mode at <stdin>:3: 100999` — rather
/// than failing later when it tries to chmod, so the case separates a header
/// validator from a `chmod` that happens to fail.
const A_MODE_INVALID: &[u8] = b"diff --git a/src/lib.rs b/src/lib.rs
old mode 100644
new mode 100999
";

/// A well-formed mode change on a path that does not exist. A different exit
/// code from [`A_MODE_INVALID`] — 1, not 128 — because the header parsed and
/// the *file* is what is missing.
const A_MODE_ABSENT: &[u8] = b"diff --git a/absent.txt b/absent.txt
old mode 100644
new mode 100755
";

/// A deletion whose recorded pre-image line is not the file's. The `index` line
/// names the real blob, so an implementation that trusts the id and skips the
/// content check deletes a file stock refuses to touch — which is the worst
/// class of divergence an applier has.
const A_DELETE_MISMATCH: &[u8] = b"diff --git a/src/lib.rs b/src/lib.rs
deleted file mode 100644
index 46e89a2..0000000
--- a/src/lib.rs
+++ /dev/null
@@ -1 +0,0 @@
-pub fn two() -> u32 { 2 }
";

/// Creation of an **empty** file: `new file mode` and the empty blob's id, with
/// no `---`/`+++` pair and no hunk. The empty blob
/// (`e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`) is a constant of the hash
/// function, so the id is derived rather than invented.
const A_NEW_EMPTY: &[u8] = b"diff --git a/empty.txt b/empty.txt
new file mode 100644
index 0000000..e69de29
";

/// An ordinary creation, for the flags that only have an opinion about a file
/// being *added*: `-N`/`--intent-to-add`, `--no-add`, `--directory=`.
/// `3b18e51` is `hello world\n`.
const A_CREATE: &[u8] = b"diff --git a/created.txt b/created.txt
new file mode 100644
index 0000000..3b18e51
--- /dev/null
+++ b/created.txt
@@ -0,0 +1 @@
+hello world
";

/// A creation whose `+++` name is **absolute**. Stock's default `-p1` strips
/// the leading empty component, so the file lands at `tmp/absolute.txt` inside
/// the worktree and the probe can see it. See the module header for why the
/// `--unsafe-paths` half is absent.
const A_ABSOLUTE: &[u8] = b"diff --git a/tmp/absolute.txt b/tmp/absolute.txt
new file mode 100644
index 0000000..d95f3ad
--- /dev/null
+++ /tmp/absolute.txt
@@ -0,0 +1 @@
+content
";

/// A creation at a path deep enough that `--stat`'s 50-column name budget has
/// to elide it (` .../directory/structure/with/a/long/name/file.txt`). Every
/// existing `--stat` case in the corpus is on a short path, so the elision —
/// the one part of the diffstat renderer that is not a `printf` — had never
/// run.
const A_LONG_PATH: &[u8] = b"diff --git a/a/very/deeply/nested/directory/structure/with/a/long/name/file.txt b/a/very/deeply/nested/directory/structure/with/a/long/name/file.txt
new file mode 100644
index 0000000..3b18e51
--- /dev/null
+++ b/a/very/deeply/nested/directory/structure/with/a/long/name/file.txt
@@ -0,0 +1 @@
+hello world
";

/// A creation at `src`, which is a **directory** in every shape. Stock gets as
/// far as writing and fails at the filesystem: `error: unable to write file
/// 'src' mode 100644: Directory not empty`, exit 128. The check `apply` does
/// *not* do beforehand is the point — an implementation that pre-validates
/// reports a different error at a different code.
const A_PATH_IS_DIR: &[u8] = b"diff --git a/src b/src
new file mode 100644
index 0000000..3b18e51
--- /dev/null
+++ b/src
@@ -0,0 +1 @@
+hello world
";

/// Two files in one patch: the first applies, the second cannot. `apply` is
/// **all or nothing** across files, so stock writes neither — and `--reject`
/// turns the same input into a half-applied tree with a `.rej` beside the file
/// that failed. Both are cases, because that is the whole contract of
/// `--reject` and the corpus had never seen the difference.
const A_TWO_FILES_ONE_FAILS: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+first file ok
diff --git a/src/lib.rs b/src/lib.rs
index 46e89a2..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-pub fn nine() -> u32 { 9 }
+pub fn ten() -> u32 { 10 }
";

/// A **zero-context** hunk whose header also lies about its post-image length
/// (`+2,5` for one added line). It needs `--unidiff-zero` *and* `--recount`:
/// with only the first, stock dies `corrupt patch at <stdin>:7`.
///
/// `P_ZERO` and `P_RECOUNT` in `corpus/mail_series.rs` are the two halves
/// separately; neither reaches the interaction, and the interaction is where an
/// implementation that recounts *before* deciding whether context is required
/// diverges.
const A_ZERO_AND_RECOUNT: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1,0 +2,5 @@
+zero context
";

/// A pre-image marked as having **no** trailing newline where the file has one.
/// Stock refuses, and refuses identically under `--inaccurate-eof` — that flag
/// tolerates a *missing* marker, not a spurious one. The pair is what stops
/// `--inaccurate-eof` being implemented as "ignore the marker".
const A_PREIMAGE_NO_EOL: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-# fixture
\\ No newline at end of file
+# replaced
";

/// Context indented with **spaces** where `Shape::Patches`'s `app/main.c` uses
/// a **tab**, and one added line in the same style. Stock refuses; with
/// `--ignore-whitespace` or `--ignore-space-change` it applies — and writes the
/// added line with the *patch's* spaces while leaving the context's tab alone,
/// so a port that normalises the file rather than the comparison is caught by
/// the bytes rather than by the exit code.
///
/// The hunk deliberately ends on `}` rather than on a blank line: with a blank
/// final context line stock refuses even under `--ignore-whitespace`, which
/// would have made the flag look inert.
const A_WS_CONTEXT: &[u8] = b"diff --git a/app/main.c b/app/main.c
index a43c1d3..1111111 100644
--- a/app/main.c
+++ b/app/main.c
@@ -3,4 +3,5 @@ static const int VERSION = 1;
 int add(int a, int b)
 {
    return a + b;
+   return 0;
 }
";

/// A **context** line carrying a trailing tab. `--whitespace=fix` repairs added
/// lines only, so stock warns and writes the file with the damage still in it.
const A_WS_IN_CONTEXT_LINE: &[u8] = b"diff --git a/app/main.c b/app/main.c
index a43c1d3..1111111 100644
--- a/app/main.c
+++ b/app/main.c
@@ -1,3 +1,4 @@
 static const int VERSION = 1;\t
 
+int added(void);
 int add(int a, int b)
";

// --- three-way ------------------------------------------------------------

/// A patch that **cannot** be applied directly and **can** be merged.
///
/// Its pre-image is `c6d9f18` — `Shape::Patches`'s `pending~1:app/main.c`,
/// which is in the object store but is not what the worktree holds — and the
/// hunk's trailing context reaches into the `subtract()` function that only
/// that blob has. So a direct application fails on the missing context, and the
/// three-way merge succeeds cleanly: the fake ancestor is `c6d9f18`, *theirs*
/// is that blob with `VERSION = 3`, *ours* is `main`'s `a43c1d3`, and the two
/// sides changed different regions.
///
/// Every existing `--3way` case in the corpus fails, so the branch that reports
/// `Applied patch to 'app/main.c' cleanly.` and *stages the result* — `--3way`
/// implies `--index`, which is itself unmeasured — had never run. The
/// post-image id `06ad329` is the real hash of the merged text, so `-R` names a
/// blob rather than a placeholder.
const A_THREE_WAY_CLEAN: &[u8] = b"diff --git a/app/main.c b/app/main.c
index c6d9f18..06ad329 100644
--- a/app/main.c
+++ b/app/main.c
@@ -1,9 +1,9 @@
-static const int VERSION = 1;
+static const int VERSION = 3;
 
 int add(int a, int b)
 {
 	return a + b;
 }
 
 int subtract(int a, int b)
 {
";

/// The same pre-image blob, with the change on the **one line both sides
/// touched**: `main` rewrote `return add(1, 2) + subtract(4, 3);` down to
/// `return add(1, 2);`, and this patch rewrites it to `subtract(9, 9)`.
///
/// So the three-way merge conflicts, and the four spellings produce four
/// different worktrees — plain `--3way` writes `<<<<<<< ours`/`>>>>>>> theirs`
/// markers and leaves stages 1/2/3 in the index at exit 1, while `--ours`,
/// `--theirs` and `--union` each resolve it at exit 0. Those three flags are
/// spelled nowhere in the corpus.
const A_THREE_WAY_CONFLICT: &[u8] = b"diff --git a/app/main.c b/app/main.c
index c6d9f18..4a0fff6 100644
--- a/app/main.c
+++ b/app/main.c
@@ -8,9 +8,9 @@ int add(int a, int b)
 int subtract(int a, int b)
 {
 	return a - b;
 }
 
 int main(void)
 {
-	return add(1, 2) + subtract(4, 3);
+	return add(1, 2) + subtract(9, 9);
 }
";

/// [`A_THREE_WAY_CLEAN`]'s change with a pre-image id **no repository holds**,
/// so `--3way` cannot build the ancestor at all: `repository lacks the
/// necessary blob to perform 3-way merge.` followed by `Falling back to direct
/// application...` and then the ordinary failure.
///
/// `P_FAKEIDX` in `corpus/mail_series.rs` reaches the same first line on a
/// one-line file; this one reaches the **fallback** — stock prints the notice
/// and then tries anyway, which is a second code path that the shorter payload
/// exits before.
const A_FAKE_MISSING_BLOB: &[u8] = b"diff --git a/app/main.c b/app/main.c
index 1234567..7654321 100644
--- a/app/main.c
+++ b/app/main.c
@@ -1,9 +1,9 @@
-static const int VERSION = 1;
+static const int VERSION = 3;
 
 int add(int a, int b)
 {
 	return a + b;
 }
 
 int subtract(int a, int b)
 {
";

/// Two hunks against `Shape::Patches`'s `app/main.c` with pre-image `c6d9f18`:
/// the first is confined to lines the two blobs share and applies to the
/// worktree, the second needs the `subtract()` block that the worktree does not
/// have.
///
/// Plain `apply` therefore writes nothing, and `--reject` writes the first hunk
/// into the file and hunk 2 into `app/main.c.rej`. That half-written state is
/// the thing the corpus could not previously express: its one `--reject` case
/// rejects a file's only hunk, so "rejected everything" and "applied some of
/// it" were the same measurement.
const A_HALF_REJECT: &[u8] = b"diff --git a/app/main.c b/app/main.c
index c6d9f18..01a0061 100644
--- a/app/main.c
+++ b/app/main.c
@@ -1,3 +1,3 @@
-static const int VERSION = 1;
+static const int VERSION = 7;
 
 int add(int a, int b)
@@ -8,4 +8,4 @@ int add(int a, int b)
 int subtract(int a, int b)
 {
-	return a - b;
+	return a - b; /* edited */
 }
";

// --- binary ---------------------------------------------------------------

/// A **literal** binary hunk creating a new file, as `diff --binary` writes one:
/// a 40-character `index` pair, `GIT binary patch`, a forward `literal 10` block
/// and the reverse `literal 0` block beneath it.
///
/// Every binary patch the corpus has ever applied is `patches/binary.patch` in
/// `Shape::Patches`, which is a **delta** — `deflate`d against a base the store
/// already holds. The literal encoding is a different reader (`patch_binary` ->
/// `binary_literal` rather than `binary_delta`), and it is the one every patch
/// for a *new* file uses, because there is no base to delta against. The blob's
/// ten bytes include NUL, `\r`, `\n` and two bytes above 0x7f, so a reader that
/// treats the decoded payload as text is caught by the file's contents.
const A_BINARY_LITERAL_NEW: &[u8] = b"diff --git a/tiny.bin b/tiny.bin
new file mode 100644
index 0000000000000000000000000000000000000000..7c7cee75b1ce7463d7a8bb7e8cf6896e39b1e6b2
GIT binary patch
literal 10
RcmZQzWcvS)i<g0&0{{(-0w(|f

literal 0
HcmV?d00001

";

/// A literal binary hunk **replacing a text file**: `Shape::Linear`'s
/// `README.md`, whose real blob `9741694d…` is the pre-image id, becomes twelve
/// bytes that are not text.
///
/// The reverse block is a real `literal 10` rather than the `literal 0` a
/// creation carries, so `-R` has something to read. Stock applies it with no
/// flag at all, which is what makes `--binary` and
/// `--allow-binary-replacement` measurable as the no-ops they now are: both
/// have been accepted-and-ignored since the compatibility rename, and a port
/// that treats either as a gate refuses a patch stock applies.
const A_BINARY_LITERAL_MODIFY: &[u8] = b"diff --git a/README.md b/README.md
index 9741694d75caeb49d3b7c1f59451c0c56bf6216c..771459fc5bffe20c1b8f1e3cfa542fab3acc505c 100644
GIT binary patch
literal 12
TcmZQzWcvS)i<g0&<HSh-6f*<3

literal 10
RcmY#ZNXx7!DJ@Fn0ss-I162S3

";

/// What `git diff` writes for the same change **without** `--binary`: the
/// abbreviated `index` line and the sentence `Binary files … differ`, and no
/// payload.
///
/// Stock refuses it — `cannot apply binary patch to 'README.md' without full
/// index line` — and refuses identically under `--allow-binary-replacement`.
/// This is the input a user most often has by accident, and an implementation
/// that reconstructs the post-image from the abbreviated id would apply
/// something stock will not.
const A_BINARY_NO_PAYLOAD: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..771459f 100644
Binary files a/README.md and b/README.md differ
";

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

/// The option grammar: the combinations `apply` refuses before it reads a byte,
/// and the ones where the **last** spelling wins.
///
/// None of it was measured. `corpus/mail_patch.rs` pins one unknown flag and
/// the usage dump; nothing anywhere asks what two flags do to each other, and
/// `apply` has three checks that die at parse time and a pair of flags whose
/// order decides the answer.
fn option_grammar(out: &mut Vec<Case>) {
    // `builtin/apply.c` refuses these two together, before parsing the patch.
    ax("apply", &["apply", "--reject", "--3way"], Shape::Linear, A_GIT_FORM, out);
    ax("apply", &["apply", "--3way", "--reject"], Shape::Linear, A_GIT_FORM, out);
    // The conflict-resolution flags are meaningless without a merge to resolve:
    // `fatal: --ours, --theirs, and --union require --3way`, and the same
    // refusal for all three spellings.
    ax("apply", &["apply", "--ours"], Shape::Linear, A_GIT_FORM, out);
    ax("apply", &["apply", "--theirs"], Shape::Linear, A_GIT_FORM, out);
    ax("apply", &["apply", "--union"], Shape::Linear, A_GIT_FORM, out);
    // `--no-3way` and `-3` are the same setting written twice, so the order is
    // the whole answer: one of these applies the patch and the other refuses it.
    a("apply", &["apply", "-3", "--no-3way"], Shape::Patches, A_THREE_WAY_CLEAN, out);
    a("apply", &["apply", "--no-3way", "-3"], Shape::Patches, A_THREE_WAY_CLEAN, out);
    // `--stat` is a bare switch for `apply` and an optional-width option for
    // `diff`. A port that shares the parser between them accepts this.
    ax("apply", &["apply", "--stat=40"], Shape::Linear, A_GIT_FORM, out);
    // `-p` takes a non-negative integer, and a value larger than the path is
    // deep produces a *header* diagnostic rather than an argument one.
    // Non-strict: the message carries the input line number, and the two
    // stock gits word that differently (`at <stdin>:6` against `(line 6)`), so
    // a strict case here could never pass on either. The exit code, the empty
    // stdout and the untouched tree are version-stable and are what is measured.
    a("apply", &["apply", "-p9"], Shape::Linear, A_CREATE, out);
    ax("apply", &["apply", "-p0"], Shape::Linear, A_CREATE, out);
    // `--quiet` suppresses the `No valid patches in input` line and keeps the
    // exit code, which is the only way to see that the two are separable.
    ax("apply", &["apply", "--quiet"], Shape::Linear, b"", out);
    ax("apply", &["apply", "--quiet", "--allow-empty"], Shape::Linear, b"", out);
    ax("apply", &["apply", "-q", "--check"], Shape::Linear, A_GIT_FORM, out);
    // Overlapping hunks, with and without the flag that names them.
    ax("apply", &["apply"], Shape::Patches, A_OVERLAP, out);
    ax("apply", &["apply", "--allow-overlap"], Shape::Patches, A_OVERLAP, out);
}

/// Patch **dialects**: input that is a patch without being a git patch.
///
/// Every payload the corpus had ever fed `apply` begins `diff --git`, so the
/// traditional-diff parser, the leading-junk skip and the "this is not a patch
/// even though it has `---` in it" refusal were all unreachable.
fn patch_dialects(out: &mut Vec<Case>) {
    // The control: the same change in git's own form.
    a("apply", &["apply", "--index"], Shape::Linear, A_GIT_FORM, out);

    // A traditional unified diff with timestamps, through every reader.
    a("apply", &["apply", "--check"], Shape::Linear, A_TRADITIONAL, out);
    a("apply", &["apply"], Shape::Linear, A_TRADITIONAL, out);
    a("apply", &["apply", "--index"], Shape::Linear, A_TRADITIONAL, out);
    a("apply", &["apply", "--stat"], Shape::Linear, A_TRADITIONAL, out);
    a("apply", &["apply", "--numstat"], Shape::Linear, A_TRADITIONAL, out);
    a("apply", &["apply", "--summary"], Shape::Linear, A_TRADITIONAL, out);
    // No `index` line means no pre-image id, so `--3way` has nothing to build
    // an ancestor from.
    ax("apply", &["apply", "--3way"], Shape::Linear, A_TRADITIONAL, out);

    // No prefix at all: visible only at `-p0`, invisible at the default.
    a("apply", &["apply", "-p0"], Shape::Linear, A_TRADITIONAL_NO_PREFIX, out);
    ax("apply", &["apply"], Shape::Linear, A_TRADITIONAL_NO_PREFIX, out);
    a("apply", &["apply", "-p0", "--index"], Shape::Linear, A_TRADITIONAL_NO_PREFIX, out);

    // A context diff is not a patch git can read, and saying so is the case.
    ax("apply", &["apply", "--check"], Shape::Linear, A_CONTEXT_DIFF, out);
    ax("apply", &["apply", "--stat"], Shape::Linear, A_CONTEXT_DIFF, out);
    ax("apply", &["apply", "--allow-empty"], Shape::Linear, A_CONTEXT_DIFF, out);

    // Prose before the header, as every mail body has.
    a("apply", &["apply"], Shape::Linear, A_LEADING_JUNK, out);
    a("apply", &["apply", "--stat"], Shape::Linear, A_LEADING_JUNK, out);

    // Line endings, both ways round.
    ax("apply", &["apply", "--check"], Shape::Linear, A_CRLF_PATCH, out);
    ax("apply", &["apply"], Shape::Linear, A_CRLF_PATCH, out);
    a("apply", &["apply", "--ignore-whitespace"], Shape::Linear, A_CRLF_PATCH, out);
    a("apply", &["apply"], Shape::Linear, A_CRLF_CONTENT, out);
    a("apply", &["apply", "--index"], Shape::Linear, A_CRLF_CONTENT, out);
    a("apply", &["apply", "--whitespace=fix"], Shape::Linear, A_CRLF_CONTENT, out);
    ax("apply", &["apply", "--whitespace=error"], Shape::Linear, A_CRLF_CONTENT, out);
}

/// Binary payloads in the **literal** encoding, and the abbreviated form that
/// carries none.
fn binary_payloads(out: &mut Vec<Case>) {
    a("apply", &["apply", "--check"], Shape::Linear, A_BINARY_LITERAL_NEW, out);
    a("apply", &["apply"], Shape::Linear, A_BINARY_LITERAL_NEW, out);
    a("apply", &["apply", "--index"], Shape::Linear, A_BINARY_LITERAL_NEW, out);
    a("apply", &["apply", "--stat"], Shape::Linear, A_BINARY_LITERAL_NEW, out);
    a("apply", &["apply", "--numstat"], Shape::Linear, A_BINARY_LITERAL_NEW, out);
    a("apply", &["apply", "--summary"], Shape::Linear, A_BINARY_LITERAL_NEW, out);

    a("apply", &["apply", "--check"], Shape::Linear, A_BINARY_LITERAL_MODIFY, out);
    a("apply", &["apply"], Shape::Linear, A_BINARY_LITERAL_MODIFY, out);
    a("apply", &["apply", "--index"], Shape::Linear, A_BINARY_LITERAL_MODIFY, out);
    a("apply", &["apply", "--cached"], Shape::Linear, A_BINARY_LITERAL_MODIFY, out);
    // The two spellings of the flag that has been a no-op since the rename.
    a("apply", &["apply", "--binary", "--index"], Shape::Linear, A_BINARY_LITERAL_MODIFY, out);
    a(
        "apply",
        &["apply", "--allow-binary-replacement", "--index"],
        Shape::Linear,
        A_BINARY_LITERAL_MODIFY,
        out,
    );
    a("apply", &["apply", "--stat"], Shape::Linear, A_BINARY_LITERAL_MODIFY, out);
    a("apply", &["apply", "--numstat"], Shape::Linear, A_BINARY_LITERAL_MODIFY, out);
    // The reverse block is a real one, so `-R` reads it rather than refusing.
    a("apply", &["apply", "-R", "--check"], Shape::Linear, A_BINARY_LITERAL_MODIFY, out);
    // Applying it over an index that already differs from the worktree.
    a("apply", &["apply", "--cached"], Shape::Dirty, A_BINARY_LITERAL_MODIFY, out);

    // No payload at all: the refusal, and the flag that does not lift it.
    ax("apply", &["apply", "--check"], Shape::Linear, A_BINARY_NO_PAYLOAD, out);
    ax("apply", &["apply"], Shape::Linear, A_BINARY_NO_PAYLOAD, out);
    ax(
        "apply",
        &["apply", "--allow-binary-replacement"],
        Shape::Linear,
        A_BINARY_NO_PAYLOAD,
        out,
    );
    // `--stat` never needs the payload, so it answers where applying cannot.
    a("apply", &["apply", "--stat"], Shape::Linear, A_BINARY_NO_PAYLOAD, out);
    a("apply", &["apply", "--numstat"], Shape::Linear, A_BINARY_NO_PAYLOAD, out);
}

/// `--3way` reaching a real three-way merge — clean, conflicted, resolved three
/// ways, and unable to start.
fn three_way(out: &mut Vec<Case>) {
    // Direct application fails; the merge succeeds and stages the result.
    ax("apply", &["apply"], Shape::Patches, A_THREE_WAY_CLEAN, out);
    a("apply", &["apply", "--3way"], Shape::Patches, A_THREE_WAY_CLEAN, out);
    a("apply", &["apply", "-3"], Shape::Patches, A_THREE_WAY_CLEAN, out);
    a("apply", &["apply", "--3way", "--index"], Shape::Patches, A_THREE_WAY_CLEAN, out);
    a("apply", &["apply", "--3way", "--check"], Shape::Patches, A_THREE_WAY_CLEAN, out);
    // Reversed, the ancestor it needs is the *post*-image, which is not in the
    // store — a different diagnostic reached through the same flag.
    ax("apply", &["apply", "-R", "--3way"], Shape::Patches, A_THREE_WAY_CLEAN, out);

    // The conflicting merge, and the three flags that resolve it.
    a("apply", &["apply", "--3way"], Shape::Patches, A_THREE_WAY_CONFLICT, out);
    a("apply", &["apply", "--3way", "--ours"], Shape::Patches, A_THREE_WAY_CONFLICT, out);
    a("apply", &["apply", "--3way", "--theirs"], Shape::Patches, A_THREE_WAY_CONFLICT, out);
    a("apply", &["apply", "--3way", "--union"], Shape::Patches, A_THREE_WAY_CONFLICT, out);
    a(
        "apply",
        &["apply", "--3way", "--ours", "--index"],
        Shape::Patches,
        A_THREE_WAY_CONFLICT,
        out,
    );
    // A conflict style is a merge setting, so it has to reach the markers the
    // three-way writes — nothing else in the corpus asks `apply` about one.
    out.push(
        Case::with_stdin("apply", &["apply", "--3way"], Shape::Patches, A_THREE_WAY_CONFLICT)
            .with_config(&[("merge.conflictStyle", "diff3")]),
    );
    out.push(
        Case::with_stdin("apply", &["apply", "--3way"], Shape::Patches, A_THREE_WAY_CONFLICT)
            .with_config(&[("merge.conflictStyle", "zdiff3")]),
    );

    // The ancestor cannot be built: the notice, then the fallback, then the
    // ordinary failure — three lines a port can produce two of.
    ax("apply", &["apply", "--3way"], Shape::Patches, A_FAKE_MISSING_BLOB, out);
    ax("apply", &["apply", "--3way", "--check"], Shape::Patches, A_FAKE_MISSING_BLOB, out);

    // The path is not in the index at all, which is a third way for `--3way` to
    // stop: `Shape::Dirty` has no `app/main.c`.
    ax("apply", &["apply", "--3way"], Shape::Dirty, A_THREE_WAY_CLEAN, out);
}

/// What `apply` leaves behind when it stops halfway: `.rej` files, a partially
/// written file, and the atomicity that holds without `--reject`.
fn rejects_and_partials(out: &mut Vec<Case>) {
    // One file, two hunks, one of which applies. Without `--reject` nothing is
    // written; with it, the file is half-patched and `app/main.c.rej` holds the
    // hunk that failed.
    ax("apply", &["apply"], Shape::Patches, A_HALF_REJECT, out);
    a("apply", &["apply", "--reject"], Shape::Patches, A_HALF_REJECT, out);
    a("apply", &["apply", "--reject", "-v"], Shape::Patches, A_HALF_REJECT, out);
    a("apply", &["apply", "--reject", "-q"], Shape::Patches, A_HALF_REJECT, out);
    ax("apply", &["apply", "--check"], Shape::Patches, A_HALF_REJECT, out);
    // `--3way` needs no `.rej` for the same input: it merges instead, and the
    // merge conflicts, so the file comes back with markers and the index with
    // stages 1/2/3 at exit 1. Half-applied-with-a-reject and
    // conflicted-in-place are two different answers to one patch.
    a("apply", &["apply", "--3way"], Shape::Patches, A_HALF_REJECT, out);

    // Two files, one of which fails. `apply` is all-or-nothing across files.
    ax("apply", &["apply"], Shape::Linear, A_TWO_FILES_ONE_FAILS, out);
    ax("apply", &["apply", "--index"], Shape::Linear, A_TWO_FILES_ONE_FAILS, out);
    ax("apply", &["apply", "--cached"], Shape::Linear, A_TWO_FILES_ONE_FAILS, out);
    a("apply", &["apply", "--reject"], Shape::Linear, A_TWO_FILES_ONE_FAILS, out);
    // Excluding the file that fails makes the rest apply, which is the only way
    // to see that the refusal was per-patch rather than per-file.
    a(
        "apply",
        &["apply", "--include=README.md", "--exclude=src/lib.rs"],
        Shape::Linear,
        A_TWO_FILES_ONE_FAILS,
        out,
    );
    a("apply", &["apply", "--stat"], Shape::Linear, A_TWO_FILES_ONE_FAILS, out);
}

/// Headers that describe something other than a hunk: a copy, a duplicate, a
/// mode that is not a mode, a deletion that does not match, and two truncations.
fn structural_headers(out: &mut Vec<Case>) {
    a("apply", &["apply", "--summary"], Shape::Linear, A_COPY, out);
    a("apply", &["apply", "--stat"], Shape::Linear, A_COPY, out);
    a("apply", &["apply", "--numstat"], Shape::Linear, A_COPY, out);
    a("apply", &["apply", "--index"], Shape::Linear, A_COPY, out);
    a("apply", &["apply", "--cached"], Shape::Linear, A_COPY, out);
    a("apply", &["apply"], Shape::Linear, A_COPY, out);
    ax("apply", &["apply", "-R", "--check"], Shape::Linear, A_COPY, out);

    a("apply", &["apply"], Shape::Linear, A_SAME_FILE_TWICE, out);
    a("apply", &["apply", "--index"], Shape::Linear, A_SAME_FILE_TWICE, out);
    a("apply", &["apply", "--stat"], Shape::Linear, A_SAME_FILE_TWICE, out);
    a("apply", &["apply", "--check"], Shape::Linear, A_SAME_FILE_TWICE, out);

    // Non-strict for the reason `-p9` gives: 2.55.0 says `at <stdin>:3` where
    // 2.50.1 says `on line 2`.
    a("apply", &["apply", "--check"], Shape::Linear, A_MODE_INVALID, out);
    a("apply", &["apply", "--stat"], Shape::Linear, A_MODE_INVALID, out);
    ax("apply", &["apply", "--check"], Shape::Linear, A_MODE_ABSENT, out);
    a("apply", &["apply", "--summary"], Shape::Linear, A_MODE_ABSENT, out);

    ax("apply", &["apply"], Shape::Linear, A_DELETE_MISMATCH, out);
    ax("apply", &["apply", "--index"], Shape::Linear, A_DELETE_MISMATCH, out);
    a("apply", &["apply", "--reject"], Shape::Linear, A_DELETE_MISMATCH, out);
    a("apply", &["apply", "--summary"], Shape::Linear, A_DELETE_MISMATCH, out);

    a("apply", &["apply", "--index"], Shape::Linear, A_NEW_EMPTY, out);
    a("apply", &["apply"], Shape::Linear, A_NEW_EMPTY, out);
    a("apply", &["apply", "--summary"], Shape::Linear, A_NEW_EMPTY, out);
    a("apply", &["apply", "--numstat"], Shape::Linear, A_NEW_EMPTY, out);

    // Both truncations report an input line number, which the two stock gits
    // word differently, so these are measured on exit code and state alone.
    // `--recount` stays strict: it exits 0 with nothing on stderr, and that
    // silence is identical across releases.
    a("apply", &["apply", "--check"], Shape::Linear, A_TRUNCATED_HUNK, out);
    ax("apply", &["apply", "--recount", "--check"], Shape::Linear, A_TRUNCATED_HUNK, out);
    a("apply", &["apply", "--stat"], Shape::Linear, A_TRUNCATED_HUNK, out);
    a("apply", &["apply", "--check"], Shape::Linear, A_TRUNCATED_HEADER, out);
    a("apply", &["apply", "--stat"], Shape::Linear, A_TRUNCATED_HEADER, out);

    ax("apply", &["apply", "--check"], Shape::Linear, A_EMPTY_DIFF, out);
    ax("apply", &["apply", "--allow-empty"], Shape::Linear, A_EMPTY_DIFF, out);
    ax("apply", &["apply", "--stat"], Shape::Linear, A_EMPTY_DIFF, out);

    // The zero-context and lying-count interaction: one flag is not enough.
    // Same line-number wording split, so exit code and state only.
    a("apply", &["apply", "--unidiff-zero"], Shape::Linear, A_ZERO_AND_RECOUNT, out);
    a("apply", &["apply", "--unidiff-zero", "--recount"], Shape::Linear, A_ZERO_AND_RECOUNT, out);
    ax("apply", &["apply", "--recount"], Shape::Linear, A_ZERO_AND_RECOUNT, out);
}

/// Where a patch's names point once `-p`, `--directory=` and the filesystem have
/// had their say.
fn path_rewriting(out: &mut Vec<Case>) {
    // An absolute name, whose leading component `-p1` strips.
    a("apply", &["apply"], Shape::Linear, A_ABSOLUTE, out);
    a("apply", &["apply", "--index"], Shape::Linear, A_ABSOLUTE, out);
    a("apply", &["apply", "--stat"], Shape::Linear, A_ABSOLUTE, out);
    ax("apply", &["apply", "-p0"], Shape::Linear, A_ABSOLUTE, out);

    // A nested `--directory=`, which the corpus has only ever spelled with one
    // component or with a name that does not exist.
    a("apply", &["apply", "--directory=deep/nest", "--index"], Shape::Linear, A_CREATE, out);
    a("apply", &["apply", "--directory=deep/nest/"], Shape::Linear, A_CREATE, out);
    a("apply", &["apply", "--directory=src", "--index"], Shape::Linear, A_CREATE, out);
    a("apply", &["apply", "--directory=deep/nest", "--stat"], Shape::Linear, A_CREATE, out);

    // The name elision `--stat` does, which no short path can reach.
    a("apply", &["apply", "--stat"], Shape::Linear, A_LONG_PATH, out);
    a("apply", &["apply", "--numstat"], Shape::Linear, A_LONG_PATH, out);
    a("apply", &["apply", "--summary"], Shape::Linear, A_LONG_PATH, out);
    a("apply", &["apply", "--index"], Shape::Linear, A_LONG_PATH, out);

    // A file where a directory is: the failure happens at the write, not at a
    // pre-flight check, and `--check` therefore passes.
    ax("apply", &["apply"], Shape::Linear, A_PATH_IS_DIR, out);
    a("apply", &["apply", "--check"], Shape::Linear, A_PATH_IS_DIR, out);
    a("apply", &["apply", "--cached"], Shape::Linear, A_PATH_IS_DIR, out);
}

/// `apply` run from a **subdirectory**, which no case in the corpus does.
///
/// The question is whether a patch's names and `--include=`/`--exclude=`'s
/// pathspecs are read against the working directory or against the repository
/// root. Stock uses the root for both: from `app/`, a patch naming
/// `a/app/main.c` finds the file, `--include=app/main.c` matches it, and
/// `--include=main.c` matches nothing — so a port that prefixes either with the
/// cwd applies the wrong file or none.
fn subdirectory_and_pathspecs(out: &mut Vec<Case>) {
    out.push(Case::with_stdin("apply", &["apply", "--check"], Shape::Patches, A_HALF_REJECT).in_dir("app"));
    out.push(
        Case::with_stdin("apply", &["apply", "--include=app/main.c", "--check"], Shape::Patches, A_HALF_REJECT)
            .in_dir("app"),
    );
    out.push(
        Case::with_stdin("apply", &["apply", "--include=main.c", "--check"], Shape::Patches, A_HALF_REJECT)
            .in_dir("app"),
    );
    out.push(
        Case::with_stdin("apply", &["apply", "--exclude=app/main.c", "--check"], Shape::Patches, A_HALF_REJECT)
            .in_dir("app"),
    );
    // Writing from a subdirectory, so the file that moves is the one at the
    // root's `app/main.c` rather than one below the cwd.
    out.push(Case::with_stdin("apply", &["apply", "--reject"], Shape::Patches, A_HALF_REJECT).in_dir("app"));
    out.push(Case::with_stdin("apply", &["apply", "--3way"], Shape::Patches, A_THREE_WAY_CLEAN).in_dir("app"));
    // A creation from a subdirectory: `--directory=` is relative to the root
    // too, and the file must not land under `app/`.
    out.push(Case::with_stdin("apply", &["apply", "--index"], Shape::Patches, A_CREATE).in_dir("app"));
    out.push(
        Case::with_stdin("apply", &["apply", "--directory=deep", "--index"], Shape::Patches, A_CREATE)
            .in_dir("app"),
    );
    // The one path argument this file uses, and only because the cwd is the
    // subject: the patch is named relatively while its contents are not.
    out.push(Case::new("apply", &["apply", "../patches/valid.patch"], Shape::Patches).in_dir("app"));
    out.push(
        Case::strict("apply", &["apply", "--check", "patches/valid.patch"], Shape::Patches).in_dir("app"),
    );
}

/// `-N`/`--intent-to-add` and `--no-add`: the two flags that change what an
/// *addition* means. Neither is spelled for `apply` anywhere in the corpus.
fn intent_to_add_and_no_add(out: &mut Vec<Case>) {
    // `-N` writes the file and records the path with no content, which
    // `status --porcelain=v2` renders ` A` rather than `A `.
    a("apply", &["apply", "-N"], Shape::Linear, A_CREATE, out);
    a("apply", &["apply", "--intent-to-add"], Shape::Linear, A_CREATE, out);
    // With `--index` the entry is a real staged add, so `-N` has nothing left
    // to do — the pair is what shows the flag is about the *index*, not the
    // worktree.
    a("apply", &["apply", "--intent-to-add", "--index"], Shape::Linear, A_CREATE, out);
    a("apply", &["apply", "-N", "--cached"], Shape::Linear, A_CREATE, out);
    a("apply", &["apply", "-N"], Shape::Linear, A_NEW_EMPTY, out);
    a("apply", &["apply", "-N"], Shape::Linear, A_BINARY_LITERAL_NEW, out);
    // `-N` on a shape that already has intent-to-add entries, so a port that
    // rewrites the whole index rather than adding one entry is caught.
    a("apply", &["apply", "-N"], Shape::IntentToAdd, A_CREATE, out);

    // `--no-add` drops the added lines and keeps everything else, so a creation
    // becomes an empty file and a modification becomes a deletion of the
    // removed lines alone.
    a("apply", &["apply", "--no-add"], Shape::Linear, A_CREATE, out);
    a("apply", &["apply", "--no-add", "--index"], Shape::Linear, A_CREATE, out);
    a("apply", &["apply", "--no-add"], Shape::Linear, A_GIT_FORM, out);
    a("apply", &["apply", "--no-add", "--stat"], Shape::Linear, A_GIT_FORM, out);
    a("apply", &["apply", "--no-add"], Shape::Linear, A_TRADITIONAL, out);
}

/// Whitespace as a *matching* question rather than a reporting one.
///
/// `corpus/mail_series.rs` owns `--whitespace=` over added lines that carry
/// damage, and one `--ignore-space-change --check`. What it cannot reach is a
/// patch whose **context** disagrees with the file only in whitespace: `P_WS`
/// applies either way, so the ignore flags could not change an outcome there.
fn whitespace_matching(out: &mut Vec<Case>) {
    ax("apply", &["apply", "--check"], Shape::Patches, A_WS_CONTEXT, out);
    a("apply", &["apply", "--ignore-whitespace"], Shape::Patches, A_WS_CONTEXT, out);
    a("apply", &["apply", "--ignore-space-change"], Shape::Patches, A_WS_CONTEXT, out);
    a("apply", &["apply", "--ignore-whitespace", "--check"], Shape::Patches, A_WS_CONTEXT, out);
    a("apply", &["apply", "--ignore-whitespace", "--index"], Shape::Patches, A_WS_CONTEXT, out);
    // The flag and the policy are independent axes and have never been crossed.
    a(
        "apply",
        &["apply", "--ignore-whitespace", "--whitespace=fix"],
        Shape::Patches,
        A_WS_CONTEXT,
        out,
    );
    ax(
        "apply",
        &["apply", "--ignore-whitespace", "--whitespace=error"],
        Shape::Patches,
        A_WS_CONTEXT,
        out,
    );
    // Delivered from a file scope rather than from `-c`, which is the half
    // `apply.ignoreWhitespace` has never been measured through.
    out.push(
        Case::with_stdin("apply", &["apply"], Shape::Patches, A_WS_CONTEXT)
            .with_config(&[("apply.ignoreWhitespace", "change")]),
    );
    out.push(
        Case::with_stdin("apply", &["apply"], Shape::Patches, A_WS_CONTEXT)
            .with_config(&[("apply.ignoreWhitespace", "no")]),
    );

    // Damage on a *context* line: `fix` repairs additions only, so stock warns
    // and writes the file with the trailing tab still in it.
    a("apply", &["apply"], Shape::Patches, A_WS_IN_CONTEXT_LINE, out);
    a("apply", &["apply", "--whitespace=fix"], Shape::Patches, A_WS_IN_CONTEXT_LINE, out);
    ax("apply", &["apply", "--whitespace=error"], Shape::Patches, A_WS_IN_CONTEXT_LINE, out);
    a("apply", &["apply", "--whitespace=nowarn"], Shape::Patches, A_WS_IN_CONTEXT_LINE, out);

    // A marker that claims a missing newline the file has, and the flag that
    // does *not* tolerate it.
    ax("apply", &["apply"], Shape::Linear, A_PREIMAGE_NO_EOL, out);
    ax("apply", &["apply", "--inaccurate-eof"], Shape::Linear, A_PREIMAGE_NO_EOL, out);
    a("apply", &["apply", "--reject"], Shape::Linear, A_PREIMAGE_NO_EOL, out);
}

/// `--build-fake-ancestor`, writing a file.
///
/// Both existing uses are on input that produces none — `corpus/mail_patch.rs`
/// feeds a non-patch and `corpus/mail_series.rs` pairs it with `--check` over
/// `P_CREATE`, whose only pre-image id is the all-zero one — so the index this
/// option exists to write had never been written by any case.
///
/// The output is safe to compare because it carries no machine state:
/// `builtin/apply.c` stages each pre-image blob with the `stat` fields zeroed,
/// which was confirmed by hand on stock 2.55.0 (`od -c fake.idx` shows the
/// ctime/mtime/dev/ino/uid/gid words as NUL and only the mode, id and name
/// populated). The file lands at the fixture root, where
/// `probe_worktree_content` reads its bytes.
fn fake_ancestor(out: &mut Vec<Case>) {
    a("apply", &["apply", "--build-fake-ancestor=fake.idx", "--check"], Shape::Linear, A_GIT_FORM, out);
    a("apply", &["apply", "--build-fake-ancestor=fake.idx"], Shape::Linear, A_GIT_FORM, out);
    a(
        "apply",
        &["apply", "--build-fake-ancestor=fake.idx", "--check"],
        Shape::Linear,
        A_TWO_FILES_ONE_FAILS,
        out,
    );
    // A pre-image the store does not hold: the ancestor cannot be completed.
    ax(
        "apply",
        &["apply", "--build-fake-ancestor=fake.idx", "--check"],
        Shape::Patches,
        A_FAKE_MISSING_BLOB,
        out,
    );
    // The three-way payload, whose pre-image *is* in the store but is not the
    // current file — the case the option was designed for.
    a(
        "apply",
        &["apply", "--build-fake-ancestor=fake.idx", "--check"],
        Shape::Patches,
        A_THREE_WAY_CLEAN,
        out,
    );
    // Traditional input has no ids at all, so there is nothing to record.
    ax(
        "apply",
        &["apply", "--build-fake-ancestor=fake.idx", "--check"],
        Shape::Linear,
        A_TRADITIONAL,
        out,
    );
}
