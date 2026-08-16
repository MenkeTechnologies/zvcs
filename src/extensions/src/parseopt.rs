//! git's two-stage command line for the revision-walking commands.
//!
//! Every command that ends up in `setup_revisions()` reads its argv twice, and the
//! two passes have different rules. Getting the split wrong is invisible on a
//! well-formed command line and decides *which* of two competing errors the user
//! sees on a malformed one, so the split is the behaviour.
//!
//! ### Stage 1 — `parse_options()`
//!
//! `parse_options()` sweeps the *whole* command line against the command's own
//! option table before `setup_revisions()` has looked at a single argument. An
//! option the command owns therefore reports its error ahead of every rev-list
//! option, diff option, revision and pathspec, wherever the two sit relative to
//! each other:
//!
//! ```text
//! $ git log --max-count=0x10 --decorate=bogus main
//! fatal: invalid --decorate option: bogus
//! ```
//!
//! `--decorate` is in `builtin_log_options`; `--max-count` is not, so it is not
//! parsed at all until stage 2. The three flags in [`Flags`] are what differ
//! between commands, and each one is a visible behaviour change:
//!
//! * `PARSE_OPT_KEEP_UNKNOWN_OPT` — an option outside the table is copied through
//!   for stage 2 instead of being rejected here. Without it the sweep stops dead
//!   at the first unknown option ([`Sweep::Unknown`]) and the command's own driver
//!   decides what that means: a usage error for most, `parse_revision_opt()` for
//!   `shortlog`.
//! * `PARSE_OPT_KEEP_DASHDASH` — the `--` itself survives into stage 2, where
//!   `setup_revisions()` reads it as the pathspec separator. Without it stage 1
//!   swallows the separator and stage 2 never learns the tail was quoted.
//! * `PARSE_OPT_STOP_AT_NON_OPTION` — the sweep ends at the first positional
//!   rather than collecting it, so everything from there on belongs to whatever
//!   the command does with its operands. `git bundle create` uses this to take a
//!   filename and hand the rest to `setup_revisions()` untouched.
//!
//! `PARSE_OPT_KEEP_ARGV0` only decides whether argv[0] is copied to the output;
//! this port never passes the command name in `args`, so it needs no flag.
//!
//! ### Stage 2 — `setup_revisions()`
//!
//! What survives stage 1 is walked once, left to right, with no precedence table
//! at all: whichever bad argument comes first wins. Two rules from
//! `revision.c:3079-3095` shape it, both implemented here:
//!
//! * A `--` found in *this* stream truncates it, and everything behind the
//!   separator becomes a pathspec without being inspected ([`take_dashdash`]).
//! * The first argument that fails to resolve as a revision ends revision *and*
//!   option parsing: `setup_revisions()` runs `verify_filename()` over the whole
//!   remaining tail and pushes it into `prune_data` ([`pathspec_tail`]). This is
//!   why an option written behind a pathspec is not an option at all —
//!
//!   ```text
//!   $ git log main README.md --max-count=1
//!   fatal: option '--max-count=1' must come before non-option arguments
//!   ```
//!
//!   — and why the same line is silent under `shortlog`, whose stage 1 hoisted
//!   `--max-count=1` out of argv before `setup_revisions()` could see it.
//!
//! Every expectation quoted above was read off stock git 2.55.0 before being
//! written down; the precedence tests in `tests/rev_option_precedence.rs` pin them.

use std::process::ExitCode;

/// git's `usage_with_options()` exit code.
pub const USAGE_ERROR: u8 = 129;

/// git's `die()` exit code.
pub const FATAL: u8 = 128;

/// The `enum parse_opt_flags` a command passes to `parse_options()`, reduced to
/// the three bits that change what stage 1 does with an argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    /// `PARSE_OPT_KEEP_UNKNOWN_OPT`: copy an option outside the table through to
    /// stage 2 rather than stopping on it.
    pub keep_unknown_opt: bool,
    /// `PARSE_OPT_KEEP_DASHDASH`: leave the `--` in the output, for
    /// `setup_revisions()` to find.
    pub keep_dashdash: bool,
    /// `PARSE_OPT_STOP_AT_NON_OPTION`: end the sweep at the first positional.
    pub stop_at_non_option: bool,
}

impl Flags {
    /// `PARSE_OPT_KEEP_ARGV0 | PARSE_OPT_KEEP_UNKNOWN_OPT | PARSE_OPT_KEEP_DASHDASH`
    /// — `cmd_log_init_finish()` (`log`, `show`, `whatchanged`) and
    /// `cmd_format_patch()`, `builtin/log.c:261-264` and `:1998-2001`.
    pub const LOG: Flags = Flags {
        keep_unknown_opt: true,
        keep_dashdash: true,
        stop_at_non_option: false,
    };

    /// `PARSE_OPT_KEEP_ARGV0 | PARSE_OPT_KEEP_UNKNOWN_OPT` — `cmd_fast_export()`,
    /// `builtin/fast-export.c:1223`. The missing `KEEP_DASHDASH` is why a `--`
    /// ends fast-export's own parsing *and* is invisible to `setup_revisions()`.
    pub const FAST_EXPORT: Flags = Flags {
        keep_unknown_opt: true,
        keep_dashdash: false,
        stop_at_non_option: false,
    };

    /// `PARSE_OPT_KEEP_DASHDASH | PARSE_OPT_KEEP_ARGV0` — `cmd_shortlog()`,
    /// `builtin/shortlog.c:405-406`. No `KEEP_UNKNOWN_OPT`: shortlog drives
    /// `parse_options_step()` itself and answers every `PARSE_OPT_UNKNOWN` with
    /// `parse_revision_opt()`, which is what hoists rev-list options out of argv
    /// order for this one command.
    pub const SHORTLOG: Flags = Flags {
        keep_unknown_opt: false,
        keep_dashdash: true,
        stop_at_non_option: false,
    };

    /// `PARSE_OPT_STOP_AT_NON_OPTION` — `parse_options_cmd_bundle()`,
    /// `builtin/bundle.c:58-59`. The first positional is the bundle file, and
    /// everything after it is `create_bundle()`'s to hand to `setup_revisions()`.
    pub const BUNDLE: Flags = Flags {
        keep_unknown_opt: false,
        keep_dashdash: false,
        stop_at_non_option: true,
    };
}

/// What a command's own option table did with one argument, mirroring the return
/// of `parse_long_opt()` / `parse_short_opt()`.
pub enum Step {
    /// The table owns this argument and consumed `n` argv slots — 1 for a flag or
    /// a `--opt=value`, 2 for an option that takes the next token as its value.
    Took(usize),
    /// `PARSE_OPT_UNKNOWN`: not in this table.
    Unknown,
    /// A callback rejected the value. `parse_options_step()` returns
    /// `PARSE_OPT_ERROR` and the whole command exits with this status, so nothing
    /// later on the line is ever looked at.
    Fail(ExitCode),
}

/// The result of stage 1.
pub enum Sweep {
    /// The arguments the table did not consume, in order, for stage 2.
    Kept(Vec<String>),
    /// `PARSE_OPT_UNKNOWN` reached the driver because `KEEP_UNKNOWN_OPT` is unset.
    /// The named argument is where the sweep stopped; what happens next is the
    /// command's own decision, not `parse_options()`'.
    Unknown(String),
    /// A callback in the table rejected its value.
    Failed(ExitCode),
}

/// git's `parse_options()` sweep: one left-to-right pass of `args` against the
/// command's own table, returning everything it did not consume.
///
/// `own` is the table. It is handed the current argument, the whole argument list
/// and the index it sits at, so an option that takes its value as the next token
/// can read it and report [`Step::Took(2)`](Step::Took).
///
/// Positionals are collected in order and kept for stage 2 (unless
/// `stop_at_non_option`, which ends the sweep at the first one). A lone `-` is a
/// positional, not an option — `parse_options_step()` tests `*arg != '-' ||
/// !arg[1]`.
pub fn sweep(
    args: &[String],
    flags: Flags,
    mut own: impl FnMut(&str, &[String], usize) -> Step,
) -> Sweep {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();

        // `if (*arg != '-' || !arg[1])`: a positional or a lone `-`.
        if !a.starts_with('-') || a == "-" {
            if flags.stop_at_non_option {
                out.extend_from_slice(&args[i..]);
                return Sweep::Kept(out);
            }
            out.push(args[i].clone());
            i += 1;
            continue;
        }

        // `--` and `--end-of-options` both break the loop; the token itself is
        // dropped unless `PARSE_OPT_KEEP_DASHDASH`, while the tail behind it is
        // always copied through by `parse_options_end()`'s `MOVE_ARRAY`.
        if a == "--" || a == "--end-of-options" {
            let from = if flags.keep_dashdash { i } else { i + 1 };
            out.extend_from_slice(&args[from..]);
            return Sweep::Kept(out);
        }

        match own(a, args, i) {
            Step::Took(n) => i += n.max(1),
            Step::Fail(code) => return Sweep::Failed(code),
            Step::Unknown => {
                if !flags.keep_unknown_opt {
                    return Sweep::Unknown(args[i].clone());
                }
                out.push(args[i].clone());
                i += 1;
            }
        }
    }
    Sweep::Kept(out)
}

/// `setup_revisions()`'s "First, search for `--`" pass (`revision.c:2831-2848`).
///
/// The separator truncates the argument list and everything behind it becomes a
/// pathspec *without being inspected at all* — no `verify_filename()`, so a
/// missing path or a token starting with `-` is accepted there. The returned flag
/// is git's `seen_dashdash`, which also declares every argument in front of the
/// separator a revision: one that fails to resolve dies rather than falling back
/// to a path.
pub fn take_dashdash(rest: &mut Vec<String>) -> (Vec<String>, bool) {
    let Some(p) = rest.iter().position(|t| t == "--") else {
        return (Vec::new(), false);
    };
    let paths = rest.split_off(p + 1);
    rest.pop(); // the separator itself
    (paths, true)
}

/// `setup_revisions()`'s pathspec break (`revision.c:3079-3095`).
///
/// Called with the tail that starts at the first argument which failed to resolve
/// as a revision. git runs `verify_filename()` over every element of it and then
/// pushes the whole tail into `prune_data`, so revision *and* option parsing stop
/// here — an option written behind a pathspec is a path beginning with `-`, and
/// is reported as one.
///
/// `diagnose_misspelt_rev` is set only for the first element, the one whose
/// failure is still ambiguous between a misspelt revision and a missing path.
/// Returns the message git would `die()` with.
pub fn pathspec_tail(tail: &[String]) -> Result<Vec<String>, String> {
    for (n, t) in tail.iter().enumerate() {
        if let Some(msg) = crate::setup::verify_filename(t, n == 0) {
            return Err(msg);
        }
    }
    Ok(tail.to_vec())
}

/// [`pathspec_tail`], reported the way every caller reports it: `fatal:` on
/// stderr and git's `die()` exit code.
pub fn pathspec_tail_or_die(tail: &[String]) -> Result<Vec<String>, ExitCode> {
    pathspec_tail(tail).map_err(|msg| {
        eprintln!("fatal: {msg}");
        ExitCode::from(FATAL)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table that owns `--own` (flag) and `--val` (next-token value), rejects
    /// `--own=bad`, and knows nothing else.
    fn table(a: &str, args: &[String], i: usize) -> Step {
        match a {
            "--own" => Step::Took(1),
            "--val" => {
                if args.get(i + 1).is_none() {
                    return Step::Fail(ExitCode::from(USAGE_ERROR));
                }
                Step::Took(2)
            }
            _ if a.starts_with("--own=") => {
                if a == "--own=bad" {
                    Step::Fail(ExitCode::from(USAGE_ERROR))
                } else {
                    Step::Took(1)
                }
            }
            _ => Step::Unknown,
        }
    }

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    fn kept(args: &[&str], flags: Flags) -> Vec<String> {
        match sweep(&v(args), flags, table) {
            Sweep::Kept(out) => out,
            Sweep::Unknown(a) => panic!("unexpected unknown `{a}`"),
            Sweep::Failed(_) => panic!("unexpected failure"),
        }
    }

    /// The whole point of stage 1: an option the table owns is consumed wherever
    /// it sits, and everything else survives in its original relative order.
    #[test]
    fn the_table_is_swept_over_the_whole_line() {
        assert_eq!(
            kept(&["--max-count=1", "--own", "main", "--val", "x", "--other"], Flags::LOG),
            v(&["--max-count=1", "main", "--other"])
        );
    }

    /// A rejection in the table stops the sweep, so nothing after it is examined —
    /// this is what puts `--decorate=bogus` ahead of an earlier `--max-count=0x10`.
    #[test]
    fn a_rejected_value_ends_the_sweep_immediately() {
        assert!(matches!(
            sweep(&v(&["--max-count=0x10", "--own=bad", "main"]), Flags::LOG, table),
            Sweep::Failed(_)
        ));
    }

    /// Without `KEEP_UNKNOWN_OPT` the first unknown option reaches the driver
    /// rather than being copied through — shortlog's and bundle's case.
    #[test]
    fn keep_unknown_opt_decides_whether_an_unknown_option_survives() {
        assert_eq!(kept(&["--nope", "main"], Flags::LOG), v(&["--nope", "main"]));
        match sweep(&v(&["--nope", "main"]), Flags::SHORTLOG, table) {
            Sweep::Unknown(a) => assert_eq!(a, "--nope"),
            _ => panic!("expected PARSE_OPT_UNKNOWN"),
        }
    }

    /// The separator ends the sweep either way; only the `--` itself differs, and
    /// the tail behind it is never offered to the table.
    #[test]
    fn keep_dashdash_decides_only_whether_the_separator_survives() {
        assert_eq!(
            kept(&["--own", "main", "--", "--own=bad"], Flags::LOG),
            v(&["main", "--", "--own=bad"])
        );
        assert_eq!(
            kept(&["--own", "main", "--", "--own=bad"], Flags::FAST_EXPORT),
            v(&["main", "--own=bad"])
        );
        // `--end-of-options` is the same break.
        assert_eq!(
            kept(&["--own", "--end-of-options", "--own=bad"], Flags::LOG),
            v(&["--end-of-options", "--own=bad"])
        );
    }

    /// `STOP_AT_NON_OPTION` hands the first positional and everything after it
    /// back untouched, which is how `bundle create <file> <rev-list args>` works.
    #[test]
    fn stop_at_non_option_ends_the_sweep_at_the_first_operand() {
        assert_eq!(
            kept(&["--own", "bundle.bdl", "--own=bad", "main"], Flags::BUNDLE),
            v(&["bundle.bdl", "--own=bad", "main"])
        );
        // A lone `-` is a positional, not an option.
        assert_eq!(kept(&["-", "--own=bad"], Flags::BUNDLE), v(&["-", "--own=bad"]));
    }

    /// Stage 2's separator search: the tail is taken verbatim, and `seen_dashdash`
    /// is reported so the caller knows a failed revision cannot fall back to a path.
    #[test]
    fn take_dashdash_splits_without_inspecting_the_tail() {
        let mut rest = v(&["main", "--", "-x", "no/such/path"]);
        let (paths, seen) = take_dashdash(&mut rest);
        assert!(seen);
        assert_eq!(rest, v(&["main"]));
        assert_eq!(paths, v(&["-x", "no/such/path"]));

        let mut rest = v(&["main", "README.md"]);
        let (paths, seen) = take_dashdash(&mut rest);
        assert!(!seen);
        assert!(paths.is_empty());
        assert_eq!(rest, v(&["main", "README.md"]));
    }

    /// The pathspec break's own diagnosis: a `-` token in path position is the
    /// "must come before non-option arguments" fatal, and only the first element
    /// gets the ambiguous-with-a-revision wording.
    #[test]
    fn pathspec_tail_reports_an_option_written_behind_a_pathspec() {
        let err = pathspec_tail(&v(&["--max-count=1"])).unwrap_err();
        assert_eq!(err, "option '--max-count=1' must come before non-option arguments");

        let err = pathspec_tail(&v(&["no/such/path"])).unwrap_err();
        assert!(err.starts_with("ambiguous argument 'no/such/path':"), "{err}");

        let err = pathspec_tail(&v(&["*", "no/such/path"])).unwrap_err();
        assert!(err.starts_with("no/such/path: no such path in the working tree."), "{err}");
    }
}
