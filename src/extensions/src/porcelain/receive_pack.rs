//! `git receive-pack` — the server side of a push.
//!
//! `receive-pack` is a protocol server. It writes a ref advertisement, then reads
//! commands, a packfile and (optionally) a push certificate off stdin, ingests the
//! pack, runs hooks and updates refs. The advertisement (below) and the receive path
//! (`receive`: command list → pack ingest via `gix_pack::Bundle` → hooks → ref
//! compare-and-swap → `report-status`) are both implemented — enough for a local
//! (`file://`) or ssh push served by this binary, and for the split
//! advertise-then-`--stateless-rpc` session an HTTP backend drives. The advertisement is byte-verified
//! against git 2.55.0:
//!
//!   * **The ref advertisement** — `<oid> SP <ref>` pkt-lines in refname order,
//!     with the capability list appended to the first line after a NUL, the
//!     `0000000…0 capabilities^{}` line for a repository with no refs, the
//!     `shallow <oid>` lines a shallow repository adds, and the closing flush.
//!     Symbolic refs are resolved but tags are *not* peeled — `receive-pack`
//!     advertises no `^{}` rows.
//!   * **The capability list**, in git's emission order, honouring
//!     `receive.advertiseAtomic`, `repack.useDeltaBaseOffset`,
//!     `receive.certNonceSeed` (which adds `push-cert=<nonce>`) and
//!     `receive.advertisePushOptions`, plus `object-format=<algo>` from the
//!     repository's hash and `agent=` from `GIT_USER_AGENT` (see [`super::version::user_agent_sanitized`]).
//!   * **Hidden refs** — `transfer.hideRefs` and `receive.hideRefs` are applied
//!     to the advertisement through `ref_is_hidden()` (last pattern wins, `!`
//!     un-hides), and a push to a hidden ref is rejected with
//!     `deny updating a hidden ref` (`deny deleting a hidden ref` for a delete).
//!   * **`--http-backend-info-refs` / `--advertise-refs`** — advertise and exit 0.
//!   * **`--stateless-rpc`** — serve the request without re-advertising, the
//!     half of an HTTP push that follows a separate `--advertise-refs` process.
//!     It is also the only session shape in which a certificate can echo a nonce
//!     this process did not mint, which is what `receive.certNonceSlop` grades.
//!   * **Argument handling**: `-h` prints the 68-byte usage block on *stdout*
//!     and exits 0; `--help-all` prints the 262-byte block that lists all
//!     five hidden entries, also on stdout at exit 0; an unknown option prints
//!     ``error: unknown option `x'`` (or ``unknown switch `c'``) followed by
//!     the 68-byte block on stderr, 129; `--quiet=<v>` prints ``error: option
//!     `quiet' takes no value`` alone, 129; no directory / more than one
//!     directory print `fatal: …` followed by the 158-byte block that lists
//!     only the hidden `--advertise-refs`, 129. All three blocks differ.
//!   * **`<git-dir>` resolution** without upward discovery — `<dir>` or
//!     `<dir>/.git`, the two forms git's `enter_repo()` resolves in practice;
//!     anything else is `fatal: '<dir>' does not appear to be a git
//!     repository`, exit 128. (`enter_repo()` also probes the `<dir>.git` and
//!     `<dir>/.git/.git` spellings; `gix::open` does not, so a bare repository
//!     reachable only as `<dir>.git` is reported as not a repository here.)
//!   * **The two stdin outcomes that need no pack**: an immediate flush packet
//!     ends the session with exit 0 and no further output; end-of-input before
//!     a complete pkt-line header is
//!     `fatal: the remote end hung up unexpectedly`, exit 128; a header that is
//!     not four hex digits is
//!     `fatal: protocol error: bad line length character: <4 bytes>`, exit 128.
//!
//! ### The receive path
//!
//! `receive()` reads the command list, ingests the pack and applies the
//! updates, honouring the configuration `receive_pack_config()` collects (see
//! [`Config`]):
//!
//!   * **`receive.unpackLimit` / `transfer.unpackLimit`** decide which child
//!     git would have run, and this port does what that child does: below the
//!     limit the pack is exploded into loose objects and removed
//!     (`unpack-objects`), at or above it the pack and its index are kept
//!     (`index-pack`). The `unpack <status>` line names that child on failure.
//!   * **`receive.maxInputSize`** aborts the ingest as soon as the pack stream
//!     passes the limit, with `fatal: pack exceeds maximum allowed size`.
//!   * **`receive.fsckObjects` / `transfer.fsckObjects`** run the object-content
//!     message layer ([`super::fsck::check_object`]) over every received object
//!     at `receive.fsck.<msg-id>` severities, with `receive.fsck.skipList`
//!     exemptions, followed by `fsck_finish()`'s lint of every `.gitmodules` and
//!     `.gitattributes` blob the received trees named ([`super::fsck::check_blob`]).
//!     The transfer check is always `--strict`, so a defaulted warning is an
//!     error here. An error fails the whole push with
//!     `fatal: fsck error in packed object` from the per-object pass, or
//!     `fatal: fsck error in pack objects` from the `fsck_finish()` sweep.
//!     `receive.fsck.<msg-id>` is read for *every* id in
//!     [`super::fsck::MSGS`] — git's config callback validates whatever follows
//!     `receive.fsck.` without asking whether the check can fire, so a value it
//!     rejects is `fatal: Unknown fsck message type: '<v>'` before the
//!     advertisement, and an id it does not know is only
//!     `warning: skipping unknown msg id '<id>'`. The family never falls back on
//!     `fsck.<msg-id>`.
//!   * **`core.bigFileThreshold`** decides which blobs `index-pack` would have
//!     streamed rather than held, which is the only way `gitmodulesLarge` is
//!     ever reported — and therefore only on the `index-pack` branch, since
//!     `unpack-objects` has no per-object blob check at all.
//!   * **`receive.denyCurrentBranch`, `receive.denyDeleteCurrent`,
//!     `receive.denyDeletes`, `receive.denyNonFastForwards`** are checked per
//!     command in `update()`'s order, each producing git's own band-2 message
//!     and `ng` reason, including the two advice blocks the unconfigured
//!     defaults print. `denyCurrentBranch=updateInstead` runs `update_worktree()`
//!     after the last check: the `push-to-checkout` hook when there is one,
//!     otherwise `push_to_deploy()`'s four children, whose refusals
//!     (`Working directory has unstaged changes` and the rest) become the `ng`
//!     reason.
//!   * **The connectivity check** (`check_connected()`, unless
//!     `--skip-connectivity-check`): a `rev-list --objects --stdin --not
//!     --exclude-hidden=receive --all --quiet --alternate-refs` child is fed every
//!     pushed tip, inside the quarantine, its stderr on band 2; when it fails, each
//!     tip is asked again on its own and the ones that fail are refused with
//!     `missing necessary objects`.
//!   * **Aliased updates** (`check_aliased_updates()`): a command naming a symref
//!     to a ref another command of the push also writes is skipped when the two
//!     agree, and both are refused with `inconsistent aliased update` when they do
//!     not; a symref that does not resolve is `broken symref`. A lone update
//!     through a symref writes the ref it points at, and a delete through one
//!     deletes that ref and logs on the symref and on `HEAD`
//!     (`split_symref_update()`, refs/files-backend.c:2555-2607).
//!   * **Shallow pushes** (`prepare_shallow_info()`, `update_shallow_info()`,
//!     shallow.c:516-892): the `shallow <oid>` lines a shallow client sends are
//!     split into the commits this repository has and lacks, each pushed tip is
//!     painted down to them, and a ref that needs one is refused with `shallow
//!     update not allowed`. Under `receive.shallowUpdate` the hooks and children
//!     are handed `GIT_SHALLOW_FILE` (a `shallow_XXXXXX` tempfile) and
//!     `update_shallow_ref()` adds the commits to `.git/shallow` before the ref
//!     moves, with the quirks of git 2.56's code: its connectivity check reads
//!     the *next* command's tip and cuts the command list after this one, and a
//!     push of deletes alone from a shallow client dies of SIGSEGV.
//!   * **`receive.updateServerInfo`** refreshes `info/refs` and
//!     `objects/info/packs` after the refs move.
//!   * **`receive.autogc`** hands the repository to
//!     [`super::maintenance::run_auto_maintenance`] once the report is out —
//!     git 2.55's `prepare_auto_maintenance()`, which is a
//!     `maintenance run --auto` child gated on `maintenance.auto` / `gc.auto`,
//!     not the `gc --auto` of older versions.
//!   * **`side-band-64k`**: when the client advertises it, the report-status
//!     stream is multiplexed on band 1 and every diagnostic — including every
//!     byte a hook wrote to stdout or stderr — on band 2; otherwise the report
//!     is written as plain pkt-lines and diagnostics go to stderr. The flush
//!     that ends the multiplexed stream is the last write of the session, after
//!     `post-receive` and `post-update` have had their say.
//!   * **`receive.keepAlive`**: while a hook is running and saying nothing, an
//!     empty band-1 packet (`0005\x01`) goes out every N seconds so the client's
//!     read does not time out. `0` or below disables it.
//!
//! ### Push certificates
//!
//! With `receive.certNonceSeed` set, the advertisement carries
//! `push-cert=<nonce>`, where the nonce is `<stamp>-<hmac>` and the HMAC is
//! keyed by `"<git-dir-as-spelled>:<stamp>"` over the seed
//! ([`prepare_push_cert_nonce`]). A client that takes the offer sends its
//! commands *inside* a signed certificate instead of on the wire; the port reads
//! the `push-cert` … `push-cert-end` block, takes the command list from between
//! the certificate's blank line and its signature, stores the certificate as a
//! blob, verifies the signature through [`crate::gitsig`] and grades the echoed
//! nonce ([`check_nonce`]). All of it reaches the hooks as `GIT_PUSH_CERT`,
//! `GIT_PUSH_CERT_SIGNER`, `GIT_PUSH_CERT_KEY`, `GIT_PUSH_CERT_STATUS`,
//! `GIT_PUSH_CERT_NONCE`, `GIT_PUSH_CERT_NONCE_STATUS` and (for a `SLOP` nonce)
//! `GIT_PUSH_CERT_NONCE_SLOP`.
//!
//! ### Hooks
//!
//! `pre-receive`, `update`, `post-receive`, `post-update` and `proc-receive` all
//! run, in git's order and with git's arguments, stdin payloads and environment.
//! A command whose ref name matches `receive.procReceiveRefs` is handed to the
//! `proc-receive` hook over its pkt-line protocol (version negotiation, the
//! command list, the push options, then `ok`/`ng`/`option` replies) and never
//! reaches the ref store; its `option refname` / `old-oid` / `new-oid` /
//! `forced-update` answers become the `report-status-v2` reply and the refs
//! `post-receive` is told about.
//!
//! ### The quarantine
//!
//! The pack is ingested into `objects/tmp_objdir-incoming-XXXXXX`
//! ([`crate::tmp_objdir::Quarantine`]), whose `GIT_QUARANTINE_PATH`,
//! `GIT_OBJECT_DIRECTORY` and `GIT_ALTERNATE_OBJECT_DIRECTORIES` only `pre-receive`
//! is handed. It is migrated into the object store once a ref is about to move,
//! before `update`, `proc-receive` and the ref updates, and the kept pack's
//! `.keep` is deleted after the commands ran; a push that is declined, refused in
//! full or rejected by `--strict` leaves nothing behind.
//!
//! ### Ref updates
//!
//! Outside `--atomic` each phase (deletions, then the rest) is one ref
//! transaction that may fail partially, `REF_TRANSACTION_ALLOW_FAILURE`: a
//! refused update is keyed by the name of the ref that failed, which for an
//! update through a symref is the ref it points at, so the command naming the
//! symref is still reported `ok` and the symref and `HEAD` still get their
//! log-only entries (builtin/receive-pack.c:1868-1966).
//!
//! Object alternates (git appends one `<oid> .have` line per alternate ref) bail
//! rather than producing a short advertisement.
//!
//! `GIT_NAMESPACE` **is** honoured: receive-pack is one of the three programs git
//! namespaces. The advertisement reports `strip_namespace(ref->name)` and hides
//! refs outside the namespace, and `update()`'s
//! `namespaced_name = get_git_namespace() + name` means a pushed `refs/heads/x`
//! is written to `refs/namespaces/<ns>/refs/heads/x`. See [`crate::namespace`]
//! for why almost every *other* command must ignore the variable.
//!
//! `-q`/`--quiet` is accepted and parsed: it only suppresses progress reporting,
//! which this port does not emit, so it has no observable effect.

use anyhow::{anyhow, bail, Result};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};
use std::collections::HashSet;
use std::io::{BufRead, Read, Write};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use super::fsck::{
    check_blob, check_object, Finding, MsgConfig, MsgSource, Severity, GITATTRIBUTES_BLOB,
    GITATTRIBUTES_MISSING, GITMODULES_BLOB, GITMODULES_MISSING,
};

/// The flags this port implements, quoted in every rejection message.
const PORTED: &str = "ported: -q/--quiet, --http-backend-info-refs/--advertise-refs";

/// git's `receive_pack_usage` as `parse_options` renders it for `-h` and for
/// option errors: hidden options omitted (68 bytes, git 2.55.0).
const SHORT_USAGE: &str = "\
usage: git receive-pack <git-dir>

    -q, --[no-]quiet      quiet

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`SHORT_USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]skip-connectivity-check`, `--[no-]stateless-rpc`,
/// `--[no-]http-backend-info-refs`, `--[no-]advertise-refs`,
/// `--[no-]reject-thin-pack-for-testing`.
/// Captured byte-for-byte from stock git 2.55.0's `git receive-pack --help-all`.
const HELP_ALL_USAGE: &str = r#"usage: git receive-pack <git-dir>

    -q, --[no-]quiet      quiet
    --[no-]skip-connectivity-check
    --[no-]stateless-rpc
    --[no-]http-backend-info-refs
    --[no-]advertise-refs alias of --http-backend-info-refs
    --[no-]reject-thin-pack-for-testing

"#;

/// The same block as `usage_msg_opt` renders it for the two argument-count
/// errors, which also lists the hidden `--advertise-refs` (158 bytes).
const FULL_USAGE: &str = "\
usage: git receive-pack <git-dir>

    -q, --[no-]quiet      quiet
    --[no-]advertise-refs ...
                          alias of --http-backend-info-refs

";

/// Parsed command line for a single `receive-pack` invocation.
struct Opts {
    /// `-q`/`--quiet`: suppresses receive-path reporting only.
    quiet: bool,
    /// `--http-backend-info-refs`/`--advertise-refs`: advertise, then exit 0.
    advertise_only: bool,
    /// `--stateless-rpc`: this process serves one HTTP request, so it writes no
    /// advertisement of its own (a separate `--advertise-refs` run did that) and
    /// a push certificate may echo a nonce some *other* process minted — which
    /// is the only situation `receive.certNonceSlop` grades.
    stateless_rpc: bool,
    /// `--skip-connectivity-check` (`OPT_HIDDEN_BOOL`, builtin/receive-pack.c:2506):
    /// `execute_commands()` runs no `check_connected()` pass over the pushed tips.
    skip_connectivity_check: bool,
    /// The single `<git-dir>` operand, exactly as spelled on the command line.
    dir: String,
}

/// `git receive-pack <git-dir>` — advertise refs, then read a push off stdin.
///
/// The advertisement is written verbatim, then [`receive`] ingests the push;
/// see the module docs for what the receive half does and does not model.
pub fn receive_pack(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand at index 0.
    let args = match args.first() {
        Some(a) if a == "receive-pack" => &args[1..],
        _ => args,
    };

    let opts = match parse(args)? {
        Parsed::Opts(opts) => opts,
        Parsed::Exit(code) => return Ok(code),
    };

    // `enter_repo()` reads the candidate its suffix scan settles on with the
    // dying spelling of `read_gitfile()` (path.c:811), so an operand naming a
    // plain file that is not a gitfile ends at `invalid gitfile format:` rather
    // than at the "does not appear to be a git repository" line below.
    {
        let base = std::path::Path::new(&opts.dir).as_os_str().to_owned();
        let candidates: Vec<std::path::PathBuf> = ["/.git", "", ".git/.git", ".git"]
            .iter()
            .map(|suffix| {
                let mut p = base.clone();
                p.push(suffix);
                std::path::PathBuf::from(p)
            })
            .collect();
        if let Some(code) = crate::setup::enter_repo_gitfile_gate(&candidates) {
            return Ok(code);
        }
    }

    let Some(mut repo) = open_repo(&opts.dir) else {
        eprintln!(
            "fatal: '{}' does not appear to be a git repository",
            opts.dir
        );
        return Ok(ExitCode::from(128));
    };

    // receive-pack is the second of the three programs git namespaces (see
    // [`crate::namespace`]). One `set_namespace()` covers both halves, because
    // git namespaces reads *and* writes here:
    //
    //   * the advertisement reports `strip_namespace(ref->name)`, and refs
    //     outside the namespace are not advertised at all;
    //   * `update()` builds `namespaced_name = get_git_namespace() + name` and
    //     hands *that* to the ref transaction, so a push of `refs/heads/x` lands
    //     at `refs/namespaces/<ns>/refs/heads/x`.
    //
    // `gix-ref` carries the store's namespace into transactions
    // (`store/file/transaction/prepare.rs:541`), so the write half follows from
    // the read half rather than needing its own prefixing pass.
    //
    // Applied out here rather than inside `open_repo()` so that a malformed
    // namespace surfaces as git's `bad git namespace path "<raw>"` — folding it
    // into the `Option` that function returns would report it as the unrelated
    // "does not appear to be a git repository".
    crate::namespace::apply(&mut repo)?;

    reject_unportable_advertisement(&repo)?;

    // `receive_pack_config()` runs before the advertisement, so a bad
    // `receive.fsck.<msg-id>` value kills the session before a byte is written.
    // `<git-dir>` is passed as spelled: it is `service_dir`, one half of the
    // push-certificate nonce's HMAC key.
    // The callback's refusals — a bad boolean, number or valueless string among
    // its keys, or anything `git_default_config()` refuses — come first, in
    // configuration order.
    if let Err(rejection) = crate::cmd_config::validate_receive_pack(&repo) {
        let fatal = rejection.into_fatal();
        if !fatal.is_empty() {
            eprintln!("fatal: {fatal}");
        }
        return Ok(ExitCode::from(128));
    }
    let config = match Config::read(&repo, &opts.dir) {
        Ok(config) => config,
        // `is_valid_msg_type()` reaches `parse_msg_type()`'s `die()`
        // (`fsck.c:127`), which prefixes `fatal: ` like every other one.
        Err(fatal) => {
            eprintln!("fatal: {fatal}");
            return Ok(ExitCode::from(128));
        }
    };

    // `cmd_receive_pack()`'s version switch (builtin/receive-pack.c:2662-2682).
    // v2 has no push support, so a client asking for it is served v0; v1 is v0
    // with a `version 1` pkt-line in front, written on exactly the paths that go
    // on to write an advertisement.
    if super::upload_pack::protocol_version_from_env() == 1
        && (opts.advertise_only || !opts.stateless_rpc)
    {
        use std::io::Write;
        let mut line = Vec::new();
        pkt_line(&mut line, b"version 1\n");
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&line)?;
        stdout.flush()?;
    }

    // `if (advertise_refs || !stateless_rpc) write_head_info();` — under
    // `--stateless-rpc` the advertisement was served by an earlier process.
    if opts.advertise_only || !opts.stateless_rpc {
        let adv = advertisement(&repo, &config)?;
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&adv)?;
        stdout.flush()?;
    }

    if opts.advertise_only {
        return Ok(ExitCode::SUCCESS);
    }
    receive(&mut repo, &config, &opts)
}

/// Either a fully parsed command line, or a terminal exit code for the
/// help/usage-error paths, which produce all of their own output.
enum Parsed {
    Opts(Opts),
    Exit(ExitCode),
}

/// git's `parse_options` pass over the `receive-pack` option table, followed by
/// its two argument-count checks.
fn parse(args: &[String]) -> Result<Parsed> {
    let mut quiet = false;
    let mut advertise_only = false;
    let mut stateless_rpc = false;
    let mut skip_connectivity_check = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    for a in args {
        let a = a.as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            positionals.push(a);
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): an exact match tested after the `--` break
        // above and before any table lookup, so it neither abbreviates nor takes
        // an `=<value>`. `USAGE_FULL` here is neither of the other two blocks:
        // it lists all five hidden entries, where [`FULL_USAGE`]'s
        // `usage_msg_opt()` rendering lists only `--advertise-refs`.
        if a == "--help-all" {
            return Ok(Parsed::Exit(super::show_usage(HELP_ALL_USAGE)));
        }

        if let Some(long) = a.strip_prefix("--") {
            // `--<name>=<value>` on a boolean is rejected before anything else.
            let (name, value) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let (name, on) = match name.strip_prefix("no-") {
                Some(rest) => (rest, false),
                None => (name, true),
            };
            let known = matches!(
                name,
                "quiet"
                    | "http-backend-info-refs"
                    | "advertise-refs"
                    | "stateless-rpc"
                    | "skip-connectivity-check"
            );
            if known && value.is_some() {
                eprintln!("error: option `{name}' takes no value");
                return Ok(Parsed::Exit(ExitCode::from(129)));
            }
            match name {
                "quiet" => quiet = on,
                "http-backend-info-refs" | "advertise-refs" => advertise_only = on,
                "stateless-rpc" => stateless_rpc = on,
                "skip-connectivity-check" => skip_connectivity_check = on,
                // Real but unported git options; the receive path they belong
                // to is not implemented, so accepting them would mislead.
                "reject-thin-pack-for-testing" | "signed-push" => {
                    let flag = format!("--{name}");
                    bail!("unsupported flag {flag:?} ({PORTED})")
                }
                _ => {
                    eprint!("error: unknown option `{long}'\n{SHORT_USAGE}");
                    return Ok(Parsed::Exit(ExitCode::from(129)));
                }
            }
            continue;
        }

        // Clumped short flags, e.g. `-qq`. `-h` is handled by parse_options
        // before every other check and writes to stdout.
        for c in a[1..].chars() {
            match c {
                'q' => quiet = true,
                'h' => {
                    return Ok(Parsed::Exit(super::show_usage(SHORT_USAGE)));
                }
                _ => {
                    eprint!("error: unknown switch `{c}'\n{SHORT_USAGE}");
                    return Ok(Parsed::Exit(ExitCode::from(129)));
                }
            }
        }
    }

    // git checks "too many" before "you must specify".
    if positionals.len() > 1 {
        eprint!("fatal: too many arguments\n\n{FULL_USAGE}");
        return Ok(Parsed::Exit(ExitCode::from(129)));
    }
    let Some(dir) = positionals.first() else {
        eprint!("fatal: you must specify a directory\n\n{FULL_USAGE}");
        return Ok(Parsed::Exit(ExitCode::from(129)));
    };

    Ok(Parsed::Opts(Opts {
        quiet,
        advertise_only,
        stateless_rpc,
        skip_connectivity_check,
        dir: (*dir).to_string(),
    }))
}

/// git's `enter_repo()` reduced to what `receive-pack` relies on: the operand
/// names the repository directly, either as the git directory or as the work
/// tree holding it. There is deliberately no upward search — `git receive-pack
/// <repo>/<subdir>` fails even inside a repository.
fn open_repo(dir: &str) -> Option<gix::Repository> {
    // `gix::open` already expands `<path>` to `<path>/.git` for a work tree.
    let repo = gix::open(std::path::Path::new(dir)).ok()?;

    // `enter_repo()` *chdirs into* the repository before any configuration is
    // read, and receive-pack never leaves. That is load-bearing beyond
    // tidiness: the signature backends read `gpg.ssh.allowedSignersFile` and
    // friends from the repository they find at the current directory, so
    // without the move a signed push would be graded against the *pusher's*
    // configuration instead of the receiving repository's.
    let home = std::fs::canonicalize(repo.workdir().unwrap_or_else(|| repo.git_dir())).ok()?;
    std::env::set_current_dir(&home).ok()?;
    crate::setup::note_enter_repo();
    gix::open(&home).ok()
}

/// Bail on repository state that changes the advertisement in a way this port
/// does not reproduce, rather than emitting a silently wrong ref list.
fn reject_unportable_advertisement(repo: &gix::Repository) -> Result<()> {
    let alternates = repo.common_dir().join("objects").join("info").join("alternates");
    if alternates.is_file() || std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES").is_some() {
        bail!("object alternates are not supported (git advertises one '<oid> .have' line per alternate ref)");
    }
    Ok(())
}

/// Build the complete advertisement, byte-for-byte as git's `write_head_info`
/// emits it: every ref under `refs/` in name order (capabilities appended to
/// the first line), the synthetic `capabilities^{}` line when there were none,
/// the `shallow <oid>` lines, then a flush packet.
fn advertisement(repo: &gix::Repository, config: &Config) -> Result<Vec<u8>> {
    let caps = capabilities(repo, config);
    let mut out = Vec::new();
    let mut sent_capabilities = false;

    // `write_head_info()` (receive-pack.c:343-361) deliberately walks the *whole*
    // ref store rather than the namespaced view: "We need access to the reference
    // names both with and without their namespace and thus cannot use
    // `refs_for_each_namespaced_ref()`." A ref outside the namespace is still
    // advertised, as a `.have` line deduplicated by object id, so the client can
    // use it to shrink the pack it sends but otherwise ignores it
    // (`show_ref_cb()`, receive-pack.c:308-329).
    let prefix = repo.namespace().map(|ns| ns.as_bstr().to_string());
    let mut store = repo.clone();
    store.clear_namespace();
    let mut seen: std::collections::HashSet<gix::ObjectId> = std::collections::HashSet::new();

    for reference in store.references()?.all()? {
        // Broken refs are skipped, as git's ref iteration does.
        let Ok(mut reference) = reference else {
            continue;
        };
        let full = reference.name().as_bstr().to_string();
        // `strip_namespace()` answers NULL for a name that is not under the
        // current namespace; without a namespace the two names are equal.
        let stripped = match &prefix {
            Some(p) => full.strip_prefix(p.as_str()).map(str::to_owned),
            None => Some(full.clone()),
        };
        // `show_ref()` runs every candidate through `ref_is_hidden()` first.
        if config.ref_is_hidden_qualified(stripped.as_deref(), &full) {
            continue;
        }
        // Symbolic refs resolve to their object; tags are not peeled here.
        let Ok(id) = reference.follow_to_object() else {
            continue;
        };
        let id = id.detach();
        let name = match stripped {
            Some(name) => {
                seen.insert(id);
                name
            }
            // `if (oidset_insert(seen, ref->oid)) return 0;` — one `.have` per
            // object, no matter how many refs outside the namespace carry it.
            None if seen.insert(id) => ".have".to_string(),
            None => continue,
        };
        let line = if sent_capabilities {
            format!("{} {name}\n", id.to_hex())
        } else {
            sent_capabilities = true;
            format!("{} {name}\0{caps}\n", id.to_hex())
        };
        pkt_line(&mut out, line.as_bytes());
    }

    if !sent_capabilities {
        let null = repo.object_hash().null();
        pkt_line(
            &mut out,
            format!("{} capabilities^{{}}\0{caps}\n", null.to_hex()).as_bytes(),
        );
    }

    // git's `advertise_shallow_grafts`; the graft list is oid-sorted on both sides.
    if let Ok(Some(commits)) = repo.shallow_commits() {
        for id in commits.iter() {
            pkt_line(&mut out, format!("shallow {}\n", id.to_hex()).as_bytes());
        }
    }

    flush_pkt(&mut out);
    Ok(out)
}

/// The capability list, in `receive-pack.c`'s emission order (`show_ref()`).
///
/// `atomic` and `ofs-delta` default on, `push-options` defaults off, and
/// `push-cert=<nonce>` appears only when `receive.certNonceSeed` gave this
/// session a nonce to hand out.
fn capabilities(repo: &gix::Repository, config: &Config) -> String {
    let snapshot = repo.config_snapshot();

    let mut caps = String::from("report-status report-status-v2 delete-refs side-band-64k quiet");
    if config.advertise_atomic {
        caps.push_str(" atomic");
    }
    if snapshot.boolean("repack.useDeltaBaseOffset").unwrap_or(true) {
        caps.push_str(" ofs-delta");
    }
    if let Some(nonce) = &config.push_cert_nonce {
        caps.push_str(&format!(" push-cert={nonce}"));
    }
    if config.advertise_push_options {
        caps.push_str(" push-options");
    }
    caps.push_str(&format!(" object-format={}", repo.object_hash()));
    caps.push_str(&format!(" agent={}", super::version::user_agent_sanitized()));
    caps
}

/// Append one pkt-line: a four-digit hex length covering the header itself,
/// followed by the payload.
/// Traced as `packet_buf_write()` traces it (pkt-line.c:161).
fn pkt_line(out: &mut Vec<u8>, payload: &[u8]) {
    crate::trace::packet_trace(payload, true);
    frame(out, payload);
}

/// Frame a pkt-line without tracing it, as `send_sideband()` writes (sideband.c:443-470).
fn frame(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

/// Append a flush packet, `packet_buf_flush()` (pkt-line.c:122-126).
fn flush_pkt(out: &mut Vec<u8>) {
    crate::trace::packet_trace(b"0000", true);
    out.extend_from_slice(b"0000");
}

/// One ref-update command a client sent: move `name` from `old` to `new` (a zero
/// `old` is a create, a zero `new` a delete).
struct Command {
    old: gix::ObjectId,
    new: gix::ObjectId,
    name: String,
    /// `cmd->error_string`: the `ng <ref> <reason>` the client is told, and the
    /// flag that keeps this command out of every later phase.
    error: Option<String>,
    /// `cmd->did_not_exist`: a delete of a ref that was already gone. It keeps
    /// the ref out of `post-receive`/`post-update` without being an error.
    did_not_exist: bool,
    /// `cmd->skip_update`: `check_aliased_update()` found this command writing
    /// a ref another command of the push also writes, through a symref, so
    /// `update()` never runs for it (builtin/receive-pack.c:1685-1727). Unlike
    /// `error`, it leaves the `ok` the client is told and the hooks' view alone.
    skip_update: bool,
    /// `cmd->run_proc_receive`: `receive.procReceiveRefs` matched, so the
    /// `proc-receive` hook owns this ref instead of the ref store. Cleared again
    /// when the hook answers `option fall-through`.
    proc_receive: bool,
    /// `RUN_PROC_RECEIVE_RETURNED`: the hook reported a status for this ref.
    proc_receive_returned: bool,
    /// `cmd->report`: what the proc-receive hook wants reported instead of (or
    /// as well as) the ref the client asked for. Only `report-status-v2` can
    /// carry these.
    reports: Vec<PushReport>,
    /// `cmd->index`: where `update_shallow_info()` put this command's new value
    /// in `si->ref` (builtin/receive-pack.c:2386-2391). A delete keeps the `0` its
    /// zeroed allocation gave it, so `si->shallow_ref[cmd->index]` asks a delete
    /// about the first ref that is not one.
    shallow_index: usize,
}

impl Command {
    /// `should_process_cmd()` (builtin/receive-pack.c:1851-1854).
    fn should_process(&self) -> bool {
        self.error.is_none() && !self.skip_update
    }
}

/// git's `struct ref_push_report` — one `option`-decorated line of a
/// `report-status-v2` reply, filled in by the `proc-receive` hook.
#[derive(Default)]
struct PushReport {
    /// `option refname <name>`: the ref that really moved.
    ref_name: Option<String>,
    /// `option old-oid <oid>`.
    old: Option<gix::ObjectId>,
    /// `option new-oid <oid>`.
    new: Option<gix::ObjectId>,
    /// `option forced-update`.
    forced_update: bool,
}

/// One `receive.procReceiveRefs` entry: `[adm!]*:<prefix>` or a bare `<prefix>`
/// (which wants all three of add, delete and modify).
struct ProcReceivePattern {
    want_add: bool,
    want_delete: bool,
    want_modify: bool,
    /// A leading `!` inverts the match: every command *not* under the prefix.
    negative: bool,
    /// The prefix, with trailing slashes stripped as git's parser strips them.
    prefix: String,
}

impl ProcReceivePattern {
    /// `proc_receive_ref_append()`.
    fn parse(value: &str) -> Self {
        let (flags, prefix) = match value.split_once(':') {
            Some((flags, prefix)) => (Some(flags), prefix),
            None => (None, value),
        };
        let (mut want_add, mut want_delete, mut want_modify, mut negative) =
            (false, false, false, false);
        match flags {
            None => {
                want_add = true;
                want_delete = true;
                want_modify = true;
            }
            Some(flags) => {
                for c in flags.chars() {
                    match c {
                        'a' => want_add = true,
                        'd' => want_delete = true,
                        'm' => want_modify = true,
                        '!' => negative = true,
                        _ => {}
                    }
                }
            }
        }
        Self {
            want_add,
            want_delete,
            want_modify,
            negative,
            prefix: prefix.trim_end_matches('/').to_string(),
        }
    }

    /// `proc_receive_ref_matches()`: the command kind has to be wanted, and then
    /// the prefix has to match at a `/` boundary — or, for a `!` pattern, not.
    fn matches(&self, name: &str, creating: bool, deleting: bool) -> bool {
        if (!self.want_add && creating)
            || (!self.want_delete && deleting)
            || (!self.want_modify && !creating && !deleting)
        {
            return false;
        }
        let under = match name.strip_prefix(&self.prefix) {
            Some(rest) => rest.is_empty() || rest.starts_with('/'),
            None => false,
        };
        under != self.negative
    }
}

/// `receive-pack.c`'s `enum deny_action`. `Unconfigured` is distinct from
/// `Refuse` only in that it also prints the advice block explaining how to
/// configure the variable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DenyAction {
    Unconfigured,
    Ignore,
    Warn,
    Refuse,
    UpdateInstead,
}

impl DenyAction {
    /// `receive-pack.c::parse_deny_action`: one of the four names, else the
    /// value read as a boolean (true → refuse, false → ignore).
    fn parse(value: &str) -> Self {
        for (name, action) in [
            ("ignore", DenyAction::Ignore),
            ("warn", DenyAction::Warn),
            ("refuse", DenyAction::Refuse),
            ("updateinstead", DenyAction::UpdateInstead),
        ] {
            if value.eq_ignore_ascii_case(name) {
                return action;
            }
        }
        match value {
            "yes" | "on" | "true" | "1" | "" => DenyAction::Refuse,
            _ => DenyAction::Ignore,
        }
    }
}

/// Everything `receive_pack_config()` collects before the advertisement.
struct Config {
    /// `receive.denyDeletes`: refuse to delete any `refs/heads/` ref.
    deny_deletes: bool,
    /// `receive.denyNonFastForwards`: refuse a branch update that is not a
    /// fast-forward.
    deny_non_fast_forwards: bool,
    /// `receive.denyCurrentBranch`.
    deny_current_branch: DenyAction,
    /// `receive.denyDeleteCurrent`.
    deny_delete_current: DenyAction,
    /// `receive.fsckObjects`, falling back to `transfer.fsckObjects`.
    fsck_objects: bool,
    /// Severities from `receive.fsck.<msg-id>` and `receive.fsck.skipList`.
    fsck_msgs: MsgConfig,
    /// `receive.maxInputSize`; `0` is git's "no limit".
    max_input_size: u64,
    /// `receive.unpackLimit`, falling back to `transfer.unpackLimit`, else 100.
    unpack_limit: u64,
    /// `receive.updateServerInfo`.
    update_server_info: bool,
    /// `transfer.hideRefs` and `receive.hideRefs` in configuration order, trailing
    /// slashes dropped — one list in which the last match wins.
    hide_refs: Vec<String>,
    /// `receive.advertiseAtomic`, which gates both the advertised `atomic`
    /// capability and whether a client asking for it is honoured.
    advertise_atomic: bool,
    /// `receive.advertisePushOptions`, likewise for `push-options`. It also
    /// decides whether the push-option pkt-lines are read off the wire at all —
    /// a client only sends them once the server advertised the capability.
    advertise_push_options: bool,
    /// `receive.certNonceSeed`: the HMAC key half that turns this session into a
    /// signed-push server. Present means `push-cert=<nonce>` is advertised.
    cert_nonce_seed: Option<String>,
    /// `receive.certNonceSlop`: how many seconds a nonce issued by *another*
    /// instance of this server may be off by and still count as ours. Zero (the
    /// default) disables the tolerance entirely.
    cert_nonce_slop: i64,
    /// The nonce handed out in this session's advertisement, `<stamp>-<hmac>`;
    /// `None` without `receive.certNonceSeed`.
    push_cert_nonce: Option<String>,
    /// git's `service_dir`: `<git-dir>` as spelled on the command line, the
    /// other half of the nonce's HMAC key.
    service_dir: String,
    /// The repository's hash algorithm, which is also the nonce's HMAC.
    hash_kind: gix::hash::Kind,
    /// `receive.procReceiveRefs`, in configuration order — the patterns that
    /// divert a command to the `proc-receive` hook instead of the ref store.
    proc_receive_refs: Vec<ProcReceivePattern>,
    /// `receive.keepAlive`: seconds of silence from a hook before an empty
    /// band-1 packet goes out so the client's read does not time out. `0` or
    /// below is git's `KEEPALIVE_NEVER`.
    keep_alive: i64,
    /// `receive.autogc`: run `git maintenance run --auto` once the refs have
    /// moved and the report has been written.
    auto_gc: bool,
    /// `receive.shallowUpdate`: accept a push that needs shallow commits this
    /// repository lacks, extending `.git/shallow` with them, instead of refusing
    /// it with `shallow update not allowed`.
    shallow_update: bool,
}

impl Config {
    /// Read the whole family. The error is the complete `fatal: …` line git
    /// dies with before writing the advertisement.
    ///
    /// `service_dir` is the `<git-dir>` operand exactly as spelled on the
    /// command line; git uses that string, not a canonical path, as half of the
    /// push-certificate nonce's HMAC key.
    fn read(repo: &gix::Repository, service_dir: &str) -> Result<Self, String> {
        let config = repo.config_snapshot();
        let deny = |key: &str| match config.string(key) {
            Some(v) => DenyAction::parse(&v.to_string()),
            None => DenyAction::Unconfigured,
        };
        let hide_refs = hide_ref_patterns(repo, "receive");
        let cert_nonce_seed = config.string("receive.certNonceSeed").map(|v| v.to_string());
        Ok(Self {
            deny_deletes: config.boolean("receive.denyDeletes").unwrap_or(false),
            deny_non_fast_forwards: config
                .boolean("receive.denyNonFastForwards")
                .unwrap_or(false),
            deny_current_branch: deny("receive.denyCurrentBranch"),
            deny_delete_current: deny("receive.denyDeleteCurrent"),
            fsck_objects: config
                .boolean("receive.fsckObjects")
                .or_else(|| config.boolean("transfer.fsckObjects"))
                .unwrap_or(false),
            fsck_msgs: MsgConfig::new(repo, MsgSource::Receive)?,
            max_input_size: config
                .integer("receive.maxInputSize")
                .unwrap_or(0)
                .max(0) as u64,
            unpack_limit: config
                .integer("receive.unpackLimit")
                .or_else(|| config.integer("transfer.unpackLimit"))
                .unwrap_or(100)
                .max(0) as u64,
            update_server_info: config.boolean("receive.updateServerInfo").unwrap_or(false),
            hide_refs,
            advertise_atomic: config.boolean("receive.advertiseAtomic").unwrap_or(true),
            advertise_push_options: config
                .boolean("receive.advertisePushOptions")
                .unwrap_or(false),
            cert_nonce_seed: cert_nonce_seed.clone(),
            cert_nonce_slop: config.integer("receive.certNonceSlop").unwrap_or(0),
            // `cmd_receive_pack()` stamps the nonce right after the config read,
            // so every ref line of one advertisement carries the same one.
            push_cert_nonce: cert_nonce_seed.as_deref().map(|seed| {
                prepare_push_cert_nonce(service_dir, now_seconds(), seed, repo.object_hash())
            }),
            service_dir: service_dir.to_string(),
            hash_kind: repo.object_hash(),
            proc_receive_refs: config
                .raw_values("receive.procReceiveRefs")
                .unwrap_or_default()
                .into_iter()
                .map(|v| ProcReceivePattern::parse(&v.to_string()))
                .collect(),
            keep_alive: config.integer("receive.keepAlive").unwrap_or(5),
            auto_gc: config.boolean("receive.autogc").unwrap_or(true),
            // `receive_pack_config()` (builtin/receive-pack.c:213-216).
            shallow_update: config.boolean("receive.shallowUpdate").unwrap_or(false),
        })
    }

    /// Whether the advertisement and the update path must pretend `name` does
    /// not exist.
    fn ref_is_hidden(&self, name: &str) -> bool {
        ref_is_hidden(&self.hide_refs, name)
    }

    /// The advertisement's spelling of the same question, which has both names
    /// to hand: `ref_is_hidden(strip_namespace(ref->name), ref->name)`
    /// (receive-pack.c:311). `stripped` is `None` for a ref outside the current
    /// namespace, which is exactly the `refname can be NULL` case refs.c:1735
    /// guards — a `^`-anchored pattern still matches such a ref, a plain one
    /// cannot.
    fn ref_is_hidden_qualified(&self, stripped: Option<&str>, full: &str) -> bool {
        ref_is_hidden_qualified(&self.hide_refs, stripped, full)
    }

    /// `receive-pack.c::update()`'s refusals, in git's order. `Some(reason)` is
    /// the `ng <ref> <reason>` status the client prints; the human-readable
    /// half has already gone back on band 2.
    fn refuse(
        &self,
        repo: &gix::Repository,
        cmd: &Command,
        zero: gix::ObjectId,
        head: Option<&str>,
        band: &mut Band,
        worktree: Option<&super::worktree::SharedHead>,
        update_worktree: &mut bool,
    ) -> Option<String> {
        let name = cmd.name.as_str();
        let deleting = cmd.new == zero;

        // ```c
        // const struct worktree *worktree = find_shared_symref(worktrees, "HEAD", name);
        // …
        // if (worktree && !worktree->is_bare) {
        //         switch (deny_current_branch) {
        // ```
        //
        // (receive-pack.c:1506-1541.) `find_shared_symref()` walks every worktree,
        // so a branch a linked worktree has checked out is refused as well, and so
        // is one a detached worktree is rebasing or bisecting. The gate is the
        // *worktree*, not the kind of
        // update: a **delete** of the checked-out branch is refused here, by
        // `receive.denyCurrentBranch`, and never reaches the
        // `receive.denyDeleteCurrent` block below — that one only sees deletes of
        // a branch some worktree has checked out *and* that this check let past,
        // which is `ignore`, `warn` and `updateInstead`.
        if worktree.is_some() {
            match self.deny_current_branch {
                DenyAction::Ignore => {}
                DenyAction::Warn => band.warning("updating the current branch"),
                DenyAction::Refuse | DenyAction::Unconfigured => {
                    // Here the advice follows the error line; the
                    // delete-current path below prints it the other way round.
                    band.error(&format!("refusing to update checked out branch: {name}"));
                    if self.deny_current_branch == DenyAction::Unconfigured {
                        band.write(DENY_CURRENT_BRANCH_ADVICE);
                    }
                    return Some("branch is currently checked out".into());
                }
                // `case DENY_UPDATE_INSTEAD: /* pass -- let other checks intervene
                // first */ do_update_worktree = 1;` (receive-pack.c:1537-1539). The
                // work tree is brought to the pushed tip only once every later check
                // — including the `update` hook — has passed.
                DenyAction::UpdateInstead => *update_worktree = true,
            }
        }

        // ```c
        // if (!is_null_oid(new_oid) &&
        //     !odb_has_object(the_repository->objects, new_oid, …)) {
        //         error("unpack should have generated %s, "
        //               "but I can't find it!", oid_to_hex(new_oid));
        //         ret = "bad pack";
        // ```
        //
        // (receive-pack.c:1544-1551.) `error()`, not `rp_error()`: the line goes
        // to receive-pack's own stderr, never down the side band.
        if !deleting && !repo.has_object(cmd.new) {
            eprintln!("error: unpack should have generated {}, but I can't find it!", cmd.new);
            return Some("bad pack".into());
        }

        if deleting {
            if self.deny_deletes && name.starts_with("refs/heads/") {
                band.error(&format!("denying ref deletion for {name}"));
                return Some("deletion prohibited".into());
            }
            // `if (worktree || (head_name && !strcmp(namespaced_name, head_name)))`
            // (receive-pack.c:1561).
            if worktree.is_some() || head == Some(name) {
                match self.deny_delete_current {
                    DenyAction::Ignore => {}
                    DenyAction::Warn => band.warning("deleting the current branch"),
                    _ => {
                        if self.deny_delete_current == DenyAction::Unconfigured {
                            band.write(DENY_DELETE_CURRENT_ADVICE);
                        }
                        band.error(&format!("refusing to delete the current branch: {name}"));
                        return Some("deletion of the current branch prohibited".into());
                    }
                }
            }
        }

        if self.deny_non_fast_forwards && !deleting && cmd.old != zero && name.starts_with("refs/heads/") {
            // Both ends have to parse as commits before the ancestry question is
            // asked; a tag, tree or blob on either side, or an old value this
            // repository lacks, is `error("bad sha1 objects for %s", name)` —
            // `error()` again, on receive-pack's own stderr — and `bad ref`
            // (receive-pack.c:1581-1594).
            let is_commit = |id| {
                repo.find_header(id)
                    .is_ok_and(|h| h.kind() == gix::object::Kind::Commit)
            };
            if !is_commit(cmd.old) || !is_commit(cmd.new) {
                eprintln!("error: bad sha1 objects for {name}");
                return Some("bad ref".into());
            }
        }
        if self.deny_non_fast_forwards
            && !deleting
            && cmd.old != zero
            && name.starts_with("refs/heads/")
            && !is_fast_forward(repo, cmd.old, cmd.new)
        {
            band.error(&format!("denying non-fast-forward {name} (you should pull first)"));
            return Some("non-fast-forward".into());
        }
        None
    }
}

/// `parse_hide_refs_config()` (refs.c:1688-1708) run over one `repo_config()`
/// walk for `section` (`receive`, `uploadpack`, `fetch`):
///
/// ```c
/// if (!strcmp("transfer.hiderefs", var) ||
///     (!parse_config_key(var, section, NULL, NULL, &key) &&
///      !strcmp(key, "hiderefs"))) {
///         if (!value)
///                 return config_error_nonbool(var);
///         ref = (char *)strvec_push(hide_refs, value);
///         len = strlen(ref);
///         while (len && ref[len - 1] == '/')
///                 ref[--len] = '\0';
/// }
/// ```
///
/// Both keys land in one list in the order the configuration lists them —
/// which is what decides the winner when a `!`-negation from one key overlaps
/// a pattern from the other — and each value loses its trailing slashes, so
/// `refs/heads/` hides every branch rather than nothing. `walk` is the caller's
/// `repo_config()` pass — a server program's names files the way `enter_repo()`
/// left them ([`crate::config::walk_config_after_enter_repo`]). `Err` is the
/// valueless entry the callback refuses; the caller owns the `die()` that
/// follows.
pub fn hide_ref_patterns_checked(
    walk: Vec<crate::config::ConfigValue>,
    section: &str,
) -> std::result::Result<Vec<String>, crate::config::ConfigValue> {
    let own_key = format!("{section}.hiderefs");
    let mut patterns = Vec::new();
    for entry in walk {
        if entry.key != "transfer.hiderefs" && entry.key != own_key {
            continue;
        }
        let Some(value) = entry.value.as_deref() else {
            return Err(entry);
        };
        patterns.push(value.trim_end_matches('/').to_string());
    }
    Ok(patterns)
}

/// [`hide_ref_patterns_checked`] for a command whose own configuration pass
/// (`cmd_config::validate_receive_pack`, `cmd_config::validate_upload_pack`) is
/// where a valueless entry dies; here it only yields no patterns.
pub fn hide_ref_patterns(repo: &gix::Repository, section: &str) -> Vec<String> {
    hide_ref_patterns_checked(crate::config::walk_config(repo), section).unwrap_or_default()
}

/// `refs.c::ref_is_hidden`: the last pattern that matches wins, a leading `!`
/// un-hides, a leading `^` matches the fully qualified (namespaced) name, and a
/// pattern only matches at a `/` boundary or at the end of the name.
pub fn ref_is_hidden(patterns: &[String], name: &str) -> bool {
    ref_is_hidden_qualified(patterns, Some(name), name)
}

/// `refs.c::ref_is_hidden(refname, refname_full, hide_refs)` with both names
/// spelled out: a `^`-prefixed pattern is matched against the *fully qualified*
/// name and every other one against the namespace-stripped name, which is
/// `None` — and therefore matches nothing — for a ref outside the namespace.
pub fn ref_is_hidden_qualified(patterns: &[String], stripped: Option<&str>, full: &str) -> bool {
    for pattern in patterns.iter().rev() {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern.as_str()),
        };
        let (subject, pattern) = match pattern.strip_prefix('^') {
            Some(rest) => (Some(full), rest),
            None => (stripped, pattern),
        };
        // `if (subject && skip_prefix(subject, match, &p) && (!*p || *p == '/'))`
        // — a NULL subject can never match, negation included.
        let Some(subject) = subject else { continue };
        if let Some(rest) = subject.strip_prefix(pattern) {
            if rest.is_empty() || rest.starts_with('/') {
                return !negated;
            }
        }
    }
    false
}

/// Whether `new` has `old` in its ancestry — `repo_in_merge_bases()` reduced to
/// the single-tip question `update()` asks.
fn is_fast_forward(repo: &gix::Repository, old: gix::ObjectId, new: gix::ObjectId) -> bool {
    let Ok(new) = repo.find_object(new) else { return false };
    let Ok(commit) = new.try_into_commit() else { return false };
    let Ok(walk) = commit.ancestors().all() else { return false };
    walk.flatten().any(|info| info.id == old)
}

/// `receive-pack.c::refuse_unconfigured_deny()`'s advice, verbatim.
const DENY_CURRENT_BRANCH_ADVICE: &str = "\
error: By default, updating the current branch in a non-bare repository
is denied, because it will make the index and work tree inconsistent
with what you pushed, and will require 'git reset --hard' to match
the work tree to HEAD.

You can set the 'receive.denyCurrentBranch' configuration variable
to 'ignore' or 'warn' in the remote repository to allow pushing into
its current branch; however, this is not recommended unless you
arranged to update its work tree to match what you pushed in some
other way.

To squelch this message and still keep the default behaviour, set
'receive.denyCurrentBranch' configuration variable to 'refuse'.
";

/// `receive-pack.c::refuse_unconfigured_deny_delete_current()`'s advice,
/// verbatim.
const DENY_DELETE_CURRENT_ADVICE: &str = "\
error: By default, deleting the current branch is denied, because the next
'git clone' won't result in any file checked out, causing confusion.

You can set 'receive.denyDeleteCurrent' configuration variable to
'warn' or 'ignore' in the remote repository to allow deleting the
current branch, with or without a warning message.

To squelch this message, you can set it to 'refuse'.
";

/// Read the command list off stdin, ingest the packfile, apply the ref updates, and
/// write `report-status` — the receiving half of a push. Faithful to git's
/// `receive-pack`/`send-pack` wire for the plain (non-side-band) path that zvcs's own
/// `send-pack` speaks: pkt-line commands, then a flush, then a raw (non-pkt) pack,
/// then a plain pkt-line `report-status`. An empty command list (immediate flush) is
/// a no-op success, matching a client that connects and hangs up.
fn receive(repo: &mut gix::Repository, config: &Config, opts: &Opts) -> Result<ExitCode> {
    let stateless_rpc = opts.stateless_rpc;
    // Each accepted ref update writes a reflog; a bare remote often has no configured
    // identity, so seed a synthesized system default (as git does) to keep the reflog
    // write from failing the push.
    crate::ensure_reflog_identity(repo);

    let mut stdin = std::io::stdin().lock();

    // --- command list (until flush); the first line carries the caps after a NUL.
    let hash = repo.object_hash();
    let zero = gix::ObjectId::null(hash);
    let mut cmds: Vec<Command> = Vec::new();
    let mut caps: Vec<u8> = Vec::new();
    let mut cert: Vec<u8> = Vec::new();
    // `read_head_info(&reader, &shallow)`: the client's `shallow <oid>` lines.
    let mut shallow: Vec<gix::ObjectId> = Vec::new();
    loop {
        // git's `packet_read()` failures are `die()`s, so they print `fatal: `
        // on receive-pack's own stderr and stop the session there.
        let line = match read_pkt_line(&mut stdin) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                eprintln!("fatal: {e}");
                return Ok(ExitCode::from(128));
            }
        };
        let nul = line.iter().position(|&b| b == 0);
        let text = std::str::from_utf8(&line[..nul.unwrap_or(line.len())])
            .map_err(|_| anyhow!("protocol error: non-utf8 command"))?
            .trim_end();

        // A shallow client announces its grafts before the commands
        // (builtin/receive-pack.c:2204-2211). `get_oid_hex()` reads the leading
        // hash and ignores whatever follows it.
        if let Some(rest) = text.strip_prefix("shallow ").filter(|_| line.len() > 8) {
            let hexsz = hash.len_in_hex();
            match rest.get(..hexsz).map(|hex| gix::ObjectId::from_hex(hex.as_bytes())) {
                Some(Ok(id)) => shallow.push(id),
                _ => {
                    eprintln!("fatal: protocol error: expected shallow sha, got '{rest}'");
                    return Ok(ExitCode::from(128));
                }
            }
            continue;
        }

        // `read_head_info()` reads a feature list off every other line that
        // carries one, the first command's included, and only ever sets flags
        // (builtin/receive-pack.c:2212-2246): the `shallow` lines a shallow
        // client sends first carry none, so the list is on the command after them.
        if let Some(n) = nul {
            if !caps.is_empty() {
                caps.push(b' ');
            }
            caps.extend_from_slice(&line[n + 1..]);
        }

        // `push-cert` opens the certificate block: pkt-lines with their newlines
        // intact, up to `push-cert-end`. A flush inside the block ends the whole
        // command list, exactly as git's `true_flush` does.
        if text == "push-cert" {
            match read_push_cert(&mut stdin, &mut cert) {
                Ok(true) => break,
                Ok(false) => continue,
                Err(e) => {
                    eprintln!("fatal: {e}");
                    return Ok(ExitCode::from(128));
                }
            }
        }

        cmds.push(parse_command(text)?);
    }

    // `queue_commands_from_cert()`: a signed push carries its commands *inside*
    // the signed payload, and mixing the two forms is a protocol error.
    if !cert.is_empty() {
        if !cmds.is_empty() {
            eprintln!("fatal: protocol error: got both push certificate and unsigned commands");
            return Ok(ExitCode::from(128));
        }
        match commands_from_cert(&cert) {
            Ok(parsed) => cmds = parsed,
            Err(e) => {
                eprintln!("fatal: {e}");
                return Ok(ExitCode::from(128));
            }
        }
    }
    if cmds.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // Everything git says back travels on band 2 when the client asked for
    // side-band-64k, and on receive-pack's own stderr otherwise.
    let mut band = Band {
        sideband: cap_present(&caps, b"side-band-64k"),
        keep_alive: config.keep_alive,
    };
    // `-q`, or the `quiet` capability the client asked for (builtin/receive-pack.c:2225-2226).
    let quiet = opts.quiet || cap_present(&caps, b"quiet");
    let report_v2 = cap_present(&caps, b"report-status-v2");
    let use_atomic = config.advertise_atomic && cap_present(&caps, b"atomic");
    let use_push_options = config.advertise_push_options && cap_present(&caps, b"push-options");

    // `read_push_options()`: one pkt-line per option, terminated by a flush. They
    // arrive *before* the pack, so the stream desynchronises if they are skipped.
    let mut push_options: Vec<String> = Vec::new();
    if use_push_options {
        loop {
            match read_pkt_line(&mut stdin) {
                Ok(Some(line)) => {
                    push_options.push(String::from_utf8_lossy(&line).trim_end().to_string())
                }
                Ok(None) | Err(_) => break,
            }
        }
    }

    // The certificate repeats the push options it was signed over; a mismatch
    // means the options were tampered with in transit.
    if !check_cert_push_options(&cert, &push_options) {
        for cmd in &mut cmds {
            cmd.error = Some("inconsistent push options".into());
        }
    }

    // --- ingest the pack (skipped when every command is a delete) -----------
    let mut unpack: Result<(), String> = Ok(());
    // The object transaction (builtin/receive-pack.c:2502, :2587-2597): the
    // quarantine the pack goes into, and the `.keep` that `index-pack` left for it,
    // which `odb_transaction_finalize()` deletes from wherever the migrate put it
    // once the commands have run (object-file.c:1452-1464). An unmigrated
    // quarantine lives as long as git's process does, so it is dropped only on the
    // way out of this function.
    let mut transaction: Option<Transaction> = None;
    // `prepare_shallow_info(&si, &shallow)`, and `shallow_update` cleared when the
    // client listed nothing to consider (builtin/receive-pack.c:2578-2580).
    let mut si = ShallowInfo::prepare(repo, shallow, config.shallow_update);
    if cmds.iter().any(|c| c.new != zero) {
        // `alt_shallow_file = setup_temporary_shallow(si.shallow)` when there are
        // shallow commits at all (:2582-2585). It is the unpack child's
        // `--shallow-file`, which only a `--strict` link check would consult, and
        // the in-process ingest here does not walk links.
        let alt_shallow_file = if si.has_any() { si.setup_temporary_shallow(repo) } else { String::new() };
        match Transaction::begin(repo) {
            None => unpack = Err("unable to start object transaction".into()),
            Some(mut t) => {
                // git reports the *child*'s failure rather than the reason; the reason
                // itself has already gone back on band 2, printed by that child.
                // `quiet` keeps `index-pack` from drawing its delta meter.
                if let Err(status) = ingest_pack(repo, &mut t, &mut stdin, config, &mut band, quiet) {
                    unpack = Err(status);
                }
                transaction = Some(t);
            }
        }
        // `update_shallow_info(commands, &si, &ref, alt_shallow_file)` (:2592), which
        // reads the objects the pack brought from the quarantine.
        let incoming = transaction
            .as_ref()
            .and_then(|t| t.quarantine.as_ref())
            .and_then(|q| Incoming::open(repo, q.path()).ok());
        let in_repo = find_in_repo(repo);
        let find_incoming = |id| incoming.as_ref().and_then(|i| i.find(id));
        let find: FindObject<'_> = if incoming.is_some() { &find_incoming } else { &in_repo };
        si.update_shallow_info(repo, find, &mut cmds, zero, &alt_shallow_file);
    }

    // `prepare_push_cert_sha1()`: store the certificate as a blob, check its
    // signature, and grade the nonce it echoed back. Everything it learns is
    // handed to the hooks as `GIT_PUSH_CERT*`.
    let cert = PushCert::prepare(repo, cert, config, stateless_rpc);

    // --- execute ------------------------------------------------------------
    execute_commands(
        repo,
        config,
        &mut cmds,
        &unpack,
        zero,
        use_atomic,
        use_push_options.then_some(&push_options[..]),
        cert.as_ref(),
        transaction.as_mut(),
        !opts.skip_connectivity_check,
        quiet,
        &mut si,
        &mut band,
    );
    // The commands `update_shallow_ref()` cut off the list are gone from the
    // report and the post hooks as well.
    if let Some(n) = si.cut_at {
        cmds.truncate(n);
    }
    // `odb_transaction_finalize()` (object-file.c:1452-1464).
    if let Some(t) = &mut transaction {
        t.finalize();
    }

    // --- report-status ------------------------------------------------------
    if cap_present(&caps, b"report-status") || report_v2 {
        let mut report: Vec<u8> = Vec::new();
        match &unpack {
            Ok(()) => pkt_line(&mut report, b"unpack ok\n"),
            Err(e) => pkt_line(&mut report, format!("unpack {e}\n").as_bytes()),
        }
        for cmd in &cmds {
            match &cmd.error {
                Some(reason) => {
                    pkt_line(&mut report, format!("ng {} {reason}\n", cmd.name).as_bytes())
                }
                None => {
                    pkt_line(&mut report, format!("ok {}\n", cmd.name).as_bytes());
                    // `report_v2()`: the proc-receive hook's rewritten refs ride
                    // along as `option` lines, one `ok` header per extra report.
                    if report_v2 {
                        for (n, r) in cmd.reports.iter().enumerate() {
                            if n > 0 {
                                pkt_line(&mut report, format!("ok {}\n", cmd.name).as_bytes());
                            }
                            if let Some(name) = &r.ref_name {
                                pkt_line(&mut report, format!("option refname {name}\n").as_bytes());
                            }
                            if let Some(old) = &r.old {
                                pkt_line(&mut report, format!("option old-oid {old}\n").as_bytes());
                            }
                            if let Some(new) = &r.new {
                                pkt_line(&mut report, format!("option new-oid {new}\n").as_bytes());
                            }
                            if r.forced_update {
                                pkt_line(&mut report, b"option forced-update\n");
                            }
                        }
                    }
                }
            }
        }
        flush_pkt(&mut report);

        let mut out: Vec<u8> = Vec::new();
        if band.sideband {
            // The report-status stream, its own flush included, is the band-1
            // payload. The flush that ends the *multiplexed* stream comes last of
            // all, once the post hooks have had their say on band 2.
            for chunk in report.chunks(MAX_SIDEBAND_PAYLOAD) {
                let mut payload = vec![1u8];
                payload.extend_from_slice(chunk);
                frame(&mut out, &payload);
            }
        } else {
            out = report;
        }
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&out)?;
        stdout.flush()?;
    }

    // The two notification hooks run after the client has been told the outcome.
    let env = hook_env(repo, &push_options, cert.as_ref());
    run_receive_hook(repo, "post-receive", &cmds, true, &env, &mut band);
    run_post_update_hook(repo, &cmds, &env, &mut band);

    // `receive.autogc`: git 2.55 no longer forks `gc --auto` here — it builds a
    // `maintenance run --auto` child through `prepare_auto_maintenance()`, whose
    // own `maintenance.auto` / `gc.auto` gate decides whether it runs at all.
    if config.auto_gc {
        let _ = super::maintenance::run_auto_maintenance(repo, true);
    }

    // `cmd_receive_pack()` refreshes the dumb-transport info files last of all.
    if config.update_server_info && cmds.iter().any(|c| c.error.is_none()) {
        update_server_info(repo);
    }

    // `if (use_sideband) packet_flush(1)`: the very last byte of the session,
    // after every band-2 word the post hooks and the maintenance run produced.
    if band.sideband {
        let mut stdout = std::io::stdout().lock();
        crate::trace::packet_trace(b"0000", true);
        stdout.write_all(b"0000")?;
        stdout.flush()?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `queue_command()`: one `<old> <new> <ref>` command line.
fn parse_command(text: &str) -> Result<Command> {
    let mut it = text.splitn(3, ' ');
    let (o, n, name) = match (it.next(), it.next(), it.next()) {
        (Some(o), Some(n), Some(name)) if !name.is_empty() => (o, n, name),
        // `die("protocol error: expected old/new/ref, got '%s'", line)`
        // (receive-pack.c:2156): git single-quotes the offending line rather
        // than rendering it as a Rust debug string.
        _ => crate::git_fatal!("protocol error: expected old/new/ref, got '{text}'"),
    };
    Ok(Command {
        old: gix::ObjectId::from_hex(o.as_bytes())
            .map_err(|_| anyhow!("protocol error: bad old id"))?,
        new: gix::ObjectId::from_hex(n.as_bytes())
            .map_err(|_| anyhow!("protocol error: bad new id"))?,
        name: name.to_string(),
        error: None,
        did_not_exist: false,
        skip_update: false,
        proc_receive: false,
        proc_receive_returned: false,
        reports: Vec::new(),
        shallow_index: 0,
    })
}

/// `execute_commands()` — everything between "the pack is in" and "tell the
/// client", in git's order: hidden refs, proc-receive scheduling, `pre-receive`,
/// the proc-receive hook, then the ref updates themselves.
#[allow(clippy::too_many_arguments)]
fn execute_commands(
    repo: &gix::Repository,
    config: &Config,
    cmds: &mut [Command],
    unpack: &Result<(), String>,
    zero: gix::ObjectId,
    use_atomic: bool,
    push_options: Option<&[String]>,
    cert: Option<&PushCert>,
    transaction: Option<&mut Transaction>,
    check_connectivity: bool,
    quiet: bool,
    si: &mut ShallowInfo,
    band: &mut Band,
) {
    if unpack.is_err() {
        for cmd in cmds.iter_mut() {
            cmd.error = Some("unpacker error".into());
        }
        return;
    }

    // `if (!skip_connectivity_check)` (builtin/receive-pack.c:2034-2059): one
    // `check_connected()` over every tip a command still means to write
    // (`iterate_receive_command_list()`, :1806-1821), its rev-list's stderr on
    // band 2 when there is one (`opt.err_fd = muxer.in`), and the meter only
    // there and only unquiet (`opt.progress = err_fd && !quiet`). When it fails,
    // `set_connectivity_errors()` asks again one command at a time.
    // A command `update_shallow_ref()` will check is left out of both
    // (`shallow_update && si->shallow_ref[cmd->index]`, :1784-1786, :1813-1815).
    if check_connectivity {
        let env = odb_transaction_env(transaction.as_deref());
        let tips: Vec<gix::ObjectId> = cmds
            .iter()
            .filter(|c| !si.deferred(c) && c.new != zero && !c.skip_update)
            .map(|c| c.new)
            .collect();
        let progress = band.sideband && !quiet;
        let relay = band.sideband.then_some(&mut *band);
        if check_connected(repo, &tips, env, true, relay, progress, None) {
            set_connectivity_errors(repo, cmds, zero, env, si);
        }
    }

    // `reject_updates_to_hidden()`: a ref the advertisement suppressed cannot be
    // touched. The status is the whole message — git prints nothing else.
    for cmd in cmds.iter_mut() {
        if cmd.error.is_some() || !config.ref_is_hidden(&cmd.name) {
            continue;
        }
        cmd.error = Some(if cmd.new == zero {
            "deny deleting a hidden ref".into()
        } else {
            "deny updating a hidden ref".into()
        });
    }

    // Commands whose ref name matches `receive.procReceiveRefs` never reach the
    // ref store; the hook owns them.
    let mut run_proc_receive = false;
    if !config.proc_receive_refs.is_empty() {
        for cmd in cmds.iter_mut() {
            if cmd.error.is_some() {
                continue;
            }
            if config
                .proc_receive_refs
                .iter()
                .any(|p| p.matches(&cmd.name, cmd.old == zero, cmd.new == zero))
            {
                cmd.proc_receive = true;
                run_proc_receive = true;
            }
        }
    }

    // `run_receive_hook(commands, "pre-receive", 0, transaction, push_options)`: the
    // only hook that runs inside the quarantine, and so the only one handed
    // `odb_transaction_env()` (builtin/receive-pack.c:944-945, :2081).
    let mut env = hook_env(repo, push_options.unwrap_or_default(), cert);
    env.set.extend(
        odb_transaction_env(transaction.as_deref())
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string_lossy().into_owned())),
    );
    if run_receive_hook(repo, "pre-receive", cmds, false, &env, band) {
        for cmd in cmds.iter_mut() {
            if cmd.error.is_none() {
                cmd.error = Some("pre-receive hook declined".into());
            }
        }
        return;
    }
    // If there is no command ready to run, should return directly to destroy
    // temporary data in the quarantine area.
    if cmds.iter().all(|c| c.error.is_some()) {
        return;
    }

    // Now we'll start writing out refs, which means the objects need
    // to be in their final positions so that other processes can see them
    // (`odb_transaction_commit()`, builtin/receive-pack.c:2097-2108).
    if let Some(t) = transaction {
        if !t.commit(repo) {
            for cmd in cmds.iter_mut() {
                if cmd.error.is_none() {
                    cmd.error = Some("unable to migrate objects to permanent storage".into());
                }
            }
            return;
        }
    }

    check_aliased_updates(repo, cmds, band);

    let head = head_name(repo);

    if run_proc_receive && run_proc_receive_hook(repo, cmds, push_options, use_atomic, band) {
        for cmd in cmds.iter_mut() {
            if cmd.error.is_none()
                && !cmd.proc_receive_returned
                && (cmd.proc_receive || use_atomic)
            {
                cmd.error = Some("fail to run proc-receive hook".into());
            }
        }
    }

    if use_atomic {
        execute_commands_atomic(repo, config, cmds, zero, head.as_deref(), si, band);
    } else {
        execute_commands_non_atomic(repo, config, cmds, zero, head.as_deref(), si, band);
    }

    // `if (shallow_update) BUG_if_skipped_connectivity_check(commands, si);`
    // (:2129-2130, :1856-1867), over the list as `update_shallow_ref()` left it.
    // The `__LINE__` stock 2.56 (clang) reports for the two-line `bug()` call is 1864.
    if si.update {
        let live = si.cut_at.unwrap_or(cmds.len());
        let mut bugged = false;
        for cmd in &cmds[..live] {
            if cmd.should_process() && si.deferred(cmd) {
                eprintln!("BUG: builtin/receive-pack.c:1864: connectivity check has not been run on ref {}", cmd.name);
                bugged = true;
            }
        }
        if bugged {
            eprintln!("BUG: builtin/receive-pack.c:1866: connectivity check skipped???");
            std::process::abort();
        }
    }
}

/// `update()`'s checks for one command, short of writing anything: the deny
/// policies, then the `update` hook, then the compare-and-swap it would queue.
fn check_command(
    repo: &gix::Repository,
    config: &Config,
    cmd: &Command,
    next: Option<&Command>,
    zero: gix::ObjectId,
    head: Option<&str>,
    si: &mut ShallowInfo,
    band: &mut Band,
) -> Result<RefEdit, String> {
    // `update()`'s first test (receive-pack.c:1094-1098): the name must live under
    // `refs/` and what follows has to be a valid refname *in its own right*, so a
    // one-level name like `refs/stash` is refused. git's own matching pass leaves
    // such a ref out of a `--mirror`, which is why this fires only for a refspec
    // that names one explicitly.
    //
    // ```c
    // check_refname_format(name + 5, is_null_oid(new_oid) ?
    //                      REFNAME_ALLOW_ONELEVEL : 0)
    // ```
    //
    // A delete may name a one-level ref, so `:refs/foo` gets past this and on
    // to `deleting a non-existent ref` (receive-pack.c:1491-1497).
    let flags = if cmd.new == zero { super::check_ref_format::ALLOW_ONELEVEL } else { 0 };
    let funny = match cmd.name.strip_prefix("refs/") {
        Some(tail) => !super::check_ref_format::check_refname_format(tail.as_bytes(), flags),
        None => true,
    };
    if funny {
        band.error(&format!("refusing to update funny ref '{}' remotely", cmd.name));
        return Err("funny refname".into());
    }
    let worktree = super::worktree::find_shared_head(repo, &cmd.name);
    let mut update_worktree = false;
    if let Some(reason) = config.refuse(repo, cmd, zero, head, band, worktree.as_ref(), &mut update_worktree) {
        return Err(reason);
    }
    // The `update` hook is the last of `update()`'s checks, and a non-zero exit
    // vetoes just this one ref.
    if run_update_hook(repo, cmd, band) {
        band.error(&format!("hook declined to update {}", cmd.name));
        return Err("hook declined".into());
    }
    // `if (do_update_worktree) { ret = update_worktree(new_oid->hash, worktree); … }`
    // (receive-pack.c:1617-1621), after the hook and before the ref is queued: a work
    // tree that cannot be moved is a rejection, not a half-done update.
    if let Some(worktree) = worktree.as_ref().filter(|_| update_worktree) {
        if let Some(reason) = update_worktree_to(repo, worktree, cmd.new) {
            return Err(reason);
        }
    }
    // ```c
    // if (is_null_oid(new_oid)) {
    //         if (!parse_object(the_repository, old_oid)) {
    //                 old_oid = NULL;
    //                 if (refs_ref_exists(get_main_ref_store(the_repository), name))
    //                         rp_warning("allowing deletion of corrupt ref");
    //                 else
    //                         rp_warning("deleting a non-existent ref");
    //         }
    // ```
    //
    // (receive-pack.c:1622-1632.) The question is about the *object* the pusher
    // named as the old value, not about the ref: a delete whose old value this
    // repository cannot look up is downgraded to an unconditional delete and
    // warned about, while a delete naming a real object keeps its
    // compare-and-swap and is silent even when the ref itself is gone — that
    // case is a lock failure below, not a warning.
    let mut old = cmd.old;
    if cmd.new == zero && repo.find_object(old).is_err() {
        if repo
            .try_find_reference(cmd.name.as_str())
            .ok()
            .flatten()
            .is_some()
        {
            band.warning("allowing deletion of corrupt ref");
        } else {
            band.warning("deleting a non-existent ref");
        }
        old = zero;
    }
    // `ref_transaction_update()`'s check of the new value (refs.c:1433-1448),
    // which `update()` reports with `rp_error("%s", err.buf)` and the
    // `ref_transaction_error_msg()` of `REF_TRANSACTION_ERROR_INVALID_NEW_VALUE`
    // as the status (receive-pack.c:1635-1650). The name it judges is
    // `namespaced_name`, so under a namespace no ref is a branch to
    // `is_branch()`.
    //
    // Ahead of it, `if (shallow_update && si->shallow_ref[cmd->index] &&
    // update_shallow_ref(cmd, si)) ret = "shallow error";` (:1629-1633): a ref that
    // needs shallow commits extends `.git/shallow` first, and is refused when its
    // tip is not connected even then.
    if cmd.new != zero && si.deferred(cmd) && !si.update_shallow_ref(repo, cmd, next, zero) {
        return Err("shallow error".into());
    }
    if cmd.new != zero {
        let namespaced = format!(
            "{}{}",
            repo.namespace().map(|ns| ns.as_bstr().to_string()).unwrap_or_default(),
            cmd.name
        );
        match repo.find_header(cmd.new) {
            Err(_) => {
                band.error(&format!(
                    "trying to write ref '{namespaced}' with nonexistent object {}",
                    cmd.new
                ));
                return Err("invalid new value provided".into());
            }
            Ok(header) if header.kind() != gix::object::Kind::Commit && namespaced.starts_with("refs/heads/") => {
                band.error(&format!(
                    "trying to write non-commit object {} to branch '{namespaced}'",
                    cmd.new
                ));
                return Err("invalid new value provided".into());
            }
            Ok(_) => {}
        }
    }
    ref_edit(&cmd.name, old, cmd.new, zero)
}

/// `lock_ref_for_update()` (refs/files-backend.c:2639-2675) and the old-value
/// comparison it wraps (:2593-2610), reduced to the two ways a push's
/// compare-and-swap fails.
///
/// The pair is the `error: <text>` git prints when `ref_transaction_commit()`
/// rejects the update and the `ng <ref> <reason>` the client is told, which is
/// `ref_transaction_error_msg()` of the backend's error code (refs.c:3532-3550).
/// gitoxide's own wording for the same two refusals is its own, so it is
/// translated here rather than forwarded.
fn lock_failure(repo: &gix::Repository, edit: &RefEdit) -> Option<(String, &'static str)> {
    let expected = match &edit.change {
        Change::Update { expected, .. } | Change::Delete { expected, .. } => expected,
    };
    let name = edit.name.as_bstr().to_string();
    // The value the ref holds now, with symbolic refs followed and tag objects
    // left unpeeled — what git compares `old_oid` against.
    let current = repo
        .try_find_reference(name.as_str())
        .ok()
        .flatten()
        .and_then(|mut r| r.follow_to_object().ok())
        .map(|id| id.detach());
    match expected {
        // `is_null_oid(&update->old_oid)` with something already there.
        PreviousValue::MustNotExist => current.map(|_| {
            (
                format!("cannot lock ref '{name}': reference already exists"),
                "reference already exists",
            )
        }),
        PreviousValue::MustExistAndMatch(Target::Object(old)) => match current {
            // `lock_raw_ref()` with `mustexist`, which is set for any non-null
            // old value, fails before the value comparison is reached.
            None => Some((
                format!("cannot lock ref '{name}': unable to resolve reference '{name}'"),
                "reference does not exist",
            )),
            Some(cur) if cur != *old => Some((
                format!("cannot lock ref '{name}': is at {cur} but expected {old}"),
                "incorrect old value provided",
            )),
            Some(_) => None,
        },
        _ => None,
    }
}

/// `execute_commands_non_atomic()`: every command stands or falls alone.
fn execute_commands_non_atomic(
    repo: &gix::Repository,
    config: &Config,
    cmds: &mut [Command],
    zero: gix::ObjectId,
    head: Option<&str>,
    si: &mut ShallowInfo,
    band: &mut Band,
) {
    // Deletions go first, in a transaction of their own, so a delete and a
    // create that would meet in a D/F conflict both go through
    // (`PHASE_DELETIONS`, `PHASE_OTHERS`, receive-pack.c:1895-1917). Within a
    // phase `update()` queues every command in one transaction that may fail
    // partially, `REF_TRANSACTION_ALLOW_FAILURE` (:1918-1932), so the
    // `reference-transaction` hook sees the phase's updates together, every
    // check's message precedes the backend's, and those come in the order the
    // updates were refused (`ref_transaction_for_each_rejected_update()`, :1940-1945).
    for deletions in [true, false] {
        // The transaction is begun lazily, for the first command of the phase,
        // whether or not `update()` then queues anything in it (:1919-1932).
        let mut begun = false;
        let mut queued = Vec::new();
        for i in 0..cmds.len() {
            if si.cut_at.is_some_and(|n| i >= n) {
                break;
            }
            if !cmds[i].should_process() || cmds[i].proc_receive || (cmds[i].new == zero) != deletions {
                continue;
            }
            begun = true;
            match check_command(repo, config, &cmds[i], cmds.get(i + 1), zero, head, si, band) {
                Ok(edit) => queued.push(edit),
                Err(reason) => cmds[i].error = Some(reason),
            }
            si.note_cut(i);
        }
        if !begun {
            continue;
        }
        // `if (ref_transaction_commit(transaction, &err)) { rp_error("%s", err.buf);
        // reported_error = "failed to update refs"; goto failure; }` (:1937-1941): a
        // failure of the whole transaction is every command's error, the earlier
        // phase's included.
        let rejections = match repo.edit_references_allowing_failure(queued) {
            Ok((_, rejections)) => rejections,
            Err(e) => {
                band.error(&transaction_error(&e));
                for cmd in cmds.iter_mut() {
                    cmd.error = Some("failed to update refs".into());
                }
                continue;
            }
        };
        // `ref_transaction_rejection_handler()` (:1868-1883) keys each refusal by the
        // name of the update refused, which for an update through a symbolic ref is
        // the ref it points at: the command naming the symref is not among them.
        let mut failed_refs = std::collections::HashMap::new();
        for rejection in &rejections {
            if !rejection.message.is_empty() {
                band.error(&rejection.message.to_string());
            }
            failed_refs.insert(rejection.refname.to_string(), rejection.kind.message());
        }
        for cmd in cmds.iter_mut() {
            if let Some(reason) = failed_refs.get(&cmd.name) {
                cmd.error = Some(reason.to_string());
            }
        }
    }
}

/// The `err` text of a transaction that failed as a whole: git's own for a
/// refusal of the reftable backend, the gitoxide error chain otherwise.
fn transaction_error(e: &gix::reference::edit::Error) -> String {
    use gix::refs::file::transaction::prepare;
    if let gix::reference::edit::Error::FileTransactionPrepare(prepare::Error::Reftable { message, .. }) = e {
        return message.to_string();
    }
    e.to_string()
}

/// `execute_commands_atomic()`: one transaction over every command, so a refusal
/// anywhere leaves the whole push unwritten. The first command to fail keeps its
/// own reason; the rest are told `atomic push failure`, or
/// `atomic transaction failed` when it was the commit that would not go through.
fn execute_commands_atomic(
    repo: &gix::Repository,
    config: &Config,
    cmds: &mut [Command],
    zero: gix::ObjectId,
    head: Option<&str>,
    si: &mut ShallowInfo,
    band: &mut Band,
) {
    let mut edits = Vec::new();
    let mut reported: Option<&str> = None;

    for i in 0..cmds.len() {
        if si.cut_at.is_some_and(|n| i >= n) {
            break;
        }
        if !cmds[i].should_process() || cmds[i].proc_receive {
            continue;
        }
        let checked = check_command(repo, config, &cmds[i], cmds.get(i + 1), zero, head, si, band);
        si.note_cut(i);
        match checked {
            Ok(edit) => edits.push(edit),
            Err(reason) => {
                cmds[i].error = Some(reason);
                reported = Some("atomic push failure");
                break;
            }
        }
    }

    if reported.is_none() && !edits.is_empty() {
        // `if (ref_transaction_commit(transaction, &err)) { rp_error("%s", err.buf); … }`
        // (receive-pack.c:2010-2014): one message, from the first update the
        // backend could not lock, and every command then carries the same reason.
        // The transaction runs, so the `reference-transaction` hook sees it at
        // `preparing` before the backend refuses it (refs.c:2711-2721); git's text for
        // the refusal is read off the refs it left untouched.
        let checked: Vec<_> = edits.iter().map(|edit| lock_failure(repo, edit)).collect();
        if let Err(e) = repo.edit_references(edits) {
            let text = checked
                .into_iter()
                .flatten()
                .next()
                .map_or_else(|| transaction_error(&e), |(text, _)| text);
            band.error(&text);
            reported = Some("atomic transaction failed");
        }
    }

    if let Some(reported) = reported {
        for cmd in cmds.iter_mut() {
            if cmd.error.is_none() {
                cmd.error = Some(reported.into());
            }
        }
    }
}

/// `odb_transaction_env()`: the quarantine's `GIT_OBJECT_DIRECTORY`,
/// `GIT_ALTERNATE_OBJECT_DIRECTORIES` and `GIT_QUARANTINE_PATH`, which a child must
/// be handed to see the objects the push brought before they are migrated.
fn odb_transaction_env(transaction: Option<&Transaction>) -> &[(String, std::ffi::OsString)] {
    transaction
        .and_then(|t| t.quarantine.as_ref())
        .map(|q| q.env())
        .unwrap_or_default()
}

/// `check_connected()` (connected.c:76-200) as receive-pack calls it, with no
/// transport: `git [--shallow-file <file>] rev-list --objects --stdin --not
/// [--exclude-hidden=receive] --all --quiet --alternate-refs` fed `tips`, which
/// fails when anything they reach is missing from the repository. Returns
/// whether it failed — the C function's non-zero.
///
/// `relay` is `opt.err_fd`: the child's stderr goes to band 2 through
/// [`relay_child_output`] when it is set, and is inherited otherwise
/// (`rev_list.no_stderr = opt->quiet`, which receive-pack never sets).
/// `shallow_file` is `opt.shallow_file`, which only `update_shallow_ref()` sets.
fn check_connected(
    repo: &gix::Repository,
    tips: &[gix::ObjectId],
    env: &[(String, std::ffi::OsString)],
    exclude_hidden: bool,
    relay: Option<&mut Band>,
    progress: bool,
    shallow_file: Option<&str>,
) -> bool {
    // `oid = fn(cb_data); if (!oid) return err;`
    let mut tips = tips;
    if tips.is_empty() {
        return false;
    }
    // `check_connected_promisor()` (connected.c:39-67): connected outright when
    // every tip is in a promisor pack. The walk stops at the first tip that is
    // not, and rev-list is fed from that one on — the tips before it are not
    // asked about again.
    let promisor = super::rev_list::has_promisor_remote(repo);
    if promisor {
        let held = super::rev_list::promisor_pack_objects(repo);
        match tips.iter().position(|id| !held.contains(id)) {
            None => return false,
            Some(first) => tips = &tips[first..],
        }
    }

    let mut cmd = match crate::hosted::git_exe() {
        Ok(exe) => std::process::Command::new(exe),
        Err(_) => {
            eprintln!("error: Could not run 'git rev-list'");
            return true;
        }
    };
    // `opt->shallow_file` goes ahead of the subcommand (connected.c:115-118).
    if let Some(file) = shallow_file {
        cmd.args(["--shallow-file", file]);
    }
    cmd.args(["rev-list", "--objects", "--stdin"]);
    if promisor {
        cmd.arg("--exclude-promisor-objects");
    }
    cmd.arg("--not");
    // `opt->exclude_hidden_refs_section`, which only the joint check sets, to
    // `"receive"` (builtin/receive-pack.c:2051).
    if exclude_hidden {
        cmd.arg("--exclude-hidden=receive");
    }
    cmd.args(["--all", "--quiet", "--alternate-refs"]);
    if progress {
        cmd.arg("--progress=Checking connectivity");
    }
    let (dir, git_dir) = enter_repo_dir(repo);
    cmd.current_dir(dir)
        .env("GIT_DIR", git_dir)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::piped())
        // `rev_list.no_stdout = 1`.
        .stdout(std::process::Stdio::null())
        .stderr(if relay.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::inherit()
        });
    let Ok(mut child) = cmd.spawn() else {
        eprintln!("error: Could not run 'git rev-list'");
        return true;
    };

    // `fprintf(rev_list_in, "%s\n", oid_to_hex(oid))` for each tip, with SIGPIPE
    // ignored: a rev-list that died early is reported by its exit status, and only
    // a write failure other than EPIPE/EINVAL is worth a word of its own.
    let mut failed = false;
    if let Some(mut sink) = child.stdin.take() {
        let text: String = tips.iter().map(|id| format!("{id}\n")).collect();
        if let Err(e) = sink.write_all(text.as_bytes()).and_then(|()| sink.flush()) {
            if !matches!(e.raw_os_error(), Some(libc::EPIPE) | Some(libc::EINVAL)) {
                eprintln!("error: failed write to rev-list: {}", crate::external::strerror(&e));
            }
            failed = true;
        }
    }
    if let Some(band) = relay {
        relay_child_output(&mut child, band);
    }
    !child.wait().is_ok_and(|status| status.success()) || failed
}

/// `set_connectivity_errors()` (builtin/receive-pack.c:1773-1799): after the
/// joint check failed, each command's tip on its own — every command, whatever
/// it already carries — with the child's stderr on receive-pack's own (`opt`
/// has no `err_fd` here), no meter and no `--exclude-hidden`. A tip that fails
/// is `missing necessary objects`; a delete has no tip and passes
/// (`command_singleton_iterator()`).
fn set_connectivity_errors(
    repo: &gix::Repository,
    cmds: &mut [Command],
    zero: gix::ObjectId,
    env: &[(String, std::ffi::OsString)],
    si: &ShallowInfo,
) {
    for cmd in cmds.iter_mut() {
        // "to be checked in update_shallow_ref()" (:1784-1786).
        if si.deferred(cmd) {
            continue;
        }
        let tip: &[gix::ObjectId] = if cmd.new == zero { &[] } else { std::slice::from_ref(&cmd.new) };
        if check_connected(repo, tip, env, false, None, false, None) {
            cmd.error = Some("missing necessary objects".into());
        }
    }
}

// ---------------------------------------------------------------------------
// Shallow pushes (shallow.c:516-892, builtin/receive-pack.c:1332-1366, :2329-2409)
// ---------------------------------------------------------------------------

/// An object as receive-pack's own process sees it: before the migrate that is
/// the quarantine with the real store behind it (`tmp_objdir_replace_primary_odb()`,
/// object-file.c:514-518), after it the real store alone.
type FindObject<'a> = &'a dyn Fn(gix::ObjectId) -> Option<(gix::object::Kind, Vec<u8>)>;

/// The real store, for the lookups that come after the migrate.
fn find_in_repo(repo: &gix::Repository) -> impl Fn(gix::ObjectId) -> Option<(gix::object::Kind, Vec<u8>)> + '_ {
    move |id| {
        let object = repo.find_object(id).ok()?;
        Some((object.kind, object.data.clone()))
    }
}

/// `struct shallow_info` (shallow.h:64-76) together with the receive-pack state
/// around it: `shallow_update`, the commit-graft table the process parses
/// commits with, the stat data `is_repository_shallow()` took of `.git/shallow`,
/// and the temporary shallow file `setup_temporary_shallow()` made.
#[derive(Default)]
struct ShallowInfo {
    /// `si->shallow`: the `shallow <oid>` lines the client sent, in wire order.
    shallow: Vec<gix::ObjectId>,
    /// `si->ours`: indices into `shallow` of the commits this repository has.
    ours: Vec<usize>,
    /// `si->theirs`: indices into `shallow` of the commits it lacks.
    theirs: Vec<usize>,
    /// `si->ref`: the new value of every command that is not a delete, which
    /// `cmd->index` points into.
    refs: Vec<gix::ObjectId>,
    /// `si->used_shallow`: per shallow commit, the bitmap of the refs that reach
    /// it, or `None` when none does.
    used_shallow: Vec<Option<Vec<u32>>>,
    /// `si->need_reachability_test`.
    need_reachability_test: Vec<bool>,
    /// `si->reachable`.
    reachable: Vec<bool>,
    /// `si->shallow_ref`: per ref, how many shallow commits it needs.
    shallow_ref: Vec<u32>,
    /// Whether `prepare_shallow_update()` allocated `shallow_ref` at all.
    shallow_ref_allocated: bool,
    /// `si->commits`: the ref tips `delayed_reachability_test()` collects once.
    commits: Option<Vec<gix::ObjectId>>,
    /// `shallow_update`: `receive.shallowUpdate`, cleared when the push brings no
    /// shallow commit to consider.
    update: bool,
    /// `the_repository->parsed_objects->grafts`: `info/grafts` and `.git/shallow`,
    /// plus every commit `update_shallow_ref()` registered.
    grafts: gix::revwalk::graft::Table,
    /// `shallow_stat`: what `.git/shallow` looked like when it was read, `None`
    /// when there was no such file.
    shallow_stat: Option<(u64, u64, i64, i64, i64, i64)>,
    /// The `shallow_XXXXXX` tempfile, removed when receive-pack exits.
    alt_shallow_file: Option<std::path::PathBuf>,
    /// `update_shallow_ref()` cut the command list after the command it ran for
    /// (see [`ShallowInfo::update_shallow_ref`]).
    cut: bool,
    /// How many commands the list still holds once it was cut.
    cut_at: Option<usize>,
}

impl Drop for ShallowInfo {
    /// The tempfile machinery removes `setup_temporary_shallow()`'s file at exit.
    fn drop(&mut self) {
        if let Some(path) = self.alt_shallow_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// `stat_validity_update()`'s view of a file: device, inode, size, mtime and ctime.
fn shallow_stat(path: &std::path::Path) -> Option<(u64, u64, i64, i64, i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some((m.dev(), m.ino(), m.size() as i64, m.mtime() * 1_000_000_000 + m.mtime_nsec(), m.ctime(), m.ctime_nsec()))
}

impl ShallowInfo {
    /// `prepare_shallow_info()` (shallow.c:518-544), "Step 1, split sender
    /// shallow commits into "ours" and "theirs" / Step 2, clean "ours" based on
    /// .git/shallow": a commit this repository has goes to `ours` unless its own
    /// `.git/shallow` already lists it, one it lacks to `theirs`. Then
    /// `if (!si.nr_ours && !si.nr_theirs) shallow_update = 0;` (receive-pack.c:2579-2580).
    fn prepare(repo: &gix::Repository, shallow: Vec<gix::ObjectId>, shallow_update: bool) -> Self {
        let mut si = ShallowInfo::default();
        if shallow.is_empty() {
            return si;
        }
        si.grafts = (**repo.commit_grafts()).clone();
        si.shallow_stat = shallow_stat(&repo.common_dir().join("shallow"));
        for (i, id) in shallow.iter().enumerate() {
            if repo.has_object(id) {
                if matches!(si.grafts.get(id), Some(gix::revwalk::graft::Graft::Shallow)) {
                    continue;
                }
                si.ours.push(i);
            } else {
                si.theirs.push(i);
            }
        }
        si.shallow = shallow;
        si.update = shallow_update && !(si.ours.is_empty() && si.theirs.is_empty());
        si
    }

    /// `si.nr_ours || si.nr_theirs`.
    fn has_any(&self) -> bool {
        !(self.ours.is_empty() && self.theirs.is_empty())
    }

    /// `shallow_update && si->shallow_ref[cmd->index]`: the command is left to
    /// `update_shallow_ref()`, which checks its connectivity itself.
    ///
    /// `si->shallow_ref` is only allocated by `prepare_shallow_update()`, which a
    /// push of deletes alone never reaches (`if (!delete_only(commands))`, :2581),
    /// while `shallow_update` stays set when the client listed shallow commits. The
    /// read then goes through a NULL pointer and receive-pack dies of SIGSEGV —
    /// before `pre-receive` when the connectivity check runs, after the refs moved
    /// when `--skip-connectivity-check` leaves it to
    /// `BUG_if_skipped_connectivity_check()`.
    fn deferred(&self, cmd: &Command) -> bool {
        if !self.update {
            return false;
        }
        if !self.shallow_ref_allocated {
            // The Rust runtime's own SIGSEGV handler (stack-overflow detection)
            // would swallow a raised signal, so the default action is restored first.
            unsafe {
                libc::signal(libc::SIGSEGV, libc::SIG_DFL);
                libc::raise(libc::SIGSEGV);
            }
        }
        self.shallow_ref[cmd.shallow_index] != 0
    }

    /// Record that the list now ends after command `i` when `update_shallow_ref()`
    /// cut it while `update()` ran for that command.
    fn note_cut(&mut self, i: usize) {
        if std::mem::take(&mut self.cut) {
            self.cut_at = Some(i + 1);
        }
    }

    /// `die()` with the tempfile cleanup git's exit handlers do.
    fn die(&mut self, msg: &str) -> ! {
        eprintln!("fatal: {msg}");
        if let Some(path) = self.alt_shallow_file.take() {
            let _ = std::fs::remove_file(path);
        }
        std::process::exit(128);
    }

    /// The parents `repo_parse_commit()` gives `id`, with the graft table
    /// applied (`parse_commit_buffer()`, commit.c:554-590); `None` for a missing
    /// object or one that is not a commit.
    fn parents(&self, repo: &gix::Repository, find: FindObject<'_>, id: gix::ObjectId) -> Option<Vec<gix::ObjectId>> {
        let (kind, data) = find(id)?;
        if kind != gix::object::Kind::Commit {
            return None;
        }
        let commit = gix::objs::CommitRef::from_bytes(&data, repo.object_hash()).ok()?;
        Some(match self.grafts.get(&id) {
            Some(graft) => graft.parents().to_vec(),
            None => commit.parents().collect(),
        })
    }

    /// `lookup_commit_reference_gently()`: `id` with tags peeled, if that is a commit.
    fn peel_to_commit(find: FindObject<'_>, mut id: gix::ObjectId) -> Option<gix::ObjectId> {
        loop {
            let (kind, data) = find(id)?;
            match kind {
                gix::object::Kind::Commit => return Some(id),
                gix::object::Kind::Tag => {
                    id = gix::objs::TagRefIter::from_bytes(&data, id.kind()).target_id().ok()?;
                }
                _ => return None,
            }
        }
    }

    /// The commits `refs_head_ref()` and `refs_for_each_ref()` hand `add_ref()` and
    /// `mark_uninteresting()`: `HEAD` first, then every ref of the whole store
    /// (no namespace), symrefs resolved and tags peeled, broken refs skipped.
    fn ref_commits(repo: &gix::Repository, find: FindObject<'_>) -> Vec<gix::ObjectId> {
        let mut raw = repo.clone();
        raw.clear_namespace();
        let mut tips = Vec::new();
        if let Ok(Some(mut head)) = raw.try_find_reference("HEAD") {
            if let Ok(id) = head.follow_to_object() {
                tips.push(id.detach());
            }
        }
        if let Ok(platform) = raw.references() {
            if let Ok(all) = platform.all() {
                for reference in all.flatten() {
                    let mut reference = reference;
                    if let Ok(id) = reference.follow_to_object() {
                        tips.push(id.detach());
                    }
                }
            }
        }
        tips.into_iter().filter_map(|id| Self::peel_to_commit(find, id)).collect()
    }

    /// `repo_in_merge_bases_many(r, commit, n, reference, 1)` (commit-reach.c:682-719):
    /// whether `commit` is reachable from any of `references`, parents read with
    /// the graft table and a missing commit ignored.
    fn in_merge_bases_many(
        &self,
        repo: &gix::Repository,
        find: FindObject<'_>,
        commit: gix::ObjectId,
        references: &[gix::ObjectId],
    ) -> bool {
        if self.parents(repo, find, commit).is_none() {
            return false;
        }
        let mut seen: HashSet<gix::ObjectId> = HashSet::new();
        let mut stack: Vec<gix::ObjectId> = references.to_vec();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if id == commit {
                return true;
            }
            if let Some(parents) = self.parents(repo, find, id) {
                stack.extend(parents.into_iter().filter(|p| !seen.contains(p)));
            }
        }
        false
    }

    /// `setup_temporary_shallow(si.shallow)` (shallow.c:414-436): every shallow
    /// graft of this repository, then every commit the client listed, in a
    /// `shallow_XXXXXX` tempfile in the git directory, whose path the unpack
    /// child and (under `receive.shallowUpdate`) every later child is handed.
    fn setup_temporary_shallow(&mut self, repo: &gix::Repository) -> String {
        let mut sb = String::new();
        for (id, graft) in self.grafts.iter() {
            if matches!(graft, gix::revwalk::graft::Graft::Shallow) {
                sb.push_str(&format!("{id}\n"));
            }
        }
        for id in &self.shallow {
            sb.push_str(&format!("{id}\n"));
        }
        if sb.is_empty() {
            // "is_repository_shallow() sees empty string as "no shallow file"."
            return String::new();
        }
        let git_dir = std::fs::canonicalize(repo.git_dir()).unwrap_or_else(|_| crate::hooks::absolutize(repo.git_dir()));
        let template = git_dir.join("shallow_XXXXXX");
        match crate::tmp_objdir::git_mkstemp_mode(template.as_os_str(), 0o600) {
            Ok((path, mut file)) => {
                self.alt_shallow_file = Some(path.clone());
                if let Err(e) = file.write_all(sb.as_bytes()) {
                    self.die(&format!("failed to write to {}: {}", path.display(), crate::external::strerror(&e)));
                }
                path.to_string_lossy().into_owned()
            }
            Err(e) => self.die(&format!(
                "Unable to create temporary file '{}': {}",
                template.display(),
                crate::external::strerror(&e)
            )),
        }
    }

    /// `update_shallow_info()` (builtin/receive-pack.c:2373-2409), run once the
    /// pack is in: "Step 4, remove non-existent ones in "theirs" after getting the
    /// pack" (`remove_nonexistent_theirs_shallow()`, shallow.c:564-577), number the
    /// commands that are not deletes, then either prepare the per-ref shallow
    /// updates or, without `receive.shallowUpdate`, refuse every ref that needs
    /// a shallow commit with `shallow update not allowed`.
    fn update_shallow_info(
        &mut self,
        repo: &gix::Repository,
        find: FindObject<'_>,
        cmds: &mut [Command],
        zero: gix::ObjectId,
        shallow_file: &str,
    ) {
        let shallow = &self.shallow;
        self.theirs.retain(|&i| find(shallow[i]).is_some());
        if !self.has_any() {
            self.update = false;
            return;
        }
        for cmd in cmds.iter_mut() {
            if cmd.new == zero {
                continue;
            }
            self.refs.push(cmd.new);
            cmd.shallow_index = self.refs.len() - 1;
        }
        if self.update {
            self.prepare_shallow_update(repo, find, shallow_file);
            return;
        }
        let ref_status = self.assign_shallow_commits_to_refs(repo, find, false);
        for cmd in cmds.iter_mut() {
            if cmd.new == zero {
                continue;
            }
            if ref_status[cmd.shallow_index] != 0 {
                cmd.error = Some("shallow update not allowed".into());
                cmd.skip_update = true;
            }
        }
    }

    /// `prepare_shallow_update()` (builtin/receive-pack.c:2329-2371).
    fn prepare_shallow_update(&mut self, repo: &gix::Repository, find: FindObject<'_>, shallow_file: &str) {
        self.assign_shallow_commits_to_refs(repo, find, true);
        let n = self.shallow.len();
        let mut need = vec![0u32; n];
        self.reachable = vec![false; n];
        self.shallow_ref = vec![0; self.refs.len()];
        self.shallow_ref_allocated = true;
        for &i in &self.ours {
            need[i] = 1;
        }
        for i in 0..n {
            let Some(used) = &self.used_shallow[i] else { continue };
            for (j, &word) in used.iter().enumerate() {
                if word == 0 {
                    continue;
                }
                need[i] += 1;
                for k in 0..32 {
                    if word & (1 << k) != 0 {
                        self.shallow_ref[j * 32 + k] += 1;
                    }
                }
            }
            // "true for those associated with some refs and belong in "ours" list
            // aka "step 7 not done yet""
            need[i] = u32::from(need[i] > 1);
        }
        self.need_reachability_test = need.into_iter().map(|v| v != 0).collect();
        // "keep hooks happy by forcing a temporary shallow file via env variable
        // because we can't add --shallow-file to every command. check_connected()
        // will be done with true .git/shallow though."
        std::env::set_var("GIT_SHALLOW_FILE", shallow_file);
    }

    /// `assign_shallow_commits_to_refs()` (shallow.c:711-788), "Step 6(+7),
    /// associate shallow commits with new refs": every ref's new value is painted
    /// down to the shallow commits (`paint_down()`, :616-681), stopping at what the
    /// existing refs already reach and at the shallow commits themselves. With
    /// `used` the bitmaps land in `used_shallow`; without it `ours` and `theirs` are
    /// narrowed (`post_assign_shallow()`) and the per-ref status is returned.
    fn assign_shallow_commits_to_refs(&mut self, repo: &gix::Repository, find: FindObject<'_>, used: bool) -> Vec<u32> {
        let candidates: Vec<usize> = self.ours.iter().chain(self.theirs.iter()).copied().collect();
        let words = self.refs.len().div_ceil(32);

        // "--not --all" to cut short the traversal if new refs connect to old
        // refs: `mark_uninteresting()` parses each ref's commit and marks it and
        // its parents UNINTERESTING (`mark_parents_uninteresting()` goes no deeper
        // than what is parsed, and only the ref commits are).
        let mut uninteresting: HashSet<gix::ObjectId> = HashSet::new();
        for tip in Self::ref_commits(repo, find) {
            uninteresting.insert(tip);
            if let Some(parents) = self.parents(repo, find, tip) {
                uninteresting.extend(parents);
            }
        }
        // "Mark potential bottoms so we won't go out of bound"
        let bottom: HashSet<gix::ObjectId> = candidates.iter().map(|&i| self.shallow[i]).collect();

        let mut painted: std::collections::HashMap<gix::ObjectId, Vec<u32>> = std::collections::HashMap::new();
        for (id, tip) in self.refs.clone().into_iter().enumerate() {
            let Some(start) = Self::peel_to_commit(find, tip) else { continue };
            let mut seen: HashSet<gix::ObjectId> = HashSet::new();
            let mut head = vec![start];
            while let Some(c) = head.pop() {
                if seen.contains(&c) || uninteresting.contains(&c) {
                    continue;
                }
                seen.insert(c);
                painted.entry(c).or_insert_with(|| vec![0; words])[id / 32] |= 1 << (id % 32);
                if bottom.contains(&c) {
                    continue;
                }
                let Some(parents) = self.parents(repo, find, c) else {
                    self.die(&format!("unable to parse commit {c}"));
                };
                head.extend(parents.into_iter().filter(|p| !seen.contains(p)));
            }
        }

        if used {
            self.used_shallow = vec![None; self.shallow.len()];
            for &i in &candidates {
                if let Some(map) = painted.get(&self.shallow[i]) {
                    self.used_shallow[i] = Some(map.clone());
                }
            }
            return Vec::new();
        }
        self.post_assign_shallow(repo, find, &painted)
    }

    /// `post_assign_shallow()` (shallow.c:812-872), "Step 7, reachability test on
    /// "ours" at commit level": a commit of `theirs` stays when some ref reaches it,
    /// one of `ours` when some ref reaches it and no existing ref does; every ref
    /// that reaches one that stays gets a non-zero status.
    fn post_assign_shallow(
        &mut self,
        repo: &gix::Repository,
        find: FindObject<'_>,
        painted: &std::collections::HashMap<gix::ObjectId, Vec<u32>>,
    ) -> Vec<u32> {
        let mut ref_status = vec![0u32; self.refs.len()];
        let mut update_refstatus = |bitmap: &[u32]| {
            for (i, status) in ref_status.iter_mut().enumerate() {
                if bitmap[i / 32] & (1 << (i % 32)) != 0 {
                    *status += 1;
                }
            }
        };
        let reached = |id: &gix::ObjectId| painted.get(id).filter(|map| map.iter().any(|&w| w != 0));

        let theirs = std::mem::take(&mut self.theirs);
        for i in theirs {
            if let Some(bitmap) = reached(&self.shallow[i]) {
                update_refstatus(bitmap);
                self.theirs.push(i);
            }
        }

        let cs = Self::ref_commits(repo, find);
        let ours = std::mem::take(&mut self.ours);
        for i in ours {
            let c = self.shallow[i];
            let Some(bitmap) = reached(&c) else { continue };
            if !self.in_merge_bases_many(repo, find, c, &cs) {
                update_refstatus(bitmap);
                self.ours.push(i);
            }
        }
        ref_status
    }

    /// `delayed_reachability_test()` (shallow.c:874-892), "(Delayed) step 7,
    /// reachability test at commit level".
    fn delayed_reachability_test(&mut self, repo: &gix::Repository, c: usize) -> bool {
        if self.need_reachability_test[c] {
            let find = find_in_repo(repo);
            if self.commits.is_none() {
                self.commits = Some(Self::ref_commits(repo, &find));
            }
            let commits = self.commits.clone().unwrap_or_default();
            self.reachable[c] = self.in_merge_bases_many(repo, &find, self.shallow[c], &commits);
            self.need_reachability_test[c] = false;
        }
        self.reachable[c]
    }

    /// `update_shallow_ref()` (builtin/receive-pack.c:1332-1366): the shallow
    /// commits this ref needs and no existing ref reaches are added to
    /// `.git/shallow` — written to `shallow.lock` with the current list, a tip
    /// checked for connectivity against it, then committed, or rolled back when
    /// the check fails. Returns whether the ref may be written.
    ///
    /// The tip is not this command's: `check_connected(command_singleton_iterator,
    /// cmd, &opt)` (:1349) hands the iterator `cmd` where it expects a `struct command **`
    /// (:1762-1771), so it reads the command's first member, `next`, as the list.
    /// The check is therefore of the *next* command's new value, and it stores
    /// NULL through `next`, cutting every later command out of the list — they are
    /// never run, reported or handed to the post hooks. When there is no next
    /// command, or it is a delete, nothing is checked and the list stays whole.
    fn update_shallow_ref(
        &mut self,
        repo: &gix::Repository,
        cmd: &Command,
        next: Option<&Command>,
        zero: gix::ObjectId,
    ) -> bool {
        let index = cmd.shallow_index;
        let mask = 1u32 << (index % 32);
        let mut extra = Vec::new();
        for i in 0..self.shallow.len() {
            let uses = self.used_shallow[i].as_ref().is_some_and(|map| map[index / 32] & mask != 0);
            if uses && !self.delayed_reachability_test(repo, i) {
                extra.push(self.shallow[i]);
            }
        }

        // `setup_alternate_shallow()` (shallow.c:438-461).
        let shallow_path = repo.common_dir().join("shallow");
        let lock_path = repo.common_dir().join("shallow.lock");
        let mut lock = match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock_path) {
            Ok(file) => file,
            Err(e) => self.die(&gix::lock::pid::unable_to_lock_message(&shallow_path, &e)),
        };
        // `check_shallow_file_for_update()` (shallow.c:336-344).
        if shallow_stat(&shallow_path) != self.shallow_stat {
            let _ = std::fs::remove_file(&lock_path);
            self.die("shallow file has changed since we read it");
        }
        let mut sb = String::new();
        for (id, graft) in self.grafts.iter() {
            if matches!(graft, gix::revwalk::graft::Graft::Shallow) {
                sb.push_str(&format!("{id}\n"));
            }
        }
        for id in &extra {
            sb.push_str(&format!("{id}\n"));
        }
        let shallow_file = if sb.is_empty() {
            String::new()
        } else {
            if let Err(e) = lock.write_all(sb.as_bytes()) {
                let _ = std::fs::remove_file(&lock_path);
                self.die(&format!("failed to write to {}: {}", lock_path.display(), crate::external::strerror(&e)));
            }
            lock_path.to_string_lossy().into_owned()
        };
        drop(lock);

        let tips: Vec<gix::ObjectId> = match next {
            Some(next) if next.new != zero => {
                self.cut = true;
                vec![next.new]
            }
            _ => Vec::new(),
        };
        if check_connected(repo, &tips, &[], false, None, false, Some(&shallow_file)) {
            // `rollback_shallow_file()`.
            let _ = std::fs::remove_file(&lock_path);
            return false;
        }

        // `commit_shallow_file()`, whose `is_repository_shallow()` re-reads the
        // file and its stat data; "Make sure setup_alternate_shallow() for the
        // next ref does not lose these new roots.."
        let _ = std::fs::rename(&lock_path, &shallow_path);
        self.shallow_stat = shallow_stat(&shallow_path);
        for id in extra {
            self.grafts.register(id, gix::revwalk::graft::Graft::Shallow, false);
        }
        self.shallow_ref[index] = 0;
        true
    }
}

/// `check_aliased_updates()` (builtin/receive-pack.c:1742-1761): a command whose
/// ref is a symref to the ref another command of the same push writes is not run
/// on its own. When the two agree on both values the symref's command is only
/// skipped (and reported `ok`); when they disagree both are refused.
fn check_aliased_updates(repo: &gix::Repository, cmds: &mut [Command], band: &mut Band) {
    for i in 0..cmds.len() {
        if cmds[i].error.is_some() {
            continue;
        }
        // `check_aliased_update()` (:1729-1740): resolve
        // `get_git_namespace() + cmd->ref_name` with no `RESOLVE_REF_READING`.
        let (dst_name, is_symref) = resolve_ref_unsafe(repo, &cmds[i].name);
        // `check_aliased_update_internal()` (:1685-1727).
        if !is_symref {
            continue;
        }
        let Some(dst_name) = dst_name else {
            band.error(&format!("refusing update to broken symref '{}'", cmds[i].name));
            cmds[i].skip_update = true;
            cmds[i].error = Some("broken symref".into());
            continue;
        };
        // `dst_name = strip_namespace(dst_name)`. A target outside the namespace
        // strips to NULL, which no command can be named; it is left alone.
        let prefix = repo.namespace().map(|ns| ns.as_bstr().to_string()).unwrap_or_default();
        let Some(dst_name) = dst_name.strip_prefix(prefix.as_str()) else {
            continue;
        };
        // `string_list_lookup(list, dst_name)` over every command's ref name.
        let Some(j) = cmds.iter().position(|c| c.name == dst_name) else {
            continue;
        };
        cmds[i].skip_update = true;
        if cmds[i].old == cmds[j].old && cmds[i].new == cmds[j].new {
            continue;
        }
        cmds[j].skip_update = true;
        let abbrev = |id: &gix::ObjectId| crate::abbrev::default_unique_abbrev(repo, id);
        band.error(&format!(
            "refusing inconsistent update between symref '{}' ({}..{}) and its target '{}' ({}..{})",
            cmds[i].name,
            abbrev(&cmds[i].old),
            abbrev(&cmds[i].new),
            cmds[j].name,
            abbrev(&cmds[j].old),
            abbrev(&cmds[j].new),
        ));
        cmds[i].error = Some("inconsistent aliased update".into());
        cmds[j].error = Some("inconsistent aliased update".into());
    }
}

/// `SYMREF_MAXDEPTH` (refs.h): how many symrefs a resolution follows.
const SYMREF_MAXDEPTH: usize = 5;

/// `refs_resolve_ref_unsafe(refs, get_git_namespace() + name, 0, NULL, &flag)`
/// (refs.c:2113-2194): the full name the chain of symrefs
/// starting at `name` ends on, and whether there was a symref in it
/// (`REF_ISSYMREF`). Without `RESOLVE_REF_READING` a missing ref ends the chain
/// rather than failing it; a malformed name, an unreadable ref or a chain longer
/// than [`SYMREF_MAXDEPTH`] yields `None`.
fn resolve_ref_unsafe(repo: &gix::Repository, name: &str) -> (Option<String>, bool) {
    let prefix = repo.namespace().map(|ns| ns.as_bstr().to_string()).unwrap_or_default();
    // The chain is read raw: a symref's target is a full on-disk name, never
    // namespaced again.
    let mut raw = repo.clone();
    raw.clear_namespace();
    let mut refname = format!("{prefix}{name}");
    let mut is_symref = false;
    let mut resolved = None;
    for _ in 0..SYMREF_MAXDEPTH {
        if !super::check_ref_format::check_refname_format(
            refname.as_bytes(),
            super::check_ref_format::ALLOW_ONELEVEL,
        ) {
            break;
        }
        match raw.try_find_reference(refname.as_str()) {
            Ok(Some(found)) if found.name().as_bstr() == refname.as_str() => match found.target() {
                gix::refs::TargetRef::Symbolic(target) => {
                    is_symref = true;
                    refname = target.as_bstr().to_string();
                }
                gix::refs::TargetRef::Object(_) => {
                    resolved = Some(refname);
                    break;
                }
            },
            // A missing ref is OK.
            Ok(_) => {
                resolved = Some(refname);
                break;
            }
            // `parse_loose_ref_contents()` takes `ref: <anything>` as a symref
            // before the loop's `check_refname_format()` refuses its target
            // (refs/files-backend.c, refs.c:2178-2185); gix refuses the file
            // whole, so the `ref:` it holds is looked for here.
            Err(_) => {
                is_symref |= loose_ref_is_symref(&raw, &refname);
                break;
            }
        }
    }
    (resolved, is_symref)
}

/// Whether the loose file for `refname` starts with `ref:`, the files backend's
/// symref marker. Per-worktree names live in the git directory, the rest in the
/// common one (`files_ref_path()`).
fn loose_ref_is_symref(repo: &gix::Repository, refname: &str) -> bool {
    let per_worktree = !refname.contains('/')
        || ["refs/bisect/", "refs/worktree/", "refs/rewritten/"]
            .iter()
            .any(|p| refname.starts_with(p));
    let base = if per_worktree { repo.git_dir() } else { repo.common_dir() };
    std::fs::read(base.join(refname)).is_ok_and(|content| content.starts_with(b"ref:"))
}

/// The largest payload one side-band packet can carry: 65520 minus the 4-byte
/// pkt-line header and the 1-byte band number (`LARGE_PACKET_DATA_MAX`).
const MAX_SIDEBAND_PAYLOAD: usize = 65515;

/// Where receive-pack's diagnostics go. With `side-band-64k` the client
/// multiplexes them back out with a `remote: ` prefix; without it they land on
/// receive-pack's own stderr, which for a local push is the user's terminal.
struct Band {
    sideband: bool,
    /// `receive.keepAlive`, carried here because the hook relay is the only
    /// place that can go quiet long enough for the client to give up.
    keep_alive: i64,
}

impl Band {
    /// `rp_error()`: one `error: <msg>` line back to the pusher.
    fn error(&mut self, msg: &str) {
        self.write(&format!("error: {msg}\n"));
    }

    /// `rp_warning()`: one `warning: <msg>` line back to the pusher.
    fn warning(&mut self, msg: &str) {
        self.write(&format!("warning: {msg}\n"));
    }

    /// A `die()` from the child git would have run: `fatal: <msg>`, with no
    /// `error:` prefix in front of it.
    fn fatal(&mut self, msg: &str) {
        self.write(&format!("fatal: {msg}\n"));
    }

    /// A block of advice, already newline-terminated, sent verbatim.
    fn write(&mut self, text: &str) {
        self.write_bytes(text.as_bytes());
    }

    /// [`Band::write`] for output that need not be UTF-8 — a hook's, say.
    fn write_bytes(&mut self, text: &[u8]) {
        if text.is_empty() {
            return;
        }
        if !self.sideband {
            let mut stderr = std::io::stderr().lock();
            let _ = stderr.write_all(text);
            let _ = stderr.flush();
            return;
        }
        let mut out = Vec::new();
        for chunk in text.chunks(MAX_SIDEBAND_PAYLOAD) {
            let mut payload = vec![2u8];
            payload.extend_from_slice(chunk);
            frame(&mut out, &payload);
        }
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
    }

    /// `copy_to_sideband()`'s idle tick: an empty band-1 packet, sent when a hook
    /// has produced nothing for `receive.keepAlive` seconds so the client's read
    /// does not time out. Only meaningful on a multiplexed stream.
    fn keep_alive_tick(&mut self) {
        if !self.sideband {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(b"0005\x01");
        let _ = stdout.flush();
    }
}

/// `receive-pack.c`'s `head_name`: the branch HEAD points at, or `None` when
/// HEAD is detached or unborn beyond resolution.
fn head_name(repo: &gix::Repository) -> Option<String> {
    let head = repo.head().ok()?;
    match head.referent_name() {
        Some(name) => Some(name.as_bstr().to_string()),
        None => None,
    }
}

/// `cmd_receive_pack()`'s trailing `update_server_info(0)`. The port of
/// `git update-server-info` works on the repository it discovers from the
/// current directory, which is exactly the directory git's `enter_repo()` has
/// already chdir'd into by this point; this reproduces that move for the one
/// call that needs it, after every ref has been written.
fn update_server_info(repo: &gix::Repository) {
    let git_dir = repo.path().to_path_buf();
    if std::env::set_current_dir(&git_dir).is_err() {
        return;
    }
    let _ = super::update_server_info::update_server_info(&[]);
}

/// Read one pkt-line: `None` on a flush (`0000`), else its payload (header
/// stripped). A missing/short header or a non-hex length is a protocol error.
fn read_pkt_line(r: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut hdr = [0u8; 4];
    read_exact(r, &mut hdr).map_err(|_| anyhow!("the remote end hung up unexpectedly"))?;
    let len = u16::from_str_radix(
        std::str::from_utf8(&hdr).map_err(|_| anyhow!("protocol error: bad line length character"))?,
        16,
    )
    .map_err(|_| anyhow!("protocol error: bad line length character"))?;
    // `packet_read_with_status()` traces what it read (pkt-line.c:434-445, :502-505).
    match len {
        0 => {
            crate::trace::packet_trace(b"0000", false);
            Ok(None)
        }
        1..=4 => {
            // delim/response-end or empty line
            match len {
                1 => crate::trace::packet_trace(b"0001", false),
                2 => crate::trace::packet_trace(b"0002", false),
                4 => crate::trace::packet_trace(b"", false),
                _ => {}
            }
            Ok(Some(Vec::new()))
        }
        _ => {
            let mut buf = vec![0u8; len as usize - 4];
            read_exact(r, &mut buf)?;
            crate::trace::packet_trace(&buf, false);
            Ok(Some(buf))
        }
    }
}

fn read_exact(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        match r.read(&mut buf[off..])? {
            0 => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof")),
            n => off += n,
        }
    }
    Ok(())
}

/// Whether the client advertised capability `want` (a whole space-separated token).
fn cap_present(caps: &[u8], want: &[u8]) -> bool {
    caps.split(|&b| b == b' ' || b == b'\n' || b == 0).any(|tok| tok == want)
}

/// `struct odb_transaction_files` as `ODB_TRANSACTION_RECEIVE` sets it up
/// (object-file.c:485-497, :1483-1517): the quarantine every pushed object is
/// written into, and the `.keep` files `index-pack` left there.
struct Transaction {
    /// `transaction->objdir`, `None` once migrated.
    quarantine: Option<crate::tmp_objdir::Quarantine>,
    /// `transaction->pack_lockfiles`: each `.keep` by the name it has once
    /// migrated (object-file.c:1426-1440).
    pack_lockfiles: Vec<std::path::PathBuf>,
}

impl Transaction {
    /// `odb_transaction_begin(..., ODB_TRANSACTION_RECEIVE)`, which creates the
    /// quarantine eagerly (`odb_transaction_files_prepare()`, object-file.c:500-520).
    fn begin(repo: &gix::Repository) -> Option<Transaction> {
        match crate::tmp_objdir::Quarantine::create(&object_directory(repo), "incoming") {
            Ok(quarantine) => Some(Transaction {
                quarantine: Some(quarantine),
                pack_lockfiles: Vec::new(),
            }),
            Err(_) => {
                eprintln!("error: unable to create temporary object directory");
                None
            }
        }
    }

    /// The quarantine's path. Only called while one is open: between `begin()` and
    /// `commit()` nothing else takes it.
    fn path(&self) -> &std::path::Path {
        self.quarantine.as_ref().expect("the pack is ingested before the commit").path()
    }

    /// `odb_transaction_files_commit()` (object-file.c:1259-1296): migrate the
    /// quarantine into the real store. The hardware flush git issues first has no
    /// observable effect and is not reproduced.
    fn commit(&mut self, repo: &gix::Repository) -> bool {
        let Some(quarantine) = self.quarantine.take() else {
            return true;
        };
        let shared = repo
            .config_snapshot()
            .string("core.sharedRepository")
            .and_then(|v| super::init::parse_shared_value(&v.to_string()).ok())
            .unwrap_or(0);
        let adjust = |path: &std::path::Path| {
            shared == 0 || super::init::adjust_shared_perm_recursive(path, shared).is_ok()
        };
        if !quarantine.migrate(&adjust) {
            eprintln!("error: unable to migrate temporary objects");
            return false;
        }
        true
    }

    /// `odb_transaction_files_finalize()` (object-file.c:1452-1464): delete the
    /// `.keep` files from where the migrate put them.
    fn finalize(&mut self) {
        for keep in self.pack_lockfiles.drain(..) {
            let _ = std::fs::remove_file(keep);
        }
    }
}

/// `absolute_path(repo_get_object_directory(r))` inside `receive-pack`, where
/// `enter_repo()` has `chdir()`ed into the git directory and set it to `.`
/// (setup.c:1868-1893), so the object directory is `./objects` and its absolute
/// spelling `<git-dir>/./objects`, with the git directory as `getcwd()` reports it.
/// A store kept anywhere else is spelled as itself.
fn object_directory(repo: &gix::Repository) -> std::path::PathBuf {
    let objects = crate::hooks::absolutize(repo.objects.store_ref().path());
    match std::fs::canonicalize(repo.git_dir()) {
        Ok(git_dir) if std::fs::canonicalize(&objects).ok() == Some(git_dir.join("objects")) => {
            git_dir.join("./objects")
        }
        _ => objects,
    }
}

/// The objects the push brought in as the child that took them sees them: the
/// quarantine, its `GIT_OBJECT_DIRECTORY`, with the real store behind it as
/// `GIT_ALTERNATE_OBJECT_DIRECTORIES` (tmp-objdir.c:174-180).
struct Incoming<'a> {
    quarantine: gix::odb::Handle,
    repo: &'a gix::Repository,
}

impl<'a> Incoming<'a> {
    fn open(repo: &'a gix::Repository, path: &std::path::Path) -> std::io::Result<Self> {
        let quarantine = gix::odb::at_opts(
            path,
            Vec::new(),
            gix::odb::store::init::Options {
                object_hash: repo.object_hash(),
                loose_compression: repo.loose_compression(),
                ..Default::default()
            },
        )?;
        Ok(Incoming { quarantine, repo })
    }

    /// The object's type and contents, from the quarantine or the real store.
    fn find(&self, id: gix::ObjectId) -> Option<(gix::object::Kind, Vec<u8>)> {
        use gix::objs::Find;
        let mut buf = Vec::new();
        if let Ok(Some(data)) = self.quarantine.try_find(&id, &mut buf) {
            return Some((data.kind, data.data.to_vec()));
        }
        let object = self.repo.find_object(id).ok()?;
        Some((object.kind, object.data.clone()))
    }
}

/// Index the packfile streaming off `input`, then either explode it into loose
/// objects or keep it — `receive-pack.c::unpack()`'s `unpack-objects` versus
/// `index-pack` choice, decided by the object count in the pack header against
/// `receive.unpackLimit`. The delta bases a thin pack lacks, and only those the
/// pack cannot supply itself, are appended to it from the odb (git's
/// `index-pack --fix-thin`), so a `send-pack` thin pack lands complete.
///
/// On failure the diagnostics have already gone back to the pusher and what is
/// returned is the `unpack <status>` line the client reports: either one of
/// `parse_pack_header()`'s own complaints, or `<child> abnormal exit` naming
/// whichever child git would have run.
fn ingest_pack(
    repo: &gix::Repository,
    transaction: &mut Transaction,
    input: &mut impl BufRead,
    config: &Config,
    band: &mut Band,
    quiet: bool,
) -> Result<(), String> {
    // `unpack()` reads the 12-byte header first to learn the object count, and
    // that count alone decides which child runs. A short read never reaches a
    // child, so it has a status of its own rather than an abnormal exit.
    let mut header = [0u8; 12];
    if read_exact(input, &mut header).is_err() {
        return Err("eof before pack header was fully read".into());
    }
    // `read_pack_header()` (`object-file.c`) vets the signature and the version before the count is looked at,
    // and `parse_pack_header()` turns each verdict into the `unpack <status>` line below. Neither ever reaches a
    // child, which is what makes them the answer when a client sends something that is not a pack at all — a
    // `push-options` section against a server that never advertised the capability lands here.
    if &header[..4] != b"PACK" {
        return Err("protocol error (pack signature mismatch detected)".into());
    }
    let version = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    if version != 2 && version != 3 {
        return Err("protocol error (pack version unsupported)".into());
    }
    let nr_objects = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    let to_loose = (nr_objects as u64) < config.unpack_limit;
    let child = format!(
        "{} abnormal exit",
        if to_loose { "unpack-objects" } else { "index-pack" }
    );

    // `--strict=<types>` is only handed to the child when the transfer check is
    // on, so the configuration errors it would die on are only reachable then.
    if config.fsck_objects {
        if let Some(text) = &config.fsck_msgs.deferred_fatal {
            band.fatal(text);
            return Err(child);
        }
    }

    // `receive.maxInputSize` is enforced against the bytes read off the wire,
    // header included, and aborts the child the moment it is exceeded.
    let counted = Counted {
        inner: std::io::Cursor::new(header.to_vec()).chain(input),
        read: 0,
        limit: config.max_input_size,
    };
    let mut counted = std::io::BufReader::new(counted);

    // Both children write into the quarantine: `odb_transaction_env()` hands them its
    // `GIT_OBJECT_DIRECTORY` (object-file.c:1365), whose `pack` directory
    // `setup_tmp_objdir()` made.
    let pack_dir = transaction.path().join("pack");
    // ```c
    // if (!opts->quiet && err_fd)
    //         strvec_push(&child.args, "--show-resolving-progress");
    // ```
    //
    // (object-file.c:1401-1402, the `index-pack` branch only.) `err_fd` is the
    // `copy_to_sideband()` muxer, set up only when the client asked for a sideband
    // (builtin/receive-pack.c:2312-2324), so the meter reaches the pusher on band 2.
    // `unpack-objects` gets no such flag, and with a pipe for a stderr its own
    // `quiet = !isatty(2)` (builtin/unpack-objects.c:630) keeps it silent.
    let mut progress: Box<dyn prodash::DynNestedProgress> = if !quiet && band.sideband && !to_loose {
        let keep_alive = band.keep_alive;
        Box::new(super::fetch_progress::Meters::resolving_only(std::sync::Arc::new(move |bytes: &[u8]| {
            Band { sideband: true, keep_alive }.write_bytes(bytes)
        })))
    } else {
        Box::new(gix::progress::Discard)
    };
    let outcome = gix::odb::pack::Bundle::write_to_directory(
        &mut counted,
        Some(&pack_dir),
        &mut *progress,
        &AtomicBool::new(false),
        Some(repo.objects.clone()),
        gix::odb::pack::bundle::write::Options {
            object_hash: repo.object_hash(),
            ..Default::default()
        },
    );
    drop(progress);
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => {
            if counted.get_ref().over_limit() {
                band.fatal("pack exceeds maximum allowed size");
            } else {
                band.fatal(&e.to_string());
            }
            return Err(child);
        }
    };
    // The quarantine starts empty, so the writer always moved a pack of its own
    // into place, and dropped a `.keep` beside it.
    let (index_path, data_path) = match (&outcome.index_path, &outcome.data_path) {
        (Some(index), Some(data)) => (index.clone(), data.clone()),
        _ => return Ok(()),
    };
    // A child that dies leaves no object behind: both children write into the
    // transaction's quarantine, which is discarded unless a ref is about to move
    // (object-file.c:1366-1445, tmp-objdir.c:54-77).
    let incoming = match Incoming::open(repo, transaction.path()) {
        Ok(incoming) => incoming,
        Err(e) => {
            band.fatal(&e.to_string());
            return Err(child);
        }
    };

    // `--strict`: every object the push brought in is linted before any ref
    // moves, and the first error kills the whole push.
    let received = match pack_object_ids(&index_path, repo.object_hash()) {
        Ok(ids) => ids,
        Err(e) => {
            band.fatal(&e.to_string());
            return Err(child);
        }
    };
    if config.fsck_objects {
        if let Err(message) = fsck_received(repo, &incoming, &received, config, band, to_loose) {
            band.fatal(&message);
            return Err(child);
        }
    }

    // `unpack-objects` stores a small push loose and leaves no pack behind.
    if to_loose {
        if let Err(e) = explode_pack(&incoming, &received) {
            band.fatal(&e.to_string());
            return Err(child);
        }
        for path in [Some(&index_path), Some(&data_path), outcome.keep_path.as_ref()].into_iter().flatten() {
            let _ = std::fs::remove_file(path);
        }
        return Ok(());
    }
    // `index-pack --keep="receive-pack <pid> on <host>"` (object-file.c:1408-1416):
    // `write_special_file()` writes the message and a newline (builtin/index-pack.c:1558-1588)
    // into the file `odb_pack_keep()` created 0600. It is registered under the
    // name the migrate will give it, for `odb_transaction_finalize()` to delete.
    if let Some(keep_path) = &outcome.keep_path {
        use std::os::unix::fs::PermissionsExt;
        let msg = format!("receive-pack {} on {}\n", std::process::id(), super::send_email::hostname());
        let _ = std::fs::write(keep_path, msg);
        let _ = std::fs::set_permissions(keep_path, std::fs::Permissions::from_mode(0o600));
        if let Some(name) = keep_path.file_name() {
            let objects = repo.objects.store_ref().path();
            transaction.pack_lockfiles.push(objects.join("pack").join(name));
        }
    }
    // `index-pack` writes the reverse index unless `pack.writeReverseIndex` is
    // false (builtin/index-pack.c:1687-1692, 1922-1930, 2104-2108) and leaves
    // the files it renames into place read-only (`rename_tmp_packfile()`, :1590-1606).
    {
        let snapshot = repo.config_snapshot();
        if snapshot.boolean("pack.writeReverseIndex").unwrap_or(true) {
            let _ = super::index_pack::write_rev_index(&index_path, &outcome.index.data_hash, repo.object_hash());
        }
        let _ = super::index_pack::set_read_only(&index_path);
        let _ = super::index_pack::set_read_only(&data_path);
    }
    Ok(())
}

/// A reader that fails once `limit` bytes have gone past it — `receive-pack`'s
/// `max_input_size` guard, which counts the raw bytes of the pack stream.
struct Counted<R> {
    inner: R,
    read: u64,
    /// `0` is git's "no limit".
    limit: u64,
}

impl<R: Read> Counted<R> {
    /// Whether the limit is what stopped the read, as opposed to a malformed
    /// pack.
    fn over_limit(&self) -> bool {
        self.limit > 0 && self.read > self.limit
    }
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        if self.limit > 0 && self.read > self.limit {
            return Err(std::io::Error::other("pack exceeds maximum allowed size"));
        }
        Ok(n)
    }
}

/// Every object id an index file lists, in pack order.
///
/// The index itself is sorted by object id; `index-pack` and `unpack-objects`
/// both see the objects in the order the *pack* holds them, which is what
/// decides whether a `.gitmodules` blob is linted by the per-object pass or by
/// `fsck_finish()` — see [`fsck_received`]. So the entries are re-sorted on their
/// pack offset here rather than taken as the index lists them.
fn pack_object_ids(
    index_path: &std::path::Path,
    object_hash: gix::hash::Kind,
) -> Result<Vec<gix::ObjectId>> {
    let index = gix::odb::pack::index::File::at(index_path, object_hash)?;
    let mut entries: Vec<(u64, gix::ObjectId)> =
        index.iter().map(|e| (e.pack_offset, e.oid)).collect();
    entries.sort_unstable();
    Ok(entries.into_iter().map(|(_, oid)| oid).collect())
}

/// `fsck_objects`: run the object-content message layer over everything the
/// push delivered, at the severities `receive.fsck.<msg-id>` selects. The
/// message text is `index-pack`/`unpack-objects`' spelling, which names the
/// object rather than its type.
fn fsck_received(
    repo: &gix::Repository,
    incoming: &Incoming<'_>,
    received: &[gix::ObjectId],
    config: &Config,
    band: &mut Band,
    to_loose: bool,
) -> Result<(), String> {
    // Which of `unpack-objects`/`index-pack`'s two `die()`s the caller gets:
    // the per-object pass says `fsck error in packed object`, `fsck_finish()`
    // says `fsck error in pack objects`, and git dies at the first of the two it
    // reaches.
    let mut failed = false;
    // `fsck_options`' two oidsets, filled by every tree the pack carried and
    // drained by `fsck_finish()` below.
    let mut gitmodules: HashSet<gix::ObjectId> = HashSet::new();
    let mut gitattributes: HashSet<gix::ObjectId> = HashSet::new();
    // `gitmodules_done`/`gitattributes_done`: a blob the per-object pass already
    // linted, which `fsck_blobs()` skips (`fsck.c:1334`).
    let mut done: HashSet<gix::ObjectId> = HashSet::new();
    // `parse_pack_objects()`'s delay list: the blobs `unpack_entry_data()` could
    // not hand over, drained after the whole pack has been read.
    let mut delayed: Vec<gix::ObjectId> = Vec::new();
    // `core.bigFileThreshold`, which decides whether the per-object pass sees a
    // blob's contents at all — and so whether `gitmodulesLarge` can fire.
    let threshold = super::fsck::big_file_threshold(repo);
    /// One finding at its resolved severity, in `index-pack`/`unpack-objects`'
    /// spelling — which names the object rather than its type.
    fn report(
        config: &Config,
        band: &mut Band,
        finding: &Finding,
        id: &gix::ObjectId,
        failed: &mut bool,
    ) {
        match config.fsck_msgs.severity(finding, id) {
            Severity::Ignore => {}
            Severity::Info | Severity::Warn => {
                band.warning(&format!("object {id}: {}: {}", finding.msg.id, finding.text));
            }
            Severity::Error | Severity::Fatal => {
                band.error(&format!("object {id}: {}: {}", finding.msg.id, finding.text));
                *failed = true;
            }
        }
    }

    for id in received {
        let Some((kind, data)) = incoming.find(*id) else { continue };
        // `parse_pack_objects()` (`builtin/index-pack.c:1284`) hands every
        // non-delta object to `sha1_object()` as it comes off the wire, so a blob
        // some *earlier* object in the pack already named is linted right here
        // rather than in `fsck_finish()`.
        //
        // A blob over `core.bigFileThreshold` is the exception, and the reason
        // this matters at all: `unpack_entry_data()`
        // (`builtin/index-pack.c:488`) inflates it into a fixed scratch buffer
        // and returns `NULL`, and `parse_pack_objects()` puts every such object
        // on a *delay* list (`builtin/index-pack.c:1279`) that is drained only
        // once the whole pack has been read (`builtin/index-pack.c:1308`). By
        // then every tree in the pack has contributed to `gitmodules_found`, so a
        // streamed blob is checked against the complete set no matter where it
        // sat in the pack — which is why it, and only it, can report
        // `gitmodulesLarge`. Confirmed against git 2.55.0 at
        // `core.bigFileThreshold=100` on a pack built blob-first:
        // `index-pack --strict` still reports the id.
        //
        // `unpack-objects` has no per-object blob pass at all — `write_object()`
        // (`builtin/unpack-objects.c:281`) writes a blob and flags it without
        // fscking it, and a blob over the threshold goes through `stream_blob()`
        // (`builtin/unpack-objects.c:559`) — so a small push, which git unpacks
        // loose, can never report `gitmodulesLarge`. Confirmed on the same pack:
        // `unpack-objects --strict` is silent.
        if kind == gix::object::Kind::Blob {
            if to_loose {
                continue;
            }
            if data.len() as u64 > threshold {
                delayed.push(*id);
                continue;
            }
            let as_modules = gitmodules.contains(id);
            let as_attrs = gitattributes.contains(id);
            if as_modules || as_attrs {
                done.insert(*id);
                for finding in check_blob(Some(&data), as_modules, as_attrs) {
                    report(config, band, &finding, id, &mut failed);
                }
            }
            continue;
        }
        let checked = check_object(kind, &data, true, repo.object_hash().len_in_hex());
        // The tree-entry decoder's own `error:` lines, already prefixed.
        for line in &checked.raw {
            band.write(&format!("{line}\n"));
        }
        gitmodules.extend(checked.gitmodules);
        gitattributes.extend(checked.gitattributes);
        for finding in &checked.findings {
            report(config, band, finding, id, &mut failed);
        }
    }

    // `parse_pack_objects()`'s delayed sweep (`builtin/index-pack.c:1308`): the
    // streamed blobs, now that every tree in the pack has named what it names.
    // They are still part of the per-object pass, so a finding here is
    // `fsck error in packed object`.
    for id in &delayed {
        let as_modules = gitmodules.contains(id);
        let as_attrs = gitattributes.contains(id);
        if !as_modules && !as_attrs {
            continue;
        }
        done.insert(*id);
        for finding in check_blob(None, as_modules, as_attrs) {
            report(config, band, &finding, id, &mut failed);
        }
    }

    // `fsck_finish()`: every blob the trees pointed at, whether or not the pack
    // carried it. Pack order first so the report is reproducible; anything the
    // pack did not carry follows in id order.
    let mut queue: Vec<gix::ObjectId> = received
        .iter()
        .copied()
        .filter(|id| !done.contains(id))
        .filter(|id| gitmodules.contains(id) || gitattributes.contains(id))
        .collect();
    let mut rest: Vec<gix::ObjectId> = gitmodules
        .union(&gitattributes)
        .copied()
        .filter(|id| !done.contains(id) && !queue.contains(id))
        .collect();
    rest.sort();
    queue.append(&mut rest);

    let failed_before_finish = failed;
    for id in queue {
        let as_modules = gitmodules.contains(&id);
        let as_attrs = gitattributes.contains(&id);
        // `fsck_blobs()` reports the failure to read the blob, or its being
        // some other type, once per sweep that named it.
        let (missing, non_blob) = match incoming.find(id) {
            Some((gix::object::Kind::Blob, data)) => {
                // `fsck_blobs()` always reads the whole object
                // (`fsck.c:1337`'s `odb_read_object()`), so nothing is streamed
                // here however large it is.
                for finding in check_blob(Some(&data), as_modules, as_attrs) {
                    report(config, band, &finding, &id, &mut failed);
                }
                continue;
            }
            Some(_) => (false, true),
            None => (true, false),
        };
        for (present, missing_msg, blob_msg, label) in [
            (as_modules, &GITMODULES_MISSING, &GITMODULES_BLOB, ".gitmodules"),
            (as_attrs, &GITATTRIBUTES_MISSING, &GITATTRIBUTES_BLOB, ".gitattributes"),
        ] {
            if !present {
                continue;
            }
            let finding = if missing {
                Finding { msg: missing_msg, text: format!("unable to read {label} blob") }
            } else {
                debug_assert!(non_blob);
                Finding { msg: blob_msg, text: format!("non-blob found at {label}") }
            };
            report(config, band, &finding, &id, &mut failed);
        }
    }

    if failed {
        return Err(if failed_before_finish {
            "fsck error in packed object".into()
        } else {
            "fsck error in pack objects".into()
        });
    }
    Ok(())
}

/// `unpack-objects`: write every object of the received pack into the loose
/// object store of the quarantine, so nothing of the pack survives.
fn explode_pack(incoming: &Incoming<'_>, received: &[gix::ObjectId]) -> Result<()> {
    use gix::objs::{Exists, Write};
    for id in received {
        // Writing an object some source already has only freshens it
        // (`odb_freshen_object()`, odb.c:824-832, odb/source-loose.c:949), and the
        // real store is one of the quarantine's sources. Freshening looks for the
        // file itself, so the empty tree, which `has_object()` answers for whether
        // or not the store holds it (find_cached_object(), odb.c), is written.
        if incoming.repo.objects.exists(id) {
            continue;
        }
        let (kind, data) = incoming.find(*id).ok_or_else(|| anyhow!("unable to read {id}"))?;
        // The id is already known and the object still exists *in the pack*, so
        // the write has to bypass the usual "already present" short-circuit.
        incoming
            .quarantine
            .write_buf_with_known_id(kind, &data, *id)
            .map_err(|e| anyhow!("unable to write loose object {id}: {e}"))?;
    }
    Ok(())
}

/// Build the compare-and-swap `update()` queues for one command: create when
/// `old` is zero, delete when `new` is zero, otherwise a swap that must find
/// `old` in place.
fn ref_edit(
    name: &str,
    old: gix::ObjectId,
    new: gix::ObjectId,
    zero: gix::ObjectId,
) -> Result<RefEdit, String> {
    let full = FullName::try_from(name).map_err(|_| "funny refname".to_string())?;
    let expected = if old == zero {
        PreviousValue::MustNotExist
    } else {
        PreviousValue::MustExistAndMatch(Target::Object(old))
    };
    let change = if new == zero {
        // A delete whose old value is the null oid is `git push :refs/...` naming
        // something the remote never had: `delete_ref()` takes it as "whatever is
        // there, if anything", warns on band 2, and reports `ok`. Asking for
        // `MustNotExist` alongside a deletion is a contradiction the ref
        // transaction refuses outright.
        let expected =
            if old == zero { PreviousValue::Any } else { expected };
        // `execute_commands_non_atomic()` passes `"push"` to `ref_transaction_delete()` just
        // as it does to the update, so deleting the branch the remote's `HEAD` points at
        // records `<old> <null> … push` in its `logs/HEAD` (the `REF_LOG_ONLY` half git's
        // `split_head_update()` adds). The deleted ref's own log goes away regardless.
        Change::Delete { expected, log: RefLog::AndReference, message: "push".into() }
    } else {
        Change::Update {
            log: LogChange { mode: RefLog::AndReference, force_create_reflog: false, message: "push".into() },
            expected,
            new: Target::Object(new),
        }
    };
    // `update()` passes no `REF_NO_DEREF` (builtin/receive-pack.c:1603-1648), so an
    // update naming a symref writes the ref it points at, and the symref keeps only
    // a log entry (`split_symref_update()`, refs/files-backend.c:2555-2607). A
    // delete through a symref likewise deletes the ref it points at, checked
    // against the old value, and logs on the symref and (through
    // `split_head_update()`) on `HEAD`.
    Ok(RefEdit { change, name: full, deref: true })
}

// ---------------------------------------------------------------------------
// Push certificates
// ---------------------------------------------------------------------------

/// Seconds since the epoch, git's `time(NULL)`.
fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// git's `hmac_hash()`: RFC 2104 over the repository's hash algorithm, with the
/// 64-byte block size both SHA-1 and SHA-256 use.
///
/// The argument names follow git's, which are the *opposite* way round from the
/// RFC at the one call site that matters: `prepare_push_cert_nonce` passes the
/// `"<path>:<stamp>"` string as the key and the configured seed as the text.
fn hmac_hash(kind: gix::hash::Kind, key_in: &[u8], text: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let digest = |bytes: &[&[u8]]| -> Vec<u8> {
        let mut hasher = gix::hash::hasher(kind);
        for b in bytes {
            hasher.update(b);
        }
        match hasher.try_finalize() {
            Ok(id) => id.as_slice().to_vec(),
            // The only failure is SHA-1 collision detection, which reports the
            // digest it computed anyway.
            Err(gix::hash::hasher::Error::CollisionAttack { digest }) => {
                digest.as_slice().to_vec()
            }
        }
    };

    let mut key = [0u8; BLOCK];
    if key_in.len() > BLOCK {
        let hashed = digest(&[key_in]);
        key[..hashed.len()].copy_from_slice(&hashed);
    } else {
        key[..key_in.len()].copy_from_slice(key_in);
    }
    let mut ipad = [0u8; BLOCK];
    let mut opad = [0u8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] = key[i] ^ 0x36;
        opad[i] = key[i] ^ 0x5c;
    }
    let inner = digest(&[&ipad, text]);
    digest(&[&opad, &inner])
}

/// `prepare_push_cert_nonce()`: `<stamp>-<hmac>`, where the HMAC is keyed by
/// `"<service-dir>:<stamp>"` over the `receive.certNonceSeed` value. Tying the
/// nonce to the directory and the clock is what lets a later process recognise
/// its own nonce without keeping any state.
fn prepare_push_cert_nonce(
    service_dir: &str,
    stamp: u64,
    seed: &str,
    kind: gix::hash::Kind,
) -> String {
    let key = format!("{service_dir}:{stamp}");
    let mac = hmac_hash(kind, key.as_bytes(), seed.as_bytes());
    let mut out = format!("{stamp}-");
    for b in &mac {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Read the `push-cert` … `push-cert-end` block into `cert`. `Ok(true)` means the
/// block ended at a flush packet, which ends the whole command list too — git's
/// `true_flush`.
///
/// Newlines are kept: the certificate's bytes are what the signature covers, and
/// the reader git uses here has `PACKET_READ_CHOMP_NEWLINE` switched off.
fn read_push_cert(input: &mut impl Read, cert: &mut Vec<u8>) -> Result<bool> {
    loop {
        match read_pkt_line(input)? {
            None => return Ok(true),
            Some(line) => {
                if line == b"push-cert-end\n" {
                    return Ok(false);
                }
                cert.extend_from_slice(&line);
            }
        }
    }
}

/// `queue_commands_from_cert()`: the command lines live between the
/// certificate's blank line and the start of its signature.
fn commands_from_cert(cert: &[u8]) -> Result<Vec<Command>> {
    let Some(boc) = memchr::memmem::find(cert, b"\n\n").map(|i| i + 2) else {
        crate::git_fatal!(
            "malformed push certificate {}",
            String::from_utf8_lossy(&cert[..cert.len().min(100)])
        );
    };
    let eoc = super::verify_tag::parse_signed_buffer(cert);
    let mut out = Vec::new();
    let mut at = boc;
    while at < eoc {
        let end = cert[at..eoc]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| at + p)
            .unwrap_or(eoc);
        let text = std::str::from_utf8(&cert[at..end])
            .map_err(|_| anyhow!("protocol error: non-utf8 command"))?;
        out.push(parse_command(text)?);
        at = if end < eoc { end + 1 } else { eoc };
    }
    Ok(out)
}

/// git's `find_commit_header()`: the value of the first `<key> ` line of the
/// header block, i.e. before the first empty line. Returns every occurrence in
/// order, which is what the repeated `push-option` lookup needs.
fn header_values(buf: &[u8], key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 0;
    let prefix = format!("{key} ");
    while at < buf.len() {
        let end = buf[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| at + p)
            .unwrap_or(buf.len());
        if end == at {
            break; // blank line: end of the header block
        }
        let line = &buf[at..end];
        if line.starts_with(prefix.as_bytes()) {
            out.push(String::from_utf8_lossy(&line[prefix.len()..]).into_owned());
        }
        at = end + 1;
    }
    out
}

/// `check_cert_push_options()`: the options the certificate was signed over must
/// be exactly the options that arrived on the wire, in the same order. An
/// unsigned push has nothing to disagree with and always passes.
fn check_cert_push_options(cert: &[u8], push_options: &[String]) -> bool {
    if cert.is_empty() {
        return true;
    }
    header_values(cert, "push-option") == push_options
}

/// git's `NONCE_*` gradings of the nonce a certificate echoed back.
const NONCE_UNSOLICITED: &str = "UNSOLICITED";
const NONCE_BAD: &str = "BAD";
const NONCE_MISSING: &str = "MISSING";
const NONCE_OK: &str = "OK";
const NONCE_SLOP: &str = "SLOP";

/// Everything `prepare_push_cert_sha1()` works out about a received certificate,
/// which is exactly what the hooks are told through `GIT_PUSH_CERT*`.
struct PushCert {
    /// The blob the certificate was stored as, `GIT_PUSH_CERT`.
    oid: gix::ObjectId,
    /// `sigc->signer`.
    signer: String,
    /// `sigc->key`.
    key: String,
    /// `sigc->result`, the `%G?` character.
    status: char,
    /// The nonce reported to the hook — the one this process issued, or (under
    /// `receive.certNonceSlop`) the one the certificate echoed back.
    nonce: Option<String>,
    /// One of the `NONCE_*` gradings.
    nonce_status: &'static str,
    /// How many seconds stale the echoed nonce is; only reported for `SLOP`.
    nonce_slop: i64,
}

impl PushCert {
    /// `prepare_push_cert_sha1()`: write the certificate out as a blob, verify
    /// its signature, and grade its nonce. `None` for an unsigned push, and also
    /// when the blob cannot be written — git clears the oid there and then skips
    /// every `GIT_PUSH_CERT*` variable.
    fn prepare(
        repo: &gix::Repository,
        raw: Vec<u8>,
        config: &Config,
        stateless_rpc: bool,
    ) -> Option<Self> {
        if raw.is_empty() {
            return None;
        }
        let oid = repo.write_blob(&raw).ok()?.detach();

        let bogs = super::verify_tag::parse_signed_buffer(&raw);
        let check = crate::gitsig::verify_full(&raw[bogs..], &raw[..bogs]);
        let (nonce, nonce_status, nonce_slop) = check_nonce(&raw[..bogs], config, stateless_rpc);
        Some(Self {
            oid,
            signer: check.signer,
            key: check.key,
            status: check.status.code(),
            nonce,
            nonce_status,
            nonce_slop,
        })
    }
}

/// `check_nonce()`: grade the `nonce` header of a certificate's signed payload
/// against the one this server handed out.
///
/// Outside `--stateless-rpc` the echoed nonce must be the very string this
/// process issued. Under it the advertisement came from a *different* process,
/// so a nonce that still verifies against `receive.certNonceSeed` is accepted
/// when its timestamp is within `receive.certNonceSlop` seconds, and reported as
/// `SLOP` (with the drift) when it is not.
fn check_nonce(payload: &[u8], config: &Config, stateless_rpc: bool) -> (Option<String>, &'static str, i64) {
    let issued = config.push_cert_nonce.clone();
    let Some(received) = header_values(payload, "nonce").into_iter().next() else {
        return (issued, NONCE_MISSING, 0);
    };
    let Some(issued) = issued else {
        return (None, NONCE_UNSOLICITED, 0);
    };
    if issued == received {
        return (Some(issued), NONCE_OK, 0);
    }
    if !stateless_rpc {
        return (Some(issued), NONCE_BAD, 0);
    }

    // `<seconds-since-epoch>-<hmac>`, recomputed from the seed and this
    // directory; anything that does not reproduce exactly is a forgery.
    let bad = (Some(issued.clone()), NONCE_BAD, 0);
    let Some((stamp, _)) = received.split_once('-') else { return bad };
    let Ok(stamp) = stamp.parse::<u64>() else { return bad };
    let Some(seed) = &config.cert_nonce_seed else { return bad };
    let expect = prepare_push_cert_nonce(&config.service_dir, stamp, seed, config.hash_kind);
    if expect.len() != received.len() || !constant_time_eq(expect.as_bytes(), received.as_bytes()) {
        return bad;
    }

    // Negative drift means the other server's clock runs ahead of ours.
    let ours: u64 = issued.split('-').next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let slop = ours as i64 - stamp as i64;
    if config.cert_nonce_slop != 0 && slop.abs() <= config.cert_nonce_slop {
        // It passes the HMAC check, so it is ours in every way that matters:
        // report it as the nonce we issued.
        (Some(received), NONCE_OK, slop)
    } else {
        (Some(issued), NONCE_SLOP, slop)
    }
}

/// `constant_memequal()`: no early exit, so a wrong nonce leaks nothing about
/// how much of it was right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

/// The environment additions every receive-side hook gets: the push options, and
/// — for a signed push — everything known about the certificate.
struct HookEnv {
    set: Vec<(String, String)>,
}

/// Build the hook environment: `run_receive_hook()`'s push-option block plus
/// `prepare_push_cert_sha1()`'s certificate block.
///
/// `GIT_PUSH_OPTION_COUNT` is always set, `0` included: `execute_commands()`
/// hands `run_receive_hook()` a real (if empty) option list, so the branch git
/// has for a null list is unreachable from receive-pack. Verified against stock
/// git 2.55.0, whose `pre-receive` sees `GIT_PUSH_OPTION_COUNT=0` on a push that
/// negotiated no options at all.
fn hook_env(repo: &gix::Repository, push_options: &[String], cert: Option<&PushCert>) -> HookEnv {
    let mut set = Vec::new();
    for (i, o) in push_options.iter().enumerate() {
        set.push((format!("GIT_PUSH_OPTION_{i}"), o.clone()));
    }
    set.push(("GIT_PUSH_OPTION_COUNT".into(), push_options.len().to_string()));
    if let Some(cert) = cert {
        set.push(("GIT_PUSH_CERT".into(), cert.oid.to_string()));
        set.push(("GIT_PUSH_CERT_SIGNER".into(), cert.signer.clone()));
        set.push(("GIT_PUSH_CERT_KEY".into(), cert.key.clone()));
        set.push(("GIT_PUSH_CERT_STATUS".into(), cert.status.to_string()));
        if let Some(nonce) = &cert.nonce {
            set.push(("GIT_PUSH_CERT_NONCE".into(), nonce.clone()));
            set.push(("GIT_PUSH_CERT_NONCE_STATUS".into(), cert.nonce_status.into()));
            if cert.nonce_status == NONCE_SLOP {
                set.push(("GIT_PUSH_CERT_NONCE_SLOP".into(), cert.nonce_slop.to_string()));
            }
        }
    }
    let _ = repo;
    HookEnv { set }
}

/// Where a receive-side hook starts, and the `GIT_DIR` it is handed.
///
/// `enter_repo()` settles on the git directory — `<path>/.git` for a
/// repository with a work tree, the path itself for a bare one, the target of
/// a gitfile — `chdir()`s into it and calls `set_git_dir(".", 0)` (setup.c:
/// 1868-1893), and `run_hook_ve()` leaves `dir` unset, so every hook runs in the
/// git directory with `GIT_DIR=.`, bare or not.
fn enter_repo_dir(repo: &gix::Repository) -> (std::path::PathBuf, std::path::PathBuf) {
    (crate::hooks::absolutize(repo.git_dir()), std::path::PathBuf::from("."))
}

/// Spawn `<hooks-dir>/<name>` with git's receive-side wiring: `GIT_DIR` set, the
/// hook's stdout folded into its stderr (`run_hooks_opt`'s default) and the pair
/// relayed to the pusher on band 2.
///
/// `None` when no such hook is installed; otherwise whether it exited non-zero,
/// which is the sense every caller here wants ("declined").
fn spawn_hook(
    repo: &gix::Repository,
    name: &str,
    args: &[String],
    stdin: Option<&[u8]>,
    env: &HookEnv,
    band: &mut Band,
) -> Option<bool> {
    let path = crate::hooks::find(repo, name).ok().flatten()?;

    let (hook_dir, git_dir) = enter_repo_dir(repo);
    // `find_hook()`'s `git_path("hooks/%s")` (hook.c:26-64) under `enter_repo()`'s
    // `GIT_DIR=.`: `hooks/<name>`, or the `core.hooksPath` spelling, is the hook's `$0`.
    let mut cmd = std::process::Command::new(crate::hooks::exec_name(repo, name, &path, &hook_dir));
    cmd.args(args)
        .current_dir(&hook_dir)
        .env("GIT_DIR", &git_dir)
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in &env.set {
        cmd.env(k, v);
    }

    let Ok(mut child) = cmd.spawn() else {
        band.error(&format!("cannot spawn hook '{name}'"));
        return Some(true);
    };
    if let (Some(data), Some(mut sink)) = (stdin, child.stdin.take()) {
        let _ = sink.write_all(data);
        drop(sink);
    }

    relay_child_output(&mut child, band);
    Some(!child.wait().map(|s| s.success()).unwrap_or(false))
}

/// `copy_to_sideband()`: pump the child's stdout and stderr onto band 2 as they
/// arrive, emitting an empty band-1 keepalive whenever `receive.keepAlive`
/// seconds pass with nothing to say.
fn relay_child_output(child: &mut std::process::Child, band: &mut Band) {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let mut pumps = Vec::new();
    for stream in [
        child.stdout.take().map(PipeEnd::Out),
        child.stderr.take().map(PipeEnd::Err),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        pumps.push(std::thread::spawn(move || {
            let mut reader: Box<dyn Read + Send> = match stream {
                PipeEnd::Out(s) => Box::new(s),
                PipeEnd::Err(s) => Box::new(s),
            };
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);

    let timeout = (band.keep_alive > 0).then(|| std::time::Duration::from_secs(band.keep_alive as u64));
    loop {
        let received = match timeout {
            Some(timeout) => match rx.recv_timeout(timeout) {
                Ok(chunk) => Some(chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    band.keep_alive_tick();
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
            },
            None => rx.recv().ok(),
        };
        match received {
            Some(chunk) => band.write_bytes(&chunk),
            None => break,
        }
    }
    for pump in pumps {
        let _ = pump.join();
    }
}

/// One end of a child's output, so both pipes can share a single pump body.
enum PipeEnd {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

/// `run_receive_hook()`: feed `<old> <new> <ref>` lines for every command that is
/// still in play. `skip_broken` drops the ones that already failed, which is what
/// separates `post-receive`'s view from `pre-receive`'s.
///
/// Returns whether the hook declined.
fn run_receive_hook(
    repo: &gix::Repository,
    name: &str,
    cmds: &[Command],
    skip_broken: bool,
    env: &HookEnv,
    band: &mut Band,
) -> bool {
    let mut payload = Vec::new();
    for cmd in cmds {
        if skip_broken && (cmd.error.is_some() || cmd.did_not_exist) {
            continue;
        }
        // The proc-receive hook's rewritten refs are what `post-receive` sees,
        // in place of the ref the client named.
        if cmd.reports.is_empty() {
            payload.extend_from_slice(
                format!("{} {} {}\n", cmd.old, cmd.new, cmd.name).as_bytes(),
            );
            continue;
        }
        for report in &cmd.reports {
            payload.extend_from_slice(
                format!(
                    "{} {} {}\n",
                    report.old.unwrap_or(cmd.old),
                    report.new.unwrap_or(cmd.new),
                    report.ref_name.as_deref().unwrap_or(&cmd.name)
                )
                .as_bytes(),
            );
        }
    }
    // "if there are no valid commands, don't invoke the hook at all."
    if payload.is_empty() {
        return false;
    }
    spawn_hook(repo, name, &[], Some(&payload), env, band).unwrap_or(false)
}

/// `update_worktree()` (receive-pack.c:1472): bring the pushed-to work tree to
/// `new`, which is what `receive.denyCurrentBranch=updateInstead` asks for.
///
/// ```c
/// if (worktree->is_bare)
///         return "denyCurrentBranch = updateInstead needs a worktree";
/// …
/// retval = push_to_checkout(sha1, &invoked_hook, &env, worktree->path);
/// if (!invoked_hook)
///         retval = push_to_deploy(sha1, &env, worktree);
/// ```
///
/// `Some(reason)` is the `ng <ref> <reason>` the client reports; nothing goes to
/// band 2, which is why the pusher sees only the summary line.
fn update_worktree_to(
    repo: &gix::Repository,
    worktree: &super::worktree::SharedHead,
    new: gix::ObjectId,
) -> Option<String> {
    // `worktree` is whichever worktree `find_shared_symref()` found, linked ones
    // included: the children run in `worktree->path` with
    // `GIT_DIR=get_worktree_git_dir(worktree)` (receive-pack.c:1453-1475). It is
    // never bare, as `is_shared_symref()` skips a bare one.
    let workdir = worktree.path.clone();
    let git_dir = worktree.git_dir.as_path();

    // `push_to_checkout()`: the hook takes over the whole job when it exists, and a
    // non-zero exit is the refusal. `invoked_hook` is false when there is no hook.
    //
    // `run_hooks_opt()` leaves `cp->dir` unset, so the hook starts in the git directory
    // `enter_repo()` moved to, named as `find_hook()` spells it there (hook.c:86-104),
    // with no stdin and its stdout on stderr (hook.h:183-186, hook.c:589-607). `GIT_DIR` is
    // `absolute_path(get_worktree_git_dir(worktree))` (receive-pack.c:1465-1467), which
    // for the main worktree is `absolute_path(".")`: the cwd with `/.` appended
    // (abspath.c:293-316).
    if let Ok(Some(hook)) = crate::hooks::find(repo, "push-to-checkout") {
        let (hook_dir, _) = enter_repo_dir(repo);
        let git_dir_env = match crate::setup::realpath(git_dir) == crate::setup::realpath(repo.git_dir()) {
            true => hook_dir.join("."),
            false => git_dir.to_owned(),
        };
        use std::os::fd::AsFd;
        let stdout = std::io::stderr().as_fd().try_clone_to_owned().map(std::process::Stdio::from);
        let ok = std::process::Command::new(crate::hooks::exec_name(repo, "push-to-checkout", &hook, &hook_dir))
            .arg(new.to_hex().to_string())
            .current_dir(&hook_dir)
            .env("GIT_DIR", &git_dir_env)
            .env("GIT_WORK_TREE", crate::hooks::absolutize(&workdir))
            .stdin(std::process::Stdio::null())
            .stdout(stdout.unwrap_or_else(|_| std::process::Stdio::inherit()))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        return (!ok).then(|| "push-to-checkout hook declined".to_string());
    }
    push_to_deploy(repo, worktree, new)
}

/// `push_to_deploy()` (receive-pack.c:1388): four `git` children in the work tree,
/// each with its own refusal.
///
/// ```c
/// "update-index", "-q", "--ignore-submodules", "--refresh"   -> "Up-to-date check failed"
/// "diff-files", "--quiet", "--ignore-submodules", "--"       -> "Working directory has unstaged changes"
/// "diff-index", "--quiet", "--cached", "--ignore-submodules", -> "Working directory has staged changes"
///         <HEAD or the empty tree>, "--"
/// "read-tree", "-u", "-m", <new>                             -> "Could not update working tree to new HEAD"
/// ```
///
/// The `diff-index` argument is `HEAD` unless the work tree's HEAD is unborn, in
/// which case git compares against the empty tree instead.
fn push_to_deploy(
    repo: &gix::Repository,
    worktree: &super::worktree::SharedHead,
    new: gix::ObjectId,
) -> Option<String> {
    let workdir = worktree.path.as_path();
    // `!is_null_oid(&worktree->head_oid) ? "HEAD" : empty_tree_oid_hex()`.
    let head = match worktree.head_unborn {
        false => "HEAD".to_string(),
        true => gix::ObjectId::empty_tree(repo.object_hash()).to_string(),
    };
    let new = new.to_hex().to_string();
    let steps: [(&[&str], &str); 4] = [
        (
            &["update-index", "-q", "--ignore-submodules", "--refresh"],
            "Up-to-date check failed",
        ),
        (
            &["diff-files", "--quiet", "--ignore-submodules", "--"],
            "Working directory has unstaged changes",
        ),
        (
            &["diff-index", "--quiet", "--cached", "--ignore-submodules"],
            "Working directory has staged changes",
        ),
        (&["read-tree", "-u", "-m"], "Could not update working tree to new HEAD"),
    ];
    let Ok(exe) = crate::hosted::git_exe() else {
        return Some("Up-to-date check failed".into());
    };
    for (at, (args, reason)) in steps.into_iter().enumerate() {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(args);
        match at {
            2 => {
                cmd.arg(&head).arg("--");
            }
            3 => {
                cmd.arg(&new);
            }
            _ => {}
        }
        let ok = cmd
            .current_dir(workdir)
            .env("GIT_DIR", &worktree.git_dir)
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_PREFIX")
            // `child.no_stdout = 1` / `stdout_to_stderr = 1`: nothing these print
            // belongs on the pusher's stdout.
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Some(reason.to_string());
        }
    }
    None
}

/// `run_update_hook()`: `update <ref> <old> <new>`, one run per ref.
fn run_update_hook(repo: &gix::Repository, cmd: &Command, band: &mut Band) -> bool {
    let args = [cmd.name.clone(), cmd.old.to_string(), cmd.new.to_string()];
    let env = HookEnv { set: Vec::new() };
    spawn_hook(repo, "update", &args, None, &env, band).unwrap_or(false)
}

/// `run_update_post_hook()`: `post-update <ref>…` for every ref that moved.
fn run_post_update_hook(
    repo: &gix::Repository,
    cmds: &[Command],
    env: &HookEnv,
    band: &mut Band,
) {
    let args: Vec<String> = cmds
        .iter()
        .filter(|c| c.error.is_none() && !c.did_not_exist)
        .map(|c| c.name.clone())
        .collect();
    if args.is_empty() {
        return;
    }
    spawn_hook(repo, "post-update", &args, None, env, band);
}

// ---------------------------------------------------------------------------
// The proc-receive hook
// ---------------------------------------------------------------------------

/// `run_proc_receive_hook()`: hand the scheduled commands to the `proc-receive`
/// hook over a pkt-line conversation and fold its verdicts back into `cmds`.
///
/// The exchange is: a `version=1` line with the negotiated capabilities after a
/// NUL, a flush, the hook's own `version=` reply, a flush; then the commands and
/// a flush; then (only when the hook asked for `push-options`) the options and a
/// flush; then the hook's `ok`/`ng`/`option` report.
///
/// Returns whether the exchange failed, which is what makes the caller mark
/// every unanswered command `fail to run proc-receive hook`.
fn run_proc_receive_hook(
    repo: &gix::Repository,
    cmds: &mut [Command],
    push_options: Option<&[String]>,
    use_atomic: bool,
    band: &mut Band,
) -> bool {
    let Ok(Some(path)) = crate::hooks::find(repo, "proc-receive") else {
        band.error("cannot find hook 'proc-receive'");
        return true;
    };

    // `argv[0]` is `find_hook()`'s path as `enter_repo()` left it (receive-pack.c:1145-1152).
    let program = crate::hooks::exec_name(repo, "proc-receive", &path, &enter_repo_dir(repo).0);
    let mut child = match std::process::Command::new(program)
        .current_dir(enter_repo_dir(repo).0)
        .env("GIT_DIR", enter_repo_dir(repo).1)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            band.error("cannot spawn hook 'proc-receive'");
            return true;
        }
    };
    let mut to_hook = child.stdin.take().expect("stdin piped");
    let mut from_hook = std::io::BufReader::new(child.stdout.take().expect("stdout piped"));
    // The hook's stderr is its diagnostics channel; `copy_to_sideband()` relays
    // it to the pusher on band 2, so it is collected alongside the conversation
    // and flushed once the conversation is over.
    let mut errors = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    // Version negotiation. The capability list rides after a NUL, and only the
    // capabilities this session actually negotiated are offered.
    let mut caps = String::new();
    if use_atomic {
        caps.push_str(" atomic");
    }
    if push_options.is_some() {
        caps.push_str(" push-options");
    }
    let hello = if caps.is_empty() {
        b"version=1\n".to_vec()
    } else {
        format!("version=1\0{}\n", &caps[1..]).into_bytes()
    };
    let mut greeting = Vec::new();
    pkt_line(&mut greeting, &hello);
    flush_pkt(&mut greeting);

    let mut errmsg = String::new();
    let mut version = 0;
    let mut hook_push_options = false;
    // `code` in git: the greeting write, then the read loop, which only a flush
    // ends cleanly — `PACKET_READ_EOF` (the hook exited, or stopped mid-packet)
    // is `code = -1` (receive-pack.c:1205-1235). A hook that answers with a bare
    // flush is a version-0 hook and is accepted.
    let mut negotiated = to_hook.write_all(&greeting).and_then(|()| to_hook.flush()).is_ok();
    if negotiated {
        loop {
            match read_pkt_line(&mut from_hook) {
                Ok(Some(line)) => {
                    let nul = line.iter().position(|&b| b == 0).unwrap_or(line.len());
                    let head = String::from_utf8_lossy(&line[..nul]).trim_end().to_string();
                    if let Some(v) = head.strip_prefix("version=") {
                        version = v.trim().parse().unwrap_or(0);
                        if nul < line.len() {
                            let features = String::from_utf8_lossy(&line[nul + 1..]);
                            hook_push_options =
                                features.split([' ', '\n']).any(|f| f == "push-options");
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    negotiated = false;
                    break;
                }
            }
        }
    }
    match version {
        _ if !negotiated => errmsg.push_str("fail to negotiate version with proc-receive hook"),
        0 | 1 => {}
        v =>errmsg.push_str(&format!("proc-receive version '{v}' is not supported")),
    }

    let mut failed = !errmsg.is_empty();
    if !failed {
        let mut request = Vec::new();
        for cmd in cmds.iter() {
            if !cmd.proc_receive || cmd.skip_update || cmd.error.is_some() {
                continue;
            }
            pkt_line(
                &mut request,
                format!("{} {} {}", cmd.old, cmd.new, cmd.name).as_bytes(),
            );
        }
        flush_pkt(&mut request);
        if let Some(options) = push_options.filter(|_| hook_push_options) {
            for option in options {
                pkt_line(&mut request, option.as_bytes());
            }
            flush_pkt(&mut request);
        }
        if to_hook.write_all(&request).and_then(|()| to_hook.flush()).is_err() {
            errmsg.push_str("fail to write commands to proc-receive hook");
            failed = true;
        }
    }
    drop(to_hook);

    if !failed {
        failed = read_proc_receive_report(&mut from_hook, cmds, &mut errmsg);
    }
    let _ = child.wait();
    if let Some(pump) = errors.take() {
        if let Ok(text) = pump.join() {
            band.write_bytes(&text);
        }
    }

    if !errmsg.is_empty() {
        band.error(errmsg.trim_end_matches('\n'));
    }
    failed
}

/// `read_proc_receive_report()`: apply the hook's `ok`/`ng`/`option` lines to the
/// commands it was given.
fn read_proc_receive_report(
    from_hook: &mut impl BufRead,
    cmds: &mut [Command],
    errmsg: &mut String,
) -> bool {
    let mut failed = false;
    let mut responded = false;
    // The index of the command the last `ok`/`ng` named, which is where `option`
    // lines attach. `new_report` means the next `option` opens a fresh report;
    // `report_open` is git's non-NULL `report` pointer, i.e. one is already open.
    let mut hint: Option<usize> = None;
    let mut new_report = false;
    let mut report_open = false;
    let mut once = false;

    loop {
        let line = match read_pkt_line(from_hook) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(_) => {
                if !responded {
                    errmsg.push_str("proc-receive exited abnormally");
                    return true;
                }
                break;
            }
        };
        responded = true;
        let text = String::from_utf8_lossy(&line).trim_end().to_string();
        let Some((head, rest)) = text.split_once(' ') else {
            errmsg.push_str(&format!(
                "proc-receive reported incomplete status line: '{text}'\n"
            ));
            failed = true;
            continue;
        };

        if head == "option" {
            let Some(index) = hint.filter(|_| report_open || new_report) else {
                if !once {
                    once = true;
                    errmsg.push_str(
                        "proc-receive reported 'option' without a matching 'ok/ng' directive\n",
                    );
                }
                failed = true;
                continue;
            };
            if new_report {
                cmds[index].reports.push(PushReport::default());
                new_report = false;
                report_open = true;
            }
            let (key, value) = match rest.split_once(' ') {
                Some((key, value)) => (key, Some(value)),
                None => (rest, None),
            };
            let report = cmds[index].reports.last_mut().expect("just pushed");
            match (key, value) {
                ("refname", Some(v)) => report.ref_name = Some(v.to_string()),
                ("old-oid", Some(v)) => {
                    report.old = gix::ObjectId::from_hex(v.as_bytes()).ok();
                }
                ("new-oid", Some(v)) => {
                    report.new = gix::ObjectId::from_hex(v.as_bytes()).ok();
                }
                ("forced-update", _) => report.forced_update = true,
                // "Fall through, let 'receive-pack' to execute it."
                ("fall-through", _) => {
                    cmds[index].proc_receive = false;
                    cmds[index].proc_receive_returned = false;
                    cmds[index].reports.clear();
                    report_open = false;
                }
                _ => {}
            }
            continue;
        }

        report_open = false;
        new_report = false;
        let (refname, reason) = match rest.split_once(' ') {
            Some((refname, reason)) => (refname, Some(reason)),
            None => (rest, None),
        };
        if head != "ok" && head != "ng" {
            errmsg.push_str(&format!(
                "proc-receive reported bad status '{head}' on ref '{refname}'\n"
            ));
            failed = true;
            continue;
        }

        // "first try searching at our hint, falling back to all refs"
        let found = hint
            .and_then(|from| cmds[from..].iter().position(|c| c.name == refname).map(|i| i + from))
            .or_else(|| cmds.iter().position(|c| c.name == refname));
        let Some(index) = found else {
            errmsg.push_str(&format!(
                "proc-receive reported status on unknown ref: {refname}\n"
            ));
            failed = true;
            continue;
        };
        hint = Some(index);
        if !cmds[index].proc_receive {
            errmsg.push_str(&format!(
                "proc-receive reported status on unexpected ref: {refname}\n"
            ));
            failed = true;
            continue;
        }
        cmds[index].proc_receive_returned = true;
        if head == "ng" {
            cmds[index].error = Some(reason.unwrap_or("failed").to_string());
            failed = true;
            continue;
        }
        new_report = true;
    }

    for cmd in cmds.iter_mut() {
        if cmd.proc_receive && cmd.error.is_none() && !cmd.proc_receive_returned {
            cmd.error = Some("proc-receive failed to report status".into());
            failed = true;
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The nonce formula, pinned against a value captured from stock git 2.55.0:
    /// `receive.certNonceSeed=s3cr3t` in `/tmp/.../bare.git` at stamp
    /// 1785129221 advertised `push-cert=1785129221-30796ff6…`. The HMAC is keyed
    /// by `"<dir>:<stamp>"` over the *seed*, not the other way round.
    #[test]
    fn nonce_matches_stock_gits_hmac() {
        let dir = "/private/tmp/claude-501/-Users-wizard-RustroverProjects-\
                   MenkeTechnologiesMeta-zvcs/ee941629-6b7f-4b50-bcde-b9f8f4c4a62e/\
                   scratchpad/t1/bare.git";
        assert_eq!(
            prepare_push_cert_nonce(dir, 1785129221, "s3cr3t", gix::hash::Kind::Sha1),
            "1785129221-30796ff618100b3bf91d9d42bb8ba210d8c83687"
        );
    }

    /// `proc_receive_ref_append` / `proc_receive_ref_matches`: the `adm` flags
    /// select which command kinds divert, `!` inverts the prefix test, and a
    /// prefix only matches at a `/` boundary.
    #[test]
    fn proc_receive_patterns_select_by_kind_and_prefix() {
        let bare = ProcReceivePattern::parse("refs/for/");
        assert!(bare.matches("refs/for/main", true, false));
        assert!(bare.matches("refs/for/main", false, true));
        assert!(!bare.matches("refs/formal", false, false));

        let add_only = ProcReceivePattern::parse("a:refs/for");
        assert!(add_only.matches("refs/for/main", true, false));
        assert!(!add_only.matches("refs/for/main", false, false));
        assert!(!add_only.matches("refs/for/main", false, true));

        // `!` only inverts the *prefix* test; the `adm` letters still have to
        // select the command kind. A bare `!:` therefore wants nothing and
        // matches nothing — verified against stock git 2.55.0, where
        // `receive.procReceiveRefs=!:refs/heads` leaves the hook unrun for a
        // push to `refs/for/x` while `am!:refs/heads` runs it.
        assert!(!ProcReceivePattern::parse("!:refs/heads").matches("refs/for/main", true, false));
        let negated = ProcReceivePattern::parse("am!:refs/heads");
        assert!(negated.matches("refs/for/main", true, false));
        assert!(!negated.matches("refs/heads/main", true, false));
    }

    /// The certificate's own `push-option` headers have to be exactly the
    /// options that arrived unsigned, in order.
    #[test]
    fn cert_push_options_must_match_the_wire() {
        let cert = b"certificate version 0.1\npusher x\npush-option a\npush-option b\n\n\
                     0000 1111 refs/heads/main\n"
            .to_vec();
        assert!(check_cert_push_options(&cert, &["a".into(), "b".into()]));
        assert!(!check_cert_push_options(&cert, &["a".into()]));
        assert!(!check_cert_push_options(&cert, &["b".into(), "a".into()]));
        // An unsigned push has nothing to disagree with.
        assert!(check_cert_push_options(b"", &["a".into()]));
    }

    /// The command list of a signed push comes from between the certificate's
    /// blank line and the start of its signature — never from the wire.
    #[test]
    fn commands_come_from_inside_the_signed_payload() {
        let zero = "0".repeat(40);
        let one = "1".repeat(40);
        let cert = format!(
            "certificate version 0.1\npusher x\nnonce 1-2\n\n\
             {zero} {one} refs/heads/main\n\
             -----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n"
        );
        let cmds = commands_from_cert(cert.as_bytes()).expect("parses");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "refs/heads/main");
        assert_eq!(cmds[0].new.to_string(), one);
    }
}
