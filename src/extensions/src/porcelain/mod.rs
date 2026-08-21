//! git-compatible porcelain, served natively via the vendored gitoxide
//! crates. One module per subcommand, ported incrementally. Each is the
//! drop-in equivalent of the stock `git` subcommand of the same name, so
//! tools on PATH (RustRover, gh, cargo) see identical behavior against the
//! same on-disk `.git`.

/// `parse-options`' own "this option needed a value" refusal: the option named
/// the way it was typed (`option \`depth'` for a long one, `switch \`j'` for a
/// short one), one line on stderr, exit 129.
///
/// A thin spelling of [`crate::parseopt::requires_value`] for the call sites that
/// hold the token rather than the table entry. The implementation lives in
/// `parseopt` with the rest of `get_arg()`; keeping a second copy here is how the
/// two drifted apart in the first place, with `git commit -m` reporting
/// ``option `-m'`` while `git tag -m` reported ``switch `m'``.
pub(crate) fn missing_option_value(key: &str) -> std::process::ExitCode {
    crate::parseopt::requires_value(crate::parseopt::OptName::typed(key))
}

/// `get_arg()` for a command that walks argv itself and holds the *token* rather
/// than the table entry — which is every porcelain in this port.
///
/// `i` indexes the next unread argument (the caller has stepped past the option
/// already) and is advanced past the value. `tok` is the option as typed, dashes
/// and all: it decides nothing but the wording of the refusal, and that wording
/// is the whole reason this exists rather than `args.get(i)`. See
/// [`crate::parseopt::get_arg`] for the C and for why an absent value must not
/// become an empty one.
pub(crate) fn take_value<'a>(
    args: &'a [String],
    i: &mut usize,
    tok: &str,
) -> anyhow::Result<&'a str> {
    crate::parseopt::get_arg(args, i, crate::parseopt::OptName::typed(tok))
}

/// [`take_value`] as a plain read at `i`, for the loops that peek at the next
/// argument and then step onto it rather than past it. See
/// [`crate::parseopt::value_at`].
pub(crate) fn value_at<'a>(
    args: &'a [String],
    i: usize,
    tok: &str,
) -> anyhow::Result<&'a str> {
    crate::parseopt::value_at(args, i, crate::parseopt::OptName::typed(tok))
}

/// `parse-options`' answer to `-h`: the command's usage block on **stdout**,
/// exit 129.
///
/// parse-options.c has one renderer, `usage_with_options_internal()`, and two
/// callers that differ only in where it writes. A `-h` reaches it through
/// `parse_options_step()`:
///
/// ```c
/// if (internal_help && *ctx->opt == 'h')
///         return usage_with_options_internal(ctx, usagestr, options,
///                                            USAGE_NORMAL, USAGE_TO_STDOUT);
/// ```
///
/// while every *rejection* reaches it through `usage_with_options()`, which
/// passes `USAGE_TO_STDERR` and prefixes an `error:` line of its own. Both exit
/// 129 — the status is not what distinguishes them, the stream is. That is the
/// single rule the whole port kept getting backwards: asking for help is not an
/// error, so it does not go to stderr and it is not announced as one.
///
/// The block itself is bespoke per command (each `builtin/<cmd>.c` owns its
/// `<cmd>_usage[]` and `struct option` table), so it stays a per-module `USAGE`
/// const; what is shared, and lives here, is the policy.
pub(crate) fn show_usage(usage: &str) -> std::process::ExitCode {
    print!("{usage}");
    std::process::ExitCode::from(129)
}

/// `show_usage_with_options_if_asked()` (parse-options.c:1490-1505): the same
/// block on stdout at 129, but **only when `-h` or `--help-all` is the sole
/// argument**.
///
/// Commands that do their own argv walk rather than calling `parse_options()`
/// — `fast-import`, `get-tar-commit-id`, `credential`, `merge-index`,
/// `merge-recursive` and the rest of the `show_usage_if_asked()` set — call it
/// on entry. The `ac == 2` guard is the whole difference from [`show_usage`]:
/// `git merge-index -h` prints usage, `git merge-index -h foo` does not, and
/// `git merge-index --help-all foo` does not either.
///
/// The two spellings differ only in `full_usage`: `-h` renders `USAGE_NORMAL`
/// and `--help-all` renders `USAGE_FULL`, which is the same table with the
/// `PARSE_OPT_HIDDEN` entries left in. Most option tables have no hidden entry,
/// so most commands print the same block for both — verified across stock git
/// 2.55.0, where 101 of 143 subcommands emit byte-identical `-h` and
/// `--help-all` output. [`show_usage_if_asked`] is the spelling for those;
/// [`show_usage_if_asked_full`] takes the second block for the rest.
pub(crate) fn show_usage_if_asked(
    args: &[String],
    usage: &str,
) -> Option<std::process::ExitCode> {
    show_usage_if_asked_full(args, usage, usage)
}

/// [`show_usage_if_asked`] for a command whose option table has a
/// `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` is not `USAGE_NORMAL`.
pub(crate) fn show_usage_if_asked_full(
    args: &[String],
    usage: &str,
    full_usage: &str,
) -> Option<std::process::ExitCode> {
    if args.len() != 1 {
        return None;
    }
    match args[0].as_str() {
        "-h" => Some(show_usage(usage)),
        "--help-all" => Some(show_usage(full_usage)),
        _ => None,
    }
}

/// Whether `parse_options()` would answer this argument with its own help,
/// for a command that drives its own argv walk rather than calling
/// [`show_usage_if_asked`].
///
/// The two spellings arrive by different routes, and the difference shows:
///
/// * `--help-all` is an exact `strcmp()` at parse-options.c:1125, ahead of
///   `parse_long_opt()`. It never abbreviates and never takes an `=<value>`, so
///   `--help-al` and `--help-all=x` are ordinary unknown options.
/// * `-h` has no table entry anywhere. It arrives as an *unrecognised short
///   option*: `parse_short_opt()` consumes the leading characters the table does
///   define, then `parse_options_step()` tests `*ctx->opt == 'h'`
///   (parse-options.c:1066-1069 for the first character, :1085-1086 for the rest
///   of a cluster). A cluster therefore asks for help exactly when the first
///   character the table does *not* define is `h`: `git remote prune -nh` prints
///   the block because `-n` is `prune`'s, while `git reflog list -nh` reports
///   ``unknown switch `n'`` because `list`'s table is empty.
///
/// `shorts` is the sub-command's **argument-less** short options only. One that
/// takes a value swallows the rest of the token as that value
/// (`parse_short_opt()` → `get_arg()`), so `git remote add -th` is
/// `--track=h` and not a request for help.
///
/// The test also survives `PARSE_OPT_KEEP_UNKNOWN_OPT`: it sits in the `UNKNOWN`
/// arm *before* the `unknown:` label that flag diverts to, which is why
/// `git reflog show -h` prints usage while `git reflog show --zzbogus` is
/// forwarded to the revision parser.
pub(crate) fn asks_for_help(tok: &str, shorts: &str) -> bool {
    if tok == "--help-all" {
        return true;
    }
    match tok.strip_prefix('-') {
        Some(body) if !body.is_empty() && !body.starts_with('-') => body
            .trim_start_matches(|c| shorts.contains(c))
            .starts_with('h'),
        _ => false,
    }
}

/// What a long option does with a value, mirroring the `struct option` entry it
/// was derived from.
#[derive(PartialEq, Eq, Clone, Copy)]
pub(crate) enum Arg {
    /// No value at all: `OPT_BOOL`, `OPT_BIT`, `OPT_COUNTUP`, `OPT_SET_INT`.
    None,
    /// A value is mandatory, attached or as the next argument: `OPT_STRING`,
    /// `OPT_INTEGER`, `OPT_STRING_LIST`, plain `OPT_CALLBACK`.
    Required,
    /// `PARSE_OPT_OPTARG`: only an attached `=<value>` is taken, never the next
    /// argument.
    Optional,
    /// `PARSE_OPT_LASTARG_DEFAULT`: an attached value, else the next argument,
    /// else the option's built-in default when this is the last argument.
    LastArg,
}

/// One entry of a builtin's `struct option options[]`, carrying only what
/// `parse_long_opt()` reads while it resolves a name.
#[derive(Clone, Copy)]
pub(crate) struct LongOpt {
    /// `long_name`, spelled exactly as the C table spells it — including a
    /// leading `no-` where the table itself has one (`--no-contains`), which
    /// parse-options treats as the *unset* sense of the stem.
    pub(crate) name: &'static str,
    /// Whether the automatic `--no-<name>` resolves: true unless the entry
    /// carries `PARSE_OPT_NONEG`. `-h` renders exactly these as `--[no-]x`.
    pub(crate) neg: bool,
    /// What the entry does with a value. Read by the commands that drive their
    /// whole parse off the table; a command that only borrows the *resolver*
    /// still records it, because it is the same line of C.
    pub(crate) arg: Arg,
}

/// What `parse_long_opt()` made of a long option's name.
pub(crate) enum Resolved {
    /// The option, and whether it was reached in the `OPT_UNSET` sense.
    One(&'static LongOpt, bool),
    /// Two candidate spellings, already carrying their `no-` prefix where the
    /// match was a negation — reported in that order.
    Ambiguous(String, String),
    /// No entry claimed the name.
    Unknown,
}

/// `is_alias()` (parse-options.c:471): whether two long names were joined by an
/// `OPT_ALIAS()` in this command's table, which stops the second one from making
/// the first ambiguous. `alias_groups` is empty for every command that declares
/// no `OPT_ALIAS`, and `is_alias()` then returns 0 unconditionally.
fn is_alias(groups: &[&[&str]], one: &str, other: &str) -> bool {
    groups.iter().any(|g| g.contains(&one) && g.contains(&other))
}

/// [`resolve_long_aliased`] for the commands whose option table has no
/// `OPT_ALIAS()` entry, which is all but a handful.
pub(crate) fn resolve_long(table: &'static [LongOpt], arg: &str) -> Resolved {
    resolve_long_aliased(table, &[], arg)
}

/// `parse_long_opt()` (parse-options.c:519-594), the half that turns a typed
/// `--<arg>` into the table entry it names: exact match, else unique
/// abbreviation, else the `ambiguous option:` refusal.
///
/// ```c
/// static enum parse_opt_result parse_long_opt(
///         struct parse_opt_ctx_t *p, const char *arg,
///         const struct option *options)
/// {
///         const char *arg_end = strchrnul(arg, '=');
///         const char *arg_start = arg;
///         enum opt_parsed flags = OPT_LONG;
///         int arg_starts_with_no_no = 0;
///         struct parsed_option abbrev = { .option = NULL, .flags = OPT_LONG };
///         struct parsed_option ambiguous = { .option = NULL, .flags = OPT_LONG };
///
///         if (skip_prefix(arg_start, "no-", &arg_start)) {
///                 if (skip_prefix(arg_start, "no-", &arg_start))
///                         arg_starts_with_no_no = 1;
///                 else
///                         flags |= OPT_UNSET;
///         }
///
///         for (; options->type != OPTION_END; options++) {
///                 const char *rest, *long_name = options->long_name;
///                 enum opt_parsed opt_flags = OPT_LONG;
///                 int allow_unset = !(options->flags & PARSE_OPT_NONEG);
///                 ...
///                 if (skip_prefix(long_name, "no-", &long_name))
///                         opt_flags |= OPT_UNSET;
///                 else if (arg_starts_with_no_no)
///                         continue;
///
///                 if (((flags ^ opt_flags) & OPT_UNSET) && !allow_unset)
///                         continue;
///
///                 if (skip_prefix(arg_start, long_name, &rest)) {
///                         if (*rest == '=')
///                                 p->opt = rest + 1;
///                         else if (*rest)
///                                 continue;
///                         return get_value(p, options, flags ^ opt_flags);
///                 }
///
///                 /* abbreviated? */
///                 if (!strncmp(long_name, arg_start, arg_end - arg_start))
///                         register_abbrev(p, options, flags ^ opt_flags,
///                                         &abbrev, &ambiguous);
///
///                 /* negated and abbreviated very much? */
///                 if (allow_unset && starts_with("no-", arg))
///                         register_abbrev(p, options, OPT_UNSET ^ opt_flags,
///                                         &abbrev, &ambiguous);
///         }
/// ```
///
/// Three details carry the whole behavior and each one was a bug on the way in:
///
/// * `arg` is the body **including** any `=<value>`, and `starts_with("no-", arg)`
///   asks whether *`arg` is a prefix of `"no-"`* — the one direction that makes
///   `--n` name every negatable option at once.
/// * a table entry spelled `no-<stem>` is the unset sense of `<stem>`, so
///   `--no-contains` and `--contains` are two entries that must not shadow each
///   other; the sense actually selected is `flags ^ opt_flags`.
/// * `register_abbrev()` keeps the *last two* candidates, so the two names in an
///   `ambiguous option:` sentence are in table order and reordering a table
///   changes the message.
///
/// `disallow_abbreviated_options` (the `GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS`
/// escape hatch git's own test suite sets) is not reproduced; nothing outside
/// git's test suite sets it.
pub(crate) fn resolve_long_aliased(
    table: &'static [LongOpt],
    alias_groups: &[&[&str]],
    arg: &str,
) -> Resolved {
    // `arg_end = strchrnul(arg, '=')`: names are matched without their value.
    let name = arg.split_once('=').map_or(arg, |(n, _)| n);

    let mut start = name;
    let mut unset = false;
    let mut no_no = false;
    if let Some(rest) = start.strip_prefix("no-") {
        start = rest;
        match start.strip_prefix("no-") {
            Some(rest) => {
                start = rest;
                no_no = true;
            }
            None => unset = true,
        }
    }

    let mut abbrev: Option<(&'static LongOpt, bool)> = None;
    let mut ambiguous: Option<(&'static LongOpt, bool)> = None;
    // `register_abbrev()` (parse-options.c:497): the previous candidate becomes
    // the ambiguity report unless it was the same option under an alias.
    let mut register = |opt: &'static LongOpt, sense: bool| {
        if let Some(prev) = abbrev {
            if !(prev.1 == sense && is_alias(alias_groups, prev.0.name, opt.name)) {
                ambiguous = Some(prev);
            }
        }
        abbrev = Some((opt, sense));
    };

    for opt in table {
        let (long_name, opt_unset) = match opt.name.strip_prefix("no-") {
            Some(stem) => (stem, true),
            // Only a `no-`-named entry can answer a `--no-no-...` spelling.
            None if no_no => continue,
            None => (opt.name, false),
        };
        let sense = unset ^ opt_unset;
        if sense && !opt.neg {
            continue;
        }
        if let Some(rest) = start.strip_prefix(long_name) {
            // `rest` is empty here whenever the C sees `=`, since the value was
            // split off above; a non-empty remainder is a longer, different name.
            if rest.is_empty() {
                return Resolved::One(opt, sense);
            }
        }
        if long_name.starts_with(start) {
            register(opt, sense);
        }
        if opt.neg && "no-".starts_with(arg) {
            register(opt, !opt_unset);
        }
    }

    // `(flags & OPT_UNSET) ? "no-" : ""` prefixed to `option->long_name`, which
    // is the table's own spelling — a `no-`-named entry keeps its `no-`.
    let spell = |(opt, sense): (&LongOpt, bool)| match sense {
        true => format!("no-{}", opt.name),
        false => opt.name.to_string(),
    };
    match (ambiguous, abbrev) {
        (Some(a), Some(b)) => Resolved::Ambiguous(spell(a), spell(b)),
        (None, Some(b)) => Resolved::One(b.0, b.1),
        _ => Resolved::Unknown,
    }
}

/// A `--<something>` token, respelled the way [`resolve_long`] reads it.
pub(crate) enum Long<'a> {
    /// The spelling to match on: the token as typed when it was already exact
    /// (or when no entry claims it, so the command's own "unknown option" arm
    /// still echoes what the user wrote), else the full name it abbreviates.
    Name(std::borrow::Cow<'a, str>),
    /// Two candidates, for [`ambiguous_option`].
    Ambiguous(String, String),
}

/// Expand a long option's abbreviation to the name it resolves to, so a command
/// that dispatches on exact spellings answers `--stag` the same way it answers
/// `--stage`.
///
/// This is the resolution half of `parse_long_opt()` used as a pre-step rather
/// than as a parser: the command keeps its own argument walk, its own value
/// handling and its own diagnostics, and only the *name* is normalized on the
/// way in. An abbreviation therefore lands on exactly the arm its full spelling
/// lands on — including the arm that refuses an option this port has not
/// implemented. Resolving a name is not the same as implementing it, and this
/// deliberately does not turn one into the other.
///
/// A name no entry claims is handed back untouched, so the command's own
/// `unknown option` refusal quotes what was typed.
pub(crate) fn canonical_long<'a>(tok: &'a str, table: &'static [LongOpt]) -> Long<'a> {
    canonical_long_aliased(tok, table, &[])
}

/// [`canonical_long`] for a command whose table has `OPT_ALIAS()` entries.
pub(crate) fn canonical_long_aliased<'a>(
    tok: &'a str,
    table: &'static [LongOpt],
    alias_groups: &[&[&str]],
) -> Long<'a> {
    let Some(body) = tok.strip_prefix("--") else {
        return Long::Name(tok.into());
    };
    // `--` and `--end-of-options` are consumed by parse_options_step() before
    // any table lookup happens.
    if body.is_empty() || body == "end-of-options" {
        return Long::Name(tok.into());
    }
    match resolve_long_aliased(table, alias_groups, body) {
        Resolved::Unknown => Long::Name(tok.into()),
        Resolved::Ambiguous(first, second) => Long::Ambiguous(first, second),
        Resolved::One(opt, sense) => {
            // The spelling that parses back to this same entry and sense. For a
            // `no-`-named entry the two are crossed: `--no-verify` selects the
            // `no-verify` entry positively and `--verify` selects it unset, so
            // the `no-` belongs on the token exactly when `sense` and the
            // entry's own `no-` disagree.
            let (stem, opt_unset) = match opt.name.strip_prefix("no-") {
                Some(stem) => (stem, true),
                None => (opt.name, false),
            };
            let mut out = String::from("--");
            if sense ^ opt_unset {
                out.push_str("no-");
            }
            out.push_str(stem);
            if let Some((_, value)) = body.split_once('=') {
                out.push('=');
                out.push_str(value);
            }
            match out == tok {
                true => Long::Name(tok.into()),
                false => Long::Name(out.into()),
            }
        }
    }
}

/// The ambiguous-abbreviation refusal, which is the odd one out among
/// parse-options' rejections: `parse_long_opt()` reports the reason with
/// `error()` on **stderr** and then returns `PARSE_OPT_HELP`, which
/// `parse_options_step()` routes to
///
/// ```c
///  show_usage:
///         return usage_with_options_internal(ctx, usagestr, options,
///                                            USAGE_NORMAL, USAGE_TO_STDOUT);
/// ```
///
/// so the block lands on **stdout** while its explanation is on stderr, at 129.
///
/// `tok` is the argument as typed; git echoes the body after `--`, value and
/// all (`error(_("ambiguous option: %s ..."), arg)` with `arg = argv[0] + 2`).
pub(crate) fn ambiguous_option(
    tok: &str,
    first: &str,
    second: &str,
    usage: &str,
) -> std::process::ExitCode {
    let body = tok.strip_prefix("--").unwrap_or(tok);
    eprintln!("error: ambiguous option: {body} (could be --{first} or --{second})");
    print!("{usage}");
    std::process::ExitCode::from(129)
}

/// `PARSE_OPT_UNKNOWN`: the refusal for an argument no table entry claims.
///
/// `parse_options_step()` returns it and the caller reports both the reason and
/// the usage block on **stderr** (parse-options.c:1198-1224), which is the pair
/// that separates this from every other rejection:
///
/// ```c
///         case PARSE_OPT_UNKNOWN:
///                 if (ctx.argv[0][1] == '-')
///                         error(_("unknown option `%s'"), ctx.argv[0] + 2);
///                 else if (isascii(*ctx.opt))
///                         error(_("unknown switch `%c'"), *ctx.opt);
///                 else
///                         error(_("unknown non-ascii option in string: `%s'"),
///                               ctx.argv[0]);
///                 usage_with_options(usagestr, options);
/// ```
///
/// A long one is named without its `--` and **with** any `=<value>`; a short one
/// is named by its first character only, however many were clustered behind it.
/// Both exit 129. The contrast to keep straight: `PARSE_OPT_ERROR` — what
/// `get_arg()` returns for a missing value, and what a rejecting callback returns
/// — prints its own line and *no* block, so a bad value for a known option looks
/// nothing like an unknown option.
///
/// The rendering lives in [`crate::parseopt::unknown_option`], next to the
/// `PARSE_OPT_ERROR` shapes it must stay distinguishable from.
pub(crate) fn unknown_option(tok: &str, usage: &str) -> std::process::ExitCode {
    crate::parseopt::unknown_option(tok, usage)
}

mod add;
pub(crate) mod add_interactive;
pub(crate) mod add_patch;
mod am;
mod annotate;
mod apply;
mod archimport;
mod archive;
mod backfill;
mod binary_patch;
mod bisect;
mod blame;
pub(crate) mod branch;
mod bugreport;
mod bundle;
mod cat_file;
mod check_attr;
mod check_ignore;
mod check_mailmap;
mod check_ref_format;
mod checkout;
#[allow(non_snake_case)] // maps to git's `checkout--worker` subcommand
mod checkout__worker;
mod checkout_index;
mod cherry;
mod cherry_pick;
mod clean;
mod clone;
pub(crate) mod color;
pub(crate) mod column;
mod commit;
mod commit_graph;
mod commit_tree;
mod config;
mod count_objects;
mod credential;
mod credential_cache;
#[allow(non_snake_case)] // maps to git's `credential-cache--daemon` subcommand
mod credential_cache__daemon;
mod credential_netrc;
mod credential_osxkeychain;
mod credential_store;
mod cvsexportcommit;
mod cvsimport;
mod cvsserver;
mod daemon;
mod describe;
mod diagnose;
mod diff;
/// The `color.diff.*` slot table and git's colored-patch emit layer, shared by
/// every diff-producing command.
pub(crate) mod diff_color;
mod diff_files;
/// `--diff-filter=<letters>`: `diff_opt_diff_filter()` and `diffcore_apply_filter()`.
mod diff_filter;
mod diff_index;
mod diff_optval;
mod diff_pairs;
mod diff_pickaxe;
pub(crate) mod diffstat;
mod diff_tree;
/// The diffcore rename/copy/break passes shared by the diff-producing commands.
mod diffcore_rename;
mod difftool;
#[allow(non_snake_case)] // maps to git's `difftool--helper` subcommand
mod difftool__helper;
mod fast_export;
mod fast_import;
mod fetch;
mod fetch_pack;
pub(crate) mod filespec;
mod filter_branch;
mod fmt_merge_msg;
mod for_each_ref;
mod for_each_repo;
mod format_patch;
mod format_rev;
mod fsck;
mod fsck_objects;
#[allow(non_snake_case)] // maps to git's `fsmonitor--daemon` subcommand
mod fsmonitor__daemon;
mod gc;
mod get_tar_commit_id;
mod grep;
mod hash_object;
// Public: `git --info-path` and the HTML documentation set read its
// command-list tables (`help::topics`, `help::info_dir`).
pub mod help;
mod history;
mod hook;
mod http_backend;
mod http_fetch;
mod http_push;
mod imap_send;
mod index_pack;
mod init;
mod init_db;
mod instaweb;
mod interpret_trailers;
mod jump;
mod last_modified;
mod line_log;
pub(crate) mod log;
mod ls_files;
mod ls_remote;
mod ls_tree;
mod mailinfo;
mod mailsplit;
mod maintenance;
mod merge;
mod merge_base;
mod merge_file;
mod merge_index;
mod merge_octopus;
mod merge_one_file;
mod merge_ours;
mod merge_recursive;
mod merge_recursive_ours;
mod merge_recursive_theirs;
mod merge_resolve;
mod merge_subtree;
mod merge_tree;
mod man_viewer;
mod mergetool;
mod mktag;
mod mktree;
mod multi_pack_index;
mod mv;
mod name_rev;
mod notes;
mod p4;
mod pack_objects;
mod pack_redundant;
mod pack_refs;
mod patch_id;
mod pickaxe;
mod prune;
mod prune_packed;
mod pull;
mod push;
pub(crate) mod diff_no_index;
pub(crate) mod push_proto;
mod quiltimport;
mod range_diff;
mod read_tree;
mod rebase;
mod rebase_todo;
mod receive_pack;
mod ref_filter;
pub(crate) mod reflog;
mod refs;
mod remote;
mod remote_ext;
mod remote_fd;
mod remote_ftp;
mod remote_ftps;
mod remote_http;
mod remote_https;
mod repack;
mod replace;
mod replay;
mod replay_commit;
mod repo;
mod request_pull;
mod rerere;
mod reset;
mod restore;
mod rev_list;
pub(crate) mod rev_parse;
mod revert;
mod rm;
mod send_email;
mod send_pack;
#[allow(non_snake_case)] // maps to git's `sh-i18n--envsubst` subcommand
mod sh_i18n__envsubst;
mod shell;
mod shortlog;
/// git's history simplification (`revision.c`); not a subcommand.
mod simplify;
mod convert_to_git;
mod pretty_formats;
mod pretty_pad;
mod show;
mod show_branch;
mod show_index;
mod show_ref;
/// The `Net::SMTP` transport `send_email` sends over; not a subcommand.
mod smtp;
mod sparse_checkout;
mod stage;
mod stash;
mod status;
mod stripspace;
mod submodule;
#[allow(non_snake_case)] // maps to git's `submodule--helper` subcommand
mod submodule__helper;
mod subtree;
mod switch;
mod symbolic_ref;
mod tag;
mod unpack_file;
mod unpack_objects;
mod update_index;
mod update_ref;
mod update_server_info;
mod upload_archive;
#[allow(non_snake_case)] // maps to git's `upload-archive--writer` subcommand
mod upload_archive__writer;
mod upload_pack;
mod url_parse;
mod var;
mod verify_commit;
mod verify_pack;
mod verify_tag;
mod version;
#[allow(non_snake_case)] // maps to git's `web--browse` subcommand
mod web__browse;
mod whatchanged;
mod worktree;
pub(crate) mod worktree_filespec;
pub(crate) mod write_tree;

pub use add::add;
pub use am::am;
pub use annotate::annotate;
pub use apply::apply;
pub use archimport::archimport;
pub use archive::archive;
pub use backfill::backfill;
pub use bisect::bisect;
pub use blame::blame;
pub use branch::branch;
pub use bugreport::bugreport;
pub use bundle::bundle;
pub use cat_file::cat_file;
pub use check_attr::check_attr;
pub use check_ignore::check_ignore;
pub use check_mailmap::check_mailmap;
pub use check_ref_format::check_ref_format;
pub use checkout::checkout;
pub use checkout__worker::checkout__worker;
pub use checkout_index::checkout_index;
pub use cherry::cherry;
pub use cherry_pick::cherry_pick;
pub use clean::clean;
pub use clone::clone;
pub use column::column;
pub use commit::commit;
pub use commit_graph::commit_graph;
pub use commit_tree::commit_tree;
pub use config::config;
pub use count_objects::count_objects;
pub use credential::credential;
pub use credential_cache::credential_cache;
pub use credential_cache__daemon::credential_cache__daemon;
pub use credential_netrc::credential_netrc;
pub use credential_osxkeychain::credential_osxkeychain;
pub use credential_store::credential_store;
pub use cvsexportcommit::cvsexportcommit;
pub use cvsimport::cvsimport;
pub use cvsserver::cvsserver;
pub use daemon::daemon;
pub use describe::describe;
pub use diagnose::diagnose;
pub use diff::diff;
pub use diff_files::diff_files;
pub use diff_index::diff_index;
pub use diff_pairs::diff_pairs;
pub use diff_tree::diff_tree;
pub use difftool::difftool;
pub use difftool__helper::difftool__helper;
pub use fast_export::fast_export;
pub use fast_import::fast_import;
pub use fetch::fetch;
pub use fetch_pack::fetch_pack;
pub use filter_branch::filter_branch;
pub use fmt_merge_msg::fmt_merge_msg;
pub use for_each_ref::for_each_ref;
pub use for_each_repo::for_each_repo;
pub use format_patch::format_patch;
pub use format_rev::format_rev;
pub use fsck::fsck;
pub use fsck_objects::fsck_objects;
pub use fsmonitor__daemon::fsmonitor__daemon;
pub use gc::gc;
pub use get_tar_commit_id::get_tar_commit_id;
pub use grep::grep;
pub use hash_object::hash_object;
pub use help::help;
pub use history::history;
pub use hook::hook;
pub use http_backend::http_backend;
pub use http_fetch::http_fetch;
pub use http_push::http_push;
pub use imap_send::imap_send;
pub use index_pack::index_pack;
pub use init::init;
pub use init_db::init_db;
pub use instaweb::instaweb;
pub use interpret_trailers::interpret_trailers;
pub use jump::jump;
pub use last_modified::last_modified;
pub use log::log;
/// Ledger warming for the daemon: the log caches for a repo's newest commits.
pub use log::warm_caches as warm_log_caches;
pub use ls_files::ls_files;
pub use ls_remote::ls_remote;
pub use ls_tree::ls_tree;
pub use mailinfo::mailinfo;
pub use mailsplit::mailsplit;
pub use maintenance::maintenance;
pub use merge::merge;
pub use merge_base::merge_base;
pub use merge_file::merge_file;
pub use merge_index::merge_index;
pub use merge_octopus::merge_octopus;
pub use merge_one_file::merge_one_file;
pub use merge_ours::merge_ours;
pub use merge_recursive::merge_recursive;
pub use merge_recursive_ours::merge_recursive_ours;
pub use merge_recursive_theirs::merge_recursive_theirs;
pub use merge_resolve::merge_resolve;
pub use merge_subtree::merge_subtree;
pub use merge_tree::merge_tree;
pub use mergetool::mergetool;
pub use mktag::mktag;
pub use mktree::mktree;
pub use multi_pack_index::multi_pack_index;
pub use mv::mv;
pub use name_rev::name_rev;
pub use notes::notes;
pub use p4::p4;
pub use pack_objects::pack_objects;
pub use pack_redundant::pack_redundant;
pub use pack_refs::pack_refs;
pub use patch_id::patch_id;
pub use pickaxe::pickaxe;
pub use prune::prune;
pub use prune_packed::prune_packed;
pub use pull::pull;
pub use push::push;
pub use quiltimport::quiltimport;
pub use range_diff::range_diff;
pub use read_tree::read_tree;
pub use rebase::rebase;
pub use receive_pack::receive_pack;
pub use reflog::{reflog, reflog_show_as_log, reflog_show_as_log_status};
pub use refs::refs;
pub use remote::remote;
pub use remote_ext::remote_ext;
pub use remote_fd::remote_fd;
pub use remote_ftp::remote_ftp;
pub use remote_ftps::remote_ftps;
pub use remote_http::remote_http;
pub use remote_https::remote_https;
pub use repack::repack;
pub use replace::replace;
pub use replay::replay;
pub use repo::repo;
pub use request_pull::request_pull;
pub use rerere::rerere;
pub use reset::reset;
pub use restore::restore;
pub use rev_list::rev_list;
pub use rev_parse::rev_parse;
pub use revert::revert;
pub use rm::rm;
pub use send_email::send_email;
pub use send_pack::send_pack;
pub use sh_i18n__envsubst::sh_i18n__envsubst;
pub use shell::shell;
pub use shortlog::shortlog;
pub use show::show;
pub use show_branch::show_branch;
pub use show_index::show_index;
pub use show_ref::show_ref;
pub use sparse_checkout::sparse_checkout;
pub use stage::stage;
pub use stash::stash;
pub use status::status;
pub use stripspace::stripspace;
pub use submodule::submodule;
pub use submodule__helper::submodule__helper;
pub use subtree::subtree;
pub use switch::switch;
pub use symbolic_ref::symbolic_ref;
pub use tag::tag;
pub use unpack_file::unpack_file;
pub use unpack_objects::unpack_objects;
pub use update_index::update_index;
pub use update_ref::update_ref;
pub use update_server_info::update_server_info;
pub use upload_archive::upload_archive;
pub use upload_archive__writer::upload_archive__writer;
pub use upload_pack::upload_pack;
pub use url_parse::url_parse;
pub use var::var;
pub use verify_commit::verify_commit;
pub use verify_pack::verify_pack;
pub use verify_tag::verify_tag;
pub use version::version;
pub use web__browse::web__browse;
pub use whatchanged::whatchanged;
pub use worktree::worktree;
pub use write_tree::write_tree;

#[cfg(test)]
mod tests {
    use super::{canonical_long, resolve_long, Arg, Long, LongOpt, Resolved};

    /// A table with one of every shape `parse_long_opt()` treats differently: a
    /// plain negatable flag, a `PARSE_OPT_NONEG` flag, two entries sharing a
    /// prefix, and a `no-`-named entry (which is the *unset* sense of its stem,
    /// not a separate name).
    const TABLE: &[LongOpt] = &[
        LongOpt { name: "verbose", neg: true, arg: Arg::None },
        LongOpt { name: "version", neg: true, arg: Arg::None },
        LongOpt { name: "quiet", neg: false, arg: Arg::None },
        LongOpt { name: "no-index", neg: true, arg: Arg::None },
        LongOpt { name: "sort", neg: true, arg: Arg::Required },
    ];

    fn r(name: &str) -> String {
        match resolve_long(TABLE, name) {
            Resolved::One(opt, false) => opt.name.to_string(),
            Resolved::One(opt, true) => format!("unset:{}", opt.name),
            Resolved::Ambiguous(a, b) => format!("ambiguous:{a}|{b}"),
            Resolved::Unknown => "unknown".to_string(),
        }
    }

    fn c(tok: &str) -> String {
        match canonical_long(tok, TABLE) {
            Long::Name(name) => name.into_owned(),
            Long::Ambiguous(a, b) => format!("ambiguous:{a}|{b}"),
        }
    }

    /// The abbreviation rule itself: any unique prefix resolves, a shared one
    /// does not, and the full name always wins over a prefix of a longer one.
    #[test]
    fn a_unique_prefix_resolves_and_a_shared_one_does_not() {
        assert_eq!(r("sor"), "sort");
        assert_eq!(r("s"), "sort");
        assert_eq!(r("verbose"), "verbose");
        assert_eq!(r("versio"), "version");
        // Both `verbose` and `version` start with `ver`.
        assert_eq!(r("ver"), "ambiguous:verbose|version");
        assert_eq!(r("zz"), "unknown");
    }

    /// `PARSE_OPT_NONEG` is the only thing that removes the automatic `--no-`
    /// spelling, and it removes it from the abbreviations too.
    #[test]
    fn noneg_entries_have_no_negation() {
        assert_eq!(r("no-sort"), "unset:sort");
        assert_eq!(r("no-so"), "unset:sort");
        assert_eq!(r("quiet"), "quiet");
        assert_eq!(r("no-quiet"), "unknown");
        assert_eq!(r("no-qui"), "unknown");
    }

    /// `starts_with("no-", arg)` asks whether *`arg` is a prefix of `"no-"`*, so
    /// `--n` names every negatable entry at once and reports the last two in
    /// table order. The `no-index` entry is registered in the sense that spells
    /// itself, so it appears as `--no-index` rather than `--no-no-index`.
    #[test]
    fn a_bare_n_is_an_abbreviation_of_every_negation() {
        assert_eq!(r("n"), "ambiguous:no-index|no-sort");
        assert_eq!(r("no"), "ambiguous:no-index|no-sort");
    }

    /// A table entry whose own name starts with `no-` is the unset sense of its
    /// stem: `--index` is that entry, not a missing one, and the canonical
    /// spellings of the two senses are crossed.
    #[test]
    fn a_no_named_entry_owns_both_spellings() {
        assert_eq!(r("no-index"), "no-index");
        assert_eq!(r("index"), "unset:no-index");
        assert_eq!(c("--no-ind"), "--no-index");
        assert_eq!(c("--ind"), "--index");
    }

    /// What the verbs actually consume: the token respelled, with any attached
    /// value carried across, and anything unresolvable left exactly as typed so
    /// the command's own `unknown option` still quotes what the user wrote.
    #[test]
    fn canonicalization_rewrites_only_what_it_resolves() {
        assert_eq!(c("--sor=x"), "--sort=x");
        assert_eq!(c("--no-verb"), "--no-verbose");
        assert_eq!(c("--sort"), "--sort");
        assert_eq!(c("--zz"), "--zz");
        assert_eq!(c("--ver"), "ambiguous:verbose|version");
        // Not long options at all.
        assert_eq!(c("--"), "--");
        assert_eq!(c("--end-of-options"), "--end-of-options");
        assert_eq!(c("-s"), "-s");
        assert_eq!(c("sort"), "sort");
    }

    /// `is_alias()`: two entries joined by an `OPT_ALIAS()` do not make each
    /// other ambiguous, so a prefix that names both still resolves.
    ///
    /// This is `git clone`'s `OPT_ALIAS(0, "recursive", "recurse-submodules")`.
    /// Verified against stock 2.55.0: `git clone --recur` gets as far as
    /// `fatal: You must specify a repository to clone.` rather than reporting an
    /// ambiguity, even though both names start with `recur`.
    #[test]
    fn an_alias_pair_is_not_ambiguous_with_itself() {
        const ALIASED: &[LongOpt] = &[
            LongOpt { name: "recurse-submodules", neg: true, arg: Arg::Optional },
            LongOpt { name: "recursive", neg: true, arg: Arg::Optional },
        ];
        let groups: &[&[&str]] = &[&["recursive", "recurse-submodules"]];

        let describe = |res| match res {
            Resolved::One(opt, _) => format!("one:{}", opt.name),
            Resolved::Ambiguous(a, b) => format!("ambiguous:{a}|{b}"),
            Resolved::Unknown => "unknown".to_string(),
        };

        assert_eq!(
            describe(super::resolve_long(ALIASED, "recur")),
            "ambiguous:recurse-submodules|recursive"
        );
        assert_eq!(
            describe(super::resolve_long_aliased(ALIASED, groups, "recur")),
            "one:recursive"
        );
        // An alias group only covers its own pair; `--r` here is the same two
        // names, so it resolves too, but a third unrelated entry would not.
        assert_eq!(
            describe(super::resolve_long_aliased(ALIASED, groups, "recursi")),
            "one:recursive"
        );
    }

    /// The shipped verb tables keep their `PARSE_OPT_NONEG` entries un-negatable.
    ///
    /// This is the guard for a bug that shipped four times over: a command whose
    /// own parser hand-rolled the `no-` split (or simply listed a `--no-<x>` arm)
    /// accepted a negation stock git has never had, because `PARSE_OPT_NONEG`
    /// lives in the C table and nothing in the Rust arm list recorded it. Each
    /// name below was verified against stock 2.55.0, which answers every one of
    /// them with `error: unknown option` and exit 129:
    ///
    /// * `git rebase --no-abort` — the nine `OPT_CMDMODE`s (builtin/rebase.c)
    /// * `git show-branch --no-reflog` — `PARSE_OPT_OPTARG | PARSE_OPT_NONEG`
    /// * `git range-diff --no-binary` — `diff.c`'s `--binary` callback
    /// * `git fetch --no-refetch` — `OPT_SET_INT_F(... PARSE_OPT_NONEG)`
    /// * `git config --no-get` — fourteen more `OPT_CMDMODE`s, plus the six
    ///   `OPT_CALLBACK_VALUE` type spellings (`PARSE_OPT_NOARG|PARSE_OPT_NONEG`,
    ///   builtin/config.c:140-149)
    /// * `git checkout --no-ours` / `--no-unified` — `OPT_SET_INT_F` and
    ///   `OPT_DIFF_UNIFIED`, which is `OPT_INTEGER_F(..., PARSE_OPT_NONEG)`
    /// * `git stash push --no-unified` — the same two, per sub-command table
    /// * `git multi-pack-index repack --no-batch-size` — `OPT_UNSIGNED`, whose
    ///   macro carries `PARSE_OPT_NONEG` outright (parse-options.h:287-296)
    ///
    /// The positive spelling of each is asserted alongside it, so a table that
    /// went missing entirely could not pass this by resolving nothing at all.
    #[test]
    fn noneg_entries_of_the_shipped_tables_have_no_negation() {
        let cases: &[(&str, &[LongOpt], &str)] = &[
            ("rebase", super::rebase::LONG_OPTS, "abort"),
            ("rebase", super::rebase::LONG_OPTS, "continue"),
            ("rebase", super::rebase::LONG_OPTS, "edit-todo"),
            ("show-branch", super::show_branch::LONG_OPTS, "reflog"),
            ("range-diff", super::range_diff::LONG_OPTS, "binary"),
            ("range-diff", super::range_diff::LONG_OPTS, "name-only"),
            ("fetch", super::fetch::LONG_OPTS, "refetch"),
            ("fetch", super::fetch::LONG_OPTS, "unshallow"),
            // `config`'s legacy action table: `OPT_CMDMODE`s and the
            // `OPT_CALLBACK_VALUE` type spellings.
            ("config", super::config::ACTION_OPTS, "get"),
            ("config", super::config::ACTION_OPTS, "get-urlmatch"),
            ("config", super::config::ACTION_OPTS, "replace-all"),
            ("config", super::config::ACTION_OPTS, "unset-all"),
            ("config", super::config::ACTION_OPTS, "rename-section"),
            ("config", super::config::ACTION_OPTS, "remove-section"),
            ("config", super::config::ACTION_OPTS, "list"),
            ("config", super::config::ACTION_OPTS, "edit"),
            ("config", super::config::ACTION_OPTS, "get-color"),
            ("config", super::config::ACTION_OPTS, "get-colorbool"),
            ("config", super::config::ACTION_OPTS, "bool-or-int"),
            ("config", super::config::ACTION_OPTS, "bool-or-str"),
            ("config", super::config::ACTION_OPTS, "expiry-date"),
            ("checkout", super::checkout::LONG_OPTS, "ours"),
            ("checkout", super::checkout::LONG_OPTS, "theirs"),
            ("checkout", super::checkout::LONG_OPTS, "unified"),
            ("checkout", super::checkout::LONG_OPTS, "inter-hunk-context"),
            ("stash push", super::stash::PUSH_OPTS, "unified"),
            ("stash push", super::stash::PUSH_OPTS, "inter-hunk-context"),
            ("stash save", super::stash::SAVE_OPTS, "unified"),
            ("multi-pack-index repack", super::multi_pack_index::REPACK_OPTS, "batch-size"),
            // `OPT_CALLBACK_F(..., PARSE_OPT_NONEG, ...)` in builtin/mailinfo.c.
            ("mailinfo", super::mailinfo::LONG_OPTS, "encoding"),
            ("mailinfo", super::mailinfo::LONG_OPTS, "quoted-cr"),
            // `--parse` is `PARSE_OPT_NOARG | PARSE_OPT_NONEG`
            // (builtin/interpret-trailers.c).
            ("interpret-trailers", super::interpret_trailers::LONG_OPTS, "parse"),
            // builtin/read-tree.c: `--index-output` and `--exclude-per-directory`
            // are `OPT_CALLBACK_F(..., PARSE_OPT_NONEG, ...)` and `--prefix` is a
            // designated-initializer `OPTION_STRING` carrying the same flag.
            ("read-tree", super::read_tree::LONG_OPTS, "index-output"),
            ("read-tree", super::read_tree::LONG_OPTS, "prefix"),
            ("read-tree", super::read_tree::LONG_OPTS, "exclude-per-directory"),
        ];
        for &(verb, table, name) in cases {
            assert!(
                matches!(resolve_long(table, name), Resolved::One(..)),
                "{verb}: --{name} should resolve"
            );
            assert!(
                matches!(resolve_long(table, &format!("no-{name}")), Resolved::Unknown),
                "{verb}: --no-{name} must not resolve (the entry is PARSE_OPT_NONEG)"
            );
        }
    }

    /// A `no-`-named `PARSE_OPT_NONEG` entry is reachable in exactly one sense,
    /// and it is the one that spells itself.
    ///
    /// `for-each-ref` is where the two rules meet: `OPT_MERGED`/`OPT_NO_MERGED`
    /// and `OPT_CONTAINS`/`OPT_NO_CONTAINS` put `merged` and `no-merged` in the
    /// same table, both `PARSE_OPT_NONEG`. So `--no-merged` is *not* the
    /// negation of `--merged` — it is its own entry, selected positively — while
    /// the unset sense of either is unreachable. Reading `--no-merged` as a
    /// negation would silently invert the filter, and reading it as unknown
    /// would refuse an option git accepts.
    ///
    /// This is also what the value rule in `for_each_ref` leans on: it refuses
    /// an attached value whenever the *unset* sense was selected, which is only
    /// sound if these four never report as unset.
    #[test]
    fn a_no_named_noneg_entry_is_reachable_only_in_its_own_sense() {
        let table = super::for_each_ref::LONG_OPTS;
        for (typed, want) in [
            ("merged", "merged"),
            ("no-merged", "no-merged"),
            ("contains", "contains"),
            ("no-contains", "no-contains"),
        ] {
            match resolve_long(table, typed) {
                Resolved::One(opt, unset) => {
                    assert_eq!(opt.name, want, "--{typed} should be the {want} entry");
                    assert!(!unset, "--{typed} is PARSE_OPT_NONEG: it cannot be unset");
                }
                _ => panic!("--{typed} should resolve to exactly one entry"),
            }
        }
        // The spelling that would be the negation of a `no-`-named entry is the
        // one `PARSE_OPT_NONEG` removes.
        for typed in ["no-no-merged", "no-no-contains"] {
            assert!(
                matches!(resolve_long(table, typed), Resolved::Unknown),
                "--{typed} must not resolve"
            );
        }
    }

    /// `show_usage_if_asked()` fires on a lone `-h` and on nothing else.
    ///
    /// This is the one rule that separates the two `-h` families, and getting it
    /// backwards is silent: the command still prints a plausible usage block, on
    /// the wrong stream, for command lines where stock git prints something else
    /// entirely. `git diff-tree -h HEAD` and `git var -h GIT_EDITOR` both used to
    /// answer with help on stdout where stock answers on stderr, because the
    /// `argc == 2` guard had been dropped.
    #[test]
    fn lone_dash_h_is_the_whole_predicate() {
        let args = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();

        assert!(super::show_usage_if_asked(&args(&["-h"]), "usage: x\n").is_some());

        for line in [
            vec!["-h", "HEAD"],   // a trailing operand
            vec!["--cached", "-h"], // an option ahead of it
            vec!["-h", "-h"],     // still not `argc == 2`
            vec!["--help"],       // rewritten to `git help <cmd>` by the dispatcher
            vec!["-help"],
            vec!["h"],
            vec![],
        ] {
            assert!(
                super::show_usage_if_asked(&args(&line), "usage: x\n").is_none(),
                "{line:?} must not be read as a lone -h"
            );
        }
    }

    /// `show_usage_with_options_if_asked()` reads `--help-all` under the same
    /// `ac == 2` guard as `-h`, and renders the *other* block for it.
    ///
    /// Both halves have been wrong in this port before. The guard is easy to
    /// widen by accident, because `--help-all` fires anywhere for the
    /// `parse_options()` family and only here for this one; and the two blocks
    /// are easy to cross, because for 101 of git's 143 subcommands they are the
    /// same bytes, so a swap is invisible until someone tries one of the other
    /// 42.
    #[test]
    fn help_all_shares_the_lone_argument_guard_but_not_the_block() {
        let args = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let (normal, full) = ("usage: x\n", "usage: x\n    --hidden\n");

        assert!(super::show_usage_if_asked_full(&args(&["--help-all"]), normal, full).is_some());
        assert!(super::show_usage_if_asked_full(&args(&["-h"]), normal, full).is_some());

        for line in [
            vec!["--help-all", "HEAD"],  // a trailing operand
            vec!["--cached", "--help-all"], // an option ahead of it
            vec!["--help-all", "-h"],    // still not `argc == 2`
            vec!["--help-a"],            // `strcmp`, so no abbreviation
            vec!["--help-all=x"],        // `strcmp`, so no attached value
            vec!["--no-help-all"],
            vec!["help-all"],
        ] {
            assert!(
                super::show_usage_if_asked_full(&args(&line), normal, full).is_none(),
                "{line:?} must not be read as a lone --help-all"
            );
        }

        // The one-block wrapper is not a `-h`-only shortcut: the 101 commands
        // whose two blocks are the same bytes still have to answer both
        // spellings, and they reach that through this call.
        assert!(super::show_usage_if_asked(&args(&["--help-all"]), normal).is_some());
    }

    /// `USAGE_FULL` *inserts* the `PARSE_OPT_HIDDEN` rows into `USAGE_NORMAL`;
    /// it never edits or drops one.
    ///
    /// `usage_with_options_internal()` walks one option table and skips hidden
    /// entries unless `full`, so the two renderings agree line for line
    /// everywhere else. Keeping the pair as two captured strings is what makes
    /// them able to disagree, and this is the disagreement that would go
    /// unnoticed: a wording fix applied to one block and not the other still
    /// prints a plausible usage text for whichever spelling was missed.
    ///
    /// The counts are git 2.55.0's, the version this port reproduces; the six
    /// verbs here are the ones with the most hidden rows to lose.
    #[test]
    fn help_all_blocks_are_their_normal_block_plus_hidden_rows() {
        let cat_file_usage = super::cat_file::USAGE_HEAD.to_string()
            + super::cat_file::USAGE_TAIL;
        let cat_file_usage_all = super::cat_file::USAGE_HEAD_ALL.to_string()
            + super::cat_file::USAGE_TAIL_ALL;
        let cases: [(&str, &str, &str, usize); 7] = [
            ("apply", super::apply::USAGE, super::apply::USAGE_ALL, 3),
            ("branch", super::branch::USAGE, super::branch::USAGE_ALL, 3),
            ("cat-file", &cat_file_usage, &cat_file_usage_all, 3),
            ("commit", super::commit::USAGE, super::commit::USAGE_ALL, 3),
            ("fetch", super::fetch::USAGE, super::fetch::USAGE_ALL, 4),
            ("log", super::log::USAGE, super::log::USAGE_ALL, 2),
            ("rebase", super::rebase::USAGE, super::rebase::USAGE_ALL, 6),
        ];

        for (verb, normal, full, hidden) in cases {
            let mut remaining = full.lines();
            for line in normal.lines() {
                assert!(
                    remaining.any(|candidate| candidate == line),
                    "{verb}: --help-all dropped or reworded a -h line: {line:?}"
                );
            }
            assert_eq!(
                full.lines().count(),
                normal.lines().count() + hidden,
                "{verb}: --help-all should add exactly {hidden} hidden lines"
            );
        }
    }

    /// [`super::asks_for_help`] stops at the first short option the table does
    /// not define, and the answer depends on which table is asking.
    ///
    /// Every expectation below is stock git 2.55.0's, measured. The pair that
    /// makes the rule visible is `-nh`: `git reflog expire -nh` prints the expire
    /// block on stdout because `-n` is `expire`'s `OPT_BIT`, while
    /// `git reflog list -nh` prints ``error: unknown switch `n'`` on stderr
    /// because `list`'s table is `OPT_END()` alone and `parse_short_opt()` never
    /// reaches the `h`. Collapsing this to `tok == "-h"` silently loses the first
    /// case; collapsing it to "contains an h" silently steals the second.
    #[test]
    fn a_cluster_asks_for_help_only_where_the_table_runs_out_at_h() {
        // `shorts` is the argument-less short set, which is why `-t` (an
        // `OPT_STRING`) is absent from `remote add`'s: it swallows the `h`.
        let cases: [(&str, &str, bool); 12] = [
            ("-h", "", true),
            ("--help-all", "", true),
            ("-nh", "n", true),   // reflog expire: -n is its own
            ("-nh", "", false),   // reflog list: unknown switch `n'
            ("-habc", "", true),  // reflog show: h is still first
            ("-th", "f", false),  // remote add: -t takes "h" as its value
            ("-n", "n", false),
            ("--help-al", "", false),
            ("--help-all=x", "", false),
            ("--no-help-all", "", false),
            ("-", "", false),
            ("h", "", false),
        ];
        for (tok, shorts, want) in cases {
            assert_eq!(
                super::asks_for_help(tok, shorts),
                want,
                "asks_for_help({tok:?}, shorts={shorts:?})"
            );
        }
    }
}
