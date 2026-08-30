//! Differential corpus cases for **`git am`** — the mailbox applier, its option
//! surface, and the `.git/rebase-apply/` state machine it drives.
//!
//! Every case here is compared against stock git 2.55.0 for stdout, exit code
//! and post-command repository state. `runner::probe_op_state` walks
//! `.git/rebase-apply/` file by file, so an `am` that stops mid-mailbox is
//! measured on the *contents* of `next`, `last`, `msg-clean`, `patch`,
//! `author-script`, `apply-opt`, `threeway`, `quoted-cr` and the numbered split
//! messages — not merely on whether the directory exists.
//!
//! # `am` and `rebase --apply` share this state, so a finding here is often both
//!
//! `builtin/am.c` writes `.git/rebase-apply/` and `git rebase --apply` drives
//! the *same* directory through the same engine — `next`/`last`/`patch`/
//! `author-script`/`apply-opt` are written by one implementation reached through
//! two doors. Concretely, a divergence in any of the following is a defect in
//! `corpus/rebase_engine.rs`'s territory as much as in this module's, and
//! whichever module reports it first is reporting one bug:
//!
//!  * the layout and byte content of `.git/rebase-apply/` (this module's
//!    mid-series and 3-way-conflict cases pin it; a `rebase --apply` that stops
//!    reads the same files back);
//!  * `--continue`/`--skip`/`--abort` resumption, including the `abort-safety`
//!    guard — `am --abort` and `rebase --abort` consult the same file;
//!  * everything `apply-opt` carries (`-C<n>`, `-p<n>`, `--whitespace=`,
//!    `--directory=`, `--include=`/`--exclude=`, `--reject`), because
//!    `rebase --apply` re-reads that file to re-run `git apply` on resumption.
//!
//! What is *not* shared: mailbox parsing (`mailinfo`/`mailsplit`), `--scissors`,
//! the transfer encodings, `--patch-format=`, `--message-id`, `--quoted-cr=` and
//! `-i`. `rebase` synthesises its own mail with `format-patch`, so those are
//! `am`-only and belong here alone.
//!
//! # How this divides territory with the eight modules that already run `am`
//!
//! All eight were read before a case was written here. What each owns:
//!
//! | module | what it owns for `am` |
//! |---|---|
//! | `corpus/mail_series.rs` | the nearest neighbour: one mailbox on stdin per *content* form — scissors, in-body headers, encoded words, base64, quoted-printable, multipart, latin-1, CRLF, mboxrd, StGit, hg, empty, junk — against `Shape::Linear`, plus `am.*`/`apply.whitespace`/`i18n.commitEncoding`/`core.autocrlf` |
//! | `corpus/mail_patch.rs` | `am` fed a *file that is not a mailbox* (`README.md`, `/dev/null`, a missing path), and the control verbs with **no session** (`--abort`, `--skip`, `--continue`, `--quit`, `--show-current-patch`, `--show-current-patch=diff`) |
//! | `corpus/sequences.rs` | every **multi-step** `am` session: stop→skip, stop→abort, stop→show→abort, 3-way conflict→`add`→`--continue`, abort-then-refuse-resumption, the `--empty` triple on `Shape::Cherry`, and the three `applypatch` hooks on `Shape::AmHooks` |
//! | `corpus/shape_reach.rs` | `am` on `Shape::Patches`'s own files (`mail/one.eml`, `mail/series.mbox`) as a flag sweep |
//! | `corpus/fixture_gaps3.rs` | `am` on `Shape::AmHooks`'s files, with and without `--no-verify` |
//! | `corpus/exit_codes.rs` | the exit code of `am --show-current-patch` on `Shape::Patches` |
//! | `corpus/misc_commands.rs` | `am --bogus` argument rejection, and `am` in the verb-dispatch list |
//! | `corpus/stateful_side_files.rs` | nothing for `am` (it appears only in prose there) |
//!
//! **What none of them has, and what is therefore here.** Verified by grepping
//! all eight for `"am"` before writing:
//!
//!  * `--quoted-cr=nowarn|warn|strip` — *no case anywhere sets this flag*, and
//!    the three values are a three-way split (two differ only on stderr, the
//!    third changes the committed tree). It also has a state file,
//!    `rebase-apply/quoted-cr`, that nothing was reading back.
//!  * `-C<n>` — *no case anywhere sets it*. `-C0` turns a refusal into a commit.
//!  * `--no-3way`, `--no-keep-cr`, `--no-scissors` **against a config that turns
//!    the feature on** — the negations were only ever measured against the
//!    default, where they are no-ops.
//!  * `-i`/`--interactive` — *no case anywhere*. See the section below: it is
//!    fully reachable under the pinned `GIT_EDITOR=true`.
//!  * `--show-current-patch=raw`, `-r`, `--resolved`, `--rerere-autoupdate`,
//!    `--patch-format=stgit-series`, `--empty=stop`, `--empty=<invalid>`,
//!    `--show-current-patch=<invalid>`, and two verbs at once.
//!  * a **mid-series stop** and a **3-way conflict park** as *single*
//!    invocations, so `.git/rebase-apply/`'s full contents are one case's
//!    post-state rather than a step inside a sequence.
//!  * mailbox forms no payload in the corpus has: a scissors line with no
//!    spaces (which is *not* a scissors line), `>>From` double mboxrd escaping,
//!    quoted CR (`=0D`) at every line end, a `GIT binary patch` — both
//!    well-formed and with its terminating blank line removed — a patch already
//!    in the tree, an `index` line naming a blob no repository holds, and a
//!    patch emptied by `--include`/`--exclude` rather than by its sender.
//!  * `am` on `Shape::Conflicted` (an unmerged index), on `Shape::Branched`
//!    (where a patch can be already-applied or 3-way-conflicting), and run from
//!    a **subdirectory**, which is where `--directory=` reveals that it is
//!    rooted at the worktree top and not at the cwd.
//!
//! No case below repeats a (shape, argv, stdin) triple from any of the eight.
//!
//! # Payloads are `&'static [u8]` literals
//!
//! Nothing is read off the filesystem, so a case is reproducible from its id.
//! Every mailbox below is typed out here, and three details are load-bearing and
//! must survive editing this file:
//!
//!  * **[`MB_QCR`]'s `=0D` line endings.** They are the *quoted* CR
//!    `--quoted-cr` acts on. Rewriting them as real CRs changes which flag is
//!    being measured.
//!  * **[`MB_SCISSORS_TIGHT`] has no spaces around `8<` and
//!    [`MB_SCISSORS_SPACED`] does.** Measured against stock 2.55.0: `-- 8< --`
//!    and `-- >8 --` cut, while `--8<--`, `--%<--` and `-->8--` do *not*, so the
//!    pair is the only thing in the corpus that separates "is a scissors line"
//!    from "looks like one".
//!  * **Two blob ids are facts about the fixtures, not about a build.**
//!    `46e89a2` is `hash-object` of `pub fn one() -> u32 { 1 }\n` (`src/lib.rs`
//!    in every shape's initial commit) and `9741694` is `# fixture\n`
//!    (`README.md`). Only `--3way` reads them. [`MB_3WAY`] and [`MB_APPLIED`]
//!    name `46e89a2` and are aimed at [`Shape::Branched`], whose `main` has
//!    *moved* `src/lib.rs` past that blob — that gap is what makes one of them
//!    3-way-conflict and the other already-applied. If `fixture.rs` changes
//!    either file, both cases quietly stop testing what they say they test.
//!
//! # `--ignore-date` is not measurable on any mailbox that commits
//!
//! `--ignore-date` replaces the author date with `time(NULL)`; `env::harden`
//! pins `GIT_AUTHOR_DATE`, and `am --ignore-date` overrides the pin. Measured on
//! stock alone, twice, 1.5s apart, in fresh copies of [`Shape::Linear`]:
//!
//! ```text
//! git am --ignore-date < one-message.mbox
//!   run 1: HEAD = 987589add6780584dfe1c640d5ef76fc0c64c4c5
//!   run 2: HEAD = ad336f0b85673bf1d0337e1e66d8db0002f730ed
//! ```
//!
//! So no case here passes `--ignore-date` to a mailbox that commits — including
//! a *multi*-message mailbox whose later patch fails, because the earlier one
//! has already committed and `.git/rebase-apply/abort-safety` records its id
//! (the same two runs against [`MB_THREE`] left two different `abort-safety`
//! values). The one form that *is* deterministic is a mailbox whose **first**
//! patch fails: nothing commits, and the author date reaches
//! `rebase-apply/author-script` verbatim from the `Date:` header rather than
//! from the clock. Verified: two runs 1.5s apart left byte-identical
//! `.git/rebase-apply/` trees. That form is shipped; the rest is recorded here
//! rather than shipped, in the same terms `corpus/rebase_engine.rs` records it
//! for `rebase --ignore-date`.
//!
//! # What `GIT_EDITOR=true` leaves reachable for `-i`
//!
//! `am -i` does **not** open an editor to ask its question — it prints the
//! message and prompts on stdout, and reads the answer from **stdin**. That
//! makes the whole interactive loop reachable from this harness, because stdin
//! is exactly what a case controls. Measured against stock 2.55.0 on a
//! `Shape::Patches`-equivalent fixture:
//!
//!  * `am -i` with a **mailbox on stdin** cannot work at all —
//!    `fatal: interactive mode requires patches on the command line`, exit 128 —
//!    so every `-i` case below names a file and carries the *answers* on stdin.
//!  * **stdin closed** prints the prompt and then
//!    `fatal: unable to read from stdin; aborting` at exit 128, leaving a
//!    populated `.git/rebase-apply/` behind. That is the only `-i` case here
//!    with no stdin payload, and its post-state is the point.
//!  * `y` applies, `n` skips (and leaves *no* session), `a` accepts the rest of
//!    the mailbox without asking again, `v` prints the patch to stdout and asks
//!    again, and `e` — the one answer that *does* run `$GIT_EDITOR` — re-asks
//!    with the message unchanged, because `true` accepts the buffer as it
//!    stands. Any other byte (`q`, `x`) is also a re-ask. All six are pinned.
//!
//! The editor is therefore not a wall for `-i`; it is one of the branches, and
//! `true` is what makes that branch deterministic.
//!
//! # How the payloads were verified before shipping
//!
//! Every case that commits or parks state was replayed against **stock alone,
//! twice, two seconds apart**, in freshly built fixtures, and the two runs were
//! compared on `rev-parse HEAD`, `for-each-ref`, `status --porcelain -uall`,
//! `ls-files --stage` and the escaped bytes of every file under
//! `.git/rebase-apply/`:
//!
//! ```text
//! 83 stdin cases (Linear/Branched/Dirty/Conflicted/Detached), 203 rebase-apply files: identical
//! 12 config- and cwd-scoped cases:                                                  identical
//! 13 `-i` cases (Patches):                                                          identical
//!  2 Worktree cases:                                                                identical
//! ```
//!
//! The sweep is not vacuous: replacing one case's flags with `--ignore-date`
//! made the same comparison report two different `HEAD`s, which is the positive
//! control for the whole check and the evidence behind the section above.
//!
//! The parked sessions were checked a second way, because "the bytes match" is
//! weaker than "the state works". For each parking case, `.git/rebase-apply/`
//! was produced by the **binary under test** and then handed to **stock's own**
//! `am --abort`, `am --skip` and `am --continue`; the resulting refs, index,
//! status and leftover state were compared against the same three recoveries
//! driven over a session stock had parked itself. They were identical, so
//! nothing this module measures leaves a session stock cannot resume.
//!
//! # What is not measurable here, and why
//!
//!  * **`--ignore-date` on anything that commits** — see above.
//!  * **`--gpg-sign`** — a signature needs a key the fixture has no way to hold,
//!    and a signed commit id is a function of the signing time.
//!  * **`--interactive` beyond the six answers above** — the prompt is read one
//!    byte at a time from stdin, so a case can drive it, but nothing here can
//!    make the *editor* rewrite the message: `harden` pins `GIT_EDITOR=true`,
//!    and re-pointing it is forbidden by `env::is_pinned` for the reason
//!    `corpus/shape_reach.rs` gives.
//!  * **`--unsafe-paths`-style escapes** — `am` has no such flag, and the
//!    `apply` half is already excluded by `corpus/mail_series.rs` for leaking
//!    outside the fixture.

use crate::fixture::Shape;
use crate::runner::Case;

// ---------------------------------------------------------------------------
// Mailbox payloads
// ---------------------------------------------------------------------------
//
// `Date:` is 1700000000 throughout, the instant `env::FIXED_DATE` pins, so a
// commit `am` builds here has an author date equal to its committer date and a
// reproducible id. Subjects are distinct from every payload in
// `corpus/mail_series.rs` so no case id can collide with one of its.

/// The floor: one message that applies cleanly to `README.md` in every shape
/// whose initial commit is untouched. Used as the carrier for flags whose effect
/// is visible without a refusal.
const MB_BASE: &[u8] = b"From bbbb010101010101010101010101010101010101 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: a message that applies

Body of the base message.
---
 README.md | 1 +
 1 file changed, 1 insertion(+)

diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+am-deep
";

/// One message whose context is not in the pre-image, so the **first** patch
/// fails and nothing commits. The only carrier `--ignore-date` is deterministic
/// on; see the module header.
const MB_FAIL: &[u8] = b"From bbbb020202020202020202020202020202020202 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: the first patch cannot apply

Body.
---
diff --git a/src/lib.rs b/src/lib.rs
index 46e89a2..7d4c2b1 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,3 @@
 pub fn one() -> u32 { 1 }
-a line the pre-image does not have
+replacement
+added
";

/// Three messages where the **middle** one fails: `am` commits 1/3, stops at
/// 2/3, and parks all three split messages plus `next`=2 / `last`=3 under
/// `.git/rebase-apply/`. One invocation, and the whole state machine's
/// mid-mailbox resting position is its post-state.
///
/// Verified against stock 2.55.0 on [`Shape::Linear`]: exit 128,
/// `Patch failed at 0002`, `git log` shows `am-deep: 1/3 lands` on top of
/// `initial`, and `.git/rebase-apply/` holds `0001`, `0002`, `0003`,
/// `abort-safety`, `apply-opt`, `applying`, `author-script`, `final-commit`,
/// `info`, `keep`, `last`, `messageid`, `msg`, `next`, `patch`, `quiet`,
/// `quoted-cr`, `scissors`, `sign`, `threeway`, `utf8`.
const MB_THREE: &[u8] = b"From bbbb030303030303030303030303030303030303 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH 1/3] am-deep: 1/3 lands

Body one.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+am-deep

From bbbb040404040404040404040404040404040404 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH 2/3] am-deep: 2/3 cannot apply

Body two.
---
diff --git a/src/lib.rs b/src/lib.rs
index 46e89a2..7d4c2b1 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,3 @@
 pub fn one() -> u32 { 1 }
-a line the pre-image does not have
+replacement
+added

From bbbb050505050505050505050505050505050505 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH 3/3] am-deep: 3/3 would land

Body three.
---
diff --git a/src/lib.rs b/src/lib.rs
index 46e89a2..7d4c2b1 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1,2 @@
 pub fn one() -> u32 { 1 }
+pub fn two() -> u32 { 2 }
";

/// Rewrites the one line of the *initial* `src/lib.rs` and adds another, with
/// `46e89a2` — a blob every fixture holds — on the `index` line.
///
/// Aimed at [`Shape::Branched`], whose `main` already added a second line to
/// that file. Plain `am` refuses on context; `--3way` reconstructs the base from
/// `46e89a2`, merges, and **conflicts**, leaving `UU src/lib.rs`, conflict
/// markers in the worktree, unmerged index stages, and a
/// `.git/rebase-apply/patch-merge-index` that no other case in the corpus
/// produces. Verified deterministic on stock: two runs 1.5s apart left
/// byte-identical `.git/rebase-apply/` trees, and `patch-merge-index` carries
/// zeroed stat data.
const MB_3WAY: &[u8] = b"From bbbb060606060606060606060606060606060606 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: rewrite the only line and add another

Body.
---
diff --git a/src/lib.rs b/src/lib.rs
index 46e89a2..b8c1e94 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1,2 @@
-pub fn one() -> u32 { 1 }
+pub fn one() -> u32 { 100 }
+pub fn hundred() -> u32 { 100 }
";

/// The change [`Shape::Branched`]'s `main` **already carries**. Plain `am`
/// refuses on context and parks a session; `--3way` reaches
/// `No changes -- Patch already applied.` at exit 0 with nothing committed and
/// no session left. The two halves are the point of having the payload.
const MB_APPLIED: &[u8] = b"From bbbb070707070707070707070707070707070707 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: a change the tree already carries

Body.
---
diff --git a/src/lib.rs b/src/lib.rs
index 46e89a2..7d4c2b1 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1,2 @@
 pub fn one() -> u32 { 1 }
+pub fn two() -> u32 { 2 }
";

/// `index` naming blobs no repository holds. Under `--3way` the fake ancestor
/// cannot be built at all — `error: sha1 information is lacking or useless` then
/// `error: could not build fake ancestor` — which is a different failure from
/// [`MB_3WAY`]'s merge conflict and from the plain context refusal.
const MB_FAKEIDX: &[u8] = b"From bbbb080808080808080808080808080808080808 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: index line naming a blob no repository has

Body.
---
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,3 @@
 pub fn one() -> u32 { 1 }
-a line the pre-image does not have
+replacement
+added
";

/// Quoted-printable with `=0D` — a **quoted** CR — at the end of every body
/// line, including every line of the diff. This is what `--quoted-cr` acts on,
/// and nothing else in the corpus carries one.
///
/// Verified against stock 2.55.0 on [`Shape::Linear`], and the three values are
/// a genuine three-way split:
///
/// ```text
/// am                      warning: quoted CRLF detected   + refusal, exit 128
/// am --quoted-cr=warn     warning: quoted CRLF detected   + refusal, exit 128
/// am --quoted-cr=nowarn   (no warning)                    + refusal, exit 128
/// am --quoted-cr=strip    (nothing)                       + commit,  exit 0
/// ```
///
/// So `warn` and `nowarn` differ **only on stderr**, which is why those two
/// cases are strict; and `strip` is the only one that moves `rev-parse HEAD`.
const MB_QCR: &[u8] = b"From bbbb090909090909090909090909090909090909 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: quoted CR at the end of every line
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8
Content-Transfer-Encoding: quoted-printable

Body explaining the change.=0D
---=0D
diff --git a/README.md b/README.md=0D
index 9741694..1314fa4 100644=0D
--- a/README.md=0D
+++ b/README.md=0D
@@ -1 +1,2 @@=0D
 # fixture=0D
+am-deep=0D
";

/// A hunk promising three context lines where [`Shape::Branched`]'s `main` has
/// only two. The carrier for `-C<n>`, which nothing in the corpus sets.
///
/// Verified against stock 2.55.0 on [`Shape::Branched`]: the default, `-C1` and
/// `-C3` all refuse with `patch does not apply`, while `-C0` **commits** —
/// printing `Context reduced to (0/0) to apply fragment at 3` on stderr and
/// leaving a `src/lib.rs` no other case in the corpus produces. A port that
/// parses `-C<n>` and ignores it passes three of those four and fails the one
/// that matters, which is why all four are cases.
const MB_CTX: &[u8] = b"From bbbb0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: three context lines, only two present

Body.
---
diff --git a/src/lib.rs b/src/lib.rs
index 46e89a2..7d4c2b1 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
 pub fn one() -> u32 { 1 }
 pub fn two() -> u32 { 2 }
 pub fn three() -> u32 { 3 }
+pub fn four() -> u32 { 4 }
";

/// Creates one file and touches nothing existing — the only shape of change in
/// which `--directory=`, `-p0`, `--include=` and `--exclude=` all *succeed* and
/// each leaves a **different tree**. Verified on [`Shape::Linear`]:
///
/// ```text
/// am --directory=sub     -> sub/added.txt
/// am -p0                 -> b/added.txt
/// am --include=nope.txt  -> no new path (an empty commit)
/// am --exclude=added.txt -> no new path (an empty commit)
/// ```
const MB_CREATE: &[u8] = b"From bbbb0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: create one new file

Body.
---
diff --git a/added.txt b/added.txt
new file mode 100644
index 0000000..420b1cb
--- /dev/null
+++ b/added.txt
@@ -0,0 +1 @@
+newly added
";

/// A `GIT binary patch` carried through `am`, base85 as `format-patch --binary`
/// wrote it. The literal was produced by stock 2.55.0 from a 23-byte file
/// holding `\x00\x01\x02\x03\xfd\xfe\xff\x00binary fixture\n`, and both the
/// forward (`literal 23`) and reverse (`literal 0`) halves are present because
/// `--3way` needs the reverse half to reconstruct.
///
/// **The trailing blank line is load-bearing.** A base85 block is terminated by
/// an empty line, and dropping it turns this payload into
/// [`MB_BINARY_TRUNCATED`] — which stock refuses. The two differ by exactly one
/// byte and that is the point of shipping both.
///
/// No mailbox anywhere else in the corpus is binary on stdin: `Shape::Patches`'s
/// `mail/series.mbox` is, but it is only ever passed as a *file*, and `apply`'s
/// binary payload in `corpus/mail_series.rs` never reaches `am`'s committing
/// path.
const MB_BINARY: &[u8] = b"From bbbb0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: add a binary file

---
 blob.bin | Bin 0 -> 23 bytes
 1 file changed, 0 insertions(+), 0 deletions(-)
 create mode 100644 blob.bin

diff --git a/blob.bin b/blob.bin
new file mode 100644
index 0000000000000000000000000000000000000000..d135df1f08c6f9a3d01c94fde2bd5d7ea6e7d3a5
GIT binary patch
literal 23
ecmZQzWMcmN?>|FQW?o`Zr9xU}MM-H<Di;7{qzJbF

literal 0
HcmV?d00001

";

/// [`MB_BINARY`] with the terminating blank line removed, so the reverse base85
/// block runs off the end of the message.
///
/// Stock 2.55.0 refuses it — `error: corrupt binary patch at
/// .git/rebase-apply/patch:15:` then `error: No valid patches in input (allow
/// with "--allow-empty")`, exit 128, session parked. Written as its own payload
/// because an unterminated base85 block is the one binary-patch malformation a
/// reader is most likely to introduce by hand, and because the port disagrees
/// about it (see this module's report).
const MB_BINARY_TRUNCATED: &[u8] = b"From bbbb0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: add a binary file

---
 blob.bin | Bin 0 -> 23 bytes
 1 file changed, 0 insertions(+), 0 deletions(-)
 create mode 100644 blob.bin

diff --git a/blob.bin b/blob.bin
new file mode 100644
index 0000000000000000000000000000000000000000..d135df1f08c6f9a3d01c94fde2bd5d7ea6e7d3a5
GIT binary patch
literal 23
ecmZQzWMcmN?>|FQW?o`Zr9xU}MM-H<Di;7{qzJbF

literal 0
HcmV?d00001
";

const MB_SCISSORS_TIGHT: &[u8] = b"From bbbb0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: envelope subject above a tight perforation

chatter

--8<--
Subject: am-deep: subject below a tight perforation

Body below the perforation.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+am-deep
";

/// The same message with `-- 8< --` instead, which **is** a scissors line —
/// the `8<` spelling rather than `corpus/mail_series.rs`'s `>8`. Under
/// `--scissors` or `mailinfo.scissors=true` the commit subject is the one below
/// the line; under `--no-scissors` it is the envelope's.
const MB_SCISSORS_SPACED: &[u8] = b"From bbbb0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: envelope subject above a spaced perforation

chatter

-- 8< --
Subject: am-deep: subject below a spaced perforation

Body below the perforation.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+am-deep
";

/// mboxrd escaping at **two** levels: `>From` and `>>From`. Unescaping strips
/// exactly one `>` from each, so the commit message differs from the plain-mbox
/// reading by two bytes and `rev-parse HEAD` separates them. Verified on stock:
/// default `am` and `am --patch-format=mboxrd` produce two different commit ids.
/// `corpus/mail_series.rs`'s mboxrd payload has a single level only, which a
/// port that strips *all* leading `>` would pass.
const MB_MBOXRD2: &[u8] = b"From bbbb0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: mboxrd double escaping

>From one level of escaping.
>>From two levels of escaping.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+am-deep
";

/// Already carries a `Signed-off-by:` naming the identity `env::harden` pins,
/// so `--signoff` must **not** add a second one. A port that appends
/// unconditionally commits a different message and a different id while
/// printing the same `Applying:` line.
const MB_SIGNED: &[u8] = b"From bbbb101010101010101010101010101010101010 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: already carries the exact sign-off

Body.

Signed-off-by: zvcs parity <parity@example.invalid>
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+am-deep
";

/// An in-body `Date:` that differs from the envelope's, so
/// `--committer-date-is-author-date` has an author date to copy that is neither
/// the envelope's nor `env::FIXED_DATE` — and a port that copies the wrong one
/// of the three still prints the same line.
const MB_INBODY_DATE: &[u8] = b"From bbbb111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001
From: Envelope Author <envelope@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] am-deep: envelope date

From: In Body <inbody@example.invalid>
Date: Sat, 01 Jan 2022 03:04:05 +0000
Subject: am-deep: in-body date wins

Body after the in-body header block.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+am-deep
";

// ---------------------------------------------------------------------------
// `-i` answer scripts
// ---------------------------------------------------------------------------
//
// Answers to `am -i`'s `Apply? [y]es/[n]o/[e]dit/[v]iew patch/[a]ccept all:`
// prompt, one byte and a newline each. The trailing `y` in the multi-answer
// scripts is what the loop consumes after a re-ask, so the case ends in a
// commit rather than at end of input.

/// Apply the one patch.
const ANS_YES: &[u8] = b"y\n";
/// Refuse it: `am` skips and leaves **no** `.git/rebase-apply/` behind.
const ANS_NO: &[u8] = b"n\n";
/// Accept this one and every later one without asking again.
const ANS_ALL: &[u8] = b"a\n";
/// Print the patch to stdout, then apply — the prompt is asked twice.
const ANS_VIEW: &[u8] = b"v\ny\n";
/// Run `$GIT_EDITOR` on the message, then apply. `harden` pins the editor to
/// `true`, so the message comes back unchanged and the prompt repeats.
const ANS_EDIT: &[u8] = b"e\ny\n";
/// An answer the prompt does not define: re-asked, then applied.
const ANS_UNKNOWN: &[u8] = b"x\ny\n";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One stdin-fed case.
fn si(args: &[&str], shape: Shape, input: &'static [u8], out: &mut Vec<Case>) {
    out.push(Case::with_stdin("am", args, shape, input));
}

/// One stdin-fed case with stderr compared byte for byte.
///
/// Used wherever the diagnostic *is* the behaviour: `--quoted-cr=warn` and
/// `--quoted-cr=nowarn` differ in nothing else at all, `-C0`'s
/// `Context reduced to` is the only announcement that the flag did anything, and
/// for a refusal the message is what a maintainer reads.
fn sx(args: &[&str], shape: Shape, input: &'static [u8], out: &mut Vec<Case>) {
    out.push(Case { compare_stderr: true, ..Case::with_stdin("am", args, shape, input) });
}

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    quoted_cr(out);
    context_lines(out);
    negations_against_config(out);
    interactive(out);
    state_machine_in_one_invocation(out);
    verbs_without_a_session(out);
    mailbox_forms(out);
    apply_options_through_am(out);
    hostile_trees_and_directories(out);
}

// ---------------------------------------------------------------------------
// --quoted-cr
// ---------------------------------------------------------------------------

/// `--quoted-cr=<action>` and the `am.quotedCr` key behind it.
///
/// Unreachable before this module: no case in the corpus passes the flag, no
/// payload carries a quoted CR, and `.git/rebase-apply/quoted-cr` — which `am`
/// writes on every run and `--continue` reads back — was never compared against
/// a value other than the default.
fn quoted_cr(out: &mut Vec<Case>) {
    // `warn` (the default) and `nowarn` differ only in one line of stderr.
    sx(&["am"], Shape::Linear, MB_QCR, out);
    sx(&["am", "--quoted-cr=warn"], Shape::Linear, MB_QCR, out);
    sx(&["am", "--quoted-cr=nowarn"], Shape::Linear, MB_QCR, out);
    // `strip` is the one value that commits, so it is the one that moves HEAD.
    si(&["am", "--quoted-cr=strip"], Shape::Linear, MB_QCR, out);
    // Rejected before any input is read.
    sx(&["am", "--quoted-cr=bogus"], Shape::Linear, MB_QCR, out);
    // Delivered from the repository file rather than the command line, so a
    // port that reads the flag and not the key diverges.
    out.push(
        Case::with_stdin("am", &["am"], Shape::Linear, MB_QCR)
            .with_config(&[("am.quotedCr", "strip")]),
    );
    // The flag must beat the key.
    out.push(
        Case { compare_stderr: true, ..Case::with_stdin("am", &["am", "--quoted-cr=nowarn"], Shape::Linear, MB_QCR) }
            .with_config(&[("am.quotedCr", "strip")]),
    );
}

// ---------------------------------------------------------------------------
// -C<n>
// ---------------------------------------------------------------------------

/// `-C<n>`: how many context lines have to match.
///
/// Set by no case anywhere in the corpus before this. On [`Shape::Branched`] the
/// four values split three-to-one, and the one that succeeds writes a file no
/// other case writes — so a port that accepts the flag and drops it agrees on
/// the three refusals and is caught only here.
fn context_lines(out: &mut Vec<Case>) {
    sx(&["am"], Shape::Branched, MB_CTX, out);
    sx(&["am", "-C3"], Shape::Branched, MB_CTX, out);
    sx(&["am", "-C1"], Shape::Branched, MB_CTX, out);
    // Succeeds, and announces the reduction on stderr.
    sx(&["am", "-C0"], Shape::Branched, MB_CTX, out);
    // The same flag over a hunk that needs no reduction: a no-op that must stay
    // one, so a port that reduces context unconditionally is separated from one
    // that reduces it only when the strict match fails.
    si(&["am", "-C0"], Shape::Linear, MB_BASE, out);
    // `-C` with no number, and with a value that is not one.
    sx(&["am", "-C"], Shape::Linear, MB_BASE, out);
    sx(&["am", "-Cx"], Shape::Linear, MB_BASE, out);
}

// ---------------------------------------------------------------------------
// Negations, measured against a configuration that turns the feature on
// ---------------------------------------------------------------------------

/// `--no-3way`, `--no-keep-cr`, `--no-scissors`, `--no-utf8`, `--no-verify`.
///
/// Every one of these already appears somewhere in the corpus, and every
/// appearance measures it against the **default**, where it is a no-op: the
/// feature was off, the negation turned it off again, and a port that ignores
/// the flag entirely matched. Pairing each negation with the `am.*` key that
/// turns its feature on is what makes it observable.
fn negations_against_config(out: &mut Vec<Case>) {
    // am.threeWay=true, then --no-3way: on `Branched` the difference is a
    // merge conflict (three-way on) versus a flat context refusal (off), which
    // is visible in `.git/rebase-apply/threeway`, in the index stages and in
    // the worktree.
    out.push(
        Case { compare_stderr: true, ..Case::with_stdin("am", &["am"], Shape::Branched, MB_3WAY) }
            .with_config(&[("am.threeWay", "true")]),
    );
    out.push(
        Case { compare_stderr: true, ..Case::with_stdin("am", &["am", "--no-3way"], Shape::Branched, MB_3WAY) }
            .with_config(&[("am.threeWay", "true")]),
    );
    // am.keepcr=true, then --no-keep-cr: with the key alone the CRs reach
    // `apply` and the patch is refused; the negation strips them and it lands.
    out.push(
        Case::with_stdin("am", &["am", "--no-keep-cr"], Shape::Linear, MB_QCR)
            .with_config(&[("am.keepcr", "true")]),
    );
    // mailinfo.scissors=true, then --no-scissors, on the payload that *is* a
    // scissors line. Three outcomes, two commit messages.
    out.push(
        Case::with_stdin("am", &["am"], Shape::Linear, MB_SCISSORS_SPACED)
            .with_config(&[("mailinfo.scissors", "true")]),
    );
    out.push(
        Case::with_stdin("am", &["am", "--no-scissors"], Shape::Linear, MB_SCISSORS_SPACED)
            .with_config(&[("mailinfo.scissors", "true")]),
    );
    // …and on the payload that only looks like one, where the key must change
    // nothing at all.
    out.push(
        Case::with_stdin("am", &["am"], Shape::Linear, MB_SCISSORS_TIGHT)
            .with_config(&[("mailinfo.scissors", "true")]),
    );
    // The short spellings, which no case uses: -3, -k, -s, -u, -b, -c.
    si(&["am", "-3"], Shape::Branched, MB_APPLIED, out);
    si(&["am", "-k"], Shape::Linear, MB_BASE, out);
    si(&["am", "-s"], Shape::Linear, MB_BASE, out);
    si(&["am", "-u"], Shape::Linear, MB_BASE, out);
    si(&["am", "-c"], Shape::Linear, MB_SCISSORS_SPACED, out);
    si(&["am", "-b"], Shape::Linear, MB_BASE, out);
    // --signoff over a message that already ends in exactly that trailer.
    si(&["am", "--signoff"], Shape::Linear, MB_SIGNED, out);
    si(&["am", "-s", "-k"], Shape::Linear, MB_SIGNED, out);
    // --no-verify on stdin; every existing --no-verify case passes a file.
    si(&["am", "--no-verify"], Shape::Linear, MB_BASE, out);
}

// ---------------------------------------------------------------------------
// -i / --interactive
// ---------------------------------------------------------------------------

/// `am -i`, driven by answers on stdin.
///
/// Reached by no case in the corpus. The module header records what the pinned
/// `GIT_EDITOR=true` leaves reachable; the short version is that `-i` prompts on
/// **stdin**, not through the editor, so every branch of the loop is a case.
fn interactive(out: &mut Vec<Case>) {
    // A mailbox on stdin cannot be interactive at all: `-i` needs a file, and
    // stdin is where the answers go.
    sx(&["am", "-i"], Shape::Linear, MB_BASE, out);
    // Stdin closed: the prompt is printed, the read fails, and a populated
    // `.git/rebase-apply/` is left behind — which is the post-state being
    // measured, and the only `-i` case here with no stdin.
    out.push(Case::strict("am", &["am", "-i", "mail/one.eml"], Shape::Patches));
    // The six answers.
    si(&["am", "-i", "mail/one.eml"], Shape::Patches, ANS_YES, out);
    si(&["am", "-i", "mail/one.eml"], Shape::Patches, ANS_NO, out);
    si(&["am", "-i", "mail/one.eml"], Shape::Patches, ANS_VIEW, out);
    si(&["am", "-i", "mail/one.eml"], Shape::Patches, ANS_EDIT, out);
    si(&["am", "-i", "mail/one.eml"], Shape::Patches, ANS_UNKNOWN, out);
    // `a` on a two-message mailbox: the second is applied without a prompt, so
    // a port that asks again produces the same tree and the wrong stdout.
    si(&["am", "-i", "mail/series.mbox"], Shape::Patches, ANS_ALL, out);
    // `y` on the same mailbox answers only the first; the second is asked and
    // the answer script has run out, so the session stops half-way.
    sx(&["am", "-i", "mail/series.mbox"], Shape::Patches, ANS_YES, out);
    // `n` on the same mailbox skips the first and stops at the second.
    sx(&["am", "-i", "mail/series.mbox"], Shape::Patches, ANS_NO, out);
    // `-i` composed with the flags that change what the prompt prints.
    si(&["am", "-i", "--keep", "mail/one.eml"], Shape::Patches, ANS_YES, out);
    si(&["am", "-i", "--signoff", "mail/one.eml"], Shape::Patches, ANS_YES, out);
    si(&["am", "--interactive", "mail/one.eml"], Shape::Patches, ANS_YES, out);
    // `-i` with `--quiet`, which suppresses `Applying:` but not the prompt.
    si(&["am", "-i", "--quiet", "mail/one.eml"], Shape::Patches, ANS_YES, out);
}

// ---------------------------------------------------------------------------
// The state machine, as single invocations
// ---------------------------------------------------------------------------

/// One `am` that stops, with `.git/rebase-apply/` as its post-state.
///
/// `corpus/sequences.rs` owns multi-step `am` sessions. What it does not have is
/// a **single** invocation whose whole result is the resting position of the
/// state machine, and those are cheaper to read when one fails: a sequence that
/// diverges at step 4 says nothing about which of steps 1-3 was already wrong.
///
/// `runner::probe_op_state` walks the directory and reports every file's bytes,
/// so each case below pins `next`, `last`, `msg`, `msg-clean`, `patch`,
/// `author-script`, `apply-opt`, `abort-safety`, `threeway`, `quoted-cr`,
/// `scissors`, `sign`, `utf8`, `keep`, `quiet`, `messageid`, `final-commit`,
/// `info`, `applying`, and the numbered split messages.
fn state_machine_in_one_invocation(out: &mut Vec<Case>) {
    // Stop in the middle of a three-message mailbox: 1/3 committed, 2/3 parked,
    // 3/3 still queued. `next`=2, `last`=3.
    sx(&["am"], Shape::Linear, MB_THREE, out);
    // The same mailbox under --3way, where 2/3 stops for a different reason and
    // `.git/rebase-apply/threeway` records that it was tried.
    sx(&["am", "--3way"], Shape::Linear, MB_THREE, out);
    // …and under --reject, which additionally leaves `src/lib.rs.rej` in the
    // worktree for the state probe to see as untracked.
    sx(&["am", "--3way", "--reject"], Shape::Linear, MB_THREE, out);
    sx(&["am", "--reject"], Shape::Linear, MB_THREE, out);
    // A real three-way *merge conflict*: unmerged index stages, conflict markers
    // in the worktree, and a `patch-merge-index` no other case produces.
    sx(&["am", "--3way"], Shape::Branched, MB_3WAY, out);
    sx(&["am"], Shape::Branched, MB_3WAY, out);
    // The fake-ancestor path failing outright, which is a third refusal.
    sx(&["am", "--3way"], Shape::Linear, MB_FAKEIDX, out);
    sx(&["am"], Shape::Linear, MB_FAKEIDX, out);
    // Already applied: --3way finishes at exit 0 with nothing committed and no
    // session; plain `am` parks one.
    sx(&["am", "--3way"], Shape::Branched, MB_APPLIED, out);
    sx(&["am"], Shape::Branched, MB_APPLIED, out);
    // `--ignore-date` is only deterministic where nothing commits; see the
    // module header for the two stock runs that establish it.
    sx(&["am", "--ignore-date"], Shape::Linear, MB_FAIL, out);
    sx(&["am", "--ignore-date", "--3way"], Shape::Linear, MB_FAIL, out);
    // `--rerere-autoupdate` over a three-way conflict: rerere is off in the
    // fixture, so the flag must change nothing — including not creating
    // `.git/rr-cache`, which `probe_state` would see.
    sx(&["am", "--3way", "--rerere-autoupdate"], Shape::Branched, MB_3WAY, out);
    sx(&["am", "--3way", "--no-rerere-autoupdate"], Shape::Branched, MB_3WAY, out);
}

// ---------------------------------------------------------------------------
// Control verbs with no session
// ---------------------------------------------------------------------------

/// The resumption verbs `corpus/mail_patch.rs` does not spell, and the
/// combinations no module tries.
///
/// `mail_patch.rs` owns `--abort`, `--skip`, `--continue`, `--quit`,
/// `--show-current-patch` and `--show-current-patch=diff` on [`Shape::Linear`].
/// What is left is the two aliases, the third `--show-current-patch` value, the
/// invalid values, and the mutual-exclusion diagnostics — all of which are
/// argument-parsing behaviour a port can get wrong while every existing case
/// passes.
fn verbs_without_a_session(out: &mut Vec<Case>) {
    let mut v = |args: &[&str], shape| out.push(Case::strict("am", args, shape));
    // `-r` and `--resolved` are `--continue`'s aliases.
    v(&["am", "-r"], Shape::Linear);
    v(&["am", "--resolved"], Shape::Linear);
    // The one `--show-current-patch` value no case names.
    v(&["am", "--show-current-patch=raw"], Shape::Linear);
    v(&["am", "--show-current-patch=bogus"], Shape::Linear);
    v(&["am", "--empty=bogus"], Shape::Linear);
    v(&["am", "--whitespace=bogus"], Shape::Linear);
    // Two verbs at once: refused by the parser, not by the state machine.
    v(&["am", "--skip", "--abort"], Shape::Linear);
    v(&["am", "--abort", "--quit"], Shape::Linear);
    v(&["am", "--continue", "--skip"], Shape::Linear);
    v(&["am", "--show-current-patch", "--abort"], Shape::Linear);
    // A verb *and* a mailbox: `am` has to decide which one it is being asked to
    // do before it looks at the state directory.
    v(&["am", "--abort", "mail/one.eml"], Shape::Patches);
    v(&["am", "--continue", "mail/one.eml"], Shape::Patches);
    // The verbs against a repository whose *index* is unmerged from a merge,
    // not from an `am` — the state directory is absent but the tree is not
    // clean, and the two refusals are different.
    v(&["am", "--continue"], Shape::Conflicted);
    v(&["am", "--skip"], Shape::Conflicted);
    v(&["am", "--quit"], Shape::Conflicted);
    v(&["am", "--show-current-patch=raw"], Shape::Conflicted);
}

// ---------------------------------------------------------------------------
// Mailbox forms
// ---------------------------------------------------------------------------

/// Content forms no payload in the corpus carries.
fn mailbox_forms(out: &mut Vec<Case>) {
    // A perforation that is not a scissors line, under every spelling of the
    // option — all four must agree that nothing is cut.
    si(&["am"], Shape::Linear, MB_SCISSORS_TIGHT, out);
    si(&["am", "--scissors"], Shape::Linear, MB_SCISSORS_TIGHT, out);
    si(&["am", "--no-scissors"], Shape::Linear, MB_SCISSORS_TIGHT, out);
    // …and one that is, in the `8<` spelling rather than `>8`.
    si(&["am"], Shape::Linear, MB_SCISSORS_SPACED, out);
    si(&["am", "--scissors"], Shape::Linear, MB_SCISSORS_SPACED, out);
    si(&["am", "--no-scissors"], Shape::Linear, MB_SCISSORS_SPACED, out);
    si(&["am", "--scissors", "--keep"], Shape::Linear, MB_SCISSORS_SPACED, out);
    // Two levels of mboxrd escaping: exactly one `>` comes off each line.
    si(&["am"], Shape::Linear, MB_MBOXRD2, out);
    si(&["am", "--patch-format=mboxrd"], Shape::Linear, MB_MBOXRD2, out);
    si(&["am", "--patch-format=mbox"], Shape::Linear, MB_MBOXRD2, out);
    // A binary patch through `am`'s committing path.
    si(&["am"], Shape::Linear, MB_BINARY, out);
    si(&["am", "--3way"], Shape::Linear, MB_BINARY, out);
    si(&["am", "--reject"], Shape::Linear, MB_BINARY, out);
    si(&["am", "--keep-cr"], Shape::Linear, MB_BINARY, out);
    // The same patch with its terminating blank line gone: one byte shorter,
    // and a refusal rather than a commit.
    sx(&["am"], Shape::Linear, MB_BINARY_TRUNCATED, out);
    sx(&["am", "--3way"], Shape::Linear, MB_BINARY_TRUNCATED, out);
    sx(&["am", "--reject"], Shape::Linear, MB_BINARY_TRUNCATED, out);
    // An in-body Date that is neither the envelope's nor the pinned clock.
    si(&["am"], Shape::Linear, MB_INBODY_DATE, out);
    si(&["am", "--committer-date-is-author-date"], Shape::Linear, MB_INBODY_DATE, out);
    si(&["am", "--keep-non-patch"], Shape::Linear, MB_INBODY_DATE, out);
    // `--patch-format=stgit-series` fed a stream rather than a series file:
    // refused before anything is applied.
    sx(&["am", "--patch-format=stgit-series"], Shape::Linear, MB_BASE, out);
    // The format told to be something the input is not.
    sx(&["am", "--patch-format=hg"], Shape::Linear, MB_BASE, out);
    sx(&["am", "--patch-format=stgit"], Shape::Linear, MB_THREE, out);
    // `--empty=stop` spelled out, which is the default: a port that only
    // implements the two non-default policies passes the default and fails here.
    si(&["am", "--empty=stop"], Shape::Linear, MB_BASE, out);
}

// ---------------------------------------------------------------------------
// The `apply` options `am` forwards
// ---------------------------------------------------------------------------

/// `-p<n>`, `--directory=`, `--include=`, `--exclude=`, `--whitespace=`.
///
/// Each of these is written into `.git/rebase-apply/apply-opt` and re-read by
/// `--continue`, so a port that honours the flag on the first pass and forgets
/// to record it resumes with different options — which is the `rebase --apply`
/// defect this shares with `corpus/rebase_engine.rs`.
fn apply_options_through_am(out: &mut Vec<Case>) {
    // Four spellings, four different trees, all at exit 0.
    si(&["am", "--directory=sub"], Shape::Linear, MB_CREATE, out);
    si(&["am", "-p0"], Shape::Linear, MB_CREATE, out);
    si(&["am", "--include=nope.txt"], Shape::Linear, MB_CREATE, out);
    si(&["am", "--exclude=added.txt"], Shape::Linear, MB_CREATE, out);
    // Both filters at once, where `--include` narrows and `--exclude` then
    // removes what is left.
    si(&["am", "--include=added.txt", "--exclude=added.txt"], Shape::Linear, MB_CREATE, out);
    si(&["am", "--include=added.txt"], Shape::Linear, MB_CREATE, out);
    // A patch emptied by the filters rather than by its sender: `--empty` is
    // decided from the *mail*, so the policies must not fire and an empty
    // commit is made instead.
    si(&["am", "--exclude=README.md"], Shape::Linear, MB_BASE, out);
    si(&["am", "--empty=drop", "--exclude=README.md"], Shape::Linear, MB_BASE, out);
    si(&["am", "--empty=keep", "--exclude=README.md"], Shape::Linear, MB_BASE, out);
    // `-p` values that strip too much or nothing at all.
    sx(&["am", "-p3"], Shape::Linear, MB_CREATE, out);
    sx(&["am", "-p"], Shape::Linear, MB_CREATE, out);
    // The two `--whitespace` values no case names, over a patch with nothing to
    // fix, so the flag must be inert.
    si(&["am", "--whitespace=nowarn"], Shape::Linear, MB_BASE, out);
    si(&["am", "--whitespace=warn"], Shape::Linear, MB_BASE, out);
    si(&["am", "--whitespace=strip"], Shape::Linear, MB_BASE, out);
    sx(&["am", "--whitespace=error-all"], Shape::Linear, MB_BASE, out);
    // `--ignore-whitespace` and `-C0` together: two independently relaxing
    // options on a hunk that needs the second and not the first.
    sx(&["am", "--ignore-whitespace", "-C0"], Shape::Branched, MB_CTX, out);
    sx(&["am", "--ignore-whitespace"], Shape::Branched, MB_CTX, out);
}

// ---------------------------------------------------------------------------
// Trees and directories `am` has to refuse or resolve
// ---------------------------------------------------------------------------

/// Where `am` is run, and what it is run over.
fn hostile_trees_and_directories(out: &mut Vec<Case>) {
    // An unmerged index from a *merge*: `am` refuses before reading the mailbox,
    // printing the offending path on stdout and the reason on stderr.
    sx(&["am"], Shape::Conflicted, MB_BASE, out);
    sx(&["am", "--3way"], Shape::Conflicted, MB_BASE, out);
    // A dirty worktree with a staged change and a deletion.
    sx(&["am"], Shape::Dirty, MB_CREATE, out);
    sx(&["am"], Shape::Dirty, MB_THREE, out);
    // Detached HEAD: `am` commits, and the branch it must *not* move is the one
    // `for-each-ref` reports unchanged.
    si(&["am"], Shape::Detached, MB_BASE, out);
    sx(&["am"], Shape::Detached, MB_THREE, out);
    // Run from a subdirectory. `am` operates on the worktree top, so
    // `--directory=d` puts the file at `d/added.txt` and not at `src/d/added.txt`
    // — a port that resolves the option against the cwd writes a different tree
    // while printing the same line.
    out.push(Case::with_stdin("am", &["am"], Shape::Linear, MB_BASE).in_dir("src"));
    out.push(Case::with_stdin("am", &["am", "--directory=d"], Shape::Linear, MB_CREATE).in_dir("src"));
    out.push(Case::with_stdin("am", &["am", "-p0"], Shape::Linear, MB_CREATE).in_dir("src"));
    out.push(
        Case { compare_stderr: true, ..Case::with_stdin("am", &["am"], Shape::Linear, MB_THREE) }
            .in_dir("src"),
    );
    // A linked worktree: `.git` is a file, the state directory lives under
    // `.git/worktrees/wt`, and `probe_op_state` reads the *common* directory —
    // so this is the one shape where an `am` that parks its state in the wrong
    // place is visible.
    si(&["am"], Shape::Worktree, MB_BASE, out);
    sx(&["am"], Shape::Worktree, MB_THREE, out);
}
