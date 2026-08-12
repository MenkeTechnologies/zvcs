//! `git bugreport` — write a pre-filled bug-report template to a file and open
//! it in the user's editor.
//!
//! Ported from `builtin/bugreport.c`. Covered, byte-identically with stock git:
//! the option grammar (`-o`/`--output-directory`, `-s`/`--suffix`,
//! `--no-suffix`, `--diagnose[=<mode>]`, `--no-diagnose`, `--` , unique-prefix
//! long-option abbreviation, `-h`), the `-h`/unknown-option/unknown-argument
//! usage text and their exit codes, `setup_git_directory_gently`'s chdir to the
//! worktree root with `prefix_filename()` on `-o` and the prefix stripped again
//! for display, the report path construction (prefix + `strbuf_complete` slash +
//! `git-bugreport[-<strftime suffix>].txt`), creation of the leading
//! directories, `O_CREAT|O_EXCL` file creation and its `fatal:` diagnostics, the
//! question template, the `[System Info]` / `[Enabled Hooks]` headers, the
//! `uname` and `$SHELL` lines, the enabled-hook scan (28 documented hook names,
//! `core.hooksPath` honoured, `$GIT_COMMON_DIR/hooks` otherwise) including the
//! non-repository message, the `Created new report at '<path>'.` line on stderr,
//! and the editor launch (`GIT_EDITOR`, `core.editor`, `VISUAL`, `EDITOR`, `vi`,
//! the `:` no-op, `use_shell` quoting, the canonicalized path handed to the
//! editor) with git's 0/1 exit code.
//!
//! `--diagnose[=<mode>]` writes the `git-diagnostics-<suffix>.zip` archive next
//! to the report before the report itself is written, out of the same
//! `time(NULL)`, through [`super::diagnose::create_diagnostics_archive`]; that
//! module's docs list what the archive does and does not reproduce. The option
//! carries `PARSE_OPT_OPTARG`, so a mode must be attached with `=` and a bare
//! `--diagnose bogus` leaves `bogus` as an unknown argument, as stock does.
//!
//! Three divergences that cannot be closed, rather than gaps that could be
//! filled:
//!
//!   * The build-options part of `[System Info]` (`cpu`, `sizeof-long`,
//!     `shell-path`, `rust`, `feature`, `gettext`, `libcurl`, `OpenSSL`, `zlib`,
//!     `SHA-1`, `SHA-256`, `default-ref-format`, `default-hash`, `compiler
//!     info`, `libc info`) is C-preprocessor state baked into the stock git
//!     binary at compile time. This binary is Rust on gitoxide and has no such
//!     state, so it reports what is true of *itself* — the crate version, the
//!     target architecture, `size_of::<usize>()`, and git's own "no compiler
//!     information available" / "no libc information available" fallbacks.
//!     Copying stock git's values here would put false claims in a bug report.
//!   * The `hint: Waiting for your editor to close the file...` line and the
//!     line-erase that follows it are not emitted. Both fire only when stderr is
//!     a terminal, so no piped invocation can observe the difference.
//!   * `--no-suffix --diagnose` dies of `SIGSEGV` in stock (exit 139): the zip
//!     path is built by handing `strbuf_addftime()` the `NULL` that
//!     `--no-suffix` left behind, and it dereferences it. Verified against git
//!     2.55.0. This port writes `git-diagnostics-.zip`, which is what that line
//!     of `cmd_bugreport()` spells out when the format is empty.
//!
//! One approximation: hook executability is tested with `mode & 0o111` where git
//! calls `access(X_OK)`, so a hook that is executable only for a different
//! user/group is reported as enabled here.

use super::diagnose::{self, LocalTime, Mode, Setup};
use anyhow::Result;
use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitCode};

/// `usage_with_options()` output: the synopsis realigned under `usage: ` plus
/// the option list. Printed to stdout for `-h`, to stderr for a parse error.
const USAGE_WITH_OPTIONS: &str = "\
usage: git bugreport [(-o | --output-directory) <path>]
                     [(-s | --suffix) <format> | --no-suffix]
                     [--diagnose[=<mode>]]

    --[no-]diagnose[=<mode>]
                          create an additional zip archive of detailed diagnostics (default 'stats')
    -o, --[no-]output-directory <path>
                          specify a destination for the bugreport file(s)
    -s, --[no-]suffix <format>
                          specify a strftime format suffix for the filename(s)
";

/// `usage()` output: the synopsis string verbatim, so the continuation lines
/// keep the 14-space indent they carry in git's source rather than being
/// realigned. Used for the leftover-argument error only.
const USAGE_SHORT: &str = "\
usage: git bugreport [(-o | --output-directory) <path>]
              [(-s | --suffix) <format> | --no-suffix]
              [--diagnose[=<mode>]]
";

/// `get_bug_template()` — the questions the user fills in.
const TEMPLATE: &str = "\
Thank you for filling out a Git bug report!
Please answer the following questions to help us understand your issue.

What did you do before the bug happened? (Steps to reproduce your issue)

What did you expect to happen? (Expected behavior)

What happened instead? (Actual behavior)

What's different between what you expected and what actually happened?

Anything else you want to add:

Please review the rest of the bug report below.
You can delete any lines you don't wish to share.
";

/// `hook_name_list[]` from the generated `hook-list.h`: every section heading in
/// `Documentation/githooks.adoc`, sorted with `LC_ALL=C` as the generator does.
/// The scan below emits matches in this order, so the sort must be preserved.
const HOOK_NAMES: [&str; 28] = [
    "applypatch-msg",
    "commit-msg",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "p4-prepare-changelist",
    "post-applypatch",
    "post-checkout",
    "post-commit",
    "post-index-change",
    "post-merge",
    "post-receive",
    "post-rewrite",
    "post-update",
    "pre-applypatch",
    "pre-auto-gc",
    "pre-commit",
    "pre-merge-commit",
    "pre-push",
    "pre-rebase",
    "pre-receive",
    "prepare-commit-msg",
    "proc-receive",
    "push-to-checkout",
    "reference-transaction",
    "sendemail-validate",
    "update",
];

/// The long options this command accepts, for exact and prefix matching.
const LONG_OPTS: [&str; 3] = ["diagnose", "output-directory", "suffix"];

/// git's `die()` exit status.
const EXIT_FATAL: u8 = 128;
/// git's usage/parse-error exit status (`PARSE_OPT_HELP`).
const EXIT_USAGE: u8 = 129;

/// `git bugreport` — collect information for the user to file a bug report.
///
/// Writes the report, prints its path on stderr, then hands the file to the
/// editor; the exit status is the editor's success (0) or failure (1), matching
/// `return !!launch_editor(...)`.
pub fn bugreport(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail, but tolerate the subcommand
    // being present at index 0 so both calling conventions behave the same.
    let args = match args.first() {
        Some(a) if a == "bugreport" => &args[1..],
        _ => args,
    };

    let opts = match parse_options(args)? {
        Parsed::Ok(o) => o,
        Parsed::Exit(code) => return Ok(ExitCode::from(code)),
    };

    // `setup_git_directory_gently()` leaves the process at the worktree root and
    // hands the command the subdirectory it started in; `prefix_filename()`
    // re-attaches that subdirectory to a relative `-o` value. `now` is git's
    // single `time(NULL)`, shared by the report's suffix and the archive's.
    let start = Setup::enter();
    let now = LocalTime::now();

    // Prepare the path to put the result: `<-o value>` (empty when unset), a
    // separating slash unless the buffer is empty or already ends in one, then
    // the file name.
    let mut report_path = start.prefix_filename(opts.output.as_deref().unwrap_or(""));
    if !report_path.is_empty() && !report_path.ends_with('/') {
        report_path.push('/');
    }
    // `output_path_len` — the directory part, which the archive path reuses.
    let output_path_len = report_path.len();
    report_path.push_str("git-bugreport");
    if let Some(suffix) = &opts.suffix {
        report_path.push('-');
        report_path.push_str(&now.strftime(suffix));
    }
    report_path.push_str(".txt");

    // `safe_create_leading_directories()` — a missing parent is created, an
    // existing one is fine, anything else is fatal.
    if let Some(parent) = Path::new(&report_path).parent() {
        if !parent.as_os_str().is_empty() && std::fs::create_dir_all(parent).is_err() {
            eprintln!("fatal: could not create leading directories for '{report_path}'");
            return Ok(ExitCode::from(EXIT_FATAL));
        }
    }

    // Prepare diagnostics, if requested — before the report, as git does, so its
    // stdout report and `Diagnostics complete.` come out first.
    if let Some(mode) = opts.diagnose {
        let mut zip_path = report_path[..output_path_len].to_string();
        zip_path.push_str("git-diagnostics-");
        zip_path.push_str(&now.strftime(opts.suffix.as_deref().unwrap_or("")));
        zip_path.push_str(".zip");

        if let Err(e) = diagnose::create_diagnostics_archive(&zip_path, mode, &now) {
            eprintln!("fatal: unable to create diagnostics archive {zip_path}: {e}");
            return Ok(ExitCode::from(EXIT_FATAL));
        }
    }

    // A repository is optional here: git sets this command up gently and only
    // uses the repo for the hook scan and `core.editor`.
    let repo = gix::discover(".").ok();

    let mut buffer = String::from(TEMPLATE);
    buffer.push_str("\n\n[System Info]\n");
    system_info(&mut buffer);
    buffer.push_str("\n\n[Enabled Hooks]\n");
    populated_hooks(&mut buffer, repo.as_ref())?;

    // `xopen(O_CREAT | O_EXCL | O_WRONLY)`: never clobber an existing report.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&report_path);
    let mut file = match file {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fatal: unable to create '{report_path}': {}", errno_text(&e));
            return Ok(ExitCode::from(EXIT_FATAL));
        }
    };
    if let Err(e) = file.write_all(buffer.as_bytes()) {
        eprintln!("fatal: unable to write to {report_path}: {}", errno_text(&e));
        return Ok(ExitCode::from(EXIT_FATAL));
    }
    drop(file);

    // git prints the path relative to the user — the start directory — but still
    // hands the editor the path relative to the worktree root.
    let shown = report_path
        .strip_prefix(start.prefix())
        .unwrap_or(&report_path);
    eprintln!("Created new report at '{shown}'.");

    Ok(match launch_editor(repo.as_ref(), Path::new(&report_path)) {
        true => ExitCode::SUCCESS,
        false => ExitCode::from(1),
    })
}

/// Everything the option parser produces.
struct Opts {
    /// `-o`/`--output-directory`; `None` means the current directory.
    output: Option<String>,
    /// `-s`/`--suffix`, defaulting to git's `%Y-%m-%d-%H%M`; `None` after
    /// `--no-suffix`.
    suffix: Option<String>,
    /// `--diagnose[=<mode>]`; `None` is git's `DIAGNOSE_NONE` default.
    diagnose: Option<Mode>,
}

/// Either parsed options, or an exit status git would leave with immediately.
enum Parsed {
    Ok(Opts),
    Exit(u8),
}

/// git's `parse_options()` for this command's option table, followed by the
/// leftover-argument check.
///
/// Long options may be abbreviated to any unambiguous prefix and negated with
/// `no-`, as `parse-options` allows; `--` stops option parsing.
fn parse_options(args: &[String]) -> Result<Parsed> {
    let mut opts = Opts {
        output: None,
        suffix: Some("%Y-%m-%d-%H%M".to_string()),
        diagnose: None,
    };
    let mut leftover: Vec<&str> = Vec::new();
    let mut no_more_opts = false;
    let mut i = 0;

    // Consume an attached value, or the next argument when there is none. On a
    // missing value this prints git's diagnostics and yields `None`, which the
    // caller turns into the usage exit status.
    let take_value = |i: &mut usize, attached: Option<&str>, kind: &str, opt: &str| {
        match attached {
            Some(v) => Some(v.to_string()),
            None => {
                *i += 1;
                match args.get(*i) {
                    Some(v) => Some(v.clone()),
                    None => {
                        eprint!("error: {kind} `{opt}' requires a value\n{USAGE_WITH_OPTIONS}");
                        None
                    }
                }
            }
        }
    };

    while i < args.len() {
        let a = args[i].as_str();

        if no_more_opts || a == "-" || !a.starts_with('-') {
            leftover.push(a);
            i += 1;
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            i += 1;
            continue;
        }

        if let Some(long) = a.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            // `--no-<name>` only after `<name>` itself failed to match, which is
            // the order parse-options uses.
            let (name, negated) = match resolve_long(name) {
                Some(resolved) => (resolved, false),
                None => match name.strip_prefix("no-").and_then(resolve_long) {
                    Some(resolved) => (resolved, true),
                    None => {
                        eprint!("error: unknown option `{long}'\n{USAGE_WITH_OPTIONS}");
                        return Ok(Parsed::Exit(EXIT_USAGE));
                    }
                },
            };
            match (name, negated) {
                ("diagnose", true) => opts.diagnose = None,
                // `PARSE_OPT_OPTARG`: a bare `--diagnose` means `stats`, and a
                // mode only counts when it is attached with `=`.
                ("diagnose", false) => {
                    opts.diagnose = match attached {
                        None => Some(Mode::Stats),
                        Some(v) => match diagnose::parse_mode(v) {
                            Some(m) => Some(m),
                            None => {
                                eprintln!("error: invalid --diagnose value '{v}'");
                                return Ok(Parsed::Exit(EXIT_USAGE));
                            }
                        },
                    }
                }
                ("output-directory", true) => opts.output = None,
                ("output-directory", false) => {
                    let Some(v) = take_value(&mut i, attached, "option", name) else {
                        return Ok(Parsed::Exit(EXIT_USAGE));
                    };
                    opts.output = Some(v);
                }
                ("suffix", true) => opts.suffix = None,
                ("suffix", false) => {
                    let Some(v) = take_value(&mut i, attached, "option", name) else {
                        return Ok(Parsed::Exit(EXIT_USAGE));
                    };
                    opts.suffix = Some(v);
                }
                _ => unreachable!("resolve_long only yields LONG_OPTS entries"),
            }
            i += 1;
            continue;
        }

        // Short options: `-h`, and `-o`/`-s` with an attached or separate value.
        // `a` starts with `-` and is longer than that, so a first char exists.
        let short = &a[1..];
        let c = short.chars().next().expect("non-empty after the dash");
        let attached = match &short[c.len_utf8()..] {
            "" => None,
            rest => Some(rest),
        };
        match c {
            'h' => {
                print!("{USAGE_WITH_OPTIONS}");
                return Ok(Parsed::Exit(EXIT_USAGE));
            }
            'o' | 's' => {
                let Some(v) = take_value(&mut i, attached, "switch", &c.to_string()) else {
                    return Ok(Parsed::Exit(EXIT_USAGE));
                };
                if c == 'o' {
                    opts.output = Some(v);
                } else {
                    opts.suffix = Some(v);
                }
            }
            _ => {
                eprint!("error: unknown switch `{c}'\n{USAGE_WITH_OPTIONS}");
                return Ok(Parsed::Exit(EXIT_USAGE));
            }
        }
        i += 1;
    }

    if let Some(first) = leftover.first() {
        eprint!("error: unknown argument `{first}'\n{USAGE_SHORT}");
        return Ok(Parsed::Exit(EXIT_USAGE));
    }
    Ok(Parsed::Ok(opts))
}

/// Map a long-option spelling onto its canonical name, accepting any prefix that
/// matches exactly one option. An ambiguous prefix resolves to nothing, so the
/// caller reports it as unknown.
fn resolve_long(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return None;
    }
    if let Some(exact) = LONG_OPTS.iter().copied().find(|o| *o == name) {
        return Some(exact);
    }
    let mut matches = LONG_OPTS.iter().copied().filter(|o| o.starts_with(name));
    match (matches.next(), matches.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// `get_system_info()` — the version block, `uname`, compiler/libc lines and
/// `$SHELL`.
///
/// The build-options fields git bakes in at compile time are replaced by the
/// facts that hold for this binary; see the module docs.
fn system_info(out: &mut String) {
    out.push_str("git version:\n");
    // The same block `git diagnose` puts at the head of its report, so the two
    // can never disagree about what this binary is.
    diagnose::version_info(out);

    out.push_str("uname: ");
    out.push_str(&uname_info());

    // git's fallbacks when no compiler/libc macros are defined, which is the
    // situation a Rust binary is permanently in.
    out.push_str("compiler info: no compiler information available\n");
    out.push_str("libc info: no libc information available\n");

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "<unset>".to_string());
    out.push_str(&format!("$SHELL (typically, interactive shell): {shell}\n"));
}

/// `get_uname_info(buf, 1)` — `sysname release version machine`, which is
/// exactly what `uname -srvm` prints.
///
/// git calls `uname(2)` directly and reports `strerror`/`errno` on failure;
/// running the tool instead leaves no errno to report, so the failure line says
/// so in its own words rather than imitating git's.
fn uname_info() -> String {
    match Command::new("uname").arg("-srvm").output() {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            format!("{}\n", text.strip_suffix('\n').unwrap_or(&text))
        }
        _ => "unavailable (could not run `uname`)\n".to_string(),
    }
}

/// `get_populated_hooks()` — every documented hook name that resolves to an
/// executable file, in `hook_name_list` order.
fn populated_hooks(out: &mut String, repo: Option<&gix::Repository>) -> Result<()> {
    let Some(repo) = repo else {
        out.push_str("not run from a git repository - no hooks to show\n");
        return Ok(());
    };

    // `core.hooksPath` wins over the per-repository directory, and the latter
    // lives in the common dir so linked worktrees share it.
    let hooks_dir = match repo.config_snapshot().trusted_path("core.hooksPath")? {
        Some(path) => path,
        None => repo.common_dir().join("hooks"),
    };

    for name in HOOK_NAMES {
        if is_executable(&hooks_dir.join(name)) {
            out.push_str(name);
            out.push('\n');
        }
    }
    Ok(())
}

/// git's `access(path, X_OK)`, approximated by the file mode; see the module
/// docs for the case this misses.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

/// `launch_editor()` — returns whether it succeeded, which is what git turns
/// into its 0/1 exit status.
fn launch_editor(repo: Option<&gix::Repository>, path: &Path) -> bool {
    let Some(editor) = git_editor(repo) else {
        eprintln!("error: Terminal is dumb, but EDITOR unset");
        return false;
    };
    if editor == ":" {
        return true;
    }

    // git hands the editor the real path, so a relative report path still opens
    // correctly after the editor changes directory.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    let status = editor_command(&editor, &real).status();
    match status {
        Err(_) => {
            eprintln!("error: unable to start editor '{editor}'");
            false
        }
        Ok(s) if !s.success() => {
            eprintln!("error: there was a problem with the editor '{editor}'");
            false
        }
        Ok(_) => true,
    }
}

/// `git_editor()`: `GIT_EDITOR`, then `core.editor`, then `VISUAL` (skipped on a
/// dumb terminal), then `EDITOR`, then git's built-in `vi` default. A dumb
/// terminal with none of them set yields `None`.
///
/// `core.editor` is only consulted when a repository was found — without one
/// there is no configuration stack to read it from here.
pub(crate) fn git_editor(repo: Option<&gix::Repository>) -> Option<String> {
    let dumb = match std::env::var("TERM") {
        Ok(t) => t == "dumb",
        Err(_) => true,
    };

    let mut editor = std::env::var("GIT_EDITOR").ok();
    if editor.is_none() {
        editor = repo
            .and_then(|r| r.config_snapshot().trusted_program("core.editor"))
            .map(|p| p.to_string_lossy().into_owned());
    }
    if editor.is_none() && !dumb {
        editor = std::env::var("VISUAL").ok();
    }
    if editor.is_none() {
        editor = std::env::var("EDITOR").ok();
    }
    if editor.is_none() && dumb {
        return None;
    }
    Some(editor.unwrap_or_else(|| "vi".to_string()))
}

/// `prepare_shell_cmd()` for a `use_shell` child: an editor string containing
/// anything the shell would interpret runs as `sh -c '<editor> "$@"' <editor>
/// <path>`; a bare program name is executed directly.
pub(crate) fn editor_command(editor: &str, path: &Path) -> Command {
    const META: &[char] = &[
        '|', '&', ';', '<', '>', '(', ')', '$', '`', '\\', '"', '\'', ' ', '\t', '\n', '*', '?',
        '[', '#', '~', '=', '%',
    ];
    if editor.contains(META) {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(format!("{editor} \"$@\""))
            .arg(editor)
            .arg(path);
        cmd
    } else {
        let mut cmd = Command::new(editor);
        cmd.arg(path);
        cmd
    }
}

/// The `strerror()` text `die_errno()` appends, for the errno values this
/// command can realistically hit; anything else falls back to Rust's rendering.
fn errno_text(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::AlreadyExists => "File exists".to_string(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        _ => e.to_string(),
    }
}
