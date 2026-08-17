//! git-compatible `alias.<cmd>` expansion — a faithful port of git.c's
//! `handle_alias` (the alias half of `run_argv`) and alias.c's `split_cmdline`.
//!
//! Resolution runs before paging and dispatch. A verb that dispatch already
//! serves wins over a same-named alias, exactly as git's builtins do; otherwise
//! the configured `alias.<cmd>` string is expanded:
//!   * a leading `!` marks a shell alias, run as a `use_shell` child — the
//!     user's extra arguments arrive as `"$@"` — and its exit code is returned
//!     directly;
//!   * anything else is word-split ([`split_cmdline`]) and spliced in place of
//!     the alias token, then re-resolved so aliases can chain — with git's
//!     self-reference and loop guards.

use crate::dispatch;
use std::process::ExitCode;

/// The result of resolving the leading verb through the alias table.
pub enum Outcome {
    /// A real verb (`head`) and its arguments, ready to dispatch. Also returned
    /// for an unknown verb with no matching alias, so dispatch reports it.
    Command(String, Vec<String>),
    /// A `!`-prefixed shell alias, already run; carries its exit code.
    Shell(ExitCode),
    /// A malformed alias (bad quoting, empty, recursive, or looping); carries
    /// git's diagnostic, to print as `zvcs: <msg>`.
    Fatal(String),
    /// A `handle_options()` failure the expansion provoked — a `-C` that cannot
    /// chdir, a `-C` with no value. git reports these itself and exits before the
    /// command runs, so the diagnostic is already on stderr and only the code is
    /// left to carry.
    Exit(ExitCode),
}

/// Expand `sub` (with trailing `rest`) through `alias.<cmd>`, updating
/// `pager_forced` for any pager flag an alias expansion introduces (`-p`/`-P`).
///
/// `pager_forced` mirrors git's `handle_options` running inside the `run_argv`
/// loop: an alias like `-p log` must still toggle the pager for the resolved
/// command.
pub fn resolve(sub: &str, rest: &[String], pager_forced: &mut Option<bool>) -> Outcome {
    let mut args: Vec<String> = Vec::with_capacity(1 + rest.len());
    args.push(sub.to_string());
    args.extend_from_slice(rest);

    // Alias names already expanded, to detect loops (git's `expanded_aliases`).
    let mut seen: Vec<String> = Vec::new();

    loop {
        // Consume any leading global options an expansion introduced, matching
        // git handling `handle_options` each turn of `run_argv`.
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-p" | "--paginate" => {
                    *pager_forced = Some(true);
                    i += 1;
                }
                "-P" | "--no-pager" => {
                    *pager_forced = Some(false);
                    i += 1;
                }
                "-C" => {
                    let Some(dir) = args.get(i + 1) else {
                        eprintln!("no directory given for '-C' option");
                        eprintln!("usage: {}", crate::porcelain::help::GIT_USAGE_STRING);
                        return Outcome::Exit(ExitCode::from(129));
                    };
                    if let Err(msg) = crate::chdir_global(dir) {
                        eprintln!("fatal: {msg}");
                        return Outcome::Exit(ExitCode::from(crate::fatal::EXIT_FATAL));
                    }
                    i += 2;
                }
                _ => break,
            }
        }
        if i > 0 {
            args.drain(0..i);
        }

        let Some(head) = args.first().cloned() else {
            return Outcome::Fatal("empty alias".into());
        };

        // A verb dispatch serves wins over an alias of the same name (git's
        // builtins-first ordering). Stop expanding and hand it off.
        if dispatch::is_verb(&head) {
            return Outcome::Command(head, args[1..].to_vec());
        }

        let Some(value) = lookup(&head) else {
            // Not a verb and not an alias: let dispatch produce its own error
            // (git's "is not a git command" message).
            return Outcome::Command(head, args[1..].to_vec());
        };

        // `git <alias> -h` reports the aliasing, then still runs the expansion.
        if args.len() == 2 && args[1] == "-h" {
            eprintln!("'{head}' is aliased to '{value}'");
        }

        if let Some(shell) = value.strip_prefix('!') {
            return Outcome::Shell(run_shell_alias(shell, &args[1..]));
        }

        let expansion = match split_cmdline(&value) {
            Ok(v) => v,
            Err(e) => return Outcome::Fatal(format!("bad alias.{head} string: {e}")),
        };
        if expansion.is_empty() || expansion[0].is_empty() {
            return Outcome::Fatal(format!("empty alias for {head}"));
        }
        if expansion[0] == head {
            return Outcome::Fatal(format!("recursive alias: {head}"));
        }
        if seen.iter().any(|s| s == &expansion[0]) {
            return Outcome::Fatal(format!(
                "alias loop detected: expansion of '{}' does not terminate",
                seen[0]
            ));
        }
        seen.push(head);

        // Replace the alias token (args[0]) with its expansion, keeping the
        // user's trailing arguments (git's `strvec_splice(args, 0, 1, ...)`).
        let tail: Vec<String> = args[1..].to_vec();
        args.clear();
        args.extend(expansion);
        args.extend(tail);
    }
}

/// Port of `alias_lookup`/`config_alias_cb` (alias.c): find the alias body for
/// `name` in the repository's resolved config (all scopes), or `None` when unset
/// or outside a repository.
///
/// git accepts two spellings and this scans for both, last match winning as its
/// config callback does:
///
/// * `[alias] co = checkout` — the name is everything after `alias.`, matched
///   **case-insensitively**, and may itself contain dots (`alias.foo.bar` names
///   the alias `foo.bar`).
/// * `[alias "co"] command = checkout` — the name is the subsection, matched
///   **case-sensitively as raw bytes**, so it can hold spaces and non-ASCII.
///
/// An empty subsection (`[alias ""]`) counts as no subsection, and any *other*
/// key inside a subsection falls back to the first form, which is what keeps
/// `alias.foo.bar` working after the parser has split it.
fn lookup(name: &str) -> Option<String> {
    use gix::bstr::ByteSlice;

    let repo = gix::discover(".").ok()?;
    let snapshot = repo.config_snapshot();
    let sections = snapshot.plumbing().sections_by_name("alias")?;

    let mut found: Option<String> = None;
    for section in sections {
        let subsection = section
            .header()
            .subsection_name()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_vec());
        for key in section.value_names() {
            let matches = match &subsection {
                // `[alias "<name>"] command = …`: raw-byte comparison.
                Some(sub) if key.eq_ignore_ascii_case("command") => sub == name.as_bytes(),
                // Any other key under a subsection is re-read as the flat form
                // `alias.<subsection>.<key>`.
                Some(sub) => {
                    let flat = format!("{}.{key}", sub.to_str_lossy());
                    flat.eq_ignore_ascii_case(name)
                }
                None => key.eq_ignore_ascii_case(name),
            };
            if matches {
                if let Some(v) = section.value(&key) {
                    found = Some(v.to_string());
                }
            }
        }
    }
    found
}

/// Run a `!`-prefixed shell alias, git's `handle_alias` shell path: the body is
/// pushed as the whole argv of a `use_shell` child, so `prepare_shell_cmd`
/// binds the user's remaining arguments to `"$@"` with `$0` set to the body —
/// or, for a body that is a bare program name, execs it directly. Returns the
/// child's exit code, or a failure code if it could not be spawned.
fn run_shell_alias(body: &str, user_args: &[String]) -> ExitCode {
    let mut cmd = crate::external::prepare_shell_cmd_str(body, user_args);
    match cmd.status() {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("zvcs: while expanding alias: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Port of git's `split_cmdline` (alias.c): shell-like word splitting with
/// single/double quotes and backslash escapes. Backslash escapes the next byte
/// everywhere except inside single quotes; whitespace outside quotes separates
/// words. Errors on a trailing backslash or an unclosed quote, as git does.
fn split_cmdline(s: &str) -> Result<Vec<String>, SplitError> {
    let bytes = s.as_bytes();
    let mut tokens: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut quoted: u8 = 0; // 0, b'\'', or b'"'
    let mut src = 0;

    while src < bytes.len() {
        let c = bytes[src];
        if quoted == 0 && c.is_ascii_whitespace() {
            tokens.push(std::mem::take(&mut cur));
            src += 1;
            while src < bytes.len() && bytes[src].is_ascii_whitespace() {
                src += 1;
            }
        } else if quoted == 0 && (c == b'\'' || c == b'"') {
            quoted = c;
            src += 1;
        } else if c == quoted {
            quoted = 0;
            src += 1;
        } else {
            let mut ch = c;
            if c == b'\\' && quoted != b'\'' {
                src += 1;
                if src >= bytes.len() {
                    return Err(SplitError::BadEnding);
                }
                ch = bytes[src];
            }
            cur.push(ch);
            src += 1;
        }
    }
    if quoted != 0 {
        return Err(SplitError::UnclosedQuote);
    }
    tokens.push(cur);

    Ok(tokens
        .into_iter()
        .map(|t| String::from_utf8_lossy(&t).into_owned())
        .collect())
}

/// `split_cmdline` failures, rendered with git's `split_cmdline_strerror` text.
#[derive(Debug)]
enum SplitError {
    BadEnding,
    UnclosedQuote,
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SplitError::BadEnding => write!(f, "cmdline ends with \\"),
            SplitError::UnclosedQuote => write!(f, "unclosed quote"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{split_cmdline, SplitError};

    #[test]
    fn splits_plain_words() {
        assert_eq!(split_cmdline("log -1 HEAD").unwrap(), ["log", "-1", "HEAD"]);
    }

    #[test]
    fn collapses_runs_of_whitespace() {
        assert_eq!(split_cmdline("log   \t -1").unwrap(), ["log", "-1"]);
    }

    #[test]
    fn double_quotes_group_and_keep_spaces() {
        assert_eq!(
            split_cmdline(r#"commit -m "a b c""#).unwrap(),
            ["commit", "-m", "a b c"]
        );
    }

    #[test]
    fn single_quotes_are_literal() {
        assert_eq!(split_cmdline(r#"echo '\n'"#).unwrap(), ["echo", "\\n"]);
    }

    #[test]
    fn backslash_escapes_outside_single_quotes() {
        assert_eq!(split_cmdline(r#"a\ b"#).unwrap(), ["a b"]);
        assert_eq!(split_cmdline(r#""x\"y""#).unwrap(), [r#"x"y"#]);
    }

    #[test]
    fn rejects_trailing_backslash() {
        assert!(matches!(
            split_cmdline("foo\\"),
            Err(SplitError::BadEnding)
        ));
    }

    #[test]
    fn rejects_unclosed_quote() {
        assert!(matches!(
            split_cmdline("foo \"bar"),
            Err(SplitError::UnclosedQuote)
        ));
    }
}
