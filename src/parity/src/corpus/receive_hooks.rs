//! The **server side of a push**: the hook `receive-pack` runs on the receiving
//! end, the exit-code contract that hook is judged by, and the `receive.*`
//! policy an administrator writes into the repository being pushed *to*.
//!
//! # The finding this module opens with
//!
//! **No fixture ships a `pre-receive`, a `post-receive`, a `post-update` or a
//! `proc-receive` hook.** Read against `fixture.rs`: `Shape::Hooked` installs
//! `pre-commit` and `commit-msg` (fixture.rs:1696, :1703); `Shape::AmHooks`
//! installs `applypatch-msg`, `pre-applypatch`, `post-applypatch`, `pre-commit`
//! and `commit-msg` (fixture.rs:2698-2734); `Shape::HooksFail`'s `FAILING_HOOKS`
//! installs eleven client-side hooks and no receive-side one (fixture.rs:2900).
//! Exactly **one** receive-side hook exists anywhere in the fixture set:
//! `PEER_HOOKS` (fixture.rs:3001), an `update` hook installed into
//! [`Shape::HooksFail`]'s bare peer `.remote.git`, which refuses
//! `refs/heads/veto` by name and exits 0 for everything else.
//!
//! So the sharpest question in this territory — *does a `pre-receive` that exits
//! 1 leave **every** ref unmoved where an `update` that exits 1 leaves the
//! others moved?* — is **half measurable**. The `update` half is measured here,
//! hard, and the `pre-receive` half is not measurable at all. What a fixture
//! would have to ship to close it is spelled out at the bottom of this header.
//!
//! # How this divides territory with the six adjacent modules, all read in full
//!
//! * **`wire_protocol.rs`** owns `receive-pack`'s *advertisement* and the
//!   pkt-line level on [`Shape::BehindRemote`]. Its `receive_pack_reports` group
//!   drives the peer with four **deletion** requests (`RP_DELETE_V2`,
//!   `RP_DELETE_V1`, `RP_DELETE_PLAIN`, `RP_DELETE_NO_CAPS`) and its header
//!   states the constraint that shaped it: *"A deletion is the one push command
//!   that needs no pack, which is what makes a receive-pack request expressible
//!   as a literal at all"*. **That constraint is lifted here.** Every non-delete
//!   request below ends with the 32-byte pack that holds zero objects, and
//!   every object those requests name is one the receiving repository already
//!   has, so a pack with nothing in it is the whole transfer. That
//!   makes ref **creation** and ref **update** expressible as literals for the
//!   first time, and creation is the only command shape the `update` hook can be
//!   made to refuse on this fixture. No case here is a bare deletion of the kind
//!   `wire_protocol.rs` already owns except the two that exist to isolate
//!   `receive.denyDeletes` from `receive.denyDeleteCurrent`, and both are on a
//!   different shape with a different ref.
//! * **`hooks_identity.rs`** owns the client-side hook chain, and owns the peer
//!   `update` hook *as seen through `git push`* (hooks_identity.rs:490-502:
//!   `push --no-verify origin veto:refs/heads/veto main:refs/heads/other` and
//!   its `--atomic` twin). What it measures is the **client's** rendering —
//!   `! [remote rejected] veto -> veto (hook declined)` on stderr. This module
//!   measures the **server's** own bytes for the same refusal: `ng
//!   refs/heads/veto hook declined` as a pkt-line on stdout, the hook's stderr
//!   framed onto the side band or left unframed, and — the part no `push` case
//!   can reach — what happens when the client never asked for a side band at
//!   all. Different binary role, different stream, different observable.
//! * **`branch_remote.rs`** owns `push` and the remote-tracking bookkeeping,
//!   including `--push-option=parity` (branch_remote.rs:688) and
//!   `receive.denyDeleteCurrent` as a `push` outcome (branch_remote.rs:698).
//!   Nothing there drives a server.
//! * **`external_tools.rs`** owns `git hook run` and `core.hooksPath` as an
//!   argument to *that* verb. `core.hooksPath` appears here only as a
//!   `receive-pack` command-line option aimed at the **receiving** repository,
//!   which is a resolution rule no other module exercises — see
//!   [`hooks_path_redirect`].
//! * **`transport_local.rs`** owns the self-referential spelling of the
//!   transport verbs on shapes with no peer.
//! * **`submodule_deep.rs`** owns pushes that cross a submodule boundary.
//!
//! `fetch_clone.rs` was read too, because it owns the `receive.*` keys as
//! *`send-pack` outcomes* at `Repo` scope on [`Shape::BehindRemote`]
//! (fetch_clone.rs:1528-1552: `denyCurrentBranch` default/`ignore`/
//! `updateInstead`, `denyNonFastForwards`). Two things are different here: the
//! observable is the raw `ng` line rather than a client's report, and the enum
//! is covered **whole** — `refuse` and `warn` are set by no case in the corpus,
//! and `refuse` is not a synonym for the default. Stock 2.55.0 prints a
//! multi-line advisory with the default and suppresses it under `refuse`,
//! reaching the identical `ng` line by two different paths. See
//! [`deny_current_branch`].
//!
//! # Why `-c` and not `Repo` scope
//!
//! `receive.*` is read by the repository being pushed *to*, and
//! `ConfigScope::Repo` writes `.git/config` of the **fixture**, not of
//! `.remote.git`. For a case whose argv is `receive-pack ./.remote.git` the
//! server has already chdir'd into the peer before it reads any file, so a
//! `Repo`-scoped key never reaches it. `-c` does: it is on the server's own
//! command line, and command-line scope survives the chdir. Verified by hand:
//! `core.hooksPath`, `receive.denyCurrentBranch`, `receive.denyDeleteCurrent`,
//! `receive.denyDeletes`, `receive.denyNonFastForwards`,
//! `receive.advertisePushOptions`, `receive.procReceiveRefs` and
//! `receive.maxInputSize` each produce, at some value, an answer the same
//! request without them does not — which a key the server never read could not
//! do.
//!
//! # Determinism
//!
//! Every request is a `&'static [u8]` pkt-line literal with hand-computed
//! lengths, generated by a length-computing script and replayed against stock
//! 2.55.0 in a hand-rebuilt copy of [`Shape::HooksFail`] before being
//! transcribed. The three object ids they name are fixed facts of that shape,
//! re-derived by rebuilding it from `fixture.rs`'s recipe under `env::harden`'s
//! pins and cross-checked against the transcript already in `fuzz.rs:4985`
//! (`91ddf90..f32913c  main -> main`):
//!
//! | ref | id |
//! | --- | --- |
//! | fixture `refs/heads/main`, `refs/heads/veto` | `f32913ca7c7f1d81df74db06a218011de558ad3a` |
//! | fixture and peer `refs/heads/hf-side` | `03064f1ad974bed8a3cfaebae21da4ad7fded622` |
//! | peer `refs/heads/main` | `91ddf90aac2004991e45e92ca42581b76b3f2d48` |
//!
//! No clock, no pid, no path and no random byte appears in any answer: the one
//! `receive.*` key that would have put a timestamp on the wire,
//! `receive.certNonceSeed`, is excluded for the reason `wire_protocol.rs` and
//! `fetch_clone.rs` both give.
//!
//! # What is NOT here, established by measurement
//!
//! * **`GIT_PUSH_OPTION_COUNT` / `GIT_PUSH_OPTION_0…` in the hook
//!   environment.** Unobservable. The only receive-side hook in the fixture set
//!   is `PEER_HOOKS`'s `update`, whose body branches on `$1` and prints nothing
//!   about its environment (fixture.rs:3001). A `Case` is one argv against a
//!   pristine copy and cannot write a hook, so there is no way to make a server
//!   report what it exported. The *negotiation* half is measurable and is
//!   covered — see [`push_options`].
//! * **`receive.updateServerInfo`.** It writes `.remote.git/info/refs`, and
//!   `runner::probe_peer` records the peer's refs, objects and pack census but
//!   not its file listing (runner.rs:4721). Both settings therefore produce
//!   byte-identical stdout and byte-identical state, so a case would score
//!   agreement while measuring nothing. Excluded on that basis rather than on
//!   taste; verified by hand that the file appears only under `true`.
//! * **`receive.fsckObjects` and `receive.unpackLimit`.** Both are decisions
//!   *about a pack*, and the only pack expressible as a literal here is the
//!   empty one: zero objects passes any fsck and is below any limit. Verified —
//!   `-c receive.fsckObjects=true` and `-c receive.unpackLimit=1` both produce
//!   byte-identical answers to the unconfigured request. `receive.maxInputSize`
//!   is the one member of that family that *does* decide something on an empty
//!   pack, because it measures the input stream rather than the object count,
//!   and it is covered.
//! * **A hook that exists and is not executable, or is a directory.** Neither
//!   is expressible: the fixture's one receive-side hook is installed 0755 by
//!   `install_hooks` and nothing in a case can change a mode. The adjacent
//!   question that *is* expressible — a hook chain aimed at a path that holds no
//!   hook, or at a regular file, or at the repository root — is
//!   [`hooks_path_redirect`], and `proc-receive` supplies the missing-hook case
//!   directly in [`proc_receive`].
//!
//! # What a fixture would have to ship to make the rest reachable
//!
//! One addition to `PEER_HOOKS` closes the entire gap, because `PEER_HOOKS` is
//! already installed into a bare peer that cases can push to:
//!
//! 1. A **`pre-receive`** that reads its ref list from stdin, writes it into the
//!    peer (`printf '%s\n' "$GIT_PUSH_OPTION_COUNT" > hook-pre-receive.txt` plus
//!    `cat >> hook-pre-receive.txt`), and exits **1** when any line names
//!    `refs/heads/veto-all`. Pushing `veto-all` *and* another ref then makes the
//!    whole-push refusal directly comparable against the per-ref refusal this
//!    module already measures, which is the single strongest thing this
//!    territory has to offer and is currently unmeasurable.
//! 2. A **`post-receive`** and a **`post-update`** that each record their argv
//!    and stdin into a file in the peer, and exit **1**. Git ignores both
//!    failures; an implementation that propagates one turns a successful push
//!    into a failing one, which is the receive-side twin of the `post-commit`
//!    trap `FAILING_HOOKS` already sets.
//! 3. A **`proc-receive`** speaking the version-1 pkt-line handshake, so
//!    `receive.procReceiveRefs=refs/for` reaches a hook that answers instead of
//!    one that is absent.
//!
//! None of these hooks may invoke git, for the reason `Shape::Hooked` gives: each side
//! of a case runs its own binary, and a hook naming one by path would make the
//! other side execute it too. All three can be written with `printf`, `cat` and
//! `test` alone.

use crate::fixture::Shape;
use crate::runner::Case;

/// The peer of [`Shape::HooksFail`], reached by the relative URL the fixture
/// itself records.
const PEER: &str = "./.remote.git";

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    update_hook_exit_code(out);
    hooks_path_redirect(out);
    deny_current_branch(out);
    deletion_policy(out);
    non_fast_forward(out);
    push_options(out);
    proc_receive(out);
    max_input_size(out);
    advertisement_modes(out);
}

// ---------------------------------------------------------------------------
// pkt-line request literals
// ---------------------------------------------------------------------------
//
// Grammar: `<old-id> <new-id> <ref>\0<caps>\n` for the first command, then
// `<old-id> <new-id> <ref>\n` for each further one, then a flush; then the
// push-option section if `push-options` was negotiated; then the pack, unless
// every command is a deletion. Each four-hex length counts its own four bytes.

/// Create `refs/heads/veto` **and** `refs/heads/other`, both at the peer's
/// existing `hf-side` commit, with `report-status-v2` and a side band.
///
/// The central request of this module. The peer's `update` hook refuses the
/// first ref by name and accepts the second, so one request carries both
/// verdicts and the answer separates a per-ref refusal from a whole-push one.
/// Stock 2.55.0: `ng refs/heads/veto hook declined`, `ok refs/heads/other`, and
/// `refs/heads/other` present in the peer afterwards while `refs/heads/veto` is
/// not.
const RP_VETO_AND_OTHER: &[u8] = b"00960000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/veto\x00report-status-v2 side-band-64k agent=git/parity\n00670000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/other\n0000PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// [`RP_VETO_AND_OTHER`] with `atomic` in the capability list.
///
/// The other half of the exit-code contract: the same per-ref refusal, and now
/// `ng refs/heads/other atomic push failure` beside it with **nothing** moved.
/// A port that honours the hook and ignores `atomic` passes the request above
/// and fails this one; a port that treats any refusal as fatal fails the request
/// above and passes this one. Neither can pass both by accident.
const RP_VETO_AND_OTHER_ATOMIC: &[u8] = b"009d0000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/veto\x00report-status-v2 side-band-64k atomic agent=git/parity\n00670000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/other\n0000PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// [`RP_VETO_AND_OTHER`] with **no side band negotiated**.
///
/// Where a hook's stderr goes is a wire fact, and this is the only request that
/// can ask it. With `side-band-64k` the hook's two lines are framed into stdout
/// as band-2 packets; without it they land unframed on `receive-pack`'s own
/// stderr, which is why every case carrying this payload compares stderr.
const RP_VETO_AND_OTHER_NO_BAND: &[u8] = b"00880000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/veto\x00report-status-v2 agent=git/parity\n00670000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/other\n0000PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// Create `refs/heads/veto` **and delete** `refs/heads/hf-side` in one request.
///
/// The same per-ref question across two different *kinds* of command: the hook
/// runs for a deletion too, exits 0 for it, and the deletion has to survive the
/// creation's refusal. Stock leaves the peer with `hf-side` gone and `veto`
/// never created — a state change that the ref probe sees and that a port
/// treating a hook refusal as a transaction abort cannot produce.
const RP_VETO_AND_DELETE: &[u8] = b"00960000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/veto\x00report-status-v2 side-band-64k agent=git/parity\n006903064f1ad974bed8a3cfaebae21da4ad7fded622 0000000000000000000000000000000000000000 refs/heads/hf-side\n0000PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// [`RP_VETO_AND_DELETE`] with `atomic`: the deletion is rolled back too.
const RP_VETO_AND_DELETE_ATOMIC: &[u8] = b"009d0000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/veto\x00report-status-v2 side-band-64k atomic agent=git/parity\n006903064f1ad974bed8a3cfaebae21da4ad7fded622 0000000000000000000000000000000000000000 refs/heads/hf-side\n0000PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// Create `refs/heads/other` alone: the same wire shape with no hook verdict in
/// it, so a difference in any case that carries it is about the configuration
/// the case names and nothing else.
const RP_CREATE_OTHER: &[u8] = b"00970000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/other\x00report-status-v2 side-band-64k agent=git/parity\n0000PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// [`RP_CREATE_OTHER`] with `push-options` claimed and two options sent.
///
/// The section between the command flush and the pack exists only when the
/// **server** advertised `push-options`; a client that claims the capability
/// against a server that did not offer it sends bytes the server reads as the
/// start of the pack. Stock's answer to that is `unpack protocol error (pack
/// signature mismatch detected)` — the negotiation failure made visible.
const RP_PUSH_OPTIONS: &[u8] = b"00a40000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/other\x00report-status-v2 side-band-64k push-options agent=git/parity\n0000000aone=1\n000atwo=2\n0000PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// Move the **peer's** `refs/heads/main` backwards onto `hf-side`.
///
/// `hf-side` is not a descendant of the peer's `main`, so this is a
/// non-fast-forward against a ref the peer's `HEAD` points at — the one request
/// that can ask `receive.denyNonFastForwards` a question on a bare repository.
const RP_REWIND_PEER_MAIN: &[u8] = b"009691ddf90aac2004991e45e92ca42581b76b3f2d48 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/main\x00report-status-v2 side-band-64k agent=git/parity\n0000PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// Move the **fixture's own** `refs/heads/main` onto `hf-side`.
///
/// Aimed at `receive-pack .`, which is the only way to ask
/// `receive.denyCurrentBranch` anything: the key is inert on a bare repository,
/// and [`Shape::HooksFail`]'s worktree is non-bare, has `main` checked out, and
/// is deliberately dirty — which is what makes `updateInstead` answer with a
/// third message rather than a second copy of `refuse`'s.
const RP_UPDATE_CHECKED_OUT: &[u8] = b"0096f32913ca7c7f1d81df74db06a218011de558ad3a 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/heads/main\x00report-status-v2 side-band-64k agent=git/parity\n0000PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

/// Delete the peer's `refs/heads/main`, which is the peer's `HEAD`. All
/// deletions, so no pack.
const RP_DELETE_PEER_MAIN: &[u8] = b"009691ddf90aac2004991e45e92ca42581b76b3f2d48 0000000000000000000000000000000000000000 refs/heads/main\x00report-status-v2 side-band-64k agent=git/parity\n0000";

/// Delete the peer's `refs/heads/hf-side`, which is **not** its `HEAD`.
///
/// The control that separates `receive.denyDeletes` from
/// `receive.denyDeleteCurrent`: only the former refuses this one.
const RP_DELETE_PEER_SIDE: &[u8] = b"009903064f1ad974bed8a3cfaebae21da4ad7fded622 0000000000000000000000000000000000000000 refs/heads/hf-side\x00report-status-v2 side-band-64k agent=git/parity\n0000";

/// Create `refs/for/main/topic`: a ref under the prefix
/// `receive.procReceiveRefs` claims, and outside every other case's namespace.
const RP_CREATE_FOR_REF: &[u8] = b"009a0000000000000000000000000000000000000000 03064f1ad974bed8a3cfaebae21da4ad7fded622 refs/for/main/topic\x00report-status-v2 side-band-64k agent=git/parity\n0000PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

// ---------------------------------------------------------------------------
// constructors
// ---------------------------------------------------------------------------

/// A `receive-pack` case on [`Shape::HooksFail`] with stderr compared.
///
/// Strict throughout, and not as a default preference: half of what a receive
/// hook produces *is* stderr — the hook's own diagnostics, `receive-pack`'s
/// `error: hook declined …`, and the multi-line `denyCurrentBranch`
/// advisory — and which of the two streams each line lands on is the fact under
/// test.
fn rp(args: &[&str], stdin: &'static [u8]) -> Case {
    let mut case = Case::with_stdin("receive-pack", args, Shape::HooksFail, stdin);
    case.compare_stderr = true;
    case
}

/// [`rp`] against the peer, with `-c key=value` in front of the subcommand.
fn rp_peer_cfg(stdin: &'static [u8], key: &str, value: &str) -> Case {
    rp(&["receive-pack", PEER], stdin).with_config(&[(key, value)])
}

// ---------------------------------------------------------------------------
// the exit-code contract
// ---------------------------------------------------------------------------

/// What a non-zero `update` exit does, and what it does **not** do.
///
/// The contract, in git's words: `update` runs once per ref and its refusal
/// binds that ref alone. Every case here is a request carrying **two** commands
/// where the hook refuses exactly one, because a single-ref request cannot tell
/// a per-ref refusal from a whole-push one — a port that ran the hook once for
/// the batch, or that treated any refusal as a transaction abort, would pass
/// every single-ref test in the corpus.
///
/// Verified against stock 2.55.0 in a hand-rebuilt copy of the shape:
///
/// * [`RP_VETO_AND_OTHER`] → `ng refs/heads/veto hook declined`,
///   `ok refs/heads/other`; the peer gains `refs/heads/other` and not
///   `refs/heads/veto`.
/// * [`RP_VETO_AND_OTHER_ATOMIC`] → the same `ng`, then
///   `ng refs/heads/other atomic push failure`; the peer gains nothing.
/// * [`RP_VETO_AND_DELETE`] → `ng refs/heads/veto hook declined`,
///   `ok refs/heads/hf-side`; the peer **loses** `hf-side`.
/// * [`RP_VETO_AND_DELETE_ATOMIC`] → the deletion is rolled back with
///   `ng refs/heads/hf-side atomic push failure`.
///
/// The last pair is the strongest in the module: the surviving half of a
/// partially-refused push is a *state* difference on the receiving end, not a
/// message, so no amount of message-shaped agreement can fake it.
fn update_hook_exit_code(out: &mut Vec<Case>) {
    for payload in [
        RP_VETO_AND_OTHER,
        RP_VETO_AND_OTHER_ATOMIC,
        RP_VETO_AND_DELETE,
        RP_VETO_AND_DELETE_ATOMIC,
    ] {
        out.push(rp(&["receive-pack", PEER], payload));
    }

    // Where the hook's stderr goes. Framed onto the side band in every case
    // above; unframed on `receive-pack`'s own stderr here, because this client
    // never negotiated a band.
    out.push(rp(&["receive-pack", PEER], RP_VETO_AND_OTHER_NO_BAND));

    // `--stateless-rpc`: the same exchange with the advertisement suppressed, so
    // the report is the whole of stdout and a port that emits the ref list
    // anyway diverges on the first byte.
    out.push(rp(&["receive-pack", "--stateless-rpc", PEER], RP_VETO_AND_OTHER));

    // The same request against a repository that has **no** `update` hook, which
    // is the fixture's own. `refs/heads/veto` already exists there at
    // `f32913ca…`, so stock answers `ng refs/heads/veto reference already
    // exists` — a refusal from the ref store rather than from a hook, reaching
    // the same shape of report by a different route. The pair pins that the
    // *name* `veto` is not what is being refused.
    out.push(rp(&["receive-pack", "."], RP_VETO_AND_OTHER));

    // A deletion the hook is happy with, alone: the baseline the two
    // `denyDeletes` cases below are measured against, and the proof that the
    // hook runs for deletions and lets this one through.
    out.push(rp(&["receive-pack", PEER], RP_DELETE_PEER_SIDE));
}

/// `core.hooksPath` aimed at the **receiving** repository.
///
/// A `-c` on a `receive-pack` argv is config for the server process, so this is
/// the one spelling in the corpus that moves a *receive-side* hook chain. The
/// resolution rule it exposes was measured rather than assumed: a relative
/// `core.hooksPath` is resolved against the **receiving repository's own
/// directory**, not against the working directory the case started in. So for
/// `receive-pack ./.remote.git`:
///
/// | value | stock 2.55.0 |
/// | --- | --- |
/// | `hooks` | re-finds `.remote.git/hooks/update`; `veto` still declined |
/// | `no-such-hooks` | no hook found; **both** refs created |
/// | `side-base.txt` | a regular file of the fixture, not of the peer; both created |
/// | `.` | the peer's own root, which holds no `update`; both created |
///
/// The first row is what makes the other three mean something: a port that
/// ignored `core.hooksPath` entirely would pass row one and fail rows two to
/// four, and a port that treated any redirect as "no hooks" would do the
/// reverse.
///
/// Every value is relative and inside the fixture, so nothing here can reach the
/// machine's own filesystem — the same rule `hooks_identity.rs` enforces with a
/// test over its own cases.
fn hooks_path_redirect(out: &mut Vec<Case>) {
    for value in ["hooks", "no-such-hooks", "side-base.txt", "."] {
        out.push(rp_peer_cfg(RP_VETO_AND_OTHER, "core.hooksPath", value));
    }
}

// ---------------------------------------------------------------------------
// receive.* policy
// ---------------------------------------------------------------------------

/// `receive.denyCurrentBranch`, the whole enum, against a non-bare receiver.
///
/// Aimed at `receive-pack .` because the key is inert on a bare repository —
/// verified against stock, `refuse`, `warn` and `updateInstead` each accept
/// [`RP_REWIND_PEER_MAIN`] against the bare peer byte for byte identically to
/// the unconfigured request. [`Shape::HooksFail`]'s own worktree is non-bare, has `main`
/// checked out, and carries an unstaged edit to `side-base.txt` that the fixture
/// makes on purpose, which is what gives `updateInstead` something to refuse
/// over.
///
/// Five distinct answers from stock 2.55.0, all on stdout as side-band frames:
///
/// | value | answer |
/// | --- | --- |
/// | (unset) | a multi-line advisory, then `ng … branch is currently checked out` |
/// | `refuse` | one-line `error: refusing to update checked out branch`, same `ng` |
/// | `warn` | `warning: updating the current branch`, then `ok`; the ref **moves** |
/// | `ignore` | silence, then `ok`; the ref moves |
/// | `updateInstead` | `ng refs/heads/main Working directory has unstaged changes` |
///
/// The default/`refuse` pair is the row nothing else in the corpus separates:
/// two paths to a byte-identical `ng` line that differ only in whether the
/// advisory was printed. `fetch_clone.rs` reaches the default, `ignore` and
/// `updateInstead` through `send-pack`'s client rendering at `Repo` scope; the
/// raw report, and `refuse` and `warn` at all, are new here.
///
/// `warn` and `ignore` move `refs/heads/main` while leaving the index and the
/// worktree where they were, so the state digest afterwards shows a repository
/// whose `HEAD` disagrees with its own tree — a shape no other case in the
/// corpus produces, and the exact hazard the default exists to prevent.
fn deny_current_branch(out: &mut Vec<Case>) {
    out.push(rp(&["receive-pack", "."], RP_UPDATE_CHECKED_OUT));
    for value in ["refuse", "warn", "ignore", "updateInstead"] {
        out.push(
            rp(&["receive-pack", "."], RP_UPDATE_CHECKED_OUT)
                .with_config(&[("receive.denyCurrentBranch", value)]),
        );
    }
}

/// `receive.denyDeletes` and `receive.denyDeleteCurrent`, kept apart by using
/// two refs rather than two keys on one ref.
///
/// The peer's `HEAD` is `refs/heads/main`, so a request deleting it can be
/// refused by either key and a case that changed only the key could not say
/// which one answered. Deleting `refs/heads/hf-side` instead is refused by
/// `denyDeletes` alone, which is what makes the pair a measurement.
///
/// Verified against stock 2.55.0, deleting the peer's `main`:
///
/// | config | answer |
/// | --- | --- |
/// | (unset) | a multi-line advisory + `error: refusing to delete the current branch`, `ng … deletion of the current branch prohibited` |
/// | `denyDeleteCurrent=refuse` | the `error:` line alone, same `ng` |
/// | `denyDeleteCurrent=warn` | `warning: deleting the current branch`, `ok`; the ref goes |
/// | `denyDeleteCurrent=ignore` | two empty side-band frames, `ok`; the ref goes |
/// | `denyDeletes=true` | `error: denying ref deletion for refs/heads/main`, `ng … deletion prohibited` |
///
/// The `ignore` row's two zero-length side-band packets (`00050005`) are a
/// byte-level detail nothing else in the corpus produces and that a port
/// rebuilding the report from a message list rather than relaying frames will
/// not reproduce.
fn deletion_policy(out: &mut Vec<Case>) {
    out.push(rp(&["receive-pack", PEER], RP_DELETE_PEER_MAIN));
    for value in ["refuse", "warn", "ignore"] {
        out.push(rp_peer_cfg(RP_DELETE_PEER_MAIN, "receive.denyDeleteCurrent", value));
    }
    out.push(rp_peer_cfg(RP_DELETE_PEER_MAIN, "receive.denyDeletes", "true"));
    // The same key against a ref that is not `HEAD`: refused by `denyDeletes`
    // and by nothing else, which is what isolates it from the row above.
    out.push(rp_peer_cfg(RP_DELETE_PEER_SIDE, "receive.denyDeletes", "true"));
}

/// `receive.denyNonFastForwards` on the receiving end.
///
/// The peer accepts the rewind by default — the client's own force check is a
/// separate gate and this request bypasses it by being the wire itself — and
/// refuses it with the key set, as `ng refs/heads/main non-fast-forward` beside
/// `error: denying non-fast-forward refs/heads/main (you should pull first)`.
/// The unconfigured case is not a filler: it is the one that shows the peer will
/// take the rewind, which is what makes the refusal attributable to the key.
fn non_fast_forward(out: &mut Vec<Case>) {
    out.push(rp(&["receive-pack", PEER], RP_REWIND_PEER_MAIN));
    out.push(rp_peer_cfg(RP_REWIND_PEER_MAIN, "receive.denyNonFastForwards", "true"));
}

/// Push options: the capability the **server** decides, not the client.
///
/// `receive.advertisePushOptions` does two things at once — it puts
/// `push-options` in the advertisement, and it makes the server read a
/// push-option section between the command flush and the pack. A port that
/// implements one without the other is a wire break in the strict sense, because
/// the two ends then disagree about where the pack starts. Stock 2.55.0:
///
/// | config | answer to [`RP_PUSH_OPTIONS`] |
/// | --- | --- |
/// | (unset) | no `push-options` advertised; the option lines are read as the pack; `unpack protocol error (pack signature mismatch detected)`, `ng refs/heads/other unpacker error`, nothing created |
/// | `true` | `push-options` in the advertisement; `unpack ok`, `ok refs/heads/other`, the ref created |
/// | `false` | byte-identical to unset |
///
/// `wire_protocol.rs` covers `receive.advertisePushOptions` as an
/// *advertisement* fact (its `receive_pack_advertisement` group runs
/// `receive-pack .` with stdin closed) and carries one request that claims
/// `push-options` (`RP_DELETE_PUSH_OPTION`) — but that request is an all-deletes
/// one, where there is no pack for a misread section to collide with. This is
/// the collision.
fn push_options(out: &mut Vec<Case>) {
    out.push(rp(&["receive-pack", PEER], RP_PUSH_OPTIONS));
    for value in ["true", "false"] {
        out.push(rp_peer_cfg(RP_PUSH_OPTIONS, "receive.advertisePushOptions", value));
    }
}

/// `proc-receive`: the fifth hook, reachable here only as a hook that is
/// **absent**.
///
/// `receive.procReceiveRefs=refs/for` diverts every matching command away from
/// the ref store and into a hook that this fixture does not have, so stock
/// answers `error: cannot find hook 'proc-receive'` and
/// `ng refs/for/main/topic fail to run proc-receive hook`, leaving the ref
/// uncreated. Without the key the same request creates `refs/for/main/topic`
/// literally, because nothing about the name is special to `receive-pack`.
///
/// The pair is the missing-hook question this territory can actually ask: a
/// hook the chain is told to run and cannot find, versus the same chain not
/// asked. It is also the only place in this module that puts a ref outside
/// `refs/heads` into a repository, so the ref-name parser is exercised on a
/// three-level name no other case here writes.
fn proc_receive(out: &mut Vec<Case>) {
    out.push(rp_peer_cfg(RP_CREATE_FOR_REF, "receive.procReceiveRefs", "refs/for"));
    out.push(rp(&["receive-pack", PEER], RP_CREATE_FOR_REF));
}

/// `receive.maxInputSize`, the one pack-policy key an empty pack can answer.
///
/// It bounds the *input stream*, not the object count, so a 32-byte pack trips a
/// limit of 1 and passes a limit of 1048576. Stock: `fatal: pack exceeds maximum
/// allowed size`, then `unpack unpack-objects abnormal exit` and
/// `ng refs/heads/other unpacker error`, with the ref uncreated. The generous
/// limit and the unconfigured request are both here, so the refusal is
/// attributable to the value rather than to the key's presence.
///
/// `wire_protocol.rs` sets this key in its *advertisement* group, where no pack
/// is sent and the limit therefore decides nothing.
fn max_input_size(out: &mut Vec<Case>) {
    out.push(rp(&["receive-pack", PEER], RP_CREATE_OTHER));
    for value in ["1", "1048576"] {
        out.push(rp_peer_cfg(RP_CREATE_OTHER, "receive.maxInputSize", value));
    }
}

/// The advertisement-only spellings, which no case in the corpus runs.
///
/// `fetch_clone.rs` covers `upload-pack --advertise-refs` in five forms and
/// `fetch-pack --stateless-rpc --advertise-refs` in one; the receive side has
/// only ever been reached by running the full server with stdin closed, which
/// prints the advertisement and then reports a hangup. These three flags print
/// the advertisement and exit 0 with nothing on stderr, which is a different
/// answer and a different code path.
///
/// `--http-backend-info-refs` is the spelling `git-http-backend` uses for
/// `GET /info/refs?service=git-receive-pack`; on stock 2.55.0 it produces bytes
/// identical to `--advertise-refs` here, and pinning that equality is the point
/// — a port that implemented one and stubbed the other diverges immediately.
///
/// Both repositories are covered: the bare peer, whose advertisement is two
/// refs, and the fixture itself, whose five refs include `refs/remotes/*` and
/// two branches at the same id.
fn advertisement_modes(out: &mut Vec<Case>) {
    for flag in ["--advertise-refs", "--http-backend-info-refs"] {
        out.push(Case::strict("receive-pack", &["receive-pack", flag, PEER], Shape::HooksFail));
        out.push(Case::strict("receive-pack", &["receive-pack", flag, "."], Shape::HooksFail));
    }
    out.push(Case::strict(
        "receive-pack",
        &["receive-pack", "--stateless-rpc", "--advertise-refs", PEER],
        Shape::HooksFail,
    ));
    // The one `receive.*` key that changes these bytes, on the spelling that
    // prints nothing else: the capability list gains `push-options` between the
    // NUL and the newline of the first ref line.
    out.push(
        Case::strict("receive-pack", &["receive-pack", "--advertise-refs", PEER], Shape::HooksFail)
            .with_config(&[("receive.advertisePushOptions", "true")]),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every request that is not all-deletions ends with the empty pack, and
    /// every request that *is* all-deletions does not.
    ///
    /// The distinction is not cosmetic: a non-delete command with no pack behind
    /// it makes `receive-pack` fail with `unpack eof before pack header was
    /// fully read` on both sides, which scores as agreement while measuring
    /// nothing at all — the exact trap this module was written to walk around.
    /// A payload edited later that lost its trailer would fall straight back
    /// into it, silently, so the shape is asserted here rather than trusted.
    #[test]
    fn non_delete_requests_carry_the_empty_pack() {
        const PACK: &[u8] = b"PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";
        assert_eq!(PACK.len(), 32);
        for payload in [
            RP_VETO_AND_OTHER,
            RP_VETO_AND_OTHER_ATOMIC,
            RP_VETO_AND_OTHER_NO_BAND,
            RP_VETO_AND_DELETE,
            RP_VETO_AND_DELETE_ATOMIC,
            RP_CREATE_OTHER,
            RP_PUSH_OPTIONS,
            RP_REWIND_PEER_MAIN,
            RP_UPDATE_CHECKED_OUT,
            RP_CREATE_FOR_REF,
        ] {
            assert!(payload.ends_with(PACK), "request lost its empty pack: {payload:?}");
        }
        for payload in [RP_DELETE_PEER_MAIN, RP_DELETE_PEER_SIDE] {
            assert!(!payload.ends_with(PACK), "all-deletes request grew a pack: {payload:?}");
            assert!(payload.ends_with(b"0000"), "request does not end in a flush");
        }
    }

    /// Every pkt-line length prefix counts its own four bytes and the packet
    /// lands exactly on the start of the next one.
    ///
    /// A length that is one byte wrong does not fail loudly — the server reads a
    /// truncated command and answers a question nobody asked, identically on
    /// both sides — so the whole module would pass while measuring a typo. The
    /// walk stops at the pack, which is not pkt-line framed.
    #[test]
    fn pkt_line_lengths_are_self_consistent() {
        for payload in [
            RP_VETO_AND_OTHER,
            RP_VETO_AND_OTHER_ATOMIC,
            RP_VETO_AND_OTHER_NO_BAND,
            RP_VETO_AND_DELETE,
            RP_VETO_AND_DELETE_ATOMIC,
            RP_CREATE_OTHER,
            RP_PUSH_OPTIONS,
            RP_REWIND_PEER_MAIN,
            RP_UPDATE_CHECKED_OUT,
            RP_CREATE_FOR_REF,
            RP_DELETE_PEER_MAIN,
            RP_DELETE_PEER_SIDE,
        ] {
            let mut at = 0usize;
            let mut packets = 0usize;
            while at + 4 <= payload.len() {
                if payload[at..].starts_with(b"PACK") {
                    break;
                }
                let head = std::str::from_utf8(&payload[at..at + 4]).expect("length is not ASCII");
                let len = usize::from_str_radix(head, 16).expect("length is not hex");
                if len == 0 {
                    at += 4;
                    packets += 1;
                    continue;
                }
                assert!(len >= 4, "packet length {len} is below its own header");
                assert!(at + len <= payload.len(), "packet at {at} runs off the end");
                at += len;
                packets += 1;
            }
            assert!(packets >= 2, "request has no packets: {payload:?}");
        }
    }

    /// No case leaves the fixture: `core.hooksPath` is the one value here that
    /// names a path, and an absolute one would point the receiving repository's
    /// hook chain at the machine running the harness.
    #[test]
    fn hooks_path_values_stay_inside_the_fixture() {
        let mut all = Vec::new();
        cases(&mut all);
        let mut seen = 0;
        for case in &all {
            for entry in &case.config {
                if entry.key.as_deref() != Some("core.hooksPath") {
                    continue;
                }
                let path = entry.value.clone();
                seen += 1;
                assert!(!path.starts_with('/'), "{} escapes the fixture: {path}", case.id());
                assert!(!path.starts_with('~'), "{} names a home: {path}", case.id());
            }
        }
        assert!(seen >= 4, "the hooksPath dimension lost its cases: {seen}");
    }

    /// Both halves of the exit-code contract are present: for every request
    /// carrying the peer's veto there is an `atomic` twin, and vice versa.
    ///
    /// The pairing is what the measurement rests on — a single-ref request, or a
    /// multi-ref one without its atomic counterpart, cannot separate a per-ref
    /// refusal from a whole-push one. Deleting either member of a pair would
    /// leave a module that still runs and no longer asks the question.
    #[test]
    fn every_veto_request_has_an_atomic_twin() {
        for (plain, atomic) in [
            (RP_VETO_AND_OTHER, RP_VETO_AND_OTHER_ATOMIC),
            (RP_VETO_AND_DELETE, RP_VETO_AND_DELETE_ATOMIC),
        ] {
            assert!(!contains(plain, b" atomic "), "the plain request negotiated atomic");
            assert!(contains(atomic, b" atomic "), "the atomic twin did not negotiate it");
            assert!(contains(plain, b"refs/heads/veto"));
            assert!(contains(atomic, b"refs/heads/veto"));
        }
        let mut all = Vec::new();
        cases(&mut all);
        let with = |needle: &'static [u8]| {
            all.iter().filter(|c| c.stdin == Some(needle)).count()
        };
        assert!(with(RP_VETO_AND_OTHER) >= 6, "the veto request lost its cases");
        assert_eq!(with(RP_VETO_AND_OTHER_ATOMIC), 1);
        assert_eq!(with(RP_VETO_AND_DELETE), 1);
        assert_eq!(with(RP_VETO_AND_DELETE_ATOMIC), 1);
    }

    /// Every case in this module compares stderr, and every one of them is a
    /// `receive-pack` on [`Shape::HooksFail`].
    ///
    /// Both are load-bearing. A receive hook's diagnostics are stderr on the
    /// server and side-band frames on stdout depending on what the client
    /// negotiated, so a case that stopped comparing stderr would stop measuring
    /// the half of the contract that is about *routing*. And `HooksFail` is the
    /// only shape carrying a receive-side hook at all — a case that drifted onto
    /// another shape would be measuring the same server with the hook chain
    /// switched off and no longer say so.
    #[test]
    fn every_case_is_a_strict_receive_pack_on_hooks_fail() {
        let mut all = Vec::new();
        cases(&mut all);
        assert!(all.len() >= 30, "the module lost cases: {}", all.len());
        for case in &all {
            assert_eq!(case.cmd, "receive-pack", "{}", case.id());
            assert_eq!(case.shape, Shape::HooksFail, "{}", case.id());
            assert!(case.compare_stderr, "{} stopped comparing stderr", case.id());
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
