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

// ---------------------------------------------------------------------------
// parse_short_opt()'s character loop
// ---------------------------------------------------------------------------
//
// A short option in git is a *character*, never a word. `parse_options_step()`
// points `ctx->opt` at the second byte of the argument and then keeps calling
// `parse_short_opt()` until the word is exhausted:
//
// ```c
//         if (arg[1] != '-') {
//                 ctx->opt = arg + 1;
//                 switch (parse_short_opt(ctx, options)) {
//                 ...
//                 while (ctx->opt) {
//                         switch (parse_short_opt(ctx, options)) {
//                         case PARSE_OPT_UNKNOWN:
//                                 if (internal_help && *ctx->opt == 'h')
//                                         goto show_usage;
//                                 /* fake a short option thing to hide the fact
//                                  * that we may have started to parse aggregated
//                                  * stuff */
//                                 ctx->argv[0] = xstrdup(ctx->opt - 1);
//                                 *(char *)ctx->argv[0] = '-';
//                                 goto unknown;
// ```
// (parse-options.c:1061-1107)
//
// and `parse_short_opt()` itself decides how much of the word one character
// eats:
//
// ```c
//         for (; options->type != OPTION_END; options++) {
//                 if (options->short_name == *p->opt) {
//                         p->opt = p->opt[1] ? p->opt + 1 : NULL;
//                         return get_value(p, options, OPT_SHORT);
//                 }
//                 if (options->type == OPTION_NUMBER)
//                         numopt = options;
//         }
//         if (numopt && isdigit(*p->opt)) {
//                 size_t len = 1;
//                 while (isdigit(p->opt[len]))
//                         len++;
//                 arg = xmemdupz(p->opt, len);
//                 p->opt = p->opt[len] ? p->opt + len : NULL;
// ```
// (parse-options.c:426-461)
//
// The whole word behind the matched character survives in `p->opt`, and what
// happens to it is decided by the option's *type*, in `get_arg()`:
//
// ```c
//         if (p->opt) {
//                 *arg = p->opt;
//                 p->opt = NULL;
//         } else if (p->argc > 1) {
//                 p->argc--;
//                 *arg = *++p->argv;
//         } else
//                 return error(_("%s requires a value"), optname(opt, flags));
// ```
// (parse-options.c:47-62)
//
// So a value-taking character swallows the *rest of the word* whatever it looks
// like (`git diff -U3p` is `error: --unified expects a numerical value`, not
// `-U 3` plus `-p`), an option carrying `PARSE_OPT_NOARG` leaves the rest for
// the next character, and a `PARSE_OPT_OPTARG` option takes an attached value
// but never a detached one (`git diff -Mp` is `error: invalid argument to
// find-renames`), which is why its token cannot be split at all.

/// One command's short-option table, as much of it as the clump loop needs:
/// the three shapes `get_arg()` distinguishes, plus whether the table carries
/// an `OPTION_NUMBER` entry, which makes a run of digits one option
/// (parse-options.c:441-452).
#[derive(Clone, Copy, Debug, Default)]
pub struct Shorts<'a> {
    /// Characters whose options take no value.
    pub flags: &'a str,
    /// Characters whose options require one.
    pub values: &'a str,
    /// Characters whose options take an optional *attached* value.
    pub optargs: &'a str,
    /// The table has an `OPTION_NUMBER` entry, so `-v0` is `-v` then `-0`.
    pub number: bool,
}

impl<'a> Shorts<'a> {
    /// A table of flags only, the common case.
    pub const fn flags(flags: &'a str) -> Shorts<'a> {
        Shorts { flags, values: "", optargs: "", number: false }
    }
}

/// `parse_options_step()`'s short-option loop as an argv rewrite: every clumped
/// short option is handed back as the separate words git would have read it as,
/// so a command that matches whole tokens (`"-q" | "--quiet"`) sees what
/// `parse_short_opt()` would have fed it one character at a time.
///
/// A word this table does not fully own is left where git leaves it. An unknown
/// character ends the clump and the remainder is emitted as the synthetic
/// `-<rest>` token `parse_options_step()` builds before `goto unknown`
/// (parse-options.c:1095-1096), so the caller's own refusal names the character
/// parsing stopped at — ``unknown switch `Z' `` for `-vZ`, not `` `vZ' ``.
///
/// Only the option part of the line is rewritten: `--` ends the rewrite and its
/// tail is copied through untouched, since those words are pathspecs whatever
/// they start with.
pub fn expand_short(args: &[String], table: Shorts<'_>) -> Vec<String> {
    expand(args, table, Mode { stop_at_non_option: false, owned_words_only: false })
}

/// [`expand_short`] for a command parsed with `PARSE_OPT_STOP_AT_NON_OPTION`.
///
/// Option parsing there ends at the first `*arg != '-' || !arg[1]` token
/// (parse-options.c:1023-1030), and every word behind it is an operand however
/// it is spelled — `git config user.name -ab` stores the literal `-ab`. The
/// rewrite has to stop at the same place or it invents a second operand.
pub fn expand_short_to_operand(args: &[String], table: Shorts<'_>) -> Vec<String> {
    expand(args, table, Mode { stop_at_non_option: true, owned_words_only: false })
}

/// [`expand_short`] for a command that parses its argv in more than one pass,
/// where a word is rewritten only when this table owns it from end to end.
///
/// `git show` is the case: `cmd_log_init()` sweeps `builtin_log_options` first
/// and `setup_revisions()` reaches the diff table afterwards, and the two are
/// not interchangeable. `git show -qp` works, because the first pass matches
/// `q` and hands `-p` to the second; `git show -pq` is `fatal: unrecognized
/// argument: -q`, because the pass that owns `q` stopped at the unknown `p`
/// before it could be reached. A rewritten token carries no record of which
/// pass synthesized it, so splitting a word this table does not fully own would
/// hand the other pass an option stock never lets it see. Leaving such a word
/// whole is the conservative half of the difference: the command still refuses
/// it, which is what stock does too.
pub fn expand_short_owned_words(args: &[String], table: Shorts<'_>) -> Vec<String> {
    expand(args, table, Mode { stop_at_non_option: false, owned_words_only: true })
}

/// The two ways the rewrite narrows itself, each a property of the command
/// being parsed rather than of `parse_short_opt()`.
#[derive(Clone, Copy)]
struct Mode {
    /// `PARSE_OPT_STOP_AT_NON_OPTION`: stop at the first operand.
    stop_at_non_option: bool,
    /// Rewrite only a word this table owns from end to end; see
    /// [`expand_short_owned_words`].
    owned_words_only: bool,
}

fn expand(args: &[String], table: Shorts<'_>, mode: Mode) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(args.len());
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        // `if (*arg != '-' || !arg[1])`: a positional or a lone `-`. A `--`
        // word is `parse_long_opt()`'s.
        let operand = !a.starts_with('-') || a.as_str() == "-";
        if a == "--" || (operand && mode.stop_at_non_option) {
            out.push(a.clone());
            out.extend(rest.cloned());
            return out;
        }
        if operand || a.starts_with("--") {
            out.push(a.clone());
            continue;
        }
        let mut word = Vec::new();
        let owes = expand_word(&a[1..], table, &mut word);
        // In owned-words mode the synthetic remainder token
        // `parse_options_step()` builds at parse-options.c:1095-1096 is the
        // tell that this table did not finish the word: put the word back.
        let unowned = mode.owned_words_only
            && word.last().is_some_and(|last| {
                last.starts_with('-') && !claimed(last.chars().nth(1).unwrap_or('-'), table)
            });
        match unowned {
            true => out.push(a.clone()),
            false => out.extend(word),
        }
        // `get_arg()`'s detached form: the word ended on an option that still
        // owes a value, so the next word is that value and is never looked at
        // as an option (parse-options.c:47-62). Copying it through here is what
        // keeps a value that happens to look like a clump — `git commit -m -ab`
        // — from being rewritten.
        if owes {
            if let Some(v) = rest.next() {
                out.push(v.clone());
            }
        }
    }
    out
}

/// Whether one character reaches an entry of `table` at all.
fn claimed(c: char, table: Shorts<'_>) -> bool {
    table.flags.contains(c)
        || table.values.contains(c)
        || table.optargs.contains(c)
        || (table.number && c.is_ascii_digit())
}

/// One `-xyz` word, split the way `parse_short_opt()` would have consumed it.
///
/// Returns whether the word ran out on an option that still owes a value, i.e.
/// whether `get_arg()` would take the *next* argv word.
fn expand_word(body: &str, table: Shorts<'_>, out: &mut Vec<String>) -> bool {
    let mut at = 0;
    while at < body.len() {
        let c = body[at..].chars().next().expect("a char boundary");
        let tail = &body[at + c.len_utf8()..];

        if table.flags.contains(c) {
            out.push(format!("-{c}"));
            at += c.len_utf8();
            continue;
        }
        if table.values.contains(c) {
            // `get_arg()`: `p->opt` is the value if anything is left of the
            // word, and the next argv word if not.
            out.push(format!("-{c}"));
            if !tail.is_empty() {
                out.push(tail.to_string());
                return false;
            }
            return true;
        }
        if table.optargs.contains(c) {
            out.push(format!("-{c}{tail}"));
            return false;
        }
        if table.number && c.is_ascii_digit() {
            let len = body[at..].find(|d: char| !d.is_ascii_digit()).unwrap_or(body.len() - at);
            out.push(format!("-{}", &body[at..at + len]));
            at += len;
            continue;
        }
        // `ctx->argv[0] = xstrdup(ctx->opt - 1); *ctx->argv[0] = '-';`
        out.push(format!("-{}", &body[at..]));
        return false;
    }
    false
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

    /// A flag clump is the separate words git reads it as, and the characters
    /// in front of an unknown one are still consumed — `git branch -vZ` applies
    /// `-v` and then refuses `Z` alone.
    #[test]
    fn a_clump_of_flags_becomes_one_word_per_character() {
        let t = Shorts::flags("spq");
        assert_eq!(expand_short(&v(&["-sp"]), t), v(&["-s", "-p"]));
        assert_eq!(expand_short(&v(&["-spq", "HEAD"]), t), v(&["-s", "-p", "-q", "HEAD"]));
        // `ctx->argv[0] = xstrdup(ctx->opt - 1)`: the refusal names `Z`, and
        // everything behind it rides along in the synthetic token.
        assert_eq!(expand_short(&v(&["-sZp"]), t), v(&["-s", "-Zp"]));
        assert_eq!(OptName::typed("-Zp"), OptName::Short('Z'));
    }

    /// `get_arg()`: a required value is the rest of the word, or the next word
    /// when the option ends the word. Both forms reach the command as `-m` plus
    /// one value word.
    #[test]
    fn a_value_taking_character_swallows_the_rest_of_the_word() {
        let t = Shorts { flags: "q", values: "m", optargs: "", number: false };
        assert_eq!(expand_short(&v(&["-qm", "x"]), t), v(&["-q", "-m", "x"]));
        assert_eq!(expand_short(&v(&["-qmx"]), t), v(&["-q", "-m", "x"]));
        // Whatever is left is the value, options included: `git diff -U3p` is
        // one bad `--unified` value and not `-U 3` plus `-p`.
        let u = Shorts { flags: "p", values: "U", optargs: "", number: false };
        assert_eq!(expand_short(&v(&["-U3p"]), u), v(&["-U", "3p"]));
        assert_eq!(expand_short(&v(&["-pU3"]), u), v(&["-p", "-U", "3"]));
    }

    /// `PARSE_OPT_OPTARG` takes an attached value and never a detached one, so
    /// its token must stay glued: `git diff -Mp` is `-M` with the value `p`.
    #[test]
    fn an_optarg_character_keeps_its_word_glued() {
        let t = Shorts { flags: "p", values: "", optargs: "M", number: false };
        assert_eq!(expand_short(&v(&["-Mp"]), t), v(&["-Mp"]));
        assert_eq!(expand_short(&v(&["-pM"]), t), v(&["-p", "-M"]));
        assert_eq!(expand_short(&v(&["-pM50"]), t), v(&["-p", "-M50"]));
    }

    /// `OPTION_NUMBER` (parse-options.c:441-452): a run of digits is one option
    /// and parsing continues behind it, which is what makes `git archive -v0`
    /// hand `-0` to the format backend.
    #[test]
    fn a_digit_run_is_one_option_when_the_table_has_a_number_entry() {
        let t = Shorts { flags: "v", values: "", optargs: "", number: true };
        assert_eq!(expand_short(&v(&["-v0"]), t), v(&["-v", "-0"]));
        assert_eq!(expand_short(&v(&["-v12v"]), t), v(&["-v", "-12", "-v"]));
        // Without the entry a digit is just an unknown character.
        assert_eq!(expand_short(&v(&["-v0"]), Shorts::flags("v")), v(&["-v", "-0"]));
        assert_eq!(expand_short(&v(&["-v12v"]), Shorts::flags("v")), v(&["-v", "-12v"]));
    }

    /// A detached value is a *value*, so it is copied through without being
    /// looked at — `get_arg()` reads `*++p->argv` and never `parse_short_opt()`
    /// (parse-options.c:52-54). Without that the rewrite reads an operand as a
    /// clump, which is how `git commit -m -ab` would have become `-m -a -b`.
    #[test]
    fn a_detached_value_is_never_rewritten() {
        let t = Shorts { flags: "ab", values: "m", optargs: "", number: false };
        assert_eq!(expand_short(&v(&["-m", "-ab"]), t), v(&["-m", "-ab"]));
        // …and only that one word: the next is an option again.
        assert_eq!(
            expand_short(&v(&["-am", "-ab", "-ab"]), t),
            v(&["-a", "-m", "-ab", "-a", "-b"])
        );
        // An *attached* value ends the word, so the next one is an option again.
        assert_eq!(expand_short(&v(&["-mx", "-ab"]), t), v(&["-m", "x", "-a", "-b"]));
        // A value that is itself an option word is still just a value, and only
        // that one word is skipped.
        assert_eq!(expand_short(&v(&["-m", "-m", "-ab"]), t), v(&["-m", "-m", "-a", "-b"]));
    }

    /// An operand is not an option however it is spelled, so its characters are
    /// never matched against the table.
    #[test]
    fn an_operand_is_not_a_clump() {
        let t = Shorts { flags: "ab", values: "m", optargs: "", number: false };
        // `am` is a path, not `-a -m`, and it does not eat the word behind it.
        assert_eq!(expand_short(&v(&["am", "-ab"]), t), v(&["am", "-a", "-b"]));
    }

    /// Owned-words mode rewrites a word only when the table finishes it, and
    /// still steps over a detached value rather than rewriting it.
    #[test]
    fn owned_words_mode_leaves_a_word_it_does_not_finish() {
        let t = Shorts { flags: "ab", values: "m", optargs: "", number: false };
        assert_eq!(expand_short_owned_words(&v(&["-ab"]), t), v(&["-a", "-b"]));
        // `Z` is in no entry, so the whole word goes back the way it came —
        // where plain `expand_short` would have produced `-a` plus `-Zb`.
        assert_eq!(expand_short_owned_words(&v(&["-aZb"]), t), v(&["-aZb"]));
        assert_eq!(expand_short(&v(&["-aZb"]), t), v(&["-a", "-Zb"]));
        assert_eq!(expand_short_owned_words(&v(&["-m", "-ab"]), t), v(&["-m", "-ab"]));
    }

    /// Nothing outside the option part of the line is touched: long options, a
    /// lone `-`, an operand, and every word behind `--`.
    #[test]
    fn only_clumped_short_options_are_rewritten() {
        let t = Shorts::flags("sp");
        assert_eq!(
            expand_short(&v(&["--stat", "-", "-s", "x", "--", "-sp", "-x"]), t),
            v(&["--stat", "-", "-s", "x", "--", "-sp", "-x"])
        );
        // A word that is already one option is handed back byte for byte.
        assert_eq!(expand_short(&v(&["-s"]), t), v(&["-s"]));
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
