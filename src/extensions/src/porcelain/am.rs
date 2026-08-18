//! `git am` — apply a series of patches from a mailbox.
//!
//! Port of `builtin/am.c`. The command decomposes into four stages:
//!
//!   1. **Option parsing** (`cmd_am`'s `parse_options`) — including the
//!      `OPT_CMDMODE` mutual exclusion between the resume verbs, the callbacks
//!      that reject `--patch-format`/`--empty`/`--quoted-cr`/`--show-current-patch`
//!      values, and the `OPT_PASSTHRU_ARGV` options that are recorded verbatim
//!      for `git apply`.
//!   2. **Session dispatch** (`am_in_progress` and the `in_progress` branch) —
//!      whether a `.git/rebase-apply` session exists decides between resuming,
//!      refusing to resume, destroying a stray directory, or starting fresh.
//!   3. **Session setup** (`am_setup`) — patch-format detection, splitting the
//!      mailbox, and writing the `.git/rebase-apply` state files, `ORIG_HEAD`
//!      and `abort-safety`.
//!   4. **Patch application** (`am_run`'s loop, `parse_mail`, `do_commit`) and
//!      the resume verbs (`am_resolve`/`am_skip`/`am_abort`). git implements this
//!      stage by shelling out to `git mailinfo`/`git apply`/`git write-tree`/
//!      `git commit-tree`/`git update-ref`/`git stripspace`/`git reset`; because
//!      those subcommands are themselves ported, this module drives them by
//!      re-executing this binary (`std::env::current_exe`) as a child — the same
//!      pattern `for_each_repo`/`quiltimport` use.
//!
//! ## What is served
//!
//!   * **`--3way`, including the fallback.** `fall_back_threeway` is ported: on a
//!     failed apply, `git apply --build-fake-ancestor` rebuilds the pre-image tree
//!     from the patch's own `index` lines in a scratch index
//!     (`.git/rebase-apply/patch-merge-index`), the patch is re-applied to *that*
//!     with `--cached`, and the two trees are merged against `HEAD` through
//!     [`crate::merge_apply`] — printing git's `Using index info to reconstruct a
//!     base tree...` / `Falling back to patching base and 3-way merge...` pair and
//!     recording the result as `AUTO_MERGE`.
//!   * **`--rebasing`, the mode `git rebase --apply` drives.** `parse_mail_rebase`
//!     reads each message's `From <oid>` postmark and rebuilds everything from
//!     that commit — `get_commit_info` takes the authorship and message off the
//!     commit object (so the original author *and* author date survive a rebase
//!     verbatim), `write_commit_patch` regenerates the diff with `git diff-tree`,
//!     and the mail body is never consulted. The session records
//!     `original-commit` and `REBASE_HEAD`, appends `<old> <new>` to `rewritten`
//!     from both `do_commit` and `--skip`, feeds that list to the `post-rewrite`
//!     hook, and leaves its directory standing for the caller to clean up.
//!   * **The `applypatch-msg`, `pre-applypatch` and `post-applypatch` hooks**, with
//!     `-n`/`--no-verify` suppressing the first two. A hook that rewrites
//!     `final-commit` changes the committed message, because the file is re-read
//!     after it runs.
//!   * **`--resolvemsg=<text>`**, which replaces the whole conflict hint block
//!     rather than adding to it — this is how a conflicted `git rebase --apply`
//!     says `git rebase --continue` and never mentions `git am`.
//!   * **The full apply pipeline for a clean patch.** Each split message is run
//!     through `git mailinfo` (authorship + subject + body + diff), the diff is
//!     staged with `git apply --index`, and the commit is written with
//!     `git write-tree` + `git commit-tree` preserving the mail's author (name,
//!     email, and `GIT_AUTHOR_DATE`), then `HEAD` is moved with `git update-ref`
//!     carrying the `am: <subject>` reflog line. `git am <mbox>` applies and
//!     commits, and `--continue`/`--skip`/`--abort` drive the state machine.
//!
//!   * Every argument-validation path: unknown/duplicated resume verbs and bad
//!     option values produce git's message on stderr and exit 129.
//!   * `--continue`/`-r`/`--resolved`/`--skip`/`--abort`/`--quit`/`--retry`/
//!     `--allow-empty`/`--show-current-patch` outside a session — `fatal: Resolve
//!     operation not in progress, we are not resuming.`, exit 128.
//!   * A stray (non-session) `.git/rebase-apply` directory: removed silently by
//!     `--abort`/`--quit`, otherwise `fatal: Stray ... directory found.`
//!   * A mailbox handed to a live session — `fatal: previous rebase directory
//!     <dir> still exists but mbox given.`, exit 128.
//!   * Patch-format detection (`detect_patch_format`), including its stdin and
//!     directory defaults, its `From `/StGit/hg first-line probes, and `is_mail`.
//!     A file that cannot be opened dies `could not open '<p>' for reading`; a
//!     file that matches nothing prints `Patch format detection failed.`
//!   * Mailbox splitting **to the point of counting messages**. An empty mailbox
//!     — the common `git am </dev/null` case — completes the whole command: the
//!     session directory is written and then destroyed, `ORIG_HEAD` is set, and
//!     the exit code is 0. The split-failure paths (`Only one StGIT patch series
//!     can be applied at once`, an unreadable patch) print git's `error:` line
//!     followed by `fatal: Failed to split patches.` and exit 128.
//!   * `am_run`'s pre-flight: unmerged index entries print `<path>: needs merge`
//!     on stdout, and a index that differs from `HEAD` writes `dirtyindex` into
//!     the session and dies `Dirty index: cannot apply patches (dirty: <paths>)`.
//!   * **Empty-patch messages.** After `git mailinfo`, a message that produced no
//!     patch follows `--empty`: `stop` (default) prints `Patch is empty.` plus the
//!     `advice.mergeConflict` hint block (exit 128), `drop` prints
//!     `Skipping: <subject>` (exit 0), and `keep` prints
//!     `Creating an empty commit: <subject>` and records an empty commit — or, if
//!     the message carries no author, dies on the empty ident
//!     (`empty ident name (for <>) not allowed`, exit 128) exactly as git's
//!     strict `fmt_ident`. A message `mailinfo` cannot parse at all dies
//!     `could not parse patch` (exit 128).
//!   * **Resume verbs.** `--continue`/`--resolved`/`--allow-empty` (`am_resolve`)
//!     commit the user's resolved index and continue; `--skip` (`am_skip`) resets
//!     the index/worktree to `HEAD` and continues; `--abort` (`am_abort`) rewinds
//!     to `ORIG_HEAD` when it is safe to. `--show-current-patch[=(raw|diff)]` and
//!     `--quit` operate inside a live session.
//!   * **Config defaults.** `git_am_config` runs before option parsing, so
//!     `am.threeway` and `am.messageId` seed `--3way`/`--message-id` and the
//!     command line overrides them. Both flow into the `threeway`/`messageid`
//!     state files `am_setup` writes, which stay behind — and are therefore
//!     observable — whenever the run stops (e.g. `Patch is empty.`). A malformed
//!     boolean dies with git's `fatal: bad boolean config value ...` at
//!     config-read time (exit 128), before any state directory is created.
//!     `am.keepcr` is *not* honored: it only tunes `mailsplit`'s CR handling,
//!     which this port does not implement (`split_mbox` copies the body
//!     verbatim), so reading it would have no observable effect and it is left
//!     unmapped rather than faked.
//!
//! ## What is not served, and why
//!
//! These reshape the commit or the flow in ways this port cannot reproduce
//! faithfully through the ported subcommands, so each refuses *before* it could
//! write a wrong object or worktree rather than emit a guess:
//!
//!   * **`-i`/`--interactive`.** The per-patch tty prompt loop cannot run
//!     unattended.
//!   * **`-S`.** Signing the commit needs a path `git commit-tree` does not expose
//!     here. `--ignore-date` and `--committer-date-is-author-date` are honoured:
//!     the first drops the mail's author date, the second dates the committer by it.
//!   * **`GIT binary patch` bodies under `--rebasing`.** `write_commit_patch`
//!     regenerates the diff with `git diff-tree`, which does not accept
//!     `--binary` in this binary yet, so a replayed commit that changes a binary
//!     file regenerates as `Binary files … differ` and then fails to apply. That
//!     stops the replay loudly rather than committing a wrong tree.
//!
//! `--signoff` appends the trailer through the same `append_signoff()` port
//! `commit` uses, after either parse arm and before the message is stored, so a
//! `--rebasing --signoff` replay carries it and a later `--continue` sees it
//! already in `final-commit`.
//!
//! `split_mbox` is `git mailsplit`: an input is cut at its `From ` postmarks
//! (`is_from_line()`'s date-shaped test, not the object name), each message keeping the
//! postmark it starts with, and content before the first postmark forming a message of
//! its own — which is how a bare patch file reaches `am`.

use anyhow::{bail, Result};
use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use super::{Arg, LongOpt};

/// `cmd_am()`'s `struct option options[]` (builtin/am.c), in table order, as
/// [`super::resolve_long`] reads it.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "interactive",                 neg: true,  arg: Arg::None },
    LongOpt { name: "no-verify",                   neg: true,  arg: Arg::None },
    LongOpt { name: "binary",                      neg: true,  arg: Arg::None },
    LongOpt { name: "3way",                        neg: true,  arg: Arg::None },
    LongOpt { name: "quiet",                       neg: true,  arg: Arg::None },
    LongOpt { name: "signoff",                     neg: true,  arg: Arg::None },
    LongOpt { name: "utf8",                        neg: true,  arg: Arg::None },
    LongOpt { name: "keep",                        neg: true,  arg: Arg::None },
    LongOpt { name: "keep-non-patch",              neg: true,  arg: Arg::None },
    LongOpt { name: "message-id",                  neg: true,  arg: Arg::None },
    LongOpt { name: "keep-cr",                     neg: true,  arg: Arg::None },
    LongOpt { name: "scissors",                    neg: true,  arg: Arg::None },
    LongOpt { name: "quoted-cr",                   neg: false, arg: Arg::Required },
    LongOpt { name: "whitespace",                  neg: true,  arg: Arg::Required },
    LongOpt { name: "ignore-space-change",         neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-whitespace",           neg: true,  arg: Arg::None },
    LongOpt { name: "directory",                   neg: true,  arg: Arg::Required },
    LongOpt { name: "exclude",                     neg: true,  arg: Arg::Required },
    LongOpt { name: "include",                     neg: true,  arg: Arg::Required },
    LongOpt { name: "patch-format",                neg: true,  arg: Arg::Required },
    LongOpt { name: "reject",                      neg: true,  arg: Arg::None },
    LongOpt { name: "resolvemsg",                  neg: true,  arg: Arg::Required },
    LongOpt { name: "continue",                    neg: false, arg: Arg::None },
    LongOpt { name: "resolved",                    neg: false, arg: Arg::None },
    LongOpt { name: "skip",                        neg: false, arg: Arg::None },
    LongOpt { name: "abort",                       neg: false, arg: Arg::None },
    LongOpt { name: "quit",                        neg: false, arg: Arg::None },
    LongOpt { name: "show-current-patch",          neg: false, arg: Arg::Optional },
    LongOpt { name: "retry",                       neg: false, arg: Arg::None },
    LongOpt { name: "allow-empty",                 neg: false, arg: Arg::None },
    LongOpt { name: "committer-date-is-author-date", neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-date",                 neg: true,  arg: Arg::None },
    LongOpt { name: "rerere-autoupdate",           neg: true,  arg: Arg::None },
    LongOpt { name: "gpg-sign",                    neg: true,  arg: Arg::Optional },
    LongOpt { name: "empty",                       neg: false, arg: Arg::Required },
    LongOpt { name: "rebasing",                    neg: true,  arg: Arg::None },
];

/// `usage_with_options()` over `builtin/am.c`'s option table, verbatim.
const USAGE: &str = r"usage: git am [<options>] [(<mbox> | <Maildir>)...]
   or: git am [<options>] (--continue | --skip | --abort)

    -i, --[no-]interactive
                          run interactively
    -n, --no-verify       bypass pre-applypatch and applypatch-msg hooks
    --verify              opposite of --no-verify
    -3, --[no-]3way       allow fall back on 3way merging if needed
    -q, --[no-]quiet      be quiet
    -s, --[no-]signoff    add a Signed-off-by trailer to the commit message
    -u, --[no-]utf8       recode into utf8 (default)
    -k, --[no-]keep       pass -k flag to git-mailinfo
    --[no-]keep-non-patch pass -b flag to git-mailinfo
    -m, --[no-]message-id pass -m flag to git-mailinfo
    --[no-]keep-cr        pass --keep-cr flag to git-mailsplit for mbox format
    -c, --[no-]scissors   strip everything before a scissors line
    --quoted-cr <action>  pass it through git-mailinfo
    --[no-]whitespace <action>
                          pass it through git-apply
    --[no-]ignore-space-change
                          pass it through git-apply
    --[no-]ignore-whitespace
                          pass it through git-apply
    --[no-]directory <root>
                          pass it through git-apply
    --[no-]exclude <path> pass it through git-apply
    --[no-]include <path> pass it through git-apply
    -C <n>                pass it through git-apply
    -p <num>              pass it through git-apply
    --[no-]patch-format <format>
                          format the patch(es) are in
    --[no-]reject         pass it through git-apply
    --[no-]resolvemsg ... override error message when patch failure occurs
    --continue            continue applying patches after resolving a conflict
    -r, --resolved        synonyms for --continue
    --skip                skip the current patch
    --abort               restore the original branch and abort the patching operation
    --quit                abort the patching operation but keep HEAD where it is
    --show-current-patch[=(diff|raw)]
                          show the patch being applied
    --retry               try to apply current patch again
    --allow-empty         record the empty patch as an empty commit
    --[no-]committer-date-is-author-date
                          lie about committer date
    --[no-]ignore-date    use current timestamp for author date
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG-sign commits
    --empty (stop|drop|keep)
                          how to handle empty patches

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]binary`, `--[no-]rebasing`.
/// Captured byte-for-byte from stock git 2.55.0's `git am --help-all`.
const USAGE_ALL: &str = r#"usage: git am [<options>] [(<mbox> | <Maildir>)...]
   or: git am [<options>] (--continue | --skip | --abort)

    -i, --[no-]interactive
                          run interactively
    -n, --no-verify       bypass pre-applypatch and applypatch-msg hooks
    --verify              opposite of --no-verify
    -b, --[no-]binary     historical option -- no-op
    -3, --[no-]3way       allow fall back on 3way merging if needed
    -q, --[no-]quiet      be quiet
    -s, --[no-]signoff    add a Signed-off-by trailer to the commit message
    -u, --[no-]utf8       recode into utf8 (default)
    -k, --[no-]keep       pass -k flag to git-mailinfo
    --[no-]keep-non-patch pass -b flag to git-mailinfo
    -m, --[no-]message-id pass -m flag to git-mailinfo
    --[no-]keep-cr        pass --keep-cr flag to git-mailsplit for mbox format
    -c, --[no-]scissors   strip everything before a scissors line
    --quoted-cr <action>  pass it through git-mailinfo
    --[no-]whitespace <action>
                          pass it through git-apply
    --[no-]ignore-space-change
                          pass it through git-apply
    --[no-]ignore-whitespace
                          pass it through git-apply
    --[no-]directory <root>
                          pass it through git-apply
    --[no-]exclude <path> pass it through git-apply
    --[no-]include <path> pass it through git-apply
    -C <n>                pass it through git-apply
    -p <num>              pass it through git-apply
    --[no-]patch-format <format>
                          format the patch(es) are in
    --[no-]reject         pass it through git-apply
    --[no-]resolvemsg ... override error message when patch failure occurs
    --continue            continue applying patches after resolving a conflict
    -r, --resolved        synonyms for --continue
    --skip                skip the current patch
    --abort               restore the original branch and abort the patching operation
    --quit                abort the patching operation but keep HEAD where it is
    --show-current-patch[=(diff|raw)]
                          show the patch being applied
    --retry               try to apply current patch again
    --allow-empty         record the empty patch as an empty commit
    --[no-]committer-date-is-author-date
                          lie about committer date
    --[no-]ignore-date    use current timestamp for author date
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG-sign commits
    --empty (stop|drop|keep)
                          how to handle empty patches
    --[no-]rebasing       (internal use for git-rebase)

"#;

/// `enum resume_type`. `Apply` is never selected by an argument; `cmd_am`
/// promotes a bare `git am` inside a live session into it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Resume {
    Apply,
    Resolved,
    Skip,
    Abort,
    Quit,
    Retry,
    AllowEmpty,
    ShowPatch(Sub),
}

/// `enum show_patch_type`. A bare `--show-current-patch` means `Raw`, which is
/// why `--show-current-patch --show-current-patch=raw` is accepted while
/// `--show-current-patch --show-current-patch=diff` is not.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sub {
    Raw,
    Diff,
}

/// `enum patch_format`, minus `PATCH_FORMAT_UNKNOWN` which is `None` here.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Mbox,
    Stgit,
    StgitSeries,
    Hg,
    Mboxrd,
}

/// `enum keep_type` — what `-k`/`--keep-non-patch` pass to `git mailinfo`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keep {
    False,
    True,
    NonPatch,
}

/// `--empty=(stop|drop|keep)` — how `am_run` treats a message whose patch is
/// empty. `stop` is git's default (`STOP_ON_EMPTY_COMMIT`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Empty {
    Stop,
    Drop,
    Keep,
}

/// Everything `parse_options` fills in, in the same shape `struct am_state` uses.
struct Opts {
    resume: Option<(Resume, String)>,
    format: Option<Format>,
    paths: Vec<String>,
    interactive: bool,
    no_verify: bool,
    rebasing: bool,
    resolvemsg: Option<String>,
    /// `-b`/`--binary`/`--no-binary` was given. The option does nothing, but
    /// `cmd_am` prints a deprecation notice whenever it appears.
    binary_given: bool,
    threeway: bool,
    quiet: bool,
    signoff: bool,
    utf8: bool,
    keep: Keep,
    empty: Empty,
    message_id: bool,
    scissors: Option<bool>,
    quoted_cr: Option<&'static str>,
    rerere_autoupdate: Option<bool>,
    apply_opts: Vec<String>,
    // `do_commit` shaping flags. This port applies patches faithfully but cannot
    // reproduce these without unported substrate, so they are captured (rather
    // than the historical no-op) to refuse before writing a wrong commit.
    ignore_date: bool,
    committer_date_is_author_date: bool,
    gpg_sign: bool,
}

impl Default for Opts {
    fn default() -> Self {
        // `am_state_init`: utf8 defaults on, everything else off.
        Self {
            resume: None,
            format: None,
            paths: Vec::new(),
            interactive: false,
            no_verify: false,
            rebasing: false,
            resolvemsg: None,
            binary_given: false,
            threeway: false,
            quiet: false,
            signoff: false,
            utf8: true,
            keep: Keep::False,
            empty: Empty::Stop,
            message_id: false,
            scissors: None,
            quoted_cr: None,
            rerere_autoupdate: None,
            apply_opts: Vec::new(),
            ignore_date: false,
            committer_date_is_author_date: false,
            gpg_sign: false,
        }
    }
}

/// A parse failure. git prints the message and exits 129 without usage text.
enum Usage {
    /// `-h`: `parse_options_step()` renders the block to **stdout** and exits
    /// 129, with no `error:` line — a help request is not a rejection.
    Help,
    /// `--help-all`: the same renderer with `USAGE_FULL`, which for `am` keeps
    /// the hidden `-b` and `--rebasing` entries.
    HelpAll,
    /// The `PARSE_OPT_ERROR` shape: the `error:` line alone, exit 129, no usage
    /// block. `PARSE_OPT_ERROR` is `-1`, what `get_arg()` and a rejecting callback
    /// return, and `parse_options()` exits on it without calling
    /// `usage_with_options()` — so a bad *value* for a known option gets one line.
    Error(String),
    /// The `PARSE_OPT_UNKNOWN` shape: the same `error:` line **followed by** the
    /// usage block, both on stderr, exit 129 (parse-options.c:1210-1224). An
    /// unrecognised option name is the only thing that takes this path.
    Unknown(String),
    /// An abbreviation two entries claim: the token as typed and the two candidate
    /// spellings. Unlike [`Usage::Error`] this one also prints the option block —
    /// `parse_long_opt()` returns `PARSE_OPT_HELP` after its `error()`, which
    /// routes to `usage_with_options_internal(..., USAGE_TO_STDOUT)`.
    Ambiguous(String, String, String),
}

/// The `am.*` config values `git_am_config` reads before option parsing. Only
/// the keys whose effect this port actually reproduces are carried: both feed a
/// state file `am_setup` writes (`threeway`, `messageid`). `am.keepcr` is
/// deliberately absent — it only governs `mailsplit` CR handling this port does
/// not implement, so honoring it would change nothing observable.
struct AmDefaults {
    threeway: bool,
    message_id: bool,
}

/// `git_am_config`: read `am.threeway`/`am.messageId` as booleans. A malformed
/// value is git's exact `git_config_bool` fatal, returned so `am` can exit 128
/// at config-read time. Keys are queried lowercased so the diagnostic matches
/// git's (which reports the normalized variable name).
fn am_config(repo: &gix::Repository) -> std::result::Result<AmDefaults, String> {
    let snapshot = repo.config_snapshot();
    let file = snapshot.plumbing();
    Ok(AmDefaults {
        threeway: config_bool(file, "am.threeway")?.unwrap_or(false),
        message_id: config_bool(file, "am.messageid")?.unwrap_or(false),
    })
}

fn config_bool(file: &gix::config::File, key: &str) -> std::result::Result<Option<bool>, String> {
    match file.boolean(key) {
        Ok(v) => Ok(v),
        Err(_) => {
            let raw = file
                .string(key)
                .map(|v| String::from_utf8_lossy(&v).into_owned())
                .unwrap_or_default();
            Err(format!("fatal: bad boolean config value '{raw}' for '{key}'"))
        }
    }
}

pub fn am(args: &[String]) -> Result<ExitCode> {
    // Dispatch strips the subcommand today; tolerate it being present at [0].
    let args: &[String] = match args.first() {
        Some(a) if a == "am" => &args[1..],
        _ => args,
    };

    // `git_config(git_am_config, ...)` runs before `parse_options`, so a
    // malformed `am.*` boolean is a config-time fatal (exit 128) that precedes
    // any CLI usage error (exit 129), and the config values become the option
    // defaults the command line then overrides.
    let repo = gix::discover(".")?;
    let defaults = match am_config(&repo) {
        Ok(d) => d,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(ExitCode::from(128));
        }
    };

    let opts = match parse(args, &defaults) {
        Ok(o) => o,
        Err(Usage::Help) => return Ok(super::show_usage(USAGE)),
        Err(Usage::HelpAll) => return Ok(super::show_usage(USAGE_ALL)),
        Err(Usage::Error(msg)) => {
            eprintln!("{msg}");
            return Ok(ExitCode::from(129));
        }
        Err(Usage::Unknown(msg)) => {
            eprintln!("{msg}");
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        Err(Usage::Ambiguous(tok, first, second)) => {
            return Ok(super::ambiguous_option(&tok, &first, &second, USAGE))
        }
    };

    // ```c
    // if (binary >= 0)
    //         fprintf_ln(stderr, _("The -b/--binary option has been a no-op for long time, and\n"
    //                              "it will be removed. Please do not use it anymore."));
    // ```
    // (builtin/am.c:2461-2464), immediately after `parse_options` and before any
    // session work — so it is printed even by an invocation that then dies.
    if opts.binary_given {
        eprintln!(
            "The -b/--binary option has been a no-op for long time, and\n\
             it will be removed. Please do not use it anymore."
        );
    }

    let state_dir = repo.git_dir().join("rebase-apply");

    // `am_in_progress`: the directory alone is not a session — `next` and `last`
    // are written last by `am_setup` precisely so they mark completion.
    let in_progress = state_dir.is_dir()
        && state_dir.join("last").is_file()
        && state_dir.join("next").is_file();

    if !in_progress {
        // A directory without `next`/`last` is wreckage from an interrupted
        // setup; only the two teardown verbs may clear it.
        if state_dir.exists() && !opts.rebasing {
            return match opts.resume.as_ref().map(|(r, _)| *r) {
                Some(Resume::Abort) | Some(Resume::Quit) => {
                    std::fs::remove_dir_all(&state_dir)?;
                    Ok(ExitCode::SUCCESS)
                }
                _ => {
                    eprintln!(
                        "fatal: Stray {} directory found.\nUse \"git am --abort\" to remove it.",
                        display_dir(&repo, &state_dir)
                    );
                    Ok(ExitCode::from(128))
                }
            };
        }

        if opts.resume.is_some() {
            eprintln!("fatal: Resolve operation not in progress, we are not resuming.");
            return Ok(ExitCode::from(128));
        }

        if opts.interactive && opts.paths.is_empty() {
            eprintln!("fatal: interactive mode requires patches on the command line");
            return Ok(ExitCode::from(128));
        }

        // `am_setup` splits the mailbox and writes the session, then `am_run`
        // applies it (the split messages are already on disk as `0001`, `0002`,
        // … so the loop reads them back rather than carrying them in memory).
        return match setup(&repo, &state_dir, &opts)? {
            Setup::Ready(_messages) => run_am_loop(&repo, &state_dir, &Cli::from_opts(&opts), false),
            Setup::Failed(code) => Ok(ExitCode::from(code)),
        };
    }

    // Catch a patch fed to a live session. git treats a non-tty stdin as an
    // attempt to pipe one in, even when it is `/dev/null`.
    if !opts.paths.is_empty() || (opts.resume.is_none() && !std::io::stdin().is_terminal()) {
        eprintln!(
            "fatal: previous rebase directory {} still exists but mbox given.",
            display_dir(&repo, &state_dir)
        );
        return Ok(ExitCode::from(128));
    }
    let resume = opts.resume.as_ref().map_or(Resume::Apply, |(r, _)| *r);
    let cli = Cli::from_opts(&opts);

    match resume {
        // `RESUME_FALSE`/`RESUME_APPLY` both land in `am_run`; a bare `git am`
        // inside a live session re-drives the current (previously stopped) patch.
        Resume::Apply => run_am_loop(&repo, &state_dir, &cli, true),
        Resume::ShowPatch(sub) => show_patch(&repo, &state_dir, sub),
        Resume::Quit => {
            // `am_rerere_clear()` then `am_destroy()`. Neither touches HEAD, the
            // index or the worktree — the session is simply forgotten.
            let merge_rr = repo.git_dir().join("MERGE_RR");
            if merge_rr.exists() {
                std::fs::remove_file(&merge_rr)?;
            }
            std::fs::remove_dir_all(&state_dir)?;
            Ok(ExitCode::SUCCESS)
        }
        // `am_resolve` (with/without `allow_empty`), `am_skip`, `am_abort`.
        Resume::Resolved => am_resolve(&repo, &state_dir, &cli, false),
        Resume::AllowEmpty => am_resolve(&repo, &state_dir, &cli, true),
        Resume::Skip => am_skip(&repo, &state_dir, &cli),
        Resume::Abort => am_abort(&repo, &state_dir),
        // git has no `--retry` verb; this port accepts the token in `parse` but
        // there is no faithful behavior to drive, so it stays an honest refusal.
        Resume::Retry => crate::git_fatal!(
            "`git am --retry` is not a git verb; there is no upstream behavior to port"
        ),
    }
}

// ---------------------------------------------------------------------------
// Option parsing
// ---------------------------------------------------------------------------

fn parse(args: &[String], defaults: &AmDefaults) -> Result<Opts, Usage> {
    // `am_state_init` sets these from `git_am_config` before `parse_options`
    // runs; a later `--3way`/`--no-3way`/`-m`/`--no-message-id` overrides them.
    let mut o = Opts {
        threeway: defaults.threeway,
        message_id: defaults.message_id,
        ..Opts::default()
    };
    let mut end_of_opts = false;
    let mut i = 0;

    while i < args.len() {
        let tok = args[i].as_str();
        i += 1;

        if end_of_opts || tok == "-" || !tok.starts_with('-') || tok.len() == 1 {
            o.paths.push(tok.to_string());
            continue;
        }
        if tok == "--" {
            end_of_opts = true;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): tested on the token as typed, after the `--`
        // break above and ahead of the abbreviation resolver, because it is a
        // `strcmp` — `--help-a` and `--help-all=x` stay unknown options.
        if tok == "--help-all" {
            return Err(Usage::HelpAll);
        }

        // Respell a unique abbreviation as the name it resolves to, so `--interact`
        // reaches the same arm as `--interactive`. `tok` itself is still what the
        // rejections below quote, since the resolver hands an unclaimed name back
        // untouched.
        let canonical = match super::canonical_long(tok, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Err(Usage::Ambiguous(tok.to_string(), first, second))
            }
        };
        let tok = canonical.as_ref();

        if let Some(long) = tok.strip_prefix("--") {
            let (name, attached) = match long.find('=') {
                Some(at) => (&long[..at], Some(&long[at + 1..])),
                None => (long, None),
            };
            parse_long(&mut o, tok, name, attached, args, &mut i)?;
        } else {
            parse_short(&mut o, &tok[1..], args, &mut i)?;
        }
    }

    Ok(o)
}

/// Take the value for an option that requires one: `--opt=v` or `--opt v`.
fn take_value<'a>(
    tok: &str,
    attached: Option<&'a str>,
    args: &'a [String],
    i: &mut usize,
) -> Result<&'a str, Usage> {
    if let Some(v) = attached {
        return Ok(v);
    }
    if *i < args.len() {
        let v = args[*i].as_str();
        *i += 1;
        return Ok(v);
    }
    Err(Usage::Error(format!("error: option `{}' requires a value", trim_dashes(tok))))
}

fn no_value(tok: &str, attached: Option<&str>) -> Result<(), Usage> {
    match attached {
        None => Ok(()),
        Some(_) => Err(Usage::Error(format!(
            "error: option `{}' takes no value",
            trim_dashes(tok)
        ))),
    }
}

fn trim_dashes(tok: &str) -> &str {
    let name = tok.trim_start_matches('-');
    match name.find('=') {
        Some(at) => &name[..at],
        None => name,
    }
}

fn parse_long(
    o: &mut Opts,
    tok: &str,
    name: &str,
    attached: Option<&str>,
    args: &[String],
    i: &mut usize,
) -> Result<(), Usage> {
    // `OPT_PASSTHRU_ARGV` records the option verbatim for `git apply`; the
    // negated form records `--no-<name>` rather than dropping the option.
    const PASSTHRU_ARG: &[&str] = &["whitespace", "directory", "exclude", "include"];
    const PASSTHRU_NOARG: &[&str] = &["ignore-space-change", "ignore-whitespace", "reject"];

    if let Some(base) = name.strip_prefix("no-") {
        if PASSTHRU_ARG.contains(&base) || PASSTHRU_NOARG.contains(&base) {
            no_value(tok, attached)?;
            o.apply_opts.push(format!("--no-{base}"));
            return Ok(());
        }
    }
    if PASSTHRU_ARG.contains(&name) {
        let v = take_value(tok, attached, args, i)?;
        o.apply_opts.push(format!("--{name}={v}"));
        return Ok(());
    }
    if PASSTHRU_NOARG.contains(&name) {
        no_value(tok, attached)?;
        o.apply_opts.push(format!("--{name}"));
        return Ok(());
    }

    match name {
        "interactive" => o.interactive = flag(tok, attached, true)?,
        "no-interactive" => o.interactive = flag(tok, attached, false)?,
        "3way" => o.threeway = flag(tok, attached, true)?,
        "no-3way" => o.threeway = flag(tok, attached, false)?,
        "quiet" => o.quiet = flag(tok, attached, true)?,
        "no-quiet" => o.quiet = flag(tok, attached, false)?,
        "signoff" => o.signoff = flag(tok, attached, true)?,
        "no-signoff" => o.signoff = flag(tok, attached, false)?,
        "utf8" => o.utf8 = flag(tok, attached, true)?,
        "no-utf8" => o.utf8 = flag(tok, attached, false)?,
        "keep" => {
            no_value(tok, attached)?;
            o.keep = Keep::True;
        }
        "no-keep" | "no-keep-non-patch" => {
            no_value(tok, attached)?;
            o.keep = Keep::False;
        }
        "keep-non-patch" => {
            no_value(tok, attached)?;
            o.keep = Keep::NonPatch;
        }
        "message-id" => o.message_id = flag(tok, attached, true)?,
        "no-message-id" => o.message_id = flag(tok, attached, false)?,
        // `keep-cr` is only consulted by mailsplit, which never sees a message here.
        "keep-cr" | "no-keep-cr" => no_value(tok, attached)?,
        "scissors" => o.scissors = Some(flag(tok, attached, true)?),
        "no-scissors" => o.scissors = Some(flag(tok, attached, false)?),
        "quoted-cr" => {
            let v = take_value(tok, attached, args, i)?;
            o.quoted_cr = Some(match v {
                "nowarn" => "nowarn",
                "warn" => "warn",
                "strip" => "strip",
                _ => {
                    return Err(Usage::Error(format!(
                        "error: bad action '{v}' for '--quoted-cr'"
                    )))
                }
            });
        }
        "patch-format" => {
            let v = take_value(tok, attached, args, i)?;
            o.format = Some(match v {
                "mbox" => Format::Mbox,
                "stgit" => Format::Stgit,
                "stgit-series" => Format::StgitSeries,
                "hg" => Format::Hg,
                "mboxrd" => Format::Mboxrd,
                _ => {
                    return Err(Usage::Error(format!(
                        "error: invalid value for '--patch-format': '{v}'"
                    )))
                }
            });
        }
        "no-patch-format" => {
            no_value(tok, attached)?;
            o.format = None;
        }
        "empty" => {
            let v = take_value(tok, attached, args, i)?;
            o.empty = match v {
                "stop" => Empty::Stop,
                "drop" => Empty::Drop,
                "keep" => Empty::Keep,
                _ => return Err(Usage::Error(format!("error: invalid value for '--empty': '{v}'"))),
            };
        }
        // `OPT_STRING(0, "resolvemsg", &state.resolvemsg, …)`: consulted only
        // when a patch fails to apply, where it *replaces* the whole
        // `--continue`/`--skip`/`--abort` hint block (builtin/am.c:1161-1184).
        // `git rebase --apply` passes its own `rebase_resolvemsg`, which is why a
        // conflicted `rebase --apply` says "git rebase --continue" and never
        // mentions `git am`.
        "resolvemsg" => {
            o.resolvemsg = Some(take_value(tok, attached, args, i)?.to_string());
        }
        "no-resolvemsg" => no_value(tok, attached)?,
        "rerere-autoupdate" => o.rerere_autoupdate = Some(flag(tok, attached, true)?),
        "no-rerere-autoupdate" => o.rerere_autoupdate = Some(flag(tok, attached, false)?),
        // These shape `do_commit`; captured so the apply loop can refuse rather
        // than commit with the wrong date/committer.
        "committer-date-is-author-date" => {
            o.committer_date_is_author_date = flag(tok, attached, true)?
        }
        "no-committer-date-is-author-date" => {
            o.committer_date_is_author_date = flag(tok, attached, false)?
        }
        "ignore-date" => o.ignore_date = flag(tok, attached, true)?,
        "no-ignore-date" => o.ignore_date = flag(tok, attached, false)?,
        // `OPT_BOOL('n', "no-verify", &state.no_verify, …)` — the sense of
        // the *name* is the value, so `--no-verify` sets it and `--verify`
        // clears it. It suppresses `applypatch-msg` and `pre-applypatch`;
        // `post-applypatch` runs either way.
        "no-verify" => o.no_verify = flag(tok, attached, true)?,
        "verify" => o.no_verify = flag(tok, attached, false)?,
        // `--binary` has been a documented no-op for years, but giving it still
        // prints a deprecation line (builtin/am.c:2461-2464). `binary` starts at
        // -1 and the notice fires on `binary >= 0`, i.e. for `--binary` and
        // `--no-binary` alike.
        "binary" | "no-binary" => {
            no_value(tok, attached)?;
            o.binary_given = true;
        }
        "gpg-sign" => o.gpg_sign = true, // optional value, attached only
        "no-gpg-sign" => {
            no_value(tok, attached)?;
            o.gpg_sign = false;
        }
        "rebasing" => {
            no_value(tok, attached)?;
            o.rebasing = true;
        }
        "no-rebasing" => {
            no_value(tok, attached)?;
            o.rebasing = false;
        }
        "continue" | "resolved" => cmdmode(o, tok, Resume::Resolved, attached)?,
        "skip" => cmdmode(o, tok, Resume::Skip, attached)?,
        "abort" => cmdmode(o, tok, Resume::Abort, attached)?,
        "quit" => cmdmode(o, tok, Resume::Quit, attached)?,
        "retry" => cmdmode(o, tok, Resume::Retry, attached)?,
        "allow-empty" => cmdmode(o, tok, Resume::AllowEmpty, attached)?,
        "show-current-patch" => {
            let sub = match attached {
                None | Some("raw") => Sub::Raw,
                Some("diff") => Sub::Diff,
                Some(v) => {
                    return Err(Usage::Error(format!(
                        "error: invalid value for '--show-current-patch': '{v}'"
                    )))
                }
            };
            cmdmode_checked(o, tok, Resume::ShowPatch(sub))?;
        }
        // `error(_("unknown option `%s'"), ctx.argv[0] + 2)` (parse-options.c:
        // 1215-1216) names the argument as typed, `=<value>` and all, so it
        // comes from `tok` rather than from the `name` split off it.
        _ => {
            return Err(Usage::Unknown(format!(
                "error: unknown option `{}'",
                tok.strip_prefix("--").unwrap_or(tok)
            )))
        }
    }
    Ok(())
}

/// `--opt`, `--opt=true` and `--opt=false` are the only accepted spellings for
/// an `OPT_BOOL`; a value is otherwise a usage error.
fn flag(tok: &str, attached: Option<&str>, on: bool) -> Result<bool, Usage> {
    no_value(tok, attached)?;
    Ok(on)
}

fn parse_short(
    o: &mut Opts,
    body: &str,
    args: &[String],
    i: &mut usize,
) -> Result<(), Usage> {
    // Every short option git defines is ASCII, so byte indices below are always
    // char boundaries and the `-C<n>`/`-p<num>` value slice cannot panic.
    if !body.is_ascii() {
        return Err(Usage::Unknown(format!("error: unknown switch `{body}'")));
    }
    let bytes = body.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let c = bytes[at] as char;
        at += 1;
        match c {
            'i' => o.interactive = true,
            '3' => o.threeway = true,
            'q' => o.quiet = true,
            's' => o.signoff = true,
            'u' => o.utf8 = true,
            'k' => o.keep = Keep::True,
            'm' => o.message_id = true,
            'c' => o.scissors = Some(true),
            'n' => o.no_verify = true,
            'b' => o.binary_given = true, // historical no-op, but still warns
            'r' => cmdmode(o, "-r", Resume::Resolved, None)?,
            // `-C<n>`/`-p<num>` take the rest of the token, or the next argument.
            'C' | 'p' => {
                let v = if at < bytes.len() {
                    let rest = &body[at..];
                    at = bytes.len();
                    rest.to_string()
                } else if *i < args.len() {
                    let v = args[*i].clone();
                    *i += 1;
                    v
                } else {
                    return Err(Usage::Error(format!("error: option `{c}' requires a value")));
                };
                o.apply_opts.push(format!("-{c}{v}"));
            }
            // `-S[<key-id>]` takes an optional attached value.
            'S' => {
                o.gpg_sign = true;
                at = bytes.len();
            }
            // parse_options_step() tests `internal_help` inside the
            // short-option loop, so `-h` answers from anywhere in a cluster.
            'h' => return Err(Usage::Help),
            _ => return Err(Usage::Unknown(format!("error: unknown switch `{c}'"))),
        }
    }
    Ok(())
}

/// `OPT_CMDMODE`: at most one resume verb, and the diagnostic quotes the two
/// argv tokens newest-first.
fn cmdmode(o: &mut Opts, tok: &str, want: Resume, attached: Option<&str>) -> Result<(), Usage> {
    no_value(tok, attached)?;
    cmdmode_checked(o, tok, want)
}

fn cmdmode_checked(o: &mut Opts, tok: &str, want: Resume) -> Result<(), Usage> {
    match &o.resume {
        Some((prev, prev_tok)) if *prev != want => Err(Usage::Error(format!(
            "error: options '{tok}' and '{prev_tok}' cannot be used together"
        ))),
        Some(_) => Ok(()),
        None => {
            o.resume = Some((want, tok.to_string()));
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// am_setup
// ---------------------------------------------------------------------------

enum Setup {
    /// The session directory is written; the vector holds the split messages
    /// (empty for the `git am </dev/null` case).
    Ready(Vec<Vec<u8>>),
    /// git printed a diagnostic and exits with this code.
    Failed(u8),
}

fn setup(repo: &gix::Repository, state_dir: &Path, o: &Opts) -> Result<Setup> {
    let format = match o.format {
        Some(f) => f,
        None => match detect_format(&o.paths)? {
            Detected::Format(f) => f,
            Detected::Unreadable(path, err) => {
                eprintln!(
                    "fatal: could not open '{path}' for reading: {}",
                    errno_msg(&err)
                );
                return Ok(Setup::Failed(128));
            }
            Detected::Unknown => {
                eprintln!("Patch format detection failed.");
                return Ok(Setup::Failed(128));
            }
        },
    };

    // `delete_ref(REBASE_HEAD)` runs before the split, so it happens even when
    // the split then fails.
    if repo.find_reference("REBASE_HEAD").is_ok() {
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
                message: Default::default(),
            },
            name: full_name("REBASE_HEAD")?,
            deref: false,
        })?;
    }

    let messages = match split_mail(format, &o.paths)? {
        Split::Failed(errors) => {
            // git creates the directory before splitting and `am_destroy`s it on
            // failure, so the net effect on the repository is nothing.
            for e in errors {
                eprintln!("error: {e}");
            }
            eprintln!("fatal: Failed to split patches.");
            return Ok(Setup::Failed(128));
        }
        Split::Messages(m) => m,
    };

    std::fs::create_dir_all(state_dir)?;

    // `mailsplit` numbers the messages `0001`, `0002`, … in the session; `am_run`
    // reads them back one at a time.
    for (n, msg) in messages.iter().enumerate() {
        std::fs::write(state_dir.join(format!("{:04}", n + 1)), msg)?;
    }

    write_bool(state_dir, "threeway", o.threeway || o.rebasing)?;
    write_bool(state_dir, "quiet", o.quiet)?;
    write_bool(state_dir, "sign", o.signoff)?;
    write_bool(state_dir, "utf8", o.utf8)?;
    if let Some(v) = o.rerere_autoupdate {
        write_bool(state_dir, "rerere-autoupdate", v)?;
    }
    write_text(
        state_dir,
        "keep",
        match o.keep {
            Keep::False => "f",
            Keep::True => "t",
            Keep::NonPatch => "b",
        },
    )?;
    write_bool(state_dir, "messageid", o.message_id)?;
    write_text(
        state_dir,
        "scissors",
        match o.scissors {
            None => "",
            Some(false) => "f",
            Some(true) => "t",
        },
    )?;
    write_text(state_dir, "quoted-cr", o.quoted_cr.unwrap_or(""))?;
    write_text(state_dir, "apply-opt", &sq_quote_argv(&o.apply_opts))?;
    write_text(state_dir, if o.rebasing { "rebasing" } else { "applying" }, "")?;

    match repo.head_id().ok().map(|id| id.detach()) {
        Some(head) => {
            write_text(state_dir, "abort-safety", &head.to_hex().to_string())?;
            if !o.rebasing {
                repo.edit_reference(RefEdit {
                    change: Change::Update {
                        log: LogChange {
                            mode: RefLog::AndReference,
                            force_create_reflog: false,
                            message: "am".into(),
                        },
                        expected: PreviousValue::Any,
                        new: Target::Object(head),
                    },
                    name: full_name("ORIG_HEAD")?,
                    deref: false,
                })?;
            }
        }
        None => {
            write_text(state_dir, "abort-safety", "")?;
            if !o.rebasing && repo.find_reference("ORIG_HEAD").is_ok() {
                repo.edit_reference(RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::Any,
                        log: RefLog::AndReference,
                        message: Default::default(),
                    },
                    name: full_name("ORIG_HEAD")?,
                    deref: false,
                })?;
            }
        }
    }

    // `next` and `last` are written last: they are what makes the directory a
    // session, so a crash before this point leaves a stray directory, not a
    // half-resumable one.
    write_text(state_dir, "next", "1")?;
    write_text(state_dir, "last", &messages.len().to_string())?;
    Ok(Setup::Ready(messages))
}

/// Outcome of `detect_patch_format`.
enum Detected {
    Format(Format),
    /// `xfopen` failed on the first path.
    Unreadable(String, std::io::Error),
    /// `PATCH_FORMAT_UNKNOWN`.
    Unknown,
}

fn detect_format(paths: &[String]) -> Result<Detected> {
    // git defaults to mbox for stdin and for directories, without reading them.
    let first = match paths.first() {
        None => return Ok(Detected::Format(Format::Mbox)),
        Some(p) => p.as_str(),
    };
    if first == "-" || Path::new(first).is_dir() {
        return Ok(Detected::Format(Format::Mbox));
    }

    let body = match std::fs::read(first) {
        Ok(b) => b,
        Err(e) => return Ok(Detected::Unreadable(first.to_string(), e)),
    };

    // `strbuf_getline` splits on LF and strips a trailing CR.
    let mut lines = body
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l));

    // The first non-blank line decides most formats on its own.
    let empty: &[u8] = b"";
    let l1 = lines.find(|l| !l.is_empty()).unwrap_or(empty);
    if l1.starts_with(b"From ") || l1.starts_with(b"From: ") {
        return Ok(Detected::Format(Format::Mbox));
    }
    if l1.starts_with(b"# This series applies on GIT commit") {
        return Ok(Detected::Format(Format::StgitSeries));
    }
    if l1 == b"# HG changeset patch" {
        return Ok(Detected::Format(Format::Hg));
    }
    // An all-blank file never reaches the StGit or `is_mail` probes: git guards
    // both on `l1.len`.
    if l1.is_empty() {
        return Ok(Detected::Unknown);
    }

    let l2 = lines.next().unwrap_or(empty);
    let l3 = lines.next().unwrap_or(empty);
    if l2.is_empty()
        && (l3.starts_with(b"From:") || l3.starts_with(b"Author:") || l3.starts_with(b"Date:"))
    {
        return Ok(Detected::Format(Format::Stgit));
    }

    if is_mail(&body) {
        return Ok(Detected::Format(Format::Mbox));
    }
    Ok(Detected::Unknown)
}

/// `is_mail()`: every non-indented line up to the first blank one must look like
/// an RFC 2822 header field name, i.e. match `^[!-9;-~]+:`.
fn is_mail(body: &[u8]) -> bool {
    for line in body.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break; // end of header
        }
        if line[0] == b'\t' || line[0] == b' ' {
            continue; // folded continuation
        }
        let name_len = line
            .iter()
            .take_while(|&&b| matches!(b, b'!'..=b'9' | b';'..=b'~'))
            .count();
        if name_len == 0 || line.get(name_len) != Some(&b':') {
            return false;
        }
    }
    true
}

/// The messages a mailbox holds — each already converted to mail form — or why
/// it could not be read.
enum Split {
    Messages(Vec<Vec<u8>>),
    Failed(Vec<String>),
}

/// `is_from_line()` (builtin/mailsplit.c): an mbox postmark. git does not look at the
/// object name at all — it looks for a date, requiring `HH:MM` digits around the last
/// colon on the line and a year past 90.
fn is_from_line(line: &[u8]) -> bool {
    if line.len() < 20 || !line.starts_with(b"From ") {
        return false;
    }
    // git scans back from `line + len - 2`, i.e. skipping the last byte.
    let end = line.len() - 1;
    let Some(colon) = line[5..end].iter().rposition(|b| *b == b':').map(|i| i + 5) else {
        return false;
    };
    let digit = |i: usize| line.get(i).is_some_and(u8::is_ascii_digit);
    if colon < 4 || !digit(colon - 4) || !digit(colon - 2) || !digit(colon - 1) {
        return false;
    }
    if !digit(colon + 1) || !digit(colon + 2) {
        return false;
    }
    // The year follows the time; anything at or below 90 is not a date git accepts.
    let tail = &line[colon + 3..];
    let year: i64 = std::str::from_utf8(tail)
        .ok()
        .and_then(|s| s.trim_start().split_whitespace().last().and_then(|w| w.parse().ok()))
        .unwrap_or(0);
    year > 90
}

/// `split_one()`: cut an mbox into its messages at the postmark lines, each message
/// keeping the postmark it starts with. Content before the first postmark is a message
/// of its own, which is how a bare patch file (git's `mailsplit -b`) reaches `am`.
fn split_mbox_body(body: &[u8]) -> Vec<Vec<u8>> {
    let mut msgs: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    for line in body.split_inclusive(|b| *b == b'\n') {
        let bare = line.strip_suffix(b"\n").unwrap_or(line);
        if is_from_line(bare) && !current.is_empty() {
            msgs.push(std::mem::take(&mut current));
        }
        current.extend_from_slice(line);
    }
    if !current.is_empty() {
        msgs.push(current);
    }
    msgs
}

fn split_mail(format: Format, paths: &[String]) -> Result<Split> {
    match format {
        Format::Mbox | Format::Mboxrd => split_mbox(paths),
        // `split_mail_conv` writes one message per input path, converting it;
        // with no paths it reads stdin as a single patch.
        Format::Stgit => split_conv(paths, convert_stgit),
        Format::Hg => split_conv(paths, convert_hg),
        Format::StgitSeries => split_stgit_series(paths),
    }
}

/// `git mailsplit`: each path is an mbox file or a Maildir, and no path at all
/// means stdin. The fixtures never carry an mbox `From ` envelope, so each
/// non-empty source contributes exactly one message (its whole body); a real
/// multi-message mbox would need envelope splitting this does not do.
fn split_mbox(paths: &[String]) -> Result<Split> {
    let mut msgs: Vec<Vec<u8>> = Vec::new();
    if paths.is_empty() {
        msgs.extend(split_mbox_body(&read_stdin()?));
        return Ok(Split::Messages(msgs));
    }
    for p in paths {
        if p == "-" {
            msgs.extend(split_mbox_body(&read_stdin()?));
            continue;
        }
        let path = Path::new(p);
        if path.is_dir() {
            // `populate_maildir_list` reads `new/` then `cur/`, ignoring dotfiles.
            for sub in ["new", "cur"] {
                if let Ok(entries) = std::fs::read_dir(path.join(sub)) {
                    let mut files: Vec<_> = entries
                        .filter_map(Result::ok)
                        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                        .map(|e| e.path())
                        .collect();
                    files.sort();
                    for f in files {
                        msgs.push(std::fs::read(&f).unwrap_or_default());
                    }
                }
            }
            continue;
        }
        match std::fs::read(path) {
            Ok(body) => msgs.extend(split_mbox_body(&body)),
            Err(e) => {
                return Ok(Split::Failed(vec![format!(
                    "cannot stat {p}: {}",
                    errno_msg(&e)
                )]))
            }
        }
    }
    Ok(Split::Messages(msgs))
}

/// `split_mail_conv`: one output message per input path, stdin when none. The
/// converter (`stgit`/`hg`) turns each source into mail form.
fn split_conv(paths: &[String], conv: fn(&[u8]) -> Vec<u8>) -> Result<Split> {
    if paths.is_empty() {
        return Ok(Split::Messages(vec![conv(&read_stdin()?)]));
    }
    let mut msgs: Vec<Vec<u8>> = Vec::new();
    for p in paths {
        if p == "-" {
            msgs.push(conv(&read_stdin()?));
            continue;
        }
        // git has already written the messages for the preceding paths, but the
        // caller destroys the whole session directory on failure.
        match std::fs::read(p) {
            Ok(body) => msgs.push(conv(&body)),
            Err(e) => {
                return Ok(Split::Failed(vec![format!(
                    "could not open '{p}' for reading: {}",
                    errno_msg(&e)
                )]))
            }
        }
    }
    Ok(Split::Messages(msgs))
}

/// `split_mail_stgit_series`: one series file listing patch files beside it.
fn split_stgit_series(paths: &[String]) -> Result<Split> {
    if paths.len() != 1 {
        return Ok(Split::Failed(vec![
            "Only one StGIT patch series can be applied at once".to_string(),
        ]));
    }
    let series = Path::new(&paths[0]);
    let body = match std::fs::read(series) {
        Ok(b) => b,
        Err(e) => {
            return Ok(Split::Failed(vec![format!(
                "could not open '{}' for reading: {}",
                paths[0],
                errno_msg(&e)
            )]))
        }
    };
    // `dirname()` of a bare filename is `.`, which is what git prefixes with.
    let dir = match series.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    // `strbuf_getline_lf` yields no entry for the newline that ends the last
    // line, but a genuinely blank line in the middle is an entry.
    let body = body.strip_suffix(b"\n").unwrap_or(&body);
    let mut listed: Vec<String> = Vec::new();
    for line in body.split(|&b| b == b'\n') {
        if line.first() == Some(&b'#') {
            continue; // comment line
        }
        listed.push(dir.join(line.as_bstr().to_string()).display().to_string());
    }
    // The listed patches are themselves StGit patches.
    split_conv(&listed, convert_stgit)
}

/// `stgit_patch_to_mail`: the first line becomes the `Subject`, `From:`/`Author:`
/// and `Date:` become mail headers, and the remainder is the body. Only the
/// header/subject/body shape matters downstream, so the copy is byte-faithful
/// enough for `is_empty`/`Subject` detection.
fn convert_stgit(input: &[u8]) -> Vec<u8> {
    let lines = getlines(input);
    let mut out: Vec<u8> = Vec::new();
    let mut subject_printed = false;
    let mut it = lines.iter();
    while let Some(line) = it.next() {
        if let Some(v) = strip(line, b"From: ").or_else(|| strip(line, b"Author: ")) {
            out.extend_from_slice(b"From: ");
            out.extend_from_slice(v);
            out.push(b'\n');
        } else if let Some(v) = strip(line, b"Date: ") {
            out.extend_from_slice(b"Date: ");
            out.extend_from_slice(v);
            out.push(b'\n');
        } else if !subject_printed {
            out.extend_from_slice(b"Subject: ");
            out.extend_from_slice(line);
            out.push(b'\n');
            subject_printed = true;
        } else {
            out.push(b'\n');
            out.extend_from_slice(line);
            out.push(b'\n');
            for rest in it {
                out.extend_from_slice(rest);
                out.push(b'\n');
            }
            break;
        }
    }
    out
}

/// `hg_patch_to_mail`: `# User`/`# Date` become headers, other `# ` lines are
/// dropped, and the first ordinary line starts the body.
fn convert_hg(input: &[u8]) -> Vec<u8> {
    let lines = getlines(input);
    let mut out: Vec<u8> = Vec::new();
    let mut it = lines.iter();
    while let Some(line) = it.next() {
        if let Some(v) = strip(line, b"# User ") {
            out.extend_from_slice(b"From: ");
            out.extend_from_slice(v);
            out.push(b'\n');
        } else if let Some(v) = strip(line, b"# Date ") {
            // git reformats the timestamp; only its presence matters here.
            out.extend_from_slice(b"Date: ");
            out.extend_from_slice(v);
            out.push(b'\n');
        } else if line.starts_with(b"# ") {
            continue;
        } else {
            out.push(b'\n');
            out.extend_from_slice(line);
            out.push(b'\n');
            for rest in it {
                out.extend_from_slice(rest);
                out.push(b'\n');
            }
            break;
        }
    }
    out
}

/// `strbuf_getline_lf` over a buffer: split on LF, and drop the empty trailing
/// element a final newline would otherwise produce. Empty input yields no lines.
fn getlines(input: &[u8]) -> Vec<&[u8]> {
    if input.is_empty() {
        return Vec::new();
    }
    let body = input.strip_suffix(b"\n").unwrap_or(input);
    body.split(|&b| b == b'\n').collect()
}

fn strip<'a>(line: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    line.strip_prefix(prefix)
}

// ---------------------------------------------------------------------------
// am_run
// ---------------------------------------------------------------------------

/// `am_run`'s pre-flight, shared by the fresh and resume paths: report unmerged
/// entries on stdout, then refuse a dirty index. `Some(code)` means git has
/// already stopped here; `None` means the apply loop may proceed.
fn preflight(repo: &gix::Repository, state_dir: &Path) -> Result<Option<ExitCode>> {
    let dirty_marker = state_dir.join("dirtyindex");
    if dirty_marker.exists() {
        std::fs::remove_file(&dirty_marker)?;
    }

    let index = repo.index_or_empty()?;
    let state: &gix::index::State = &index;

    // `refresh_index` under `REFRESH_QUIET` still reports unmerged entries, once
    // per path, on stdout.
    {
        let mut out = std::io::stdout().lock();
        let mut reported: BTreeSet<BString> = BTreeSet::new();
        for e in state.entries() {
            if e.stage_raw() == 0 {
                continue;
            }
            let path = e.path(state).to_owned();
            if reported.insert(path.clone()) {
                writeln!(out, "{path}: needs merge")?;
            }
        }
    }

    let dirty = dirty_paths(repo, state)?;
    if !dirty.is_empty() {
        write_bool(state_dir, "dirtyindex", true)?;
        let list: Vec<String> = dirty.iter().map(|p| p.to_string()).collect();
        eprintln!(
            "fatal: Dirty index: cannot apply patches (dirty: {})",
            list.join(" ")
        );
        return Ok(Some(ExitCode::from(128)));
    }
    Ok(None)
}

/// The CLI-only knobs `am_load` never persists (`empty_type`, `--interactive`,
/// and the `do_commit`-shaping flags), threaded through the loop and the resume
/// verbs from the current command line.
struct Cli {
    empty: Empty,
    interactive: bool,
    no_verify: bool,
    resolvemsg: Option<String>,
    ignore_date: bool,
    committer_date_is_author_date: bool,
    gpg_sign: bool,
}

impl Cli {
    fn from_opts(o: &Opts) -> Self {
        Self {
            empty: o.empty,
            interactive: o.interactive,
            no_verify: o.no_verify,
            resolvemsg: o.resolvemsg.clone(),
            ignore_date: o.ignore_date,
            committer_date_is_author_date: o.committer_date_is_author_date,
            gpg_sign: o.gpg_sign,
        }
    }
}

/// How to re-invoke this binary for a ported subcommand. git's `am` shells out
/// to `git mailinfo`/`git apply`/`git write-tree`/`git commit-tree`/… and always
/// runs from the worktree root; mirror that by running the child from the
/// worktree with the state directory addressed relative to it, so a diagnostic
/// like `error: empty patch: '.git/rebase-apply/patch'` reads as git's does.
struct Ctx {
    exe: PathBuf,
    cwd: Option<PathBuf>,
    sdir: PathBuf,
    /// The same directory as an absolute path, for the `GIT_INDEX_FILE` the
    /// three-way fallback's scratch index needs: a child that inherits it
    /// resolves it against its *own* cwd, which is not always the worktree root.
    sdir_abs: PathBuf,
}

impl Ctx {
    fn new(repo: &gix::Repository, state_dir: &Path) -> Result<Ctx> {
        let exe = std::env::current_exe()
            .map_err(|e| anyhow::anyhow!("cannot locate the running executable: {e}"))?;
        let (cwd, sdir) = match repo.workdir() {
            Some(w) if state_dir.starts_with(w) => (
                Some(w.to_path_buf()),
                state_dir.strip_prefix(w).unwrap_or(state_dir).to_path_buf(),
            ),
            _ => (None, state_dir.to_path_buf()),
        };
        let sdir_abs = if state_dir.is_absolute() {
            state_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|d| d.join(state_dir))
                .unwrap_or_else(|_| state_dir.to_path_buf())
        };
        Ok(Ctx {
            exe,
            cwd,
            sdir,
            sdir_abs,
        })
    }

    /// A child running `git <sub>` from the worktree root.
    fn cmd(&self, sub: &str) -> Command {
        let mut c = Command::new(&self.exe);
        c.arg(sub);
        if let Some(w) = &self.cwd {
            c.current_dir(w);
        }
        c
    }

    /// The `.git/rebase-apply/<name>` argument as the child should see it.
    fn spath(&self, name: &str) -> PathBuf {
        self.sdir.join(name)
    }
}

/// The session settings `am_load` reads back from the state directory. Both a
/// fresh run (right after `am_setup` wrote them) and a resume read the same
/// files, so the apply loop behaves identically in either entry path.
struct Loaded {
    threeway: bool,
    quiet: bool,
    signoff: bool,
    utf8: bool,
    keep: Keep,
    message_id: bool,
    scissors: Option<bool>,
    quoted_cr: String,
    apply_opts: Vec<String>,
    rebasing: bool,
}

fn read_state(state_dir: &Path, name: &str) -> String {
    std::fs::read_to_string(state_dir.join(name))
        .map(|s| s.trim_end_matches('\n').to_string())
        .unwrap_or_default()
}

/// `am_load`, restricted to the fields the apply loop consumes.
fn load_state(state_dir: &Path) -> Loaded {
    Loaded {
        threeway: read_state(state_dir, "threeway") == "t",
        quiet: read_state(state_dir, "quiet") == "t",
        signoff: read_state(state_dir, "sign") == "t",
        utf8: read_state(state_dir, "utf8") == "t",
        keep: match read_state(state_dir, "keep").as_str() {
            "t" => Keep::True,
            "b" => Keep::NonPatch,
            _ => Keep::False,
        },
        message_id: read_state(state_dir, "messageid") == "t",
        scissors: match read_state(state_dir, "scissors").as_str() {
            "t" => Some(true),
            "f" => Some(false),
            _ => None,
        },
        quoted_cr: read_state(state_dir, "quoted-cr"),
        apply_opts: sq_dequote(&read_state(state_dir, "apply-opt")),
        rebasing: state_dir.join("rebasing").exists(),
    }
}

/// The authorship and message `parse_mail` extracts (or `am_load` reads back
/// from `author-script`/`final-commit` when resuming).
struct CommitInfo {
    msg: Vec<u8>,
    author_name: String,
    author_email: String,
    author_date: String,
}

/// `am_run`: apply every queued mail. `resume` marks the first iteration as a
/// live resume (`RESUME_APPLY`) — the current patch's `author-script`/
/// `final-commit`/`patch` are reused rather than re-parsed, but it is still
/// re-applied. A clean patch is applied with `git apply --index` and committed
/// preserving the mail's authorship; anything needing unported substrate refuses
/// before it could write a wrong commit or a wrong worktree.
fn run_am_loop(
    repo: &gix::Repository,
    state_dir: &Path,
    cli: &Cli,
    mut resume: bool,
) -> Result<ExitCode> {
    if let Some(code) = preflight(repo, state_dir)? {
        return Ok(code);
    }

    let ctx = Ctx::new(repo, state_dir)?;
    let ld = load_state(state_dir);

    let mut cur = read_count(state_dir, "next")?;
    let last = read_count(state_dir, "last")?;

    while cur <= last {
        let mail = state_dir.join(format!("{cur:04}"));
        if !mail.exists() {
            am_next(repo, state_dir, &mut cur)?;
            resume = false;
            continue;
        }

        let info = if resume {
            match load_current(repo, state_dir)? {
                Some(ci) => ci,
                None => return Ok(ExitCode::from(128)),
            }
        } else {
            // `if (state->rebasing) skip = parse_mail_rebase(…); else skip =
            // parse_mail(…);` (builtin/am.c:1845). `parse_mail_rebase` never
            // skips, so only the `parse_mail` arm has a `goto next`.
            let mut ci = if ld.rebasing {
                match parse_mail_rebase(&ctx, repo, state_dir, &mail)? {
                    Some(ci) => ci,
                    None => return Ok(ExitCode::from(128)),
                }
            } else {
                match parse_mail(&ctx, state_dir, &ld, &mail)? {
                    ParseOutcome::Skip => {
                        am_next(repo, state_dir, &mut cur)?;
                        resume = false;
                        continue;
                    }
                    ParseOutcome::Died(code) => return Ok(code),
                    ParseOutcome::Parsed(ci) => ci,
                }
            };
            // `am --signoff`: the trailer goes on before the message is stored, so
            // `final-commit` (and a later `--continue`) already carries it. git
            // appends it after either parse arm, so a `--rebasing --signoff`
            // replay gets it too.
            if ld.signoff {
                let sig = repo
                    .committer()
                    .transpose()?
                    .ok_or_else(|| anyhow::anyhow!("unable to auto-detect email address"))?;
                let ident = format!("{} <{}>", sig.name, sig.email);
                let mut msg = String::from_utf8_lossy(&ci.msg).into_owned();
                super::commit::append_signoff(&mut msg, &ident, 0, true);
                ci.msg = msg.into_bytes();
            }
            write_author_script(state_dir, &ci)?;
            // `write_commit_msg(state)` (builtin/am.c:1858), unconditional.
            std::fs::write(state_dir.join("final-commit"), &ci.msg)?;
            ci
        };
        let mut info = info;

        if cli.interactive {
            bail!(
                "`git am -i` interactive mode is not ported: it drives a per-patch \
                 [y]es/[n]o/[e]dit/[v] tty prompt loop that cannot run unattended"
            );
        }

        let first = first_line(&info.msg);
        let patch_empty = is_empty_or_missing(&state_dir.join("patch"));
        let mut to_keep = false;

        if patch_empty {
            match cli.empty {
                Empty::Drop => {
                    if !ld.quiet {
                        println!("Skipping: {first}");
                    }
                    am_next(repo, state_dir, &mut cur)?;
                    resume = false;
                    continue;
                }
                Empty::Keep => {
                    to_keep = true;
                    if !ld.quiet {
                        println!("Creating an empty commit: {first}");
                    }
                }
                Empty::Stop => {
                    println!("Patch is empty.");
                    return die_user_resolve(repo, state_dir, cli);
                }
            }
        }

        // `if (run_applypatch_msg_hook(state)) exit(1);` (builtin/am.c:1889) —
        // after the empty-patch decision and before either the commit shortcut
        // or the apply, so a `--empty=keep` commit runs it too.
        if !run_applypatch_msg_hook(repo, state_dir, cli.no_verify, &mut info)? {
            return Ok(ExitCode::from(1));
        }
        let first = first_line(&info.msg);

        if !to_keep {
            if !ld.quiet {
                println!("Applying: {first}");
            }
            if !run_apply(&ctx, &ld, None)? {
                // `--3way` (and therefore every `--rebasing` session, which
                // `am_setup` forces threeway on) reconstructs a base tree from
                // the patch's own index lines and merges instead of giving up.
                let recovered = if ld.threeway {
                    let merged = fall_back_threeway(&ctx, repo, &ld, &info.msg)?;
                    // "Applying the patch to an earlier tree and merging the
                    // result may have produced the same tree as ours."
                    if merged && index_has_no_changes(repo)? {
                        if !ld.quiet {
                            println!("No changes -- Patch already applied.");
                        }
                        am_next(repo, state_dir, &mut cur)?;
                        resume = false;
                        continue;
                    }
                    merged
                } else {
                    false
                };
                if !recovered {
                    println!("Patch failed at {cur:04} {first}");
                    if crate::advice::enabled("amWorkDir") {
                        eprintln!(
                            "hint: Use 'git am --show-current-patch=diff' to see the failed patch"
                        );
                    }
                    return die_user_resolve(repo, state_dir, cli);
                }
            }
        }

        if cli.gpg_sign {
            bail!(
                "`git am -S` is not ported: signing the commit it writes needs the signing \
                 path `commit-tree` does not expose here"
            );
        }

        if let Some(code) = do_commit(
            &ctx,
            repo,
            state_dir,
            &info,
            &ld,
            cli.no_verify,
            cli.ignore_date,
            cli.committer_date_is_author_date,
        )? {
            return Ok(code);
        }

        am_next(repo, state_dir, &mut cur)?;
        resume = false;
    }

    finish_am_run(repo, state_dir, &ld)
}

/// `am_run`'s tail (builtin/am.c:1930-1946): the `rewritten` list is replayed to
/// notes and the `post-rewrite` hook, and only a non-`--rebasing` session tears
/// its own directory down — under `--rebasing` "it's up to the caller to take
/// care of housekeeping", which is why `git rebase --apply` still finds
/// `.git/rebase-apply` after `am` exits 0.
fn finish_am_run(repo: &gix::Repository, state_dir: &Path, ld: &Loaded) -> Result<ExitCode> {
    let rewritten = state_dir.join("rewritten");
    if !is_empty_or_missing(&rewritten) {
        // `copy_notes_for_rebase()` copies notes from each old commit to its
        // replacement. Nothing in this port rewrites notes and no
        // `notes.rewriteRef` is honoured anywhere, so with none configured stock
        // copies nothing either and the step is a no-op rather than a divergence.
        let payload = std::fs::read(&rewritten)?;
        let _ = crate::hooks::run(repo, "post-rewrite", &["rebase"], Some(&payload));
    }
    if !ld.rebasing {
        std::fs::remove_dir_all(state_dir)?;
        super::maintenance::run_auto_maintenance(repo, ld.quiet)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Outcome of running the mail through `git mailinfo`.
enum ParseOutcome {
    /// git's `parse_mail` returned 1 (pine folder data) — skip this message.
    Skip,
    /// `mailinfo` failed; git dies `could not parse patch`. Carries the exit code.
    Died(ExitCode),
    Parsed(CommitInfo),
}

/// `parse_mail`: run `git mailinfo <flags> msg patch < mail > info`, then read the
/// authorship/subject back out of `info` and assemble `final-commit`. The flags
/// mirror how `am` configures `struct mailinfo` from the loaded state.
fn parse_mail(ctx: &Ctx, state_dir: &Path, ld: &Loaded, mail: &Path) -> Result<ParseOutcome> {
    let info_file = state_dir.join("info");

    let input = std::fs::File::open(mail)
        .map_err(|e| anyhow::anyhow!("cannot open {mail:?}: {e}"))?;
    let info_out = std::fs::File::create(&info_file)
        .map_err(|e| anyhow::anyhow!("cannot create {info_file:?}: {e}"))?;

    let mut c = ctx.cmd("mailinfo");
    match ld.keep {
        Keep::True => {
            c.arg("-k");
        }
        Keep::NonPatch => {
            c.arg("-b");
        }
        Keep::False => {}
    }
    if ld.message_id {
        c.arg("-m");
    }
    if !ld.utf8 {
        c.arg("-n");
    }
    match ld.scissors {
        Some(true) => {
            c.arg("--scissors");
        }
        Some(false) => {
            c.arg("--no-scissors");
        }
        None => {}
    }
    if !ld.quoted_cr.is_empty() {
        c.arg(format!("--quoted-cr={}", ld.quoted_cr));
    }
    c.arg(ctx.spath("msg"))
        .arg(ctx.spath("patch"))
        .stdin(input)
        .stdout(info_out)
        .stderr(Stdio::inherit());

    let ok = c
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run mailinfo: {e}"))?
        .success();
    if !ok {
        // git: `if (mailinfo(...)) die("could not parse patch")`. mailinfo has
        // already reported the specific reason (`error: empty patch: '<path>'`).
        eprintln!("fatal: could not parse patch");
        return Ok(ParseOutcome::Died(ExitCode::from(128)));
    }

    // Extract Subject/Author/Email/Date from the info block.
    let info = std::fs::read(&info_file).unwrap_or_default();
    let mut subjects: Vec<Vec<u8>> = Vec::new();
    let mut author_name = String::new();
    let mut author_email = String::new();
    let mut author_date = String::new();
    for line in info.split(|&b| b == b'\n') {
        if let Some(v) = line.strip_prefix(b"Subject: ") {
            subjects.push(v.to_vec());
        } else if let Some(v) = line.strip_prefix(b"Author: ") {
            author_name = String::from_utf8_lossy(v).into_owned();
        } else if let Some(v) = line.strip_prefix(b"Email: ") {
            author_email = String::from_utf8_lossy(v).into_owned();
        } else if let Some(v) = line.strip_prefix(b"Date: ") {
            author_date = String::from_utf8_lossy(v).into_owned();
        }
    }

    // git skips pine's internal folder marker.
    if author_name == "Mail System Internal Data" {
        return Ok(ParseOutcome::Skip);
    }

    // msg = <subjects joined by LF> + "\n\n" + <mailinfo body>, then stripspace.
    let mut msg: Vec<u8> = Vec::new();
    for (i, s) in subjects.iter().enumerate() {
        if i > 0 {
            msg.push(b'\n');
        }
        msg.extend_from_slice(s);
    }
    msg.extend_from_slice(b"\n\n");
    msg.extend_from_slice(&std::fs::read(state_dir.join("msg")).unwrap_or_default());
    let msg = stripspace(ctx, &msg)?;

    // write_commit_msg: `final-commit` holds the exact bytes.
    std::fs::write(state_dir.join("final-commit"), &msg)?;

    Ok(ParseOutcome::Parsed(CommitInfo {
        msg,
        author_name,
        author_email,
        author_date,
    }))
}

/// `strbuf_stripspace(&msg, 0)` == `git stripspace`.
fn stripspace(ctx: &Ctx, input: &[u8]) -> Result<Vec<u8>> {
    let mut child = ctx
        .cmd("stripspace")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to run stripspace: {e}"))?;
    child
        .stdin
        .take()
        .expect("stripspace stdin was piped")
        .write_all(input)?;
    let out = child
        .wait_with_output()
        .map_err(|e| anyhow::anyhow!("failed to run stripspace: {e}"))?;
    Ok(out.stdout)
}

/// `run_apply` (builtin/am.c:1489): `git apply <apply-opts> <patch>`.
///
/// `index_file` is git's second parameter. `NULL` sets `apply_state.check_index`
/// — the `--index` spelling, applying to index *and* worktree — while a path
/// sets `apply_state.index_file` plus `apply_state.cached`, i.e. `--cached`
/// against that index, touching no file in the worktree. The second form is only
/// used by [`fall_back_threeway`], which builds two throw-away trees in a
/// scratch index before any of it reaches the user's files.
///
/// Under `--3way` git also silences the first attempt (`apply_verbosity =
/// verbosity_silent`), because a patch that fails here is expected to succeed
/// through the fallback and its complaints would be noise.
///
/// Returns whether the patch applied cleanly; the child's own diagnostics reach
/// stderr.
fn run_apply(ctx: &Ctx, ld: &Loaded, index_file: Option<&Path>) -> Result<bool> {
    let mut c = ctx.cmd("apply");
    match index_file {
        Some(path) => {
            c.arg("--cached").env("GIT_INDEX_FILE", path);
        }
        None => {
            c.arg("--index");
        }
    }
    for opt in &ld.apply_opts {
        c.arg(opt);
    }
    c.arg(ctx.spath("patch"));
    if ld.threeway && index_file.is_none() {
        c.stderr(Stdio::null());
    }
    Ok(c
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run apply: {e}"))?
        .success())
}

/// `fall_back_threeway` (builtin/am.c:1560): what `am -3` does when the patch
/// does not apply to the current tree.
///
/// The patch carries the pre-image blob of every hunk it touches in its `index`
/// lines, so `git apply --build-fake-ancestor` can reconstruct the tree the
/// patch *was* written against — for the paths the patch touches, and only
/// those. That reconstructed tree is the merge base; `HEAD` is ours; the patch
/// applied *to the reconstructed tree* is theirs. Merging the three lands the
/// patch's intent on the current tree, or leaves conflict markers where it
/// cannot.
///
/// git runs the two intermediate applies against a scratch index
/// (`.git/rebase-apply/patch-merge-index`) so a failure never touches the
/// worktree, and only the final merge is checked out.
///
/// Returns `Ok(true)` when the merge produced a clean result, `Ok(false)` when
/// it stopped — either arm having already printed what git prints.
fn fall_back_threeway(ctx: &Ctx, repo: &gix::Repository, ld: &Loaded, msg: &[u8]) -> Result<bool> {
    let index_path = ctx.sdir_abs.join("patch-merge-index");
    let _ = std::fs::remove_file(&index_path);

    // `repo_get_oid("HEAD")` falling back to the empty tree: an `am` onto an
    // unborn branch merges against nothing. git hands the *commit* to
    // `merge_ort_generic`, which peels it; this port's tree-merge takes trees
    // directly, so the peel happens here instead.
    let our_tree = match repo.head_id().ok().map(|id| id.detach()) {
        Some(head) => repo.find_commit(head)?.tree_id()?.detach(),
        None => repo.object_hash().empty_tree(),
    };

    let mut fake = ctx.cmd("apply");
    fake.arg(format!("--build-fake-ancestor={}", index_path.display()));
    for opt in &ld.apply_opts {
        fake.arg(opt);
    }
    fake.arg(ctx.spath("patch"));
    if !fake
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run apply: {e}"))?
        .success()
    {
        eprintln!("error: could not build fake ancestor");
        return Ok(false);
    }

    let base_tree = match capture(write_tree_in(ctx, &index_path))? {
        Some(t) => t,
        None => {
            eprintln!("error: Repository lacks necessary blobs to fall back on 3-way merge.");
            return Ok(false);
        }
    };

    if !ld.quiet {
        println!("Using index info to reconstruct a base tree...");
        // `run_diff_index(DIFF_INDEX_CACHED)` filtered to A/M against `HEAD`:
        // the paths that needed reconstructing, so the user knows where to look
        // for a mismerge.
        let _ = ctx
            .cmd("diff-index")
            .arg("--cached")
            .arg("--name-status")
            .arg("--diff-filter=AM")
            .arg("HEAD")
            .env("GIT_INDEX_FILE", &index_path)
            .status();
    }

    if !run_apply(ctx, ld, Some(&index_path))? {
        eprintln!(
            "error: Did you hand edit your patch?\nIt does not apply to blobs recorded in its \
             index."
        );
        return Ok(false);
    }

    let their_tree = match capture(write_tree_in(ctx, &index_path))? {
        Some(t) => t,
        None => {
            eprintln!("error: could not write tree");
            return Ok(false);
        }
    };

    if !ld.quiet {
        println!("Falling back to patching base and 3-way merge...");
    }

    // `o.branch1 = "HEAD"`, `o.branch2 = <first line of the message>`,
    // `o.ancestor = "constructed fake ancestor"` — the labels that end up in the
    // conflict markers.
    let their_label = first_line(msg);
    let labels = gix::merge::blob::builtin_driver::text::Labels {
        ancestor: Some(BStr::new(b"constructed fake ancestor")),
        current: Some(BStr::new(b"HEAD")),
        other: Some(BStr::new(their_label.as_bytes())),
    };
    let old_index = repo.index_or_load_from_head()?.into_owned();
    let applied = crate::merge_apply::three_way_merge_verbose(
        repo,
        oid_of(&base_tree)?,
        our_tree,
        oid_of(&their_tree)?,
        &old_index,
        labels,
        &std::sync::atomic::AtomicBool::default(),
        !ld.quiet,
    )?;
    let mut index = applied.index;
    index.write(Default::default())?;
    // `merge_switch_to_result()` records the result either way (merge-ort.c:4950).
    crate::merge_apply::write_auto_merge(repo, applied.tree_id)?;

    if !applied.conflicts.is_empty() {
        eprintln!("error: Failed to merge in the changes.");
        return Ok(false);
    }
    Ok(true)
}

/// `write_index_as_tree(..., index_path, ...)`: `git write-tree` reading the
/// scratch index rather than the repository's.
fn write_tree_in(ctx: &Ctx, index_path: &Path) -> Command {
    let mut c = ctx.cmd("write-tree");
    c.env("GIT_INDEX_FILE", index_path);
    c
}

fn oid_of(hex: &str) -> Result<ObjectId> {
    ObjectId::from_hex(hex.trim().as_bytes())
        .map_err(|e| anyhow::anyhow!("could not parse object name {hex:?}: {e}"))
}

/// `get_mail_commit_oid` (builtin/am.c:88): under `--rebasing` the mailbox is
/// `git format-patch --stdout` output, whose `From <oid> Mon Sep 17 00:00:00
/// 2001` postmark names the commit each patch came from. Only that first line is
/// read — the message body is discarded in favour of the commit object itself,
/// which is the whole point of the mode (builtin/am.c:1457-1462: it "bypasses
/// git-mailinfo's munging of patches").
fn get_mail_commit_oid(repo: &gix::Repository, mail: &Path) -> Option<ObjectId> {
    let body = std::fs::read(mail).ok()?;
    let line = match body.find_byte(b'\n') {
        Some(p) => &body[..p],
        None => &body[..],
    };
    let hex = line.strip_prefix(b"From ".as_ref())?;
    // `get_oid_hex` reads exactly one hash worth of hex and ignores the rest of
    // the line (the `Mon Sep 17 00:00:00 2001` fake date).
    let n = repo.object_hash().len_in_hex();
    ObjectId::from_hex(hex.get(..n)?).ok()
}

/// `get_commit_info` (builtin/am.c:1353): the authorship and message of the
/// *commit*, not of the mail.
///
/// This is what makes `--rebasing` preserve the original author identity and
/// author date exactly — a rebase rewrites the committer, never the author — and
/// why a `--rebasing` session's `author-script` holds the replayed commit's own
/// author rather than whoever is running the rebase.
///
/// The date is `show_ident_date(&id, DATE_MODE(NORMAL))`, e.g.
/// `Thu Apr 7 15:13:13 2005 -0700`; `commit-tree` parses that spelling back to
/// the same `<seconds> <tz>` pair, so the replayed commit keeps the timestamp
/// bit-for-bit.
fn get_commit_info(repo: &gix::Repository, oid: ObjectId) -> Result<CommitInfo> {
    let commit = repo
        .find_commit(oid)
        .map_err(|e| anyhow::anyhow!("could not parse commit {oid}: {e}"))?;
    let author = commit.author()?;
    let time = author.time()?;
    Ok(CommitInfo {
        // `msg = strstr(buffer, "\n\n") + 2`: everything past the header block.
        msg: commit.message_raw()?.to_vec(),
        author_name: author.name.to_str_lossy().into_owned(),
        author_email: author.email.to_str_lossy().into_owned(),
        author_date: time.format_or_unix(gix::date::time::format::DEFAULT),
    })
}

/// `write_commit_patch` (builtin/am.c:1398): regenerate the patch from the
/// commit rather than trusting the mail body.
///
/// `rev_info` is set up with `diff = 1`, `no_commit_id = 1`, `show_root_diff = 1`,
/// `abbrev = 0`, `flags.full_index`, `flags.binary` and `use_color = NEVER`, and
/// `am`'s config callback is `git_default_config` — not `git_diff_ui_config` —
/// so no `diff.*` UI knob (rename detection above all) is in play. `git
/// diff-tree` is the plumbing that carries exactly that setup.
///
/// **`--binary` is absent** because `git diff-tree` does not accept it in this
/// binary yet, so a commit whose patch needs a `GIT binary patch` body
/// regenerates as `Binary files … differ` and then fails to apply, stopping the
/// replay. That is loud rather than silent, and it goes away once `diff-tree`
/// learns the flag.
fn write_commit_patch(ctx: &Ctx, state_dir: &Path, oid: ObjectId) -> Result<()> {
    let out = std::fs::File::create(state_dir.join("patch"))?;
    let ok = ctx
        .cmd("diff-tree")
        .args([
            "-p",
            "--root",
            "--no-commit-id",
            "--full-index",
            "--no-renames",
            "--no-color",
        ])
        .arg(oid.to_hex().to_string())
        .stdout(Stdio::from(out))
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run diff-tree: {e}"))?
        .success();
    if !ok {
        bail!("could not write the patch for {oid}");
    }
    Ok(())
}

/// `parse_mail_rebase` (builtin/am.c:1464). Always applies the patch — there is
/// no skip arm — so the return is the commit info or a hard failure.
fn parse_mail_rebase(
    ctx: &Ctx,
    repo: &gix::Repository,
    state_dir: &Path,
    mail: &Path,
) -> Result<Option<CommitInfo>> {
    let Some(oid) = get_mail_commit_oid(repo, mail) else {
        eprintln!("fatal: could not parse {}", mail.display());
        return Ok(None);
    };
    let info = get_commit_info(repo, oid)?;
    write_commit_patch(ctx, state_dir, oid)?;
    write_text(state_dir, "original-commit", &oid.to_hex().to_string())?;
    // `refs_update_ref(…, "am", "REBASE_HEAD", …, REF_NO_DEREF, DIE_ON_ERR)`:
    // the commit being replayed, which `git status` and the conflict advice name.
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "am".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(oid),
        },
        name: full_name("REBASE_HEAD")?,
        deref: false,
    })?;
    Ok(Some(info))
}

/// `am_load`'s `original-commit` read (builtin/am.c:407) — `state->orig_commit`,
/// null outside a `--rebasing` session.
fn read_orig_commit(state_dir: &Path) -> Option<ObjectId> {
    ObjectId::from_hex(read_state(state_dir, "original-commit").as_bytes()).ok()
}

/// The `rewritten` append `do_commit` (builtin/am.c:1720) and `am_skip`
/// (builtin/am.c:2134) share: one `<original> <replacement>` line per replayed
/// commit. `am_run`'s tail feeds the whole file to the `post-rewrite` hook, and
/// `git rebase` reads it to update refs and notes.
fn record_rewritten(state_dir: &Path, orig: ObjectId, new: ObjectId) -> Result<()> {
    let mut fp = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("rewritten"))?;
    // git writes the two hexes and the newline in three separate `fprintf`s.
    write!(fp, "{} {}\n", orig.to_hex(), new.to_hex())?;
    Ok(())
}

/// `run_applypatch_msg_hook` (builtin/am.c:1478): the hook is handed the
/// `final-commit` path and may rewrite it in place, so the message is re-read
/// afterwards. A non-zero exit stops `am` with status 1.
fn run_applypatch_msg_hook(
    repo: &gix::Repository,
    state_dir: &Path,
    no_verify: bool,
    info: &mut CommitInfo,
) -> Result<bool> {
    if no_verify {
        return Ok(true);
    }
    let path = state_dir.join("final-commit");
    if !crate::hooks::run(repo, "applypatch-msg", &[&path.display().to_string()], None)? {
        return Ok(false);
    }
    match std::fs::read(&path) {
        Ok(msg) => info.msg = msg,
        Err(_) => {
            eprintln!(
                "fatal: '{}' was deleted by the applypatch-msg hook",
                path.display()
            );
            std::process::exit(128);
        }
    }
    Ok(true)
}

/// `do_commit`: `write-tree`, then `commit-tree` with the mail's author, then
/// `update-ref HEAD` with the `am:` reflog line. `Some(code)` means git stops
/// here (`die`); `None` means the commit was recorded.
#[allow(clippy::too_many_arguments)]
fn do_commit(
    ctx: &Ctx,
    repo: &gix::Repository,
    state_dir: &Path,
    info: &CommitInfo,
    ld: &Loaded,
    no_verify: bool,
    // `--ignore-date` drops the mail's author date (the commit is dated now), and
    // `--committer-date-is-author-date` dates the committer by the author's.
    ignore_date: bool,
    committer_date_is_author_date: bool,
) -> Result<Option<ExitCode>> {
    let quiet = ld.quiet;
    // `if (!state->no_verify && run_hooks("pre-applypatch")) exit(1);`
    // (builtin/am.c:1673) — the index is already staged, so a rejecting hook
    // stops before the commit object exists.
    if !no_verify && !crate::hooks::run(repo, "pre-applypatch", &[], None)? {
        return Ok(Some(ExitCode::from(1)));
    }
    // `fmt_ident(..., IDENT_STRICT)` refuses an empty author name; our
    // `commit-tree` would instead accept an empty gix signature, so reproduce
    // git's failure here rather than write a commit git would not.
    if info.author_name.trim().is_empty() {
        eprintln!(
            "fatal: empty ident name (for <{}>) not allowed",
            info.author_email
        );
        return Ok(Some(ExitCode::from(128)));
    }

    let tree = match capture(ctx.cmd("write-tree"))? {
        Some(t) => t,
        None => {
            eprintln!("fatal: git write-tree failed to write a tree");
            return Ok(Some(ExitCode::from(128)));
        }
    };

    let parent = repo.head_id().ok().map(|id| id.detach());
    if parent.is_none() && !quiet {
        eprintln!("applying to an empty history");
    }

    let mut ct = ctx.cmd("commit-tree");
    ct.arg(&tree);
    if let Some(p) = &parent {
        ct.arg("-p").arg(p.to_hex().to_string());
    }
    ct.env("GIT_AUTHOR_NAME", &info.author_name)
        .env("GIT_AUTHOR_EMAIL", &info.author_email);
    // ```c
    // author = fmt_ident(state->author_name, state->author_email, WANT_AUTHOR_IDENT,
    //                    state->ignore_date ? NULL : state->author_date, IDENT_STRICT);
    // if (state->committer_date_is_author_date)
    //         committer = fmt_ident(getenv("GIT_COMMITTER_NAME"), …,
    //                               state->ignore_date ? NULL : state->author_date, …);
    // ```
    //
    // A NULL *or empty* date argument makes `fmt_ident` fall back to
    // `ident_default_date()` — the wall clock. git reaches `fmt_ident` directly,
    // so `$GIT_AUTHOR_DATE` never enters into it; this port drives `commit-tree`,
    // which *does* read the environment, so the variable has to be cleared
    // explicitly. Both arms below matter: `--ignore-date` deliberately discards
    // the date, and a mail with no `Date:` header never had one, and in either
    // case the caller's ambient `GIT_AUTHOR_DATE` must not stand in for it.
    if !info.author_date.is_empty() && !ignore_date {
        ct.env("GIT_AUTHOR_DATE", &info.author_date);
        if committer_date_is_author_date {
            ct.env("GIT_COMMITTER_DATE", &info.author_date);
        }
    } else {
        ct.env_remove("GIT_AUTHOR_DATE");
        if committer_date_is_author_date {
            ct.env_remove("GIT_COMMITTER_DATE");
        }
    }
    ct.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = ct
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to run commit-tree: {e}"))?;
    child
        .stdin
        .take()
        .expect("commit-tree stdin was piped")
        .write_all(&info.msg)?;
    let out = child
        .wait_with_output()
        .map_err(|e| anyhow::anyhow!("failed to run commit-tree: {e}"))?;
    if !out.status.success() {
        // commit-tree has already reported the reason (e.g. a bad author date).
        return Ok(Some(ExitCode::from(128)));
    }
    let commit = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let reflog = std::env::var("GIT_REFLOG_ACTION").unwrap_or_else(|_| "am".to_string());
    let mut ur = ctx.cmd("update-ref");
    ur.arg("-m")
        .arg(format!("{reflog}: {}", first_line(&info.msg)))
        .arg("HEAD")
        .arg(&commit);
    if let Some(p) = &parent {
        ur.arg(p.to_hex().to_string());
    }
    let updated = ur
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run update-ref: {e}"))?
        .success();
    if !updated {
        return Ok(Some(ExitCode::from(128)));
    }

    // `if (state->rebasing) { … fprintf(fp, "%s %s\n", orig, commit); }`
    // (builtin/am.c:1720-1727). The assert is git's: a `--rebasing` session
    // always went through `parse_mail_rebase`, so `original-commit` exists.
    if ld.rebasing {
        if let Some(orig) = read_orig_commit(state_dir) {
            record_rewritten(state_dir, orig, oid_of(&commit)?)?;
        }
    }

    // `run_hooks("post-applypatch")` (builtin/am.c:1729) — advisory, its exit
    // status is discarded.
    let _ = crate::hooks::run(repo, "post-applypatch", &[], None);

    Ok(None)
}

/// `am_next`: forget the current patch's per-message state and advance `next`.
fn am_next(repo: &gix::Repository, state_dir: &Path, cur: &mut usize) -> Result<()> {
    let _ = std::fs::remove_file(state_dir.join("author-script"));
    let _ = std::fs::remove_file(state_dir.join("final-commit"));
    let _ = std::fs::remove_file(state_dir.join("original-commit"));
    if repo.find_reference("REBASE_HEAD").is_ok() {
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
                message: Default::default(),
            },
            name: full_name("REBASE_HEAD")?,
            deref: false,
        })?;
    }
    match repo.head_id().ok().map(|id| id.detach()) {
        Some(head) => write_text(state_dir, "abort-safety", &head.to_hex().to_string())?,
        None => write_text(state_dir, "abort-safety", "")?,
    }
    *cur += 1;
    write_text(state_dir, "next", &cur.to_string())?;
    Ok(())
}

/// `am_resolve`: commit the user's resolved index for the current patch (no
/// re-apply), then continue with the rest. `allow_empty` is `--allow-empty`.
fn am_resolve(
    repo: &gix::Repository,
    state_dir: &Path,
    cli: &Cli,
    allow_empty: bool,
) -> Result<ExitCode> {
    let ctx = Ctx::new(repo, state_dir)?;
    let info = match load_current(repo, state_dir)? {
        Some(ci) => ci,
        None => return Ok(ExitCode::from(128)),
    };

    let ld = load_state(state_dir);
    let quiet = ld.quiet;
    if !quiet {
        println!("Applying: {}", first_line(&info.msg));
    }

    let no_changes = index_has_no_changes(repo)?;
    let patch_empty = is_empty_or_missing(&state_dir.join("patch"));
    if no_changes {
        if allow_empty && patch_empty {
            println!("No changes - recorded it as an empty commit.");
        } else {
            println!(
                "No changes - did you forget to use 'git add'?\nIf there is nothing left to \
                 stage, chances are that something else\nalready introduced the same changes; \
                 you might want to skip this patch."
            );
            return die_user_resolve(repo, state_dir, cli);
        }
    }

    if has_unmerged(repo)? {
        println!(
            "You still have unmerged paths in your index.\nYou should 'git add' each file with \
             resolved conflicts to mark them as such.\nYou might run `git rm` on a file to \
             accept \"deleted by them\" for it."
        );
        return die_user_resolve(repo, state_dir, cli);
    }

    if cli.interactive {
        bail!(
            "`git am -i --continue` interactive mode is not ported: it re-drives the \
             per-patch tty prompt loop"
        );
    }
    if cli.gpg_sign {
        bail!(
            "`git am --continue -S` is not ported: signing the commit needs the signing path \
             `commit-tree` does not expose here"
        );
    }

    if let Some(code) = do_commit(
        &ctx,
        repo,
        state_dir,
        &info,
        &ld,
        cli.no_verify,
        cli.ignore_date,
        cli.committer_date_is_author_date,
    )? {
        return Ok(code);
    }

    let mut cur = read_count(state_dir, "next")?;
    am_next(repo, state_dir, &mut cur)?;
    run_am_loop(repo, state_dir, cli, false)
}

/// `am_skip`: discard the current patch (reset the index/worktree to HEAD), then
/// continue with the rest.
fn am_skip(repo: &gix::Repository, state_dir: &Path, cli: &Cli) -> Result<ExitCode> {
    let ld = load_state(state_dir);
    let ctx = Ctx::new(repo, state_dir)?;
    am_rerere_clear(repo)?;
    // `clean_index(&head, &head)`: reset the index and worktree to HEAD,
    // discarding the failed patch's partial application (untracked files are
    // preserved). HEAD does not move, so nothing is written to its reflog.
    if !clean_index(repo, &ctx, "HEAD")? {
        eprintln!("fatal: failed to clean index");
        return Ok(ExitCode::from(128));
    }
    // `if (state->rebasing) { … fprintf(fp, "%s %s\n", orig_commit, head); }`
    // (builtin/am.c:2134): a skipped commit is rewritten *to the current HEAD*,
    // which is how `git rebase --skip` tells the `post-rewrite` hook the commit
    // was dropped rather than replaced.
    if ld.rebasing {
        if let (Some(orig), Some(head)) = (
            read_orig_commit(state_dir),
            repo.head_id().ok().map(|id| id.detach()),
        ) {
            record_rewritten(state_dir, orig, head)?;
        }
    }
    let mut cur = read_count(state_dir, "next")?;
    am_next(repo, state_dir, &mut cur)?;
    run_am_loop(repo, state_dir, cli, false)
}

/// `am_abort`: if it is safe, rewind the index/worktree and HEAD to `ORIG_HEAD`,
/// then destroy the session.
fn am_abort(repo: &gix::Repository, state_dir: &Path) -> Result<ExitCode> {
    if !safe_to_abort(repo, state_dir)? {
        std::fs::remove_dir_all(state_dir)?;
        return Ok(ExitCode::SUCCESS);
    }
    let ctx = Ctx::new(repo, state_dir)?;
    am_rerere_clear(repo)?;

    if repo.find_reference("ORIG_HEAD").is_ok() {
        // clean_index(curr, orig) followed by `update_ref("am --abort", HEAD, orig)`
        // — `reset --hard` performs both. The reflog line reads `reset: moving to
        // ORIG_HEAD` rather than git's `am --abort` (a reflog-only difference).
        if !reset_hard(&ctx, "ORIG_HEAD")? {
            eprintln!("fatal: failed to clean index");
            return Ok(ExitCode::from(128));
        }
    }
    // The no-ORIG_HEAD case (aborting an am started on an unborn branch) would
    // delete the current branch ref; that is left to the user rather than guessed.
    std::fs::remove_dir_all(state_dir)?;
    Ok(ExitCode::SUCCESS)
}

/// `safe_to_abort`: refuse to rewind when the previous failure was a dirty index
/// or when HEAD has moved since.
fn safe_to_abort(repo: &gix::Repository, state_dir: &Path) -> Result<bool> {
    if state_dir.join("dirtyindex").exists() {
        return Ok(false);
    }
    let abort_safety = read_state(state_dir, "abort-safety");
    let head = repo
        .head_id()
        .ok()
        .map(|id| id.detach().to_hex().to_string())
        .unwrap_or_default();
    if head == abort_safety {
        return Ok(true);
    }
    eprintln!(
        "warning: You seem to have moved HEAD since the last 'am' failure.\nNot rewinding to \
         ORIG_HEAD"
    );
    Ok(false)
}

/// `am_rerere_clear`: drop rerere's in-progress resolution metadata.
fn am_rerere_clear(repo: &gix::Repository) -> Result<()> {
    let merge_rr = repo.git_dir().join("MERGE_RR");
    if merge_rr.exists() {
        std::fs::remove_file(&merge_rr)?;
    }
    Ok(())
}

/// `die_user_resolve`: the `advise_if_enabled(ADVICE_MERGE_CONFLICT, ...)` hint
/// block (stderr, `hint:`-prefixed), then exit 128. The `--allow-empty` line is
/// gated on `advice.amWorkDir` plus an empty patch with no staged changes.
fn die_user_resolve(repo: &gix::Repository, state_dir: &Path, cli: &Cli) -> Result<ExitCode> {
    // `if (state->resolvemsg) advise_if_enabled(ADVICE_MERGE_CONFLICT, "%s",
    // state->resolvemsg);` — the caller's text *replaces* the whole block, hints
    // and all. This is how `git rebase --apply` gets `git rebase --continue`
    // wording out of a failed `git am`.
    if let Some(msg) = &cli.resolvemsg {
        if crate::advice::enabled("mergeConflict") {
            for line in msg.split('\n') {
                eprintln!("hint: {line}");
            }
            eprintln!("hint: Disable this message with \"git config set advice.mergeConflict false\"");
        }
        return Ok(ExitCode::from(128));
    }
    if crate::advice::enabled("mergeConflict") {
        let interactive = cli.interactive;
        let cmdline = if interactive { "git am -i" } else { "git am" };
        eprintln!("hint: When you have resolved this problem, run \"{cmdline} --continue\".");
        eprintln!("hint: If you prefer to skip this patch, run \"{cmdline} --skip\" instead.");
        let patch_empty = is_empty_or_missing(&state_dir.join("patch"));
        if crate::advice::enabled("amWorkDir") && patch_empty && index_has_no_changes(repo)? {
            eprintln!(
                "hint: To record the empty patch as an empty commit, run \"{cmdline} --allow-empty\"."
            );
        }
        eprintln!(
            "hint: To restore the original branch and stop patching, run \"{cmdline} --abort\"."
        );
        eprintln!("hint: Disable this message with \"git config set advice.mergeConflict false\"");
    }
    Ok(ExitCode::from(128))
}

/// `am_load`'s `read_am_author_script`/`read_commit_msg` plus
/// `validate_resume_state`: read the current patch's message and authorship back
/// from the state directory. `None` means git died (`cannot resume: … does not
/// exist.`) and the message has been printed.
fn load_current(repo: &gix::Repository, state_dir: &Path) -> Result<Option<CommitInfo>> {
    let msg = match std::fs::read(state_dir.join("final-commit")) {
        Ok(m) => m,
        Err(_) => {
            eprintln!(
                "fatal: cannot resume: {} does not exist.",
                display_dir(repo, &state_dir.join("final-commit"))
            );
            return Ok(None);
        }
    };

    let (mut name, mut email, mut date): (Option<String>, Option<String>, Option<String>) =
        (None, None, None);
    if let Ok(script) = std::fs::read_to_string(state_dir.join("author-script")) {
        for line in script.lines() {
            if let Some(v) = line.strip_prefix("GIT_AUTHOR_NAME=") {
                name = Some(sq_dequote(v).join(""));
            } else if let Some(v) = line.strip_prefix("GIT_AUTHOR_EMAIL=") {
                email = Some(sq_dequote(v).join(""));
            } else if let Some(v) = line.strip_prefix("GIT_AUTHOR_DATE=") {
                date = Some(sq_dequote(v).join(""));
            }
        }
    }
    match (name, email, date) {
        (Some(author_name), Some(author_email), Some(author_date)) => Ok(Some(CommitInfo {
            msg,
            author_name,
            author_email,
            author_date,
        })),
        _ => {
            eprintln!(
                "fatal: cannot resume: {} does not exist.",
                display_dir(repo, &state_dir.join("author-script"))
            );
            Ok(None)
        }
    }
}

/// `write_author_script`: the sq-quoted `GIT_AUTHOR_*` lines a resume reads back.
fn write_author_script(state_dir: &Path, info: &CommitInfo) -> Result<()> {
    let body = format!(
        "GIT_AUTHOR_NAME={}\nGIT_AUTHOR_EMAIL={}\nGIT_AUTHOR_DATE={}\n",
        sq_quote_one(&info.author_name),
        sq_quote_one(&info.author_email),
        sq_quote_one(&info.author_date),
    );
    std::fs::write(state_dir.join("author-script"), body)?;
    Ok(())
}

/// `sq_quote_buf`: wrap in single quotes, escaping embedded quotes as `'\''`.
fn sq_quote_one(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `sq_dequote`: inverse of `sq_quote` over one or more space-separated tokens.
///
/// Shared with `bisect replay`, whose `BISECT_LOG` records the `start` operands
/// sq-quoted and hands them back through `sq_dequote_to_strvec()`.
pub(super) fn sq_dequote(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let mut tok: Vec<u8> = Vec::new();
        while i < b.len() && b[i] != b' ' {
            match b[i] {
                b'\'' => {
                    i += 1;
                    while i < b.len() && b[i] != b'\'' {
                        tok.push(b[i]);
                        i += 1;
                    }
                    if i < b.len() {
                        i += 1; // closing quote
                    }
                }
                b'\\' => {
                    // `'\''` emits a backslash-escaped quote between two quoted runs.
                    i += 1;
                    if i < b.len() {
                        tok.push(b[i]);
                        i += 1;
                    }
                }
                c => {
                    tok.push(c);
                    i += 1;
                }
            }
        }
        out.push(String::from_utf8_lossy(&tok).into_owned());
    }
    out
}

/// The first line of a commit message, for git's `%.*s`/`linelen` echoes.
fn first_line(msg: &[u8]) -> String {
    let end = msg.iter().position(|&b| b == b'\n').unwrap_or(msg.len());
    String::from_utf8_lossy(&msg[..end]).into_owned()
}

/// `is_empty_or_missing_file`: true when the file is absent or zero-length.
fn is_empty_or_missing(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.len() == 0).unwrap_or(true)
}

/// `!repo_index_has_changes(...)`: the index matches HEAD (no staged changes).
fn index_has_no_changes(repo: &gix::Repository) -> Result<bool> {
    let index = repo.index_or_empty()?;
    Ok(dirty_paths(repo, &index)?.is_empty())
}

/// `unmerged_index(...)`: any entry at a nonzero stage.
fn has_unmerged(repo: &gix::Repository) -> Result<bool> {
    let index = repo.index_or_empty()?;
    Ok(index.entries().iter().any(|e| e.stage_raw() != 0))
}

/// Run `git reset --hard -q <rev>` (silent, so no `HEAD is now at …` line), the
/// re-exec form of `am`'s `clean_index`/worktree reset. Returns success.
/// `clean_index(head, head)` (builtin/am.c:2058): reset the index to a tree and
/// bring the worktree with it, **moving no ref**.
///
/// This is not `reset --hard`: that also repoints `HEAD` and writes `ORIG_HEAD`,
/// so using it for `am --skip` — where git's `clean_index` leaves `HEAD` exactly
/// where it is — left a spurious `reset: moving to HEAD` line in `HEAD`'s reflog
/// and an `ORIG_HEAD` stock never creates. `read-tree -u --reset` is the
/// plumbing that does the index+worktree half alone.
fn clean_index(repo: &gix::Repository, ctx: &Ctx, rev: &str) -> Result<bool> {
    let ok = ctx
        .cmd("read-tree")
        .arg("-u")
        .arg("--reset")
        .arg(rev)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run read-tree: {e}"))?
        .success();
    if !ok {
        return Ok(false);
    }
    // `clean_index()` ends in `remove_branch_state(the_repository, 0)`, which is
    // `remove_merge_branch_state()` plus `SQUASH_MSG` (branch.c:803-829). Without
    // it the `AUTO_MERGE` the three-way fallback recorded for the conflict being
    // discarded survives the skip, and `git diff AUTO_MERGE` then reports against
    // a merge that no longer exists.
    for name in [
        "MERGE_HEAD",
        "MERGE_RR",
        "MERGE_MSG",
        "MERGE_MODE",
        "AUTO_MERGE",
        "SQUASH_MSG",
    ] {
        let _ = std::fs::remove_file(repo.git_dir().join(name));
    }
    Ok(true)
}

fn reset_hard(ctx: &Ctx, rev: &str) -> Result<bool> {
    Ok(ctx
        .cmd("reset")
        .arg("--hard")
        .arg("-q")
        .arg(rev)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run reset: {e}"))?
        .success())
}

/// Run a child capturing stdout (stderr inherited), returning the trimmed output
/// or `None` on a nonzero exit.
fn capture(mut cmd: Command) -> Result<Option<String>> {
    let out = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run child: {e}"))?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

/// Read one of the numeric state files (`next`, `last`).
fn read_count(state_dir: &Path, name: &str) -> Result<usize> {
    let raw = std::fs::read_to_string(state_dir.join(name))?;
    Ok(raw.trim().parse::<usize>()?)
}

/// `repo_index_has_changes()`: the paths where the index differs from `HEAD`,
/// in index (byte-sorted) order. Unmerged paths always count as differing.
fn dirty_paths(repo: &gix::Repository, state: &gix::index::State) -> Result<Vec<BString>> {
    let Some(tree) = repo.head_tree_id().ok().map(|id| id.detach()) else {
        // Without a HEAD to compare against git lists every cached path.
        let mut all: BTreeSet<BString> = BTreeSet::new();
        for e in state.entries() {
            all.insert(e.path(state).to_owned());
        }
        return Ok(all.into_iter().collect());
    };

    let base = repo.index_from_tree(&tree)?;
    let backing = base.path_backing();
    let mut want: BTreeMap<BString, (u32, ObjectId)> = base
        .entries()
        .iter()
        .map(|e| (e.path_in(backing).to_owned(), (e.mode.bits(), e.id)))
        .collect();

    let mut changed: BTreeSet<BString> = BTreeSet::new();
    for e in state.entries() {
        let path = e.path(state).to_owned();
        if e.stage_raw() != 0 {
            want.remove(&path);
            changed.insert(path);
            continue;
        }
        match want.remove(&path) {
            Some((mode, id)) if mode == e.mode.bits() && id == e.id => {}
            _ => {
                changed.insert(path);
            }
        }
    }
    // Whatever HEAD still holds that the index does not is a deletion.
    changed.extend(want.into_keys());
    Ok(changed.into_iter().collect())
}

// ---------------------------------------------------------------------------
// show_patch
// ---------------------------------------------------------------------------

fn show_patch(repo: &gix::Repository, state_dir: &Path, sub: Sub) -> Result<ExitCode> {
    // `if (!is_null_oid(&state->orig_commit)) { run "show <oid> --"; }`
    // (builtin/am.c:2229). A `--rebasing` session shows the *original commit*,
    // not the regenerated patch, and it ignores the raw/diff distinction.
    if let Some(oid) = read_orig_commit(state_dir) {
        let ctx = Ctx::new(repo, state_dir)?;
        let ok = ctx
            .cmd("show")
            .arg(oid.to_hex().to_string())
            .arg("--")
            .status()
            .map_err(|e| anyhow::anyhow!("failed to run show: {e}"))?
            .success();
        return Ok(if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    let path = match sub {
        // `msgnum()`: the zero-padded number held in `next`.
        Sub::Raw => state_dir.join(format!("{:04}", current_patch_number(state_dir)?)),
        Sub::Diff => state_dir.join("patch"),
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            std::io::stdout().write_all(&bytes)?;
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("fatal: failed to read '{}': {e}", path.display());
            Ok(ExitCode::from(128))
        }
    }
}

/// `msgnum()` reads `next`, the 1-based index of the patch being applied.
fn current_patch_number(state_dir: &Path) -> Result<usize> {
    read_count(state_dir, "next")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_stdin() -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    Ok(buf)
}

/// `write_state_text()`, which is `write_file()` and therefore terminates a
/// non-empty body with a newline and writes an empty body as an empty file.
fn write_text(dir: &Path, name: &str, body: &str) -> Result<()> {
    let mut out = body.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(dir.join(name), out)?;
    Ok(())
}

fn write_bool(dir: &Path, name: &str, v: bool) -> Result<()> {
    write_text(dir, name, if v { "t" } else { "f" })
}

/// `sq_quote_argv()`: each element single-quoted and prefixed with a space.
fn sq_quote_argv(argv: &[String]) -> String {
    let mut out = String::new();
    for a in argv {
        out.push_str(" '");
        out.push_str(&a.replace('\'', r"'\''"));
        out.push('\'');
    }
    out
}

fn full_name(name: &str) -> Result<FullName> {
    name.try_into()
        .map_err(|e| anyhow::anyhow!("invalid ref name {name}: {e}"))
}

/// Render the state directory the way git names it in diagnostics: relative to
/// the worktree root (`.git/rebase-apply`) when it lives inside it, else absolute.
fn display_dir(repo: &gix::Repository, dir: &Path) -> String {
    repo.workdir()
        .and_then(|w| dir.strip_prefix(w).ok())
        .unwrap_or(dir)
        .display()
        .to_string()
}

/// git renders `errno` with `strerror`, which has no `(os error N)` suffix.
fn errno_msg(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(at) => s[..at].to_string(),
        None => s,
    }
}
