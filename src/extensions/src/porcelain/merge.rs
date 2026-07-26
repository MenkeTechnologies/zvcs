//! `git merge` — fast-forward, `--no-ff` over a fast-forwardable history,
//! `--abort` and `--quit`.
//!
//! What is served natively via the vendored gitoxide crates:
//!
//! * A fast-forward merge: the ref being merged is a descendant of the current
//!   `HEAD` (their merge-base is `HEAD` itself). The branch `HEAD` points to is
//!   advanced (or `HEAD` itself on a detached head), and the clean worktree +
//!   index are moved to the new tree.
//! * `--no-ff` over that same fast-forwardable history. The merged tree is then
//!   exactly the tree of the ref being merged — when the merge-base *is* our
//!   own commit, the three-way merge of every path resolves to theirs — so the
//!   merge commit is written directly with no three-way machinery involved.
//! * A real merge of diverged histories (with or without `--no-ff`), via the
//!   shared three-way merge in [`crate::merge_apply`]: `Auto-merging`/`CONFLICT`
//!   reporting, a clean two-parent merge commit, or — on conflict —
//!   `MERGE_HEAD`/`MERGE_MSG` plus the conflicted index and worktree markers, then
//!   `Automatic merge failed; fix conflicts and then commit the result.` (exit 1).
//! * `--abort` / `--quit`: `--quit` drops the in-progress merge state files;
//!   `--abort` additionally restores the index and the merge-affected worktree
//!   paths to `HEAD`, as `git reset --merge` does.
//!
//! Also served, as faithful ports of git's behaviour:
//!
//! * `--squash`/`--no-squash`: fold the merge into the worktree/index without a
//!   commit or ref move, writing `SQUASH_MSG` (a port of `squash_message()`,
//!   including the `git log`-medium body).
//! * `--commit`/`--no-commit`: `--no-commit` records `MERGE_HEAD`/`MERGE_MODE`/
//!   `MERGE_MSG` and stops with `Automatic merge went well; stopped before
//!   committing as requested`, leaving `git commit` (or `--continue`) to finish.
//! * `--continue`: finalize a resolved, staged in-progress merge.
//! * `-s ours` (and `-s ort`/`octopus`): `ours` records every head as a parent
//!   but keeps our tree verbatim.
//! * `--allow-unrelated-histories`: merge with an empty base tree; without it,
//!   `fatal: refusing to merge unrelated histories` (exit 128).
//! * `--signoff`, `-F`/`--file`, `--cleanup=<mode>`, `-q`/`--quiet`,
//!   `-v`/`--verbose`, and `--no-verify` (bypassing the `pre-merge-commit` and
//!   `commit-msg` hooks).
//! * `--log[=<n>]`/`--no-log` (and its `merge.log`/`merge.summary` defaults): the
//!   `* <origin>:` shortlog of every merged head folded into the merge message,
//!   a port of `fmt_merge_msg()`'s shortlog loop — including the
//!   `: (<n> commits)` header plus trailing `...` when more commits were merged
//!   than listed, the `merge.branchdesc` branch description, and the `By`/`Via`
//!   credit lines (which git emits only under `--edit`).
//! * `--compact-summary`/`--no-compact-summary` and `merge.stat`/`merge.diffstat`
//!   (`false`, `true`, `compact`): git's `show_diffstat` tri-state. The compact
//!   form drops the `create mode`/`delete mode` summary block and folds it into
//!   each diffstat name as ` (new)`, ` (new +x)`, ` (new +l)`, ` (gone)`,
//!   ` (mode +x)`, ` (mode -x)`, ` (mode +l)`, ` (mode -l)` — a port of
//!   `get_compact_summary()`. `--no-compact-summary` suppresses the diffstat
//!   entirely, as `option_parse_compact_summary()`'s `unset` does.
//! * `-e`/`--edit`/`--no-edit`, resolved through `default_edit_option()`: the
//!   merge message is written to `MERGE_MSG` with git's commented editor block
//!   below it (the scissors variant under `--cleanup=scissors`), opened with the
//!   `GIT_EDITOR` → `core.editor` → `$VISUAL` → `$EDITOR` → `vi` chain (`:` is
//!   the no-op editor), and the edited text is then stripped of comment lines,
//!   since an edited message defaults to `COMMIT_MSG_CLEANUP_ALL`. A failing
//!   editor or an empty message aborts with git's `Not committing merge; use
//!   'git commit' to complete the merge.` and leaves the merge in progress.
//! * `--autostash`/`--no-autostash` and `merge.autoStash`: a dirty worktree is
//!   snapshotted into a stash-like commit parked under the `MERGE_AUTOSTASH` ref
//!   (`Created autostash: <id>`), the merge runs against the clean tree, and the
//!   changes are re-applied afterwards (`Applied autostash.`). A merge that stops
//!   early — conflict, `--squash`, `--no-commit` — leaves the ref in place and
//!   prints git's ``When finished, apply stashed changes with `git stash pop` ``.
//! * `-S`/`--gpg-sign[=<keyid>]`/`--no-gpg-sign` and `commit.gpgsign`: the merge
//!   commit is signed through `gpg.program` with `-S<keyid>`, else
//!   `user.signingKey`, else the committer identity (git's `get_signing_key()`),
//!   and the armored signature is carried as the commit's `gpgsig` header. A gpg
//!   failure reproduces git's `error: gpg failed to sign the data:` followed by
//!   gpg's own diagnostics and `fatal: failed to write commit object`.
//! * `--progress`/`--no-progress`: accepted and inert. In `builtin/merge.c`
//!   `show_progress` has exactly one consumer, `o.show_rename_progress`, which
//!   forces merge-ort's delayed "Performing inexact rename detection" meter;
//!   this build's merge runs no such meter, so neither spelling can change a
//!   byte of its output.
//! * `commit.cleanup` — the same `cleanup_arg` `--cleanup` sets.
//! * `-m`/`--message` accumulation: repeated `-m` values are joined into
//!   paragraphs by a blank line (a port of `option_parse_message`), so
//!   `-m a -m b` produces `a\n\nb`.
//! * The value-clearing negations `--no-message` (empty the accumulated
//!   message), `--no-into-name` (restore the real target destination) and
//!   `--no-cleanup` (back to the default `whitespace` mode) — ports of git's
//!   OPT_STRING/OPT_CLEANUP `unset` behaviour.
//! * `--into-name <name>`: a port of git's `into_name` — override the merge
//!   message's destination (the ` into <name>` title and the
//!   `merge.suppressDest` test), rather than the real current branch.
//! * The default-matching negations `--no-rerere-autoupdate`, `--no-strategy`
//!   (git's `option_parse_strategy` no-ops on `unset`), and
//!   `--overwrite-ignore`, accepted as no-ops: each names behaviour this build
//!   already performs (no rerere, ignored files overwritten), so passing them
//!   reproduces stock git rather than erroring.
//! * `--[no-]verify-signatures` and `merge.verifySignatures`: a port of
//!   `verify_merge_signature()`, run over the heads left after the
//!   already-reachable ones are dropped, with `gpg.minTrustLevel` deciding
//!   whether git applies its own `TRUST_MARGINAL` floor on top.
//!
//! What is refused or deferred rather than faked:
//!
//! * `-s recursive`/`resolve`/`subtree`: distinct conflict-resolution engines
//!   that are not vendored, refused rather than aliased onto `ort`.
//! * `-X`/`--strategy-option`: the strategy options (`ours`, `theirs`,
//!   `ignore-space-change`, `diff-algorithm=`, `renormalize`, `find-renames=`)
//!   have to reach the blob/tree merge itself, and the shared
//!   `merge_apply::three_way_merge` takes no options — it builds
//!   `Repository::tree_merge_options()` internally. Accepting `-X` here would
//!   silently ignore it, so it stays rejected.
//! * `--rerere-autoupdate`: rerere's *recording* half is not ported (see
//!   `rerere.rs`, whose record/forget paths `bail!` rather than guess a conflict
//!   id without `ll_merge()`), so there is nothing to auto-stage.
//! * `--no-overwrite-ignore`: needs gitignore-aware checkout.
//!
//! Known fidelity gaps, stated rather than hidden: the diffstat is computed
//! with rename detection off, while `git merge` enables it, so a merge that
//! renames a file reports it as a delete plus a create instead of a `rename`
//! summary line; diffstat column widths measure Unicode scalar values rather
//! than terminal columns; `--verbose`'s extra stderr diagnostics are not
//! emitted; a `pre-merge-commit` hook that edits the index is not reflected
//! in the committed tree (the pre-computed merge tree is committed); the
//! `prepare-commit-msg` hook is not run before the editor; `default_edit_option`
//! tests that stdin and stdout are both terminals rather than that they are the
//! same file; `--signoff` adds its trailer before the `--edit` comment block
//! rather than through `ignored_log_message_bytes()`; and
//! `Automatic merge went well; stopped before committing as requested` is
//! printed on stdout where git uses stderr.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stage, Stat};
use gix::object::tree::{diff::Action, diff::Change as TreeChange, EntryKind};
use gix::objs::WriteTo;
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// git's `DEFAULT_MERGE_LOG_LEN` — how many shortlog entries a valueless
/// `--log` (or `merge.log = true`) asks for.
const DEFAULT_MERGE_LOG_LEN: i64 = 20;

/// The mutually exclusive top-level modes of `git merge`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Merge,
    Abort,
    Quit,
    Continue,
}

/// How the fast-forward question is answered, mirroring git's `fast_forward`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ff {
    Allow,
    Never,
    Only,
}

/// The merge strategy selected by `-s`/`--strategy`. Only the strategies the
/// vendored primitives implement byte-for-byte are represented; the remaining
/// git strategies (`recursive`, `resolve`, `subtree`) are refused rather than
/// aliased onto `ort`, since their conflict resolution genuinely differs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Strategy {
    /// The default three-way merge (git's `ort`).
    Ort,
    /// `-s ours`: record every head as a parent but keep our tree verbatim.
    Ours,
}

/// `--cleanup=<mode>` — how the commit message is stripped, a port of git's
/// `cleanup_mode` / `strbuf_stripspace`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    /// `whitespace` when a message is supplied without an editor (merge's default).
    Default,
    Verbatim,
    Whitespace,
    Strip,
    Scissors,
}

/// git's `show_diffstat` tri-state (`MERGE_SHOW_DIFFSTAT` /
/// `MERGE_SHOW_COMPACTSUMMARY` / off), driven by `--stat`/`--no-stat`,
/// `--compact-summary`/`--no-compact-summary` and `merge.stat`/`merge.diffstat`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatMode {
    /// `show_diffstat == 0`: nothing is printed after the merge.
    None,
    /// `MERGE_SHOW_DIFFSTAT`: the diffstat plus the `create mode`/`delete mode`
    /// summary block (`DIFF_FORMAT_DIFFSTAT | DIFF_FORMAT_SUMMARY`).
    Diffstat,
    /// `MERGE_SHOW_COMPACTSUMMARY`: the diffstat alone, with the summary folded
    /// into each name as ` (new)`/` (gone)`/` (mode +x)`… (`stat_with_summary`).
    CompactSummary,
}

/// Everything the argument loop gathers for a real merge, so the merge helpers
/// take one struct rather than a growing parameter list.
struct Opts {
    ff: Ff,
    /// Whether `--no-ff` was passed explicitly (needed for the `--squash`
    /// incompatibility check, which git keys off the literal flag).
    no_ff_given: bool,
    stat: StatMode,
    /// `-m`/`--message` or `-F`/`--file` contents (the latter read eagerly).
    message: Option<String>,
    squash: bool,
    /// `--commit`/`--no-commit` as given; `None` leaves the default (`!squash`).
    commit: Option<bool>,
    /// `--commit` was given explicitly (for the `--squash` incompatibility check).
    commit_given: bool,
    signoff: bool,
    /// `--verify-signatures` / `--no-verify-signatures`; `None` defers to
    /// `merge.verifySignatures`.
    verify_signatures: Option<bool>,
    allow_unrelated: bool,
    no_verify: bool,
    quiet: bool,
    cleanup: Cleanup,
    strategy: Strategy,
    /// `--into-name <name>`: use `<name>` instead of the real target branch when
    /// composing the merge message's ` into <name>` title (a port of git's
    /// `into_name`, which overrides `current_branch` in `fmt_merge_msg`).
    into_name: Option<String>,
    /// `--log[=<n>]`/`--no-log`, seeded from `merge.log`/`merge.summary`: how
    /// many shortlog entries to fold into the merge message. git keeps the count
    /// signed, so a negative `--log=<n>` lists nothing but still emits the block.
    log_len: i64,
    /// `merge.branchdesc` — whether `branch.<name>.description` is spliced into
    /// a merged local branch's shortlog block.
    branch_desc: bool,
    /// `-e`/`--edit`/`--no-edit`; `None` leaves git's `default_edit_option()` to
    /// decide (no message given and stdin/stdout are the same terminal).
    edit: Option<bool>,
    /// `--autostash`/`--no-autostash` and `merge.autoStash`.
    autostash: bool,
    /// `-S`/`--gpg-sign[=<keyid>]`/`--no-gpg-sign` and `commit.gpgsign`:
    /// `Some(key)` signs the merge commit, an empty key deferring to
    /// `user.signingKey` (and, failing that, the committer identity, as git's
    /// `get_signing_key()` does).
    sign: Option<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            ff: Ff::Allow,
            no_ff_given: false,
            stat: StatMode::Diffstat,
            message: None,
            squash: false,
            commit: None,
            commit_given: false,
            signoff: false,
            verify_signatures: None,
            allow_unrelated: false,
            no_verify: false,
            quiet: false,
            cleanup: Cleanup::Default,
            strategy: Strategy::Ort,
            into_name: None,
            log_len: -1,
            branch_desc: false,
            edit: None,
            autostash: false,
            sign: None,
        }
    }
}

pub fn merge(args: &[String]) -> Result<ExitCode> {
    let mut op = Op::Merge;
    let mut opts = Opts::default();
    let mut refs: Vec<String> = Vec::new();
    // A pending `-F`/`--file` read, resolved after parsing so the diagnostic
    // order matches git (options first, file open second).
    let mut file: Option<String> = None;
    // git's `merge_log_config`, applied only after the options are parsed: a
    // `--log=<n>` that leaves the count negative falls back to it.
    let mut merge_log_config: i64 = 0;

    // The `git_merge_config()` defaults, applied before the CLI options below
    // override them. merge.suppressDest is consulted later, in `dest_suppressed`,
    // when the default merge message's title is composed.
    if let Ok(repo) = gix::discover(".") {
        let snap = repo.config_snapshot();
        match snap.string("merge.ff").map(|v| v.to_string().to_ascii_lowercase()).as_deref() {
            Some("only") => opts.ff = Ff::Only,
            Some("false" | "no" | "off" | "0") => opts.ff = Ff::Never,
            Some(_) => opts.ff = Ff::Allow, // true/yes/on/1/valueless → allow
            None => {}
        }
        if let Some(mode) = stat_config(&snap) {
            opts.stat = mode;
        }
        merge_log_config = shortlog_config(&snap);
        opts.branch_desc = snap.boolean("merge.branchdesc").unwrap_or(false);
        opts.autostash = snap.boolean("merge.autoStash").unwrap_or(false);
        // `commit.gpgsign` sets git's `sign_commit` to the empty key, meaning
        // "sign, letting `get_signing_key()` choose".
        if snap.boolean("commit.gpgsign") == Some(true) {
            opts.sign = Some(String::new());
        }
        // `commit.cleanup` feeds the same `cleanup_arg` `--cleanup` sets.
        if let Some(v) = snap.string("commit.cleanup") {
            match parse_cleanup(&v.to_string()) {
                Some(mode) => opts.cleanup = mode,
                None => {
                    eprintln!("fatal: Invalid cleanup mode {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--abort" => op = Op::Abort,
            "--quit" => op = Op::Quit,
            "--continue" => op = Op::Continue,
            "--ff" => opts.ff = Ff::Allow,
            "--no-ff" => {
                opts.ff = Ff::Never;
                opts.no_ff_given = true;
            }
            "--ff-only" => opts.ff = Ff::Only,
            "--stat" | "--summary" => opts.stat = StatMode::Diffstat,
            "--no-stat" | "--no-summary" | "-n" => opts.stat = StatMode::None,
            // `option_parse_compact_summary`: set → the compact summary, unset →
            // no diffstat at all (`show_diffstat = 0`), not "back to --stat".
            "--compact-summary" => opts.stat = StatMode::CompactSummary,
            "--no-compact-summary" => opts.stat = StatMode::None,
            "--squash" => opts.squash = true,
            "--no-squash" => opts.squash = false,
            "--commit" => {
                opts.commit = Some(true);
                opts.commit_given = true;
            }
            "--no-commit" => opts.commit = Some(false),
            "--signoff" => opts.signoff = true,
            "--no-signoff" => opts.signoff = false,
            "--allow-unrelated-histories" => opts.allow_unrelated = true,
            "--no-allow-unrelated-histories" => opts.allow_unrelated = false,
            "--no-verify" => opts.no_verify = true,
            "--verify" => opts.no_verify = false,
            // `--into-name <name>` / `--into-name=<name>`: override the merge
            // message's destination (port of git's `into_name`).
            "--into-name" => {
                i += 1;
                match args.get(i) {
                    Some(n) => opts.into_name = Some(n.clone()),
                    None => {
                        eprintln!("error: option `{a}' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if a.starts_with("--into-name=") => {
                opts.into_name = Some(a["--into-name=".len()..].to_string())
            }
            // `--no-into-name`: git's OPT_STRING negation sets `into_name` to
            // NULL, restoring the real target branch as the message destination.
            "--no-into-name" => opts.into_name = None,
            // `--log[=<n>]` is git's `OPT_INTEGER` with `PARSE_OPT_OPTARG` and a
            // default of DEFAULT_MERGE_LOG_LEN, so only the `=<n>` spelling takes a
            // value (`--log 5` leaves `5` as a head to merge). `--no-log` is 0.
            "--log" => opts.log_len = DEFAULT_MERGE_LOG_LEN,
            "--no-log" => opts.log_len = 0,
            _ if a.starts_with("--log=") => {
                let value = &a["--log=".len()..];
                match parse_option_int(value) {
                    Some(n) => opts.log_len = n,
                    // parse-options.c distinguishes the two failures: an empty
                    // argument never reaches `git_parse_int`.
                    None if value.is_empty() => {
                        eprintln!("error: option `log' expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                    None => {
                        eprintln!(
                            "error: option `log' expects an integer value with an optional k/m/g suffix"
                        );
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `--autostash`: stash the dirty worktree before the merge and restore
            // it afterwards (`create_autostash_ref`/`apply_autostash_ref`).
            "--autostash" => opts.autostash = true,
            "--no-autostash" => opts.autostash = false,
            // `-S`/`--gpg-sign[=<keyid>]`: an OPTARG option, so only the attached
            // spellings carry a key; `--no-gpg-sign` clears `sign_commit`.
            "-S" | "--gpg-sign" => opts.sign = Some(String::new()),
            "--no-gpg-sign" => opts.sign = None,
            _ if a.starts_with("--gpg-sign=") => {
                opts.sign = Some(a["--gpg-sign=".len()..].to_string())
            }
            _ if a.len() > 2 && a.starts_with("-S") && !a.starts_with("--") => {
                opts.sign = Some(a[2..].to_string())
            }
            // `--progress`/`--no-progress` force or suppress git's progress
            // meters. In `builtin/merge.c` `show_progress` feeds exactly one
            // consumer, `o.show_rename_progress`, which drives merge-ort's delayed
            // "Performing inexact rename detection" meter; this build's merge has
            // no such meter to force, so both spellings are accepted and change
            // nothing (see the module docs).
            "--progress" | "--no-progress" => {}
            // Flags whose git behaviour is already this build's default, accepted
            // as no-ops so they match stock git rather than erroring:
            //  * `--no-rerere-autoupdate`: no rerere machinery runs here anyway.
            //  * `--overwrite-ignore`: ignored files are overwritten (git's default).
            //  * `--no-strategy`: git's `option_parse_strategy` returns early on
            //    `unset` without clearing the strategy list, so it is a no-op that
            //    leaves any earlier `-s` in force (default `ort` when none given).
            "--no-rerere-autoupdate" | "--overwrite-ignore" | "--no-strategy" => {}
            // `--[no-]verify-signatures` / `merge.verifySignatures`: check every
            // head's signature before merging it. `None` leaves the config to
            // decide.
            "--verify-signatures" => opts.verify_signatures = Some(true),
            "--no-verify-signatures" => opts.verify_signatures = Some(false),
            // Verbosity: git keeps a signed level; only quiet has an observable
            // effect on stdout (it silences the summary/diffstat). `--verbose`'s
            // extra diagnostics go to stderr and are not reproduced.
            "-q" | "--quiet" => opts.quiet = true,
            "-v" | "--verbose" => opts.quiet = false,
            // `-e`/`--edit`/`--no-edit`: whether the merge message is opened in an
            // editor before the merge commit is written. Left `None` here so
            // `default_edit_option()` decides (see `edit_wanted`).
            "-e" | "--edit" => opts.edit = Some(true),
            "--no-edit" => opts.edit = Some(false),
            // `-m`/`--message` accumulate into one buffer, joined by a blank line
            // (git's `option_parse_message`: `buf->len ? "\n\n" : ""`), so
            // `-m a -m b` yields the two-paragraph message `a\n\nb`.
            "-m" | "--message" => {
                i += 1;
                match args.get(i) {
                    Some(m) => append_message(&mut opts.message, m),
                    None => {
                        eprintln!("error: option `{a}' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // `--no-message`: clear the accumulated message (git's
            // `option_parse_message` on `unset` does `strbuf_setlen(buf, 0)`).
            "--no-message" => opts.message = None,
            _ if a.starts_with("--message=") => {
                append_message(&mut opts.message, &a["--message=".len()..])
            }
            _ if a.len() > 2 && a.starts_with("-m") && !a.starts_with("--") => {
                append_message(&mut opts.message, &a[2..])
            }
            "-F" | "--file" => {
                i += 1;
                match args.get(i) {
                    Some(p) => file = Some(p.clone()),
                    None => {
                        eprintln!("error: option `{a}' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if a.starts_with("--file=") => file = Some(a["--file=".len()..].to_string()),
            _ if a.len() > 2 && a.starts_with("-F") && !a.starts_with("--") => {
                file = Some(a[2..].to_string())
            }
            "--cleanup" => {
                i += 1;
                match args.get(i).and_then(|v| parse_cleanup(v)) {
                    Some(mode) => opts.cleanup = mode,
                    None => {
                        let bad = args.get(i).map(String::as_str).unwrap_or("");
                        eprintln!("fatal: Invalid cleanup mode {bad}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            _ if a.starts_with("--cleanup=") => match parse_cleanup(&a["--cleanup=".len()..]) {
                Some(mode) => opts.cleanup = mode,
                None => {
                    eprintln!("fatal: Invalid cleanup mode {}", &a["--cleanup=".len()..]);
                    return Ok(ExitCode::from(128));
                }
            },
            // `--no-cleanup`: git's OPT_CLEANUP is an OPT_STRING, so the negation
            // sets `cleanup_arg` to NULL and `get_cleanup_mode(NULL, 0)` returns
            // the default (`whitespace` without an editor) — our `Cleanup::Default`.
            "--no-cleanup" => opts.cleanup = Cleanup::Default,
            "-s" | "--strategy" => {
                i += 1;
                match args.get(i).map(String::as_str).map(resolve_strategy) {
                    Some(Ok(s)) => opts.strategy = s,
                    Some(Err(code)) => return Ok(code),
                    None => {
                        eprintln!("error: option `{a}' requires a value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if a.starts_with("--strategy=") => match resolve_strategy(&a["--strategy=".len()..]) {
                Ok(s) => opts.strategy = s,
                Err(code) => return Ok(code),
            },
            _ if a.len() > 2 && a.starts_with("-s") && !a.starts_with("--") => {
                match resolve_strategy(&a[2..]) {
                    Ok(s) => opts.strategy = s,
                    Err(code) => return Ok(code),
                }
            }
            _ if a.len() > 1 && a.starts_with('-') => {
                anyhow::bail!("unsupported flag {a}")
            }
            _ => refs.push(a.to_string()),
        }
        i += 1;
    }

    // `if (shortlog_len < 0) shortlog_len = (merge_log_config > 0) ? … : 0;` —
    // an unset (or negative) `--log` count defers to `merge.log`/`merge.summary`,
    // and a negative config value means no shortlog at all.
    if opts.log_len < 0 {
        opts.log_len = if merge_log_config > 0 { merge_log_config } else { 0 };
    }

    // `-F <path>` — read now, after option parsing. `-` and an empty value are
    // stdin, matching git's `read_from_file`/`fix_filename`.
    if let Some(path) = file {
        let data = if path == "-" || path.is_empty() {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut buf)?;
            buf
        } else {
            match std::fs::read(&path) {
                Ok(buf) => buf,
                Err(e) => {
                    eprintln!("fatal: could not open '{path}' for reading: {}", strerror(&e));
                    return Ok(ExitCode::from(128));
                }
            }
        };
        opts.message = Some(String::from_utf8_lossy(&data).into_owned());
    }

    match op {
        // git: `--abort`/`--quit`/`--continue` expect no arguments.
        Op::Abort | Op::Quit | Op::Continue if !refs.is_empty() => {
            let which = match op {
                Op::Abort => "--abort",
                Op::Quit => "--quit",
                _ => "--continue",
            };
            eprintln!("fatal: {which} expects no arguments");
            Ok(ExitCode::from(129))
        }
        Op::Abort => abort(),
        Op::Quit => quit(),
        Op::Continue => continue_merge(&opts),
        Op::Merge => {
            // git's `builtin/merge.c` incompatibility checks, keyed off the literal
            // flags. `--squash` cannot fast-forward, so it clashes with `--no-ff`,
            // and it never commits, so it clashes with `--commit`.
            if opts.squash && opts.commit_given {
                eprintln!("fatal: options '--squash' and '--commit.' cannot be used together");
                return Ok(ExitCode::from(128));
            }
            if opts.squash && opts.no_ff_given {
                eprintln!("fatal: options '--squash' and '--no-ff.' cannot be used together");
                return Ok(ExitCode::from(128));
            }
            do_merge(&refs, &opts)
        }
    }
}

// ---------------------------------------------------------------------------
// --abort / --quit
// ---------------------------------------------------------------------------

/// The state files `remove_merge_branch_state()` (branch.c) unlinks.
const MERGE_STATE_FILES: &[&str] = &["MERGE_HEAD", "MERGE_RR", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"];

/// The extra state `remove_branch_state()` unlinks on top of the merge state;
/// `git merge --abort` reaches it by running `git reset --merge`.
const BRANCH_STATE_FILES: &[&str] = &["SQUASH_MSG", "CHERRY_PICK_HEAD", "REVERT_HEAD"];

fn remove_merge_state(git_dir: &Path, and_branch_state: bool) {
    for name in MERGE_STATE_FILES {
        let _ = std::fs::remove_file(git_dir.join(name));
    }
    if and_branch_state {
        for name in BRANCH_STATE_FILES {
            let _ = std::fs::remove_file(git_dir.join(name));
        }
        let _ = std::fs::remove_dir_all(git_dir.join("sequencer"));
    }
}

/// `git merge --quit`: forget the in-progress merge, leaving index and worktree
/// exactly as they are.
fn quit() -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    remove_merge_state(repo.git_dir(), false);
    Ok(ExitCode::SUCCESS)
}

/// `git merge --abort`: `git reset --merge` plus dropping the merge state.
///
/// The reset is confined to the paths the merge touched — every path that has a
/// conflicted stage, or whose index entry disagrees with `HEAD` — so unrelated
/// local modifications and untracked files survive, as they do under git.
fn abort() -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    if !repo.git_dir().join("MERGE_HEAD").exists() {
        eprintln!("fatal: There is no merge to abort (MERGE_HEAD missing).");
        return Ok(ExitCode::from(128));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let head = repo.head()?;
    let head_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    let head_tree = repo.find_object(head_id)?.peel_to_tree()?.id;

    let old_index = repo.index_or_load_from_head()?.into_owned();
    let should_interrupt = AtomicBool::new(false);
    update_worktree(&repo, &old_index, head_tree, &should_interrupt)?;

    // git's `reset_refs()` records the pre-reset HEAD in ORIG_HEAD.
    set_orig_head(&repo, head_id)?;
    remove_merge_state(repo.git_dir(), true);

    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

/// Port of `verify_merge_signature()` (commit.c). Returns `Some(exit)` for each
/// `die()` — a bad signature, a good-but-untrusted one, or no signature at all —
/// and prints the `has a good GPG signature by` line on the accepting path
/// unless `-q` lowered the verbosity below zero.
///
/// git quotes gpg's *signer name* (`sigc->signer`, the text after the key id on
/// the `GOODSIG`/`BADSIG` status line), not the key id, hence
/// [`crate::gitsig::evaluate_full`].
fn verify_merge_signature(
    repo: &gix::Repository,
    id: ObjectId,
    quiet: bool,
    check_trust: bool,
) -> Result<Option<ExitCode>> {
    use crate::gitsig::{GStatus, Trust};

    let hex = id.attach(repo).shorten_or_id().to_string();
    let raw = repo.find_object(id)?.data.clone();
    let sig = crate::gitsig::evaluate_full(&raw);

    // The C switches on `sigc->result`, and every case but 'G' and 'B' — including
    // the untrusted/expired/revoked codes — falls into the same `default: /* 'N' */`
    // arm, so an unverifiable signature reports as an absent one.
    match sig.status {
        GStatus::Good | GStatus::GoodUnknown => {
            if check_trust && sig.trust < Trust::Marginal {
                eprintln!(
                    "fatal: Commit {hex} has an untrusted GPG signature, allegedly by {}.",
                    sig.signer
                );
                return Ok(Some(ExitCode::from(128)));
            }
        }
        GStatus::Bad => {
            eprintln!(
                "fatal: Commit {hex} has a bad GPG signature allegedly by {}.",
                sig.signer
            );
            return Ok(Some(ExitCode::from(128)));
        }
        _ => {
            eprintln!("fatal: Commit {hex} does not have a GPG signature.");
            return Ok(Some(ExitCode::from(128)));
        }
    }
    if !quiet {
        println!("Commit {hex} has a good GPG signature by {}", sig.signer);
    }
    Ok(None)
}

/// `merge_options.verbosity`, as `init_merge_options()` resolves it: the
/// built-in default of 2, overridden by `merge.verbosity`, then by the
/// `GIT_MERGE_VERBOSITY` environment variable (`strtol`, so trailing garbage is
/// ignored and an unparsable value reads as 0). `merge-ort-wrappers.c` turns it
/// into `show_msgs = !!verbosity`, which is the only part of the scale this
/// build's output surface can express: levels 1–5 all print the same
/// `Auto-merging` / `CONFLICT (…)` block, level 0 prints none of it.
fn merge_verbosity(repo: &gix::Repository) -> i64 {
    if let Ok(env) = std::env::var("GIT_MERGE_VERBOSITY") {
        let digits = env.trim_start();
        let end = digits
            .char_indices()
            .position(|(i, c)| !(c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+'))))
            .unwrap_or(digits.len());
        return digits[..end].parse().unwrap_or(0);
    }
    repo.config_snapshot().integer("merge.verbosity").unwrap_or(2)
}

/// Port of `setup_with_upstream()`: with no commit on the command line and
/// `merge.defaultToUpstream` on, `git merge` merges the current branch's
/// configured upstream — `branch.<name>.merge` mapped through
/// `remote.<remote>.fetch` to its remote-tracking ref. `Err` carries git's
/// `die()` text (without the `fatal: ` prefix) for each way that can fail.
fn setup_with_upstream(repo: &gix::Repository) -> std::result::Result<Vec<String>, String> {
    use gix::bstr::ByteSlice;

    let head = repo.head_ref().ok().flatten();
    let full = head
        .as_ref()
        .map(|r| r.name().to_owned())
        .ok_or_else(|| "No current branch.".to_string())?;
    let name = full.shorten().to_owned();

    let snap = repo.config_snapshot();
    if snap.string(format!("branch.{name}.remote").as_str()).is_none() {
        return Err("No remote for the current branch.".into());
    }
    // `branch->merge_nr`: the `branch.<name>.merge` entries, in config order.
    let sources: Vec<gix::bstr::BString> = snap
        .plumbing()
        .values::<gix::bstr::BString>(format!("branch.{name}.merge").as_str())
        .unwrap_or_default();
    if sources.is_empty() {
        return Err("No default upstream defined for the current branch.".into());
    }

    // `branch->merge[i]->dst`, i.e. the remote-tracking ref the refspec maps the
    // source to. gix resolves the whole `branch.<name>.{remote,merge}` pair at
    // once, so a single source (git's overwhelmingly common case) is looked up
    // directly; git reports the unmapped ones by name.
    match repo.branch_remote_tracking_ref_name(full.as_ref(), gix::remote::Direction::Fetch) {
        Some(Ok(dst)) if sources.len() == 1 => Ok(vec![dst.as_bstr().to_string()]),
        _ => {
            let remote = snap
                .string(format!("branch.{name}.remote").as_str())
                .map(|v| v.to_str_lossy().into_owned())
                .unwrap_or_default();
            Err(format!(
                "No remote-tracking branch for {} from {remote}",
                sources[0]
            ))
        }
    }
}

fn do_merge(refs: &[String], opts: &Opts) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    if repo.git_dir().join("MERGE_HEAD").exists() {
        eprintln!("fatal: You have not concluded your merge (MERGE_HEAD exists).");
        eprintln!("Please, commit your changes before you merge.");
        return Ok(ExitCode::from(128));
    }

    // `if (!argc) { if (default_to_upstream) argc = setup_with_upstream(&argv);
    // else die(...); }` — the sole reader of `merge.defaultToUpstream`, which
    // git defaults to true.
    let upstream_refs;
    let refs: &[String] = if refs.is_empty() {
        if repo.config_snapshot().boolean("merge.defaultToUpstream") == Some(false) {
            eprintln!("fatal: No commit specified and merge.defaultToUpstream not set.");
            return Ok(ExitCode::from(128));
        }
        match setup_with_upstream(&repo) {
            Ok(v) => {
                upstream_refs = v;
                &upstream_refs
            }
            Err(msg) => {
                eprintln!("fatal: {msg}");
                return Ok(ExitCode::from(128));
            }
        }
    } else {
        refs
    };

    // Current HEAD state. An unborn branch has no commit to fast-forward from;
    // a real merge into it would be a checkout, which is out of scope.
    let head = repo.head()?;
    if head.is_unborn() {
        anyhow::bail!("cannot merge into an unborn branch");
    }
    let local_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    // Owned branch name when attached; `None` when detached.
    let branch: Option<FullName> = head.referent_name().map(std::borrow::ToOwned::to_owned);
    // The ref to move: the attached branch, or HEAD itself when detached. Both
    // are direct (non-symbolic) refs here, so `deref` is false either way.
    let name: FullName = match &branch {
        Some(b) => b.clone(),
        None => "HEAD"
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
    };

    // Resolve every ref to merge and peel it to a commit (tags included).
    let mut targets: Vec<ObjectId> = Vec::with_capacity(refs.len());
    for spec in refs {
        let id = repo.rev_parse_single(spec.as_str())?.object()?.peel_to_commit()?.id;
        targets.push(id);
    }

    // `--verify-signatures` / `merge.verifySignatures`. git runs this over the
    // heads `collect_parents()` kept, and that function has already dropped every
    // head reachable from HEAD — which is why `Already up to date.` preempts the
    // check while a fast-forward does not.
    if opts.verify_signatures.unwrap_or_else(|| {
        repo.config_snapshot().boolean("merge.verifySignatures") == Some(true)
    }) {
        // `gpg.minTrustLevel` moves the floor into `check_signature()` itself, so
        // git clears its own `TRUST_MARGINAL` test when the key is configured.
        let check_trust = repo.config_snapshot().string("gpg.minTrustLevel").is_none();
        for id in &targets {
            let reachable = repo
                .merge_bases_many(local_id, &[*id])?
                .iter()
                .any(|b| b.detach() == *id);
            if reachable {
                continue;
            }
            if let Some(code) = verify_merge_signature(&repo, *id, opts.quiet, check_trust)? {
                return Ok(code);
            }
        }
    }

    // `-s ours`: every head becomes a parent while our tree is kept verbatim.
    // Handles any number of heads and never fast-forwards.
    if opts.strategy == Strategy::Ours {
        return merge_ours(&repo, name, branch.as_ref(), local_id, &targets, refs, opts);
    }

    // More than one head, default strategy → octopus.
    if refs.len() > 1 {
        return do_octopus(&repo, refs, &targets, local_id, branch.as_ref(), name, opts);
    }

    let spec = refs[0].as_str();
    let target_id = targets[0];

    // merge-base analysis. An empty set of merge bases means unrelated histories,
    // which git refuses without `--allow-unrelated-histories`.
    let bases = repo.merge_bases_many(local_id, &[target_id])?;
    if bases.is_empty() && !opts.allow_unrelated {
        eprintln!("fatal: refusing to merge unrelated histories");
        return Ok(ExitCode::from(128));
    }
    if bases.iter().any(|b| b.detach() == target_id) {
        // Target already reachable from HEAD (or identical). git checks this
        // before it consults --no-ff, so --no-ff does not force a commit here.
        if !opts.quiet {
            println!("Already up to date.");
        }
        return Ok(ExitCode::SUCCESS);
    }
    // Fast-forwardable exactly when HEAD is one of the merge bases.
    let diverged = !bases.iter().any(|b| b.detach() == local_id);
    if diverged && opts.ff == Ff::Only {
        eprintln!("fatal: Not possible to fast-forward, aborting.");
        return Ok(ExitCode::from(128));
    }

    // From here on we mutate a ref, the index and the worktree. Serialize the
    // whole read-modify-write through the repo coordinator (a no-op if no
    // daemon is running), matching the zsync/zbump write path.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // `--autostash`: snapshot and reset the dirty worktree before anything is
    // touched, so the merge runs against a clean tree and the local changes come
    // back at the end (or stay recoverable in MERGE_AUTOSTASH if it stops).
    let stash = begin_autostash(&repo, opts)?;

    // Never clobber uncommitted work.
    if repo.is_dirty()? {
        anyhow::bail!("worktree has uncommitted changes; refusing to merge");
    }

    let old_index = repo.index_or_load_from_head()?.into_owned();
    let head_tree = repo.find_object(local_id)?.peel_to_tree()?.id;
    let target_tree = repo.find_object(target_id)?.peel_to_tree()?.id;
    let should_interrupt = AtomicBool::new(false);
    let message = compose_message(&repo, refs, &targets, branch.as_ref(), local_id, opts)?;

    // Diverged histories: a genuine three-way merge (`ort` strategy) of HEAD and
    // the target against their merge base (an empty tree for unrelated histories).
    // On a clean merge the finish step commits/squashes/records per the options;
    // on conflict we record MERGE_HEAD/MERGE_MSG and stop, exactly as git does.
    if diverged {
        // `git`'s recursive base for the three-way; the empty tree stands in for an
        // unrelated history (`--allow-unrelated-histories`), which has no base.
        let base_tree = if bases.is_empty() {
            gix::ObjectId::empty_tree(repo.object_hash())
        } else {
            let base = repo.merge_base(local_id, target_id)?.detach();
            repo.find_object(base)?.peel_to_tree()?.id
        };
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new(b"merged common ancestors")),
            current: Some(BStr::new(b"HEAD")),
            other: Some(BStr::new(spec.as_bytes())),
        };
        let applied = crate::merge_apply::three_way_merge_verbose(
            &repo,
            base_tree,
            head_tree,
            target_tree,
            &old_index,
            labels,
            &should_interrupt,
            merge_verbosity(&repo) != 0,
        )?;
        let mut index = applied.index;
        index.write(Default::default())?;

        if applied.conflicts.is_empty() {
            return finalize_clean(
                &repo,
                name,
                local_id,
                &[target_id],
                message,
                applied.tree_id,
                head_tree,
                opts,
                "ort",
                spec,
                stash,
            );
        }

        // Conflicts: record the in-progress merge and stop with git's message.
        set_orig_head(&repo, local_id)?;
        let git_dir = repo.git_dir();
        std::fs::write(git_dir.join("MERGE_HEAD"), format!("{target_id}\n"))?;
        std::fs::write(git_dir.join("MERGE_MODE"), merge_mode(opts.ff))?;
        let mut merge_msg = message.into_bytes();
        merge_msg.extend_from_slice(b"\n# Conflicts:\n");
        for path in &applied.conflicts {
            merge_msg.extend_from_slice(b"#\t");
            merge_msg.extend_from_slice(&path[..]);
            merge_msg.push(b'\n');
        }
        std::fs::write(git_dir.join("MERGE_MSG"), &merge_msg)?;
        if !opts.quiet {
            println!("Automatic merge failed; fix conflicts and then commit the result.");
        }
        end_autostash(&repo, stash, false)?;
        return Ok(ExitCode::from(1));
    }

    // `--no-ff` over a fast-forwardable history: the merge-base is our own commit,
    // so a three-way merge of every path resolves to theirs — the merged tree is
    // exactly the target's tree. Sync the worktree, then finish as a merge commit.
    if opts.ff == Ff::Never {
        update_worktree(&repo, &old_index, target_tree, &should_interrupt)?;
        return finalize_clean(
            &repo,
            name,
            local_id,
            &[target_id],
            message,
            target_tree,
            head_tree,
            opts,
            "ort",
            spec,
            stash,
        );
    }

    // Pure fast-forward territory. `--squash` fast-forwards the *content* but does
    // not move the ref: git updates the worktree, prints the fast-forward summary,
    // then the squash notice and writes SQUASH_MSG.
    if opts.squash {
        update_worktree(&repo, &old_index, target_tree, &should_interrupt)?;
        if !opts.quiet {
            println!(
                "Updating {}..{}",
                local_id.to_hex_with_len(7),
                target_id.to_hex_with_len(7)
            );
            println!("Fast-forward");
            print!("{}", diffstat(&repo, head_tree, target_tree, opts.stat)?);
        }
        write_squash_msg(&repo, &[target_id], local_id)?;
        if !opts.quiet {
            println!("Squash commit -- not updating HEAD");
        }
        end_autostash(&repo, stash, false)?;
        return Ok(ExitCode::SUCCESS);
    }

    // Normal fast-forward. `--no-commit` does not stop a fast-forward (there is no
    // merge commit to stop before), matching git.
    set_orig_head(&repo, local_id)?;
    advance(&repo, name, local_id, target_id, format!("merge {spec}: Fast-forward"))?;
    update_worktree(&repo, &old_index, target_tree, &should_interrupt)?;
    if !opts.quiet {
        println!(
            "Updating {}..{}",
            local_id.to_hex_with_len(7),
            target_id.to_hex_with_len(7)
        );
        println!("Fast-forward");
        print!("{}", diffstat(&repo, head_tree, target_tree, opts.stat)?);
    }
    end_autostash(&repo, stash, true)?;
    Ok(ExitCode::SUCCESS)
}

/// The clean-merge finish shared by the diverged, `--no-ff`, and `-s ours` paths:
/// records `ORIG_HEAD`, then squashes, stops before committing, or writes the
/// merge commit, honouring `--signoff`, `--cleanup`, `--no-verify` and `--quiet`.
/// `merged_tree` is the already-computed result tree (its worktree/index are
/// assumed synced by the caller); `head_tree` feeds the diffstat.
#[allow(clippy::too_many_arguments)]
fn finalize_clean(
    repo: &gix::Repository,
    name: FullName,
    local_id: ObjectId,
    targets: &[ObjectId],
    message: String,
    merged_tree: ObjectId,
    head_tree: ObjectId,
    opts: &Opts,
    strategy_name: &str,
    spec_label: &str,
    stash: Option<ObjectId>,
) -> Result<ExitCode> {
    set_orig_head(repo, local_id)?;
    let do_commit = opts.commit.unwrap_or(!opts.squash);

    // `--squash`: no commit, no ref move, no MERGE_HEAD — just SQUASH_MSG.
    if opts.squash {
        if !opts.quiet {
            println!("Automatic merge went well; stopped before committing as requested");
        }
        write_squash_msg(repo, targets, local_id)?;
        if !opts.quiet {
            println!("Squash commit -- not updating HEAD");
        }
        end_autostash(repo, stash, false)?;
        return Ok(ExitCode::SUCCESS);
    }

    // `--no-commit`: leave the merge in progress for `git commit` to finalize.
    if !do_commit {
        let git_dir = repo.git_dir();
        write_merge_heads(repo, targets, opts.ff)?;
        std::fs::write(git_dir.join("MERGE_MSG"), &message)?;
        if !opts.quiet {
            println!("Automatic merge went well; stopped before committing as requested");
        }
        end_autostash(repo, stash, false)?;
        return Ok(ExitCode::SUCCESS);
    }

    // `pre-merge-commit` runs before the commit; a non-zero exit vetoes it. The
    // hook's own output (inherited on stderr) is the whole diagnostic, as in git.
    if !opts.no_verify && !crate::hooks::run(repo, "pre-merge-commit", &[], None)? {
        return Ok(ExitCode::from(1));
    }

    // git's `prepare_to_commit()` from here: build the buffer, persist the merge
    // state, run the editor and the `commit-msg` hook over that file, then clean
    // the message up and refuse an empty one.
    let edit = match edit_wanted(opts) {
        Ok(edit) => edit,
        Err(code) => return Ok(code),
    };
    let comment = comment_char(repo);
    let mut msg = message;
    if opts.signoff {
        append_signoff(repo, &mut msg)?;
    }
    if edit {
        append_editor_comment(&mut msg, opts.cleanup, &comment);
    }

    let git_dir = repo.git_dir();
    write_merge_heads(repo, targets, opts.ff)?;
    let msg_path = git_dir.join("MERGE_MSG");
    std::fs::write(&msg_path, &msg)?;

    if edit && !launch_editor(repo, &msg_path)? {
        eprintln!("Not committing merge; use 'git commit' to complete the merge.");
        return Ok(ExitCode::from(1));
    }
    // `commit-msg` gets the same file and may rewrite it.
    if !opts.no_verify {
        let arg = msg_path.to_string_lossy().into_owned();
        if !crate::hooks::run(repo, "commit-msg", &[&arg], None)? {
            return Ok(ExitCode::from(1));
        }
    }
    msg = std::fs::read_to_string(&msg_path)?;
    // `get_cleanup_mode(cleanup_arg, 0 < option_edit)`: with no explicit
    // `--cleanup`/`commit.cleanup`, an edited message is stripped of its comment
    // lines (`COMMIT_MSG_CLEANUP_ALL`) while an unedited one only loses
    // whitespace (`COMMIT_MSG_CLEANUP_SPACE`).
    let cleanup = match (opts.cleanup, edit) {
        (Cleanup::Default, true) => Cleanup::Strip,
        (mode, _) => mode,
    };
    msg = cleanup_message(&msg, cleanup, &comment);
    if msg.is_empty() {
        eprintln!("error: Empty commit message.");
        eprintln!("Not committing merge; use 'git commit' to complete the merge.");
        return Ok(ExitCode::from(1));
    }

    let author = repo
        .author()
        .ok_or_else(|| anyhow::anyhow!("author identity is not configured"))??;
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let mut parents: Vec<ObjectId> = Vec::with_capacity(targets.len() + 1);
    parents.push(local_id);
    parents.extend_from_slice(targets);
    let mut commit = gix::objs::Commit {
        message: msg.into(),
        tree: merged_tree,
        author: author.to_owned()?,
        committer: committer.to_owned()?,
        encoding: None,
        parents: parents.into_iter().collect(),
        extra_headers: Default::default(),
    };
    // `-S`/`--gpg-sign`/`commit.gpgsign`: sign the serialized commit and carry the
    // armored signature as the `gpgsig` header.
    if let Some(key) = &opts.sign {
        if let Err(code) = sign_commit(repo, &mut commit, key) {
            return Ok(code);
        }
    }
    let new_id = repo.write_object(&commit)?.detach();
    advance(
        repo,
        name,
        local_id,
        new_id,
        format!("merge {spec_label}: Merge made by the '{strategy_name}' strategy."),
    )?;
    // git's `finish()` → `remove_merge_branch_state()`: the merge is over.
    remove_merge_state(git_dir, false);
    if !opts.quiet {
        println!("Merge made by the '{strategy_name}' strategy.");
        print!("{}", diffstat(repo, head_tree, merged_tree, opts.stat)?);
    }
    end_autostash(repo, stash, true)?;
    Ok(ExitCode::SUCCESS)
}

/// git's `write_merge_heads()`: every merged head in `MERGE_HEAD`, plus the
/// `MERGE_MODE` marker that records whether the merge may fast-forward.
fn write_merge_heads(repo: &gix::Repository, targets: &[ObjectId], ff: Ff) -> Result<()> {
    let git_dir = repo.git_dir();
    let mut merge_head = String::new();
    for t in targets {
        merge_head.push_str(&format!("{t}\n"));
    }
    std::fs::write(git_dir.join("MERGE_HEAD"), merge_head)?;
    std::fs::write(git_dir.join("MERGE_MODE"), merge_mode(ff))?;
    Ok(())
}

/// `-s ours`: record every head as a parent, keep our tree verbatim. Never
/// fast-forwards; already up to date only when every head is reachable from HEAD.
fn merge_ours(
    repo: &gix::Repository,
    name: FullName,
    branch: Option<&FullName>,
    local_id: ObjectId,
    targets: &[ObjectId],
    refs: &[String],
    opts: &Opts,
) -> Result<ExitCode> {
    let mut all_reachable = true;
    for t in targets {
        let bases = repo.merge_bases_many(local_id, &[*t])?;
        if !bases.iter().any(|b| b.detach() == *t) {
            all_reachable = false;
            break;
        }
    }
    if all_reachable {
        if !opts.quiet {
            println!("Already up to date.");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let stash = begin_autostash(repo, opts)?;
    if repo.is_dirty()? {
        anyhow::bail!("worktree has uncommitted changes; refusing to merge");
    }

    let head_tree = repo.find_object(local_id)?.peel_to_tree()?.id;
    let old_index = repo.index_or_load_from_head()?.into_owned();
    let should_interrupt = AtomicBool::new(false);
    // Our tree is unchanged; sync the index (a no-op checkout).
    update_worktree(repo, &old_index, head_tree, &should_interrupt)?;

    let message = compose_message(repo, refs, targets, branch, local_id, opts)?;
    let spec_label = refs.join(" ");
    finalize_clean(
        repo,
        name,
        local_id,
        targets,
        message,
        head_tree,
        head_tree,
        opts,
        "ours",
        &spec_label,
        stash,
    )
}

/// `git merge <a> <b> [<c>...]` — the octopus strategy: fold each head into the
/// result with a three-way merge, then write one commit carrying every head as a
/// parent. Any head that cannot merge cleanly fails the octopus (git does not
/// resolve conflicts under octopus), leaving the conflicted state and `MERGE_HEAD`.
fn do_octopus(
    repo: &gix::Repository,
    refs: &[String],
    targets: &[ObjectId],
    local_id: ObjectId,
    _branch: Option<&FullName>,
    name: FullName,
    opts: &Opts,
) -> Result<ExitCode> {
    // Every head, resolved by the caller; pair each with its spec for messages.
    let heads: Vec<(String, ObjectId)> = refs
        .iter()
        .cloned()
        .zip(targets.iter().copied())
        .collect();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let stash = begin_autostash(repo, opts)?;
    if repo.is_dirty()? {
        anyhow::bail!("worktree has uncommitted changes; refusing to merge");
    }

    let mut cur_index = repo.index_or_load_from_head()?.into_owned();
    let mut mrt = repo.find_object(local_id)?.peel_to_tree()?.id; // merge result tree
    // `MRC` (git's merge-result-commit list): the parents of the eventual commit.
    // It starts as HEAD but, while still a single commit, is *replaced* by a head
    // that fast-forwards it (so `merge a b` where main is an ancestor of `a` yields
    // parents `[a, b]`, not `[main, a, b]`).
    let mut mrc: Vec<ObjectId> = vec![local_id];
    let should_interrupt = AtomicBool::new(false);

    for (spec, head_id) in &heads {
        let common = if mrc.len() == 1 {
            repo.merge_base(mrc[0], *head_id)?.detach()
        } else {
            repo.merge_base(local_id, *head_id)?.detach()
        };
        if common == *head_id {
            if !opts.quiet {
                println!("Already up to date with {spec}");
            }
            continue;
        }
        let head_tree = repo.find_object(*head_id)?.peel_to_tree()?.id;

        // Fast-forward: while MRC is still a single commit and it is the merge base,
        // git advances the base line to this head rather than recording a parent.
        if mrc.len() == 1 && common == mrc[0] {
            update_worktree(repo, &cur_index, head_tree, &should_interrupt)?;
            cur_index = repo.index_from_tree(&head_tree)?;
            mrt = head_tree;
            mrc = vec![*head_id];
            continue;
        }

        let base_tree = repo.find_object(common)?.peel_to_tree()?.id;
        let labels = gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BStr::new(b"merged common ancestors")),
            current: Some(BStr::new(b"HEAD")),
            other: Some(BStr::new(spec.as_bytes())),
        };
        let applied = crate::merge_apply::three_way_merge_verbose(
            repo,
            base_tree,
            mrt,
            head_tree,
            &cur_index,
            labels,
            &should_interrupt,
            merge_verbosity(repo) != 0,
        )?;
        cur_index = applied.index;
        cur_index.write(Default::default())?;

        if !applied.conflicts.is_empty() {
            // Octopus aborts on the first conflicting head, leaving the conflicted
            // worktree/index and MERGE_HEAD listing every head, as git does.
            let git_dir = repo.git_dir();
            let mut merge_head = String::new();
            for (_, h) in &heads {
                merge_head.push_str(&format!("{h}\n"));
            }
            std::fs::write(git_dir.join("MERGE_HEAD"), merge_head)?;
            std::fs::write(git_dir.join("MERGE_MODE"), b"")?;
            set_orig_head(repo, local_id)?;
            if !opts.quiet {
                println!("Automatic merge failed; fix conflicts and then commit the result.");
            }
            end_autostash(repo, stash, false)?;
            return Ok(ExitCode::from(1));
        }
        mrt = applied.tree_id;
        mrc.push(*head_id);
    }

    // Nothing merged: every head was already reachable.
    if mrc.len() == 1 && mrc[0] == local_id {
        if !opts.quiet {
            println!("Already up to date.");
        }
        end_autostash(repo, stash, true)?;
        return Ok(ExitCode::SUCCESS);
    }
    // Everything collapsed onto one line via fast-forward — a plain fast-forward,
    // not an octopus commit.
    if mrc.len() == 1 {
        set_orig_head(repo, local_id)?;
        advance(
            repo,
            name,
            local_id,
            mrc[0],
            format!("merge {}: Fast-forward", refs.join(" ")),
        )?;
        if !opts.quiet {
            println!("Fast-forward");
        }
        end_autostash(repo, stash, true)?;
        return Ok(ExitCode::SUCCESS);
    }

    // The default octopus message (or the explicit `-m`/`-F` text), plus the
    // `--log` shortlog of every merged head.
    let message = compose_message(repo, refs, targets, None, local_id, opts)?;
    // The finish (squash / stop-before-commit / commit) is shared with the two-head
    // paths; every merged head becomes a parent (`mrc` minus HEAD).
    let extra_parents: Vec<ObjectId> = mrc.iter().copied().filter(|p| *p != local_id).collect();
    finalize_clean(
        repo,
        name,
        local_id,
        &extra_parents,
        message,
        mrt,
        mrt, // no diffstat basis distinct from the octopus tree; git prints none
        opts,
        "octopus",
        &refs.join(" "),
        stash,
    )
}

/// The default octopus commit subject: `Merge branches 'a', 'b' and 'c'`.
fn octopus_message(refs: &[&str]) -> String {
    let quoted: Vec<String> = refs.iter().map(|r| format!("'{r}'")).collect();
    let joined = match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, init)) => format!("{} and {}", init.join(", "), last),
        None => String::new(),
    };
    format!("Merge branches {joined}\n")
}

// ---------------------------------------------------------------------------
// Configuration: merge.stat/merge.diffstat, merge.log/merge.summary
// ---------------------------------------------------------------------------

/// `git_parse_maybe_bool_text()`: the textual booleans only. An empty value is
/// false and anything else is "not a boolean" (`None`).
///
/// Fidelity gap shared with the rest of this build: gix reports a *valueless*
/// key (`[merge]\n\tstat`) as an empty value, where git would see `NULL` and
/// treat it as true.
fn maybe_bool_text(value: &BStr) -> Option<bool> {
    let text = value.to_str().ok()?;
    if text.is_empty() {
        return Some(false);
    }
    match text.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// `merge.stat` / `merge.diffstat` — equivalent keys in `git_merge_config()`, so
/// the last one in configuration order wins. A boolean picks between no diffstat
/// and the ordinary one; the literal `compact` selects the compact summary; any
/// other value falls back to the ordinary diffstat (git's `default:` branch).
fn stat_config(snapshot: &gix::config::Snapshot<'_>) -> Option<StatMode> {
    let mut chosen = None;
    for section in snapshot.plumbing().sections() {
        let header = section.header();
        if header.subsection_name().is_some()
            || !header.name().to_string().eq_ignore_ascii_case("merge")
        {
            continue;
        }
        // Per-name cursors over the two spellings, walked in the order the
        // section actually lists them (as `comment_string` does for core.*).
        let body = section.body();
        let stats = body.values("stat");
        let diffstats = body.values("diffstat");
        let (mut stat_at, mut diffstat_at) = (0usize, 0usize);

        for value_name in body.value_names() {
            let value = if value_name.eq_ignore_ascii_case("stat") {
                let value = stats.get(stat_at);
                stat_at += 1;
                value
            } else if value_name.eq_ignore_ascii_case("diffstat") {
                let value = diffstats.get(diffstat_at);
                diffstat_at += 1;
                value
            } else {
                continue;
            };
            let Some(value) = value else { continue };
            let value: &BStr = value.as_ref();
            chosen = Some(match maybe_bool_text(value) {
                Some(false) => StatMode::None,
                Some(true) => StatMode::Diffstat,
                None if value == BStr::new("compact") => StatMode::CompactSummary,
                None => StatMode::Diffstat,
            });
        }
    }
    chosen
}

/// `merge.log` / `merge.summary` (the deprecated synonym) folded to a shortlog
/// length by `git_config_bool_or_int()`: an integer is taken as-is, a true
/// boolean is `DEFAULT_MERGE_LOG_LEN`, a false one (and an unset key) is 0.
fn shortlog_config(snapshot: &gix::config::Snapshot<'_>) -> i64 {
    let plumbing = snapshot.plumbing();
    let log = plumbing.values::<BString>("merge.log").unwrap_or_default();
    let summary = plumbing.values::<BString>("merge.summary").unwrap_or_default();
    log.last()
        .or(summary.last())
        .and_then(|v| bool_or_int(v.as_bstr()))
        .unwrap_or(0)
}

/// `git_parse_int()` behind parse-options' `OPT_INTEGER`: `strtoimax` over the
/// value (leading whitespace allowed) with an optional `k`/`m`/`g` binary
/// suffix. `None` is git's "not an integer".
fn parse_option_int(value: &str) -> Option<i64> {
    let (digits, factor) = match value.chars().last() {
        Some('k' | 'K') => (&value[..value.len() - 1], 1024i64),
        Some('m' | 'M') => (&value[..value.len() - 1], 1024 * 1024),
        Some('g' | 'G') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        _ => (value, 1),
    };
    digits.trim_start().parse::<i64>().ok()?.checked_mul(factor)
}

/// `git_config_bool_or_int()` as the shortlog length reads it.
fn bool_or_int(value: &BStr) -> Option<i64> {
    let text = value.to_str().ok()?;
    if let Ok(n) = text.trim().parse::<i64>() {
        return Some(n);
    }
    match text.to_ascii_lowercase().as_str() {
        "" | "true" | "yes" | "on" => Some(DEFAULT_MERGE_LOG_LEN),
        "false" | "no" | "off" => Some(0),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The merge message: title, `--log` shortlog, `--edit` comment block
// ---------------------------------------------------------------------------

/// How one merged ref is named, in the two spellings git needs: the `merge_name()`
/// description that goes into the title, and the `handle_line()` *origin* that
/// heads the `--log` shortlog block.
struct SpecOrigin {
    /// e.g. `branch 'topic'`, `tag 'v1'`, `commit 'abc123'`.
    described: String,
    /// e.g. `topic`, `tag 'v1'`, `commit 'abc123'` — git keeps the `tag ` prefix
    /// and drops the quotes only when the whole origin is quoted.
    origin: String,
    /// Whether the origin is a local branch (gates `merge.branchdesc`).
    is_local_branch: bool,
}

/// The whole merge commit message: the title (or the explicit `-m`/`-F` text)
/// followed by the `--log` shortlog of every merged head.
fn compose_message(
    repo: &gix::Repository,
    refs: &[String],
    targets: &[ObjectId],
    branch: Option<&FullName>,
    local_id: ObjectId,
    opts: &Opts,
) -> Result<String> {
    // git's `opts.add_title = !have_message`: an explicit message replaces the
    // generated title but never the shortlog.
    let mut msg = match (&opts.message, refs.len()) {
        (Some(m), _) => {
            let mut m = m.clone();
            if !m.ends_with('\n') {
                m.push('\n');
            }
            m
        }
        (None, 1) => merge_message(repo, &refs[0], branch, opts.into_name.as_deref())?,
        (None, _) => {
            let specs: Vec<&str> = refs.iter().map(String::as_str).collect();
            octopus_message(&specs)
        }
    };
    if opts.log_len != 0 {
        append_shortlog(repo, refs, targets, local_id, opts, &mut msg)?;
    }
    Ok(msg)
}

/// The `--log[=<n>]` block: one `* <origin>:` shortlog per merged head, a port of
/// `fmt_merge_msg()`'s shortlog loop. `credit_people` (the `By`/`Via` comment
/// lines) is on only under `--edit`, as `builtin/merge.c` sets it.
fn append_shortlog(
    repo: &gix::Repository,
    refs: &[String],
    targets: &[ObjectId],
    head: ObjectId,
    opts: &Opts,
    out: &mut String,
) -> Result<()> {
    let comment = comment_char(repo);
    let credit = matches!(edit_wanted(opts), Ok(true));
    complete_line(out);
    for (spec, tip) in refs.iter().zip(targets.iter()) {
        let origin = describe_spec(repo, spec);
        shortlog(repo, &origin, *tip, head, opts, &comment, credit, out)?;
    }
    complete_line(out);
    Ok(())
}

/// git's `shortlog()`: the `* <name>:` block for one merged tip, listing at most
/// `limit` subjects (newest first) and switching the header to
/// `: (<n> commits)` with a trailing `...` when more were merged.
#[allow(clippy::too_many_arguments)]
fn shortlog(
    repo: &gix::Repository,
    origin: &SpecOrigin,
    tip: ObjectId,
    head: ObjectId,
    opts: &Opts,
    comment: &str,
    credit: bool,
    out: &mut String,
) -> Result<()> {
    let limit = opts.log_len;
    let mut count = 0usize;
    let mut subjects: Vec<String> = Vec::new();
    let mut authors: Vec<(BString, usize)> = Vec::new();
    let mut committers: Vec<(BString, usize)> = Vec::new();

    let walk = repo
        .rev_walk([tip])
        .with_hidden([head])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    for info in walk.all()? {
        let commit = info?.object()?;
        if commit.parent_ids().count() > 1 {
            // Merges are not listed, but their committer is still credited.
            record_person(&mut committers, commit.committer()?.trim().name);
            continue;
        }
        if count == 0 {
            record_person(&mut committers, commit.committer()?.trim().name);
        }
        record_person(&mut authors, commit.author()?.trim().name);
        count += 1;
        if subjects.len() as i64 > limit {
            continue;
        }
        let message = commit.message()?;
        let subject = message.summary();
        if subject.is_empty() {
            subjects.push(commit.id.to_hex().to_string());
        } else {
            subjects.push(subject.to_str_lossy().into_owned());
        }
    }

    if credit {
        add_people_info(repo, &mut authors, &mut committers, comment, out);
    }

    if count as i64 > limit {
        out.push_str(&format!("\n* {}: ({count} commits)\n", origin.origin));
    } else {
        out.push_str(&format!("\n* {}:\n", origin.origin));
    }

    if origin.is_local_branch && opts.branch_desc {
        add_branch_desc(repo, &origin.origin, out);
    }

    for (i, subject) in subjects.iter().enumerate() {
        if i as i64 >= limit {
            out.push_str("  ...\n");
        } else {
            out.push_str(&format!("  {subject}\n"));
        }
    }
    Ok(())
}

/// git's `record_person()`: count one appearance, keeping the list sorted by name.
fn record_person(people: &mut Vec<(BString, usize)>, name: &BStr) {
    match people.binary_search_by(|(known, _)| known.as_bstr().cmp(name)) {
        Ok(at) => people[at].1 += 1,
        Err(at) => people.insert(at, (name.to_owned(), 1)),
    }
}

/// git's `add_people_info()`: the `By`/`Via` credit lines, ordered by descending
/// appearance count.
fn add_people_info(
    repo: &gix::Repository,
    authors: &mut [(BString, usize)],
    committers: &mut [(BString, usize)],
    comment: &str,
    out: &mut String,
) {
    authors.sort_by_key(|a| std::cmp::Reverse(a.1));
    committers.sort_by_key(|c| std::cmp::Reverse(c.1));
    let me_author = identity(repo.author());
    let me_committer = identity(repo.committer());
    credit_people(authors, "By", me_author.as_deref(), comment, out);
    credit_people(committers, "Via", me_committer.as_deref(), comment, out);
}

/// `git_author_info(IDENT_NO_DATE)` / `git_committer_info(IDENT_NO_DATE)`.
fn identity(
    configured: Option<std::result::Result<gix::actor::SignatureRef<'_>, gix::config::time::Error>>,
) -> Option<String> {
    let signature = configured?.ok()?;
    Some(format!(
        "{} <{}>",
        signature.name.to_str_lossy(),
        signature.email.to_str_lossy()
    ))
}

/// git's `credit_people()`: the line is skipped when nobody, or only the
/// configured identity, would be credited.
fn credit_people(
    people: &[(BString, usize)],
    label: &str,
    me: Option<&str>,
    comment: &str,
    out: &mut String,
) {
    let only_me = people.len() == 1
        && me.is_some_and(|me| {
            me.as_bytes()
                .strip_prefix(people[0].0.as_slice())
                .is_some_and(|rest| rest.starts_with(b" <"))
        });
    if people.is_empty() || only_me {
        return;
    }
    out.push_str(&format!("\n{comment} {label} "));
    add_people_count(people, out);
}

/// git's `add_people_count()`.
fn add_people_count(people: &[(BString, usize)], out: &mut String) {
    match people {
        [] => {}
        [(name, _)] => out.push_str(&name.to_str_lossy()),
        [(a, an), (b, bn)] => out.push_str(&format!(
            "{} ({an}) and {} ({bn})",
            a.to_str_lossy(),
            b.to_str_lossy()
        )),
        [(a, an), ..] => out.push_str(&format!("{} ({an}) and others", a.to_str_lossy())),
    }
}

/// git's `add_branch_desc()`: `branch.<name>.description`, one `  : <line>` per
/// line (a literal prefix, not a comment one), ahead of the shortlog subjects.
fn add_branch_desc(repo: &gix::Repository, name: &str, out: &mut String) {
    let snapshot = repo.config_snapshot();
    let Some(desc) = snapshot.string(format!("branch.{name}.description").as_str()) else {
        return;
    };
    for line in desc.to_str_lossy().split_inclusive('\n') {
        out.push_str(&format!("  : {line}"));
    }
    complete_line(out);
}

/// git's `strbuf_add_commented_lines()`: every line gets the comment prefix, and
/// a separating space unless the line is empty or starts with a tab.
fn add_commented_lines(text: &str, comment: &str, out: &mut String) {
    for line in text.split_inclusive('\n') {
        out.push_str(comment);
        if !line.starts_with('\n') && !line.starts_with('\t') {
            out.push(' ');
        }
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push('\n');
        }
    }
}

/// git's `strbuf_complete_line()`.
fn complete_line(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// The comment block `prepare_to_commit()` appends below the message when an
/// editor is opened. Under `--cleanup=scissors` the cut line comes first and the
/// closing sentence changes, since comment lines survive that mode.
fn append_editor_comment(msg: &mut String, cleanup: Cleanup, comment: &str) {
    // git strips the message's trailing newline and adds one back; the net effect
    // on a message that already ends in a newline is nothing.
    complete_line(msg);
    if cleanup == Cleanup::Scissors {
        msg.push_str(&format!(
            "{comment} ------------------------ >8 ------------------------\n"
        ));
        add_commented_lines(
            "Do not modify or remove the line above.\nEverything below it will be ignored.\n",
            comment,
            msg,
        );
        msg.push_str(&format!("{comment}\n"));
    }
    add_commented_lines(
        "Please enter a commit message to explain why this merge is necessary,\n\
         especially if it merges an updated upstream into a topic branch.\n\n",
        comment,
        msg,
    );
    if cleanup == Cleanup::Scissors {
        add_commented_lines("An empty message aborts the commit.\n", comment, msg);
    } else {
        add_commented_lines(
            &format!(
                "Lines starting with '{comment}' will be ignored, and an empty message aborts\n\
                 the commit.\n"
            ),
            comment,
            msg,
        );
    }
}

// ---------------------------------------------------------------------------
// --edit: the editor hand-off
// ---------------------------------------------------------------------------

/// git's `default_edit_option()` resolved against `-e`/`--edit`/`--no-edit`: an
/// explicit flag wins, an explicit message means no editor, `GIT_MERGE_AUTOEDIT`
/// decides next, and otherwise the editor is opened only for an interactive run.
///
/// Fidelity gap: git additionally requires stdin and stdout to be the *same*
/// file (same device/inode/mode), which is not reachable without `fstat` on the
/// descriptors; both being terminals is the test here.
fn edit_wanted(opts: &Opts) -> std::result::Result<bool, ExitCode> {
    if let Some(edit) = opts.edit {
        return Ok(edit);
    }
    if opts.message.is_some() {
        return Ok(false);
    }
    if let Ok(value) = std::env::var("GIT_MERGE_AUTOEDIT") {
        return match value.to_ascii_lowercase().as_str() {
            "" | "true" | "yes" | "on" | "1" => Ok(true),
            "false" | "no" | "off" | "0" => Ok(false),
            _ => {
                eprintln!("fatal: Bad value '{value}' in environment 'GIT_MERGE_AUTOEDIT'");
                Err(ExitCode::from(128))
            }
        };
    }
    Ok(std::io::stdin().is_terminal() && std::io::stdout().is_terminal())
}

/// git's `git_editor()`: `GIT_EDITOR`, then `core.editor`, then `$VISUAL`
/// (skipped on a dumb terminal), then `$EDITOR`, then `vi` — and nothing at all
/// when the terminal is dumb and none of them is set.
fn resolve_editor(repo: &gix::Repository, dumb: bool) -> Option<String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let Some(e) = env("GIT_EDITOR") {
        return Some(e);
    }
    if let Some(e) = repo.config_snapshot().string("core.editor") {
        return Some(e.to_string());
    }
    if !dumb {
        if let Some(e) = env("VISUAL") {
            return Some(e);
        }
    }
    if let Some(e) = env("EDITOR") {
        return Some(e);
    }
    if dumb {
        return None;
    }
    Some("vi".to_string())
}

/// Open `path` in the configured editor and wait, git's `launch_editor()`. The
/// command runs through the shell so `core.editor = "code -w"` works, and stdio
/// is inherited so an interactive editor owns the terminal. `:` is git's
/// documented no-op editor. Returns whether the edit succeeded; the diagnostics
/// on failure are git's own.
fn launch_editor(repo: &gix::Repository, path: &Path) -> Result<bool> {
    let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(true);
    let Some(editor) = resolve_editor(repo, dumb) else {
        eprintln!("error: Terminal is dumb, but EDITOR unset");
        return Ok(false);
    };
    if editor == ":" {
        return Ok(true);
    }
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor) // $0
        .arg(path) // $1
        .status();
    match status {
        Ok(status) if status.success() => Ok(true),
        Ok(_) => {
            eprintln!("error: there was a problem with the editor '{editor}'");
            Ok(false)
        }
        Err(e) => {
            eprintln!("error: unable to start editor '{editor}': {e}");
            Ok(false)
        }
    }
}

// ---------------------------------------------------------------------------
// --autostash
// ---------------------------------------------------------------------------

/// The ref git parks the autostash commit under while the merge runs.
const MERGE_AUTOSTASH: &str = "MERGE_AUTOSTASH";

/// git's `create_autostash_ref()`: with `--autostash` (or `merge.autoStash`) and
/// a dirty worktree, snapshot the local changes into a stash-like commit, reset
/// the tracked tree to `HEAD`, and remember the commit under `MERGE_AUTOSTASH`.
/// A clean worktree stashes nothing, exactly as git's own dirty-check decides.
fn begin_autostash(repo: &gix::Repository, opts: &Opts) -> Result<Option<ObjectId>> {
    if !opts.autostash || !repo.is_dirty()? {
        return Ok(None);
    }
    let id = crate::porcelain::stash::create_autostash(repo)?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "merge: autostash".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(id),
        },
        name: MERGE_AUTOSTASH
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name {MERGE_AUTOSTASH}: {e}"))?,
        deref: false,
    })?;
    println!("Created autostash: {}", id.attach(repo).shorten_or_id());
    Ok(Some(id))
}

/// The other half: `apply_autostash_ref()` once the merge produced a new `HEAD`,
/// or — when it stopped early (conflict, `--squash`, `--no-commit`) — git's
/// pointer at the stash it left behind under `MERGE_AUTOSTASH`.
fn end_autostash(repo: &gix::Repository, stash: Option<ObjectId>, applied: bool) -> Result<()> {
    let Some(id) = stash else { return Ok(()) };
    if !applied {
        println!("When finished, apply stashed changes with `git stash pop`");
        return Ok(());
    }
    // The shared apply reports on stdout for `rebase`; merge's own notices go to
    // stderr, so it runs quiet here and the messages are emitted below.
    let conflicts = crate::porcelain::stash::apply_autostash(repo, id, true)?;
    if conflicts.is_empty() {
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
            },
            name: MERGE_AUTOSTASH
                .try_into()
                .map_err(|e| anyhow::anyhow!("invalid ref name {MERGE_AUTOSTASH}: {e}"))?,
            deref: false,
        })?;
        eprintln!("Applied autostash.");
    } else {
        // git keeps the stash reachable so the user can retry the apply.
        eprintln!("Applying autostash resulted in conflicts.");
        eprintln!("Your changes are safe in the stash.");
        eprintln!("You can run \"git stash pop\" or \"git stash drop\" at any time.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// -S / --gpg-sign
// ---------------------------------------------------------------------------

/// git's `sign_commit_to_strbuf()`: sign the serialized commit and attach the
/// armored signature as its `gpgsig` header. The key is `-S<keyid>` when given,
/// else `user.signingKey`, else the committer identity — git's
/// `get_signing_key()` ladder — and the program is `gpg.program`.
fn sign_commit(
    repo: &gix::Repository,
    commit: &mut gix::objs::Commit,
    key: &str,
) -> std::result::Result<(), ExitCode> {
    let snapshot = repo.config_snapshot();
    let program = snapshot
        .string("gpg.program")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "gpg".to_string());
    let signing_key = if !key.is_empty() {
        key.to_string()
    } else {
        match snapshot.string("user.signingKey") {
            Some(v) => v.to_string(),
            None => match identity(repo.committer()) {
                Some(me) => me,
                None => {
                    eprintln!("fatal: no committer identity to sign with");
                    return Err(ExitCode::from(128));
                }
            },
        }
    };

    let mut payload = Vec::new();
    if let Err(e) = commit.write_to(&mut payload) {
        eprintln!("fatal: failed to serialize commit object: {e}");
        return Err(ExitCode::from(128));
    }
    match crate::gitsig::sign(&payload, &program, Some(&signing_key)) {
        Ok(signature) => {
            commit
                .extra_headers
                .push(("gpgsig".into(), signature.into()));
            Ok(())
        }
        Err(e) => {
            eprintln!("error: gpg failed to sign the data:\n{e}\n");
            eprintln!("fatal: failed to write commit object");
            Err(ExitCode::from(128))
        }
    }
}

// ---------------------------------------------------------------------------
// Option plumbing: strategy, cleanup, squash message, signoff, --continue
// ---------------------------------------------------------------------------

/// `MERGE_MODE`'s body: git writes `no-ff` (no trailing newline) when the merge
/// must not fast-forward, and an empty file otherwise.
fn merge_mode(ff: Ff) -> &'static [u8] {
    if ff == Ff::Never {
        b"no-ff"
    } else {
        b""
    }
}

/// Append one `-m`/`--message` value to the accumulating message buffer, joining
/// paragraphs with a blank line. Port of `option_parse_message()`
/// (builtin/merge.c): `strbuf_addf(buf, "%s%s", buf->len ? "\n\n" : "", arg)`.
fn append_message(buf: &mut Option<String>, arg: &str) {
    match buf {
        Some(existing) => {
            existing.push_str("\n\n");
            existing.push_str(arg);
        }
        None => *buf = Some(arg.to_string()),
    }
}

/// Map a `--cleanup=<mode>` value to its mode, or `None` for an invalid one.
fn parse_cleanup(value: &str) -> Option<Cleanup> {
    Some(match value {
        "default" => Cleanup::Default,
        "verbatim" => Cleanup::Verbatim,
        "whitespace" => Cleanup::Whitespace,
        "strip" => Cleanup::Strip,
        "scissors" => Cleanup::Scissors,
        _ => return None,
    })
}

/// Resolve a `-s`/`--strategy` value. Only `ort` and `ours` map to a real merge
/// here; `octopus` folds onto the default path (which already selects the octopus
/// engine for multiple heads). `recursive`/`resolve`/`subtree` are genuine git
/// strategies with distinct conflict resolution that is not vendored, so they are
/// refused rather than silently aliased onto `ort`. An unknown name reproduces
/// git's `Could not find merge strategy` diagnostic.
fn resolve_strategy(name: &str) -> std::result::Result<Strategy, ExitCode> {
    match name {
        "ort" | "octopus" => Ok(Strategy::Ort),
        "ours" => Ok(Strategy::Ours),
        "recursive" | "resolve" | "subtree" => {
            eprintln!("merge: strategy '{name}' is not supported by this build (use 'ort' or 'ours')");
            Err(ExitCode::from(128))
        }
        _ => {
            eprintln!("Could not find merge strategy '{name}'.");
            eprintln!("Available strategies are: octopus ours recursive resolve subtree.");
            Err(ExitCode::from(128))
        }
    }
}

/// `core.commentChar` for cleanup, defaulting to `#` (and treating `auto` and an
/// empty value as `#`). The full multi-valued `commentChar`/`commentString`
/// interleaving lives in `fmt-merge-msg`; a single character covers the merge
/// message paths.
fn comment_char(repo: &gix::Repository) -> String {
    match repo.config_snapshot().string("core.commentChar") {
        Some(v) => {
            let s = v.to_string();
            if s.is_empty() || s == "auto" {
                "#".to_string()
            } else {
                s
            }
        }
        None => "#".to_string(),
    }
}

/// Append git's `Signed-off-by:` trailer (from the committer identity) to a merge
/// message, inserting a blank separator line when the message does not already end
/// with one. This is the common title-only case; a message that already ends in a
/// trailer block is not de-duplicated (git's `append_signoff` scans for that).
fn append_signoff(repo: &gix::Repository, msg: &mut String) -> Result<()> {
    let sig = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let trailer = format!(
        "Signed-off-by: {} <{}>",
        sig.name.to_str_lossy(),
        sig.email.to_str_lossy()
    );
    if !msg.ends_with('\n') {
        msg.push('\n');
    }
    if !msg.ends_with("\n\n") {
        msg.push('\n');
    }
    msg.push_str(&trailer);
    msg.push('\n');
    Ok(())
}

/// git's `cleanup_message()` / `strbuf_stripspace()` for the modes merge exposes.
fn cleanup_message(input: &str, mode: Cleanup, comment: &str) -> String {
    match mode {
        Cleanup::Verbatim => input.to_string(),
        Cleanup::Scissors => {
            let marker = format!("{comment} ------------------------ >8 ------------------------");
            stripspace(&input[..scissors_cut(input, &marker)], None)
        }
        Cleanup::Strip => stripspace(input, Some(comment)),
        // merge's default when a message is supplied without an editor is
        // `whitespace`: strip trailing whitespace and blank runs, keep comments.
        Cleanup::Whitespace | Cleanup::Default => stripspace(input, None),
    }
}

/// The byte offset of the scissors line (`# ----- >8 -----`), or the input length
/// when it is absent; everything from there on is dropped.
fn scissors_cut(input: &str, marker: &str) -> usize {
    let mut pos = 0;
    for line in input.split_inclusive('\n') {
        if line.strip_suffix('\n').unwrap_or(line) == marker {
            return pos;
        }
        pos += line.len();
    }
    input.len()
}

/// Port of `strbuf_stripspace()`: rtrim every line, drop leading/trailing blank
/// lines and collapse consecutive blank lines to one; when `comment` is set,
/// lines starting with it are removed entirely.
fn stripspace(input: &str, comment: Option<&str>) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 1);
    let mut empties = 0usize;
    let mut rest = bytes;

    while !rest.is_empty() {
        let len = match rest.iter().position(|&b| b == b'\n') {
            Some(offset) => offset + 1,
            None => rest.len(),
        };
        let (line, tail) = rest.split_at(len);
        rest = tail;

        if let Some(c) = comment {
            if line.starts_with(c.as_bytes()) {
                continue;
            }
        }

        let mut end = line.len();
        while end > 0 && line[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end == 0 {
            empties += 1;
            continue;
        }
        if empties > 0 && !out.is_empty() {
            out.push(b'\n');
        }
        empties = 0;
        out.extend_from_slice(&line[..end]);
        out.push(b'\n');
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Write `SQUASH_MSG`, a port of `squash_message()` (builtin/merge.c): the header
/// line, then, for every non-merge commit reachable from `targets` but not from
/// `head` (newest first), a `commit <id>` line and its `git log`-medium body.
fn write_squash_msg(repo: &gix::Repository, targets: &[ObjectId], head: ObjectId) -> Result<()> {
    let mut out = String::from("Squashed commit of the following:\n");
    let walk = repo
        .rev_walk(targets.iter().copied())
        .with_hidden([head])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    for info in walk.all()? {
        let commit = info?.object()?;
        // `rev.ignore_merges = 1`: merge commits are skipped.
        if commit.parent_ids().count() > 1 {
            continue;
        }
        out.push('\n');
        out.push_str(&format!("commit {}\n", commit.id));
        let author = commit.author()?;
        out.push_str(&format!(
            "Author: {} <{}>\n",
            author.name.to_str_lossy(),
            author.email.to_str_lossy()
        ));
        let date = author.time()?.format_or_unix(gix::date::time::format::DEFAULT);
        out.push_str(&format!("Date:   {date}\n\n"));
        // medium format indents every message line by four spaces, empty ones too.
        let raw = commit.message_raw()?;
        let body = raw.to_str_lossy();
        for line in body.trim_end_matches('\n').split('\n') {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
    std::fs::write(repo.git_dir().join("SQUASH_MSG"), out)?;
    Ok(())
}

/// Build a tree object from `index` (all stage-0 entries) and return its id, the
/// standard gix editor pass over the index in path order — the tree `--continue`
/// commits.
fn index_tree(repo: &gix::Repository, index: &gix::index::File) -> Result<ObjectId> {
    let backing = index.path_backing();
    let mut editor =
        gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, repo.object_hash());
    for entry in index.entries() {
        let path = entry.path_in(backing);
        let mode = entry
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow::anyhow!("index entry `{path}` has an unrepresentable mode"))?;
        editor.upsert(path.split(|&b| b == b'/').map(|c| c.as_bstr()), mode.kind(), entry.id)?;
    }
    Ok(editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?)
}

/// `git merge --continue`: finish a merge whose conflicts have been resolved and
/// staged, writing the merge commit from the current index and clearing the
/// in-progress state, exactly as `git commit` does when `MERGE_HEAD` is present.
fn continue_merge(opts: &Opts) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    let git_dir = repo.git_dir().to_owned();
    if !git_dir.join("MERGE_HEAD").exists() {
        eprintln!("fatal: There is no merge in progress (MERGE_HEAD missing).");
        return Ok(ExitCode::from(128));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Refuse while the index still carries conflicted (stage 1/2/3) entries.
    let index = repo.open_index()?;
    if index.entries().iter().any(|e| e.stage() != Stage::Unconflicted) {
        eprintln!("error: Committing is not possible because you have unmerged files.");
        // `error_resolve_conflict` (sequencer.c) prints the error unconditionally
        // and the two-line direction only under `advice.resolveConflict`.
        crate::advice::Advice::ResolveConflict.advise_plain(
            "Fix them up in the work tree, and then use 'git add/rm <file>'\n\
             as appropriate to mark resolution and make a commit.",
        );
        eprintln!("fatal: Exiting because of an unresolved conflict.");
        return Ok(ExitCode::from(128));
    }

    let head = repo.head()?;
    if head.is_unborn() {
        anyhow::bail!("cannot conclude a merge on an unborn branch");
    }
    let local_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();

    // Parents: HEAD first, then every id listed in MERGE_HEAD.
    let mut parents: Vec<ObjectId> = vec![local_id];
    let merge_head = std::fs::read_to_string(git_dir.join("MERGE_HEAD"))?;
    for line in merge_head.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        parents.push(
            ObjectId::from_hex(line.as_bytes())
                .map_err(|e| anyhow::anyhow!("invalid id in MERGE_HEAD: {e}"))?,
        );
    }

    // Message from MERGE_MSG, comment lines (the `# Conflicts:` block) stripped as
    // git's finalize cleanup does.
    let raw = std::fs::read_to_string(git_dir.join("MERGE_MSG")).unwrap_or_default();
    let comment = comment_char(&repo);
    let mut msg = cleanup_message(&raw, Cleanup::Strip, &comment);
    if opts.signoff {
        append_signoff(&repo, &mut msg)?;
    }
    if !opts.no_verify {
        let msg_path = git_dir.join("COMMIT_EDITMSG");
        std::fs::write(&msg_path, &msg)?;
        let arg = msg_path.to_string_lossy().into_owned();
        if !crate::hooks::run(&repo, "commit-msg", &[&arg], None)? {
            return Ok(ExitCode::from(1));
        }
        msg = std::fs::read_to_string(&msg_path)?;
    }
    let subject = msg.lines().next().unwrap_or("").to_string();

    let tree_id = index_tree(&repo, &index)?;
    let commit_id = repo.commit("HEAD", &msg, tree_id, parents)?;

    remove_merge_state(&git_dir, true);
    let _ = crate::hooks::run(&repo, "post-merge", &["0"], None);

    if !opts.quiet {
        let short = commit_id.shorten_or_id();
        let branch_label = match repo.head_name()? {
            Some(name) => name.shorten().to_string(),
            None => "detached HEAD".to_string(),
        };
        println!("[{branch_label} {short}] {subject}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `strerror(errno)`: the bare message, without Rust's ` (os error <n>)` tail.
fn strerror(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

/// Move `name` from `old` to `new`, writing `reflog` as the reflog message.
fn advance(
    repo: &gix::Repository,
    name: FullName,
    old: ObjectId,
    new: ObjectId,
    reflog: String,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: reflog.into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(old)),
            new: Target::Object(new),
        },
        name,
        deref: false,
    })?;
    Ok(())
}

/// Point `ORIG_HEAD` at `id`, as git does before it moves `HEAD`.
fn set_orig_head(repo: &gix::Repository, id: ObjectId) -> Result<()> {
    let name: FullName = "ORIG_HEAD"
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid ref name ORIG_HEAD: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "updating ORIG_HEAD".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(id),
        },
        name,
        deref: false,
    })?;
    Ok(())
}

/// The merge commit's message.
///
/// Port of `merge_name()` (builtin/merge.c) feeding `fmt_merge_msg_title()`
/// (fmt-merge-msg.c): the ref is described by the category it resolved into,
/// and ` into <branch>` is appended unless the current branch matches a
/// `merge.suppressDest` glob (defaulting to `main`/`master`), see
/// `dest_suppressed`.
fn merge_message(
    repo: &gix::Repository,
    spec: &str,
    branch: Option<&FullName>,
    into_name: Option<&str>,
) -> Result<String> {
    let described = describe_spec(repo, spec).described;

    // git's `into_name` overrides the destination name verbatim (used both for
    // the ` into <name>` title and the `merge.suppressDest` test); otherwise the
    // shortened current branch name (or `HEAD` when detached) is used.
    let current = match into_name {
        Some(n) => n.to_string(),
        None => match branch {
            Some(b) => b.shorten().to_str_lossy().into_owned(),
            None => "HEAD".to_string(),
        },
    };
    let mut out = format!("Merge {described}");
    if !dest_suppressed(repo, &current) {
        out.push_str(&format!(" into {current}"));
    }
    out.push('\n');
    Ok(out)
}

/// Port of `dest_suppressed()` and the default seeding in `fmt_merge_msg()`
/// (fmt-merge-msg.c): the merge title's ` into <branch>` is dropped when the
/// current branch matches any glob in `merge.suppressDest`, tested with
/// `wildmatch(pattern, branch, WM_PATHNAME)` — case-sensitive, and `*` does not
/// cross a `/`. The variable is multi-valued and accumulates in config order;
/// an empty value clears whatever was gathered so far. When the key is never
/// set at all, the list defaults to `main` then `master`.
fn dest_suppressed(repo: &gix::Repository, branch: &str) -> bool {
    let patterns = suppress_dest_patterns(repo);
    let value = branch.as_bytes().as_bstr();
    patterns
        .iter()
        .any(|p| gix::glob::wildmatch(p.as_bstr(), value, gix::glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL))
}

/// The accumulated `merge.suppressDest` pattern list, resolving git's
/// empty-value-clears rule and its `main`/`master` default when unset.
///
/// Fidelity gap: a *valueless* `merge.suppressDest` (no `=`) makes git die with
/// `config_error_nonbool` at config-parse time; gix reports it as an empty
/// value, indistinguishable from `suppressDest=`, so here it clears the list
/// rather than aborting. This is a config-subsystem limitation shared across
/// keys, not specific to the merge logic.
fn suppress_dest_patterns(repo: &gix::Repository) -> Vec<BString> {
    match repo.config_snapshot().raw_values("merge.suppressDest") {
        Ok(values) => {
            let mut list: Vec<BString> = Vec::new();
            for v in values {
                if v.is_empty() {
                    list.clear();
                } else {
                    list.push(v);
                }
            }
            list
        }
        // `suppress_dest_pattern_seen` never set → the built-in default.
        Err(_) => vec![BString::from("main"), BString::from("master")],
    }
}

/// Classify one merged ref the way `merge_name()` writes it into git's
/// FETCH_HEAD-shaped buffer and `handle_line()` reads it back out.
///
/// gix resolves a partial name through the same rule list git's `dwim_ref`
/// uses ("", tags, heads, remotes), so the full name it lands on is the category
/// git would have reported. An invalid ref name (`main~2`) is not an error here,
/// it just means no ref matched.
///
/// The origin is `handle_line()`'s: the `branch `/`remote-tracking branch `
/// prefix is dropped and the surrounding quotes with it, `tag ` is kept (so the
/// quotes survive), and anything else — a raw commit — is used verbatim.
fn describe_spec(repo: &gix::Repository, spec: &str) -> SpecOrigin {
    if let Ok(Some(r)) = repo.try_find_reference(spec) {
        let full = r.name().as_bstr().to_str_lossy().into_owned();
        if full.starts_with("refs/heads/") {
            return SpecOrigin {
                described: format!("branch '{spec}'"),
                origin: spec.to_string(),
                is_local_branch: true,
            };
        }
        if full.starts_with("refs/tags/") {
            return SpecOrigin {
                described: format!("tag '{spec}'"),
                origin: format!("tag '{spec}'"),
                is_local_branch: false,
            };
        }
        if full.starts_with("refs/remotes/") {
            return SpecOrigin {
                described: format!("remote-tracking branch '{spec}'"),
                origin: spec.to_string(),
                is_local_branch: false,
            };
        }
    } else if let Some((name, early)) = early_part_of_branch(repo, spec) {
        // `branch 'x' (early part)` no longer ends in a quote, so git's
        // quote-stripping leaves the trailing tag — and the quotes — in place.
        return SpecOrigin {
            described: format!("branch '{name}'{}", if early { " (early part)" } else { "" }),
            origin: if early {
                format!("'{name}' (early part)")
            } else {
                name
            },
            is_local_branch: true,
        };
    }
    SpecOrigin {
        described: format!("commit '{spec}'"),
        origin: format!("commit '{spec}'"),
        is_local_branch: false,
    }
}

/// `merge_name()`'s second attempt: `<name>^^^` or `<name>~<number>` naming a
/// point inside an existing branch. The suffix is stripped and, if a branch by
/// the remaining name exists, that branch is what git reports — tagged
/// `(early part)` whenever the suffix actually walks back at least one commit.
fn early_part_of_branch(repo: &gix::Repository, spec: &str) -> Option<(String, bool)> {
    let bytes = spec.as_bytes();
    let mut len = 0usize;
    let mut early = false;

    let carets = bytes.iter().rev().take_while(|&&b| b == b'^').count();
    if carets > 0 && carets < bytes.len() {
        len = carets;
        early = true;
    } else if carets == 0 {
        if let Some(tilde) = spec.rfind('~') {
            let digits = &bytes[tilde + 1..];
            if digits.iter().all(u8::is_ascii_digit) {
                len = 1 + digits.len();
                // "name~" means "name~1"; "name~0" walks back nothing.
                early = digits.is_empty() || digits.iter().any(|&b| b != b'0');
            }
        }
    }

    if len == 0 || len >= bytes.len() {
        return None;
    }
    let stripped = &spec[..bytes.len() - len];
    match repo.try_find_reference(format!("refs/heads/{stripped}").as_str()) {
        Ok(Some(_)) => Some((stripped.to_string(), early)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Worktree + index transition
// ---------------------------------------------------------------------------

/// Move the worktree and its index from the state captured in `old` to
/// `new_tree`, writing only the paths that changed.
///
/// Ported from the `zsync` reconcile path: the change set is derived by
/// comparing the old index against the new tree-index (file-level granularity),
/// added/modified files are checked out via `gix-worktree-state`, removed files
/// are deleted, and the new index is written reusing prior stats for unchanged
/// entries so a later status stays cheap.
///
/// A path carrying any conflicted stage in `old` is always treated as changed:
/// its worktree file holds conflict markers rather than any indexed blob, so it
/// must be rewritten even when one of its stages happens to match the new tree.
fn update_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_tree: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    // Index the current entries by path for change detection and stat reuse.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    let mut conflicted: HashSet<BString> = HashSet::new();
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing).to_owned();
            if e.stage_raw() != 0 {
                conflicted.insert(path.clone());
            }
            old_map.insert(path, (e.id, e.mode, e.stat));
        }
    }

    // Full target index (all new-tree entries) — what is finally written; a
    // reduced copy of only the changed entries is what is checked out.
    let mut new_index = repo.index_from_tree(&new_tree)?;
    let mut subset = repo.index_from_tree(&new_tree)?;
    subset.remove_entries(|_, path, entry| {
        let path = path.to_owned();
        if conflicted.contains(&path) {
            return false;
        }
        match old_map.get(&path) {
            // Present before with identical content and mode → unchanged, drop it.
            Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
            // Absent before → an addition, keep it.
            None => false,
        }
    });

    // Write the changed files into the worktree, overwriting in place.
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

    // Remove files present before but not in the new tree.
    let new_paths: HashSet<BString> = {
        let backing = new_index.path_backing();
        new_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing);
            if !new_paths.contains(&path.to_owned()) {
                if let Some(full) = repo.workdir_path(path) {
                    let _ = std::fs::remove_file(full);
                }
            }
        }
    }

    // Fresh stats produced by the checkout for the changed entries.
    let mut subset_stats: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    // Changed entries get their fresh stat; unchanged entries reuse the old one.
    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode && !conflicted.contains(&path) {
                    e.stat = *stat;
                }
            }
        }
    }

    // Drop any stale cache-tree extension before persisting.
    new_index.remove_tree();
    new_index.write(Default::default())?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Diffstat and summary (diff.c)
// ---------------------------------------------------------------------------

/// One diffstat row.
struct StatRow {
    /// Quoted path, as git's `fill_print_name` produces it.
    name: String,
    /// Inserted lines, or the new blob's byte size when `binary`.
    added: u64,
    /// Deleted lines, or the old blob's byte size when `binary`.
    deleted: u64,
    binary: bool,
    /// `get_compact_summary()`'s annotation for this pair, used only under
    /// `--compact-summary`; `None` for a content-only modification.
    compact: Option<&'static str>,
}

/// Port of `get_compact_summary()` (diff.c): the parenthesized note
/// `--compact-summary` folds into a diffstat name, in git's order — creation
/// (`new`/`new +x`/`new +l`), deletion (`gone`), then the symlink and
/// executable-bit mode transitions.
fn compact_comment(old: Option<EntryKind>, new: Option<EntryKind>) -> Option<&'static str> {
    // DIFF_STATUS_ADDED.
    if old.is_none() {
        return Some(match new {
            Some(EntryKind::Link) => "new +l",
            Some(EntryKind::BlobExecutable) => "new +x",
            _ => "new",
        });
    }
    // DIFF_STATUS_DELETED.
    if new.is_none() {
        return Some("gone");
    }
    let (old, new) = (old.expect("old present"), new.expect("new present"));
    let (old_link, new_link) = (old == EntryKind::Link, new == EntryKind::Link);
    if old_link && !new_link {
        Some("mode -l")
    } else if !old_link && new_link {
        Some("mode +l")
    } else if old == EntryKind::Blob && new == EntryKind::BlobExecutable {
        Some("mode +x")
    } else if old == EntryKind::BlobExecutable && new == EntryKind::Blob {
        Some("mode -x")
    } else {
        None
    }
}

/// git's `decimal_width`.
fn decimal_width(mut n: u64) -> i64 {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

/// git's `scale_linear`: at least one column for any non-zero change.
fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// Display width in Unicode scalar values (git measures terminal columns; wide
/// characters are counted as 1 here, see the module note).
fn display_width(s: &str) -> i64 {
    s.chars().count() as i64
}

/// git's `quote_c_style` as applied to diff path names.
fn quote_path(path: &[u8]) -> String {
    let needs = path
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return String::from_utf8_lossy(path).into_owned();
    }
    let mut out = String::from("\"");
    for &b in path {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// The block `finish()` (builtin/merge.c) prints after a merge, rendered as one
/// string: `DIFF_FORMAT_DIFFSTAT | DIFF_FORMAT_SUMMARY` for `--stat`, and the
/// diffstat alone with `stat_with_summary` — each name carrying its
/// ` (new)`/` (gone)`/` (mode +x)` annotation — for `--compact-summary`.
fn diffstat(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
    mode: StatMode,
) -> Result<String> {
    if mode == StatMode::None {
        return Ok(String::new());
    }
    let (mut rows, summary) = collect(repo, old_tree, new_tree)?;
    let mut out = String::new();
    if mode == StatMode::CompactSummary {
        // `fill_print_name()` appends the annotation to the name, so it counts
        // towards the name column's width.
        for row in &mut rows {
            if let Some(comment) = row.compact {
                row.name.push_str(&format!(" ({comment})"));
            }
        }
    }
    emit_stats(&mut out, &rows);
    if mode == StatMode::Diffstat {
        for line in &summary {
            out.push_str(&format!(" {line}\n"));
        }
    }
    Ok(out)
}

/// Walk the tree-to-tree diff once, producing the stat rows and the summary
/// lines, both ordered by path as git's tree recursion orders them.
fn collect(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
) -> Result<(Vec<StatRow>, Vec<String>)> {
    let old = repo.find_tree(old_tree)?;
    let new = repo.find_tree(new_tree)?;
    let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;

    // Per row: path (for ordering), display name, line counts, and — when the
    // blob diff declined because a side is binary — the ids whose sizes git
    // reports instead. Sizes are looked up after the walk so the callback stays
    // infallible.
    type RawRow = (
        BString,
        String,
        Option<(u64, u64)>,
        Option<ObjectId>,
        Option<ObjectId>,
        Option<&'static str>,
    );
    let mut raw: Vec<RawRow> = Vec::new();
    let mut summary: Vec<(BString, String)> = Vec::new();

    let mut platform = old.changes()?;
    platform.options(|opts| {
        opts.track_rewrites(None);
    });
    let _rewrites = platform.for_each_to_obtain_tree(&new, |change| {
        let path: BString = change.location().to_owned();
        let display = quote_path(&path[..]);
        let (old_id, new_id, compact) = match change {
            TreeChange::Addition { entry_mode, id, .. } => {
                summary.push((
                    path.clone(),
                    format!("create mode {:06o} {display}", entry_mode.value()),
                ));
                (
                    None,
                    Some(id.detach()),
                    compact_comment(None, Some(entry_mode.kind())),
                )
            }
            TreeChange::Deletion { entry_mode, id, .. } => {
                summary.push((
                    path.clone(),
                    format!("delete mode {:06o} {display}", entry_mode.value()),
                ));
                (
                    Some(id.detach()),
                    None,
                    compact_comment(Some(entry_mode.kind()), None),
                )
            }
            TreeChange::Modification {
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
                ..
            } => {
                if previous_entry_mode.value() != entry_mode.value() {
                    summary.push((
                        path.clone(),
                        format!(
                            "mode change {:06o} => {:06o} {display}",
                            previous_entry_mode.value(),
                            entry_mode.value()
                        ),
                    ));
                }
                (
                    Some(previous_id.detach()),
                    Some(id.detach()),
                    compact_comment(Some(previous_entry_mode.kind()), Some(entry_mode.kind())),
                )
            }
            // Rewrites cannot occur: rename tracking is off above.
            TreeChange::Rewrite {
                source_entry_mode,
                source_id,
                entry_mode,
                id,
                ..
            } => (
                Some(source_id.detach()),
                Some(id.detach()),
                compact_comment(Some(source_entry_mode.kind()), Some(entry_mode.kind())),
            ),
        };

        let counts = change
            .diff(&mut resource_cache)
            .ok()
            .and_then(|mut p| p.line_counts().ok())
            .flatten()
            .map(|c| (u64::from(c.insertions), u64::from(c.removals)));
        raw.push((path, display, counts, old_id, new_id, compact));

        resource_cache.clear_resource_cache_keep_allocation();
        Ok::<_, std::convert::Infallible>(Action::Continue(()))
    })?;
    drop(platform);

    let blob_size = |id: Option<ObjectId>| -> Result<u64> {
        match id {
            // git's `diff_filespec_size` of an invalid filespec is 0.
            None => Ok(0),
            Some(id) => Ok(repo.find_object(id)?.data.len() as u64),
        }
    };

    let mut rows: Vec<(BString, StatRow)> = Vec::with_capacity(raw.len());
    for (path, name, counts, old_id, new_id, compact) in raw {
        let row = match counts {
            Some((added, deleted)) => StatRow {
                name,
                added,
                deleted,
                binary: false,
                compact,
            },
            None => StatRow {
                name,
                added: blob_size(new_id)?,
                deleted: blob_size(old_id)?,
                binary: true,
                compact,
            },
        };
        rows.push((path, row));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    summary.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((
        rows.into_iter().map(|(_, r)| r).collect(),
        summary.into_iter().map(|(_, l)| l).collect(),
    ))
}

/// Port of `show_stats()` (diff.c) at merge's `stat_width = -1`, which resolves
/// to `term_columns()` — 80 whenever stdout is not a terminal and `COLUMNS` is
/// unset, as it is under the parity harness. Followed by
/// `print_stat_summary_inserts_deletes()`.
fn emit_stats(out: &mut String, files: &[StatRow]) {
    if files.is_empty() {
        return;
    }

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    for f in files {
        max_len = max_len.max(display_width(&f.name));
        if f.binary {
            // "Bin XXX -> YYY bytes"
            bin_width = bin_width.max(14 + decimal_width(f.added) + decimal_width(f.deleted));
            // Display change counts aligned with "Bin".
            number_width = 3;
            continue;
        }
        max_change = max_change.max((f.added + f.deleted) as i64);
    }

    let mut width: i64 = 80;
    number_width = number_width.max(decimal_width(max_change as u64));

    // Guarantee 3/8*16==6 for the graph part and 5/8*16==10 for the filename.
    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = if max_change + 4 > bin_width { max_change } else { bin_width - 4 };
    let mut name_width = max_len;
    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = width * 3 / 8 - number_width - 6;
            if graph_width < 6 {
                graph_width = 6;
            }
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    for f in files {
        // Scale the filename: elide the head, then resume at a path separator.
        let mut len = name_width;
        let mut prefix = "";
        let mut name: &str = &f.name;
        if name_width < display_width(name) {
            prefix = "...";
            len -= 3;
            if len < 0 {
                len = 0;
            }
            let mut name_len = display_width(name);
            let mut off = 0;
            while name_len > len && off < name.len() {
                let c = name[off..]
                    .chars()
                    .next()
                    .expect("off stays on a char boundary");
                off += c.len_utf8();
                name_len -= 1;
            }
            name = &name[off..];
            if let Some(slash) = name.find('/') {
                name = &name[slash..];
            }
        }
        let padding = (len - display_width(name)).max(0) as usize;
        let nw = number_width as usize;

        if f.binary {
            out.push_str(&format!(" {prefix}{name}{:padding$} | {:>nw$}", "", "Bin"));
            if f.added == 0 && f.deleted == 0 {
                out.push('\n');
            } else {
                out.push_str(&format!(" {} -> {} bytes\n", f.deleted, f.added));
            }
            continue;
        }

        let total = f.added + f.deleted;
        let mut add = f.added as i64;
        let mut del = f.deleted as i64;
        if graph_width <= max_change && max_change > 0 {
            let mut sum = scale_linear(add + del, graph_width, max_change);
            if sum < 2 && add > 0 && del > 0 {
                sum = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = sum - add;
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = sum - del;
            }
        }

        out.push_str(&format!(
            " {prefix}{name}{:padding$} | {:>nw$}{}",
            "",
            total,
            if total > 0 { " " } else { "" },
        ));
        for _ in 0..add.max(0) {
            out.push('+');
        }
        for _ in 0..del.max(0) {
            out.push('-');
        }
        out.push('\n');
    }

    // Binary rows count as changed files but contribute no insertions/deletions.
    let mut adds: u64 = 0;
    let mut dels: u64 = 0;
    for f in files {
        if !f.binary {
            adds += f.added;
            dels += f.deleted;
        }
    }

    let n = files.len();
    let mut line = format!(" {n} {} changed", if n == 1 { "file" } else { "files" });
    if adds > 0 || dels == 0 {
        line.push_str(&format!(
            ", {adds} {}",
            if adds == 1 { "insertion(+)" } else { "insertions(+)" }
        ));
    }
    if dels > 0 || adds == 0 {
        line.push_str(&format!(
            ", {dels} {}",
            if dels == 1 { "deletion(-)" } else { "deletions(-)" }
        ));
    }
    out.push_str(&line);
    out.push('\n');
}
