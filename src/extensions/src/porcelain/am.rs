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
//!   * **`--3way`.** The fallback merge on apply failure needs
//!     `build_fake_ancestor` plus the merge machinery; a clean patch under
//!     `--3way` still applies, but a patch that fails and falls back refuses
//!     (the session is left intact for `--abort`).
//!   * **`--signoff`.** Faithful `append_signoff` trailer placement/dedup is not
//!     vendored, so appending a Signed-off-by line is refused rather than
//!     committed wrong.
//!   * **`-i`/`--interactive`.** The per-patch tty prompt loop cannot run
//!     unattended.
//!   * **`--ignore-date` / `--committer-date-is-author-date` / `-S`.** These
//!     reshape the commit's date, committer, or signature; `git commit-tree`
//!     cannot be driven to reproduce them here.
//!   * **`--rebasing` / rebase-driven sessions.** `parse_mail_rebase`, the
//!     `rewritten` note replay, and `--show-current-patch` on an
//!     `original-commit` all need the rebase machinery, which is an empty
//!     placeholder (`gix-rebase`/`gix-sequencer`).
//!   * **Multi-message mbox splitting.** `split_mbox` treats each source file as
//!     a single message (the fixtures carry no `From ` envelope); a real
//!     multi-patch mbox would need envelope splitting `git mailsplit` does.

use anyhow::{bail, Result};
use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

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
    rebasing: bool,
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
            rebasing: false,
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
struct Usage(String);

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
        Err(Usage(msg)) => {
            eprintln!("{msg}");
            return Ok(ExitCode::from(129));
        }
    };

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
        Resume::ShowPatch(sub) => show_patch(&state_dir, sub),
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
        Resume::Retry => bail!(
            "`git am --retry` is not a git verb; there is no upstream behavior to port"
        ),
    }
}

// ---------------------------------------------------------------------------
// Option parsing
// ---------------------------------------------------------------------------

fn parse(args: &[String], defaults: &AmDefaults) -> Result<Opts, Usage> {
    let mut o = Opts::default();
    // `am_state_init` sets these from `git_am_config` before `parse_options`
    // runs; a later `--3way`/`--no-3way`/`-m`/`--no-message-id` overrides them.
    o.threeway = defaults.threeway;
    o.message_id = defaults.message_id;
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
    Err(Usage(format!("error: option `{}' requires a value", trim_dashes(tok))))
}

fn no_value(tok: &str, attached: Option<&str>) -> Result<(), Usage> {
    match attached {
        None => Ok(()),
        Some(_) => Err(Usage(format!(
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
                    return Err(Usage(format!(
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
                    return Err(Usage(format!(
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
                _ => return Err(Usage(format!("error: invalid value for '--empty': '{v}'"))),
            };
        }
        // Consulted only when a patch fails to apply.
        "resolvemsg" => {
            take_value(tok, attached, args, i)?;
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
        // `--verify`/`--binary` govern hooks/apply this port does not special-case.
        "verify" | "no-verify" | "binary" | "no-binary" => no_value(tok, attached)?,
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
                    return Err(Usage(format!(
                        "error: invalid value for '--show-current-patch': '{v}'"
                    )))
                }
            };
            cmdmode_checked(o, tok, Resume::ShowPatch(sub))?;
        }
        _ => return Err(Usage(format!("error: unknown option `{name}'"))),
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
        return Err(Usage(format!("error: unknown switch `{body}'")));
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
            'n' => {} // --no-verify: hooks are never reached
            'b' => {} // historical no-op
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
                    return Err(Usage(format!("error: option `{c}' requires a value")));
                };
                o.apply_opts.push(format!("-{c}{v}"));
            }
            // `-S[<key-id>]` takes an optional attached value.
            'S' => {
                o.gpg_sign = true;
                at = bytes.len();
            }
            _ => return Err(Usage(format!("error: unknown switch `{c}'"))),
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
        Some((prev, prev_tok)) if *prev != want => Err(Usage(format!(
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

/// Number of mbox messages in `body`, counted by the `From ` envelope separator
/// git's mailsplit splits on (a line `From <40-hex-sha> <date>`, as produced by
/// `format-patch`). Used to REFUSE a multi-patch mbox rather than silently
/// squashing it into one commit — this port does not yet re-exec `git mailsplit`
/// for real envelope splitting, so a series must be applied one patch at a time.
fn mbox_message_count(body: &[u8]) -> usize {
    let is_from_line = |line: &[u8]| -> bool {
        let Some(rest) = line.strip_prefix(b"From ") else {
            return false;
        };
        // `From ` followed by a 40- or 64-hex object id and a space.
        let hex_len = rest.iter().take_while(|b| b.is_ascii_hexdigit()).count();
        (hex_len == 40 || hex_len == 64) && rest.get(hex_len) == Some(&b' ')
    };
    body.split(|&b| b == b'\n').filter(|l| is_from_line(l)).count()
}

/// The honest refusal for a multi-message mbox, so `git am <series>` fails
/// cleanly (git's `Failed to split patches`, exit 128) instead of corrupting
/// history by committing the whole series as one squashed patch.
fn multi_message_unsupported() -> Split {
    Split::Failed(vec![
        "multi-patch mbox splitting is not ported (needs git mailsplit envelope \
         splitting); apply the patches one at a time"
            .to_string(),
    ])
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
        let body = read_stdin()?;
        if mbox_message_count(&body) > 1 {
            return Ok(multi_message_unsupported());
        }
        if !body.is_empty() {
            msgs.push(body);
        }
        return Ok(Split::Messages(msgs));
    }
    for p in paths {
        if p == "-" {
            let body = read_stdin()?;
            if mbox_message_count(&body) > 1 {
                return Ok(multi_message_unsupported());
            }
            if !body.is_empty() {
                msgs.push(body);
            }
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
            Ok(body) => {
                if mbox_message_count(&body) > 1 {
                    return Ok(multi_message_unsupported());
                }
                if !body.is_empty() {
                    msgs.push(body);
                }
            }
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
    ignore_date: bool,
    committer_date_is_author_date: bool,
    gpg_sign: bool,
}

impl Cli {
    fn from_opts(o: &Opts) -> Self {
        Self {
            empty: o.empty,
            interactive: o.interactive,
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
        Ok(Ctx { exe, cwd, sdir })
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
            if ld.rebasing {
                bail!(
                    "`git am --rebasing` / rebase-driven am is not ported: it needs \
                     `parse_mail_rebase` (commit replay) and the `rewritten` note machinery, \
                     neither of which exists in the vendored crates"
                );
            }
            match parse_mail(&ctx, state_dir, &ld, &mail)? {
                ParseOutcome::Skip => {
                    am_next(repo, state_dir, &mut cur)?;
                    resume = false;
                    continue;
                }
                ParseOutcome::Died(code) => return Ok(code),
                ParseOutcome::Parsed(ci) => {
                    if ld.signoff {
                        bail!(
                            "`git am --signoff` is not ported: appending a Signed-off-by trailer \
                             faithfully needs git's `append_signoff` trailer placement/dedup, which \
                             is not vendored; refusing rather than committing a wrong message"
                        );
                    }
                    write_author_script(state_dir, &ci)?;
                    // `final-commit` was written by `parse_mail`.
                    ci
                }
            }
        };

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
                    return die_user_resolve(repo, state_dir, cli.interactive);
                }
            }
        }

        if !to_keep {
            if !ld.quiet {
                println!("Applying: {first}");
            }
            if !run_apply(&ctx, &ld)? {
                if ld.threeway {
                    bail!(
                        "`git am --3way` fallback is not ported: reconstructing a base tree and \
                         running a 3-way merge needs `build_fake_ancestor` + the merge machinery \
                         that is not vendored; the state directory is left intact for `--abort`"
                    );
                }
                println!("Patch failed at {cur:04} {first}");
                if crate::advice::enabled("amWorkDir") {
                    eprintln!(
                        "hint: Use 'git am --show-current-patch=diff' to see the failed patch"
                    );
                }
                return die_user_resolve(repo, state_dir, cli.interactive);
            }
        }

        if cli.ignore_date || cli.committer_date_is_author_date || cli.gpg_sign {
            bail!(
                "`git am` with --ignore-date/--committer-date-is-author-date/-S is not ported: \
                 these reshape the commit's date/committer/signature and cannot be reproduced \
                 through `git commit-tree`; refusing rather than committing a wrong object"
            );
        }

        if let Some(code) = do_commit(&ctx, repo, &info, ld.quiet)? {
            return Ok(code);
        }

        am_next(repo, state_dir, &mut cur)?;
        resume = false;
    }

    // `am_destroy`: nothing left to apply, so the session is torn down.
    std::fs::remove_dir_all(state_dir)?;
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

/// `run_apply(state, NULL)`: `git apply --index <apply-opts> <patch>` — apply to
/// both the index and the worktree, checking against the index. Returns whether
/// the patch applied cleanly (the child's own diagnostics reach stderr).
fn run_apply(ctx: &Ctx, ld: &Loaded) -> Result<bool> {
    let mut c = ctx.cmd("apply");
    c.arg("--index");
    for opt in &ld.apply_opts {
        c.arg(opt);
    }
    c.arg(ctx.spath("patch"));
    Ok(c
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run apply: {e}"))?
        .success())
}

/// `do_commit`: `write-tree`, then `commit-tree` with the mail's author, then
/// `update-ref HEAD` with the `am:` reflog line. `Some(code)` means git stops
/// here (`die`); `None` means the commit was recorded.
fn do_commit(
    ctx: &Ctx,
    repo: &gix::Repository,
    info: &CommitInfo,
    quiet: bool,
) -> Result<Option<ExitCode>> {
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
    if !info.author_date.is_empty() {
        ct.env("GIT_AUTHOR_DATE", &info.author_date);
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

    let quiet = read_state(state_dir, "quiet") == "t";
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
            return die_user_resolve(repo, state_dir, cli.interactive);
        }
    }

    if has_unmerged(repo)? {
        println!(
            "You still have unmerged paths in your index.\nYou should 'git add' each file with \
             resolved conflicts to mark them as such.\nYou might run `git rm` on a file to \
             accept \"deleted by them\" for it."
        );
        return die_user_resolve(repo, state_dir, cli.interactive);
    }

    if cli.interactive {
        bail!(
            "`git am -i --continue` interactive mode is not ported: it re-drives the \
             per-patch tty prompt loop"
        );
    }
    if cli.ignore_date || cli.committer_date_is_author_date || cli.gpg_sign {
        bail!(
            "`git am --continue` with --ignore-date/--committer-date-is-author-date/-S is not \
             ported: these reshape the commit and cannot be reproduced through `git commit-tree`"
        );
    }

    if let Some(code) = do_commit(&ctx, repo, &info, quiet)? {
        return Ok(code);
    }

    let mut cur = read_count(state_dir, "next")?;
    am_next(repo, state_dir, &mut cur)?;
    run_am_loop(repo, state_dir, cli, false)
}

/// `am_skip`: discard the current patch (reset the index/worktree to HEAD), then
/// continue with the rest.
fn am_skip(repo: &gix::Repository, state_dir: &Path, cli: &Cli) -> Result<ExitCode> {
    if load_state(state_dir).rebasing {
        bail!(
            "`git am --skip` for a rebase-driven session is not ported: it must append the \
             skipped commit to `rewritten`, which the unported rebase machinery consumes"
        );
    }
    let ctx = Ctx::new(repo, state_dir)?;
    am_rerere_clear(repo)?;
    // clean_index(HEAD, HEAD): reset the index and worktree to HEAD, discarding
    // the failed patch's partial application (untracked files are preserved).
    if !reset_hard(&ctx, "HEAD")? {
        eprintln!("fatal: failed to clean index");
        return Ok(ExitCode::from(128));
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
fn die_user_resolve(
    repo: &gix::Repository,
    state_dir: &Path,
    interactive: bool,
) -> Result<ExitCode> {
    if crate::advice::enabled("mergeConflict") {
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
fn sq_dequote(s: &str) -> Vec<String> {
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

fn show_patch(state_dir: &Path, sub: Sub) -> Result<ExitCode> {
    if state_dir.join("original-commit").is_file() {
        // git delegates to `git show <orig-commit> --` for a rebase-driven
        // session; that session cannot be produced here in the first place.
        bail!(
            "--show-current-patch for a rebase-driven am session is not yet ported: it \
             replays `git show <original-commit>`, and gix-rebase is an empty placeholder"
        );
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
