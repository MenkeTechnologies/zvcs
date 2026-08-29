//! Differential corpus cases for git's **helper protocols** — the places where
//! git stops doing the work itself and speaks a line protocol to a cooperating
//! program over a pipe.
//!
//! `credential`, `credential-store`, `credential-cache`, `remote-ext`,
//! `remote-fd`, the four `remote-{http,https,ftp,ftps}` aliases of remote-curl,
//! `send-email`, `imap-send`, `http-backend` and `request-pull`. Every one of
//! them has a porcelain face that is easy to reproduce and a wire behind it that
//! is easy to skip, and a port that implements only the face scores the same as
//! one that implements both wherever the corpus never makes the wire speak.
//!
//! # The rule that makes the wire measurable: the helper lives inside the case
//!
//! git's credential helpers are programs (`credential.c:credential_do`), and a
//! `Case` is one argv against a pristine fixture copy — it cannot install a
//! program first. It does not have to. A `credential.helper` value beginning
//! with `!` is run **through the shell**, so the whole helper can be a one-liner
//! carried in the case's own configuration:
//!
//! ```text
//! -c 'credential.helper=!f() { echo username=u; echo password=p; }; f'
//! ```
//!
//! Nothing is installed, nothing outside the fixture is executed, and the same
//! literal produces the same helper on both sides. Three shapes of one-liner are
//! used below and each measures a different branch of `credential_fill()`:
//!
//! * **answers** — `echo username=…; echo password=…`, so the fill loop stops at
//!   the first helper and never reaches the prompt.
//! * **declines** — `:`, exit 0 with no output, so the loop must go on to the
//!   next helper and then to the prompt.
//! * **fails** — `exit 3`, which git treats exactly like a decline; a port that
//!   propagates the helper's status instead is visible here and nowhere else.
//!
//! A fourth shape is the strongest probe in this file: a helper that writes what
//! it was handed into a file in the worktree
//! (`!f() { echo "op=$1" > cred-log; cat >> cred-log; }; f`). The runner's
//! worktree-content probe compares that file byte for byte, so the case measures
//! **the exact request git sent** — the operation name in argv, and which
//! description fields were forwarded. That is what pins `credential.useHttpPath`
//! (a `path=` line that is present or absent) and the `get`/`store`/`erase` verb
//! mapping behind `fill`/`approve`/`reject`, neither of which shows on stdout.
//!
//! `credential.helper=store --file=creds` is the same trick without a shell: the
//! stock file-backed helper, aimed at a **repository-relative** path, so the
//! credential file it writes lands under the same probe.
//!
//! # What `env::harden`'s pins make unreachable, precisely
//!
//! `harden` sets `GIT_ASKPASS=true` and `GIT_TERMINAL_PROMPT=0`. `git_prompt()`
//! (`prompt.c`) consults `GIT_ASKPASS`, then `core.askPass`, then `SSH_ASKPASS`,
//! and takes the first that is set — so with `GIT_ASKPASS` pinned:
//!
//! * **The askpass *mechanism* is reachable and is measured.** Every fill that
//!   no helper answered ends in a prompt, `true` writes nothing, and git reports
//!   `username=`/`password=` empty at exit 0. That answer is a fact about the
//!   prompt path and several cases below pin it.
//! * **`core.askPass` and `SSH_ASKPASS` are unreachable.** `GIT_ASKPASS` always
//!   wins, so a port that never reads either key is indistinguishable here. Two
//!   cases set `core.askPass` deliberately and assert that setting it changes
//!   **nothing**, which pins the precedence rather than the value.
//! * **The terminal prompt is unreachable.** It is only tried when no askpass
//!   program is configured at all, and one always is. `GIT_TERMINAL_PROMPT=0`
//!   therefore never fires either; both are pinned by `env::is_pinned` and a case
//!   may not clear them.
//!
//! # Fixture constraints this file works inside
//!
//! * **Nothing may touch the network.** No hostname a resolver could answer, no
//!   socket, no daemon. Every URL is under `example.invalid` (RFC 6761: never
//!   resolves) and exists only to be refused before a transport is opened.
//! * **`credential-cache store` starts a daemon and is therefore banned.**
//!   Measured on stock 2.55.0: `credential-cache store` with *empty* stdin
//!   creates `$HOME/.cache/git/credential/socket` and leaves a
//!   `git credential-cache--daemon` running. `get`, `erase` and `exit` do not —
//!   they attempt one connect, find nothing, and exit 0 silently. Only those are
//!   used, plus the option-parse refusals that fire before any of it.
//! * **`$HOME` is shared by both sides and by every case**
//!   (`fixture::Templates::home`), so a case that wrote `~/.git-credentials`
//!   would hand its state to the next case and to the other side. Every
//!   credential write here names `--file=<repository-relative path>`; none may
//!   ever be dropped.
//! * **`send-email`'s success path is not comparable and is excluded.**
//!   `git-send-email.perl:846` computes `$time = time - scalar $#files` and
//!   line 1619 renders it into `Date:`, with `Message-ID:` built from the same
//!   clock plus the pid. Any invocation that reaches message composition prints
//!   two values that differ between the two runs by construction. So every
//!   `send-email` case here stops *before* composition: option validation, alias
//!   files, and the `No subject line in <file>?` die. `patches/valid.patch` is
//!   the vehicle — a plain diff with no `Subject:` — which lets an option be
//!   accepted and observed without the message ever being built.
//! * **`imap-send` connects the moment stdin holds a message.** Every case here
//!   feeds it nothing, so it stops at `nothing to send` or at a config error.
//! * **A case is removed when the *port* reaches a resolver, not only when stock
//!   does.** Two `ls-remote ext::<cmd>` cases were written, run, and then taken
//!   out: stock refuses them at the `protocol.ext.allow` gate without spawning
//!   anything, but the binary under test does not recognise `ext::` as a
//!   transport, falls through to the scp-like `host:path` syntax and runs
//!   `ssh ext`, which performs a DNS lookup. See [`remote_ext`] for where that is
//!   recorded. `credential-cache --socket=<relative path> get` came out for the
//!   sibling reason: stock never spawns a daemon on a read, the port does.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    credential_front_end(out);
    credential_store(out);
    credential_cache(out);
    remote_ext(out);
    remote_fd(out);
    remote_curl(out);
    send_email(out);
    request_pull(out);
    imap_send(out);
    http_backend(out);
}

// ---------------------------------------------------------------------------
// Credential descriptions fed on stdin
// ---------------------------------------------------------------------------
//
// Written flush-left as single-line byte literals. A `\`-continued Rust string
// swallows the next line's leading whitespace, which silently rewrote every
// context line of an earlier payload set in this corpus; a one-line literal
// cannot express that bug. Each payload's bytes are checkable against the
// `stdin[<len>B/<hash>]` segment `--list-cases` prints for the case.

/// The minimum git accepts: a protocol and a host, terminated by a blank line.
const CRED_HTTPS: &[u8] = b"protocol=https\nhost=example.invalid\n\n";
/// The same, carrying a path — invisible to the helper unless
/// `credential.useHttpPath` is on.
const CRED_PATH: &[u8] = b"protocol=https\nhost=example.invalid\npath=a/b.git\n\n";
/// A complete credential: what `approve`/`reject` forward to the helper, and
/// what `store` needs before it will write anything.
const CRED_FULL: &[u8] =
    b"protocol=https\nhost=example.invalid\nusername=bob\npassword=s3cret\n\n";
/// The `url=` shorthand, which git expands into the individual fields
/// (`credential.c:credential_from_url`) before any helper sees it.
const CRED_URL: &[u8] = b"url=https://bob:s3cret@example.invalid/x.git\n\n";
/// A description with a username but no password: `approve` must not call the
/// helper for this, because there is nothing to store.
const CRED_USER_ONLY: &[u8] = b"protocol=https\nhost=example.invalid\nusername=bob\n\n";
/// No host. `credential_fill` refuses outright rather than asking anyone.
const CRED_NO_HOST: &[u8] = b"protocol=https\n\n";
/// A line that is not `key=value`: a warning and then a fatal read failure.
const CRED_JUNK: &[u8] = b"not-a-key-value-line\n\n";
/// A protocol-v1 `capability` line, which `credential` answers on its own.
const CRED_CAPABILITY: &[u8] = b"capability authtype\n\n";

// ---------------------------------------------------------------------------
// In-fixture credential helpers
// ---------------------------------------------------------------------------

/// Answers the request outright, so the fill loop never reaches the prompt.
const HELPER_ANSWER: &str = "!f() { echo username=u; echo password=p; }; f";
/// Runs, prints nothing, exits 0 — a decline. The loop must continue.
const HELPER_DECLINE: &str = "!f() { :; }; f";
/// Exits non-zero. git treats this exactly like a decline; a port that
/// propagates the status instead is only visible here.
const HELPER_FAIL: &str = "!f() { exit 3; }; f";
/// A second, distinguishable answer, used to pin *which* of two chained helpers
/// git took the answer from.
const HELPER_SECOND: &str = "!g() { echo username=second; echo password=sp; }; g";
/// Records the operation git passed in argv and the request it wrote on stdin
/// into a worktree file the state probe compares byte for byte.
const HELPER_LOG: &str = "!f() { echo \"op=$1\" > cred-log; cat >> cred-log; }; f";
/// The stock file-backed helper, aimed inside the fixture.
const HELPER_STORE: &str = "store --file=creds";

// ---------------------------------------------------------------------------
// credential
// ---------------------------------------------------------------------------

/// The `credential` front-end: `fill`, `approve`, `reject`, `capability`.
///
/// What a port gets wrong without these: the front-end is three thin wrappers
/// over `credential_fill`/`approve`/`reject`, so reproducing its *stdout* takes
/// only an echo of the description. What it does not take is running a helper,
/// mapping `fill`→`get` / `approve`→`store` / `reject`→`erase`, stopping the loop
/// at the first helper that answered, treating a non-zero helper as a decline,
/// resetting the helper list on an empty value, or withholding the `path=` field
/// unless `credential.useHttpPath` is set. Every one of those is invisible on
/// stdout alone; each is measured here through the answer git prints, or through
/// the file `HELPER_LOG` leaves in the worktree.
fn credential_front_end(out: &mut Vec<Case>) {
    let with = |args: &[&str], cfg: &[(&str, &str)], input, out: &mut Vec<Case>| {
        out.push(Case::with_stdin("credential", args, Shape::Linear, input).with_config(cfg));
    };

    // ---- fill: which helper answers, and what happens when none does ----
    with(&["credential", "fill"], &[("credential.helper", HELPER_ANSWER)], CRED_HTTPS, out);
    with(&["credential", "fill"], &[("credential.helper", HELPER_DECLINE)], CRED_HTTPS, out);
    // A helper that exits non-zero is a decline, not an abort: same answer as the
    // line above, same exit code.
    with(&["credential", "fill"], &[("credential.helper", HELPER_FAIL)], CRED_HTTPS, out);
    // No helper at all: the prompt runs, `GIT_ASKPASS=true` prints nothing, and
    // git reports both fields empty at exit 0. This is the askpass mechanism, and
    // it is the half of the prompt path the pins leave reachable.
    out.push(Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, CRED_HTTPS));
    // `core.askPass` is consulted only when `GIT_ASKPASS` is unset, and `harden`
    // always sets it. This must print exactly what the case above did: the
    // setting is measured by being ignored.
    with(&["credential", "fill"], &[("core.askPass", "echo")], CRED_HTTPS, out);
    // A helper name with no such program behind it. git says
    // `credential-<name> is not a git command` on stderr and carries on to the
    // prompt at exit 0 — a decline, not a failure.
    with(&["credential", "fill"], &[("credential.helper", "no-such-helper-xyz")], CRED_HTTPS, out);

    // ---- two helpers, tried in configuration order ----
    // First declines, second answers: the answer must be the second one's.
    with(
        &["credential", "fill"],
        &[("credential.helper", HELPER_DECLINE), ("credential.helper", HELPER_SECOND)],
        CRED_HTTPS,
        out,
    );
    // Both answer: the loop stops at the first, so `second` must not appear.
    with(
        &["credential", "fill"],
        &[("credential.helper", HELPER_ANSWER), ("credential.helper", HELPER_SECOND)],
        CRED_HTTPS,
        out,
    );
    // An empty `credential.helper` **resets** the accumulated list, so the order
    // of these two decides whether anything runs at all. Measured on stock
    // 2.55.0: empty-then-helper answers `username=u`, helper-then-empty answers
    // `username=`.
    with(
        &["credential", "fill"],
        &[("credential.helper", ""), ("credential.helper", HELPER_ANSWER)],
        CRED_HTTPS,
        out,
    );
    with(
        &["credential", "fill"],
        &[("credential.helper", HELPER_ANSWER), ("credential.helper", "")],
        CRED_HTTPS,
        out,
    );

    // ---- credential.<url>.* : does the URL pattern match this request ----
    with(
        &["credential", "fill"],
        &[("credential.https://example.invalid.helper", HELPER_ANSWER)],
        CRED_HTTPS,
        out,
    );
    with(
        &["credential", "fill"],
        &[("credential.https://other.invalid.helper", HELPER_ANSWER)],
        CRED_HTTPS,
        out,
    );
    // Scheme mismatch: an `http://` section must not answer an `https://` request.
    with(
        &["credential", "fill"],
        &[("credential.http://example.invalid.helper", HELPER_ANSWER)],
        CRED_HTTPS,
        out,
    );
    // A configured username seeds the description before any helper runs, so it
    // survives a decline.
    with(
        &["credential", "fill"],
        &[
            ("credential.https://example.invalid.username", "alice"),
            ("credential.helper", HELPER_DECLINE),
        ],
        CRED_HTTPS,
        out,
    );

    // ---- what git actually sends the helper ----
    // The log file is the measurement. Without `useHttpPath` the `path=` line is
    // withheld; with it the line is forwarded — a difference that never reaches
    // stdout, because `fill` echoes the description it was given either way.
    with(&["credential", "fill"], &[("credential.helper", HELPER_LOG)], CRED_PATH, out);
    with(
        &["credential", "fill"],
        &[("credential.helper", HELPER_LOG), ("credential.useHttpPath", "true")],
        CRED_PATH,
        out,
    );
    with(
        &["credential", "fill"],
        &[
            ("credential.helper", HELPER_LOG),
            ("credential.https://example.invalid.useHttpPath", "true"),
        ],
        CRED_PATH,
        out,
    );
    // `url=` is expanded into fields before the helper sees anything, so the log
    // is the only place the expansion is visible.
    with(&["credential", "fill"], &[("credential.helper", HELPER_LOG)], CRED_URL, out);

    // ---- approve maps to `store`, reject maps to `erase` ----
    with(&["credential", "approve"], &[("credential.helper", HELPER_LOG)], CRED_FULL, out);
    with(&["credential", "reject"], &[("credential.helper", HELPER_LOG)], CRED_FULL, out);
    // Nothing to store: `approve` must not invoke the helper at all, so no
    // `cred-log` may appear. Measured on stock 2.55.0.
    with(&["credential", "approve"], &[("credential.helper", HELPER_LOG)], CRED_USER_ONLY, out);
    // No helper configured: the verb is a complete no-op.
    out.push(Case::with_stdin("credential", &["credential", "reject"], Shape::Linear, CRED_FULL));

    // ---- the stock file helper, reached through the front end ----
    // These write `creds` into the worktree, where the probe reads its bytes.
    with(&["credential", "approve"], &[("credential.helper", HELPER_STORE)], CRED_FULL, out);
    with(&["credential", "approve"], &[("credential.helper", HELPER_STORE)], CRED_URL, out);
    // Two helpers on the write side: `store` runs *and* the logger runs, because
    // `approve` broadcasts rather than stopping at the first answer.
    with(
        &["credential", "approve"],
        &[("credential.helper", HELPER_STORE), ("credential.helper", HELPER_LOG)],
        CRED_FULL,
        out,
    );

    // ---- capability ----
    with(
        &["credential", "capability"],
        &[("credential.helper", HELPER_ANSWER)],
        CRED_CAPABILITY,
        out,
    );

    // ---- the descriptions git refuses, where the message is the contract ----
    for input in [CRED_NO_HOST, CRED_JUNK, CRED_CAPABILITY] {
        out.push(
            Case {
                compare_stderr: true,
                ..Case::with_stdin("credential", &["credential", "fill"], Shape::Linear, input)
            }
            .with_config(&[("credential.helper", HELPER_ANSWER)]),
        );
    }
}

// ---------------------------------------------------------------------------
// credential-store
// ---------------------------------------------------------------------------

/// `credential-store`, always against a **repository-relative** `--file`.
///
/// The `--file` is never omitted, and that is not tidiness: with no `--file` the
/// helper writes `$HOME/.git-credentials`, and the harness gives both sides and
/// every case the same `$HOME`. A single case that stored there would leak its
/// credential into the next case and across the two runs. Pointing `--file`
/// inside the fixture puts the file under the worktree-content probe instead,
/// which compares its bytes — so these measure not only the exit code but the
/// *format* of the line the helper writes
/// (`https://bob:s3cret@example.invalid`, with the path attached only when the
/// request carried one).
///
/// What a port gets wrong without these: `store` writing the URL with the path
/// when it should not, `erase` truncating the file rather than removing the
/// matching line, and `get` answering from a file whose lines do not parse as
/// URLs at all.
fn credential_store(out: &mut Vec<Case>) {
    let cs = |args: &[&str], input, out: &mut Vec<Case>| {
        out.push(Case::with_stdin("credential-store", args, Shape::Linear, input));
    };

    // Write, and then the same write from the `url=` shorthand — that one carries
    // a path, so the stored line does too.
    cs(&["credential-store", "--file=creds", "store"], CRED_FULL, out);
    cs(&["credential-store", "--file=creds", "store"], CRED_URL, out);
    // Incomplete descriptions: nothing to write, and no file may appear.
    cs(&["credential-store", "--file=creds", "store"], CRED_HTTPS, out);
    // Read and delete against a store that does not exist yet. A case is one
    // invocation, so it is always empty on entry; what is measured is that an
    // absent store is silence at exit 0, not an error and not a stray file.
    cs(&["credential-store", "--file=creds", "get"], CRED_FULL, out);
    cs(&["credential-store", "--file=creds", "get"], CRED_HTTPS, out);
    cs(&["credential-store", "--file=creds", "erase"], CRED_FULL, out);
    // A file that exists and is not a credential store. Every line fails to parse
    // as a URL; the contract is that `get` says nothing rather than failing, and
    // that the file is left alone — which the probe checks, because `src/lib.rs`
    // is tracked and any rewrite shows up in `status`.
    cs(&["credential-store", "--file=src/lib.rs", "get"], CRED_FULL, out);
    // A store in a subdirectory that already exists.
    cs(&["credential-store", "--file=src/creds", "store"], CRED_FULL, out);
    cs(&["credential-store", "--file=src/creds", "get"], CRED_FULL, out);
    // Refusals. The lock cannot be taken when the directory is absent, and
    // `--timeout` is not an option this helper has.
    out.push(Case {
        compare_stderr: true,
        ..Case::with_stdin(
            "credential-store",
            &["credential-store", "--file=no/such/dir/creds", "store"],
            Shape::Linear,
            CRED_FULL,
        )
    });
    out.push(Case::strict(
        "credential-store",
        &["credential-store", "--file=creds", "--timeout=5", "store"],
        Shape::Linear,
    ));
}

// ---------------------------------------------------------------------------
// credential-cache
// ---------------------------------------------------------------------------

/// `credential-cache`, restricted to the operations that create no socket.
///
/// **`store` is deliberately absent.** Measured on stock 2.55.0 with an empty
/// `$HOME`: `git credential-cache store` — even with *empty* stdin — creates
/// `$HOME/.cache/git/credential/socket` and leaves a `git credential-cache--daemon`
/// running against it. A corpus case that did that would outlive its own fixture,
/// share a socket with every later case, and leave a process behind on both
/// sides. `get`, `erase` and `exit` attempt one connect, find nothing listening,
/// and exit 0 in silence with no directory created — verified in the same run.
///
/// What is left is the argument surface plus the three silent no-ops, and that is
/// the whole of what this command can contribute without a daemon. It is still a
/// real slice: a port that answered `get` with anything at all, or that exited
/// non-zero because no daemon was running, fails here.
fn credential_cache(out: &mut Vec<Case>) {
    let cc = |args: &[&str], input, out: &mut Vec<Case>| {
        out.push(Case::with_stdin("credential-cache", args, Shape::Linear, input));
    };

    cc(&["credential-cache", "get"], CRED_HTTPS, out);
    cc(&["credential-cache", "erase"], CRED_FULL, out);
    cc(&["credential-cache", "exit"], CRED_FULL, out);
    cc(&["credential-cache", "--timeout=30", "get"], CRED_HTTPS, out);
    // `--socket=<relative path>` is deliberately **absent**. Stock's `get` never
    // spawns the daemon, so a relative socket path is only resolved and connected
    // to; the binary under test answers the same request with
    // `fatal: socket directory must be an absolute path` followed by
    // `fatal: cache daemon did not start`, i.e. it takes the *spawn* branch on a
    // read. The spawn is refused by the same absolute-path check today, so nothing
    // is left running — but a case that survives only because the spawn it
    // triggers happens to fail is not one to leave in a corpus that must never
    // start a daemon.
    // The daemon's own argument check, before it binds anything.
    out.push(Case::strict(
        "credential-cache--daemon",
        &["credential-cache--daemon", "--debug"],
        Shape::Linear,
    ));
    // Option-parse refusals, which fire before the action word is looked at — and
    // therefore before the branch that would spawn a daemon.
    out.push(Case::strict(
        "credential-cache",
        &["credential-cache", "--timeout=not-a-number", "get"],
        Shape::Linear,
    ));
    out.push(Case::strict("credential-cache", &["credential-cache", "--timeout"], Shape::Linear));
}

// ---------------------------------------------------------------------------
// remote-ext
// ---------------------------------------------------------------------------

/// `remote-ext` — the transport helper that runs a command you name in the URL.
///
/// Invoked **directly**, which is the form where the URL argument arrives already
/// stripped of its `ext::` prefix; `remote-ext.c` runs the rest as a command after
/// expanding the `%s`/`%S`/`%G`/`%V`/`%%` placeholders. Two things make it
/// measurable inside a fixture: the helper loop reads its commands from stdin, so
/// a case with **no** stdin never spawns anything at all, and the placeholder
/// parser rejects an unknown letter before the command is assembled.
///
/// `%G` alone is deliberately absent: measured on stock 2.55.0 it reaches
/// `BUG: run-command.c:413: command is empty` and aborts on SIGABRT, which is a
/// crash to compare rather than a behaviour.
///
/// What a port gets wrong without these: implementing the URL parse and not the
/// helper loop, so `capabilities` answers nothing; or implementing the loop and
/// not the placeholder table, so `%q` reaches the shell instead of being refused.
fn remote_ext(out: &mut Vec<Case>) {
    // No stdin: the loop reads EOF immediately, so the named command is never run
    // and the exit code is the whole answer.
    for url in ["sh -c :", "no-such-program-xyz", "sh -c : %s", "sh -c : %S", "sh -c : %%"] {
        out.push(Case::new("remote-ext", &["remote-ext", "origin", url], Shape::Linear));
    }
    // `capabilities` is answered by the helper itself out of a static table, so
    // no child is spawned even though the loop is running.
    out.push(Case::with_stdin(
        "remote-ext",
        &["remote-ext", "origin", "sh -c :"],
        Shape::Linear,
        b"capabilities\n",
    ));
    // `connect` is the only command that spawns. `sh -c :` exits at once with an
    // empty stream, so the transport ends immediately; the absent program takes
    // the `cannot run` branch instead.
    out.push(Case::with_stdin(
        "remote-ext",
        &["remote-ext", "origin", "sh -c :"],
        Shape::Linear,
        b"connect git-upload-pack\n\n",
    ));
    out.push(Case::with_stdin(
        "remote-ext",
        &["remote-ext", "origin", "no-such-program-xyz"],
        Shape::Linear,
        b"connect git-upload-pack\n\n",
    ));
    // An unknown placeholder is refused while the URL is parsed, before anything
    // is spawned — the message is the contract.
    out.push(Case::strict("remote-ext", &["remote-ext", "origin", "sh -c : %q"], Shape::Linear));
    // The transport *gate* — `ls-remote ext::<cmd>`, which stock refuses with
    // `fatal: transport 'ext' not allowed` because `protocol.ext.allow` defaults
    // to `never` — is deliberately **absent**, and the reason is a defect rather
    // than a limitation. Measured against the binary under test at
    // `target/debug/git`: `ls-remote ext::sh -c :` does not recognise `ext::` as a
    // transport at all, falls through to the scp-like `host:path` syntax, and runs
    // `ssh ext` — which answers
    // `ssh: Could not resolve hostname ext`. That is a **name resolution**, and a
    // case whose port side reaches a resolver is nondeterministic by construction,
    // so it cannot live in this corpus while the behaviour stands. The gate is
    // still reachable from the helper side above, where nothing spawns.
}

// ---------------------------------------------------------------------------
// remote-fd
// ---------------------------------------------------------------------------

/// `remote-fd` — a transport that reads and writes numbered file descriptors.
///
/// The descriptors named here are **closed**: the harness gives a child stdin,
/// stdout and stderr and nothing else, so `3` and `3,4` are guaranteed absent on
/// both sides. That makes the URL parser reachable without a peer and makes the
/// `connect` path terminate in a bounded, identical failure instead of blocking.
///
/// `ls-remote fd::<n>` is deliberately **not** here: measured on stock 2.55.0 it
/// does not terminate — the read error is reported and the transport then waits on
/// a stream that will never close (killed at an 8-second timeout).
fn remote_fd(out: &mut Vec<Case>) {
    // URL parsing, with no stdin so nothing is dup'd.
    for url in ["3", "3,4"] {
        out.push(Case::new("remote-fd", &["remote-fd", "origin", url], Shape::Linear));
    }
    // The two URL shapes git refuses outright — including `fd::3`, which is what a
    // caller who forgot that the prefix is already stripped would pass.
    out.push(Case::strict("remote-fd", &["remote-fd", "origin", "not-a-number"], Shape::Linear));
    out.push(Case::strict("remote-fd", &["remote-fd", "origin", "fd::3"], Shape::Linear));
    // Commands. `capabilities` is answered from the table; `connect` reaches the
    // closed descriptor and fails there.
    out.push(Case::with_stdin(
        "remote-fd",
        &["remote-fd", "origin", "3"],
        Shape::Linear,
        b"capabilities\n",
    ));
    out.push(Case::with_stdin(
        "remote-fd",
        &["remote-fd", "origin", "3"],
        Shape::Linear,
        b"connect git-upload-pack\n\n",
    ));
    out.push(Case::with_stdin(
        "remote-fd",
        &["remote-fd", "origin", "3,4"],
        Shape::Linear,
        b"connect git-upload-pack\n\n",
    ));
}

// ---------------------------------------------------------------------------
// remote-http / remote-https / remote-ftp / remote-ftps
// ---------------------------------------------------------------------------

/// The four names remote-curl is installed under, on their argument-validation
/// paths only.
///
/// **No case here may reach a network, and none can.** Two properties keep it that
/// way and both were measured on stock 2.55.0. A URL with no scheme is refused
/// while the credential description is built from it (`warning: url has no scheme`
/// then `fatal: credential url cannot be parsed`), which happens before curl is
/// initialized. And a *well-formed* URL is accepted, after which the helper reads
/// its command loop from stdin — with no stdin it sees EOF and exits 1 without
/// ever issuing a request. Every URL is under `example.invalid`, which RFC 6761
/// reserves as never-resolving, so even the accepted ones name nothing a resolver
/// could answer.
///
/// What a port gets wrong without these: treating the four names as one alias of a
/// single unimplemented stub, and so agreeing on the usage error while disagreeing
/// on every URL that gets past it.
fn remote_curl(out: &mut Vec<Case>) {
    for cmd in ["remote-http", "remote-https", "remote-ftp", "remote-ftps"] {
        // A remote name in the URL slot: no scheme, so the credential parse
        // refuses it. The message names the URL git built, which also pins the
        // trailing `/` git appends to it.
        out.push(Case::strict(cmd, &[cmd, "origin", "origin"], Shape::Linear));
        // A well-formed URL that is accepted, followed by EOF on the command
        // stream. Nothing is requested; the exit code is the whole answer.
        out.push(Case::new(
            cmd,
            &[cmd, "origin", "https://example.invalid/repo.git"],
            Shape::Linear,
        ));
    }
    // One more shape of a bad URL, on one of the four — the parser is shared, so
    // repeating it four times would measure the same code four times.
    out.push(Case::strict(
        "remote-http",
        &["remote-http", "origin", "/absolute/path"],
        Shape::Linear,
    ));
}

// ---------------------------------------------------------------------------
// send-email
// ---------------------------------------------------------------------------

/// `send-email` on the paths that stop before a message is composed.
///
/// Why the restriction is not a choice: `git-send-email.perl:846` sets
/// `$time = time - scalar $#files`, and line 1619 renders `format_2822_time($time++)`
/// into the `Date:` header, with `Message-ID:` built from the same clock plus the
/// process id. There is no option and no environment variable that pins either. So
/// an invocation that reaches `gen_header()` prints two values that differ between
/// the two sides by construction, and would score as a difference forever.
///
/// `mail_patch.rs` already runs the flag surface on `Shape::Linear`, where nothing
/// in the worktree is a patch. This runs the surface that needs a real patch tree,
/// on [`Shape::Patches`], in three deterministic groups:
///
/// * **`patches/valid.patch`** — a plain diff with no `Subject:`, so every option
///   is parsed and accepted and the run then dies at
///   `No subject line in patches/valid.patch?` with the file name on stdout and
///   exit 255. That separates "the option was accepted" (255, file name printed)
///   from "the option was rejected" (1, nothing printed), which is the only
///   distinction available without composing a message.
/// * **alias tables** — `--dump-aliases` and `--translate-aliases` read a file and
///   print; neither touches the clock. `mail/one.eml` read as a *sendmail* alias
///   file parses its `Date:`/`From:`/`Subject:` headers as three real aliases, so
///   this is a populated table rather than an empty one, and the three other
///   `sendemail.aliasfiletype` parsers must each find nothing in the same bytes.
/// * **refusals** — a `--suppress-cc` field, a `--confirm` setting, and
///   `--relogin-delay` without `--batch-size`.
fn send_email(out: &mut Vec<Case>) {
    let se = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("send-email", args, Shape::Patches));
    };

    // Options accepted, then the missing-subject die. Each would have shaped a
    // header that is never built, so what is measured is acceptance.
    se(&["send-email", "--dry-run", "--to=a@example.invalid", "patches/valid.patch"], out);
    se(&["send-email", "--dry-run", "--cc=c@example.invalid", "patches/valid.patch"], out);
    se(&["send-email", "--dry-run", "--bcc=b@example.invalid", "patches/valid.patch"], out);
    se(
        &[
            "send-email",
            "--dry-run",
            "--to=a@example.invalid",
            "--in-reply-to=<seed@example.invalid>",
            "patches/valid.patch",
        ],
        out,
    );
    se(
        &[
            "send-email",
            "--dry-run",
            "--to=a@example.invalid",
            "--subject=Overridden",
            "patches/valid.patch",
        ],
        out,
    );
    se(
        &["send-email", "--dry-run", "--to=a@example.invalid", "--annotate", "patches/valid.patch"],
        out,
    );
    se(
        &["send-email", "--dry-run", "--to=a@example.invalid", "--no-thread", "patches/valid.patch"],
        out,
    );
    se(
        &[
            "send-email",
            "--dry-run",
            "--to=a@example.invalid",
            "--suppress-cc=all",
            "patches/valid.patch",
        ],
        out,
    );
    // A sendmail-style program path, which is what `--smtp-server` means when the
    // value looks like one. It names a file *inside the fixture* and is never
    // executed: the run dies at the subject first.
    se(
        &[
            "send-email",
            "--dry-run",
            "--to=a@example.invalid",
            "--smtp-server=patches/valid.patch",
            "patches/valid.patch",
        ],
        out,
    );
    se(
        &[
            "send-email",
            "--dry-run",
            "--to=a@example.invalid",
            "--batch-size=2",
            "--relogin-delay=1",
            "patches/valid.patch",
        ],
        out,
    );
    // A patch file that is not there: `send-email` hands anything it cannot open
    // to `format-patch` as a revision range, which is where the error comes from.
    se(&["send-email", "--dry-run", "--to=a@example.invalid", "patches/no-such.patch"], out);
    // A patch whose hunk header is damaged. It has no subject either, so the run
    // still dies before composition — the file name on stdout is the difference.
    se(&["send-email", "--dry-run", "--to=a@example.invalid", "patches/corrupt.patch"], out);

    // Alias tables. The file is inside the fixture and is read, never written.
    let alias = |ftype: &str, args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("send-email", args, Shape::Patches).with_config(&[
            ("sendemail.aliasesfile", "mail/one.eml"),
            ("sendemail.aliasfiletype", ftype),
        ]));
    };
    alias("sendmail", &["send-email", "--dump-aliases"], out);
    alias("mailrc", &["send-email", "--dump-aliases"], out);
    alias("mutt", &["send-email", "--dump-aliases"], out);
    alias("pine", &["send-email", "--dump-aliases"], out);
    // The same table, queried by name on stdin. `From` and `Subject` resolve;
    // `nobody` is not an alias and must come back unchanged.
    out.push(
        Case::with_stdin(
            "send-email",
            &["send-email", "--translate-aliases"],
            Shape::Patches,
            b"From\nSubject\nnobody\n",
        )
        .with_config(&[
            ("sendemail.aliasesfile", "mail/one.eml"),
            ("sendemail.aliasfiletype", "sendmail"),
        ]),
    );

    // Argument-validation refusals, each dying before any patch is opened.
    for args in [
        &[
            "send-email",
            "--dry-run",
            "--to=a@example.invalid",
            "--suppress-cc=bogus",
            "patches/valid.patch",
        ][..],
        &[
            "send-email",
            "--dry-run",
            "--to=a@example.invalid",
            "--confirm=bogus",
            "patches/valid.patch",
        ],
        &[
            "send-email",
            "--dry-run",
            "--to=a@example.invalid",
            "--relogin-delay=1",
            "patches/valid.patch",
        ],
    ] {
        out.push(Case::strict("send-email", args, Shape::Patches));
    }
}

// ---------------------------------------------------------------------------
// request-pull
// ---------------------------------------------------------------------------

/// `request-pull` over the shapes that did not exist when the rest of its corpus
/// was written.
///
/// `mail_patch.rs` covers `Branched`/`Merged`/`AwkwardPaths`/`Submodule` and
/// `mail_series.rs` covers `BehindRemote`/`Renamed`/`Whitespace`/`Patches`/
/// `Octopus`. What none of them can express:
///
/// * **[`Shape::Unrelated`]** — two roots with no merge base, so
///   `request-pull main . alien` has to take the branch that reports
///   `No commits in common` at exit 1 with nothing on stdout. Every other shape
///   has a merge base by construction, so that branch was unreachable.
/// * **[`Shape::TagChain`]** — a ref argument that is an *annotated tag*, and one
///   that is a tag three deep. When the named ref is a tag, request-pull prints the
///   **tag message** in place of the shortlog, which no case naming a branch has
///   ever produced. Tags pointing at a blob and at a tree give the refusal for a
///   ref that does not peel to a commit.
/// * **[`Shape::Shallow`]** — a truncated history, where `HEAD~2` does not resolve
///   at all and the same argument succeeds in every other shape, and where the
///   peer inside the fixture holds refs the local repository does not name.
fn request_pull(out: &mut Vec<Case>) {
    let rp = |args: &[&str], shape, out: &mut Vec<Case>| {
        out.push(Case::new("request-pull", args, shape));
    };

    // Unrelated. The normal path on `main`, then the same message asked for on the
    // far root, whose tag is the published name.
    rp(&["request-pull", "HEAD~1", "."], Shape::Unrelated, out);
    rp(&["request-pull", "HEAD~1", ".", "main"], Shape::Unrelated, out);
    rp(&["request-pull", "-p", "HEAD~1", "."], Shape::Unrelated, out);
    rp(&["request-pull", "alien~1", ".", "alien"], Shape::Unrelated, out);
    rp(&["request-pull", "alien~1", ".", "alien-tip"], Shape::Unrelated, out);
    // A repository URL that is not a repository. The head of the message is
    // printed before the peer is contacted, so stdout is non-empty at exit 1.
    rp(&["request-pull", "HEAD~1", "./.remote.git"], Shape::Unrelated, out);
    // No merge base: exit 1 with nothing printed, in both directions.
    out.push(Case::strict("request-pull", &["request-pull", "main", ".", "alien"], Shape::Unrelated));
    out.push(Case::strict("request-pull", &["request-pull", "alien", ".", "main"], Shape::Unrelated));

    // TagChain: the named ref is a tag, so the tag message replaces the shortlog.
    rp(&["request-pull", "HEAD~2", "."], Shape::TagChain, out);
    rp(&["request-pull", "-p", "HEAD~2", "."], Shape::TagChain, out);
    rp(&["request-pull", "inner", "."], Shape::TagChain, out);
    rp(&["request-pull", "inner", ".", "main"], Shape::TagChain, out);
    rp(&["request-pull", "HEAD~2", ".", "outermost"], Shape::TagChain, out);
    // A lightweight ref at a tag object: the same three-deep peel reached through a
    // ref that is not itself a tag.
    rp(&["request-pull", "HEAD~2", ".", "light-to-tag"], Shape::TagChain, out);
    // Tags that do not peel to a commit.
    out.push(Case::strict(
        "request-pull",
        &["request-pull", "HEAD~2", ".", "blobtag"],
        Shape::TagChain,
    ));
    out.push(Case::strict(
        "request-pull",
        &["request-pull", "HEAD~2", ".", "treetag"],
        Shape::TagChain,
    ));

    // Shallow: what is and is not reachable below the graft.
    rp(&["request-pull", "HEAD~1", "."], Shape::Shallow, out);
    rp(&["request-pull", "origin/main", ".", "main"], Shape::Shallow, out);
    rp(&["request-pull", "HEAD~1", "./.remote.git"], Shape::Shallow, out);
    // Past the graft: the revision does not exist in this repository.
    out.push(Case::strict("request-pull", &["request-pull", "HEAD~2", "."], Shape::Shallow));
    // A ref the peer holds that the local repository does not name.
    out.push(Case::strict(
        "request-pull",
        &["request-pull", "HEAD~1", "./.remote.git", "sh-side"],
        Shape::Shallow,
    ));
}

// ---------------------------------------------------------------------------
// imap-send
// ---------------------------------------------------------------------------

/// `imap-send`'s configuration parser, with **nothing on stdin**.
///
/// The command reads a mailbox from stdin and only then opens a connection, so an
/// empty payload is what keeps this offline: git stops at `nothing to send` after
/// the whole `imap.*` block has been validated. That ordering is itself the
/// measurement — a bad boolean is a fatal at 128 *before* `nothing to send`, a
/// missing host is an error at 1 with two hint lines, and a host that is merely
/// unreachable never gets that far.
///
/// `mail_patch.rs` runs the flag surface on `Shape::Linear`; these run the config
/// table on [`Shape::Patches`], where a mailbox exists in the worktree and the
/// command still must not read it.
fn imap_send(out: &mut Vec<Case>) {
    let is = |cfg: &[(&str, &str)], args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("imap-send", args, Shape::Patches).with_config(cfg));
    };

    // No host at all: the error and its hint lines.
    is(&[("imap.folder", "INBOX")], &["imap-send"], out);
    is(&[], &["imap-send", "--folder=INBOX"], out);
    // Host present, so validation completes and the empty payload ends it.
    is(&[("imap.folder", "INBOX"), ("imap.host", "imap://127.0.0.1:1")], &["imap-send"], out);
    is(
        &[
            ("imap.folder", "INBOX"),
            ("imap.host", "imap://127.0.0.1:1"),
            ("imap.user", "u"),
            ("imap.pass", "p"),
        ],
        &["imap-send"],
        out,
    );
    // A tunnel instead of a host: the command is named but never run, because
    // there is nothing to send.
    is(&[("imap.folder", "INBOX"), ("imap.tunnel", "sh -c :")], &["imap-send"], out);
    // A scheme imap-send does not know, and the curl/no-curl split.
    is(&[("imap.folder", "INBOX"), ("imap.host", "bogus://example.invalid")], &["imap-send"], out);
    is(
        &[
            ("imap.folder", "INBOX"),
            ("imap.host", "imap://127.0.0.1:1"),
            ("imap.authMethod", "CRAM-MD5"),
        ],
        &["imap-send", "--curl"],
        out,
    );
    // Booleans git refuses outright, before the payload is read.
    for key in ["imap.preformattedHTML", "imap.sslverify"] {
        out.push(Case::strict("imap-send", &["imap-send"], Shape::Patches).with_config(&[
            ("imap.folder", "INBOX"),
            ("imap.host", "imap://127.0.0.1:1"),
            (key, "maybe"),
        ]));
    }
}

// ---------------------------------------------------------------------------
// http-backend
// ---------------------------------------------------------------------------

/// `http-backend` — a CGI whose entire input is the environment.
///
/// Nothing here is a request over a socket: the program is run directly with the
/// variables a server would have set, and it writes the response to stdout.
///
/// The repository it serves is named through `PATH_TRANSLATED={repo}/…` rather
/// than through the `GIT_PROJECT_ROOT` + `PATH_INFO` pair, and that is forced
/// rather than chosen: `runner::apply_case_env` asserts that no case environment
/// value begins with `/`, because such a value would name one side's copy, and a
/// `PATH_INFO` is a URL path that begins with `/` by construction.
/// `PATH_TRANSLATED` is the other half of the same contract in
/// `http-backend.c` — the server-translated absolute path, used when no project
/// root is set — and it starts with [`crate::runner::REPO_PLACEHOLDER`], which is
/// substituted per side. The path is echoed back inside git's own 404 text and
/// the runner's `mask_paths` rewrites it to `<REPO>` on both sides, so the bodies
/// compare.
///
/// What a port gets wrong without these: `http-backend` has an argv of exactly one
/// token, so a corpus that runs only `-h` and a bad flag measures nothing at all
/// about it. These pin the environment contract instead — which missing variable
/// produces which 500, that `REQUEST_METHOD` is validated before the path is
/// routed, that a repository without `GIT_HTTP_EXPORT_ALL` or an `export-ok`
/// marker is 404 rather than served, which paths the dumb protocol answers, and
/// that `http.getanyfile=false` turns exactly those into a 403.
///
/// The smart `service=git-upload-pack` advertisement is deliberately absent: its
/// capability line carries `agent=git/<version>`, which is a fact about the build
/// rather than about the protocol.
fn http_backend(out: &mut Vec<Case>) {
    let hb = |env: &[(&str, &str)], out: &mut Vec<Case>| {
        out.push(Case::new("http-backend", &["http-backend"], Shape::Linear).with_env(env));
    };

    // The two fatal-before-anything cases: a path with no method, and a method
    // with no path.
    hb(&[("PATH_TRANSLATED", "{repo}/HEAD")], out);
    hb(&[("REQUEST_METHOD", "GET")], out);
    // A path, but the repository is not exported: 404 with the path in the body.
    hb(&[("REQUEST_METHOD", "GET"), ("PATH_TRANSLATED", "{repo}/HEAD")], out);
    hb(
        &[
            ("REQUEST_METHOD", "GET"),
            ("PATH_TRANSLATED", "{repo}/info/refs"),
            ("QUERY_STRING", "service=git-upload-pack"),
        ],
        out,
    );
    // Method validation happens before the path is routed.
    hb(
        &[
            ("REQUEST_METHOD", "NOTAMETHOD"),
            ("GIT_HTTP_EXPORT_ALL", "1"),
            ("PATH_TRANSLATED", "{repo}/HEAD"),
        ],
        out,
    );
    // Exported: the dumb-protocol reads, which are a file and a generated ref
    // listing — no clock, no agent string and no `Last-Modified` in either.
    hb(
        &[
            ("REQUEST_METHOD", "GET"),
            ("GIT_HTTP_EXPORT_ALL", "1"),
            ("PATH_TRANSLATED", "{repo}/HEAD"),
        ],
        out,
    );
    hb(
        &[
            ("REQUEST_METHOD", "GET"),
            ("GIT_HTTP_EXPORT_ALL", "1"),
            ("PATH_TRANSLATED", "{repo}/info/refs"),
        ],
        out,
    );
    // A service name that is not one, and a path that routes to nothing.
    hb(
        &[
            ("REQUEST_METHOD", "GET"),
            ("GIT_HTTP_EXPORT_ALL", "1"),
            ("PATH_TRANSLATED", "{repo}/info/refs"),
            ("QUERY_STRING", "service=git-bogus"),
        ],
        out,
    );
    hb(
        &[
            ("REQUEST_METHOD", "GET"),
            ("GIT_HTTP_EXPORT_ALL", "1"),
            ("PATH_TRANSLATED", "{repo}/no/such/thing"),
        ],
        out,
    );
    // `http.getanyfile` gates the dumb reads specifically: measured on stock
    // 2.55.0 the same request that served `HEAD` above becomes
    // `403 Unsupported service: getanyfile`.
    out.push(
        Case::new("http-backend", &["http-backend"], Shape::Linear)
            .with_config(&[("http.getanyfile", "false")])
            .with_env(&[
                ("REQUEST_METHOD", "GET"),
                ("GIT_HTTP_EXPORT_ALL", "1"),
                ("PATH_TRANSLATED", "{repo}/HEAD"),
            ]),
    );
}
