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

// ---------------------------------------------------------------------------
// parse-options.c's error vocabulary
// ---------------------------------------------------------------------------
//
// git has exactly four things to say about a malformed option, and it says them
// in exactly two shapes. Which shape a command uses is not a style choice: it is
// decided by which `enum parse_opt_result` reached `parse_options()`, and the
// two differ in whether the usage block follows and therefore in how much a
// caller's stderr grows.
//
// ```c
//         switch (parse_options_step(&ctx, options, usagestr)) {
//         case PARSE_OPT_HELP:
//         case PARSE_OPT_ERROR:
//                 exit(129);
//         ...
//         case PARSE_OPT_UNKNOWN:
//                 if (ctx.argv[0][1] == '-') {
//                         error(_("unknown option `%s'"), ctx.argv[0] + 2);
//                 } else if (isascii(*ctx.opt)) {
//                         error(_("unknown switch `%c'"), *ctx.opt);
//                 } else {
//                         error(_("unknown non-ascii option in string: `%s'"),
//                               ctx.argv[0]);
//                 }
//                 usage_with_options(usagestr, options);
//         }
// ```
// (parse-options.c:1198-1224)
//
// `PARSE_OPT_ERROR` is what `get_arg()` returns for a missing value and what a
// rejecting callback returns. It has already printed its own `error:` line, and
// `parse_options()` does nothing but `exit(129)` — **no usage block**.
// `PARSE_OPT_UNKNOWN` is the other shape: `parse_options()` prints the `error:`
// line *itself* and then calls `usage_with_options()`, which renders the block on
// stderr and exits 129. So `git commit -m` is one line and `git commit -b` is
// eighty, and a port that appends the block to both — or to neither — is wrong in
// a way scripts notice.

/// `optname()` (parse-options.c:30-45): how parse-options names an option inside
/// a diagnostic.
///
/// ```c
/// static const char *optname(const struct option *opt, enum opt_parsed flags)
/// {
///         if (flags & OPT_SHORT)
///                 strbuf_addf(&sb, "switch `%c'", opt->short_name);
///         else if (flags & OPT_UNSET)
///                 strbuf_addf(&sb, "option `no-%s'", opt->long_name);
///         else if (flags == OPT_LONG)
///                 strbuf_addf(&sb, "option `%s'", opt->long_name);
/// ```
///
/// The distinction is the *spelling the user typed*, not the option's identity:
/// `git commit -m` says ``switch `m'`` and `git commit --message` says
/// ``option `message'`` even though both reach the same table entry. Naming a
/// short option by its long name (or the reverse) is the single most common way
/// this port used to diverge, which is why the choice is a type rather than a
/// `format!`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptName<'a> {
    /// `OPT_SHORT`: named by the one character the table matched.
    Short(char),
    /// `OPT_LONG`: named by `long_name`, without the leading `--`.
    Long(&'a str),
    /// `OPT_UNSET`: `long_name` with git's own `no-` glued back on. git builds
    /// this from the table's stem, so a `--no-x` spelling of a `no-x` *entry*
    /// still reports `no-x` and not `no-no-x`.
    Unset(&'a str),
}

impl<'a> OptName<'a> {
    /// The name for an option as the user spelled it on the command line.
    ///
    /// This is the mapping every ordinary call site wants, because `optname()`'s
    /// `flags` argument is exactly `parse_short_opt()`'s `OPT_SHORT` versus
    /// `parse_long_opt()`'s `OPT_LONG`/`OPT_UNSET` — i.e. which of the two
    /// parsers saw the token. A `--no-<stem>` token arrives here as
    /// `Long("no-<stem>")`, which renders identically to `Unset("<stem>")`; the
    /// two variants differ only for a caller that holds the table entry rather
    /// than the token.
    ///
    /// `tok` is the argument with its dashes: `-m`, `--message`, `--message=x`.
    /// A value glued on with `=` is dropped, since git names the option and not
    /// the assignment.
    pub fn typed(tok: &'a str) -> OptName<'a> {
        match tok.strip_prefix("--") {
            Some(body) => OptName::Long(body.split_once('=').map_or(body, |(n, _)| n)),
            // A cluster (`-fam`) is named by the character parsing stopped at,
            // so a caller that has already split one out passes `-m`; passing
            // the whole cluster names its first character, which is what
            // `*ctx->opt` holds when the *first* character is the unknown one.
            None => OptName::Short(tok.trim_start_matches('-').chars().next().unwrap_or('-')),
        }
    }
}

impl std::fmt::Display for OptName<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OptName::Short(c) => write!(f, "switch `{c}'"),
            OptName::Long(name) => write!(f, "option `{name}'"),
            OptName::Unset(name) => write!(f, "option `no-{name}'"),
        }
    }
}

/// `get_arg()`'s refusal (parse-options.c:59-60), the `PARSE_OPT_ERROR` shape:
/// one `error:` line on stderr, **no usage block**, exit 129.
///
/// ```c
///         } else
///                 return error(_("%s requires a value"), optname(opt, flags));
/// ```
pub fn requires_value(name: OptName<'_>) -> ExitCode {
    eprintln!("error: {name} requires a value");
    ExitCode::from(USAGE_ERROR)
}

/// `do_get_value()`'s two `takes no value` refusals (parse-options.c:138-143):
/// an `=<value>` written on the `--no-` spelling of any option, or on an option
/// carrying `PARSE_OPT_NOARG`. Same shape as [`requires_value`].
pub fn takes_no_value(name: OptName<'_>) -> ExitCode {
    eprintln!("error: {name} takes no value");
    ExitCode::from(USAGE_ERROR)
}

/// `do_get_value()`'s `PARSE_OPT_NONEG` refusal (parse-options.c:140-141), for a
/// `--no-` spelling the table forbids. Reachable only when the caller resolves
/// the negation itself; [`crate::porcelain::resolve_long`] normally answers such
/// a token with `Unknown` instead, which is the *other* shape.
pub fn isnt_available(name: OptName<'_>) -> ExitCode {
    eprintln!("error: {name} isn't available");
    ExitCode::from(USAGE_ERROR)
}

/// `get_arg()` (parse-options.c:47-62) for a command that walks argv itself.
///
/// ```c
/// static enum parse_opt_result get_arg(struct parse_opt_ctx_t *p,
///                                      const struct option *opt,
///                                      enum opt_parsed flags, const char **arg)
/// {
///         if (p->opt) {
///                 *arg = p->opt;
///                 p->opt = NULL;
///         } else if (p->argc == 1 && (opt->flags & PARSE_OPT_LASTARG_DEFAULT)) {
///                 *arg = (const char *)opt->defval;
///         } else if (p->argc > 1) {
///                 p->argc--;
///                 *arg = *++p->argv;
///         } else
///                 return error(_("%s requires a value"), optname(opt, flags));
///         return 0;
/// }
/// ```
///
/// `i` is the index of the *next* unread argument, i.e. the caller has already
/// stepped past the option token; it is advanced past the value on success. The
/// attached forms (`--opt=v`, `-mv`) correspond to `p->opt` being non-NULL and
/// are the caller's to peel off before getting here, because only the caller
/// knows how many characters of the token were the option's name.
///
/// The whole point is the failure branch. Reading the value as
/// `args.get(i).map(String::as_str).unwrap_or("")` — which is what several verbs
/// in this port used to do — turns "you forgot the value" into "the value is the
/// empty string", so `git merge --cleanup` reported an invalid cleanup mode and
/// `git add --pathspec-from-file` tried to open `''`. Both are `error: option
/// `<name>' requires a value` and 129 in stock git, and neither ever reaches the
/// command's own logic.
pub fn get_arg<'a>(
    args: &'a [String],
    i: &mut usize,
    name: OptName<'_>,
) -> Result<&'a str, anyhow::Error> {
    let value = value_at(args, *i, name)?;
    *i += 1;
    Ok(value)
}

/// [`get_arg`] as a plain read, for the argv loops that *peek* at the following
/// argument and then step onto it rather than past it.
///
/// The two conventions are equally common in this port and neither is wrong;
/// what matters is that both reach the same refusal, so a verb's wording does
/// not depend on how its loop happens to count. `i` is the index the value is
/// expected at and nothing is advanced.
pub fn value_at<'a>(
    args: &'a [String],
    i: usize,
    name: OptName<'_>,
) -> Result<&'a str, anyhow::Error> {
    match args.get(i) {
        Some(v) => Ok(v.as_str()),
        None => {
            let _ = requires_value(name);
            Err(silent(USAGE_ERROR))
        }
    }
}

/// `PARSE_OPT_LASTARG_DEFAULT` (parse-options.c:54-55): the option's built-in
/// default when it is the *last* argument, and the next token otherwise —
/// consumed unconditionally, whatever it looks like. `--contains`, `--merged`
/// and `--points-at` are the family, all defaulting to `HEAD`.
pub fn get_arg_lastarg<'a>(args: &'a [String], i: &mut usize, defval: &'a str) -> &'a str {
    match args.get(*i) {
        Some(v) => {
            *i += 1;
            v.as_str()
        }
        None => defval,
    }
}

/// `PARSE_OPT_UNKNOWN` (parse-options.c:1214-1223): the `error:` line **and** the
/// usage block, both on stderr, exit 129.
///
/// A long option is named without its `--` and *with* any `=<value>` still
/// attached (`ctx.argv[0] + 2`); a short one is named by the single character
/// parsing stopped at, however many were clustered behind it, and a non-ASCII
/// byte is reported as the whole token. `tok` must therefore be the token as
/// git would hold it at the point of failure: for a cluster whose *second*
/// character is unknown, parse-options rewrites `ctx->argv[0]` to a synthetic
/// `-<rest>` (parse-options.c:1095-1096) before jumping to `unknown:`, so a
/// caller that splits clusters must pass `-<c><rest>` and not the original.
pub fn unknown_option(tok: &str, usage: &str) -> ExitCode {
    match tok.strip_prefix("--") {
        Some(body) => eprintln!("error: unknown option `{body}'"),
        None => {
            let c = tok.trim_start_matches('-').chars().next().unwrap_or('-');
            match c.is_ascii() {
                true => eprintln!("error: unknown switch `{c}'"),
                false => eprintln!("error: unknown non-ascii option in string: `{tok}'"),
            }
        }
    }
    eprint!("{usage}");
    ExitCode::from(USAGE_ERROR)
}

/// An already-printed parse-options refusal as an error to return with `?`.
///
/// Every helper above has written its own stderr by the time it returns, so what
/// unwinds must carry the exit code and nothing else — `run_command()` in
/// `lib.rs` prints `zvcs: <verb>: …` for an ordinary `anyhow` error, which would
/// both duplicate the message and replace 129 with 1.
///
/// It takes the raw status rather than an [`ExitCode`] because `ExitCode` has no
/// accessor to read one back out of; the constructors above hand back `ExitCode`
/// for the call sites that `return Ok(…)` it directly.
pub fn silent(code: u8) -> anyhow::Error {
    anyhow::Error::new(crate::fatal::Silent(code))
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

    /// `optname()`'s three renderings, each measured off stock git 2.55.0:
    /// `git commit -m`, `git commit --message`, `git branch --no-color=x`.
    #[test]
    fn optname_renders_the_spelling_that_was_typed() {
        assert_eq!(OptName::Short('m').to_string(), "switch `m'");
        assert_eq!(OptName::Long("message").to_string(), "option `message'");
        assert_eq!(OptName::Unset("color").to_string(), "option `no-color'");
        // The token's own dashes decide, and an `=<value>` is not part of the
        // name — `--message=` reports `option `message'`, not `message=`.
        assert_eq!(OptName::typed("-m"), OptName::Short('m'));
        assert_eq!(OptName::typed("--message"), OptName::Long("message"));
        assert_eq!(OptName::typed("--message=x"), OptName::Long("message"));
        // A `--no-` token is named with its `no-`, which is the same text
        // `Unset` produces from the stem.
        assert_eq!(OptName::typed("--no-color").to_string(), "option `no-color'");
        // A cluster is named by the character parsing stopped at; a caller that
        // has not split one yet names its first.
        assert_eq!(OptName::typed("-fam"), OptName::Short('f'));
    }

    /// `get_arg()`'s success path advances past the value, and its failure path
    /// is a refusal rather than an empty string — the distinction that decides
    /// whether `git merge --cleanup` reports a missing value or an invalid mode.
    #[test]
    fn get_arg_consumes_the_next_token_or_refuses() {
        let args = v(&["--cleanup", "verbatim"]);
        let mut i = 1;
        assert_eq!(get_arg(&args, &mut i, OptName::Long("cleanup")).unwrap(), "verbatim");
        assert_eq!(i, 2);

        // A value that looks like another option is still the value: `get_arg()`
        // reads `*++p->argv` without inspecting it.
        let args = v(&["-m", "--amend"]);
        let mut i = 1;
        assert_eq!(get_arg(&args, &mut i, OptName::Short('m')).unwrap(), "--amend");

        let args = v(&["--cleanup"]);
        let mut i = 1;
        let err = get_arg(&args, &mut i, OptName::Long("cleanup")).unwrap_err();
        assert_eq!(
            err.downcast_ref::<crate::fatal::Silent>().expect("a printed refusal").0,
            USAGE_ERROR
        );
        // The index must not move: nothing was consumed.
        assert_eq!(i, 1);
    }

    /// `PARSE_OPT_LASTARG_DEFAULT` consumes whatever follows and falls back to
    /// the option's default only when the option ends the command line.
    #[test]
    fn lastarg_default_takes_the_next_token_whatever_it_is() {
        let args = v(&["--contains", "-x"]);
        let mut i = 1;
        assert_eq!(get_arg_lastarg(&args, &mut i, "HEAD"), "-x");
        assert_eq!(i, 2);

        let args = v(&["--contains"]);
        let mut i = 1;
        assert_eq!(get_arg_lastarg(&args, &mut i, "HEAD"), "HEAD");
        assert_eq!(i, 1);
    }
}
