//! git-compatible porcelain, served natively via the vendored gitoxide
//! crates. One module per subcommand, ported incrementally. Each is the
//! drop-in equivalent of the stock `git` subcommand of the same name, so
//! tools on PATH (RustRover, gh, cargo) see identical behavior against the
//! same on-disk `.git`.

/// `parse-options`' own "this option needed a value" refusal: the option named
/// the way it was typed (`option \`depth'` for a long one, `switch \`j'` for a
/// short one), one line on stderr, exit 129.
///
/// It is here rather than in one porcelain because every command that takes an
/// option value shares it — the message comes from `get_arg()` in
/// parse-options.c, not from any subcommand.
pub(crate) fn missing_option_value(key: &str) -> std::process::ExitCode {
    match key.strip_prefix("--") {
        Some(long) => eprintln!("error: option `{long}' requires a value"),
        None => eprintln!("error: switch `{}' requires a value", &key[1..]),
    }
    std::process::ExitCode::from(129)
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
        Resolved::One(opt, negated) => {
            let mut out = String::from("--");
            if negated {
                out.push_str("no-");
            }
            out.push_str(opt.name);
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
mod branch;
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
mod column;
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
mod diff_index;
mod diff_optval;
mod diff_pairs;
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
mod log;
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
mod reflog;
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
mod repo;
mod request_pull;
mod rerere;
mod reset;
mod restore;
mod rev_list;
mod rev_parse;
mod revert;
mod rm;
mod send_email;
mod send_pack;
#[allow(non_snake_case)] // maps to git's `sh-i18n--envsubst` subcommand
mod sh_i18n__envsubst;
mod shell;
mod shortlog;
mod convert_to_git;
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
mod write_tree;

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
pub use reflog::{reflog, reflog_show_as_log};
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
}
