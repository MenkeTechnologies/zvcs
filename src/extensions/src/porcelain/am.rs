//! `git am` — apply a series of patches from a mailbox.
//!
//! ## What is served
//!
//! Only the session-state verbs that need no patch application, and for those
//! the output bytes, exit code and resulting repository state match stock git:
//!
//!   * `--show-current-patch[=(raw|diff)]` — dumps `.git/rebase-apply/<NNNN>`
//!     (raw, the whole mail) or `.git/rebase-apply/patch` (diff) verbatim.
//!   * `--quit` — removes the `.git/rebase-apply` state directory and
//!     `.git/MERGE_RR` (git's `am_rerere_clear`), leaving `HEAD`, index and
//!     worktree untouched. No output, exit 0.
//!   * every resume verb outside a session — `fatal: Resolve operation not in
//!     progress, we are not resuming.`, exit 128.
//!   * a mailbox handed to a live session — `fatal: previous rebase directory
//!     <dir> still exists but mbox given.`, exit 128.
//!   * conflicting resume verbs — `error: options '<new>' and '<old>' cannot be
//!     used together`, exit 129 — and `error: invalid value for
//!     '--show-current-patch': '<v>'`, exit 129.
//!
//! ## What is not served, and why
//!
//! Applying a mailbox — the default mode, and therefore `--continue`, `--skip`,
//! `--abort`, `--retry` and `--allow-empty` inside a session — is **not**
//! implemented. Three pieces of substrate are missing from the vendored
//! gitoxide crates in `src/ported`:
//!
//!   * **No patch applier.** `gix-diff` only *produces* unified diffs
//!     (`gix-diff/src/blob/unified_diff/`); nothing in the tree parses `@@`
//!     hunk headers or applies a diff to an index/worktree. `git apply`, which
//!     `git am` shells out to for every patch, has no counterpart here.
//!   * **No mail parsing.** There is no `git mailsplit` (mbox/Maildir splitter)
//!     and no `git mailinfo` (RFC 2047 header decode, subject cleanup,
//!     scissors, body/patch separation). `gix-mailmap` rewrites identities only.
//!   * **No sequencer.** `gix-sequencer/src/lib.rs` and `gix-rebase/src/lib.rs`
//!     contain only `#![forbid(unsafe_code)]`, so the `.git/rebase-apply` state
//!     machine those verbs drive does not exist to be resumed.
//!
//! Those paths bail with that reason rather than emit a guess: a patch applied
//! approximately is a silently wrong worktree, which is worse than an error.

use anyhow::{bail, Result};
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

/// The single `resume` mode git tracks; at most one may be selected.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Continue,
    Skip,
    Abort,
    Quit,
    Retry,
    AllowEmpty,
    ShowPatch(Sub),
}

/// Which file `--show-current-patch` dumps. A bare `--show-current-patch` means
/// `Raw`, which is why `--show-current-patch --show-current-patch=raw` is
/// accepted by git while `... --show-current-patch=diff` is not.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sub {
    Raw,
    Diff,
}

pub fn am(args: &[String]) -> Result<ExitCode> {
    // Dispatch strips the subcommand today; tolerate it being present at [0].
    let args: &[String] = match args.first() {
        Some(a) if a == "am" => &args[1..],
        _ => args,
    };

    // The selected mode plus the literal argv token that selected it — git's
    // incompatibility message quotes what the user typed, newest option first.
    let mut mode: Option<(Mode, String)> = None;
    let mut mailboxes: Vec<&str> = Vec::new();
    let mut end_of_opts = false;

    for a in args {
        if end_of_opts {
            mailboxes.push(a.as_str());
            continue;
        }
        let m = match a.as_str() {
            "--" => {
                end_of_opts = true;
                continue;
            }
            "--continue" | "-r" | "--resolved" => Mode::Continue,
            "--skip" => Mode::Skip,
            "--abort" => Mode::Abort,
            "--quit" => Mode::Quit,
            "--retry" => Mode::Retry,
            "--allow-empty" => Mode::AllowEmpty,
            "--show-current-patch" => Mode::ShowPatch(Sub::Raw),
            "--show-current-patch=raw" => Mode::ShowPatch(Sub::Raw),
            "--show-current-patch=diff" => Mode::ShowPatch(Sub::Diff),
            other if other.starts_with("--show-current-patch=") => {
                let v = &other["--show-current-patch=".len()..];
                eprintln!("error: invalid value for '--show-current-patch': '{v}'");
                return Ok(ExitCode::from(129));
            }
            other if other.len() > 1 && other.starts_with('-') => bail!(
                "unsupported flag {other:?} (ported: --quit, --show-current-patch[=(raw|diff)]); \
                 applying patches needs a `git apply` patch applier and `git mailinfo` mail \
                 parsing, neither of which exists in the vendored gitoxide crates"
            ),
            _ => {
                mailboxes.push(a.as_str());
                continue;
            }
        };
        match &mode {
            Some((prev, tok)) if *prev != m => {
                eprintln!("error: options '{a}' and '{tok}' cannot be used together");
                return Ok(ExitCode::from(129));
            }
            Some(_) => {}
            None => mode = Some((m, a.clone())),
        }
    }

    let repo = gix::discover(".")?;
    let state_dir = repo.git_dir().join("rebase-apply");
    let in_progress = state_dir.is_dir();

    let Some((mode, _)) = mode else {
        // Default mode: split the mailbox and apply each patch.
        if in_progress && !mailboxes.is_empty() {
            eprintln!(
                "fatal: previous rebase directory {} still exists but mbox given.",
                display_dir(&repo, &state_dir)
            );
            return Ok(ExitCode::from(128));
        }
        bail!(unported_reason());
    };

    if !in_progress {
        eprintln!("fatal: Resolve operation not in progress, we are not resuming.");
        return Ok(ExitCode::from(128));
    }

    match mode {
        Mode::ShowPatch(sub) => {
            let path = match sub {
                // git's `msgnum()`: the zero-padded number held in `next`.
                Sub::Raw => state_dir.join(format!("{:04}", current_patch_number(&state_dir)?)),
                Sub::Diff => state_dir.join("patch"),
            };
            let bytes = std::fs::read(&path)
                .map_err(|e| anyhow::anyhow!("could not open '{}' for reading: {e}", path.display()))?;
            std::io::stdout().write_all(&bytes)?;
            Ok(ExitCode::SUCCESS)
        }
        Mode::Quit => {
            // git: am_rerere_clear() then am_destroy(). Neither touches HEAD,
            // the index or the worktree — the session is simply forgotten.
            let merge_rr = repo.git_dir().join("MERGE_RR");
            if merge_rr.exists() {
                std::fs::remove_file(&merge_rr)?;
            }
            std::fs::remove_dir_all(&state_dir)?;
            Ok(ExitCode::SUCCESS)
        }
        // Every remaining verb has to re-apply or unwind applied patches.
        Mode::Continue | Mode::Skip | Mode::Abort | Mode::Retry | Mode::AllowEmpty => {
            bail!(unported_reason())
        }
    }
}

/// The reason applying is refused, phrased once for every path that hits it.
fn unported_reason() -> &'static str {
    "applying a mailbox is not ported: the vendored gitoxide crates have no patch applier \
     (gix-diff only writes unified diffs), no mbox/mailinfo parser, and gix-sequencer/gix-rebase \
     are empty placeholders, so no .git/rebase-apply session can be driven"
}

/// Read `.git/rebase-apply/next` — the 1-based index of the patch being applied.
fn current_patch_number(state_dir: &Path) -> Result<usize> {
    let raw = std::fs::read_to_string(state_dir.join("next"))?;
    Ok(raw.trim().parse::<usize>()?)
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
