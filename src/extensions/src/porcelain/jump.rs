//! `git jump` — emit "quickfix" lines for interesting spots and hand them to an
//! editor. **All four modes (`diff`, `merge`, `grep`, `ws`) plus `auto` are
//! ported; only the editor hand-off bails.**
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
//!   * `mode_grep`: `git grep -n --column <args>` — or the word-split (not
//!     `eval`'d) command in `jump.grepCmd` — piped through
//!     `perl -pe 's/[ \t]+/ /g; s/^ *//;'`. The default command re-runs this
//!     binary, which is the git installation the script would have found. The
//!     grep's exit status is discarded, as the pipeline discards it.
//!   * `mode_diff`: `git diff --no-prefix --relative <args>` piped through the
//!     perl filter that emits `<file>:<new-line>:1: <text>` for the first
//!     changed line of every hunk. Like `mode_grep`, the `git` the script means
//!     is the installation it ships with — this binary — so the patch comes from
//!     re-running `diff.rs`, which already implements both flags.
//!   * `mode_ws`: `git diff --check <args>`, whose output is already in quickfix
//!     form and is forwarded unfiltered.
//!   * `mode_auto`: the `--is-inside-work-tree` gate, then unmerged paths →
//!     `mode_merge`, then `git diff --quiet` → `mode_diff`, else usage/exit 1.
//!   * Running any mode outside a repository: the underlying git command's
//!     `fatal: not a git repository (or any of the parent directories): .git`
//!     goes to stderr and the script still exits 0, because only the pipeline's
//!     output is consulted.
//!
//! NOT ported — each bails, naming the missing substrate:
//!
//!   1. **The editor hand-off** — `git var GIT_EDITOR`, the `mktemp` file, and the
//!      emacs/vi `eval` split. Spawning the user's editor is not gitoxide
//!      substrate; a non-empty result therefore bails instead of pretending.
//!   2. **Options for `merge`.** Stock forwards them to `ls-files`, which answers
//!      an unknown one with its own multi-screen usage and the script still exits
//!      0. That text is not reproduced here; only `--` and pathspecs are accepted.
//!
//! `diff` and `ws` inherit whatever `diff.rs` gets right or wrong about
//! `--no-prefix`/`--relative`/`--check`; that is the same coupling stock has,
//! since the script shells out to the `git` it ships with.
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
        "diff" => mode_diff(mode_args)?,
        "ws" => mode_ws(mode_args)?,
        "grep" => mode_grep(mode_args)?,
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
        return Ok(Some(mode_diff(args)?));
    }
    Ok(None)
}

/// `mode_diff`:
///
/// ```sh
/// git diff --no-prefix --relative "$@" | perl -ne '…'
/// ```
///
/// The script is a driver: it runs the `git` it ships with, which is this
/// binary, exactly as `mode_grep` already does. The pipeline's exit status is
/// perl's, so a diff that failed still yields whatever it printed.
fn mode_diff(args: &[String]) -> Result<Vec<u8>> {
    let out = run_self(&["diff", "--no-prefix", "--relative"], args)?;
    Ok(first_line_of_each_hunk(&out))
}

/// `mode_ws`: `git diff --check "$@"`, unfiltered — its output is already in
/// `<file>:<line>: <error>` quickfix form.
fn mode_ws(args: &[String]) -> Result<Vec<u8>> {
    run_self(&["diff", "--check"], args)
}

/// Run this binary with `leading` followed by the mode's own arguments,
/// returning its stdout. stderr is inherited and the exit status discarded,
/// which is what a shell pipeline whose last stage is `perl` (or the bare
/// command, for `mode_ws`) does with it.
fn run_self(leading: &[&str], args: &[String]) -> Result<Vec<u8>> {
    let exe = std::env::current_exe()?;
    let out = std::process::Command::new(exe)
        .args(leading)
        .args(args)
        .stderr(std::process::Stdio::inherit())
        .output()?;
    Ok(out.stdout)
}

/// `mode_diff`'s perl filter, line for line:
///
/// ```perl
/// if (m{^\+\+\+ (.*?)\t?$}) { $file = $1 eq "/dev/null" ? undef : $1; next }
/// defined($file) or next;
/// if (m/^@@ .*?\+(\d+)/) { $line = $1; next }
/// defined($line) or next;
/// if (/^ /) { $line++; next }
/// if (/^[-+]\s*(.*)/) { print "$file:$line:1: $1\n"; $line = undef; }
/// ```
///
/// One element per hunk: the first changed line in it, reported at the hunk's
/// new-side start line. `$line = undef` after a hit is what stops a hunk from
/// contributing a second element; only the next `@@` re-arms it.
fn first_line_of_each_hunk(patch: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut file: Option<&[u8]> = None;
    let mut line: Option<u64> = None;

    for raw in patch.split_inclusive(|&b| b == b'\n') {
        // Perl's `$` matches before a final newline, so the trailing one is not
        // part of what the patterns see.
        let l = raw.strip_suffix(b"\n").unwrap_or(raw);

        if let Some(rest) = l.strip_prefix(b"+++ ") {
            // `(.*?)\t?$`: the non-greedy capture gives up at most one trailing
            // tab, which is how git terminates a name it had to quote or that
            // contains a space.
            let name = rest.strip_suffix(b"\t").unwrap_or(rest);
            file = if name == b"/dev/null" { None } else { Some(name) };
            continue;
        }
        if file.is_none() {
            continue;
        }
        if l.starts_with(b"@@ ") {
            // `.*?\+(\d+)` — the first `+<digits>` after the marker, which is the
            // new-side start of the hunk.
            line = hunk_new_start(&l[3..]);
            continue;
        }
        let Some(n) = line else { continue };
        if l.first() == Some(&b' ') {
            line = Some(n + 1);
            continue;
        }
        if matches!(l.first(), Some(b'-' | b'+')) {
            // `^[-+]\s*(.*)`: `\s*` is greedy and `\s` includes the newline, so a
            // change line holding nothing but blanks captures the empty string.
            let body = &l[1..];
            let text = match body.iter().position(|b| !is_perl_space(*b)) {
                Some(i) => &body[i..],
                None => &body[body.len()..],
            };
            out.extend_from_slice(file.expect("checked above"));
            out.extend_from_slice(format!(":{n}:1: ").as_bytes());
            out.extend_from_slice(text);
            out.push(b'\n');
            line = None;
        }
    }
    out
}

/// Perl's `\s` for a non-Unicode pattern: space, tab, newline, form feed,
/// carriage return and (since 5.18) vertical tab.
fn is_perl_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// The `\+(\d+)` of `@@ -<a>,<b> +<c>,<d> @@`: the first run of digits that
/// follows a `+`.
fn hunk_new_start(rest: &[u8]) -> Option<u64> {
    let plus = rest.iter().position(|&b| b == b'+')?;
    let digits: &[u8] = rest[plus + 1..]
        .split(|b| !b.is_ascii_digit())
        .next()
        .unwrap_or(&[]);
    if digits.is_empty() {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
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

/// `mode_grep`:
///
/// ```sh
/// cmd=$(git config jump.grepCmd)
/// test -n "$cmd" || cmd="git grep -n --column"
/// $cmd "$@" | perl -pe 's/[ \t]+/ /g; s/^ *//;'
/// ```
///
/// `$cmd` is unquoted, so the shell word-splits it into a command and its
/// leading arguments — it is not `eval`'d, so no quoting or redirection inside
/// it is honoured, and neither is a glob (the words here never contain one).
/// The pipeline's exit status is perl's, so a grep that matched nothing simply
/// produces no elements.
///
/// The default command runs *this* binary rather than whatever `git` a `PATH`
/// lookup would find: the script's `git` is the installation it ships with, and
/// this port is that installation.
fn mode_grep(args: &[String]) -> Result<Vec<u8>> {
    let configured = gix::discover(".")
        .ok()
        .and_then(|repo| {
            repo.config_snapshot()
                .string("jump.grepCmd")
                .map(|v| v.to_string())
        })
        .filter(|v| !v.is_empty());

    let mut argv: Vec<std::ffi::OsString> = match &configured {
        Some(cmd) => cmd.split_whitespace().map(Into::into).collect(),
        None => {
            let exe = std::env::current_exe()?;
            vec![exe.into(), "grep".into(), "-n".into(), "--column".into()]
        }
    };
    // A `jump.grepCmd` of only whitespace word-splits to nothing, which the shell
    // would then treat as running `"$@"` itself; that is not a command this port
    // can guess at.
    let Some((program, leading)) = argv.split_first_mut() else {
        crate::git_fatal!("jump.grepCmd is set but contains no command word");
    };
    let program = program.clone();
    let leading: Vec<std::ffi::OsString> = leading.to_vec();

    let out = std::process::Command::new(&program)
        .args(&leading)
        .args(args)
        .stderr(std::process::Stdio::inherit())
        .output()?;

    Ok(squeeze_blanks(&out.stdout))
}

/// The `perl -pe 's/[ \t]+/ /g; s/^ *//;'` filter `mode_grep` pipes through:
/// per line, every run of spaces and tabs collapses to one space and any leading
/// spaces are then dropped. `-p` prints each line whether or not it changed, and
/// a final line without a newline stays that way.
fn squeeze_blanks(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    for line in input.split_inclusive(|&b| b == b'\n') {
        let (body, eol): (&[u8], &[u8]) = match line.strip_suffix(b"\n") {
            Some(b) => (b, b"\n"),
            None => (line, b""),
        };
        let mut squeezed: Vec<u8> = Vec::with_capacity(body.len());
        let mut in_blank = false;
        for &b in body {
            if b == b' ' || b == b'\t' {
                if !in_blank {
                    squeezed.push(b' ');
                    in_blank = true;
                }
            } else {
                squeezed.push(b);
                in_blank = false;
            }
        }
        // `s/^ *//` runs after the collapse, so it can only ever strip the one
        // space the collapse left behind.
        let start = usize::from(squeezed.first() == Some(&b' '));
        out.extend_from_slice(&squeezed[start..]);
        out.extend_from_slice(eol);
    }
    out
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
            anyhow::bail!(
                "unsupported argument {a:?}: git jump forwards it to `git ls-files -u`, whose \
                 option parser and usage text are not reproduced here"
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
                .any(|&b| !(0x20..0x7f).contains(&b) || b == b'"' || b == b'\\')
            {
                anyhow::bail!(
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
            Err(e) => crate::git_fatal!("grep: {name}: {e}"),
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
            anyhow::bail!(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The expectations here are the output of `git jump`'s own perl filter,
    /// run verbatim over the same input:
    ///
    /// ```sh
    /// perl -ne '
    ///   if (m{^\+\+\+ (.*?)\t?$}) { $file = $1 eq "/dev/null" ? undef : $1; next }
    ///   defined($file) or next;
    ///   if (m/^@@ .*?\+(\d+)/) { $line = $1; next }
    ///   defined($line) or next;
    ///   if (/^ /) { $line++; next }
    ///   if (/^[-+]\s*(.*)/) { print "$file:$line:1: $1\n"; $line = undef; }
    ///   '
    /// ```
    #[test]
    fn one_element_per_hunk_matches_the_perl_filter() {
        // Two hunks in one file (the second reports its *deletion*, at the line
        // the preceding context advanced to), a deletion whose `+++ /dev/null`
        // disarms the file, and an addition whose text is empty after `\s*`.
        let patch = b"\
diff --git README.md README.md
index 9741694..1b0a2f1 100644
--- README.md
+++ README.md
@@ -1,3 +1,4 @@
 # fixture
 line two
+added here
 line three
@@ -10,4 +12,5 @@ context text
 keep
-drop me
+  replaced
 tail
diff --git gone.txt gone.txt
deleted file mode 100644
index bac0ee7..0000000
--- gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-was here
diff --git src/new.rs src/new.rs
new file mode 100644
index 0000000..46e89a2
--- /dev/null
+++ src/new.rs
@@ -0,0 +1,2 @@
+
+fn x() {}
";
        assert_eq!(
            first_line_of_each_hunk(patch),
            b"README.md:3:1: added here\nREADME.md:13:1: drop me\nsrc/new.rs:1:1: \n".to_vec()
        );
    }

    /// git terminates a `+++` path that needs it with a tab, which the filter's
    /// non-greedy `(.*?)\t?$` drops; the `\s*` after the `+` then eats the
    /// indentation of the added line.
    #[test]
    fn trailing_tab_and_indentation_match_the_perl_filter() {
        let patch = b"\
diff --git my file.txt my file.txt
index 111..222 100644
--- my file.txt\t
+++ my file.txt\t
@@ -5,2 +7,3 @@ fn ctx()
 unchanged
+\ttabbed add
 context
";
        assert_eq!(
            first_line_of_each_hunk(patch),
            b"my file.txt:8:1: tabbed add\n".to_vec()
        );
    }

    /// `@@ .*?\+(\d+)`: the first `+<digits>` after the marker. A hunk header
    /// whose old range is absent still parses, and a malformed one yields
    /// nothing rather than a wrong line number.
    #[test]
    fn hunk_new_start_reads_the_new_side_offset() {
        assert_eq!(hunk_new_start(b"-1,3 +1,4 @@"), Some(1));
        assert_eq!(hunk_new_start(b"-0,0 +12 @@ trailer"), Some(12));
        assert_eq!(hunk_new_start(b"-1 +7,2 @@"), Some(7));
        assert_eq!(hunk_new_start(b"nonsense"), None);
    }
}
