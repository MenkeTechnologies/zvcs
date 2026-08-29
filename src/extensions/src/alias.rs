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
    /// A malformed alias (bad quoting, empty, recursive, or looping). Each is a
    /// `die()` in `handle_alias`, so the caller prints it as `fatal: <msg>` and
    /// leaves with 128 — verified one message at a time against stock 2.55.0.
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
///
/// `expanded` is `run_argv`'s `done_alias` return: set when at least one
/// expansion happened, because `cmd_main` reports an unresolvable verb that came
/// out of an alias differently from one the user typed ([`crate::run`]).
///
/// `overrides` is the caller's command-line configuration list, which an
/// expansion's own `-c`/`--config-env` appends to. It is shared rather than
/// local because git's overrides are process-global from the moment they are
/// pushed: the command the chain finally resolves to sees the ones its own
/// aliases added, and the caller checks the whole list against that command.
pub fn resolve(
    sub: &str,
    rest: &[String],
    pager_forced: &mut Option<bool>,
    expanded: &mut bool,
    overrides: &mut Vec<crate::ConfigOverride>,
) -> Outcome {
    let mut args: Vec<String> = Vec::with_capacity(1 + rest.len());
    args.push(sub.to_string());
    args.extend_from_slice(rest);

    // Every command name `run_argv` has looked at this pass, in order — git's
    // `cmd_list`. It is both the loop guard and the trace the loop diagnostic
    // prints, so it has to keep the names *and* their order, not just a set.
    let mut cmd_list: Vec<String> = Vec::new();

    loop {
        let Some(head) = args.first().cloned() else {
            return Outcome::Fatal("empty alias".into());
        };

        // A verb dispatch serves wins over an alias of the same name (git's
        // builtins-first ordering). Stop expanding and hand it off.
        if dispatch::is_verb(&head) {
            return Outcome::Command(head, args[1..].to_vec());
        }

        // `run_argv`'s loop guard, between the builtin lookup and `handle_alias`:
        // a name seen earlier this pass means the expansion cannot terminate.
        if let Some(pos) = cmd_list.iter().position(|s| s == &head) {
            return Outcome::Fatal(loop_detected(&cmd_list, pos));
        }
        cmd_list.push(head.clone());

        // `alias_lookup()` reads the configuration, and reading it is what makes
        // git report a malformed `-c` key. So a bad key an earlier turn of this
        // loop pushed surfaces *here*, at the next lookup — before the empty /
        // recursive / loop guards of the turn that would follow, which is why
        // `alias.x = "-c foo x"` reports the recursion rather than the key.
        if let Some(code) = crate::report_bad_config_overrides(overrides) {
            return Outcome::Exit(code);
        }

        let Some(value) = lookup(&head) else {
            // Not a verb and not an alias: let dispatch produce its own error
            // (git's "is not a git command" message).
            return Outcome::Command(head, args[1..].to_vec());
        };

        // `git <alias> -h` reports the aliasing, then still runs the expansion.
        // The C's guard is `args->nr == 2 && !strcmp(args->v[1], "-h")`: the
        // notice is for the *bare* `git <alias> -h` only, so `git <alias> -h x`
        // — where the `-h` is an argument to the expanded command rather than a
        // request to describe the alias — stays silent.
        if args.len() == 2 && args[1] == "-h" {
            eprintln!("'{head}' is aliased to '{value}'");
        }

        if let Some(shell) = value.strip_prefix('!') {
            return Outcome::Shell(run_shell_alias(shell, &args[1..]));
        }

        let mut expansion = match split_cmdline(&value) {
            Ok(v) => v,
            Err(e) => return Outcome::Fatal(format!("bad alias.{head} string: {e}")),
        };

        // `handle_options()` runs over the **expansion alone**, before the user's
        // trailing arguments are spliced in, and with a non-null `envchanged`.
        // Every global that reaches a child process through the environment sets
        // it, and `handle_alias` then refuses the alias outright rather than
        // letting it leak a setting into the rest of the process — the one thing
        // an alias may not do. `-p`/`--paginate`, `-c` and `--config-env` do not
        // set it, so an alias may still choose paging or push configuration.
        let mut envchanged = false;
        let consumed =
            match crate::handle_options(&expansion, pager_forced, overrides, &mut envchanged) {
                crate::Handled::Consumed(n) => n,
                crate::Handled::Exit(code) => return Outcome::Exit(code),
            };
        if envchanged {
            eprintln!(
                "fatal: alias '{head}' changes environment variables.\n\
                 You can use '!git' in the alias to do this"
            );
            return Outcome::Exit(ExitCode::from(crate::fatal::EXIT_FATAL));
        }
        expansion.drain(0..consumed);

        // `if (count < 1) die(_("empty alias for %s"), …)`. The test is on the
        // token *count*, not on the token: `split_cmdline("")` yields one empty
        // token, so `alias.x = ""` expands to `""` and fails later as a command
        // that does not exist, which is what stock does.
        if expansion.is_empty() {
            return Outcome::Fatal(format!("empty alias for {head}"));
        }
        if expansion[0] == head {
            return Outcome::Fatal(format!("recursive alias: {head}"));
        }

        // Replace the alias token (args[0]) with its expansion, keeping the
        // user's trailing arguments (git's `strvec_splice(args, 0, 1, ...)`).
        let tail: Vec<String> = args[1..].to_vec();
        args.clear();
        args.extend(expansion);
        args.extend(tail);
        *expanded = true;
    }
}

/// `run_argv`'s loop diagnostic, built from the command names seen this pass and
/// the index of the one that repeated.
///
/// The C names the *first* command of the chain in the sentence and then walks
/// the whole list, marking the repeated entry `<==` and the last entry `==>` —
/// and only marks the last when it is not itself the repeated one, since the
/// two markers are an if/else over the same item.
fn loop_detected(cmd_list: &[String], repeated: usize) -> String {
    let mut trace = String::new();
    for (i, name) in cmd_list.iter().enumerate() {
        trace.push_str(&format!("\n  {name}"));
        if i == repeated {
            trace.push_str(" <==");
        } else if i == cmd_list.len() - 1 {
            trace.push_str(" ==>");
        }
    }
    format!(
        "alias loop detected: expansion of '{}' does not terminate:{trace}",
        cmd_list[0]
    )
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

    let repo = crate::setup::discover().ok()?;
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
pub(crate) fn split_cmdline(s: &str) -> Result<Vec<String>, SplitError> {
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
pub(crate) enum SplitError {
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
