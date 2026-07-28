//! Differential corpus cases for the mail_patch subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! Covers `am`, `apply`, `format-patch`, `mailinfo`, `mailsplit`, `send-email`,
//! `imap-send`, `request-pull`, `interpret-trailers`, `fmt-merge-msg` and
//! `quiltimport`.
//!
//! # What this corpus can and cannot reach
//!
//! Three properties of the harness bound what is expressible here, and each one
//! removes a real part of this subsystem from measurement. They are written down
//! rather than worked around, because a case that quietly tests something weaker
//! than it appears to is worse than a missing case.
//!
//! ## 1. Every case gets `/dev/null` on stdin
//!
//! `runner::run_side` spawns both sides with `.stdin(Stdio::null())`. Half of
//! this group is defined by what it reads from stdin: `mailinfo` has no file
//! argument at all, `am`/`apply`/`imap-send`/`fmt-merge-msg` default to stdin,
//! and `mailsplit` reads stdin when given no file. Those commands are therefore
//! reachable only on their empty-input and file-argument paths. The empty-input
//! path is still worth pinning — it is where the "no valid patches", "empty
//! mbox" and "empty patch" diagnostics live — but it is not the same thing as
//! feeding a real message through.
//!
//! ## 2. No case can materialize a valid patch or mailbox
//!
//! A `Case` is one argv against one prebuilt [`Shape`], and no shape ships a
//! `.patch` or `.mbox` file. There is no pre-step hook, so `apply` cannot be
//! handed a good patch, a corrupt-but-patch-shaped patch, or a context-only
//! hunk, and `am` cannot be handed a mailbox. What is left for `apply` is the
//! flag-parse and input-rejection surface: every case below feeds it a tracked
//! file that is not a patch, which stock rejects with `unrecognized input`
//! before any hunk logic runs. So `apply --check --cached` here pins only that
//! non-patch input still exits 128 — it does *not* reproduce the
//! `error: corrupt patch at <file>:<line>` regression, which needs input that
//! parses as a patch header and then breaks. Reaching that needs a fixture that
//! ships a patch file, which is outside this module.
//!
//! ## 3. `format-patch`'s `Message-ID` embeds `time(NULL)`, and cannot be pinned
//!
//! Two stock runs two seconds apart emit
//! `<…5915d79….1785173648.git.parity@example.invalid>` and
//! `<…5915d79….1785173650.git.parity@example.invalid>`. Nothing pins it:
//! `--in-reply-to` only sets the *parent* id, and there is no config key or
//! environment variable for the generated one (unlike `GIT_COMMITTER_DATE`).
//! The harness's `Nondeterministic` guard cannot be relied on either, since two
//! runs inside the same second do agree, which would make such a case flap
//! rather than fail.
//!
//! Two facts make threading testable anyway:
//!
//! * a `Message-ID` is emitted **only** under `--thread` (verified: plain
//!   `format-patch -2 --stdout` and `--cover-letter` emit none, and
//!   `--in-reply-to` alone emits `In-Reply-To`/`References` but no id of its
//!   own), so every `--stdout` case below that omits `--thread` is fully
//!   deterministic and byte-compared;
//! * `format-patch` writing files puts only *filenames* on stdout, and the
//!   post-state probe sees the results as untracked entries (`?? 0001-….patch`),
//!   so the `--thread` cases are written as file-emitting invocations.
//!
//! The consequence is explicit: for the `--thread` cases the **patch bodies are
//! not compared**. They pin filenames, numbering, exit code and the set of files
//! produced. A wrong `In-Reply-To`/`References` chain inside those files would
//! pass. That is the ceiling this harness allows, not a judgement that the
//! header chain does not matter.
//!
//! ## 4. `send-email` and `imap-send` cannot reach a server
//!
//! `--dry-run` is expressible and is used, but with no valid patch (see 2) every
//! `send-email` invocation dies at `No subject line in <file>?` before any SMTP
//! decision, so `--smtp-server=<program>` never runs the program. `imap-send`
//! reaches only its config-validation errors plus one loopback-refused
//! connection.

use crate::fixture::Shape;
use crate::runner::Case;

/// `format-patch` writing to stdout: the whole body is byte-compared.
fn fp(args: &[&str], out: &mut Vec<Case>) {
    out.push(Case::new("format-patch", args, Shape::Branched));
}

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    format_patch(out);
    apply(out);
    am(out);
    mailinfo(out);
    mailsplit(out);
    send_email(out);
    imap_send(out);
    request_pull(out);
    interpret_trailers(out);
    fmt_merge_msg(out);
    quiltimport(out);
}

/// `format-patch` — the largest option surface in this group, and the one whose
/// output a maintainer reads byte for byte before applying a series.
fn format_patch(out: &mut Vec<Case>) {
    // ---- body-shaping options, byte-compared via --stdout ----
    fp(&["format-patch", "--stdout", "-1"], out);
    fp(&["format-patch", "--stdout", "-2"], out);
    fp(&["format-patch", "--stdout", "-1", "--signoff"], out);
    fp(&["format-patch", "--stdout", "-2", "--numbered"], out);
    fp(&["format-patch", "--stdout", "-2", "--no-numbered"], out);
    fp(&["format-patch", "--stdout", "-1", "--subject-prefix=RFC"], out);
    fp(&["format-patch", "--stdout", "-1", "--rfc"], out);
    fp(&["format-patch", "--stdout", "-1", "--keep-subject"], out);
    fp(&["format-patch", "--stdout", "-1", "--no-signature"], out);
    fp(&["format-patch", "--stdout", "-1", "--signature=custom-sig"], out);
    fp(&["format-patch", "--stdout", "-1", "--signature-file=README.md"], out);
    fp(&["format-patch", "--stdout", "-1", "--no-stat"], out);
    fp(&["format-patch", "--stdout", "-1", "--stat=60"], out);
    fp(&["format-patch", "--stdout", "-1", "--zero-commit"], out);
    fp(&["format-patch", "--stdout", "-1", "-U5"], out);
    fp(&["format-patch", "--stdout", "-1", "--minimal"], out);
    fp(&["format-patch", "--stdout", "-1", "--no-prefix"], out);
    fp(&["format-patch", "--stdout", "-1", "--binary"], out);
    fp(&["format-patch", "--stdout", "-1", "--no-binary"], out);
    fp(&["format-patch", "--stdout", "-1", "--function-context"], out);
    fp(&["format-patch", "--stdout", "-1", "--src-prefix=x/", "--dst-prefix=y/"], out);
    fp(&["format-patch", "--stdout", "-1", "--always"], out);
    fp(&["format-patch", "--stdout", "-1", "--progress"], out);

    // ---- headers and identity ----
    fp(&["format-patch", "--stdout", "-1", "--from"], out);
    fp(&["format-patch", "--stdout", "-1", "--from=Someone <s@example.invalid>"], out);
    fp(
        &[
            "format-patch",
            "--stdout",
            "-1",
            "--force-in-body-from",
            "--from=Someone <s@example.invalid>",
        ],
        out,
    );
    fp(&["format-patch", "--stdout", "-1", "--to=a@example.invalid"], out);
    fp(&["format-patch", "--stdout", "-1", "--cc=b@example.invalid"], out);
    fp(&["format-patch", "--stdout", "-1", "--add-header=X-Test: yes"], out);
    // Deterministic: --in-reply-to sets the parent id only, and generates none.
    fp(&["format-patch", "--stdout", "-1", "--in-reply-to=<root@example.invalid>"], out);

    // ---- MIME ----
    fp(&["format-patch", "--stdout", "-1", "--attach"], out);
    fp(&["format-patch", "--stdout", "-1", "--inline"], out);
    fp(&["format-patch", "--stdout", "-1", "--no-attach"], out);

    // ---- cover letter ----
    fp(&["format-patch", "--stdout", "-1", "--cover-letter"], out);
    fp(&["format-patch", "--stdout", "-2", "--cover-letter"], out);
    fp(&["format-patch", "--stdout", "-1", "--no-cover-letter"], out);
    fp(&["format-patch", "--stdout", "-2", "--cover-from-description=subject"], out);
    fp(&["format-patch", "--stdout", "-2", "--cover-letter", "--cover-from-description=message"], out);
    fp(&["format-patch", "--stdout", "-2", "--cover-letter", "--commit-list-format=%h %s"], out);
    fp(&["format-patch", "--stdout", "-1", "--description-file=README.md"], out);

    // ---- series metadata ----
    fp(&["format-patch", "--stdout", "-1", "-v2"], out);
    fp(&["format-patch", "--stdout", "-1", "--reroll-count=3"], out);
    fp(&["format-patch", "--stdout", "-1", "--base=HEAD~1"], out);
    fp(&["format-patch", "--stdout", "-1", "--notes"], out);
    fp(&["format-patch", "--stdout", "-1", "--interdiff=HEAD~1"], out);
    fp(&["format-patch", "--stdout", "-1", "--range-diff=HEAD~1"], out);
    fp(&["format-patch", "--stdout", "-2", "--cover-letter", "--creation-factor=50"], out);

    // ---- revision selection ----
    fp(&["format-patch", "--stdout"], out);
    fp(&["format-patch", "--stdout", "--root", "main"], out);
    fp(&["format-patch", "--stdout", "HEAD~1..HEAD"], out);
    fp(&["format-patch", "--stdout", "main..feature"], out);

    // ---- error paths ----
    fp(&["format-patch", "--stdout", "-1", "--base=auto"], out);
    fp(&["format-patch", "--stdout", "-1", "--ignore-if-in-upstream"], out);
    fp(&["format-patch", "--stdout", "-1", "--output=out.patch"], out);
    fp(&["format-patch", "--stdout", "-1", "--mbox"], out);

    // ---- writing files: stdout carries filenames, post-state carries the set ----
    // Patch bodies are NOT compared here (see the module header); these pin
    // naming, numbering, the output directory and the produced file set.
    fp(&["format-patch", "-1"], out);
    fp(&["format-patch", "-2"], out);
    fp(&["format-patch", "-2", "--numbered-files"], out);
    fp(&["format-patch", "-1", "--suffix=.txt"], out);
    fp(&["format-patch", "-2", "--start-number=7"], out);
    fp(&["format-patch", "-1", "--filename-max-length=12"], out);
    fp(&["format-patch", "-2", "-o", "patches"], out);
    fp(&["format-patch", "-1", "--quiet"], out);
    fp(&["format-patch", "-2", "--cover-letter"], out);
    // Threading: the only form in which it is measurable at all.
    fp(&["format-patch", "-2", "--thread"], out);
    fp(&["format-patch", "-2", "--thread=shallow"], out);
    fp(&["format-patch", "-2", "--thread=deep"], out);
    fp(&["format-patch", "-2", "--cover-letter", "--thread=deep"], out);
    fp(&["format-patch", "-1", "--no-thread"], out);

    // ---- format.* config, which a user sets once and then never passes ----
    for key in [
        "format.signoff=true",
        "format.numbered=true",
        "format.numbered=auto",
        "format.subjectPrefix=PATCHSET",
        "format.signature=cfg-sig",
        "format.coverLetter=true",
        "format.attach=true",
        "format.from=Cfg Person <cfg@example.invalid>",
        "format.thread=deep",
        "format.to=to@example.invalid",
        "format.cc=cc@example.invalid",
        "format.headers=X-Cfg: 1",
        "format.zeroCommit=true",
        "format.forceInBodyFrom=true",
        "format.notes=true",
        "format.coverFromDescription=auto",
        "format.mboxrd=true",
        "format.noprefix=true",
        "format.useAutoBase=true",
    ] {
        out.push(Case::new(
            "format-patch",
            &["-c", key, "format-patch", "--stdout", "-1"],
            Shape::Branched,
        ));
    }
    // Config that only bites when files are written.
    out.push(Case::new(
        "format-patch",
        &["-c", "format.outputDirectory=out", "format-patch", "-1"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "format-patch",
        &["-c", "format.suffix=.mbox", "format-patch", "-1"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "format-patch",
        &["-c", "format.filenameMaxLength=15", "format-patch", "-1"],
        Shape::Branched,
    ));

    // ---- the same invocation across every repository shape ----
    for &shape in &[
        Shape::Linear,
        Shape::Merged,
        Shape::Dirty,
        Shape::Detached,
        Shape::Conflicted,
        Shape::AwkwardPaths,
        Shape::Submodule,
    ] {
        out.push(Case::new("format-patch", &["format-patch", "--stdout", "-1"], shape));
    }
    out.push(Case::new("format-patch", &["format-patch", "--stdout", "-3"], Shape::Merged));
    out.push(Case::new("format-patch", &["format-patch", "-1"], Shape::AwkwardPaths));
}

/// `apply` — reachable only on its rejection paths, because no shape ships a
/// patch file and stdin is `/dev/null`. Each case feeds it a tracked file that
/// is not a patch and pins the exit code and the untouched worktree.
fn apply(out: &mut Vec<Case>) {
    let mut a = |args: &[&str], shape| out.push(Case::new("apply", args, shape));

    // The modes that matter, each on input stock rejects as not-a-patch.
    a(&["apply", "README.md"], Shape::Linear);
    a(&["apply", "--check", "README.md"], Shape::Linear);
    // The `--check --cached` pair: recently fixed to stop exiting 0 on input it
    // could not parse. This pins the 128 on non-patch input; it does not reach
    // the `corrupt patch at` path (module header, note 2).
    a(&["apply", "--check", "--cached", "README.md"], Shape::Linear);
    a(&["apply", "--cached", "README.md"], Shape::Linear);
    a(&["apply", "--index", "README.md"], Shape::Linear);
    a(&["apply", "--reverse", "README.md"], Shape::Linear);
    a(&["apply", "--3way", "README.md"], Shape::Linear);
    a(&["apply", "--stat", "README.md"], Shape::Linear);
    a(&["apply", "--numstat", "README.md"], Shape::Linear);
    a(&["apply", "--summary", "README.md"], Shape::Linear);
    a(&["apply", "--recount", "--check", "README.md"], Shape::Linear);
    a(&["apply", "--whitespace=error", "README.md"], Shape::Linear);
    a(&["apply", "--build-fake-ancestor=fake", "README.md"], Shape::Linear);
    a(&["apply", "--directory=sub", "README.md"], Shape::Linear);

    // Empty and missing input.
    a(&["apply"], Shape::Linear);
    a(&["apply", "--allow-empty"], Shape::Linear);
    a(&["apply", "/dev/null"], Shape::Linear);
    a(&["apply", "--check", "/dev/null"], Shape::Linear);
    a(&["apply", "no-such.patch"], Shape::Linear);
    a(&["apply", "--bogus-flag", "README.md"], Shape::Linear);

    // A dirty worktree must survive a rejected apply untouched.
    a(&["apply", "--index", "README.md"], Shape::Dirty);
    a(&["apply", "--check", "--cached", "README.md"], Shape::Dirty);
    a(&["apply", "src/lib.rs"], Shape::AwkwardPaths);
}

/// `am` — same shape of limitation as `apply`: no mailbox to consume, so the
/// reachable surface is empty input, non-mailbox input, and the
/// no-session-in-progress control verbs.
fn am(out: &mut Vec<Case>) {
    let mut a = |args: &[&str], shape| out.push(Case::new("am", args, shape));

    a(&["am"], Shape::Linear);
    a(&["am", "/dev/null"], Shape::Linear);
    a(&["am", "README.md"], Shape::Linear);
    a(&["am", "no-such.mbox"], Shape::Linear);
    a(&["am", "--3way", "README.md"], Shape::Linear);
    a(&["am", "--signoff", "README.md"], Shape::Linear);
    a(&["am", "--keep", "README.md"], Shape::Linear);
    a(&["am", "--keep-cr", "README.md"], Shape::Linear);
    a(&["am", "--empty=drop", "README.md"], Shape::Linear);
    a(&["am", "--patch-format=mbox", "README.md"], Shape::Linear);
    a(&["am", "--patch-format=bogus", "README.md"], Shape::Linear);
    a(&["am", "--whitespace=fix", "README.md"], Shape::Linear);
    a(&["am", "-p2", "README.md"], Shape::Linear);
    a(&["am", "--exclude=x", "README.md"], Shape::Linear);
    a(&["am", "--no-verify", "README.md"], Shape::Linear);
    a(&["am", "--bogus-flag"], Shape::Linear);

    // Control verbs with nothing in progress: all must refuse, and none may
    // leave a half-built `.git/rebase-apply` behind.
    a(&["am", "--abort"], Shape::Linear);
    a(&["am", "--skip"], Shape::Linear);
    a(&["am", "--continue"], Shape::Linear);
    a(&["am", "--quit"], Shape::Linear);
    a(&["am", "--show-current-patch"], Shape::Linear);
    a(&["am", "--show-current-patch=diff"], Shape::Linear);
    a(&["am", "--abort"], Shape::Conflicted);

    // am.* config on the rejection path.
    out.push(Case::new("am", &["-c", "am.threeWay=true", "am", "README.md"], Shape::Linear));
    out.push(Case::new("am", &["-c", "am.keepcr=true", "am", "README.md"], Shape::Linear));
    out.push(Case::new("am", &["-c", "am.messageid=true", "am", "README.md"], Shape::Linear));
}

/// `mailinfo` — stdin-only, so every case runs on the empty-input path. The two
/// output files it is told to write show up in the post-state probe as untracked
/// entries, which is what makes these more than an exit-code check.
fn mailinfo(out: &mut Vec<Case>) {
    let mut m = |args: &[&str]| out.push(Case::new("mailinfo", args, Shape::Linear));

    m(&["mailinfo", "msg.txt", "patch.txt"]);
    m(&["mailinfo", "-k", "msg.txt", "patch.txt"]);
    m(&["mailinfo", "-b", "msg.txt", "patch.txt"]);
    m(&["mailinfo", "-u", "msg.txt", "patch.txt"]);
    m(&["mailinfo", "-n", "msg.txt", "patch.txt"]);
    m(&["mailinfo", "--encoding=latin1", "msg.txt", "patch.txt"]);
    m(&["mailinfo", "--scissors", "msg.txt", "patch.txt"]);
    m(&["mailinfo", "--no-scissors", "msg.txt", "patch.txt"]);
    m(&["mailinfo", "--message-id", "msg.txt", "patch.txt"]);
    m(&["mailinfo", "--quoted-cr=strip", "msg.txt", "patch.txt"]);
    // Argument-count and enum-validation errors.
    m(&["mailinfo", "--quoted-cr=bogus", "msg.txt", "patch.txt"]);
    m(&["mailinfo"]);
    m(&["mailinfo", "onlyone.txt"]);
    // Output paths that collide with tracked content.
    m(&["mailinfo", "README.md", "src/lib.rs"]);
}

/// `mailsplit` — the one command in this group whose real work is reachable,
/// because it takes file arguments and `-b` makes any file a single message.
fn mailsplit(out: &mut Vec<Case>) {
    let mut m = |args: &[&str], shape| out.push(Case::new("mailsplit", args, shape));

    // -b: README.md is split into `0001`, which the post-state probe sees.
    m(&["mailsplit", "-o.", "-b", "README.md"], Shape::Linear);
    m(&["mailsplit", "-o.", "-b", "README.md", "src/lib.rs"], Shape::Linear);
    m(&["mailsplit", "-o.", "-b", "-f5", "README.md"], Shape::Linear);
    m(&["mailsplit", "-o.", "-b", "-d3", "README.md"], Shape::Linear);
    m(&["mailsplit", "-o.", "-b", "--keep-cr", "README.md"], Shape::Linear);
    m(&["mailsplit", "-oout", "-b", "README.md"], Shape::Linear);
    m(&["mailsplit", "-o.", "-b", "\u{fc}\u{f1}\u{ef}\u{e7}\u{f8}d\u{e9}.txt"], Shape::AwkwardPaths);
    m(&["mailsplit", "-o.", "-b", "with space.txt"], Shape::AwkwardPaths);

    // Without -b the input must look like an mbox; these are the rejections.
    m(&["mailsplit", "-o.", "README.md"], Shape::Linear);
    m(&["mailsplit", "-o.", "src/lib.rs", "README.md"], Shape::Linear);
    m(&["mailsplit", "-o.", "/dev/null"], Shape::Linear);
    m(&["mailsplit", "-o.", "no-such"], Shape::Linear);
    m(&["mailsplit", "-onodir", "README.md"], Shape::Linear);
    // Empty stdin, and the missing -o usage error.
    m(&["mailsplit", "-o."], Shape::Linear);
    m(&["mailsplit"], Shape::Linear);
}

/// `send-email` — `--dry-run` keeps this offline, but with no valid patch every
/// invocation dies at the missing subject line, so the SMTP transport itself
/// stays out of reach. What these pin is the argument/config plumbing and the
/// exit status of the `die` paths.
fn send_email(out: &mut Vec<Case>) {
    let mut s = |args: &[&str]| out.push(Case::new("send-email", args, Shape::Linear));

    s(&["send-email"]);
    s(&["send-email", "--dry-run"]);
    s(&["send-email", "--dry-run", "--to=a@example.invalid", "README.md"]);
    s(&["send-email", "--dry-run", "--to=a@example.invalid", "--smtp-server=/bin/true", "README.md"]);
    s(&["send-email", "--dry-run", "--to=a@example.invalid", "--validate", "README.md"]);
    s(&["send-email", "--dry-run", "--to=a@example.invalid", "--confirm=bogus", "README.md"]);
    s(&["send-email", "--dry-run", "--to=a@example.invalid", "--suppress-cc=bogus", "README.md"]);
    s(&["send-email", "--dry-run", "--to=a@example.invalid", "--transfer-encoding=bogus", "README.md"]);
    // A directory argument expands to the files inside it.
    s(&["send-email", "--dry-run", "--to=a@example.invalid", "src"]);
    // Anything not a path is handed to `format-patch` as a revision range.
    s(&["send-email", "--dry-run", "--to=a@example.invalid", "no-such.patch"]);
    s(&["send-email", "--bogus-flag"]);
    s(&["send-email", "--dump-aliases"]);
    s(&["send-email", "--translate-aliases"]);

    // Same die path, but with a sendemail.* key present — stock's exit status
    // for these `die`s depends on whether that config lookup matched.
    out.push(Case::new(
        "send-email",
        &["-c", "sendemail.to=a@example.invalid", "send-email", "--dry-run", "README.md"],
        Shape::Linear,
    ));
    out.push(Case::new(
        "send-email",
        &[
            "-c",
            "sendemail.smtpserver=/bin/true",
            "send-email",
            "--dry-run",
            "--to=a@example.invalid",
            "README.md",
        ],
        Shape::Linear,
    ));
}

/// `imap-send` — no server, so this is the config-validation surface plus one
/// connection to a refused loopback port.
fn imap_send(out: &mut Vec<Case>) {
    let mut i = |args: &[&str]| out.push(Case::new("imap-send", args, Shape::Linear));

    i(&["imap-send"]);
    i(&["imap-send", "--curl"]);
    i(&["imap-send", "--no-curl"]);
    i(&["imap-send", "--list"]);
    i(&["imap-send", "--nonexistent-flag"]);
    out.push(Case::new("imap-send", &["-c", "imap.folder=INBOX", "imap-send"], Shape::Linear));
    out.push(Case::new(
        "imap-send",
        &["-c", "imap.host=imaps://127.0.0.1:1", "imap-send"],
        Shape::Linear,
    ));
    // Loopback port 1 refuses immediately; no name resolution is involved.
    out.push(Case::new(
        "imap-send",
        &[
            "-c",
            "imap.folder=INBOX",
            "-c",
            "imap.host=imap://127.0.0.1:1",
            "imap-send",
            "--list",
        ],
        Shape::Linear,
    ));
}

/// `request-pull` — fully testable, because the repository can stand in for its
/// own remote URL. The generated diffstat and shortlog are byte-compared.
fn request_pull(out: &mut Vec<Case>) {
    let mut r = |args: &[&str], shape| out.push(Case::new("request-pull", args, shape));

    r(&["request-pull", "HEAD~1", "."], Shape::Branched);
    r(&["request-pull", "HEAD~1", ".", "HEAD"], Shape::Branched);
    r(&["request-pull", "-p", "HEAD~1", "."], Shape::Branched);
    r(&["request-pull", "v0.1.0", ".", "main"], Shape::Branched);
    r(&["request-pull", "HEAD~2", "."], Shape::Merged);
    r(&["request-pull", "HEAD~1", ".", "HEAD"], Shape::Merged);
    // Nested paths in the range: the diffstat must not list the trees.
    r(&["request-pull", "HEAD~1", "."], Shape::AwkwardPaths);
    r(&["request-pull", "-p", "HEAD~1", "."], Shape::AwkwardPaths);
    r(&["request-pull", "HEAD~1", "."], Shape::Submodule);
    // Error paths: nothing to report, unknown remote, missing arguments.
    r(&["request-pull", "HEAD", "."], Shape::Branched);
    r(&["request-pull", "HEAD~1", "."], Shape::Linear);
    r(&["request-pull", "HEAD~1", "no-such-url"], Shape::Branched);
    r(&["request-pull"], Shape::Branched);
}

/// `interpret-trailers` — takes file arguments, so this one is fully exercised.
fn interpret_trailers(out: &mut Vec<Case>) {
    let mut t = |args: &[&str]| out.push(Case::new("interpret-trailers", args, Shape::Linear));

    t(&["interpret-trailers", "README.md"]);
    t(&["interpret-trailers", "--trailer", "Acked-by: A <a@example.invalid>", "README.md"]);
    t(&["interpret-trailers", "--in-place", "--trailer", "Acked-by: A <a@example.invalid>", "README.md"]);
    t(&["interpret-trailers", "--only-trailers", "README.md"]);
    t(&["interpret-trailers", "--only-input", "README.md"]);
    t(&["interpret-trailers", "--unfold", "README.md"]);
    t(&["interpret-trailers", "--parse", "README.md"]);
    t(&["interpret-trailers", "--no-divider", "README.md"]);
    t(&["interpret-trailers", "--trim-empty", "--trailer", "Empty:", "README.md"]);
    t(&["interpret-trailers", "--where=before", "--trailer", "X: y", "README.md"]);
    t(&["interpret-trailers", "--where=after", "--trailer", "X: y", "README.md"]);
    t(&["interpret-trailers", "--if-exists=addIfDifferent", "--trailer", "X: y", "README.md"]);
    t(&["interpret-trailers", "--if-missing=doNothing", "--trailer", "X: y", "README.md"]);
    t(&["interpret-trailers", "--trailer", "X: y", "README.md", "src/lib.rs"]);
    // Bad specs and bad enum values.
    t(&["interpret-trailers", "--trailer", "no-colon-here", "README.md"]);
    t(&["interpret-trailers", "--trailer", "README.md"]);
    t(&["interpret-trailers", "--where=bogus", "--trailer", "X: y", "README.md"]);
    t(&["interpret-trailers", "--if-exists=bogus", "--trailer", "X: y", "README.md"]);
    t(&["interpret-trailers", "--if-missing=bogus", "--trailer", "X: y", "README.md"]);
    t(&["interpret-trailers", "no-such-file"]);
    t(&["interpret-trailers"]);
    // Trailer config that supplies a token shorthand.
    out.push(Case::new(
        "interpret-trailers",
        &[
            "-c",
            "trailer.ack.key=Acked-by: ",
            "interpret-trailers",
            "--trailer",
            "ack: A <a@example.invalid>",
            "README.md",
        ],
        Shape::Linear,
    ));
    out.push(Case::new(
        "interpret-trailers",
        &["-c", "trailer.separators=:=", "interpret-trailers", "--trailer", "X=y", "README.md"],
        Shape::Linear,
    ));
}

/// `fmt-merge-msg` — its input is `FETCH_HEAD`-shaped and no shape ships one,
/// so this is the empty-input and malformed-input surface.
fn fmt_merge_msg(out: &mut Vec<Case>) {
    let mut f = |args: &[&str], shape| out.push(Case::new("fmt-merge-msg", args, shape));

    f(&["fmt-merge-msg"], Shape::Linear);
    f(&["fmt-merge-msg", "-F", "/dev/null"], Shape::Linear);
    f(&["fmt-merge-msg", "-F", "README.md"], Shape::Linear);
    f(&["fmt-merge-msg", "-F", "README.md"], Shape::Branched);
    f(&["fmt-merge-msg", "-F", "no-such"], Shape::Linear);
    f(&["fmt-merge-msg", "--log", "-F", "README.md"], Shape::Linear);
    f(&["fmt-merge-msg", "--no-log", "-F", "/dev/null"], Shape::Linear);
    f(&["fmt-merge-msg", "-m", "custom", "-F", "README.md"], Shape::Linear);
    f(&["fmt-merge-msg", "-F", "/dev/null"], Shape::Merged);
}

/// `quiltimport` — needs a quilt `series` file; no shape has one, so these pin
/// the missing-series diagnostics and the argument plumbing.
fn quiltimport(out: &mut Vec<Case>) {
    let mut q = |args: &[&str]| out.push(Case::new("quiltimport", args, Shape::Linear));

    q(&["quiltimport"]);
    q(&["quiltimport", "--dry-run"]);
    q(&["quiltimport", "--patches", "nowhere"]);
    q(&["quiltimport", "--dry-run", "--patches", "."]);
    q(&["quiltimport", "--dry-run", "--patches", ".", "--series", "README.md"]);
    q(&["quiltimport", "--dry-run", "--author", "A <a@example.invalid>"]);
    q(&["quiltimport", "--keep-non-patch"]);
}
