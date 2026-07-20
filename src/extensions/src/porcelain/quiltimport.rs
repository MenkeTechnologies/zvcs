//! `git quiltimport` — apply a quilt patchset onto the current branch.
//! **The import itself is not ported: the first patch that would actually be
//! applied bails.**
//!
//! Stock `git-quiltimport` is a POSIX shell script
//! (`$(git --exec-path)/git-quiltimport`, `#!/bin/sh` on line 1). Its per-patch
//! body is four `git` invocations glued together:
//!
//! ```text
//! git mailinfo $MAILINFO_OPT "$tmp_msg" "$tmp_patch" <"$QUILT_PATCHES/$patch_name" >"$tmp_info"
//! git apply --index -C1 ${level:+"$level"} "$tmp_patch"
//! tree=$(git write-tree)
//! commit=$( { echo "$SUBJECT"; echo; cat "$tmp_msg"; } | git commit-tree $tree -p $commit )
//! git update-ref -m "quiltimport: $patch_name" HEAD $commit
//! ```
//!
//! `git mailinfo` is the missing substrate. It is what splits each quilt patch
//! into a message body, a diff, and the `Author:`/`Email:`/`Date:`/`Subject:`
//! info block the script then `sed`s the ident out of — and there is no
//! mailinfo port in this tree (`src/extensions/src/porcelain/` has `apply.rs`,
//! `write_tree.rs`, `commit_tree.rs` and `update_ref.rs`, but no `mailinfo.rs`),
//! nor any RFC-2822/mbox splitter under the vendored gitoxide crates in
//! `src/ported/` — gitoxide has no `gix-mailinfo` equivalent. Without it the
//! author ident, the subject, the message body and the diff hunk boundaries all
//! have to be invented, and the resulting commits (the whole observable result
//! of the command) would differ from stock. So the loop stops at the first
//! patch it would have to feed to mailinfo rather than approximating it.
//!
//! ### Covered (byte-identical stdout/stderr and exit code against git 2.55.0)
//!
//! Everything the script does before and around that call, all of which is
//! argument, config-free path and text handling:
//!
//! * The `git rev-parse --parseopt` front end that `git-sh-setup` builds from
//!   `OPTIONS_SPEC` (line 59 of `git-sh-setup`, before `git_dir_init`): the
//!   307-byte usage block on **stdout** for `-h` (exit 129), unique-prefix
//!   abbreviation (`--se` → `--series`), `--name=value` sticking (the script
//!   leaves `OPTIONS_STUCKLONG` empty, so parseopt re-splits it), the four
//!   error shapes — ``error: unknown option `x'``, ``error: unknown switch `x'``,
//!   ``error: option `x' requires a value``, ``error: option `x' takes no
//!   value`` — each followed by the usage block on **stderr**, exit 129, and
//!   git's `ambiguous option:` diagnostic with its last-two-candidates wording.
//! * `--no-*` negations, which parseopt accepts and passes through but the
//!   script's own `case` has no arm for: they fall to `*) usage`, which under
//!   `OPTIONS_SPEC` is a re-exec of `"$0" -h`, so the usage block lands on
//!   stdout with exit **1**, not 129.
//! * `git_dir_init`: `fatal: not a git repository …` exit 128, and — because
//!   the script sets `SUBDIRECTORY_ON`, not `SUBDIRECTORY_OK` — the
//!   `You need to run this command from the toplevel of the working tree.`
//!   check on stderr, exit 1.
//! * `--author` validation via the script's two `expr` BREs, including their
//!   greedy rightmost-`<` behaviour (`A <x> B <y@z>` → name `A <x> B`, email
//!   `y@z`) and `die "malformed --author parameter"` on stderr, exit 1.
//! * `$QUILT_PATCHES` / `$QUILT_SERIES` resolution with `:=` semantics (unset
//!   *or empty* takes the default), and the two existence messages, on
//!   **stdout**, exit 1: `The "<dir>" directory does not exist.` and
//!   `The "<file>" file does not exist.`
//! * `commit=$(git rev-parse HEAD)`, which is unchecked in the script: on an
//!   unborn HEAD it emits git's three-line `fatal: ambiguous argument 'HEAD'`
//!   to stderr and the script carries on regardless.
//! * `mkdir $tmp_dir || exit 2` against `$GIT_DIR/rebase-apply`.
//! * The whole series-file loop short of mailinfo: `while read patch_name level
//!   garbage` field splitting, blank and `#`-comment lines skipped, a trailing
//!   line with no newline never processed (shell `read` fails on it), `-p*`
//!   levels accepted, `unable to parse patch level, ignoring it.` for anything
//!   else, `trailing garbage found in series file: <garbage>` exit 1 —
//!   which, faithfully, leaves `$GIT_DIR/rebase-apply` behind — and
//!   `<name> doesn't exist. Skipping.` for a missing patch file. A series that
//!   names no existing patch therefore completes exactly as stock does:
//!   `rm -rf` the temp dir, exit 0.
//!
//! ### Not covered
//!
//! * Any patch that exists: mailinfo, `git apply --index -C1`, `write-tree`,
//!   `commit-tree`, `update-ref`, and the `Patch is empty.  Was it split
//!   wrong?` guard. This bails, after removing the temp dir it created so the
//!   repository is not left mid-import.
//! * `--dry-run` past that same point — it still runs mailinfo per patch, so
//!   `No author found in <patch>` and the interactive `Author: ` prompt are
//!   unreachable here.
//! * `read`'s backslash processing (the script uses plain `read`, not
//!   `read -r`), so a series line containing `\` is field-split literally.
//! * The `mkdir` failure line is the platform `mkdir(1)` text. The BSD form
//!   (`mkdir: <path>: File exists`) is emitted, matching this tree's Darwin
//!   target; GNU coreutils words it differently.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The usage block parseopt renders from `OPTIONS_SPEC`: 307 bytes, help
/// column 22.
const USAGE: &str = "\
usage: git quiltimport [options]

    -n, --[no-]dry-run    dry run
    --[no-]author ...     author name and email address for patches without any
    --[no-]patches ...    path to the quilt patches
    --[no-]series ...     path to the quilt series file
    --[no-]keep-non-patch Pass -b to git mailinfo

";

/// One line of `OPTIONS_SPEC`: short letter (only `-n` has one), long name, and
/// whether the spec line ended in `=` (i.e. the option takes a value).
struct Spec {
    short: Option<char>,
    long: &'static str,
    takes_value: bool,
}

/// `OPTIONS_SPEC`, in declaration order — the order git's `parse_options` scans
/// for abbreviation matches, which the `ambiguous option` wording depends on.
const SPECS: &[Spec] = &[
    Spec { short: Some('n'), long: "dry-run", takes_value: false },
    Spec { short: None, long: "author", takes_value: true },
    Spec { short: None, long: "patches", takes_value: true },
    Spec { short: None, long: "series", takes_value: true },
    Spec { short: None, long: "keep-non-patch", takes_value: false },
];

/// A token as parseopt would emit it into the script's `set --` line.
enum Token {
    /// `-n` / `--author` etc., with the value parseopt detached from it.
    Opt(String, Option<String>),
    /// `--no-<long>`: accepted by parseopt, unhandled by the script's `case`.
    Negated,
}

/// `-h` and the script's own `usage()` write the block to stdout; parseopt's
/// error paths write the message and the block to stderr.
fn usage_stdout(code: u8) -> ExitCode {
    print!("{USAGE}");
    ExitCode::from(code)
}

/// `error: <msg>` then the usage block, all on stderr, exit 129 — git's
/// `usage_with_options` after `error()`.
fn usage_stderr(msg: String) -> ExitCode {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// The outcome of the parseopt pass: either tokens for the script's loop, or a
/// terminal exit code it never gets to see.
enum Parseopt {
    Tokens(Vec<Token>),
    Exit(ExitCode),
}

/// Reproduce `git rev-parse --parseopt` over [`SPECS`].
///
/// `OPTIONS_KEEPDASHDASH` and `OPTIONS_STUCKLONG` are both empty in the script,
/// so `--` is swallowed, `--name=value` is re-split, and positionals are
/// permuted to the tail — where the script's loop, which breaks on the `--`
/// parseopt always emits, never looks at them.
fn parseopt(args: &[String]) -> Parseopt {
    let mut out = Vec::new();
    let mut it = args.iter().peekable();

    while let Some(arg) = it.next() {
        if arg == "--" {
            break;
        }
        // A lone `-` and anything without a leading dash is a positional; they
        // are permuted past the `--` and thus unreachable.
        let Some(body) = arg.strip_prefix('-').filter(|b| !b.is_empty()) else {
            continue;
        };

        if let Some(long) = body.strip_prefix('-') {
            let (name, attached) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            // Negations are matched against `no-<long>` for every option, since
            // parse_options synthesises a `--no-` form for each.
            let negated_names: Vec<String> =
                SPECS.iter().map(|s| format!("no-{}", s.long)).collect();
            let mut hits: Vec<(Option<&Spec>, &str)> = Vec::new();
            for spec in SPECS {
                if spec.long == name {
                    hits.clear();
                    hits.push((Some(spec), spec.long));
                    break;
                }
                if spec.long.starts_with(name) {
                    hits.push((Some(spec), spec.long));
                }
            }
            if hits.len() != 1 || hits[0].1 != name {
                for neg in &negated_names {
                    if neg == name {
                        hits.clear();
                        hits.push((None, neg.as_str()));
                        break;
                    }
                    if neg.starts_with(name) {
                        hits.push((None, neg.as_str()));
                    }
                }
            }

            match hits.len() {
                0 => return Parseopt::Exit(usage_stderr(format!("unknown option `{name}'"))),
                1 => {}
                _ => {
                    // git keeps the last two abbreviation candidates it saw:
                    // `(could be --<second-to-last> or --<last>)`.
                    let a = hits[hits.len() - 2].1;
                    let b = hits[hits.len() - 1].1;
                    return Parseopt::Exit(usage_stderr(format!(
                        "ambiguous option: {name} (could be --{a} or --{b})"
                    )));
                }
            }

            let (spec, _matched) = hits[0];
            let Some(spec) = spec else {
                // A `--no-<long>`: parseopt passes it through verbatim.
                out.push(Token::Negated);
                continue;
            };
            if !spec.takes_value {
                if attached.is_some() {
                    return Parseopt::Exit(usage_stderr(format!(
                        "option `{}' takes no value",
                        spec.long
                    )));
                }
                out.push(Token::Opt(format!("--{}", spec.long), None));
                continue;
            }
            let value = match attached.or_else(|| it.next().cloned()) {
                Some(v) => v,
                None => {
                    return Parseopt::Exit(usage_stderr(format!(
                        "option `{}' requires a value",
                        spec.long
                    )))
                }
            };
            out.push(Token::Opt(format!("--{}", spec.long), Some(value)));
            continue;
        }

        // Short cluster. `-h` is synthesised by parse_options itself.
        let mut rest = body;
        while let Some(c) = rest.chars().next() {
            rest = &rest[c.len_utf8()..];
            if c == 'h' {
                return Parseopt::Exit(usage_stdout(129));
            }
            match SPECS.iter().find(|s| s.short == Some(c)) {
                None => return Parseopt::Exit(usage_stderr(format!("unknown switch `{c}'"))),
                Some(spec) if !spec.takes_value => {
                    out.push(Token::Opt(format!("-{c}"), None));
                }
                Some(spec) => {
                    let value = if rest.is_empty() {
                        it.next().cloned()
                    } else {
                        Some(std::mem::take(&mut rest).to_string())
                    };
                    match value {
                        Some(v) => out.push(Token::Opt(format!("--{}", spec.long), Some(v))),
                        None => {
                            return Parseopt::Exit(usage_stderr(format!(
                                "option `{}' requires a value",
                                spec.long
                            )))
                        }
                    }
                    break;
                }
            }
        }
    }

    Parseopt::Tokens(out)
}

/// `expr "z$a" : 'z\(.*[^ ]\) *<.*'` — the greedy leading group, so the split
/// happens at the *rightmost* `<` whose prefix, with trailing spaces removed,
/// is non-empty and ends in a non-space. Backtracks to earlier `<`s if not.
fn author_name(author: &str) -> Option<&str> {
    let mut end = author.len();
    while let Some(lt) = author[..end].rfind('<') {
        let name = author[..lt].trim_end_matches(' ');
        if !name.is_empty() {
            return Some(name);
        }
        end = lt;
    }
    None
}

/// `expr "z$a" : '.*<\([^>]*\)'` — greedy `.*`, so the rightmost `<`, then
/// everything up to the next `>` or end of string. Empty capture counts as no
/// match (`expr` exits 1), which the script's `test '' != …` also rejects.
fn author_email(author: &str) -> Option<&str> {
    let lt = author.rfind('<')?;
    let tail = &author[lt + 1..];
    let email = match tail.find('>') {
        Some(gt) => &tail[..gt],
        None => tail,
    };
    (!email.is_empty()).then_some(email)
}

/// `: ${VAR:=default}` — the `:=` form, so an empty value also takes the
/// default.
fn env_or(name: &str, default: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => default.to_string(),
    }
}

/// One parsed series line, as `read patch_name level garbage` produces it.
struct SeriesLine {
    patch_name: String,
    level: String,
    garbage: String,
}

/// Split a series line the way `read a b c` does with the default `IFS`:
/// leading whitespace dropped, two whitespace-delimited fields, and the
/// remainder — internal spacing preserved, trailing whitespace stripped — into
/// the third variable.
fn read_fields(line: &str) -> SeriesLine {
    let ws = |c: char| c == ' ' || c == '\t';
    let rest = line.trim_start_matches(ws);
    let (patch_name, rest) = match rest.find(ws) {
        Some(i) => (&rest[..i], rest[i..].trim_start_matches(ws)),
        None => (rest, ""),
    };
    let (level, rest) = match rest.find(ws) {
        Some(i) => (&rest[..i], rest[i..].trim_start_matches(ws)),
        None => (rest, ""),
    };
    SeriesLine {
        patch_name: patch_name.to_string(),
        level: level.to_string(),
        garbage: rest.trim_end_matches(ws).to_string(),
    }
}

/// The lines a shell `while read` loop actually processes: a final line with no
/// terminating newline leaves `read` non-zero, so the body never runs for it.
fn complete_lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n')
        .filter(|l| l.ends_with('\n'))
        .map(|l| l.trim_end_matches('\n'))
        .collect()
}

/// `git-sh-setup`'s `git_dir_init` with `SUBDIRECTORY_OK` unset: locate the
/// repository, refuse to run below its top level, and return the absolute
/// `$GIT_DIR`.
fn git_dir_init() -> Result<PathBuf, ExitCode> {
    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(_) => {
            eprintln!("fatal: not a git repository (or any of the parent directories): .git");
            return Err(ExitCode::from(128));
        }
    };
    // `test -z "$(git rev-parse --show-cdup)"`: empty for a bare repository and
    // at the top of a work tree, non-empty anywhere below it.
    if let Some(workdir) = repo.workdir() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let same = match (cwd.canonicalize(), workdir.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => cwd == workdir,
        };
        if !same {
            eprintln!("You need to run this command from the toplevel of the working tree.");
            return Err(ExitCode::from(1));
        }
    }
    let git_dir = repo.git_dir().to_path_buf();
    match git_dir.canonicalize() {
        Ok(abs) => Ok(abs),
        Err(_) => {
            eprintln!("Unable to determine absolute path of git directory");
            Err(ExitCode::from(1))
        }
    }
}

/// The BSD `mkdir(1)` diagnostic the script's `mkdir $tmp_dir` would print.
fn mkdir_error(path: &Path, err: &std::io::Error) -> String {
    let reason = match err.kind() {
        std::io::ErrorKind::AlreadyExists => "File exists".to_string(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        _ => err.to_string(),
    };
    format!("mkdir: {}: {reason}", path.display())
}

/// `git quiltimport` — apply a quilt patchset onto the current branch.
///
/// Reproduces the shell script up to, but not including, the point where each
/// patch would be handed to `git mailinfo`; that bails, naming the missing
/// substrate. See the module docs for the exact split.
pub fn quiltimport(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand name at index 0 so both calling conventions work.
    let args = match args.first() {
        Some(a) if a == "quiltimport" => &args[1..],
        _ => args,
    };

    // `. git-sh-setup` runs the parseopt eval (line 59) before `git_dir_init`
    // (end of file), so option errors precede the repository check.
    let tokens = match parseopt(args) {
        Parseopt::Exit(code) => return Ok(code),
        Parseopt::Tokens(t) => t,
    };
    let git_dir = match git_dir_init() {
        Ok(dir) => dir,
        Err(code) => return Ok(code),
    };

    // The script's own option loop over what parseopt handed back.
    let mut dry_run = false;
    let mut quilt_author: Option<String> = None;
    let mut quilt_patches: Option<String> = None;
    let mut quilt_series: Option<String> = None;
    let mut keep_non_patch = false;
    for token in &tokens {
        match token {
            // `*) usage` — under OPTIONS_SPEC that is `"$0" -h; exit 1`, so the
            // block goes to stdout but the status is 1.
            Token::Negated => return Ok(usage_stdout(1)),
            Token::Opt(name, value) => match name.as_str() {
                "-n" | "--dry-run" => dry_run = true,
                "--author" => quilt_author = value.clone(),
                "--patches" => quilt_patches = value.clone(),
                "--series" => quilt_series = value.clone(),
                "--keep-non-patch" => keep_non_patch = true,
                other => bail!("unsupported flag {other:?} (ported: -n/--dry-run, --author, --patches, --series, --keep-non-patch)"),
            },
        }
    }

    // Quilt author. Both `expr`s must match non-empty or the script dies.
    if let Some(author) = quilt_author.as_deref().filter(|a| !a.is_empty()) {
        if author_name(author).is_none() || author_email(author).is_none() {
            eprintln!("malformed --author parameter");
            return Ok(ExitCode::from(1));
        }
    }

    // `: ${QUILT_PATCHES:=patches}` — the flag wins, then the environment.
    let patches_dir = match quilt_patches.filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => env_or("QUILT_PATCHES", "patches"),
    };
    if !Path::new(&patches_dir).is_dir() {
        println!("The \"{patches_dir}\" directory does not exist.");
        return Ok(ExitCode::from(1));
    }

    // `: ${QUILT_SERIES:=$QUILT_PATCHES/series}`; `[ -e ]`, so any file type.
    let series_file = match quilt_series.filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => env_or("QUILT_SERIES", &format!("{patches_dir}/series")),
    };
    if !Path::new(&series_file).exists() {
        println!("The \"{series_file}\" file does not exist.");
        return Ok(ExitCode::from(1));
    }

    // `commit=$(git rev-parse HEAD)` — unchecked, so an unborn HEAD only
    // produces git's diagnostic and the script continues.
    if let Ok(repo) = gix::discover(".") {
        if repo.head_id().is_err() {
            eprintln!(
                "fatal: ambiguous argument 'HEAD': unknown revision or path not in the working tree."
            );
            eprintln!("Use '--' to separate paths from revisions, like this:");
            eprintln!("'git <command> [<revision>...] -- [<file>...]'");
        }
    }

    // Read the series before `mkdir`, so a read failure cannot leave the temp
    // directory behind. The script reads it lazily via fd 3; the only
    // observable difference is on an unreadable series file.
    let series = std::fs::read(&series_file)
        .map_err(|e| anyhow::anyhow!("cannot read series file {series_file:?}: {e}"))?;
    let series = String::from_utf8_lossy(&series).into_owned();

    let tmp_dir = git_dir.join("rebase-apply");
    if let Err(e) = std::fs::create_dir(&tmp_dir) {
        eprintln!("{}", mkdir_error(&tmp_dir, &e));
        return Ok(ExitCode::from(2));
    }

    for line in complete_lines(&series) {
        let SeriesLine { patch_name, level, garbage } = read_fields(line);
        if patch_name.is_empty() || patch_name.starts_with('#') {
            continue;
        }
        if !(level.is_empty() || level.starts_with('#') || level.starts_with("-p")) {
            println!("unable to parse patch level, ignoring it.");
        }
        if !(garbage.is_empty() || garbage.starts_with('#')) {
            println!("trailing garbage found in series file: {garbage}");
            // The script exits here without removing the temp directory.
            return Ok(ExitCode::from(1));
        }
        let patch_path = Path::new(&patches_dir).join(&patch_name);
        if !patch_path.is_file() {
            println!("{patch_name} doesn't exist. Skipping.");
            continue;
        }

        // Everything past this point needs `git mailinfo`. Remove the temp
        // directory first so the repository is not left mid-import.
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let _ = (dry_run, keep_non_patch);
        bail!(
            "unsupported: applying {patch_name:?} needs `git mailinfo` to split the quilt patch \
             into message, diff and author ident; no mailinfo port exists in this tree and \
             gitoxide provides no mbox/RFC-2822 splitter (ported: option parsing, --author \
             validation, QUILT_PATCHES/QUILT_SERIES resolution, series-file parsing)"
        );
    }

    // `rm -rf $tmp_dir || exit 5`
    if std::fs::remove_dir_all(&tmp_dir).is_err() {
        return Ok(ExitCode::from(5));
    }
    Ok(ExitCode::SUCCESS)
}
