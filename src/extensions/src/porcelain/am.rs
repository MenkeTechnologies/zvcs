//! `git am` — apply a series of patches from a mailbox.
//!
//! Port of `builtin/am.c`. The command decomposes into four stages, and this
//! module reproduces the first three exactly:
//!
//!   1. **Option parsing** (`cmd_am`'s `parse_options`) — including the
//!      `OPT_CMDMODE` mutual exclusion between the resume verbs, the callbacks
//!      that reject `--patch-format`/`--empty`/`--quoted-cr`/`--show-current-patch`
//!      values, and the `OPT_PASSTHRU_ARGV` options that are recorded verbatim
//!      for `git apply` rather than acted on here.
//!   2. **Session dispatch** (`am_in_progress` and the `in_progress` branch) —
//!      whether a `.git/rebase-apply` session exists decides between resuming,
//!      refusing to resume, destroying a stray directory, or starting fresh.
//!   3. **Session setup** (`am_setup`) — patch-format detection, splitting the
//!      mailbox, and writing the `.git/rebase-apply` state files, `ORIG_HEAD`
//!      and `abort-safety`.
//!   4. **Patch application** (`am_run`'s loop, `parse_mail`, `do_commit`) — the
//!      stage that needs substrate the vendored gitoxide crates do not have.
//!
//! ## What is served
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
//!   * `--show-current-patch[=(raw|diff)]` and `--quit` inside a live session.
//!
//! ## What is not served, and why
//!
//! Once a mailbox contains at least one message, applying it is **not**
//! implemented, and neither are the resume verbs that re-drive that loop
//! (`--continue`, `--skip`, `--abort`, `--retry`, `--allow-empty` inside a
//! session). Three pieces of substrate are missing from `src/ported`:
//!
//!   * **No patch applier.** `gix-diff` only *produces* unified diffs
//!     (`gix-diff/src/blob/unified_diff/`); nothing in the tree parses `@@`
//!     hunk headers or applies a diff to an index/worktree. `git apply`, which
//!     `git am` shells out to for every patch, has no counterpart here.
//!   * **No mail parsing.** There is no `git mailinfo` (RFC 2047 header decode,
//!     subject cleanup, scissors, body/patch separation), so a split-out message
//!     cannot be turned into an authorship record plus a diff. `gix-mailmap`
//!     rewrites identities only.
//!   * **No sequencer.** `gix-sequencer/src/lib.rs` and `gix-rebase/src/lib.rs`
//!     contain only `#![forbid(unsafe_code)]`, so the `.git/rebase-apply` state
//!     machine the resume verbs drive cannot be advanced or unwound.
//!
//! Those paths bail rather than emit a guess: a patch applied approximately is a
//! silently wrong worktree, which is worse than an error. Because the session
//! that git would leave behind cannot be resumed here either, this module does
//! not create one on that path — it refuses before touching the repository.

use anyhow::{bail, Result};
use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::process::ExitCode;

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
    message_id: bool,
    scissors: Option<bool>,
    quoted_cr: Option<&'static str>,
    rerere_autoupdate: Option<bool>,
    apply_opts: Vec<String>,
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
            message_id: false,
            scissors: None,
            quoted_cr: None,
            rerere_autoupdate: None,
            apply_opts: Vec::new(),
        }
    }
}

/// A parse failure. git prints the message and exits 129 without usage text.
struct Usage(String);

pub fn am(args: &[String]) -> Result<ExitCode> {
    // Dispatch strips the subcommand today; tolerate it being present at [0].
    let args: &[String] = match args.first() {
        Some(a) if a == "am" => &args[1..],
        _ => args,
    };

    let opts = match parse(args) {
        Ok(o) => o,
        Err(Usage(msg)) => {
            eprintln!("{msg}");
            return Ok(ExitCode::from(129));
        }
    };

    let repo = gix::discover(".")?;
    let state_dir = repo.git_dir().join("rebase-apply");

    // `am_in_progress`: the directory alone is not a session — `next` and `last`
    // are written last by `am_setup` precisely so they mark completion.
    let in_progress = state_dir.is_dir()
        && state_dir.join("last").is_file()
        && state_dir.join("next").is_file();

    let resume = if in_progress {
        // Catch a patch fed to a live session. git treats a non-tty stdin as an
        // attempt to pipe one in, even when it is `/dev/null`.
        if !opts.paths.is_empty()
            || (opts.resume.is_none() && !std::io::stdin().is_terminal())
        {
            eprintln!(
                "fatal: previous rebase directory {} still exists but mbox given.",
                display_dir(&repo, &state_dir)
            );
            return Ok(ExitCode::from(128));
        }
        opts.resume.as_ref().map_or(Resume::Apply, |(r, _)| *r)
    } else {
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

        match setup(&repo, &state_dir, &opts)? {
            Setup::Ready => Resume::Apply,
            Setup::Failed(code) => return Ok(ExitCode::from(code)),
        }
    };

    match resume {
        // `RESUME_FALSE`/`RESUME_APPLY` both land in `am_run`. Reaching here
        // means the mailbox held no messages, so the loop body never executes.
        Resume::Apply => run(&repo, &state_dir),
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
        // `am_resolve`, `am_skip` and `am_abort` all re-enter the apply loop or
        // unwind commits it made.
        Resume::Resolved | Resume::AllowEmpty | Resume::Skip | Resume::Abort | Resume::Retry => {
            bail!(
                "resuming an am session is not yet ported: driving `.git/rebase-apply` \
                 needs a `git apply` patch applier and `git mailinfo` mail parsing, neither \
                 of which exists in the vendored gitoxide crates"
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Option parsing
// ---------------------------------------------------------------------------

fn parse(args: &[String]) -> Result<Opts, Usage> {
    let mut o = Opts::default();
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
            // Only consulted for a message whose patch turns out to be empty,
            // which is past the point this module reaches; the value is still
            // validated because git rejects a bad one before doing anything.
            let v = take_value(tok, attached, args, i)?;
            if !matches!(v, "stop" | "drop" | "keep") {
                return Err(Usage(format!("error: invalid value for '--empty': '{v}'")));
            }
        }
        // Consulted only when a patch fails to apply.
        "resolvemsg" => {
            take_value(tok, attached, args, i)?;
        }
        "no-resolvemsg" => no_value(tok, attached)?,
        "rerere-autoupdate" => o.rerere_autoupdate = Some(flag(tok, attached, true)?),
        "no-rerere-autoupdate" => o.rerere_autoupdate = Some(flag(tok, attached, false)?),
        // These only shape the commit this module never reaches.
        "committer-date-is-author-date"
        | "no-committer-date-is-author-date"
        | "ignore-date"
        | "no-ignore-date"
        | "verify"
        | "no-verify"
        | "binary"
        | "no-binary" => no_value(tok, attached)?,
        "gpg-sign" => {} // optional value, attached only
        "no-gpg-sign" => no_value(tok, attached)?,
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
            'S' => at = bytes.len(),
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
    /// The session directory is written and holds zero messages.
    Ready,
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

    match split_mail(format, &o.paths)? {
        Split::Failed(errors) => {
            // git creates the directory before splitting and `am_destroy`s it on
            // failure, so the net effect on the repository is nothing.
            for e in errors {
                eprintln!("error: {e}");
            }
            eprintln!("fatal: Failed to split patches.");
            return Ok(Setup::Failed(128));
        }
        Split::Messages(0) => {}
        Split::Messages(_) => bail!(
            "applying a mailbox is not yet ported: turning a message into a commit needs \
             `git mailinfo` mail parsing and a `git apply` patch applier, neither of which \
             exists in the vendored gitoxide crates"
        ),
    }

    std::fs::create_dir_all(state_dir)?;

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
    write_text(state_dir, "last", "0")?;
    Ok(Setup::Ready)
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

/// How many messages a mailbox holds, or why it could not be read.
enum Split {
    Messages(usize),
    Failed(Vec<String>),
}

fn split_mail(format: Format, paths: &[String]) -> Result<Split> {
    match format {
        Format::Mbox | Format::Mboxrd => split_mbox(paths),
        // `split_mail_conv` writes one message per input path, converting it;
        // with no paths it reads stdin as a single patch.
        Format::Stgit | Format::Hg => Ok(split_conv(paths)),
        Format::StgitSeries => split_stgit_series(paths),
    }
}

/// `git mailsplit`: each path is an mbox file or a Maildir, and no path at all
/// means stdin.
fn split_mbox(paths: &[String]) -> Result<Split> {
    if paths.is_empty() {
        return Ok(Split::Messages(usize::from(!read_stdin()?.is_empty())));
    }
    let mut total = 0usize;
    for p in paths {
        if p == "-" {
            total += usize::from(!read_stdin()?.is_empty());
            continue;
        }
        let path = Path::new(p);
        if path.is_dir() {
            // `populate_maildir_list` reads `new/` then `cur/`, ignoring dotfiles.
            for sub in ["new", "cur"] {
                if let Ok(entries) = std::fs::read_dir(path.join(sub)) {
                    total += entries
                        .filter_map(Result::ok)
                        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                        .count();
                }
            }
            continue;
        }
        match std::fs::metadata(path) {
            Ok(md) => total += usize::from(md.len() != 0),
            Err(e) => {
                return Ok(Split::Failed(vec![format!(
                    "cannot stat {p}: {}",
                    errno_msg(&e)
                )]))
            }
        }
    }
    Ok(Split::Messages(total))
}

/// `split_mail_conv`: one output message per input path, stdin when none.
fn split_conv(paths: &[String]) -> Split {
    if paths.is_empty() {
        return Split::Messages(1);
    }
    for p in paths {
        if p == "-" {
            continue;
        }
        // git has already written the messages for the preceding paths, but the
        // caller destroys the whole session directory on failure.
        if let Err(e) = std::fs::File::open(p) {
            return Split::Failed(vec![format!(
                "could not open '{p}' for reading: {}",
                errno_msg(&e)
            )]);
        }
    }
    Split::Messages(paths.len())
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
    Ok(split_conv(&listed))
}

// ---------------------------------------------------------------------------
// am_run
// ---------------------------------------------------------------------------

fn run(repo: &gix::Repository, state_dir: &Path) -> Result<ExitCode> {
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
        return Ok(ExitCode::from(128));
    }

    // The apply loop runs `while cur <= last`. A freshly set-up session only
    // reaches here with `last == 0`, but resuming a live session in a terminal
    // lands here too, and that one has messages waiting.
    let cur = read_count(state_dir, "next")?;
    let last = read_count(state_dir, "last")?;
    if cur <= last {
        bail!(
            "applying a mailbox is not yet ported: turning a message into a commit needs \
             `git mailinfo` mail parsing and a `git apply` patch applier, neither of which \
             exists in the vendored gitoxide crates"
        );
    }

    // Nothing left to apply, so `am_destroy` tears the session down.
    std::fs::remove_dir_all(state_dir)?;
    Ok(ExitCode::SUCCESS)
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
