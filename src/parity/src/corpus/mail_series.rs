//! Differential corpus cases for the **patch-series pipeline**: the path a
//! change takes when it leaves one repository as text and enters another.
//!
//! Every case here is compared against stock git 2.55.0 for stdout, exit code
//! and post-command repository state.
//!
//! ```text
//!   format-patch  ->  mbox  ->  mailsplit  ->  mailinfo  ->  apply  ->  commit
//!                                                   \____ am ____/
//! ```
//!
//! Each arrow is a parser, and each parser has its own notion of what its input
//! looks like. `mailsplit` decides where one message ends; `mailinfo` decides
//! which bytes are headers, which are commit message and which are patch;
//! `apply` decides which lines are hunks and where they land. A port can get any
//! one of them right and the next one wrong, so they are measured separately
//! rather than only end to end.
//!
//! # How this divides territory with `corpus/mail_patch.rs`
//!
//! `mail_patch.rs` was written when the runner spawned both sides with
//! `Stdio::null()` and no shape shipped a patch file. Its module header records
//! that limitation, and its cases are shaped by it: `apply` and `am` are fed a
//! *tracked file that is not a patch*, `mailinfo` runs only on empty input, and
//! `format-patch` is exercised as a flag matrix against [`Shape::Branched`].
//! Two things have since changed — `Case::with_stdin` and [`Shape::Patches`] —
//! and the split follows from that:
//!
//! | axis | owner |
//! |---|---|
//! | `format-patch` flag matrix on `Branched`, `format.*` config | `mail_patch.rs` |
//! | `format-patch` output *location* (`-o`, `--output`, precedence) | `corpus.rs` |
//! | `format-patch` on the shapes whose **content** shapes a patch | here |
//! | `apply`/`am` on non-patch input, control verbs with no session | `mail_patch.rs` |
//! | `apply`/`am` on the `Patches` shape's **files** | `corpus/shape_reach.rs` |
//! | `apply`/`am` on a **mailbox or patch delivered on stdin** | here |
//! | multi-step `am` sessions (stop, skip, abort, continue) | `corpus/sequences.rs` |
//! | `mailinfo`/`mailsplit` on empty input and file arguments | `mail_patch.rs` |
//! | `mailinfo`/`mailsplit` on real messages | here |
//! | `patch-id` on a single-commit diff, rename, binary, CRLF | `corpus/stdin_plumbing.rs` |
//! | `patch-id` on a multi-message mbox, combined diff, mode change | here |
//! | `interpret-trailers` with file arguments | `mail_patch.rs` |
//! | `interpret-trailers` on stdin, and the `trailer.*` config table | here |
//! | `request-pull` on `Branched`/`Merged`/`AwkwardPaths`/`Submodule` | `mail_patch.rs` |
//! | `request-pull` against a **real remote**, published and not | here |
//!
//! Nothing below repeats an argv/shape/stdin triple that already exists in one
//! of those modules.
//!
//! # Payloads are `&'static [u8]` literals
//!
//! Same rule as `corpus/stdin_plumbing.rs`: a case is reproducible from its id,
//! so nothing is read off the filesystem at generation time. Every mailbox and
//! every patch below is typed out in this file. Three consequences:
//!
//! * **Line endings, trailing blanks and the `-- ` signature separator are
//!   literal.** They are the whole point of several cases — a trailing space
//!   after `--` is what makes a signature a signature, and `WS_*` payloads exist
//!   to carry damage that `--whitespace` has to act on — so they must survive
//!   editing this file. A stripped trailing space silently converts a real case
//!   into a weaker one.
//! * **Two blob ids are hard-coded**, and they are facts about
//!   [`Shape::Linear`]'s content rather than about a build:
//!   `9741694…` is `hash-object` of `# fixture\n` (README.md) and `46e89a2…` is
//!   `hash-object` of `pub fn one() -> u32 { 1 }\n` (src/lib.rs). Only `--3way`
//!   reads them; plain `apply` ignores an `index` line entirely. If `fixture.rs`
//!   ever changes those two files, the `--3way` cases stop testing the
//!   fallback-succeeds path and start testing the fallback-refused path — which
//!   is why both are present and labelled below.
//! * **No case may name a commit id**, so `format-patch --base=<oid>` and
//!   `--in-reply-to` chains keyed to a real message are not expressible; the
//!   `--base` cases use revisions and `--base=auto`.
//!
//! # Which output is *not* byte-stable, and why it is avoided
//!
//! * **`Message-Id`** is derived from `time(NULL)` and the commit
//!   (`builtin/log.c`), and nothing pins it. `format-patch` emits one only under
//!   `--thread`, so every `--stdout` case here omits `--thread`; the threading
//!   cases live in `mail_patch.rs` in file-writing form, where only filenames
//!   are compared.
//! * **The default signature is `git --version`.** `format-patch` with no
//!   `--signature`/`--no-signature` ends every message with `-- \n2.55.0\n`.
//!   That is a fact about the *installed* git on the stock side and about the
//!   port's own version string on the other; the two agree today (verified:
//!   both sides emit `2.55.0` for `format-patch --stdout -1 -M` on
//!   [`Shape::Renamed`]), and every one of these cases would fail on a machine
//!   with a different stock git. The harness's second oracle proves the point:
//!   on the `format-patch` cases that fail for another reason it reports
//!   `gits-disagree`, because git 2.55.0 signs `2.55.0` and /usr/bin/git signs
//!   `2.50.1 (Apple Git-155)` in the same position. Cases that care about the
//!   body rather than the trailer pass `--no-signature`.
//! * **`am`'s whitespace diagnostics name `.git/rebase-apply/patch`**, and
//!   `apply`'s name `<stdin>`. Both are stable strings, not paths into the
//!   fixture, so they are compared.
//!
//! # What the state probe can and cannot see
//!
//! `runner::probe_state` reads `status --porcelain=v1 -uall`, `for-each-ref`,
//! `rev-parse HEAD`, `ls-files --stage`, `stash list`,
//! `cat-file --batch-check --batch-all-objects` and `config --list --local`.
//! For `am` and `apply` that is the whole result — index, worktree and HEAD —
//! and it is why the `am` cases below are worth more than their one-line
//! stdout: a wrong author date, a wrong message or a wrong tree all move
//! `rev-parse HEAD`.
//!
//! It does **not** read file contents. So `mailinfo msg.txt patch.txt` is
//! measured on its stdout header block (`Author`/`Email`/`Subject`/`Date`,
//! which is the parse result) plus the *existence* of the two files; the bytes
//! it wrote into them are not compared. Likewise `mailsplit` prints only a
//! count, and the split messages are compared as untracked filenames
//! (`?? 0001`), not as content. Both limits are stated here rather than papered
//! over, because a case that looks like it compares a message body and does not
//! is worse than a missing case.
//!
//! `.git/rebase-apply` is not probed either, so an `am` that stops mid-session
//! is pinned by its exit code and diagnostics; whether the session directory is
//! well-formed is `corpus/sequences.rs`'s job.

use crate::fixture::Shape;
use crate::runner::Case;

// ---------------------------------------------------------------------------
// Mailbox payloads
// ---------------------------------------------------------------------------
//
// Every diff below applies to Shape::Linear's `README.md` (`# fixture\n`) or
// `src/lib.rs` (`pub fn one() -> u32 { 1 }\n`). `Date:` is 1700000000, the same
// instant `env::FIXED_DATE` pins, so a commit `am` builds from one of these has
// an author date equal to its committer date and the resulting commit id is
// reproducible.

/// The floor mailbox: one message, one hunk, a `---` divider, a diffstat and a
/// signature. Everything else in this section is this with one thing changed.
const MBOX_ONE: &[u8] = b"From 1111111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] add a line to README

Body text explaining the change.

---
 README.md | 1 +
 1 file changed, 1 insertion(+)

diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
--\x20
2.55.0

";

/// Two messages in one mailbox, touching different files, so `am` has to commit
/// twice and `mailsplit` has to find the boundary. The second `From ` line is
/// the boundary, and it is preceded by a blank line exactly as `format-patch`
/// writes it.
const MBOX_TWO: &[u8] = b"From 1111111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH 1/2] add a line to README

First message body.
---
 README.md | 1 +
 1 file changed, 1 insertion(+)

diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified

From 2222222222222222222222222222222222222222 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH 2/2] add a function

Second message body.
---
 src/lib.rs | 1 +
 1 file changed, 1 insertion(+)

diff --git a/src/lib.rs b/src/lib.rs
index 46e89a2..7d4c2b1 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1,2 @@
 pub fn one() -> u32 { 1 }
+pub fn two() -> u32 { 2 }
";

/// A scissors line with a second `Subject:` below it. Verified under stock: the
/// commit subject is `subject above the scissors` by default and
/// `the real subject` under `--scissors`, so the two spellings are
/// distinguishable in `rev-parse HEAD` and not only in the `Applying:` line.
const MBOX_SCISSORS: &[u8] = b"From 3333333333333333333333333333333333333333 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] subject above the scissors

Chatter that must not reach the commit message.

-- >8 --
Subject: the real subject

The real body.
---
 README.md | 1 +
 1 file changed, 1 insertion(+)

diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// A well-formed message carrying no diff at all — the input `--empty` exists
/// for. Stock: `Patch is empty.` and exit 128 by default, `Skipping:` under
/// `--empty=drop`, `Creating an empty commit:` under `--empty=keep`.
const MBOX_EMPTY: &[u8] = b"From 4444444444444444444444444444444444444444 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] a message that carries no diff at all

Only prose, no hunks anywhere.
";

/// Base64 body. `mailinfo.c` has to decode before it can find the `---` divider
/// or the diff, so a port that scans the raw body sees neither.
const MBOX_B64: &[u8] = b"From 5555555555555555555555555555555555555555 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] base64 encoded body
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8
Content-Transfer-Encoding: base64

Qm9keSBleHBsYWluaW5nIHRoZSBjaGFuZ2UuCi0tLQogUkVBRE1FLm1kIHwgMSArCiAxIGZpbGUg
Y2hhbmdlZCwgMSBpbnNlcnRpb24oKykKCmRpZmYgLS1naXQgYS9SRUFETUUubWQgYi9SRUFETUUu
bWQKaW5kZXggOTc0MTY5NC4uMTMxNGZhNCAxMDA2NDQKLS0tIGEvUkVBRE1FLm1kCisrKyBiL1JF
QURNRS5tZApAQCAtMSArMSwyIEBACiAjIGZpeHR1cmUKK21vZGlmaWVkCg==
";

/// Quoted-printable body. The only encoded byte is `=` as `=3D`, which appears
/// in `@@ -1 +1,2 @@` and in `100644` context — decode it wrong and the hunk
/// header stops parsing.
const MBOX_QP: &[u8] = b"From 6666666666666666666666666666666666666666 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] quoted-printable body
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8
Content-Transfer-Encoding: quoted-printable

Body explaining the change.
---
 README.md | 1 +
 1 file changed, 1 insertion(+)

diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// `Content-Transfer-Encoding: 8bit` with `charset=ISO-8859-1`: raw `0xe9` in
/// the author name, the subject and the body. Stock warns
/// `commit message did not conform to UTF-8` and commits the bytes as they are,
/// which is what `--utf8`/`--no-utf8` and `i18n.commitEncoding` sit on top of.
const MBOX_LATIN1: &[u8] = b"From 7777777777777777777777777777777777777777 Mon Sep 17 00:00:00 2001
From: A\xe9 Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] latin1 body with a caf\xe9
MIME-Version: 1.0
Content-Type: text/plain; charset=ISO-8859-1
Content-Transfer-Encoding: 8bit

Body mentioning a caf\xe9.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// [`MBOX_ONE`] with CRLF throughout. Stock warns `quoted CRLF detected` and
/// applies cleanly by default, because `mailinfo` strips the CR; under
/// `--keep-cr` the CRs reach `apply`, the context stops matching `# fixture`
/// and the patch is refused. That inversion is the whole contract of the flag.
const MBOX_CRLF: &[u8] = b"From 1111111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001\r
From: A U Thor <author@example.invalid>\r
Date: Tue, 14 Nov 2023 22:13:20 +0000\r
Subject: [PATCH] add a line to README\r
\r
Body text explaining the change.\r
\r
---\r
 README.md | 1 +\r
 1 file changed, 1 insertion(+)\r
\r
diff --git a/README.md b/README.md\r
index 9741694..1314fa4 100644\r
--- a/README.md\r
+++ b/README.md\r
@@ -1 +1,2 @@\r
 # fixture\r
+modified\r
-- \r
2.55.0\r
\r
";

/// Not a mailbox and not a patch. `mailsplit` calls it `corrupt mailbox`;
/// `am` reads it as one message with an empty patch.
const MBOX_JUNK: &[u8] = b"this file is not a mailbox
and carries no patch
";

/// A header block with no blank line before the body. `mailinfo.c` ends the
/// header block at the first line that is not a header continuation, so the
/// `diff --git` line both terminates the headers and starts the patch.
const MBOX_NOBLANK: &[u8] = b"From 8888888888888888888888888888888888888888 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] no blank line after the headers
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// An in-body `From:`/`Subject:`/`Date:` block that must win over the envelope
/// headers. Verified: stock reports `Author: In Body`, `Subject: in-body
/// subject`, `Date: Wed, 15 Nov 2023 01:00:00 +0000`, so a port that reads only
/// the envelope commits under the wrong identity *and* the wrong date.
const MBOX_INBODY: &[u8] = b"From 9999999999999999999999999999999999999999 Mon Sep 17 00:00:00 2001
From: Envelope Author <envelope@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] header subject

From: In Body <inbody@example.invalid>
Subject: in-body subject
Date: Wed, 15 Nov 2023 01:00:00 +0000

Body after the in-body header block.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// RFC 2047 encoded words in `From:` and `Subject:`, plus a folded continuation
/// line. Stock decodes `=?UTF-8?q?caf=C3=A9=20subject?=` and then appends the
/// folded `X-Folded: continued` to the subject, because a leading-space line
/// after a header *is* that header's continuation.
const MBOX_ENCWORD: &[u8] = b"From aaaa111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001
From: =?UTF-8?q?A=C3=A9=20Thor?= <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] =?UTF-8?q?caf=C3=A9=20subject?=
 X-Folded: continued

Body.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// `multipart/mixed`: commentary in the first part, the patch in the second.
/// `mailinfo` has to walk to the boundary and keep only the part that carries
/// the diff.
const MBOX_MULTIPART: &[u8] = b"From bbbb111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] multipart message
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary=\"BOUND\"

--BOUND
Content-Type: text/plain; charset=UTF-8

Commentary part.

--BOUND
Content-Type: text/x-patch; name=\"fix.patch\"

diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified

--BOUND--
";

/// mboxrd escaping: a body line beginning `From ` is stored as `>From `. Under
/// `--patch-format=mboxrd` the leading `>` is removed again; under plain `mbox`
/// it is not, and the commit message differs by one byte.
const MBOX_MBOXRD: &[u8] = b"From cccc111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] mboxrd escaping

>From the desk of the author.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// StGit export: no `From ` envelope line at all, so mbox detection must fail
/// and `--patch-format=stgit` must be given. Verified: stock commits the
/// subject as the literal string `Subject: stgit formatted patch`, because
/// StGit's leader is a *message*, not a header block.
const PATCH_STGIT: &[u8] = b"From: A U Thor <author@example.invalid>
Subject: stgit formatted patch
Date: Tue, 14 Nov 2023 22:13:20 +0000

StGit body.

---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// Mercurial `hg export`: `# HG changeset patch` leader, `# User`, `# Date`
/// as epoch-plus-offset, then a bare subject line.
const PATCH_HG: &[u8] = b"# HG changeset patch
# User A U Thor <author@example.invalid>
# Date 1700000000 0
# Node ID dddd111111111111111111111111111111111111
hg exported subject

diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// Carries a `Message-Id`, which `--message-id`/`am.messageid` appends to the
/// commit message as a `Message-Id:` trailer — so the commit id moves.
const MBOX_MSGID: &[u8] = b"From eeee111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001
Message-Id: <20231114221320.1234-1-author@example.invalid>
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] carries a message id

Body.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// A patch whose context line is not in the pre-image, with a *real* pre-image
/// blob id on the `index` line. Plain `am` refuses at
/// `error: patch failed: README.md:1`; `--3way` gets far enough to reconstruct
/// the base tree from `9741694…` and then refuses with
/// `It does not apply to blobs recorded in its index.` — a different diagnostic
/// reached through a different code path.
const MBOX_NOAPPLY: &[u8] = b"From aaaa222222222222222222222222222222222222 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] context that is not there

Body.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1,2 +1,3 @@
 # fixture
-a line the pre-image does not have
+replacement
+added
";

/// The same change under one extra leading path component, so `-p2` strips
/// `a/proj/` and lands on `README.md` while the default `-p1` does not.
const MBOX_DEEP: &[u8] = b"From aaaa333333333333333333333333333333333333 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] one extra leading component

Body.
---
diff --git a/proj/README.md b/proj/README.md
index 9741694..1314fa4 100644
--- a/proj/README.md
+++ b/proj/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// Added lines carrying trailing blanks and a space-before-tab indent. The
/// trailing spaces on the `+trailing blanks   ` line are load-bearing.
const MBOX_WS: &[u8] = b"From aaaa444444444444444444444444444444444444 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] whitespace-damaging additions

Body.
---
diff --git a/README.md b/README.md
index 9741694..2222222 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,3 @@
 # fixture
+trailing blanks\x20\x20\x20
+ \x09space before tab
";

/// One message touching both tracked files, so `--include=`/`--exclude=` have
/// something to sort and the resulting tree differs by which one was applied.
const MBOX_BOTH: &[u8] = b"From aaaa555555555555555555555555555555555555 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] touch both tracked files

Body.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
diff --git a/src/lib.rs b/src/lib.rs
index 46e89a2..7d4c2b1 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1,2 @@
 pub fn one() -> u32 { 1 }
+pub fn two() -> u32 { 2 }
";

/// A bracketed prefix that is not `PATCH`. Verified: the default strips it
/// (`a non-PATCH bracket prefix`), `-b` keeps it — the opposite of what the
/// flag's name suggests to a reader who has not read `mailinfo.c`, and the only
/// payload here that can tell `-b` from the default at all.
const MBOX_BRACKET: &[u8] = b"From aaaa666666666666666666666666666666666666 Mon Sep 17 00:00:00 2001
From: A U Thor <author@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [OTHER v2] a non-PATCH bracket prefix

Body.
---
diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

// ---------------------------------------------------------------------------
// Bare-patch payloads, for `apply` and `patch-id`
// ---------------------------------------------------------------------------

/// A modify, an add and a delete in one patch: three different `apply` paths,
/// and the minimum for `--stat`/`--numstat`/`--summary` to differ from each
/// other in more than formatting.
const P_MULTI: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # fixture
+modified
diff --git a/added.txt b/added.txt
new file mode 100644
index 0000000..420b1cb
--- /dev/null
+++ b/added.txt
@@ -0,0 +1 @@
+newly added
diff --git a/src/lib.rs b/src/lib.rs
deleted file mode 100644
index 46e89a2..0000000
--- a/src/lib.rs
+++ /dev/null
@@ -1 +0,0 @@
-pub fn one() -> u32 { 1 }
";

/// Creates one new file and touches nothing existing — the only shape in which
/// `--directory=` can *succeed*, since it rewrites the target path and an
/// existing-file hunk would then miss.
const P_CREATE: &[u8] = b"diff --git a/added.txt b/added.txt
new file mode 100644
index 0000000..420b1cb
--- /dev/null
+++ b/added.txt
@@ -0,0 +1 @@
+newly added
";

/// One extra leading component, for `-p0`/`-p2`.
const P_DEEP: &[u8] = b"diff --git a/proj/README.md b/proj/README.md
index 9741694..1314fa4 100644
--- a/proj/README.md
+++ b/proj/README.md
@@ -1 +1,2 @@
 # fixture
+modified
";

/// A zero-context hunk (`@@ -1,0 +2 @@`). Refused as `patch does not apply`
/// without `--unidiff-zero` and applied with it: the flag turns off the
/// context check that a zero-context hunk cannot satisfy.
const P_ZERO: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1,0 +2 @@
+modified
";

/// Post-image with no trailing newline, marked by `\\ No newline at end of
/// file`. Applying it removes the final LF, which `ls-files --stage` sees as a
/// different blob.
const P_NOEOL: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..3333333 100644
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-# fixture
+# fixture no eol
\\ No newline at end of file
";

/// A hunk header promising nine lines and supplying two. Stock: `error: corrupt
/// patch at <stdin>:8`, exit 128 — and exit 0 under `--recount`, which recounts
/// from the body instead of trusting the header. The `<stdin>` in that message
/// is why this is a stdin case rather than a file one.
const P_RECOUNT: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1,9 +1,9 @@
 # fixture
+modified
";

/// Two whitespace-damaged added lines: trailing blanks, and a space before a
/// tab in the indent. Both trailing spaces are literal and load-bearing.
const P_WS: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..2222222 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,3 @@
 # fixture
+trailing blanks\x20\x20\x20
+ \x09space before tab
";

/// Context that is not in the pre-image, with a real pre-image blob id. Under
/// `--3way` git *can* build the fake ancestor and still refuses, because the
/// patch does not apply to the blob it names.
const P_NOAPPLY: &[u8] = b"diff --git a/README.md b/README.md
index 9741694..1314fa4 100644
--- a/README.md
+++ b/README.md
@@ -1,2 +1,3 @@
 # fixture
-a line the pre-image does not have
+replacement
+added
";

/// [`P_NOAPPLY`] with blob ids no repository has. The `--3way` diagnostic is a
/// different one — `repository lacks the necessary blob to perform 3-way
/// merge.` — and separating the two is the point of having both payloads.
const P_FAKEIDX: &[u8] = b"diff --git a/README.md b/README.md
index 1111111..2222222 100644
--- a/README.md
+++ b/README.md
@@ -1,2 +1,3 @@
 # fixture
-a line the pre-image does not have
+replacement
+added
";

/// A mode change with no hunk at all. `apply --index` has to move the index
/// entry's mode with no content to read, and `patch-id` has to hash a patch
/// with no `@@` line.
const P_MODE: &[u8] = b"diff --git a/src/lib.rs b/src/lib.rs
old mode 100644
new mode 100755
";

/// A pure rename, expressed the way `format-patch -M` writes one: a
/// `similarity index 100%` header and no hunk.
const P_RENAME: &[u8] = b"diff --git a/README.md b/docs/README.md
similarity index 100%
rename from README.md
rename to docs/README.md
";

/// A new symlink: mode 120000 with the target as the blob's only line.
const P_SYMLINK: &[u8] = b"diff --git a/link b/link
new file mode 120000
index 0000000..d0b1d59
--- /dev/null
+++ b/link
@@ -0,0 +1 @@
+README.md
\\ No newline at end of file
";

/// A path that climbs out of the worktree. Stock refuses with
/// `error: invalid path '../escape.txt'` at exit 128. **Only the refusal is a
/// case**: `--unsafe-paths` makes stock write the file into the case
/// directory's *parent*, which the state probe cannot see and which would leak
/// between cases, so that half is deliberately not measured.
const P_UNSAFE: &[u8] = b"diff --git a/../escape.txt b/../escape.txt
new file mode 100644
index 0000000..d95f3ad
--- /dev/null
+++ b/../escape.txt
@@ -0,0 +1 @@
+content
";

/// Zero bytes. `No valid patches in input (allow with "--allow-empty")` at exit
/// 128, and exit 0 with the flag.
const P_EMPTY: &[u8] = b"";

/// A combined diff, as `diff --cc` writes one for a merge: two-column hunk
/// header, `@@@`, and two-character line prefixes. `patch-id` has to hash it
/// without mistaking the second `+` column for content.
const P_COMBINED: &[u8] = b"diff --cc conflict.txt
index 1111111,2222222..3333333
--- a/conflict.txt
+++ b/conflict.txt
@@@ -1,1 -1,1 +1,2 @@@
+ ours
+ theirs
";

// ---------------------------------------------------------------------------
// Commit-message payloads, for `interpret-trailers`
// ---------------------------------------------------------------------------

/// Subject, body, and a two-line trailer block.
const MSG_TRAILERS: &[u8] = b"subject line

Body paragraph explaining the change.

Signed-off-by: A U Thor <author@example.invalid>
Acked-by: R Viewer <reviewer@example.invalid>
";

/// Subject and body, no trailer block: the `ifMissing` branch.
const MSG_NOTRAILERS: &[u8] = b"subject line

Body paragraph with no trailer block at all.
";

/// A subject and nothing else. A trailer added here has to create the blank
/// line that separates it from the subject.
const MSG_SUBJECT: &[u8] = b"subject only
";

/// A trailer block followed by a comment line, which `--no-divider` and the
/// default treat differently.
const MSG_COMMENT: &[u8] = b"subject

Body.

Signed-off-by: A U Thor <author@example.invalid>

# comment after the trailers
";

/// A folded trailer: a continuation line indented by two spaces. `--unfold`
/// joins it onto one line; without the flag it stays folded.
const MSG_FOLDED: &[u8] = b"subject

Body.

Folded-by: first line
  continued on the next
Signed-off-by: A U Thor <author@example.invalid>
";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// One stdin-fed case.
fn si(cmd: &'static str, args: &[&str], shape: Shape, input: &'static [u8], out: &mut Vec<Case>) {
    out.push(Case::with_stdin(cmd, args, shape, input));
}

/// One stdin-fed case with stderr compared byte for byte. Used where the
/// diagnostic *is* the contract — for `apply` and `am` a refusal is the thing a
/// maintainer reads, and a port that refuses for a different reason has not
/// matched.
fn sx(cmd: &'static str, args: &[&str], shape: Shape, input: &'static [u8], out: &mut Vec<Case>) {
    out.push(Case { compare_stderr: true, ..Case::with_stdin(cmd, args, shape, input) });
}

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    am_stdin(out);
    apply_stdin(out);
    mailinfo_forms(out);
    mailsplit_forms(out);
    format_patch_content(out);
    request_pull_remote(out);
    interpret_trailers_stdin(out);
    patch_id_forms(out);
}

// ---------------------------------------------------------------------------
// am
// ---------------------------------------------------------------------------

/// `am` fed a mailbox on stdin — the invocation `git send-email` output lands
/// in, and the one no other module reaches.
///
/// What each case pins is not the `Applying: …` line, which is cheap, but the
/// commit it leaves behind: `probe_state`'s `rev-parse HEAD` folds the author
/// name, the author date, the subject, the body and the tree into one id. A
/// port that decodes a base64 body but keeps the envelope `From:` over the
/// in-body one produces a different id here while printing the same line.
fn am_stdin(out: &mut Vec<Case>) {
    // The floor: one message applies and commits.
    si("am", &["am"], Shape::Linear, MBOX_ONE, out);
    si("am", &["am", "--signoff"], Shape::Linear, MBOX_ONE, out);
    si("am", &["am", "--committer-date-is-author-date"], Shape::Linear, MBOX_ONE, out);
    // `--keep` leaves `[PATCH]` in the subject, so the commit id moves.
    si("am", &["am", "--keep"], Shape::Linear, MBOX_ONE, out);
    si("am", &["am", "--3way"], Shape::Linear, MBOX_ONE, out);
    // Two messages: two commits, and the second must see the first's tree.
    si("am", &["am"], Shape::Linear, MBOX_TWO, out);
    si("am", &["am", "--quiet"], Shape::Linear, MBOX_TWO, out);

    // ---- what counts as the commit message ----
    // Scissors: the subject above the line versus the one below it.
    si("am", &["am"], Shape::Linear, MBOX_SCISSORS, out);
    si("am", &["am", "--scissors"], Shape::Linear, MBOX_SCISSORS, out);
    si("am", &["am", "--no-scissors"], Shape::Linear, MBOX_SCISSORS, out);
    // In-body headers must beat the envelope: author, subject and date.
    si("am", &["am"], Shape::Linear, MBOX_INBODY, out);
    si("am", &["am", "--keep-non-patch"], Shape::Linear, MBOX_INBODY, out);
    si("am", &["am"], Shape::Linear, MBOX_ENCWORD, out);
    si("am", &["am"], Shape::Linear, MBOX_BRACKET, out);
    si("am", &["am", "-b"], Shape::Linear, MBOX_BRACKET, out);
    si("am", &["am"], Shape::Linear, MBOX_NOBLANK, out);
    si("am", &["am", "--message-id"], Shape::Linear, MBOX_MSGID, out);

    // ---- transfer encodings: the body has to be decoded before it parses ----
    si("am", &["am"], Shape::Linear, MBOX_B64, out);
    si("am", &["am"], Shape::Linear, MBOX_QP, out);
    si("am", &["am"], Shape::Linear, MBOX_MULTIPART, out);
    si("am", &["am"], Shape::Linear, MBOX_LATIN1, out);
    si("am", &["am", "--utf8"], Shape::Linear, MBOX_LATIN1, out);
    si("am", &["am", "--no-utf8"], Shape::Linear, MBOX_LATIN1, out);

    // ---- line endings ----
    // Default strips the CR and applies; --keep-cr hands it to `apply`, whose
    // context then fails to match. Both halves are cases.
    si("am", &["am"], Shape::Linear, MBOX_CRLF, out);
    sx("am", &["am", "--keep-cr"], Shape::Linear, MBOX_CRLF, out);

    // ---- the input formats `am` can be told to expect ----
    si("am", &["am", "--patch-format=mboxrd"], Shape::Linear, MBOX_MBOXRD, out);
    si("am", &["am"], Shape::Linear, MBOX_MBOXRD, out);
    si("am", &["am", "--patch-format=stgit"], Shape::Linear, PATCH_STGIT, out);
    si("am", &["am", "--patch-format=hg"], Shape::Linear, PATCH_HG, out);

    // ---- flags that reach `apply` through `am` ----
    si("am", &["am", "-p2"], Shape::Linear, MBOX_DEEP, out);
    si("am", &["am", "--include=README.md"], Shape::Linear, MBOX_BOTH, out);
    si("am", &["am", "--exclude=src/lib.rs"], Shape::Linear, MBOX_BOTH, out);
    si("am", &["am", "--ignore-whitespace"], Shape::Linear, MBOX_WS, out);
    si("am", &["am", "--whitespace=fix"], Shape::Linear, MBOX_WS, out);

    // ---- refusals: for `am` the diagnostic is the contract ----
    // An empty patch, under each `--empty` policy.
    sx("am", &["am"], Shape::Linear, MBOX_EMPTY, out);
    si("am", &["am", "--empty=drop"], Shape::Linear, MBOX_EMPTY, out);
    si("am", &["am", "--empty=keep"], Shape::Linear, MBOX_EMPTY, out);
    // Not a mailbox at all: read as one message with an empty patch.
    sx("am", &["am"], Shape::Linear, MBOX_JUNK, out);
    // Context that is not there — three different refusal paths.
    sx("am", &["am"], Shape::Linear, MBOX_NOAPPLY, out);
    sx("am", &["am", "--3way"], Shape::Linear, MBOX_NOAPPLY, out);
    // --reject writes `README.md.rej`, which the state probe sees as untracked.
    si("am", &["am", "--reject"], Shape::Linear, MBOX_NOAPPLY, out);
    // --directory rewrites the target into a path the index does not have.
    sx("am", &["am", "--directory=nowhere"], Shape::Linear, MBOX_DEEP, out);
    sx("am", &["am", "--whitespace=error"], Shape::Linear, MBOX_WS, out);
    // An unknown `--patch-format` value is rejected before any input is read.
    sx("am", &["am", "--patch-format=bogus"], Shape::Linear, MBOX_ONE, out);

    // ---- am.* config, on input that reaches the behaviour it controls ----
    out.push(
        Case { compare_stderr: true, ..Case::with_stdin("am", &["am"], Shape::Linear, MBOX_NOAPPLY) }
            .with_config(&[("am.threeWay", "true")]),
    );
    out.push(
        Case { compare_stderr: true, ..Case::with_stdin("am", &["am"], Shape::Linear, MBOX_CRLF) }
            .with_config(&[("am.keepcr", "true")]),
    );
    out.push(
        Case::with_stdin("am", &["am"], Shape::Linear, MBOX_MSGID)
            .with_config(&[("am.messageid", "true")]),
    );
    out.push(
        Case { compare_stderr: true, ..Case::with_stdin("am", &["am"], Shape::Linear, MBOX_WS) }
            .with_config(&[("apply.whitespace", "error")]),
    );
    // i18n.commitEncoding is recorded in the commit object's `encoding` header,
    // so a port that records it and one that does not differ in `rev-parse HEAD`
    // even when both print the same line.
    out.push(
        Case::with_stdin("am", &["am"], Shape::Linear, MBOX_LATIN1)
            .with_config(&[("i18n.commitEncoding", "ISO-8859-1")]),
    );
    // core.autocrlf decides what lands in the *worktree* after the commit, which
    // `status --porcelain` reports and `rev-parse HEAD` does not.
    out.push(
        Case::with_stdin("am", &["am"], Shape::Linear, MBOX_CRLF)
            .with_config(&[("core.autocrlf", "true")]),
    );

    // A repository that already has one file removed and another staged: `am`
    // must still land the patch that touches neither.
    si("am", &["am"], Shape::Dirty, MBOX_ONE, out);
}

// ---------------------------------------------------------------------------
// apply
// ---------------------------------------------------------------------------

/// `apply` fed a patch on stdin.
///
/// `corpus/shape_reach.rs` covers the [`Shape::Patches`] files; this covers the
/// forms no file in that shape has — a delete, an add, a mode change, a rename
/// header with no hunk, a symlink, a zero-context hunk, a missing final newline,
/// a wrong hunk count, a path that climbs out of the worktree — and it covers
/// them on stdin, where the diagnostics name `<stdin>` rather than a file. That
/// name is part of the message a maintainer reads, and it is emitted by
/// `apply.c` from the input's own name.
fn apply_stdin(out: &mut Vec<Case>) {
    // ---- the read-only reports, on a patch with three kinds of change ----
    si("apply", &["apply", "--stat"], Shape::Linear, P_MULTI, out);
    si("apply", &["apply", "--numstat"], Shape::Linear, P_MULTI, out);
    si("apply", &["apply", "--numstat", "-z"], Shape::Linear, P_MULTI, out);
    si("apply", &["apply", "--summary"], Shape::Linear, P_MULTI, out);
    si("apply", &["apply", "-v", "--check"], Shape::Linear, P_MULTI, out);
    // A rename header with no hunk: `--stat` prints a zero-line change and
    // `--summary` prints the `rename … (100%)` line, which is the only place
    // the similarity survives.
    si("apply", &["apply", "--stat"], Shape::Linear, P_RENAME, out);
    si("apply", &["apply", "--summary"], Shape::Linear, P_RENAME, out);

    // ---- where the result lands: worktree, index, or both ----
    si("apply", &["apply"], Shape::Linear, P_MULTI, out);
    si("apply", &["apply", "--index"], Shape::Linear, P_MULTI, out);
    si("apply", &["apply", "--cached"], Shape::Linear, P_MULTI, out);
    si("apply", &["apply", "--index"], Shape::Linear, P_RENAME, out);
    si("apply", &["apply", "--index"], Shape::Linear, P_MODE, out);
    si("apply", &["apply", "--index"], Shape::Linear, P_SYMLINK, out);
    si("apply", &["apply", "--index"], Shape::Linear, P_NOEOL, out);
    si("apply", &["apply", "--build-fake-ancestor=fake", "--check"], Shape::Linear, P_CREATE, out);

    // ---- path rewriting ----
    si("apply", &["apply", "-p2", "--index"], Shape::Linear, P_DEEP, out);
    si("apply", &["apply", "--directory=src", "--index"], Shape::Linear, P_CREATE, out);
    si("apply", &["apply", "--include=README.md", "--index"], Shape::Linear, P_MULTI, out);
    si("apply", &["apply", "--exclude=src/lib.rs", "--index"], Shape::Linear, P_MULTI, out);

    // ---- hunk matching ----
    si("apply", &["apply", "--unidiff-zero"], Shape::Linear, P_ZERO, out);
    si("apply", &["apply", "--recount"], Shape::Linear, P_RECOUNT, out);
    si("apply", &["apply", "--inaccurate-eof"], Shape::Linear, P_NOEOL, out);
    si("apply", &["apply", "-C1", "--check"], Shape::Linear, P_MULTI, out);
    si("apply", &["apply", "--allow-empty"], Shape::Linear, P_EMPTY, out);

    // ---- whitespace policy, on additions that carry real damage ----
    si("apply", &["apply", "--whitespace=nowarn"], Shape::Linear, P_WS, out);
    si("apply", &["apply", "--whitespace=warn"], Shape::Linear, P_WS, out);
    si("apply", &["apply", "--whitespace=fix", "--index"], Shape::Linear, P_WS, out);
    sx("apply", &["apply", "--whitespace=error"], Shape::Linear, P_WS, out);
    sx("apply", &["apply", "--whitespace=error-all"], Shape::Linear, P_WS, out);
    si("apply", &["apply", "--ignore-space-change", "--check"], Shape::Linear, P_WS, out);

    // ---- reversal ----
    si("apply", &["apply", "-R"], Shape::Linear, P_RENAME, out);
    // Three paths fail for three different reasons; the order stock reports them
    // in is part of the message.
    sx("apply", &["apply", "-R", "--check"], Shape::Linear, P_MULTI, out);

    // ---- refusals ----
    // Zero context without the flag that permits it.
    sx("apply", &["apply"], Shape::Linear, P_ZERO, out);
    // A hunk header that lies about its own size.
    sx("apply", &["apply"], Shape::Linear, P_RECOUNT, out);
    // Context that is not in the pre-image, with and without a usable blob.
    sx("apply", &["apply", "--check"], Shape::Linear, P_NOAPPLY, out);
    sx("apply", &["apply", "--3way"], Shape::Linear, P_NOAPPLY, out);
    sx("apply", &["apply", "--3way"], Shape::Linear, P_FAKEIDX, out);
    // --reject leaves a `.rej` file, which the state probe sees.
    si("apply", &["apply", "--reject"], Shape::Linear, P_NOAPPLY, out);
    // Nothing at all in the input.
    sx("apply", &["apply"], Shape::Linear, P_EMPTY, out);
    // A path that climbs out of the worktree. See P_UNSAFE for why the
    // `--unsafe-paths` half is absent.
    sx("apply", &["apply"], Shape::Linear, P_UNSAFE, out);
    // -p0 leaves the `a/` prefix on the name.
    sx("apply", &["apply", "-p0"], Shape::Linear, P_DEEP, out);

    // ---- apply.* config, which decides the same questions from a file ----
    out.push(
        Case::with_stdin("apply", &["apply"], Shape::Linear, P_WS)
            .with_config(&[("apply.whitespace", "fix")]),
    );
    out.push(
        Case::with_stdin("apply", &["apply", "--check"], Shape::Linear, P_WS)
            .with_config(&[("apply.ignoreWhitespace", "change")]),
    );

    // ---- a worktree that is already dirty ----
    // `Dirty` has README.md edited but not staged and src/lib.rs deleted, so
    // the worktree check and the index check answer differently for the same
    // patch: `--check` refuses, `--check --cached` accepts.
    sx("apply", &["apply", "--check"], Shape::Dirty, P_MULTI, out);
    si("apply", &["apply", "--check", "--cached"], Shape::Dirty, P_MULTI, out);
    si("apply", &["apply", "--index"], Shape::Dirty, P_CREATE, out);
}

// ---------------------------------------------------------------------------
// mailinfo
// ---------------------------------------------------------------------------

/// `mailinfo` — the parser `am` delegates to, reachable only through stdin
/// because it has no file argument for its input.
///
/// Compared: the `Author`/`Email`/`Subject`/`Date` block on stdout, the exit
/// code, and the appearance of `msg.txt`/`patch.txt` as untracked entries. Not
/// compared: what was written into those two files (see the module header).
fn mailinfo_forms(out: &mut Vec<Case>) {
    let mi = |args: &[&str], input, out: &mut Vec<Case>| {
        si("mailinfo", args, Shape::Linear, input, out);
    };
    let plain: &[&str] = &["mailinfo", "msg.txt", "patch.txt"];

    // One form per parser branch.
    mi(plain, MBOX_B64, out);
    mi(plain, MBOX_QP, out);
    mi(plain, MBOX_MULTIPART, out);
    mi(plain, MBOX_NOBLANK, out);
    mi(plain, MBOX_INBODY, out);
    mi(plain, MBOX_ENCWORD, out);
    mi(plain, MBOX_JUNK, out);

    // Subject handling: -k keeps everything, -b keeps a non-PATCH bracket, the
    // default strips both. MBOX_BRACKET is the only payload that separates -b
    // from the default.
    mi(&["mailinfo", "-k", "msg.txt", "patch.txt"], MBOX_ONE, out);
    mi(&["mailinfo", "-b", "msg.txt", "patch.txt"], MBOX_BRACKET, out);
    mi(plain, MBOX_BRACKET, out);

    // Encoding: -u recodes to UTF-8, -n does not, --encoding names the target.
    mi(plain, MBOX_LATIN1, out);
    mi(&["mailinfo", "-u", "msg.txt", "patch.txt"], MBOX_LATIN1, out);
    mi(&["mailinfo", "-n", "msg.txt", "patch.txt"], MBOX_LATIN1, out);
    mi(&["mailinfo", "--encoding=latin1", "msg.txt", "patch.txt"], MBOX_LATIN1, out);

    // Scissors, both ways, on the payload that has a second subject below it.
    mi(&["mailinfo", "--scissors", "msg.txt", "patch.txt"], MBOX_SCISSORS, out);
    mi(&["mailinfo", "--no-scissors", "msg.txt", "patch.txt"], MBOX_SCISSORS, out);

    // The quoted-CR policy, on the only payload that triggers the warning.
    mi(plain, MBOX_CRLF, out);
    mi(&["mailinfo", "--quoted-cr=nowarn", "msg.txt", "patch.txt"], MBOX_CRLF, out);
    mi(&["mailinfo", "--quoted-cr=strip", "msg.txt", "patch.txt"], MBOX_CRLF, out);

    // -m appends the Message-Id to the message file.
    mi(&["mailinfo", "-m", "msg.txt", "patch.txt"], MBOX_MSGID, out);

    // Output paths that already exist as tracked files: the write must still
    // happen, and `status` then reports them as modified rather than untracked.
    mi(&["mailinfo", "README.md", "src/lib.rs"], MBOX_ONE, out);
}

// ---------------------------------------------------------------------------
// mailsplit
// ---------------------------------------------------------------------------

/// `mailsplit` — where one message ends and the next begins.
///
/// Stdout is a count; the answer is the set of files, which `status -uall`
/// reports as untracked entries named `0001`, `0002`, … So these cases pin the
/// number of messages found, the numbering width and the starting number, which
/// is the whole of what the command decides.
fn mailsplit_forms(out: &mut Vec<Case>) {
    let ms = |args: &[&str], input, out: &mut Vec<Case>| {
        si("mailsplit", args, Shape::Linear, input, out);
    };

    // Two messages: the boundary is the second `From ` line.
    ms(&["mailsplit", "-o."], MBOX_TWO, out);
    ms(&["mailsplit", "-o.", "-b"], MBOX_TWO, out);
    ms(&["mailsplit", "-o.", "-d3", "-f5"], MBOX_TWO, out);
    ms(&["mailsplit", "-oout", "-d4"], MBOX_TWO, out);
    // One message each.
    ms(&["mailsplit", "-o."], MBOX_MBOXRD, out);
    ms(&["mailsplit", "-o.", "--keep-cr"], MBOX_CRLF, out);
    ms(&["mailsplit", "-o."], MBOX_CRLF, out);
    // A body line beginning `From ` inside a multipart part must not be read as
    // a new message.
    ms(&["mailsplit", "-o."], MBOX_MULTIPART, out);
    // Refusals: input that is not an mbox, and an output directory that is not
    // there. `-b` turns any input into exactly one message, so the same bytes
    // are a refusal without it and a success with it.
    sx("mailsplit", &["mailsplit", "-o."], Shape::Linear, MBOX_JUNK, out);
    ms(&["mailsplit", "-o.", "-b"], MBOX_JUNK, out);
    sx("mailsplit", &["mailsplit", "-onodir"], Shape::Linear, MBOX_TWO, out);
}

// ---------------------------------------------------------------------------
// format-patch
// ---------------------------------------------------------------------------

/// `format-patch` on the shapes whose *content* decides what the patch says.
///
/// `mail_patch.rs` owns the flag matrix against [`Shape::Branched`], whose
/// history is a one-line edit and a new file — enough to exercise a flag's
/// presence, not enough to exercise what it computes. Everything here runs
/// against a shape that has something for the flag to decide: [`Shape::Renamed`]
/// has a pure rename, a rename at a known similarity, a copy and a rewrite;
/// [`Shape::Whitespace`] has commits whose only change is indentation or line
/// endings; [`Shape::Patches`] has a binary file that changes; and the two
/// awkward-path shapes decide how a path is spelled in a `diff --git` header.
///
/// All of these use `--stdout`, so the whole message is byte-compared. None use
/// `--thread`, which would emit a clock-derived `Message-Id` (module header).
fn format_patch_content(out: &mut Vec<Case>) {
    let fp = |args: &[&str], shape, out: &mut Vec<Case>| {
        out.push(Case::new("format-patch", args, shape));
    };

    // ---- rename and copy detection: what -M/-C/-B actually compute ----
    fp(&["format-patch", "--stdout", "-4", "-M"], Shape::Renamed, out);
    fp(&["format-patch", "--stdout", "-4", "--no-renames"], Shape::Renamed, out);
    // The rename-with-edit commit scores R072, so -M90% must miss it and -M50%
    // must catch it. A port that ignores the threshold passes one and fails the
    // other.
    fp(&["format-patch", "--stdout", "-4", "-M90%"], Shape::Renamed, out);
    fp(&["format-patch", "--stdout", "-4", "-M50%"], Shape::Renamed, out);
    fp(&["format-patch", "--stdout", "-2", "-C"], Shape::Renamed, out);
    fp(&["format-patch", "--stdout", "-2", "-C", "--find-copies-harder"], Shape::Renamed, out);
    fp(&["format-patch", "--stdout", "-1", "-B", "-M"], Shape::Renamed, out);
    fp(&["format-patch", "--stdout", "-4", "--stat", "--summary"], Shape::Renamed, out);
    fp(&["format-patch", "--stdout", "-4", "--compact-summary"], Shape::Renamed, out);
    fp(&["format-patch", "--stdout", "--root", "-5"], Shape::Renamed, out);

    // ---- whitespace: the flags that decide whether a commit has any diff ----
    fp(&["format-patch", "--stdout", "-4"], Shape::Whitespace, out);
    fp(&["format-patch", "--stdout", "-1", "-w"], Shape::Whitespace, out);
    fp(&["format-patch", "--stdout", "-1", "-b"], Shape::Whitespace, out);
    fp(&["format-patch", "--stdout", "-2", "--ignore-blank-lines"], Shape::Whitespace, out);

    // ---- binary content ----
    // `--binary` emits the literal base85 block; the default emits only
    // `Binary files differ`, which is not appliable. That difference is the
    // reason `Shape::Patches` builds its mailbox with `--binary`.
    fp(&["format-patch", "--stdout", "--binary", "main..pending"], Shape::Patches, out);
    fp(&["format-patch", "--stdout", "--no-binary", "main..pending"], Shape::Patches, out);
    fp(&["format-patch", "--stdout", "main..pending"], Shape::Patches, out);
    fp(&["format-patch", "--stdout", "--stat", "main..pending"], Shape::Patches, out);
    fp(&["format-patch", "--stdout", "-2", "--cover-letter", "main..pending"], Shape::Patches, out);
    fp(&["format-patch", "--stdout", "--attach", "--binary", "main..pending"], Shape::Patches, out);
    fp(&["format-patch", "--stdout", "--zero-commit", "--no-signature", "main..pending"], Shape::Patches, out);
    fp(&["format-patch", "--stdout", "--numbered", "-v3", "--start-number=4", "main..pending"], Shape::Patches, out);
    // A second series that differs from the first: --interdiff and --range-diff
    // both have real input here, unlike on a shape with one branch.
    fp(&["format-patch", "--stdout", "-1", "--interdiff=main", "pending"], Shape::Patches, out);
    fp(&["format-patch", "--stdout", "-2", "--range-diff=main", "main..pending"], Shape::Patches, out);
    out.push(
        Case::new("format-patch", &["format-patch", "--stdout", "main..pending"], Shape::Patches)
            .with_config(&[("format.coverLetter", "true"), ("format.signature", "cfg-sig")]),
    );

    // ---- how a path is spelled in a patch header ----
    // The `diff --git` line, the `---`/`+++` lines and the diffstat each quote
    // independently. `core.quotePath=false` turns off the high-byte escaping
    // and nothing else — the `"` in `quote"name.txt` stays escaped either way.
    fp(&["format-patch", "--stdout", "-1", "--stat"], Shape::AwkwardPaths, out);
    out.push(
        Case::new("format-patch", &["format-patch", "--stdout", "-1"], Shape::AwkwardPaths)
            .with_config(&[("core.quotePath", "false")]),
    );
    fp(&["format-patch", "--stdout", "-1"], Shape::DecomposedPaths, out);

    // ---- --base=auto, which needs an upstream to resolve against ----
    // `BehindRemote` is the only shape with a tracking branch, so it is the only
    // one where `auto` can answer rather than die.
    fp(&["format-patch", "--stdout", "--base=auto", "origin/main..div"], Shape::BehindRemote, out);
    // With the base inside the range, stock dies rather than emitting a
    // base-commit line.
    out.push(Case::strict(
        "format-patch",
        &["format-patch", "--stdout", "--base=auto", "-1"],
        Shape::BehindRemote,
    ));

    // ---- diff.* config, on a history where renames exist to find ----
    for (key, value) in [("diff.renames", "false"), ("diff.renames", "copies"), ("diff.renameLimit", "1")] {
        out.push(
            Case::new("format-patch", &["format-patch", "--stdout", "-4"], Shape::Renamed)
                .with_config(&[(key, value)]),
        );
    }
    // Writing files instead: stdout carries filenames, and the state probe
    // carries the produced set. `--filename-max-length` truncates the name
    // built from the subject, which needs a subject long enough to truncate.
    out.push(Case::new(
        "format-patch",
        &["format-patch", "-4", "-o", "series", "--filename-max-length=16"],
        Shape::Renamed,
    ));
    out.push(
        Case::new("format-patch", &["format-patch", "-4"], Shape::Renamed)
            .with_config(&[("format.outputDirectory", "series"), ("format.suffix", ".mbox")]),
    );
}

// ---------------------------------------------------------------------------
// request-pull
// ---------------------------------------------------------------------------

/// `request-pull` — the message a maintainer is asked to act on, and the one
/// command in this group that *contacts* the named repository.
///
/// `mail_patch.rs` runs it against `.`, which is always published because it is
/// the repository itself. [`Shape::BehindRemote`] has a real remote inside the
/// fixture (`.remote.git`, reached by a relative URL) whose refs are behind the
/// local ones, so the "did you push it?" branch — a warning pair on stderr and
/// exit 1 with the body still printed — is reachable for the first time.
fn request_pull_remote(out: &mut Vec<Case>) {
    let rp = |args: &[&str], shape, out: &mut Vec<Case>| {
        out.push(Case::new("request-pull", args, shape));
    };

    // Published: `origin/main` exists in `.remote.git`, so no warning.
    rp(&["request-pull", "HEAD~1", "./.remote.git", "main"], Shape::BehindRemote, out);
    rp(&["request-pull", "origin/main", "./.remote.git", "main"], Shape::BehindRemote, out);
    // Unpublished: `div` has moved locally, so the tip is not at the remote.
    // Stock warns twice and exits 1 with the whole message still on stdout.
    out.push(Case::strict(
        "request-pull",
        &["request-pull", "origin/main", "./.remote.git", "div"],
        Shape::BehindRemote,
    ));
    out.push(Case::strict(
        "request-pull",
        &["request-pull", "-p", "origin/main", "./.remote.git", "div"],
        Shape::BehindRemote,
    ));

    // Histories where the shortlog and the diffstat have real work to describe.
    // `HEAD~4` reaches back past the rename-with-edit commit, so the diffstat
    // has to score an inexact rename; `HEAD~2` does not.
    rp(&["request-pull", "HEAD~2", "."], Shape::Renamed, out);
    rp(&["request-pull", "HEAD~4", ".", "main"], Shape::Renamed, out);
    rp(&["request-pull", "HEAD~3", "."], Shape::Whitespace, out);
    // A binary file in the range: the diffstat says `Bin` rather than a count.
    rp(&["request-pull", "main", ".", "pending"], Shape::Patches, out);
    rp(&["request-pull", "HEAD~1", "."], Shape::Octopus, out);
}

// ---------------------------------------------------------------------------
// interpret-trailers
// ---------------------------------------------------------------------------

/// `interpret-trailers` on stdin, plus the `trailer.*` configuration table.
///
/// `mail_patch.rs` runs it over tracked files, whose content is a README and a
/// one-line Rust file — neither has a trailer block, so every `--if-exists`
/// value collapsed to the same answer and `--only-trailers`, `--parse` and
/// `--unfold` had nothing to print. These payloads have a subject, a body, a
/// two-line trailer block, a folded trailer and a comment line, which is what
/// separates the branches of `trailer.c`'s placement decision.
fn interpret_trailers_stdin(out: &mut Vec<Case>) {
    let it = |args: &[&str], input, out: &mut Vec<Case>| {
        si("interpret-trailers", args, Shape::Linear, input, out);
    };

    // Reading an existing block.
    it(&["interpret-trailers", "--only-trailers"], MSG_TRAILERS, out);
    it(&["interpret-trailers", "--only-trailers", "--only-input"], MSG_TRAILERS, out);
    it(&["interpret-trailers", "--parse"], MSG_TRAILERS, out);
    it(&["interpret-trailers", "--only-trailers", "--unfold"], MSG_FOLDED, out);
    it(&["interpret-trailers", "--only-trailers"], MSG_FOLDED, out);
    it(&["interpret-trailers", "--only-trailers"], MSG_NOTRAILERS, out);

    // Adding one, against each of the message shapes.
    let add = "Acked-by: R V <r@example.invalid>";
    it(&["interpret-trailers", "--trailer", add], MSG_TRAILERS, out);
    it(&["interpret-trailers", "--trailer", add], MSG_NOTRAILERS, out);
    it(&["interpret-trailers", "--trailer", add], MSG_SUBJECT, out);
    it(&["interpret-trailers", "--trailer", add], MSG_COMMENT, out);
    it(&["interpret-trailers", "--no-divider", "--trailer", add], MSG_COMMENT, out);

    // Placement.
    it(&["interpret-trailers", "--where=start", "--trailer", "X: y"], MSG_TRAILERS, out);
    it(&["interpret-trailers", "--where=before", "--trailer", "X: y"], MSG_TRAILERS, out);

    // The duplicate decision: the same token with the same value, and with a
    // different one. Only a message that already has the token can tell these
    // apart, which is why this axis was unmeasurable from a README.
    let same = "Acked-by: R Viewer <reviewer@example.invalid>";
    let other = "Acked-by: Someone Else <else@example.invalid>";
    for policy in ["addIfDifferent", "addIfDifferentNeighbor", "replace", "doNothing"] {
        let flag = format!("--if-exists={policy}");
        it(&["interpret-trailers", &flag, "--trailer", same], MSG_TRAILERS, out);
        it(&["interpret-trailers", &flag, "--trailer", other], MSG_TRAILERS, out);
    }
    it(&["interpret-trailers", "--if-missing=doNothing", "--trailer", "X: y"], MSG_NOTRAILERS, out);
    it(&["interpret-trailers", "--trim-empty", "--trailer", "Bug:"], MSG_TRAILERS, out);

    // The config table. Each key is the file-scoped form of a flag above, and
    // `trailer.<token>.*` adds a shorthand no flag can express.
    let cfg = |pairs: &[(&str, &str)], args: &[&str], input, out: &mut Vec<Case>| {
        out.push(
            Case::with_stdin("interpret-trailers", args, Shape::Linear, input).with_config(pairs),
        );
    };
    cfg(&[("trailer.where", "start")], &["interpret-trailers", "--trailer", "X: y"], MSG_TRAILERS, out);
    cfg(&[("trailer.ifexists", "doNothing")], &["interpret-trailers", "--trailer", other], MSG_TRAILERS, out);
    cfg(&[("trailer.ifmissing", "doNothing")], &["interpret-trailers", "--trailer", "X: y"], MSG_NOTRAILERS, out);
    // A second separator: `X=y` is a trailer only when `=` is one.
    cfg(&[("trailer.separators", ":=")], &["interpret-trailers", "--trailer", "X=y"], MSG_TRAILERS, out);
    cfg(&[("trailer.separators", ":#")], &["interpret-trailers", "--trailer", "X#y"], MSG_TRAILERS, out);
    // Token shorthands, with per-token placement and duplicate policy.
    cfg(
        &[("trailer.ack.key", "Acked-by: "), ("trailer.ack.where", "before")],
        &["interpret-trailers", "--trailer", "ack: R V <r@example.invalid>"],
        MSG_TRAILERS,
        out,
    );
    cfg(
        &[("trailer.sign.key", "Signed-off-by: "), ("trailer.sign.ifexists", "replace")],
        &["interpret-trailers", "--trailer", "sign: Someone Else <else@example.invalid>"],
        MSG_TRAILERS,
        out,
    );
    // `trailer.<token>.cmd` runs a program and uses its output as the value.
    // `/bin/echo` is chosen because it exists on every target, takes the trailer
    // value as its only argument and writes it back unchanged, so the case stays
    // deterministic while still proving the command was run.
    cfg(
        &[("trailer.cmdtok.key", "Cmd: "), ("trailer.cmdtok.cmd", "/bin/echo")],
        &["interpret-trailers", "--trailer", "cmdtok: arg"],
        MSG_TRAILERS,
        out,
    );

    // Refusals: a spec with no separator, and an enum with a value outside it.
    sx("interpret-trailers", &["interpret-trailers", "--trailer", "no-separator"], Shape::Linear, MSG_TRAILERS, out);
    sx("interpret-trailers", &["interpret-trailers", "--if-exists=bogus", "--trailer", "X: y"], Shape::Linear, MSG_TRAILERS, out);
}

// ---------------------------------------------------------------------------
// patch-id
// ---------------------------------------------------------------------------

/// `patch-id` on the diff shapes `corpus/stdin_plumbing.rs` does not carry.
///
/// That module covers a single-commit diff, a rename, a binary block and the
/// CRLF triple. What is left is the *stream* dimension — a mailbox holding two
/// messages, where the second column comes from each message's own `From <oid>`
/// line rather than from a `commit` header — and the diff shapes with no `@@`
/// line at all: a mode change, a rename with no hunk, and a combined `@@@` diff.
/// Each of those is a place a hash can silently include or omit bytes, and every
/// disagreement is a wrong id rather than an error.
fn patch_id_forms(out: &mut Vec<Case>) {
    let pi = |args: &[&str], input, out: &mut Vec<Case>| {
        si("patch-id", args, Shape::Linear, input, out);
    };

    // A two-message mailbox: two ids, each keyed to its own `From ` line oid.
    pi(&["patch-id"], MBOX_TWO, out);
    pi(&["patch-id", "--stable"], MBOX_TWO, out);
    // A single message: the mail headers, the diffstat and the signature must
    // not be hashed. Verified under stock 2.55.0 — this prints the same
    // `6e813516…` as the first line of the MBOX_TWO case above, whose message
    // carries the same diff with different headers.
    pi(&["patch-id"], MBOX_ONE, out);

    // Diffs with no hunk header.
    pi(&["patch-id"], P_MODE, out);
    pi(&["patch-id"], P_RENAME, out);
    pi(&["patch-id"], P_COMBINED, out);
    pi(&["patch-id", "--stable"], P_COMBINED, out);

    // Three files in one diff, so `--stable` (sorted per-file digests) and the
    // default `--unstable` (source order) have a real chance to disagree.
    pi(&["patch-id"], P_MULTI, out);
    pi(&["patch-id", "--stable"], P_MULTI, out);
    pi(&["patch-id", "--unstable"], P_MULTI, out);
    // A missing final newline is content: `--verbatim` and the default disagree.
    pi(&["patch-id"], P_NOEOL, out);
    pi(&["patch-id", "--verbatim"], P_NOEOL, out);
    // Whitespace-only additions, which the default strips and --verbatim keeps.
    pi(&["patch-id", "--verbatim"], P_WS, out);
    // Nothing in the input: no output, exit 0 — not an error.
    pi(&["patch-id"], P_EMPTY, out);
}
