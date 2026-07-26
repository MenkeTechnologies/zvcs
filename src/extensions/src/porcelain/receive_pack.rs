//! `git receive-pack` — the server side of a push.
//!
//! `receive-pack` is a protocol server. It writes a ref advertisement, then reads
//! commands, a packfile and (optionally) a push certificate off stdin, ingests the
//! pack, runs hooks and updates refs. The advertisement (below) and the receive path
//! (`receive`: command list → pack ingest via `gix_pack::Bundle` → ref
//! compare-and-swap → `report-status`) are both implemented — enough for a local
//! (`file://`) or ssh push served by this binary. Hooks, the quarantine, push certs,
//! push-options and atomic pushes are not modelled (see the notes below). The
//! advertisement is byte-verified against git 2.55.0:
//!
//!   * **The ref advertisement** — `<oid> SP <ref>` pkt-lines in refname order,
//!     with the capability list appended to the first line after a NUL, the
//!     `0000000…0 capabilities^{}` line for a repository with no refs, the
//!     `shallow <oid>` lines a shallow repository adds, and the closing flush.
//!     Symbolic refs are resolved but tags are *not* peeled — `receive-pack`
//!     advertises no `^{}` rows.
//!   * **The capability list**, in git's emission order, honouring
//!     `receive.advertiseAtomic`, `repack.useDeltaBaseOffset` and
//!     `receive.advertisePushOptions`, plus `object-format=<algo>` from the
//!     repository's hash and `agent=` from `GIT_USER_AGENT` (see [`agent`]).
//!   * **Hidden refs** — `transfer.hideRefs` and `receive.hideRefs` are applied
//!     to the advertisement through `ref_is_hidden()` (last pattern wins, `!`
//!     un-hides), and a push to a hidden ref is rejected with
//!     `deny updating a hidden ref`.
//!   * **`--http-backend-info-refs` / `--advertise-refs`** — advertise and exit 0.
//!   * **Argument handling**: `-h` prints the 68-byte usage block on *stdout*
//!     and exits 129; an unknown option prints ``error: unknown option `x'``
//!     (or ``unknown switch `c'``) followed by that usage block on stderr, 129;
//!     `--quiet=<v>` prints ``error: option `quiet' takes no value`` alone, 129;
//!     no directory / more than one directory print `fatal: …` followed by the
//!     158-byte usage block that also lists the hidden `--advertise-refs`, 129.
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
//!   * **`receive.denyCurrentBranch`, `receive.denyDeleteCurrent`,
//!     `receive.denyDeletes`, `receive.denyNonFastForwards`** are checked per
//!     command in `update()`'s order, each producing git's own band-2 message
//!     and `ng` reason, including the two advice blocks the unconfigured
//!     defaults print.
//!   * **`receive.updateServerInfo`** refreshes `info/refs` and
//!     `objects/info/packs` after the refs move.
//!   * **`side-band-64k`**: when the client advertises it, the report-status
//!     stream is multiplexed on band 1 and every diagnostic on band 2;
//!     otherwise the report is written as plain pkt-lines and diagnostics go to
//!     stderr.
//!
//! ### Not ported (bailed on with a precise message, never silently ignored)
//!
//!   1. **Thin-pack completion.** `gix_pack::Bundle::write_to_directory` takes
//!      a `thin_pack_base_object_lookup` used to *resolve* external deltas for
//!      index computation (`gix-pack/src/bundle/write/mod.rs:53`), but it does
//!      not append the base objects to the pack the way `index-pack --fix-thin`
//!      does, and it writes no `.rev` reverse index. A kept pack therefore
//!      differs on disk from the one git would have stored.
//!   2. **Hooks and the quarantine.** "client-side hooks for … push" and
//!      "quarantine-aware hook execution" are unchecked
//!      (`crate-status.md:670`, `:672`). `pre-receive`, `update`,
//!      `post-receive` and `post-update` all observe `GIT_QUARANTINE_PATH`, and
//!      their exit codes decide which refs move.
//!   3. **`receive.denyCurrentBranch=updateInstead`** would have to check the
//!      remote work tree out at the pushed tip (git's `push-to-checkout` hook
//!      path); the command is rejected instead of pretending to have done it.
//!   4. **Atomic pushes, push options and push certificates.** `atomic` and
//!      `push-options` are advertised when configured but not implemented on
//!      the receive side, and `receive.certNonceSeed` (which adds a
//!      `push-cert=<nonce>` capability) bails before the advertisement.
//!
//! `GIT_NAMESPACE` (git advertises namespaced names) and object alternates (git
//! appends one `<oid> .have` line per alternate ref) also bail rather than
//! producing a short advertisement.
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

/// The same block as `usage_msg_opt` renders it for the two argument-count
/// errors, which also lists the hidden `--advertise-refs` (158 bytes).
const FULL_USAGE: &str = "\
usage: git receive-pack <git-dir>

    -q, --[no-]quiet      quiet
    --[no-]advertise-refs ...
                          alias of --http-backend-info-refs

";

/// The git version this port reproduces, used to build the `agent=` capability.
const GIT_VERSION: &str = "2.55.0";

/// Parsed command line for a single `receive-pack` invocation.
struct Opts {
    /// `-q`/`--quiet`: suppresses receive-path reporting only.
    quiet: bool,
    /// `--http-backend-info-refs`/`--advertise-refs`: advertise, then exit 0.
    advertise_only: bool,
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

    let Some(mut repo) = open_repo(&opts.dir) else {
        eprintln!(
            "fatal: '{}' does not appear to be a git repository",
            opts.dir
        );
        return Ok(ExitCode::from(128));
    };

    reject_unportable_advertisement(&repo)?;

    // `receive_pack_config()` runs before the advertisement, so a bad
    // `receive.fsck.<msg-id>` value kills the session before a byte is written.
    let config = match Config::read(&repo) {
        Ok(config) => config,
        Err(fatal) => {
            eprintln!("{fatal}");
            return Ok(ExitCode::from(128));
        }
    };

    let adv = advertisement(&repo, &config)?;
    {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&adv)?;
        stdout.flush()?;
    }

    if opts.advertise_only {
        return Ok(ExitCode::SUCCESS);
    }
    let _ = opts.quiet; // suppresses progress only; the report-status is unaffected.

    receive(&mut repo, &config)
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
                "quiet" | "http-backend-info-refs" | "advertise-refs"
            );
            if known && value.is_some() {
                eprintln!("error: option `{name}' takes no value");
                return Ok(Parsed::Exit(ExitCode::from(129)));
            }
            match name {
                "quiet" => quiet = on,
                "http-backend-info-refs" | "advertise-refs" => advertise_only = on,
                // Real but unported git options; the receive path they belong
                // to is not implemented, so accepting them would mislead.
                "stateless-rpc" | "skip-connectivity-check" | "reject-thin-pack-for-testing"
                | "signed-push" => {
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
                    print!("{SHORT_USAGE}");
                    return Ok(Parsed::Exit(ExitCode::from(129)));
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
        dir: (*dir).to_string(),
    }))
}

/// git's `enter_repo()` reduced to what `receive-pack` relies on: the operand
/// names the repository directly, either as the git directory or as the work
/// tree holding it. There is deliberately no upward search — `git receive-pack
/// <repo>/<subdir>` fails even inside a repository.
fn open_repo(dir: &str) -> Option<gix::Repository> {
    // `gix::open` already expands `<path>` to `<path>/.git` for a work tree.
    gix::open(std::path::Path::new(dir)).ok()
}

/// Bail on repository state that changes the advertisement in a way this port
/// does not reproduce, rather than emitting a silently wrong ref list.
fn reject_unportable_advertisement(repo: &gix::Repository) -> Result<()> {
    let config = repo.config_snapshot();
    if config.string("receive.certNonceSeed").is_some() {
        bail!("receive.certNonceSeed is not supported (the push-cert capability needs a nonce and a signed-push reader)");
    }
    if std::env::var_os("GIT_NAMESPACE").is_some() {
        bail!("GIT_NAMESPACE is not supported (git advertises namespaced ref names)");
    }
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
    let caps = capabilities(repo);
    let mut out = Vec::new();
    let mut sent_capabilities = false;

    for reference in repo.references()?.all()? {
        // Broken refs are skipped, as git's ref iteration does.
        let Ok(mut reference) = reference else { continue };
        let name = reference.name().as_bstr().to_string();
        // `show_ref()` runs every candidate through `ref_is_hidden()` first.
        if config.ref_is_hidden(&name) {
            continue;
        }
        // Symbolic refs resolve to their object; tags are not peeled here.
        let Ok(id) = reference.follow_to_object() else {
            continue;
        };
        let line = if sent_capabilities {
            format!("{} {name}\n", id.detach().to_hex())
        } else {
            sent_capabilities = true;
            format!("{} {name}\0{caps}\n", id.detach().to_hex())
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

/// The capability list, in `receive-pack.c`'s emission order.
///
/// `atomic` and `ofs-delta` default on, `push-options` defaults off.
fn capabilities(repo: &gix::Repository) -> String {
    let config = repo.config_snapshot();
    let on = |key: &str, default: bool| config.boolean(key).unwrap_or(default);

    let mut caps = String::from("report-status report-status-v2 delete-refs side-band-64k quiet");
    if on("receive.advertiseAtomic", true) {
        caps.push_str(" atomic");
    }
    if on("repack.useDeltaBaseOffset", true) {
        caps.push_str(" ofs-delta");
    }
    if on("receive.advertisePushOptions", false) {
        caps.push_str(" push-options");
    }
    caps.push_str(&format!(" object-format={}", repo.object_hash()));
    caps.push_str(&format!(" agent={}", agent()));
    caps
}

/// git's `git_user_agent()`: `$GIT_USER_AGENT` when set, else
/// `git/<version>-<uname -s>`.
///
/// The suffix is the kernel name git appends at runtime; the mapping below
/// covers the platforms zvcs targets, and falls back to Rust's own OS name.
fn agent() -> String {
    if let Some(agent) = std::env::var_os("GIT_USER_AGENT") {
        return agent.to_string_lossy().into_owned();
    }
    let sysname = match std::env::consts::OS {
        "macos" => "Darwin",
        "linux" => "Linux",
        "freebsd" => "FreeBSD",
        "netbsd" => "NetBSD",
        "openbsd" => "OpenBSD",
        other => other,
    };
    format!("git/{GIT_VERSION}-{sysname}")
}

/// Append one pkt-line: a four-digit hex length covering the header itself,
/// followed by the payload.
fn pkt_line(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

/// Append a flush packet.
fn flush_pkt(out: &mut Vec<u8>) {
    out.extend_from_slice(b"0000");
}

/// One ref-update command a client sent: move `name` from `old` to `new` (a zero
/// `old` is a create, a zero `new` a delete).
struct Command {
    old: gix::ObjectId,
    new: gix::ObjectId,
    name: String,
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
    /// `transfer.hideRefs` then `receive.hideRefs`, in that order — git reads
    /// both into one list and lets the last match win, so a `!`-negation in
    /// `receive.hideRefs` overrides a `transfer.hideRefs` pattern but not the
    /// other way around, which is where this ordering can differ from git's
    /// (git keeps whatever order the config files produced).
    hide_refs: Vec<String>,
}

impl Config {
    /// Read the whole family. The error is the complete `fatal: …` line git
    /// dies with before writing the advertisement.
    fn read(repo: &gix::Repository) -> Result<Self, String> {
        let config = repo.config_snapshot();
        let deny = |key: &str| match config.string(key) {
            Some(v) => DenyAction::parse(&v.to_string()),
            None => DenyAction::Unconfigured,
        };
        let hide_refs = hide_ref_patterns(&config, "receive.hideRefs");
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
        })
    }

    /// Whether the advertisement and the update path must pretend `name` does
    /// not exist.
    fn ref_is_hidden(&self, name: &str) -> bool {
        ref_is_hidden(&self.hide_refs, name)
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
    ) -> Option<String> {
        let name = cmd.name.as_str();
        let deleting = cmd.new == zero;

        // A hidden ref is refused with the status alone; git prints nothing.
        if self.ref_is_hidden(name) {
            return Some("deny updating a hidden ref".into());
        }

        if !deleting && head == Some(name) && !repo.is_bare() {
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
                DenyAction::UpdateInstead => {
                    // Would have to check the work tree out at the new tip.
                    band.error(
                        "receive.denyCurrentBranch=updateInstead is not supported \
                         (the work tree would have to be updated to the pushed tip)",
                    );
                    return Some("branch is currently checked out".into());
                }
            }
        }

        if deleting {
            if self.deny_deletes && name.starts_with("refs/heads/") {
                band.error(&format!("denying ref deletion for {name}"));
                return Some("deletion prohibited".into());
            }
            if head == Some(name) {
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

/// `refs.c::parse_hide_refs_config` for one protocol: the shared
/// `transfer.hideRefs` patterns followed by the protocol's own
/// (`receive.hideRefs`, `uploadpack.hideRefs`), which is the order that decides
/// which `!`-negation wins. git keeps whatever order the config files produced,
/// so a `transfer.hideRefs` negation of a `receive.hideRefs` pattern is where
/// this can differ.
pub fn hide_ref_patterns(config: &gix::config::Snapshot<'_>, protocol_key: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    for key in ["transfer.hideRefs", protocol_key] {
        patterns.extend(
            config
                .raw_values(key)
                .unwrap_or_default()
                .into_iter()
                .map(|v| v.to_string()),
        );
    }
    patterns
}

/// `refs.c::ref_is_hidden`: the last pattern that matches wins, a leading `!`
/// un-hides, a leading `^` matches the fully qualified (namespaced) name, and a
/// pattern only matches at a `/` boundary or at the end of the name.
pub fn ref_is_hidden(patterns: &[String], name: &str) -> bool {
    for pattern in patterns.iter().rev() {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern.as_str()),
        };
        // Without a namespace the qualified and unqualified names are equal.
        let pattern = pattern.strip_prefix('^').unwrap_or(pattern);
        if let Some(rest) = name.strip_prefix(pattern) {
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
fn receive(repo: &mut gix::Repository, config: &Config) -> Result<ExitCode> {
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
        let (body, cap) = match (cmds.is_empty() && caps.is_empty(), line.iter().position(|&b| b == 0)) {
            (true, Some(n)) => (&line[..n], Some(line[n + 1..].to_vec())),
            _ => (&line[..], None),
        };
        if let Some(c) = cap {
            caps = c;
        }
        let text = std::str::from_utf8(body)
            .map_err(|_| anyhow!("protocol error: non-utf8 command"))?
            .trim_end();
        let mut it = text.splitn(3, ' ');
        let (o, n, name) = match (it.next(), it.next(), it.next()) {
            (Some(o), Some(n), Some(name)) if !name.is_empty() => (o, n, name),
            _ => bail!("protocol error: expected old/new/ref, got {text:?}"),
        };
        cmds.push(Command {
            old: gix::ObjectId::from_hex(o.as_bytes()).map_err(|_| anyhow!("protocol error: bad old id"))?,
            new: gix::ObjectId::from_hex(n.as_bytes()).map_err(|_| anyhow!("protocol error: bad new id"))?,
            name: name.to_string(),
        });
    }
    if cmds.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // Everything git says back travels on band 2 when the client asked for
    // side-band-64k, and on receive-pack's own stderr otherwise.
    let mut band = Band { sideband: cap_present(&caps, b"side-band-64k") };

    // --- ingest the pack (skipped when every command is a delete) -----------
    let mut unpack: Result<(), String> = Ok(());
    if cmds.iter().any(|c| c.new != zero) {
        // git reports the *child*'s failure rather than the reason; the reason
        // itself has already gone back on band 2, printed by that child.
        if let Err(status) = ingest_pack(repo, &mut stdin, config, &mut band) {
            unpack = Err(status);
        }
    }

    // --- apply the ref updates ---------------------------------------------
    let head = head_name(repo);
    let mut verdicts: Vec<(String, Result<(), String>)> = Vec::with_capacity(cmds.len());
    for c in &cmds {
        let v = if unpack.is_err() {
            Err("unpacker error".to_string())
        } else {
            match config.refuse(repo, c, zero, head.as_deref(), &mut band) {
                Some(reason) => Err(reason),
                None => apply_update(repo, &c.name, c.old, c.new, zero),
            }
        };
        verdicts.push((c.name.clone(), v));
    }

    // --- report-status ------------------------------------------------------
    if cap_present(&caps, b"report-status") || cap_present(&caps, b"report-status-v2") {
        let mut report: Vec<u8> = Vec::new();
        match &unpack {
            Ok(()) => pkt_line(&mut report, b"unpack ok\n"),
            Err(e) => pkt_line(&mut report, format!("unpack {e}\n").as_bytes()),
        }
        for (name, v) in &verdicts {
            match v {
                Ok(()) => pkt_line(&mut report, format!("ok {name}\n").as_bytes()),
                Err(reason) => pkt_line(&mut report, format!("ng {name} {reason}\n").as_bytes()),
            }
        }
        flush_pkt(&mut report);

        let mut out: Vec<u8> = Vec::new();
        if band.sideband {
            // The whole report-status stream is one band-1 payload, followed by
            // the flush that ends the multiplexed stream itself.
            for chunk in report.chunks(MAX_SIDEBAND_PAYLOAD) {
                let mut payload = vec![1u8];
                payload.extend_from_slice(chunk);
                pkt_line(&mut out, &payload);
            }
            flush_pkt(&mut out);
        } else {
            out = report;
        }
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&out)?;
        stdout.flush()?;
    }

    // `cmd_receive_pack()` refreshes the dumb-transport info files last of all.
    if config.update_server_info && verdicts.iter().any(|(_, v)| v.is_ok()) {
        update_server_info(repo);
    }
    Ok(ExitCode::SUCCESS)
}

/// The largest payload one side-band packet can carry: 65520 minus the 4-byte
/// pkt-line header and the 1-byte band number (`LARGE_PACKET_DATA_MAX`).
const MAX_SIDEBAND_PAYLOAD: usize = 65515;

/// Where receive-pack's diagnostics go. With `side-band-64k` the client
/// multiplexes them back out with a `remote: ` prefix; without it they land on
/// receive-pack's own stderr, which for a local push is the user's terminal.
struct Band {
    sideband: bool,
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
        if !self.sideband {
            eprint!("{text}");
            return;
        }
        let mut out = Vec::new();
        for chunk in text.as_bytes().chunks(MAX_SIDEBAND_PAYLOAD) {
            let mut payload = vec![2u8];
            payload.extend_from_slice(chunk);
            pkt_line(&mut out, &payload);
        }
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&out);
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
    match len {
        0 => Ok(None),                 // flush
        1..=4 => Ok(Some(Vec::new())), // flush/delim/response-end or empty line
        _ => {
            let mut buf = vec![0u8; len as usize - 4];
            read_exact(r, &mut buf)?;
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

/// Index the packfile streaming off `input`, then either explode it into loose
/// objects or keep it — `receive-pack.c::unpack()`'s `unpack-objects` versus
/// `index-pack` choice, decided by the object count in the pack header against
/// `receive.unpackLimit`. A thin pack's external delta bases are resolved from
/// the odb (git's `index-pack --fix-thin`), so a `send-pack` thin pack lands
/// complete.
///
/// On failure the diagnostics have already gone back to the pusher and what is
/// returned is the `unpack <status>` line the client reports: either one of
/// `parse_pack_header()`'s own complaints, or `<child> abnormal exit` naming
/// whichever child git would have run.
fn ingest_pack(
    repo: &gix::Repository,
    input: &mut impl BufRead,
    config: &Config,
    band: &mut Band,
) -> Result<(), String> {
    // `unpack()` reads the 12-byte header first to learn the object count, and
    // that count alone decides which child runs. A short read never reaches a
    // child, so it has a status of its own rather than an abnormal exit.
    let mut header = [0u8; 12];
    if read_exact(input, &mut header).is_err() {
        return Err("eof before pack header was fully read".into());
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

    let pack_dir = repo.objects.store_ref().path().join("pack");
    if std::fs::create_dir_all(&pack_dir).is_err() {
        band.fatal("cannot create pack directory");
        return Err(child);
    }
    let outcome = gix::odb::pack::Bundle::write_to_directory(
        &mut counted,
        Some(&pack_dir),
        &mut gix::progress::Discard,
        &AtomicBool::new(false),
        Some(repo.objects.clone()),
        gix::odb::pack::bundle::write::Options {
            object_hash: gix::hash::Kind::Sha1,
            ..Default::default()
        },
    );
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
    // `write_to_directory` always drops a `.keep`; a received push keeps none.
    if let Some(kp) = &outcome.keep_path {
        let _ = std::fs::remove_file(kp);
    }
    let (index_path, data_path) = match (&outcome.index_path, &outcome.data_path) {
        (Some(index), Some(data)) => (index.clone(), data.clone()),
        _ => return Ok(()),
    };

    // `--strict`: every object the push brought in is linted before any ref
    // moves, and the first error kills the whole push.
    let received = match pack_object_ids(&index_path) {
        Ok(ids) => ids,
        Err(e) => {
            band.fatal(&e.to_string());
            return Err(child);
        }
    };
    if config.fsck_objects {
        if let Err(message) = fsck_received(repo, &received, config, band) {
            band.fatal(&message);
            return Err(child);
        }
    }

    // `unpack-objects` stores a small push loose and leaves no pack behind.
    if to_loose {
        if let Err(e) = explode_pack(repo, &received) {
            band.fatal(&e.to_string());
            return Err(child);
        }
        let _ = std::fs::remove_file(&index_path);
        let _ = std::fs::remove_file(&data_path);
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
fn pack_object_ids(index_path: &std::path::Path) -> Result<Vec<gix::ObjectId>> {
    let index = gix::odb::pack::index::File::at(index_path, gix::hash::Kind::Sha1)?;
    Ok(index.iter().map(|e| e.oid).collect())
}

/// `fsck_objects`: run the object-content message layer over everything the
/// push delivered, at the severities `receive.fsck.<msg-id>` selects. The
/// message text is `index-pack`/`unpack-objects`' spelling, which names the
/// object rather than its type.
fn fsck_received(
    repo: &gix::Repository,
    received: &[gix::ObjectId],
    config: &Config,
    band: &mut Band,
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
        let Ok(object) = repo.find_object(*id) else { continue };
        let checked = check_object(object.kind, &object.data, true);
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

    // `fsck_finish()`: every blob the trees pointed at, whether or not the pack
    // carried it. Pack order first so the report is reproducible; anything the
    // pack did not carry follows in id order.
    let mut queue: Vec<gix::ObjectId> = received
        .iter()
        .copied()
        .filter(|id| gitmodules.contains(id) || gitattributes.contains(id))
        .collect();
    let mut rest: Vec<gix::ObjectId> = gitmodules
        .union(&gitattributes)
        .copied()
        .filter(|id| !queue.contains(id))
        .collect();
    rest.sort();
    queue.append(&mut rest);

    let failed_before_finish = failed;
    for id in queue {
        let as_modules = gitmodules.contains(&id);
        let as_attrs = gitattributes.contains(&id);
        // `fsck_blobs()` reports the failure to read the blob, or its being
        // some other type, once per sweep that named it.
        let (missing, non_blob) = match repo.find_object(id) {
            Ok(object) if object.kind == gix::object::Kind::Blob => {
                for finding in check_blob(&object.data, as_modules, as_attrs) {
                    report(config, band, &finding, &id, &mut failed);
                }
                continue;
            }
            Ok(_) => (false, true),
            Err(_) => (true, false),
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
/// object store, so nothing of the pack survives.
fn explode_pack(repo: &gix::Repository, received: &[gix::ObjectId]) -> Result<()> {
    use gix::objs::Write;
    for id in received {
        let object = repo.find_object(*id)?;
        // The id is already known and the object still exists *in the pack*, so
        // the write has to bypass the usual "already present" short-circuit.
        repo.objects
            .write_buf_with_known_id(object.kind, &object.data, *id)
            .map_err(|e| anyhow!("unable to write loose object {id}: {e}"))?;
    }
    Ok(())
}

/// Apply one ref update as a compare-and-swap against `old` (create when `old` is
/// zero, delete when `new` is zero), returning the client-facing reason on failure.
fn apply_update(
    repo: &gix::Repository,
    name: &str,
    old: gix::ObjectId,
    new: gix::ObjectId,
    zero: gix::ObjectId,
) -> Result<(), String> {
    let full = FullName::try_from(name).map_err(|_| "funny refname".to_string())?;
    let expected = if old == zero {
        PreviousValue::MustNotExist
    } else {
        PreviousValue::MustExistAndMatch(Target::Object(old))
    };
    let change = if new == zero {
        Change::Delete { expected, log: RefLog::AndReference }
    } else {
        Change::Update {
            log: LogChange { mode: RefLog::AndReference, force_create_reflog: false, message: "push".into() },
            expected,
            new: Target::Object(new),
        }
    };
    repo.edit_reference(RefEdit { change, name: full, deref: false })
        .map(|_| ())
        .map_err(|e| e.to_string())
}
