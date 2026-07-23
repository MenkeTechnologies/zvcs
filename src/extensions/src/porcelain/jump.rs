//! `git jump` — emit "quickfix" lines for interesting spots and hand them to an
//! editor. **Only the `merge` mode is ported; `diff`, `ws` and `grep` bail.**
//!
//! Stock `git jump` is a `/bin/sh` script installed in `$(git --exec-path)`
//! (originally `contrib/git-jump/git-jump`; 2.55.0 ships it at
//! `libexec/git-core/git-jump`). It is a driver: every mode is a shell pipeline
//! over other git commands plus `perl`, `sort` and `grep`, and the default exit
//! path writes the result to a `mktemp` file and `eval`s `git var GIT_EDITOR`.
//!
//! Ported, byte-verified against git 2.55.0 on Darwin:
//!
//!   * The `usage()` heredoc, verbatim, on **stderr** with exit 1 — for an
//!     unknown `--*` option (the glob matches a bare `--` too), an unknown mode,
//!     and the two `mode_auto` dead ends.
//!   * The option loop: `--stdout` may repeat, the first non-`--*` word ends it
//!     (so a bare `-` and `-x` become *modes*, not options, and fail the mode
//!     check), and an empty argument list defaults to `auto`.
//!   * `--stdout`: print the quickfix lines and `exit 0`. Without it, the script
//!     runs the mode first and `test -s "$tmp" || exit 0` — an empty result is
//!     exit 0 with no editor, which is reproduced exactly.
//!   * `mode_merge`: `git ls-files -u <args>` → strip through the first tab →
//!     `sort -u` → `grep -Hn '^<<<<<<<'` per file. Paths are cwd-relative and
//!     pathspec-limited exactly as `ls-files` resolves them, `grep`'s
//!     `grep: <file>: No such file or directory` is reproduced on stderr for a
//!     delete/modify conflict, and its exit status is discarded as the pipeline
//!     does.
//!   * `mode_auto`: the `--is-inside-work-tree` gate, then unmerged paths →
//!     `mode_merge`, then `git diff --quiet` → `mode_diff`, else usage/exit 1.
//!   * Running any mode outside a repository: the underlying git command's
//!     `fatal: not a git repository (or any of the parent directories): .git`
//!     goes to stderr and the script still exits 0, because only the pipeline's
//!     output is consulted.
//!
//! NOT ported — each bails, naming the missing substrate:
//!
//!   1. **`mode_diff`** — `git diff --no-prefix --relative "$@"` piped through a
//!      perl filter that emits `<file>:<new-line>:1: <text>` for the first
//!      changed line of each hunk. `diff.rs` cannot be reused (only `pub fn
//!      diff` is exported, its hunk walk is module-private, and it implements
//!      neither `--no-prefix` nor `--relative`), and the mode forwards arbitrary
//!      user diff arguments, so this needs a second patch generator rather than
//!      a call.
//!   2. **`mode_ws`** — `git diff --check`. `gix-diff` has no whitespace-error
//!      checker at all (`grep -rl whitespace src/ported/gix-diff/src/` matches
//!      nothing), so git's `ws_check_emit` output has no backing.
//!   3. **`mode_grep`** — forwards arbitrary arguments to `git grep -n --column`
//!      or, when `jump.grepCmd` is set, to an arbitrary external command word-split
//!      by the shell. `grep.rs` ships no regex engine (literal patterns only) and
//!      running a configured foreign command is subprocess work, not a port.
//!   4. **The editor hand-off** — `git var GIT_EDITOR`, the `mktemp` file, and the
//!      emacs/vi `eval` split. Spawning the user's editor is not gitoxide
//!      substrate; a non-empty result therefore bails instead of pretending.
//!   5. **Options for `merge`.** Stock forwards them to `ls-files`, which answers
//!      an unknown one with its own multi-screen usage and the script still exits
//!      0. That text is not reproduced here; only `--` and pathspecs are accepted.
//!
//! Known divergences, deliberately left rather than guessed at: `sort -u` uses
//! the caller's collation while this port sorts by bytes (identical for ASCII
//! paths), and a conflicted file containing NUL makes system `grep` print
//! `Binary file <f> matches` — that case bails instead of emitting line hits.

use anyhow::{bail, Result};
use std::collections::BTreeSet;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};

/// The `usage()` heredoc, byte for byte (858 bytes including the final newline).
const USAGE: &str = "\
usage: git jump [--stdout] <mode> [<args>]
   or: git jump [--stdout]

Jump to interesting elements in an editor.
The <mode> parameter is one of the following.
With no <mode> and no <args>, it defaults to \"auto\".

diff: elements are diff hunks. Arguments are given to diff.

merge: elements are merge conflicts. Arguments are given to ls-files -u.

grep: elements are grep hits. Arguments are given to git grep or, if
      configured, to the command in `jump.grepCmd`.

ws: elements are whitespace errors. Arguments are given to diff --check.

auto: select one of the other modes based on worktree state;
      \"merge\" if there are unmerged paths, \"diff\" if there are
      unstaged changes, \"ws\" if there are whitespace errors.

If the optional argument `--stdout` is given, print the quickfix
lines to standard output instead of feeding it to the editor.
";

/// `usage >&2; exit 1`.
fn usage_err() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(1)
}

/// The conflict marker `grep -Hn '^<<<<<<<'` looks for.
const MARKER: &[u8] = b"<<<<<<<";

/// `git jump` — see the module documentation for the ported surface.
pub fn jump(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail; tolerate the subcommand at index
    // 0 so both calling conventions behave identically.
    let args = match args.first() {
        Some(a) if a == "jump" => &args[1..],
        _ => args,
    };

    // The option loop. `--stdout` sets the flag, any other `--*` (including a
    // bare `--`) is a usage error, and anything else breaks out as the mode.
    let mut use_stdout = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--stdout" => use_stdout = true,
            a if a.starts_with("--") => return Ok(usage_err()),
            _ => break,
        }
        i += 1;
    }

    // `if test $# -lt 1; then set -- auto; fi` then `mode=$1; shift`.
    let (mode, mode_args): (&str, &[String]) = match args[i..].split_first() {
        Some((m, rest)) => (m.as_str(), rest),
        None => ("auto", &[]),
    };

    // `type "mode_$mode" >/dev/null 2>&1 || { usage >&2; exit 1; }`. The script
    // resolves any command of that name, so a `mode_<x>` executable on PATH
    // would also pass; only the five real functions are honoured here.
    if !matches!(mode, "diff" | "merge" | "grep" | "ws" | "auto") {
        return Ok(usage_err());
    }

    let quickfix = match mode {
        "merge" => mode_merge(mode_args)?,
        "auto" => match mode_auto(mode_args)? {
            Some(lines) => lines,
            None => return Ok(usage_err()),
        },
        "diff" => bail!(
            "unsupported mode \"diff\" (ported: merge, auto, --stdout, the option loop and \
             usage/exit codes). It runs `git diff --no-prefix --relative` over arbitrary \
             user-supplied diff arguments and derives one quickfix line per hunk from the \
             patch text; diff.rs exports only `pub fn diff`, keeps its hunk walk private, \
             and implements neither --no-prefix nor --relative"
        ),
        "ws" => bail!(
            "unsupported mode \"ws\" (ported: merge, auto). It is `git diff --check`, and \
             gix-diff contains no whitespace-error checker — nothing under src/ported backs \
             git's ws_check_emit output"
        ),
        "grep" => bail!(
            "unsupported mode \"grep\" (ported: merge, auto). It forwards arbitrary arguments \
             to `git grep -n --column`, or to the arbitrary external command in jump.grepCmd; \
             grep.rs has no regex engine (literal patterns only) and running a configured \
             foreign command is subprocess work"
        ),
        _ => unreachable!("mode was validated above"),
    };

    if use_stdout {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(&quickfix)?;
        out.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    // `test -s "$tmp" || exit 0` — no elements means no editor and a clean exit.
    if quickfix.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    bail!(
        "unsupported: handing the quickfix list to an editor is not ported ({} bytes of \
         elements found; re-run with --stdout to print them). Stock resolves `git var \
         GIT_EDITOR`, writes a mktemp file and `eval`s the editor with -q (or an emacs \
         --eval form) — spawning the user's editor is not gitoxide substrate",
        quickfix.len()
    );
}

/// `mode_auto`. `Ok(None)` is the script's `usage >&2; exit 1` path.
fn mode_auto(args: &[String]) -> Result<Option<Vec<u8>>> {
    // `test "$(git rev-parse --is-inside-work-tree 2>/dev/null)" != "true"` —
    // false both outside a repository and inside its git directory.
    let Ok(repo) = gix::discover(".") else {
        return Ok(None);
    };
    if !is_inside_work_tree(&repo) {
        return Ok(None);
    }

    // `test -n "$(git ls-files -u "$@")"` — any unmerged entry selects merge mode.
    let conflicted = unmerged_paths(&repo, args)?;
    if !conflicted.is_empty() {
        return Ok(Some(grep_markers(&conflicted)?));
    }

    // `! git diff --quiet "$@"` — index vs worktree only, staged changes and
    // untracked files do not count.
    if has_unstaged_changes(&repo, args)? {
        bail!(
            "unsupported: `git jump` with unstaged changes selects mode \"diff\", which is not \
             ported — it runs `git diff --no-prefix --relative` and derives a quickfix line per \
             hunk; diff.rs exports only `pub fn diff` and implements neither flag"
        );
    }
    Ok(None)
}

/// `mode_merge`: `git ls-files -u "$@"` → first-tab strip → `sort -u` →
/// `grep -Hn '^<<<<<<<' "$fn"` per file.
fn mode_merge(args: &[String]) -> Result<Vec<u8>> {
    // Outside a repository `git ls-files` prints its own fatal and the script
    // ignores the status, so the run still ends at exit 0 with no elements.
    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(Vec::new());
    };
    let paths = unmerged_paths(&repo, args)?;
    grep_markers(&paths)
}

/// The `ls-files -u` half of `mode_merge`: cwd-relative paths of every entry at
/// a conflict stage, pathspec-limited, deduplicated and byte-sorted (`sort -u`).
fn unmerged_paths(repo: &gix::Repository, args: &[String]) -> Result<BTreeSet<BString>> {
    let mut patterns: Vec<BString> = Vec::new();
    let mut no_more_flags = false;
    for a in args {
        if !no_more_flags && a == "--" {
            no_more_flags = true;
            continue;
        }
        if !no_more_flags && a.starts_with('-') {
            bail!(
                "unsupported argument {a:?}: git jump forwards it to `git ls-files -u`, whose \
                 option parser and usage text are not reproduced here (ported: -- and pathspecs)"
            );
        }
        patterns.push(BString::from(a.as_str()));
    }

    let index = repo.open_index()?;

    // Index paths are repository-relative; `ls-files` prints them relative to the
    // current directory, which is what the following `grep` then opens.
    let prefix: Option<BString> = match repo.prefix()? {
        Some(p) if !p.as_os_str().is_empty() => {
            let mut b = gix::path::into_bstr(p).into_owned();
            b.push(b'/');
            Some(b)
        }
        _ => None,
    };

    // `empty_patterns_match_prefix = true` reproduces git's default of limiting a
    // bare invocation from a subdirectory to that subdirectory.
    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;

    let mut out = BTreeSet::new();
    if let Some(iter) = ps.index_entries_with_paths(&index) {
        for (path, entry) in iter {
            if entry.stage_raw() == 0 {
                continue;
            }
            let display: &[u8] = match &prefix {
                Some(pref) => path
                    .as_bytes()
                    .strip_prefix(pref.as_bytes())
                    .unwrap_or_else(|| path.as_bytes()),
                None => path.as_bytes(),
            };
            // A path git would render with `core.quotePath` reaches the shell as
            // its quoted spelling, which `grep` then fails to open. Refuse rather
            // than guess which of the two spellings the harness will see.
            if display
                .iter()
                .any(|&b| b < 0x20 || b >= 0x7f || b == b'"' || b == b'\\')
            {
                bail!(
                    "unsupported path {:?}: git ls-files renders it in quoted form and stock \
                     git-jump then greps a filename that does not exist",
                    display.as_bstr()
                );
            }
            out.insert(BString::from(display));
        }
    }
    Ok(out)
}

/// The `grep -Hn '^<<<<<<<' "$fn"` half of `mode_merge`, run once per file with
/// its status discarded, exactly as the `while read` loop does.
fn grep_markers(paths: &BTreeSet<BString>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for path in paths {
        let name = path.to_str_lossy();
        let content = match std::fs::read(name.as_ref()) {
            Ok(c) => c,
            // grep reports and moves on; the pipeline keeps its own exit status.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("grep: {name}: No such file or directory");
                continue;
            }
            Err(e) => bail!("grep: {name}: {e}"),
        };

        // Trailing newline terminates the last line rather than starting a new one.
        let body = content.strip_suffix(b"\n").unwrap_or(&content);
        let mut hits: Vec<(usize, &[u8])> = Vec::new();
        for (n, line) in body.split(|&b| b == b'\n').enumerate() {
            if line.starts_with(MARKER) {
                hits.push((n + 1, line));
            }
        }
        if hits.is_empty() {
            continue;
        }
        if content.contains(&0) {
            bail!(
                "unsupported binary conflicted file {name:?}: system grep answers \
                 \"Binary file {name} matches\" instead of line hits, which is not reproduced"
            );
        }
        for (n, line) in hits {
            out.extend_from_slice(path.as_bytes());
            write!(out, ":{n}:")?;
            out.extend_from_slice(line);
            out.push(b'\n');
        }
    }
    Ok(out)
}

/// `git diff --quiet "$@"` — true when the worktree differs from the index for a
/// tracked path. Untracked files and stat-only staleness do not count, matching
/// `diff-files` after the refresh git performs first.
fn has_unstaged_changes(repo: &gix::Repository, args: &[String]) -> Result<bool> {
    let patterns: Vec<BString> = args.iter().map(|a| BString::from(a.as_str())).collect();
    for item in repo.status(gix::progress::Discard)?.into_iter(patterns)? {
        if let gix::status::Item::IndexWorktree(iw) = item? {
            use gix::status::index_worktree::Item;
            use gix::status::plumbing::index_as_worktree::EntryStatus;
            match iw {
                Item::Modification { status, .. } => match status {
                    EntryStatus::NeedsUpdate(_) => {}
                    _ => return Ok(true),
                },
                Item::Rewrite { .. } => return Ok(true),
                Item::DirectoryContents { .. } => {}
            }
        }
    }
    Ok(false)
}

/// `git rev-parse --is-inside-work-tree` — a worktree exists and the current
/// directory is not inside the git directory itself.
fn is_inside_work_tree(repo: &gix::Repository) -> bool {
    if repo.workdir().is_none() {
        return false;
    }
    let (Ok(cwd), Ok(git_dir)) = (
        std::env::current_dir().and_then(std::fs::canonicalize),
        std::fs::canonicalize(repo.git_dir()),
    ) else {
        return false;
    };
    !cwd.starts_with(git_dir)
}
