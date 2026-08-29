use anyhow::{anyhow, bail, Result};
use std::io::Write as _;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::config::{File as ConfigFile, Source};
use gix::hash::ObjectId;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use super::{ref_filter, resolve_long, Arg, LongOpt, Resolved};

/// The SGR reset git emits after a colored branch name (`\e[m`, not `\e[0m`).
const RESET: &str = "\x1b[m";

/// The exact usage block stock git prints for an option-parsing error,
/// reproduced verbatim so `--list --show-current` and friends match byte for byte.
pub(super) const USAGE: &str = r#"usage: git branch [<options>] [-r | -a] [--merged] [--no-merged]
   or: git branch [<options>] [-f] [--recurse-submodules] <branch-name> [<start-point>]
   or: git branch [<options>] [-l] [<pattern>...]
   or: git branch [<options>] [-r] (-d | -D) <branch-name>...
   or: git branch [<options>] (-m | -M) [<old-branch>] <new-branch>
   or: git branch [<options>] (-c | -C) [<old-branch>] <new-branch>
   or: git branch [<options>] [-r | -a] [--points-at]
   or: git branch [<options>] [-r | -a] [--format]

Generic options
    -v, --[no-]verbose    show hash and subject, give twice for upstream branch
    -q, --[no-]quiet      suppress informational messages
    -t, --[no-]track[=(direct|inherit)]
                          set branch tracking configuration
    -u, --[no-]set-upstream-to <upstream>
                          change the upstream info
    --[no-]unset-upstream unset the upstream info
    --[no-]color[=<when>] use colored output
    -r, --remotes         act on remote-tracking branches
    --contains <commit>   print only branches that contain the commit
    --no-contains <commit>
                          print only branches that don't contain the commit
    --[no-]abbrev[=<n>]   use <n> digits to display object names

Specific git-branch actions:
    -a, --all             list both remote-tracking and local branches
    -d, --[no-]delete     delete fully merged branch
    -D                    delete branch (even if not merged)
    -m, --[no-]move       move/rename a branch and its reflog
    -M                    move/rename a branch, even if target exists
    --[no-]omit-empty     do not output a newline after empty formatted refs
    -c, --[no-]copy       copy a branch and its reflog
    -C                    copy a branch, even if target exists
    -l, --[no-]list       list branch names
    --[no-]show-current   show current branch name
    --[no-]create-reflog  create the branch's reflog
    --[no-]edit-description
                          edit the description for the branch
    -f, --[no-]force      force creation, move/rename, deletion
    --merged <commit>     print only branches that are merged
    --no-merged <commit>  print only branches that are not merged
    --[no-]column[=<style>]
                          list branches in columns
    --[no-]sort <key>     field name to sort on
    --[no-]points-at <object>
                          print only branches of the object
    -i, --[no-]ignore-case
                          sorting and filtering are case insensitive
    --[no-]recurse-submodules
                          recurse through submodules
    --[no-]format <format>
                          format to use for the output

"#;

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]set-upstream`, `--with`, `--without`.
/// Captured byte-for-byte from stock git 2.55.0's `git branch --help-all`.
pub(super) const USAGE_ALL: &str = r#"usage: git branch [<options>] [-r | -a] [--merged] [--no-merged]
   or: git branch [<options>] [-f] [--recurse-submodules] <branch-name> [<start-point>]
   or: git branch [<options>] [-l] [<pattern>...]
   or: git branch [<options>] [-r] (-d | -D) <branch-name>...
   or: git branch [<options>] (-m | -M) [<old-branch>] <new-branch>
   or: git branch [<options>] (-c | -C) [<old-branch>] <new-branch>
   or: git branch [<options>] [-r | -a] [--points-at]
   or: git branch [<options>] [-r | -a] [--format]

Generic options
    -v, --[no-]verbose    show hash and subject, give twice for upstream branch
    -q, --[no-]quiet      suppress informational messages
    -t, --[no-]track[=(direct|inherit)]
                          set branch tracking configuration
    --[no-]set-upstream   do not use
    -u, --[no-]set-upstream-to <upstream>
                          change the upstream info
    --[no-]unset-upstream unset the upstream info
    --[no-]color[=<when>] use colored output
    -r, --remotes         act on remote-tracking branches
    --contains <commit>   print only branches that contain the commit
    --no-contains <commit>
                          print only branches that don't contain the commit
    --with <commit>       print only branches that contain the commit
    --without <commit>    print only branches that don't contain the commit
    --[no-]abbrev[=<n>]   use <n> digits to display object names

Specific git-branch actions:
    -a, --all             list both remote-tracking and local branches
    -d, --[no-]delete     delete fully merged branch
    -D                    delete branch (even if not merged)
    -m, --[no-]move       move/rename a branch and its reflog
    -M                    move/rename a branch, even if target exists
    --[no-]omit-empty     do not output a newline after empty formatted refs
    -c, --[no-]copy       copy a branch and its reflog
    -C                    copy a branch, even if target exists
    -l, --[no-]list       list branch names
    --[no-]show-current   show current branch name
    --[no-]create-reflog  create the branch's reflog
    --[no-]edit-description
                          edit the description for the branch
    -f, --[no-]force      force creation, move/rename, deletion
    --merged <commit>     print only branches that are merged
    --no-merged <commit>  print only branches that are not merged
    --[no-]column[=<style>]
                          list branches in columns
    --[no-]sort <key>     field name to sort on
    --[no-]points-at <object>
                          print only branches of the object
    -i, --[no-]ignore-case
                          sorting and filtering are case insensitive
    --[no-]recurse-submodules
                          recurse through submodules
    --[no-]format <format>
                          format to use for the output

"#;

/// git's fatal error convention: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// git's non-fatal branch-operation convention: `error: <msg>` on stderr, exit 1.
/// `git branch -d` uses this (rather than 128) for a missing or unmerged branch.
fn error_exit(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(1))
}

/// git's option-parsing convention: the full usage block on stderr, exit 129.
///
/// This is a bare `usage_with_options()` — the shape `cmd_branch()` uses for its
/// own post-parse checks (`noncreate_actions > 1`, too many operands), which
/// carry no `error:` line of their own.
fn usage_exit() -> Result<ExitCode> {
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// `parse_options()`' rejection half: an `error:` line, then the same usage block
/// on **stderr**, exit 129.
///
/// ```c
/// case PARSE_OPT_UNKNOWN:
///         if (ctx.argv[0][1] == '-') {
///                 error(_("unknown option `%s'"), ctx.argv[0] + 2);
///         } else if (isascii(*ctx.opt)) {
///                 error(_("unknown switch `%c'"), *ctx.opt);
///         } else {
///                 error(_("unknown non-ascii option in string: `%s'"),
///                       ctx.argv[0]);
///         }
///         usage_with_options(usagestr, options);
/// ```
///
/// The stream is the whole difference from [`super::show_usage`], which serves
/// `-h` on stdout: asking for help is not an error, being rejected is.
fn usage_error(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// An option-value complaint with no usage block: `get_value()` and the option
/// callbacks `return error(...)`, which becomes `PARSE_OPT_ERROR` and a bare
/// `exit(129)` in `parse_options()` — nothing renders the usage block on that
/// path.
fn value_error(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(129))
}

/// [`super::ambiguous_option`] against `git branch`'s usage block: the
/// explanation on stderr, the block on stdout, exit 129. Verified against stock
/// 2.55.0, `git branch --col` → `error: ambiguous option: col (could be --color
/// or --column)`.
fn ambiguous_exit(body: &str, first: &str, second: &str) -> Result<ExitCode> {
    Ok(super::ambiguous_option(body, first, second, USAGE))
}

/// Every long option `git branch` resolves, **in `builtin/branch.c` table
/// order**.
///
/// The order is load-bearing twice over: `parse_long_opt()` walks the table and
/// keeps the last two abbreviation candidates, so reordering this array changes
/// which two names an `ambiguous option:` diagnostic reports. Verified against
/// stock 2.55.0: `--c` → `--create-reflog or --column`, `--col` → `--color or
/// --column`, `--s` → `--show-current or --sort`, `--wit` → `--with or
/// --without`, `--n` → `--no-recurse-submodules or --no-format`.
///
/// `set-upstream`, `with` and `without` are `PARSE_OPT_HIDDEN`: absent from the
/// usage block but resolved by the parser like any other, so they belong here.
/// `-D`, `-M` and `-C` have no long name and so have no entry.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "verbose", neg: true, arg: Arg::None },
    LongOpt { name: "quiet", neg: true, arg: Arg::None },
    LongOpt { name: "track", neg: true, arg: Arg::Optional },
    LongOpt { name: "set-upstream", neg: true, arg: Arg::None },
    LongOpt { name: "set-upstream-to", neg: true, arg: Arg::Required },
    LongOpt { name: "unset-upstream", neg: true, arg: Arg::None },
    LongOpt { name: "color", neg: true, arg: Arg::Optional },
    LongOpt { name: "remotes", neg: false, arg: Arg::None },
    LongOpt { name: "contains", neg: false, arg: Arg::LastArg },
    LongOpt { name: "no-contains", neg: false, arg: Arg::LastArg },
    LongOpt { name: "with", neg: false, arg: Arg::LastArg },
    LongOpt { name: "without", neg: false, arg: Arg::LastArg },
    LongOpt { name: "abbrev", neg: true, arg: Arg::Optional },
    LongOpt { name: "all", neg: false, arg: Arg::None },
    LongOpt { name: "delete", neg: true, arg: Arg::None },
    LongOpt { name: "move", neg: true, arg: Arg::None },
    LongOpt { name: "omit-empty", neg: true, arg: Arg::None },
    LongOpt { name: "copy", neg: true, arg: Arg::None },
    LongOpt { name: "list", neg: true, arg: Arg::None },
    LongOpt { name: "show-current", neg: true, arg: Arg::None },
    LongOpt { name: "create-reflog", neg: true, arg: Arg::None },
    LongOpt { name: "edit-description", neg: true, arg: Arg::None },
    LongOpt { name: "force", neg: true, arg: Arg::None },
    LongOpt { name: "merged", neg: false, arg: Arg::LastArg },
    LongOpt { name: "no-merged", neg: false, arg: Arg::LastArg },
    LongOpt { name: "column", neg: true, arg: Arg::Optional },
    LongOpt { name: "sort", neg: true, arg: Arg::Required },
    LongOpt { name: "points-at", neg: true, arg: Arg::Required },
    LongOpt { name: "ignore-case", neg: true, arg: Arg::None },
    LongOpt { name: "recurse-submodules", neg: true, arg: Arg::None },
    LongOpt { name: "format", neg: true, arg: Arg::Required },
];

/// The short options the loop in [`branch`] accepts; kept beside it so the two
/// stay in step. Only [`child_branch_option_rejection`] reads it — the loop
/// itself matches the letters directly.
const SHORT_OPTS: &str = "arlvqidDmMcCtfhu";

/// What a child `git branch <name> HEAD` does when `<name>` is option-shaped,
/// and so is parsed as an option instead of taken as the branch to create.
/// `Some(code)` means it refused (and this has printed the child's diagnosis).
///
/// `add_worktree()` creates the `-b` branch by *running `git branch`* in the new
/// worktree rather than calling into the branch code, so
/// `git worktree add -b --zzbogus <path>` is refused by that child process, not
/// by `worktree` itself — and stock leaves neither the ref nor the worktree
/// behind. Verified against stock 2.55.0, all with `<name>` untouched on disk:
///
/// | `-b <name>` | child sees | result |
/// |---|---|---|
/// | `--zzbogus` | no such option | ``unknown option `zzbogus'`` + usage, **255** |
/// | `-Z` | no such switch | ``unknown switch `Z'`` + usage, **255** |
/// | `--force`, `--no-verb` | a flag; `HEAD` is left as the branch to create | `fatal: 'HEAD' is not a valid branch name`, **255** |
/// | `--sort`, `--merged` | an option that eats `HEAD` as its value | `fatal: invalid reference: <name>`, **128** |
///
/// One documented gap in the last row: stock's child also prints its branch
/// listing on stdout (`* main`) before `worktree` reports the bad reference,
/// which is not reproduced here — the exit code, the stderr and the empty
/// post-state all match.
pub(crate) fn child_branch_option_rejection(
    repo: &gix::Repository,
    name: &str,
) -> Option<ExitCode> {
    /// The child was left with `HEAD` as its only positional.
    fn head_is_not_a_branch_name(repo: &gix::Repository) -> Option<ExitCode> {
        eprintln!("fatal: 'HEAD' is not a valid branch name");
        ref_syntax_hints(repo);
        Some(ExitCode::from(255))
    }
    /// The child ate `HEAD` as an option value, so no branch was ever created
    /// and `worktree` fails to resolve the one it asked for.
    fn invalid_reference(name: &str) -> Option<ExitCode> {
        eprintln!("fatal: invalid reference: {name}");
        Some(ExitCode::from(128))
    }

    let rest = name.strip_prefix('-')?;
    if rest.is_empty() {
        // A bare `-` is an operand to the child, and `check_branch_ref()`
        // refuses it for the leading dash like any other name.
        eprintln!("fatal: '-' is not a valid branch name");
        ref_syntax_hints(repo);
        return Some(ExitCode::from(255));
    }
    let Some(body) = rest.strip_prefix('-') else {
        // Short: the child reports the first letter of the bundle.
        let c = rest.chars().next().unwrap_or_default();
        return match SHORT_OPTS.contains(c) {
            // `-u` is the only short option taking a value.
            true if c == 'u' => invalid_reference(name),
            true => head_is_not_a_branch_name(repo),
            false => {
                let _ = match c.is_ascii() {
                    true => usage_error(format!("unknown switch `{c}'")),
                    false => usage_error(format!("unknown non-ascii option in string: `{name}'")),
                };
                Some(ExitCode::from(255))
            }
        };
    };
    if body.is_empty() {
        // `--` ends the child's options, leaving `HEAD` as its only operand.
        return head_is_not_a_branch_name(repo);
    }
    let head = body.split_once('=').map_or(body, |(n, _)| n);
    match resolve_long(LONG_OPTS, head) {
        Resolved::Unknown => {
            let _ = usage_error(format!("unknown option `{body}'"));
            Some(ExitCode::from(255))
        }
        Resolved::Ambiguous(first, second) => {
            let _ = ambiguous_exit(body, &first, &second);
            Some(ExitCode::from(255))
        }
        // A flag leaves `HEAD` as the branch to create; anything that takes a
        // value swallows it instead. An attached `=value` never reaches for the
        // next argument, so it leaves `HEAD` behind too.
        Resolved::One(opt, negated) => {
            match !negated && body.split_once('=').is_none() && opt.arg != Arg::None {
                true => invalid_reference(name),
                false => head_is_not_a_branch_name(repo),
            }
        }
    }
}

/// `check_branch_ref()` (refs.c): whether `name` may become `refs/heads/<name>`.
///
/// ```c
/// int check_branch_ref(struct strbuf *sb, const char *name)
/// {
///         ...
///         strbuf_splice(sb, 0, 0, "refs/heads/", 11);
///
///         if (*name == '-' ||
///             !strcmp(sb->buf, "refs/heads/HEAD"))
///                 return -1;
///
///         return check_refname_format(sb->buf, 0);
/// }
/// ```
///
/// The leading-dash rule lives *here*, not in `check_refname_format()`, which is
/// all `gix::validate::reference::branch_name()` implements — so a name like
/// `-foo` or `--bogus` passes the gitoxide check and has to be rejected
/// separately. Without it `git branch -- -foo` created `refs/heads/-foo` and
/// `git branch -m -- -bad` renamed the current branch to `-bad`.
pub(crate) fn valid_branch_name(name: &str) -> bool {
    let full = format!("refs/heads/{name}");
    !name.starts_with('-')
        && full != "refs/heads/HEAD"
        && gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_ok()
}

/// The child `git branch <name> <start>` refusing a name `check_branch_ref()` will not take:
/// `fatal: '<name>' is not a valid branch name` with the `advice.refSyntax` hints behind it.
///
/// `worktree add` reaches this through the same child it uses for every other `-b` refusal, and
/// `run_command()` returning non-zero is its `return -1` — which git reports as 255, not as the
/// child's own 128.
pub(crate) fn child_branch_invalid_name(repo: &gix::Repository, name: &str) -> Option<ExitCode> {
    if valid_branch_name(name) {
        return None;
    }
    eprintln!("fatal: '{name}' is not a valid branch name");
    ref_syntax_hints(repo);
    Some(ExitCode::from(255))
}

/// The `refSyntax` advice git prints after rejecting a branch name. git spells
/// this `advise_if_enabled(ADVICE_REF_SYNTAX, …)` (`builtin/branch.c`), so the
/// `Disable this message with …` trailer appears only while the slot is
/// unconfigured — setting `advice.refSyntax=true` keeps the hint and drops it.
fn ref_syntax_hints(repo: &gix::Repository) {
    crate::advice::Advice::RefSyntax.advise_in(repo, "See 'git help check-ref-format'");
}

/// Which ref namespace a listing covers. `-a`/`-r` are a single mode selector in
/// git's option table, so the last one on the command line wins.
#[derive(PartialEq, Eq, Clone, Copy)]
enum ListMode {
    Local,
    Remotes,
    All,
}

/// `-t`/`--track[=(direct|inherit)]` / `--no-track` selector, in git's option
/// order (the last one wins).
#[derive(PartialEq, Eq, Clone, Copy)]
enum Track {
    /// Neither `--track` nor `--no-track` given: auto-track per `branch.autoSetupMerge`.
    Unset,
    /// `--no-track`: never set up tracking.
    No,
    /// `-t` / `--track` / `--track=direct`: track the start-point's remote directly.
    Direct,
    /// `--track=inherit`: copy the start-point branch's own upstream configuration.
    Inherit,
    /// The hidden `--set-upstream`: `OPT_SET_INT_F(0, "set-upstream", &track,
    /// N_("do not use"), BRANCH_TRACK_OVERRIDE, PARSE_OPT_HIDDEN)`. Accepted by
    /// the parser, then refused by name in the creation path.
    Override,
}

/// `--color[=<when>]` tri-state, matching `git branch`'s default of `auto`.
#[derive(PartialEq, Eq, Clone, Copy)]
enum ColorWhen {
    Auto,
    Always,
    Never,
}

/// Parsed `git branch` command line.
struct Opts {
    mode: ListMode,
    show_current: bool,
    /// `-l`/`--list` was given explicitly, which forces list mode even with
    /// positionals (they become patterns rather than a branch to create).
    explicit_list: bool,
    verbose: u8,
    format: Option<String>,
    /// Raw `--sort=<key>` values in command-line order; an empty vec falls back
    /// to the multi-valued `branch.sort` config at listing time.
    sorts: Vec<String>,
    /// A `--no-sort` was seen. `OPT_REF_SORT` is an `OPT_STRING_LIST`, whose
    /// unset callback clears the *whole* list — including the `branch.sort`
    /// values `repo_config()` seeded into it before `parse_options()` ran, and
    /// including the implicit `refname` (builtin/branch.c:795-797). With the
    /// list empty, `ref_sorting_options()` returns NULL and `ref_array_sort()`
    /// is a no-op, which is the only way `git branch` prints refs unsorted.
    sort_cleared: bool,
    delete: bool,
    rename: bool,
    copy: bool,
    force: bool,
    quiet: bool,
    ignore_case: bool,
    create_reflog: bool,
    edit_description: bool,
    track: Track,
    /// `-u <up>` / `--set-upstream-to=<up>`: the upstream spec to install.
    set_upstream_to: Option<String>,
    unset_upstream: bool,
    color: ColorWhen,
    /// Column layout state (git's `colopts`), seeded from `column.ui`/`column.branch`
    /// and refined by `--column[=<opts>]` / `--no-column`.
    colopts: u32,
    /// `--abbrev=<n>` for `-v`: `None` = configured default, `Some(0)` = full hash.
    abbrev: Option<usize>,
    // Reachability filters (each entry is a raw rev spec, resolved at list time).
    contains: Vec<String>,
    no_contains: Vec<String>,
    merged: Vec<String>,
    no_merged: Vec<String>,
    points_at: Vec<String>,
    /// `--omit-empty`: drop a formatted line that rendered to nothing, rather
    /// than printing its bare newline (`format.array_opts.omit_empty`).
    omit_empty: bool,
    /// `--recurse-submodules`: git's `recurse_submodules_explicit`, which only
    /// means anything once `submodule.propagateBranches` is enabled.
    recurse_submodules: bool,
    names: Vec<String>,
}

impl Opts {
    /// Whether any reachability/points-at filter is present. git forces list mode
    /// when one is, so positionals become patterns rather than a branch to create.
    fn has_filter(&self) -> bool {
        !self.contains.is_empty()
            || !self.no_contains.is_empty()
            || !self.merged.is_empty()
            || !self.no_merged.is_empty()
            || !self.points_at.is_empty()
    }
}

/// Reachability filters resolved to concrete commit ids, as git's `ref-filter`
/// does before walking the ref list.
struct Filters {
    contains: Vec<ObjectId>,
    no_contains: Vec<ObjectId>,
    merged: Vec<ObjectId>,
    no_merged: Vec<ObjectId>,
    points_at: Vec<ObjectId>,
}

impl Filters {
    /// The four reachability filters in the shape `ref-filter` takes them.
    /// `--points-at` is not one of them: `apply_ref_filter()` tests it against
    /// the ref's own id before any commit lookup happens.
    fn shared(&self) -> super::for_each_ref::Filters {
        super::for_each_ref::Filters {
            contains: self.contains.clone(),
            no_contains: self.no_contains.clone(),
            merged: self.merged.clone(),
            no_merged: self.no_merged.clone(),
        }
    }
}

/// `git branch` — list, create, copy, rename, and delete branches, backed by the
/// vendored gitoxide ref store.
///
/// Implemented: listing (`-a`/`--all`, `-r`/`--remotes`, `-v`/`-vv`,
/// `--format=<fmt>`, `-l`/`--list` with optional glob patterns),
/// `--sort=[-][version:]<field>` (multi-level, defaulting to the multi-valued
/// `branch.sort` config), `--show-current`, creation at an optional
/// `<start-point>` with `-t`/`--track[=(direct|inherit)]`/`--no-track` upstream
/// setup, `-m`/`-M` rename and `-c`/`-C` copy (both carrying the reflog and the
/// `branch.<name>.*` config across), `-d`/`-D` delete, `-u`/`--set-upstream-to`
/// and `--unset-upstream`, the `--contains`/`--no-contains`/`--merged`/
/// `--no-merged`/`--points-at` reachability filters, `--abbrev[=<n>]`/
/// `--no-abbrev`, `-i`/`--ignore-case`, `-q`/`--quiet`, `--color[=<when>]`/
/// `--no-color`, `--create-reflog`, `--omit-empty`, and `--column[=<opts>]`/
/// `--no-column` (honoring `column.ui`/`column.branch`, resolving `auto` against
/// the terminal, and mutually exclusive with `-v`/`--verbose`).
///
/// Option parsing goes through [`LONG_OPTS`] and [`resolve_long`], which
/// reproduce `parse_long_opt()`: unique-prefix abbreviations (`--verb`), the
/// automatic `--no-` negations for every entry without `PARSE_OPT_NONEG`, the
/// `PARSE_OPT_HIDDEN` entries (`--set-upstream`, `--with`, `--without`), and
/// `--end-of-options`. A name no entry claims is `error: unknown option
/// '<name>'` plus the usage block on stderr at 129, *before* anything is
/// created — it is not taken as a branch to create.
///
/// Listing is the same `ref-filter.c` run `for-each-ref` and `tag --list` are,
/// driven through [`super::ref_filter`]. `git branch` has no listing code of its
/// own beyond deciding *which* refs to ask for and building a format string when
/// the user gave none — `print_ref_list()` (builtin/branch.c:445-502) filters,
/// sizes the name column, calls [`build_format`], and hands the result to the
/// shared evaluator. So every decoration this command is known for — the `* `
/// marker, the `+ ` worktree marker, the color slots, the padded name column
/// under `-v`, the ` -> <symref>` tail, the `-vv` upstream and ahead/behind
/// fields — is an ordinary atom, and `--format` replaces all of them at once
/// rather than composing with any of them. `--column` is orthogonal and applies
/// to whichever format ran, but is refused with `-v`/`-vv`
/// (builtin/branch.c:842-847).
///
/// Two things `git branch` does *not* inherit from `filter_and_format_refs()`:
/// `--shell`/`--perl`/`--python`/`--tcl` (its option table has no `OPT_QUOTING`,
/// so they are `error: unknown option`), and `filter_is_base()`, which
/// `print_ref_list()` simply never calls — so `%(is-base:<x>)` is empty for
/// every branch even where `git for-each-ref` marks one.
///
/// `--edit-description` is refused: it needs an interactive
/// editor loop that is not wired in this environment. `--recurse-submodules`
/// reproduces both of git's refusals (`submodule.propagateBranches` unset, and
/// the non-creation actions) and then says it is not ported, rather than
/// claiming the flag is unknown.
///
/// The merge check for `-d` uses reachability from HEAD only (not a configured
/// upstream), which is git's behavior when no upstream is set.
pub fn branch(args: &[String]) -> Result<ExitCode> {
    let mut o = Opts {
        mode: ListMode::Local,
        show_current: false,
        explicit_list: false,
        verbose: 0,
        format: None,
        sorts: Vec::new(),
        sort_cleared: false,
        delete: false,
        rename: false,
        copy: false,
        force: false,
        quiet: false,
        ignore_case: false,
        create_reflog: false,
        edit_description: false,
        track: Track::Unset,
        set_upstream_to: None,
        unset_upstream: false,
        color: ColorWhen::Auto,
        colopts: super::column::DISABLED,
        abbrev: None,
        contains: Vec::new(),
        no_contains: Vec::new(),
        merged: Vec::new(),
        no_merged: Vec::new(),
        points_at: Vec::new(),
        omit_empty: false,
        recurse_submodules: false,
        names: Vec::new(),
    };

    // git seeds `colopts` from `column.ui` / `column.branch` while reading config,
    // before the command line is parsed, so a `--column` flag overrides the config.
    if let Err(msg) = super::column::config_colopts(&mut o.colopts, "branch") {
        eprint!("{msg}");
        return Ok(ExitCode::from(128));
    }

    let mut i = 0;
    // `--` and `--end-of-options` both end option parsing; everything after is an
    // operand. parse_options drops the token itself in either case.
    let mut only_names = false;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            _ if only_names => o.names.push(a.to_string()),
            "--" | "--end-of-options" => only_names = true,
            // `if (internal_help && !strcmp(arg + 2, "help-all"))`
            // (parse-options.c:1122): an exact match tested right after those
            // two breaks and ahead of parse_long_opt(), so it never abbreviates
            // and never takes an `=<value>`. It renders `USAGE_FULL`, which for
            // `branch` keeps the hidden `--set-upstream`, `--with` and
            // `--without`.
            "--help-all" => return Ok(super::show_usage(USAGE_ALL)),
            // Every long option goes through the parse-options resolver, so an
            // abbreviation is honored and an unrecognized name is refused here
            // rather than silently becoming a branch to create.
            _ if a.starts_with("--") => {
                if let Some(code) = apply_long(&mut o, args, &mut i, &a[2..])? {
                    return Ok(code);
                }
            }
            // A single-dash argument is a bundle of short flags (`-vv`, `-dr`).
            _ if a.starts_with('-') && a.len() > 1 => {
                let flags = &a[1..];
                let bytes = flags.as_bytes();
                let mut ci = 0;
                while ci < bytes.len() {
                    match bytes[ci] as char {
                        'a' => o.mode = ListMode::All,
                        'r' => o.mode = ListMode::Remotes,
                        'l' => o.explicit_list = true,
                        'v' => o.verbose = o.verbose.saturating_add(1),
                        'q' => o.quiet = true,
                        'i' => o.ignore_case = true,
                        'd' => o.delete = true,
                        'D' => {
                            o.delete = true;
                            o.force = true;
                        }
                        'm' => o.rename = true,
                        'M' => {
                            o.rename = true;
                            o.force = true;
                        }
                        'c' => o.copy = true,
                        'C' => {
                            o.copy = true;
                            o.force = true;
                        }
                        't' => o.track = Track::Direct,
                        'f' => o.force = true,
                        // parse_options_step(): `if (internal_help && *ctx->opt
                        // == 'h')` is tested inside the short-option loop, so a
                        // clustered `-ah` answers with help too — and on stdout,
                        // not through `usage_exit()`'s stderr.
                        'h' => return Ok(super::show_usage(USAGE)),
                        // `-u` takes an upstream: the rest of this token, else the
                        // next argument.
                        'u' => {
                            let rest = &flags[ci + 1..];
                            let v = if rest.is_empty() {
                                i += 1;
                                match args.get(i) {
                                    Some(v) => v.clone(),
                                    None => return Ok(super::missing_option_value("-u")),
                                }
                            } else {
                                rest.to_string()
                            };
                            o.set_upstream_to = Some(v);
                            break;
                        }
                        // An unrecognized short option is `unknown switch` when
                        // it is ASCII, and otherwise the whole (possibly
                        // rewritten) argument:
                        //
                        //     } else if (isascii(*ctx.opt)) {
                        //             error(_("unknown switch `%c'"), *ctx.opt);
                        //     } else {
                        //             error(_("unknown non-ascii option in string: `%s'"),
                        //                   ctx.argv[0]);
                        //     }
                        //
                        // parse_options_step() rebuilds `argv[0]` as `-` plus the
                        // rest of the bundle for every switch after the first, so
                        // `git branch -vé` reports `-é`, not `-vé`.
                        c => {
                            return match c.is_ascii() {
                                true => usage_error(format!("unknown switch `{c}'")),
                                false => usage_error(format!(
                                    "unknown non-ascii option in string: `-{}'",
                                    &flags[ci..]
                                )),
                            }
                        }
                    }
                    ci += 1;
                }
            }
            _ => o.names.push(a.to_string()),
        }
        i += 1;
    }

    // git forces list mode when a reachability filter is present, so a positional
    // becomes a pattern rather than a branch to create.
    if o.has_filter() {
        o.explicit_list = true;
    }

    // ```c
    // if (recurse_submodules_explicit) {
    //         if (!submodule_propagate_branches)
    //                 die(_("branch with --recurse-submodules can only be used if submodule.propagateBranches is enabled"));
    //         if (noncreate_actions)
    //                 die(_("--recurse-submodules can only be used to create branches"));
    // }
    // ```
    //
    // Both refusals are reachable without any submodule machinery, so they are
    // reproduced exactly; only the recursive creation itself is missing, and it
    // says so rather than pretending the flag was unknown.
    if o.recurse_submodules {
        let propagate = gix::open(".")
            .ok()
            .and_then(|r| r.config_snapshot().boolean("submodule.propagateBranches"))
            .unwrap_or(false);
        if !propagate {
            return fatal(
                "branch with --recurse-submodules can only be used if \
                 submodule.propagateBranches is enabled",
            );
        }
        if o.delete
            || o.rename
            || o.copy
            || o.explicit_list
            || o.show_current
            || o.edit_description
            || o.unset_upstream
            || o.set_upstream_to.is_some()
        {
            return fatal("--recurse-submodules can only be used to create branches");
        }
        bail!(
            "`git branch --recurse-submodules` is not ported: creating a branch in every \
             submodule needs the submodule worktree walk that is not wired here"
        );
    }

    // git's option table marks --show-current and the list options as mutually
    // exclusive, so `--list --show-current` is a usage error before any work.
    if o.show_current && (o.explicit_list || o.delete || o.rename || o.copy) {
        return usage_exit();
    }

    // Resolve `auto` against the terminal (git's `finalize_colopts(&colopts, -1)`),
    // then apply the `--column` vs `--verbose` incompatibility: an explicit
    // `--column` is fatal, a config-only "always" is silently downgraded.
    super::column::finalize(&mut o.colopts);
    if o.verbose > 0 {
        if super::column::explicitly_enabled(o.colopts) {
            return fatal("options '--column' and '--verbose' cannot be used together");
        }
        o.colopts = super::column::DISABLED;
    }

    // Every ref this moves carries a reflog line, and git writes those with an
    // identity it synthesizes from the OS when `user.*` is unset — only a
    // `commit` with nothing determinable is refused. Without this a bare runner,
    // a container or a `sudo` shell cannot switch branches at all, and a
    // recursive submodule walk aborts on the first one it reaches.
    let mut repo = gix::discover(".")?;
    crate::ensure_reflog_identity(&mut repo);

    // `git_branch_config()` runs `color_parse()` on every `color.branch.<slot>`
    // it recognizes while configuration is being read, so an unparseable spec is
    // fatal for *every* `git branch` invocation — deletes and renames included,
    // and even for the `plain` slot the listing never paints with.
    if let Some((key, spec, meta)) = super::color::first_invalid_slot(&repo, "color.branch", &COLOR_SLOTS)
    {
        return Ok(super::color::invalid_color_fatal(&key, &spec, &meta));
    }

    if o.rename {
        return rename_branch(&repo, &o);
    }
    if o.copy {
        return copy_branch(&repo, &o);
    }
    if o.delete {
        return delete_branches(&repo, &o);
    }
    if o.edit_description {
        // --edit-description opens the configured editor on the branch
        // description; that interactive editor loop is not wired here.
        bail!("--edit-description is not supported by this port");
    }
    if let Some(up) = o.set_upstream_to.clone() {
        return set_upstream(&repo, &o, &up);
    }
    if o.unset_upstream {
        return unset_upstream(&repo, &o);
    }
    if o.show_current {
        return show_current(&repo);
    }
    if !o.names.is_empty() && !o.explicit_list {
        return create_branch(&repo, &o);
    }
    list_branches(&repo, &o)
}

/// Consume git's `LASTARG_DEFAULT` value for a bare filter flag: the next token
/// if there is one, otherwise `HEAD`. Advances `i` past a consumed token.
/// Resolve and apply one `--<body>` argument, advancing `i` past a detached
/// value. `Some(code)` means the command is over.
///
/// This is `parse_long_opt()` plus the `get_value()` call it ends in: the name is
/// resolved first, so a rejection happens before any branch is created — the bug
/// this replaced let `git branch --bogus` fall through to the positional arm and
/// create `refs/heads/--bogus`.
fn apply_long(
    o: &mut Opts,
    args: &[String],
    i: &mut usize,
    body: &str,
) -> Result<Option<ExitCode>> {
    // `arg_end = strchrnul(arg, '=')`: the name is looked up without its value,
    // but every diagnostic that echoes what was typed uses the whole body.
    let (name, attached) = match body.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (body, None),
    };

    let (opt, negated) = match resolve_long(LONG_OPTS, name) {
        Resolved::One(opt, negated) => (opt, negated),
        Resolved::Unknown => return usage_error(format!("unknown option `{body}'")).map(Some),
        Resolved::Ambiguous(first, second) => {
            return ambiguous_exit(body, &first, &second).map(Some)
        }
    };

    // How `optname()` spells the option in a diagnostic: the resolved long name,
    // carrying `no-` when it was reached through negation, so `--no-format=1`
    // complains about `no-format` and an abbreviation complains about the full
    // name it resolved to.
    let shown = match negated {
        true => format!("no-{}", opt.name),
        false => opt.name.to_string(),
    };

    // `get_value()` refuses a value for a negated option and for a flag:
    // `if (unset && p->opt) return error(_("%s takes no value"), ...)`.
    if attached.is_some() && (negated || opt.arg == Arg::None) {
        return value_error(format!("option `{shown}' takes no value")).map(Some);
    }

    let value: Option<String> = match (negated, opt.arg) {
        (true, _) | (_, Arg::None) => None,
        // PARSE_OPT_OPTARG never reaches for the next argument.
        (_, Arg::Optional) => attached.map(str::to_string),
        (_, Arg::Required) => match attached {
            Some(v) => Some(v.to_string()),
            None => {
                *i += 1;
                match args.get(*i) {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Ok(Some(super::missing_option_value(&format!("--{shown}"))))
                    }
                }
            }
        },
        (_, Arg::LastArg) => Some(match attached {
            Some(v) => v.to_string(),
            None => lastarg_default(args, i),
        }),
    };
    // Every arm below that reads a value has one by construction.
    let val = || value.clone().unwrap_or_default();

    match (opt.name, negated) {
        ("verbose", false) => o.verbose = o.verbose.saturating_add(1),
        ("verbose", true) => o.verbose = 0,
        ("quiet", n) => o.quiet = !n,
        ("track", false) => {
            o.track = match value.as_deref() {
                None | Some("direct") => Track::Direct,
                Some("inherit") => Track::Inherit,
                Some(_) => {
                    return value_error("option `--track' expects \"direct\" or \"inherit\"")
                        .map(Some)
                }
            }
        }
        ("track", true) => o.track = Track::No,
        // The hidden `--set-upstream` only selects BRANCH_TRACK_OVERRIDE here;
        // the creation path is where git refuses it by name. `OPT_SET_INT`
        // unset writes 0, which is the same "never track" `--no-track` picks.
        ("set-upstream", false) => o.track = Track::Override,
        ("set-upstream", true) => o.track = Track::No,
        ("set-upstream-to", false) => o.set_upstream_to = value,
        ("set-upstream-to", true) => o.set_upstream_to = None,
        ("unset-upstream", n) => o.unset_upstream = !n,
        ("color", false) => {
            o.color = match value.as_deref() {
                None | Some("always") => ColorWhen::Always,
                Some("never" | "false") => ColorWhen::Never,
                Some("auto") => ColorWhen::Auto,
                Some(_) => {
                    return value_error(
                        "option `color' expects \"always\", \"auto\", or \"never\"",
                    )
                    .map(Some)
                }
            }
        }
        ("color", true) => o.color = ColorWhen::Never,
        ("remotes", _) => o.mode = ListMode::Remotes,
        ("all", _) => o.mode = ListMode::All,
        // `--with` / `--without` are the hidden aliases of `--contains` /
        // `--no-contains`, sharing their `filter.with_commit` slot.
        ("contains" | "with", _) => o.contains.push(val()),
        ("no-contains" | "without", _) => o.no_contains.push(val()),
        ("merged", _) => o.merged.push(val()),
        ("no-merged", _) => o.no_merged.push(val()),
        ("abbrev", false) => {
            o.abbrev = match value {
                None => None,
                Some(v) => match v.parse::<usize>() {
                    Ok(n) => Some(n),
                    Err(_) => {
                        return value_error("option `abbrev' expects a numerical value").map(Some)
                    }
                },
            }
        }
        ("abbrev", true) => o.abbrev = Some(0),
        ("delete", n) => o.delete = !n,
        ("move", n) => o.rename = !n,
        ("copy", n) => o.copy = !n,
        ("omit-empty", n) => o.omit_empty = !n,
        ("list", n) => o.explicit_list = !n,
        ("show-current", n) => o.show_current = !n,
        ("create-reflog", n) => o.create_reflog = !n,
        ("edit-description", n) => o.edit_description = !n,
        ("force", n) => o.force = !n,
        ("ignore-case", n) => o.ignore_case = !n,
        ("recurse-submodules", n) => o.recurse_submodules = !n,
        ("format", false) => o.format = value,
        ("format", true) => o.format = None,
        ("sort", false) => o.sorts.push(val()),
        ("sort", true) => {
            o.sorts.clear();
            o.sort_cleared = true;
        }
        ("points-at", false) => o.points_at.push(val()),
        ("points-at", true) => o.points_at.clear(),
        // `OPT_COLUMN`: a bad `<style>` token is the callback's own error.
        ("column", n) => {
            if super::column::parseopt_column(&mut o.colopts, value.as_deref(), n).is_err() {
                return usage_exit().map(Some);
            }
        }
        (other, _) => bail!("unsupported option --{other}"),
    }
    Ok(None)
}

fn lastarg_default(args: &[String], i: &mut usize) -> String {
    if *i + 1 < args.len() {
        *i += 1;
        args[*i].clone()
    } else {
        "HEAD".to_string()
    }
}

/// `--show-current`: the checked-out branch's short name, or nothing at all when
/// HEAD is detached or unborn. Exits 0 either way.
fn show_current(repo: &gix::Repository) -> Result<ExitCode> {
    if let Some(name) = repo.head_name()? {
        println!("{}", name.shorten());
    }
    Ok(ExitCode::SUCCESS)
}


/// git's `ref_to_worktree_map` (`ref-filter.c`), the table `%(worktreepath)`
/// looks a branch up in: every working tree whose `HEAD` is symbolic, keyed by
/// the full refname it points at and valued by the tree's path.
///
/// Mirrors `get_worktrees()`: the main working tree (the common dir with a
/// trailing `/.git` cut off, which leaves a bare repository untouched) followed
/// by each `<common-dir>/worktrees/<id>`, whose path is the `gitdir` file's
/// contents with the same suffix removed. A worktree with a detached or
/// unreadable `HEAD` contributes nothing, so it never marks a branch.
pub(crate) fn worktree_map(repo: &gix::Repository) -> std::collections::HashMap<BString, String> {
    /// `<dir>/HEAD`'s symbolic target, or `None` when detached/unreadable.
    fn head_ref(dir: &std::path::Path) -> Option<BString> {
        let raw = std::fs::read(dir.join("HEAD")).ok()?;
        let target = raw.strip_prefix(b"ref:".as_slice())?;
        Some(BString::from(target.trim().to_owned()))
    }
    /// Drop the `/.git` a worktree's git dir ends in, leaving the checkout path.
    fn checkout_of(git_dir: &std::path::Path) -> std::path::PathBuf {
        match git_dir.file_name().and_then(|n| n.to_str()) {
            Some(".git") => git_dir.parent().unwrap_or(git_dir).to_path_buf(),
            _ => git_dir.to_path_buf(),
        }
    }

    let mut map = std::collections::HashMap::new();
    let common = gix::path::realpath(repo.common_dir()).unwrap_or_else(|_| repo.common_dir().into());
    let mut add = |dir: &std::path::Path, path: std::path::PathBuf| {
        if let Some(name) = head_ref(dir) {
            map.entry(name)
                .or_insert_with(|| gix::path::into_bstr(path).to_str_lossy().into_owned());
        }
    };
    if !repo.is_bare() {
        add(&common, checkout_of(&common));
    }
    let Ok(dir) = std::fs::read_dir(common.join("worktrees")) else {
        return map;
    };
    // git sorts the linked worktrees by path before inserting, so a branch that
    // is somehow checked out twice resolves to the same tree git would name.
    let mut linked: Vec<std::path::PathBuf> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    linked.sort();
    for wt in linked {
        let Ok(raw) = std::fs::read(wt.join("gitdir")) else {
            continue;
        };
        // `get_linked_worktree()` (worktree.c:158-162) resolves a *relative* recording against
        // the administrative directory before using it, which is what `worktree add
        // --relative-paths` and `worktree.useRelativePaths` write. Taking the string literally
        // printed `../../../wt` where git prints the worktree's absolute path.
        let git_dir = super::worktree::recorded_dot_git(&wt, raw.trim());
        add(&wt, checkout_of(&git_dir));
    }
    map
}

/// git's `branch_get_upstream`: the remote-tracking ref `full` is configured to
/// build on, or `None` when it tracks nothing.
pub(crate) fn upstream_ref(repo: &gix::Repository, full: &BStr) -> Option<FullName> {
    let name = FullName::try_from(full.to_owned()).ok()?;
    // git's `set_merge`: a `branch.<name>.remote` of `.` means the upstream lives
    // in this very repository, so `branch.<name>.merge` *is* the upstream ref and
    // no fetch refspec is consulted.
    let local_remote = repo
        .config_snapshot()
        .string(&format!("branch.{}.remote", name.shorten()))
        .is_some_and(|v| v.as_bstr() == ".");
    if local_remote {
        repo.branch_remote_ref_name(name.as_ref(), gix::remote::Direction::Fetch)?
            .ok()
    } else {
        repo.branch_remote_tracking_ref_name(name.as_ref(), gix::remote::Direction::Fetch)?
            .ok()
    }
}

/// git's `branch_get_push`: the remote-tracking ref that would mirror a push of
/// `full`, or `None` when the branch has no push destination.
pub(crate) fn push_ref(repo: &gix::Repository, full: &BStr) -> Option<FullName> {
    let name = FullName::try_from(full.to_owned()).ok()?;
    repo.branch_remote_tracking_ref_name(name.as_ref(), gix::remote::Direction::Push)?
        .ok()
}

/// git's `stat_tracking_info` with `AHEAD_BEHIND_FULL`: the commit counts each
/// side has that the other does not, or `None` for its `-1` return — the
/// upstream ref is gone, or either end fails to name a commit.
pub(crate) fn stat_tracking_info(
    repo: &gix::Repository,
    local: Option<gix::Id<'_>>,
    upstream: &FullName,
) -> Option<(usize, usize)> {
    let up = repo
        .try_find_reference(upstream.as_ref())
        .ok()
        .flatten()
        .and_then(|r| r.into_fully_peeled_id().ok())?;
    let local = local?;
    if local.detach() == up.detach() {
        return Some((0, 0));
    }
    let count = |tip: ObjectId, hidden: ObjectId| -> usize {
        match repo.rev_walk(Some(tip)).with_hidden(Some(hidden)).all() {
            Ok(walk) => walk.take_while(Result::is_ok).count(),
            Err(_) => 0,
        }
    };
    Some((
        count(local.detach(), up.detach()),
        count(up.detach(), local.detach()),
    ))
}

/// Per-slot colors for the branch listing, resolved once. `on` is false when
/// coloring is disabled, in which case no SGR (and no reset) is emitted.
struct Colors {
    on: bool,
    current: String,
    local: String,
    remote: String,
    /// `color.branch.upstream` — the `[<upstream>` name inside the `-vv`
    /// tracking field. git's default is blue.
    upstream: String,
    /// `color.branch.worktree` — the name of a branch checked out in some other
    /// worktree (marked `+`), and the `(<path>)` field `-vv` prints for it.
    /// git's default is cyan.
    worktree: String,
    /// `color.branch.reset` — the sequence that closes a colored name. git makes
    /// this a slot of its own (`BRANCH_COLOR_RESET`), so a user can replace the
    /// plain `\e[m` with any spec.
    reset: String,
}

/// `color_branch_slots[]` — every name `git_branch_config()` accepts under
/// `color.branch.`. `plain` is in the table because the callback runs
/// `color_parse()` on it, even though `build_format()` never asks for
/// `BRANCH_COLOR_PLAIN`: setting it to a spec git's parser rejects is fatal, and
/// setting it to a valid one changes nothing.
pub(crate) const COLOR_SLOTS: [&str; 7] = [
    "reset", "plain", "remote", "local", "current", "upstream", "worktree",
];

/// Decide whether `git branch` colors its output and, if so, resolve every slot's
/// SGR. Mirrors git: `--color` overrides, else `color.branch` falling back to
/// `color.ui` (default `auto`); `auto` colors only on a terminal.
fn resolve_colors(repo: &gix::Repository, when: ColorWhen) -> Colors {
    let on = match when {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => super::color::want_color_stdout(repo, "branch"),
    };
    if !on {
        return Colors {
            on: false,
            current: String::new(),
            local: String::new(),
            remote: String::new(),
            upstream: String::new(),
            worktree: String::new(),
            reset: String::new(),
        };
    }
    let snap = repo.config_snapshot();
    let slot = |key: &str, default: &str| -> String {
        let spec = snap
            .string(key)
            .map(|v| v.to_string())
            .unwrap_or_else(|| default.to_string());
        color_sgr(&spec)
    };
    Colors {
        on: true,
        current: slot("color.branch.current", "green"),
        local: slot("color.branch.local", "normal"),
        remote: slot("color.branch.remote", "red"),
        // git's `BRANCH_COLOR_UPSTREAM` / `BRANCH_COLOR_WORKTREE` defaults.
        upstream: slot("color.branch.upstream", "blue"),
        worktree: slot("color.branch.worktree", "cyan"),
        // git's `BRANCH_COLOR_RESET` default is the bare reset, which
        // `color_sgr` renders from the `reset` attribute as `\e[0m`; git emits
        // `\e[m` for it, so the default is kept literal.
        reset: match snap.string("color.branch.reset") {
            Some(spec) => super::color::parse_color_spec(&spec.to_string())
                .unwrap_or_else(|| RESET.to_string()),
            None => RESET.to_string(),
        },
    }
}

/// Convert a git color spec (`"green"`, `"bold red"`, `"#ff00ff"`, `"reverse"`)
/// into its SGR sequence, or an empty string when the spec sets nothing visible
/// (git's `normal`). An unparsable spec yields an empty SGR rather than failing.
fn color_sgr(spec: &str) -> String {
    // git parses a spec into leading attributes then up to two colors (foreground
    // then background); the SGR emits attributes first, then the color codes.
    let mut codes: Vec<String> = Vec::new();
    let mut color_words: Vec<&str> = Vec::new();
    for word in spec.split_whitespace() {
        if let Some(code) = attr_code(word) {
            codes.push(code.to_string());
        } else {
            color_words.push(word);
        }
    }
    for (idx, word) in color_words.iter().take(2).enumerate() {
        // The foreground slot is consumed even when it renders no code (`normal`),
        // so a following color still lands in the background slot.
        if let Some(code) = color_code(word, idx == 1) {
            codes.push(code);
        }
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// git's SGR attribute codes (`color.c`), with `no`/`no-` negations.
fn attr_code(word: &str) -> Option<&'static str> {
    let (word, neg) = match word.strip_prefix("no-").or_else(|| word.strip_prefix("no")) {
        Some(rest) if !rest.is_empty() && rest != "rmal" => (rest, true),
        _ => (word, false),
    };
    Some(match (word, neg) {
        ("bold", false) => "1",
        ("dim", false) => "2",
        ("italic", false) => "3",
        ("ul", false) => "4",
        ("blink", false) => "5",
        ("reverse", false) => "7",
        ("strike", false) => "9",
        ("bold", true) | ("dim", true) => "22",
        ("italic", true) => "23",
        ("ul", true) => "24",
        ("blink", true) => "25",
        ("reverse", true) => "27",
        ("strike", true) => "29",
        ("reset", false) => "0",
        _ => return None,
    })
}

/// git's SGR color code for a name, as foreground (`bg=false`) or background.
/// `normal` produces no code (git's `-1`).
fn color_code(word: &str, bg: bool) -> Option<String> {
    let base = if bg { 40 } else { 30 };
    let bright = if bg { 100 } else { 90 };
    let (name, is_bright) = match word.strip_prefix("bright") {
        Some(rest) => (rest, true),
        None => (word, false),
    };
    let idx = match name {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "white" => 7,
        "normal" if !is_bright => return None,
        "default" if !is_bright => return Some((base + 9).to_string()),
        _ => {
            if let Ok(n) = word.parse::<u8>() {
                let sel = if bg { 48 } else { 38 };
                return Some(format!("{sel};5;{n}"));
            }
            if let Some(hex) = word.strip_prefix('#') {
                if hex.len() == 6 {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..2], 16),
                        u8::from_str_radix(&hex[2..4], 16),
                        u8::from_str_radix(&hex[4..6], 16),
                    ) {
                        let sel = if bg { 48 } else { 38 };
                        return Some(format!("{sel};2;{r};{g};{b}"));
                    }
                }
            }
            return None;
        }
    };
    Some(((if is_bright { bright } else { base }) + idx).to_string())
}

/// `print_ref_list()` (builtin/branch.c:445-502): filter the refs this listing
/// asked for, size the name column from what survived, build the format string
/// unless the user gave one, and hand the whole thing to the shared
/// `ref-filter` evaluator. `--format` therefore *replaces* `-v`'s layout rather
/// than being overridden by it — `if (!format->format)` is the only place
/// [`build_format`] is reached.
fn list_branches(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    // ```c
    // repo_config(the_repository, git_branch_config, &sorting_options);
    // if (!sorting_options.nr)
    //         string_list_append(&sorting_options, "refname");
    // ```
    // (builtin/branch.c:795-797) — *before* `parse_options()`. So the
    // `branch.sort` values come first, an implicit `refname` stands in when there
    // are none, and every `--sort` from the command line appends after them,
    // ending up most significant. A `--no-sort` clears the lot, config and
    // implicit key included, which is what leaves `sorting` NULL.
    let mut sorts: Vec<String> = if o.sort_cleared {
        Vec::new()
    } else {
        let mut cfg: Vec<String> = repo
            .config_snapshot()
            .plumbing()
            .values::<BString>("branch.sort")
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.to_string())
            .collect();
        if cfg.is_empty() {
            cfg.push("refname".to_string());
        }
        cfg
    };
    sorts.extend(o.sorts.iter().cloned());
    // `ref_array_sort()` runs only `if (sorting)`, and
    // `REF_SORTING_DETACHED_HEAD_FIRST` lives on the sorting nodes — so with an
    // empty list nothing moves, and the detached pseudo entry stays where
    // `do_filter_refs()` appended it: last.
    let detached_head_first = !sorts.is_empty();

    // Resolve every reachability filter before walking refs. The operand's own
    // callback decides the diagnostic and the status — 129 for the two that
    // return `PARSE_OPT_ERROR`, 128 for `--merged`'s outright `die()`.
    let filters = match resolve_filters(repo, o) {
        Ok(f) => f,
        Err(e) => return Ok(e.report()),
    };

    let colors = resolve_colors(repo, o.color);

    // `filter.kind`, and the `remote_prefix` that widens a remote-tracking name
    // whenever local refs are in the same listing (builtin/branch.c:459-460).
    let kinds = match o.mode {
        ListMode::Local => ref_filter::kind::BRANCHES,
        ListMode::Remotes => ref_filter::kind::REMOTES,
        ListMode::All => ref_filter::kind::BRANCHES | ref_filter::kind::REMOTES,
    };
    let remote_prefix = if o.mode == ListMode::Remotes {
        ""
    } else {
        "remotes/"
    };

    // `git branch --list` also shows HEAD when it is detached:
    //
    //     if ((filter.kind & FILTER_REFS_BRANCHES) && filter.detached)
    //             filter.kind |= FILTER_REFS_DETACHED_HEAD;
    let head = repo.head()?;
    let head_desc = if kinds & ref_filter::kind::BRANCHES != 0 && head.is_detached() {
        // `get_head_description()` (ref-filter.c:2297-2327) names the *switch* the
        // reflog recorded, not the object HEAD holds, and says `at` only while
        // HEAD still sits on it:
        //
        // ```c
        // else if (state.detached_from) {
        //         if (state.detached_at)
        //                 strbuf_addf(&desc, _("(HEAD detached at %s)"), state.detached_from);
        //         else
        //                 strbuf_addf(&desc, _("(HEAD detached from %s)"), state.detached_from);
        // } else
        //         strbuf_addstr(&desc, _("(no branch)"));
        // ```
        //
        // `wt_status_get_detached_from()` is the same one `git status`'s long
        // format uses, so the two commands cannot disagree about the wording.
        Some(
            match super::status::detached_from(repo) {
                Some((name, true)) => format!("(HEAD detached at {name})"),
                Some((name, false)) => format!("(HEAD detached from {name})"),
                // NULL `detached_from`: a hand-written HEAD, or a pruned reflog.
                None => "(no branch)".to_string(),
            }
            .into_bytes(),
        )
    } else {
        None
    };

    let kinds = kinds | if head_desc.is_some() { ref_filter::kind::DETACHED_HEAD } else { 0 };
    let built = |cands: &[ref_filter::Candidate]| -> Vec<u8> {
        let maxwidth = if o.verbose > 0 {
            calc_maxwidth(cands, remote_prefix.len())
        } else {
            0
        };
        build_format(o, &colors, maxwidth, remote_prefix)
    };
    let spec = ref_filter::ListSpec {
        repo,
        format: match &o.format {
            Some(f) => ref_filter::Format::Fixed(f.as_bytes().to_vec()),
            None => ref_filter::Format::Built(&built),
        },
        sort_specs: sorts,
        kinds,
        patterns: o.names.clone(),
        ignore_case: o.ignore_case,
        points_at: filters.points_at.clone(),
        filters: filters.shared(),
        omit_empty: o.omit_empty,
        color_on: colors.on,
        head_desc,
        // `print_ref_list()` (builtin/branch.c:476-477) has no `filter_is_base()`
        // call, so `%(is-base:<x>)` is always empty under `git branch`.
        run_is_base: false,
        detached_head_first,
        // `filter.verbose = !!verbose` (builtin/branch.c), which is what makes `-v` drop a branch
        // whose object is missing while a plain listing still names it.
        verbose: o.verbose > 0,
    };

    let lines = match ref_filter::filter_and_format(&spec)? {
        ref_filter::Listing::Lines(lines) => lines,
        ref_filter::Listing::Exit(code) => return Ok(code),
    };

    if super::column::active(o.colopts) {
        emit_columns(o.colopts, lines);
        return Ok(ExitCode::SUCCESS);
    }
    let mut out: Vec<u8> = Vec::new();
    for line in lines {
        out.extend_from_slice(&line);
        out.push(b'\n');
    }
    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// `calc_maxwidth()` (builtin/branch.c:351-374): the widest name in the filtered
/// array, measured after its namespace prefix is stripped, with remote-tracking
/// names charged for the `remotes/` they will be printed with.
fn calc_maxwidth(cands: &[ref_filter::Candidate], remote_bonus: usize) -> usize {
    let mut max = 0;
    for c in cands {
        let mut w = match &c.head_desc {
            Some(desc) => String::from_utf8_lossy(desc).chars().count(),
            None => {
                let desc = c
                    .refname
                    .strip_prefix(b"refs/heads/".as_slice())
                    .or_else(|| c.refname.strip_prefix(b"refs/remotes/".as_slice()))
                    .unwrap_or(&c.refname);
                String::from_utf8_lossy(desc).chars().count()
            }
        };
        if c.kind == ref_filter::kind::REMOTES {
            w += remote_bonus;
        }
        max = max.max(w);
    }
    max
}

/// `build_format()` (builtin/branch.c:386-443) — the format string `git branch`
/// falls back to when no `--format` was given.
///
/// Every decoration `git branch` is known for is in here and nowhere else: the
/// `* ` current marker, the `+ ` worktree marker, the color slots, the padded
/// name column under `-v`, the `-> <symref:short>` tail, and the `-vv` upstream
/// and ahead/behind fields. They are ordinary atoms evaluated by the same
/// `ref-filter` machinery `--format` runs on, which is why `--format` replaces
/// all of them at once rather than composing with any of them.
fn build_format(o: &Opts, colors: &Colors, maxwidth: usize, remote_prefix: &str) -> Vec<u8> {
    let reset = colors.reset.as_str();
    let mut local = format!(
        "%(if)%(HEAD)%(then)* {}%(else)%(if)%(worktreepath)%(then)+ {}%(else)  {}%(end)%(end)",
        colors.current, colors.worktree, colors.local
    );
    let mut remote = format!("  {}", colors.remote);

    if o.verbose > 0 {
        let obname = match o.abbrev {
            None => "%(objectname:short)".to_string(),
            Some(0) => "%(objectname)".to_string(),
            Some(n) => format!("%(objectname:short={n})"),
        };
        local.push_str(&format!(
            "%(align:{maxwidth},left)%(refname:lstrip=2)%(end)"
        ));
        local.push_str(reset);
        local.push_str(&format!(" {obname} "));

        if o.verbose > 1 {
            local.push_str(&format!(
                "%(if:notequals=*)%(HEAD)%(then)%(if)%(worktreepath)%(then)({}%(worktreepath){}) %(end)%(end)",
                colors.worktree, reset
            ));
            local.push_str(&format!(
                "%(if)%(upstream)%(then)[{}%(upstream:short){}%(if)%(upstream:track)\
                 %(then): %(upstream:track,nobracket)%(end)] %(end)%(contents:subject)",
                colors.upstream, reset
            ));
        } else {
            local.push_str(
                "%(if)%(upstream:track)%(then)%(upstream:track) %(end)%(contents:subject)",
            );
        }

        remote.push_str(&format!(
            "%(align:{maxwidth},left){}%(refname:lstrip=2)%(end){}\
             %(if)%(symref)%(then) -> %(symref:short)\
             %(else) {obname} %(contents:subject)%(end)",
            quote_literal_for_format(remote_prefix),
            reset
        ));
    } else {
        local.push_str(&format!(
            "%(refname:lstrip=2){reset}%(if)%(symref)%(then) -> %(symref:short)%(end)"
        ));
        remote.push_str(&format!(
            "{}%(refname:lstrip=2){reset}%(if)%(symref)%(then) -> %(symref:short)%(end)",
            quote_literal_for_format(remote_prefix)
        ));
    }

    format!("%(if:notequals=refs/remotes)%(refname:rstrip=-2)%(then){local}%(else){remote}%(end)")
        .into_bytes()
}

/// `quote_literal_for_format()` (builtin/branch.c:376-384): a literal spliced
/// into a format string has to double its `%`, or it would be read as an atom.
fn quote_literal_for_format(s: &str) -> String {
    s.replace('%', "%%")
}

/// Lay `cells` out through the shared column engine (git's `print_columns` with
/// a NULL `column_options`: padding 1, no indent, `\n` newline, terminal width)
/// and write the result to stdout.
fn emit_columns(colopts: u32, cells: Vec<Vec<u8>>) {
    let opts = super::column::ColumnOptions {
        width: 0,
        padding: 1,
        indent: None,
        nl: None,
    };
    let bytes = super::column::layout(&cells, colopts, &opts);
    let _ = std::io::stdout().write_all(&bytes);
}

/// Resolve every `--contains`/`--no-contains`/`--merged`/`--no-merged`/
/// `--points-at` operand.
///
/// git does this from three different `parse_options()` callbacks, and they do
/// not share a diagnostic: `OPT_CONTAINS` is `parse_opt_commits`, `OPT_MERGED`
/// is `parse_opt_merge_filter`, and `--points-at` is `parse_opt_object_name`
/// (which never peels and never consults the odb, so it accepts an absent id and
/// simply matches nothing). Routing all five through one resolver is what made
/// every one of them report `fatal: malformed object name` at 128.
fn resolve_filters(
    repo: &gix::Repository,
    o: &Opts,
) -> Result<Filters, crate::objname::OperandError> {
    let commits = |specs: &[String]| -> Result<Vec<ObjectId>, crate::objname::OperandError> {
        specs
            .iter()
            .map(|s| crate::objname::parse_opt_commits(repo, s))
            .collect()
    };
    let merges = |specs: &[String],
                  long_name: &str|
     -> Result<Vec<ObjectId>, crate::objname::OperandError> {
        specs
            .iter()
            .map(|s| crate::objname::parse_opt_merge_filter(repo, s, long_name))
            .collect()
    };
    Ok(Filters {
        contains: commits(&o.contains)?,
        no_contains: commits(&o.no_contains)?,
        merged: merges(&o.merged, "merged")?,
        no_merged: merges(&o.no_merged, "no-merged")?,
        points_at: o
            .points_at
            .iter()
            .map(|s| crate::objname::parse_opt_object_name(repo, s))
            .collect::<Result<_, _>>()?,
    })
}

/// Create a local branch. With no `<start-point>` it starts at the current HEAD
/// commit; with one, at that resolved commit. `-t`/`--track`/`branch.autoSetupMerge`
/// then records the upstream, and `-f` allows overwriting an existing branch.
fn create_branch(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    if o.names.len() > 2 {
        return usage_exit();
    }
    let name = o.names[0].as_str();
    let full = format!("refs/heads/{name}");

    // `-a`/`-r` widen `filter.kind`, and the creation arm refuses to run under a
    // widened one — before `--set-upstream` and before any name validation:
    //
    // ```c
    // if (filter.kind != FILTER_REFS_BRANCHES)
    //         die(_("the -a, and -r, options to 'git branch' do not take a branch name.\n"
    //               "Did you mean to use: -a|-r --list <pattern>?"));
    // ```
    // (builtin/branch.c:1000-1002). Without this, `git branch -a <name>` quietly
    // *creates* `<name>` at HEAD and exits 0.
    if o.mode != ListMode::Local {
        return fatal(
            "the -a, and -r, options to 'git branch' do not take a branch name.\n\
             Did you mean to use: -a|-r --list <pattern>?",
        );
    }

    // The hidden `--set-upstream` is accepted by the parser and refused here, by
    // name, ahead of any name validation:
    //
    //     if (track == BRANCH_TRACK_OVERRIDE)
    //             die(_("the '--set-upstream' option is no longer supported. "
    //                   "Please use '--track' or '--set-upstream-to' instead"));
    if o.track == Track::Override {
        return fatal(
            "the '--set-upstream' option is no longer supported. \
             Please use '--track' or '--set-upstream-to' instead",
        );
    }

    if !valid_branch_name(name) {
        let code = fatal(format!("'{name}' is not a valid branch name"))?;
        ref_syntax_hints(repo);
        return Ok(code);
    }

    let start = o.names.get(1).map(|s| s.as_str());

    // The reflog message names the start-point: the literal argument, or the
    // current branch's short name when starting from HEAD.
    let current_short = repo.head_name()?.map(|n| n.shorten().to_string());
    let start_name = match start {
        Some(s) => s.to_string(),
        None => current_short.clone().unwrap_or_else(|| "HEAD".to_string()),
    };

    // Resolve the target commit and, when the start-point is itself a ref, its
    // full name — used to decide tracking.
    let (target, start_ref): (ObjectId, Option<BString>) = match start {
        Some(s) => {
            // git's `dwim_branch_start()` (`branch.c`) draws the line here:
            // `repo_get_oid_mb()` failing is `not a valid object name`, but a name
            // that *did* resolve and then fails `lookup_commit_reference()` is
            // `not a valid branch point` — which is what an absent full-length hex
            // name reaches, since `get_oid_basic()` decodes it without asking the
            // odb whether the object exists.
            let Some(id) = crate::objname::resolve(repo, s) else {
                return fatal(format!("not a valid object name: '{s}'"));
            };
            // `dwim_branch_start()` then DWIMs the same name, and more than one
            // match is fatal — checked before `lookup_commit_reference()`:
            //
            // ```c
            // switch (repo_dwim_ref(r, start_name, strlen(start_name), &oid, &real_ref, 0)) {
            // …
            // default:
            //         die(_("ambiguous object name: '%s'"), start_name);
            // }
            // ```
            if super::rev_parse::dwim_ref_matches(repo, s).len() > 1 {
                return fatal(format!("ambiguous object name: '{s}'"));
            }
            let found = crate::objname::lookup_commit_reference(repo, id);
            let crate::objname::CommitRef::Commit(commit) = found else {
                // `object_as_type()` has already complained about a present
                // object of the wrong type; git prints that line before dying.
                if let Some(note) = found.type_error() {
                    eprintln!("error: {note}");
                }
                return fatal(format!("not a valid branch point: '{s}'"));
            };
            let start_ref = repo
                .find_reference(s)
                .ok()
                .map(|r| r.name().as_bstr().to_owned());
            (commit, start_ref)
        }
        None => {
            let head = repo.head()?;
            if head.is_unborn() {
                return fatal("not a valid object name: 'HEAD'");
            }
            let id = head
                .id()
                .ok_or_else(|| anyhow!("HEAD does not point to a commit"))?
                .detach();
            let start_ref = repo.head_name()?.map(|n| n.as_bstr().to_owned());
            (id, start_ref)
        }
    };

    // Decide tracking before touching the ref: git dies (without creating the
    // branch) if `--track` was explicit but the start-point is not a branch.
    let start_ref_bstr = start_ref.as_ref().map(|b| b.as_bstr());
    if let Some(code) = ambiguous_tracking(repo, start_ref_bstr, o.track)? {
        return Ok(code);
    }
    let upstream = tracking_upstream(repo, start_ref_bstr, o.track, name);
    if matches!(o.track, Track::Direct | Track::Inherit) && upstream.is_none() {
        return fatal(format!(
            "cannot set up tracking information; starting point '{start_name}' is not a branch"
        ));
    }

    // Serialize the ref read-modify-write through the repo coordinator.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let existed = repo.try_find_reference(full.as_str())?.is_some();
    if existed && !o.force {
        return fatal(format!("a branch named '{name}' already exists"));
    }
    // ```c
    // if ((path = branch_checked_out(ref->buf)))
    //         die(_("cannot force update the branch '%s' used by worktree at '%s'"), …);
    // ```
    // (branch.c:481-484, `validate_new_branchname()`.) `--force` overrides the branch already
    // being there, never a worktree standing on it: moving the ref would leave that checkout's
    // index and worktree describing a commit its own `HEAD` no longer names.
    if existed && o.force {
        if let Some(path) = super::worktree::branch_checked_out(repo, &full)? {
            return fatal(format!(
                "cannot force update the branch '{name}' used by worktree at '{}'",
                super::worktree::path_to_string(&path)
            ));
        }
    }

    let verb = if existed { "Reset to" } else { "Created from" };
    let message = format!("branch: {verb} {start_name}");

    repo.reference(
        full,
        target,
        if o.force {
            PreviousValue::Any
        } else {
            PreviousValue::MustNotExist
        },
        message,
    )?;

    if let Some(up) = upstream {
        install_tracking(repo, name, &up, o.quiet)?;
    }

    Ok(ExitCode::SUCCESS)
}

/// `setup_tracking()`'s `for_each_remote(find_tracked_branch)` pass (branch.c):
/// before deciding an upstream, git asks which remotes' *fetch* refspecs map
/// onto the start-point ref. Two or more is a configuration error it refuses to
/// guess through — it dies naming the remotes, without creating the branch, and
/// the list of them is the `advice.ambiguousFetchRefspec` hint.
///
/// Returns `Some(128)` when the caller must stop. The scan is skipped exactly
/// where git skips it: with `--no-track`, for a start-point that is not a ref,
/// for `--track=inherit` (which reads the start branch's own upstream instead of
/// consulting remotes), and when `branch.autoSetupMerge=false` left git's
/// `track` at `BRANCH_TRACK_NEVER` — but an explicit `--track` overrides that
/// last one, and even `branch.autoSetupMerge=simple` reaches the check before it
/// gets to compare names.
fn ambiguous_tracking(
    repo: &gix::Repository,
    start_ref: Option<&BStr>,
    track: Track,
) -> Result<Option<ExitCode>> {
    if track == Track::No {
        return Ok(None);
    }
    let Some(orig_ref) = start_ref else { return Ok(None) };
    let snap = repo.config_snapshot();
    let mode = snap
        .string("branch.autoSetupMerge")
        .map(|v| v.to_str_lossy().to_ascii_lowercase());
    let mode = mode.as_deref();
    let explicit = matches!(track, Track::Direct | Track::Inherit);
    if track == Track::Inherit || mode == Some("inherit") {
        return Ok(None);
    }
    if !explicit && matches!(mode, Some("false" | "no" | "off" | "0")) {
        return Ok(None);
    }

    let matches = remotes_fetching_into(repo, orig_ref);
    if matches.len() < 2 {
        return Ok(None);
    }
    eprintln!("fatal: not tracking: ambiguous information for ref '{orig_ref}'");
    let mut listed = String::new();
    for name in &matches {
        listed.push_str(&format!("  {name}\n"));
    }
    crate::advice::Advice::AmbiguousFetchRefspec.advise_plain_in(
        repo,
        &format!(
            "There are multiple remotes whose fetch refspecs map to the remote\n\
             tracking ref '{orig_ref}':\n\
             {listed}\n\
             This is typically a configuration error.\n\
             \n\
             To support setting up tracking branches, ensure that\n\
             different remotes' fetch refspecs map into different\n\
             tracking namespaces."
        ),
    );
    Ok(Some(ExitCode::from(128)))
}

/// `find_tracked_branch()` reduced to what the ambiguity check needs: the names
/// of the remotes with a fetch refspec whose destination covers `name`. Mirrors
/// `refspec_find_match()`'s dst-side lookup — refspecs with no destination and
/// negative (`^`) ones are skipped, and a destination containing `*` matches by
/// prefix and suffix (`match_name_with_pattern`).
fn remotes_fetching_into(repo: &gix::Repository, name: &BStr) -> Vec<String> {
    let needle: &[u8] = name.as_ref();
    let mut hits = Vec::new();
    for remote_name in repo.remote_names() {
        let Ok(remote) = repo.find_remote(&*remote_name) else { continue };
        let covered = remote.refspecs(gix::remote::Direction::Fetch).iter().any(|spec| {
            let gix::refspec::Instruction::Fetch(gix::refspec::instruction::Fetch::AndUpdate {
                dst,
                ..
            }) = spec.to_ref().instruction()
            else {
                return false;
            };
            let dst: &[u8] = dst.as_ref();
            match dst.iter().position(|&b| b == b'*') {
                Some(star) => {
                    let (prefix, suffix) = (&dst[..star], &dst[star + 1..]);
                    needle.len() >= prefix.len() + suffix.len()
                        && needle.starts_with(prefix)
                        && needle.ends_with(suffix)
                }
                None => dst == needle,
            }
        });
        if covered {
            hits.push(remote_name.to_str_lossy().into_owned());
        }
    }
    hits
}

/// The upstream a branch should track: `(remote, merge_ref, short)`. Auto-set
/// when the start-point is a remote-tracking branch (git's default
/// `branch.autoSetupMerge=true`); a local branch is tracked only with an explicit
/// `--track`. `--no-track` disables it. `--track=inherit` copies the start
/// branch's own upstream. Mirrors `git switch`'s tracking logic.
fn tracking_upstream(
    repo: &gix::Repository,
    start_ref: Option<&BStr>,
    track: Track,
    new_branch: &str,
) -> Option<(String, String, String)> {
    if track == Track::No {
        return None;
    }
    let full = start_ref?;
    let s = full.to_str_lossy();
    let explicit = matches!(track, Track::Direct | Track::Inherit);

    let snap = repo.config_snapshot();
    let mode = snap
        .string("branch.autoSetupMerge")
        .map(|v| v.to_str_lossy().to_ascii_lowercase());
    let mode = mode.as_deref();
    let off = matches!(mode, Some("false" | "no" | "off" | "0"));

    // `--track=inherit` (or `branch.autoSetupMerge=inherit`) copies the start
    // branch's own upstream rather than pointing at the start branch itself.
    if track == Track::Inherit || mode == Some("inherit") {
        if let Some(b) = s.strip_prefix("refs/heads/") {
            return inherited_upstream(&snap, b);
        }
    }

    if let Some(rest) = s.strip_prefix("refs/remotes/") {
        let (remote, branch) = rest.split_once('/')?;
        let auto = if off {
            false
        } else if mode == Some("simple") {
            branch == new_branch
        } else {
            true
        };
        if explicit || auto {
            return Some((
                remote.to_string(),
                format!("refs/heads/{branch}"),
                format!("{remote}/{branch}"),
            ));
        }
        return None;
    }

    if let Some(branch) = s.strip_prefix("refs/heads/") {
        if explicit || mode == Some("always") {
            return Some((
                ".".to_string(),
                format!("refs/heads/{branch}"),
                branch.to_string(),
            ));
        }
    }
    None
}

/// The upstream inherited from a local start branch's own `branch.<b>.remote`/
/// `branch.<b>.merge`, if it has one.
fn inherited_upstream(
    snap: &gix::config::Snapshot<'_>,
    branch: &str,
) -> Option<(String, String, String)> {
    let remote = snap
        .string(&format!("branch.{branch}.remote"))?
        .to_str_lossy()
        .into_owned();
    let merge = snap
        .string(&format!("branch.{branch}.merge"))?
        .to_str_lossy()
        .into_owned();
    let short = match merge.strip_prefix("refs/heads/") {
        Some(b) if remote == "." => b.to_string(),
        Some(b) => format!("{remote}/{b}"),
        None => merge.clone(),
    };
    Some((remote, merge, short))
}

/// `-u`/`--set-upstream-to`: point a branch's upstream at `<upstream>`. Operates
/// on the given branch, or the current one when no positional is present.
fn set_upstream(repo: &gix::Repository, o: &Opts, upstream_spec: &str) -> Result<ExitCode> {
    let branch_name = match o.names.first() {
        Some(n) => n.clone(),
        None => match repo.head_name()? {
            Some(h) => h.shorten().to_string(),
            None => {
                return fatal(format!(
                    "could not set upstream of HEAD to {upstream_spec} when it does not point to any branch"
                ))
            }
        },
    };

    let full = format!("refs/heads/{branch_name}");
    if repo.try_find_reference(full.as_str())?.is_none() {
        return fatal(format!("branch '{branch_name}' does not exist"));
    }

    let up = match resolve_upstream(repo, upstream_spec)? {
        Some(u) => u,
        None => {
            let code = fatal(format!(
                "the requested upstream branch '{upstream_spec}' does not exist"
            ))?;
            // `advise_if_enabled(ADVICE_SET_UPSTREAM_FAILURE, upstream_advice)`
            // in `branch.c`: the trailer is git's, not ours, so it appears only
            // while the slot is unconfigured.
            crate::advice::Advice::SetUpstreamFailure.advise_in(
                repo,
                "\nIf you are planning on basing your work on an upstream\n\
                 branch that already exists at the remote, you may need to\n\
                 run \"git fetch\" to retrieve it.\n\
                 \n\
                 If you are planning to push out a new local branch that\n\
                 will track its remote counterpart, you may want to use\n\
                 \"git push -u\" to set the upstream config as you push.",
            );
            return Ok(code);
        }
    };

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    install_tracking(repo, &branch_name, &up, o.quiet)?;
    Ok(ExitCode::SUCCESS)
}

/// Resolve an upstream spec to `(remote, merge_ref, short)`. A remote-tracking
/// ref maps to its remote and the remote-side branch; a local branch maps to the
/// `.` remote. `None` when the spec does not name a ref.
fn resolve_upstream(
    repo: &gix::Repository,
    spec: &str,
) -> Result<Option<(String, String, String)>> {
    let full: BString = match repo.find_reference(spec) {
        Ok(r) => r.name().as_bstr().to_owned(),
        Err(_) => return Ok(None),
    };
    let s = full.to_str_lossy();
    if let Some(rest) = s.strip_prefix("refs/remotes/") {
        if let Some((remote, branch)) = rest.split_once('/') {
            return Ok(Some((
                remote.to_string(),
                format!("refs/heads/{branch}"),
                format!("{remote}/{branch}"),
            )));
        }
    }
    if let Some(b) = s.strip_prefix("refs/heads/") {
        return Ok(Some((".".to_string(), s.to_string(), b.to_string())));
    }
    // Any other ref (e.g. a tag): git records it against the `.` remote.
    Ok(Some((".".to_string(), s.to_string(), spec.to_string())))
}

/// `--unset-upstream`: drop `branch.<name>.remote` and `branch.<name>.merge` for
/// the given branch (or the current one). Refuses a branch with no upstream.
fn unset_upstream(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let branch_name = match o.names.first() {
        Some(n) => n.clone(),
        None => match repo.head_name()? {
            Some(h) => h.shorten().to_string(),
            None => {
                return fatal("could not unset upstream of HEAD when it does not point to any branch")
            }
        },
    };

    let snap = repo.config_snapshot();
    let has_upstream = snap
        .string(&format!("branch.{branch_name}.remote"))
        .is_some()
        || snap
            .string(&format!("branch.{branch_name}.merge"))
            .is_some();
    if !has_upstream {
        return fatal(format!("branch '{branch_name}' has no upstream information"));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let path = repo.common_dir().join("config");
    let mut file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;
    if let Ok(mut section) = file.section_mut("branch", Some(BStr::new(branch_name.as_bytes()))) {
        while section.remove("remote").is_some() {}
        while section.remove("merge").is_some() {}
    }
    write_config(&path, &file)?;
    Ok(ExitCode::SUCCESS)
}

/// Write `branch.<name>.remote`/`branch.<name>.merge` (and, per
/// `branch.autoSetupRebase`, `.rebase`) into the local config, then print git's
/// `set up to track` notice on stdout unless `--quiet`. Called with the repo lock
/// held.
/// The tracking half of the child `git branch <new> <start>` that `worktree add`
/// spawns (`builtin/worktree.c:930-949`), which is where its `--[no-]track` ends
/// up: the option is an `OPT_PASSTHRU` pushed onto that child's command line
/// verbatim, so the mode's meaning, `branch.autoSetupMerge`/`autoSetupRebase`,
/// the "starting point is not a branch" refusal and the
/// `branch '<n>' set up to track '<u>'.` line all have to be this module's rather
/// than a second copy that drifts from it.
///
/// `track` is the bare distinction `worktree add` can express: `Some(true)` for
/// `--track`, `Some(false)` for `--no-track`, and `None` when neither was given —
/// which is not "do nothing" but [`Track::Unset`], the state in which
/// `branch.autoSetupMerge` decides. That case is the reason this runs
/// unconditionally: the child is `git branch <new> <start>` whether or not a
/// passthru was appended, so `autoSetupMerge = always` sets an upstream for a
/// plain `worktree add -b`.
///
/// `worktree add` cannot say `direct`/`inherit` explicitly: its option carries
/// `PARSE_OPT_NOARG`, so `--track=<anything>` is a parse-options error before this
/// is ever reached.
///
/// Returns `Some(code)` when git would fail, which it does *before* writing the
/// ref — hence a decision made here rather than after `create_branch`.
pub(crate) fn worktree_tracking(
    repo: &gix::Repository,
    new_branch: &str,
    start_ref: Option<&BStr>,
    start_name: &str,
    track: Option<bool>,
    quiet: bool,
) -> Result<Option<ExitCode>> {
    let track = match track {
        Some(true) => Track::Direct,
        Some(false) => Track::No,
        None => Track::Unset,
    };
    if let Some(code) = ambiguous_tracking(repo, start_ref, track)? {
        return Ok(Some(code));
    }
    let upstream = tracking_upstream(repo, start_ref, track, new_branch);
    if track == Track::Direct && upstream.is_none() {
        return Ok(Some(fatal(format!(
            "cannot set up tracking information; starting point '{start_name}' is not a branch"
        ))?));
    }
    if let Some(up) = upstream {
        install_tracking(repo, new_branch, &up, quiet)?;
    }
    Ok(None)
}

fn install_tracking(
    repo: &gix::Repository,
    branch: &str,
    upstream: &(String, String, String),
    quiet: bool,
) -> Result<()> {
    let (remote, merge_ref, short) = upstream;
    let path = repo.common_dir().join("config");
    let mut file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;
    let sub = BStr::new(branch.as_bytes());
    file.set_raw_value_by("branch", Some(sub), "remote", remote.as_str())?;
    file.set_raw_value_by("branch", Some(sub), "merge", merge_ref.as_str())?;

    let want_rebase = autosetup_rebase(repo, remote);
    if want_rebase {
        file.set_raw_value_by("branch", Some(sub), "rebase", "true")?;
    }

    write_config(&path, &file)?;

    if !quiet {
        // `printf_ln(rebasing ? _("branch '%s' set up to track '%s' by rebasing.") :
        //                       _("branch '%s' set up to track '%s'."), …)` (branch.c:168-171):
        // the same `rebasing` that wrote `branch.<name>.rebase` picks the wording, so a
        // `branch.autoSetupRebase` that took effect says so.
        println!("{}", tracking_line(branch, short, want_rebase));
    }
    Ok(())
}

/// `branch.autoSetupRebase` for one upstream: whether `install_branch_config()` records
/// `branch.<name>.rebase = true` — and therefore whether its notice says `by rebasing.`
///
/// `always` for either kind of upstream, `local` only for the `.` remote, `remote` only for a real
/// one, and `never` (the default, and anything unrecognised) for neither.
pub(super) fn autosetup_rebase(repo: &gix::Repository, remote: &str) -> bool {
    let is_local = remote == ".";
    match repo
        .config_snapshot()
        .string("branch.autoSetupRebase")
        .map(|v| v.to_str_lossy().into_owned())
        .as_deref()
    {
        Some("always") => true,
        Some("local") => is_local,
        Some("remote") => !is_local,
        _ => false,
    }
}

/// The notice `install_branch_config_multiple_remotes()` prints for a single upstream
/// (branch.c:168-171). One function because git has one `printf_ln`, and the `by rebasing.`
/// half went missing from every copy of it in this port.
pub(super) fn tracking_line(branch: &str, short: &str, rebasing: bool) -> String {
    match rebasing {
        true => format!("branch '{branch}' set up to track '{short}' by rebasing."),
        false => format!("branch '{branch}' set up to track '{short}'."),
    }
}

/// Append the `<tip> <tip>` entry a rename or copy onto the branch's *own* name
/// leaves in that branch's reflog.
///
/// `files_copy_or_rename_ref()` is not a ref transaction: it ends in
/// `commit_ref_update()`, which calls `files_log_ref_write()` unconditionally, so
/// the destination is logged even when it already held the value. A transaction
/// takes the other path — `lock_ref_for_update()` withholds `REF_NEEDS_COMMIT`
/// when `oideq(&lock->old_oid, &update->new_oid)`, and `files_transaction_finish()`
/// logs only `REF_NEEDS_COMMIT || REF_LOG_ONLY` updates — which is why
/// `update-ref refs/heads/main <its own tip>` and `branch -f` leave the branch's
/// log untouched while `branch -m main main` does not.
///
/// `RefLog::Only` is that `REF_LOG_ONLY`: it writes the log and not the ref, which
/// is right because the ref already holds `target`, and — like git's flag in
/// `split_head_update()` — it is exempt from the `HEAD` mirror, whose entry the
/// caller's own update already produced.
///
/// The entry's own "old" side is patched in afterwards rather than demanded up
/// front: a `PreviousValue::MustExistAndMatch` constraint is checked against the
/// loose reference alone and fails outright on a branch that lives only in
/// `packed-refs`, which is every branch of a fresh bare clone.
fn log_unchanged_rename(
    repo: &gix::Repository,
    name: &FullName,
    target: ObjectId,
    message: &str,
    force_create_reflog: bool,
) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::Only,
                force_create_reflog,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(target),
        },
        name: name.clone(),
        deref: false,
    })?;
    rewrite_last_reflog_old_id(
        &repo
            .git_dir()
            .join("logs")
            .join(name.as_bstr().to_str_lossy().as_ref()),
        target,
    );
    Ok(())
}

/// `-m`/`-M`: rename a branch, carrying its reflog and `branch.<name>.*` config
/// across and re-pointing HEAD when the renamed branch is the checked-out one.
///
/// With one positional the current branch is renamed; with two, the first names
/// the branch to rename. git's reflog is a file keyed by ref name, so the rename
/// is a file move followed by a normal update — that preserves history where a
/// delete-and-create would drop it.
fn rename_branch(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let (old, new) = match o.names.len() {
        0 => return fatal("branch name required"),
        1 => {
            let Some(head) = repo.head_name()? else {
                return fatal("cannot rename the current branch while not on any");
            };
            (head.shorten().to_string(), o.names[0].clone())
        }
        2 => (o.names[0].clone(), o.names[1].clone()),
        _ => return fatal("too many arguments for a rename operation"),
    };

    let old_full = format!("refs/heads/{old}");
    let new_full = format!("refs/heads/{new}");

    if !valid_branch_name(&new) {
        let code = fatal(format!("'{new}' is not a valid branch name"))?;
        ref_syntax_hints(repo);
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut old_ref = match repo.try_find_reference(old_full.as_str())? {
        Some(r) => r,
        None => return fatal(format!("no branch named '{old}'")),
    };
    if old_full != new_full && repo.try_find_reference(new_full.as_str())?.is_some() && !o.force {
        return fatal(format!("a branch named '{new}' already exists"));
    }
    let target = old_ref.peel_to_id()?.detach();

    let old_name: FullName = old_full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{old}': {e}"))?;
    let new_name: FullName = new_full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{new}': {e}"))?;

    let head_follows = repo.head_name()?.map(|n| n == old_name).unwrap_or(false);
    let message = format!("Branch: renamed {old_full} to {new_full}");

    // Move the reflog first so the update below appends to the carried-over
    // history rather than starting a fresh log.
    if old_full != new_full {
        let logs = repo.git_dir().join("logs");
        let from = logs.join(&old_full);
        let to = logs.join(&new_full);
        if from.exists() {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&from, &to)?;
        }
    }

    // `files_copy_or_rename_ref()` deletes the old name before writing the new one, so the
    // `<tip> <null>` half always comes first in `.git/logs/HEAD`. When the two names differ
    // that half falls out of the deletion below, through `split_head_update()`. Renaming a
    // branch onto its own name performs no deletion at all, so it is written here — ahead of
    // the update whose own head-split supplies the `<tip> <tip>` half.
    if head_follows && old_full == new_full {
        super::checkout::append_head_log(repo, Some(target), None, &message);
    }

    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: o.create_reflog,
                message: message.clone().into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(target),
        },
        name: new_name.clone(),
        deref: false,
    })?;
    // The ref is new to the ref store, so gitoxide logs it as a creation; git renamed a
    // *ref that already pointed there*, and its entry reads `<tip> <tip>`.
    if old_full != new_full {
        rewrite_last_reflog_old_id(&repo.git_dir().join("logs").join(&new_full), target);
    } else {
        log_unchanged_rename(repo, &new_name, target, &message, o.create_reflog)?;
    }

    if old_full != new_full {
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
                // `files_copy_or_rename_ref()` passes `logmsg` to `refs_delete_ref()`, so when
                // `HEAD` is symbolic to the branch being renamed the deletion's `REF_LOG_ONLY`
                // mirror lands in `.git/logs/HEAD` carrying this message. That mirror is the
                // `<tip> <null>` half of the pair below.
                message: message.clone().into(),
            },
            name: old_name,
            deref: false,
        })?;
        // git renames the branch's config section along with the ref.
        move_branch_config(repo, &old, &new, true)?;
    }

    if head_follows {
        // HEAD is symbolic to the branch being renamed, so both halves of the rename are
        // mirrored into its log: the old name going away, then the new one arriving.
        // `refs_rename_ref()` performs the delete and the create, and each is logged
        // through the symref.
        //
        // Only the *create* half is left to write, and only when the name changed: the
        // deletion above already contributed its `<tip> <null>` line via
        // `split_head_update()`, and this create splits off nothing because `HEAD` still
        // names the old branch rather than the new one. A same-name rename took the other
        // route — its pair is the manual line before the update plus that update's own
        // head-split — so nothing more belongs here.
        if old_full != new_full {
            super::checkout::append_head_log(repo, None, Some(target), &message);
        }
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: message.into(),
                },
                expected: PreviousValue::Any,
                new: Target::Symbolic(new_name),
            },
            name: "HEAD"
                .try_into()
                .map_err(|e| anyhow!("invalid ref name 'HEAD': {e}"))?,
            deref: false,
        })?;
    }

    Ok(ExitCode::SUCCESS)
}

/// `-c`/`-C`: copy a branch, duplicating its reflog and `branch.<name>.*` config
/// into the new name and leaving the source (and HEAD) untouched.
///
/// With one positional the current branch is copied; with two, the first names
/// the source. `-C` allows overwriting an existing target.
fn copy_branch(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let (old, new) = match o.names.len() {
        0 => return fatal("branch name required"),
        1 => {
            let Some(head) = repo.head_name()? else {
                return fatal("cannot copy the current branch while not on any");
            };
            (head.shorten().to_string(), o.names[0].clone())
        }
        2 => (o.names[0].clone(), o.names[1].clone()),
        _ => return fatal("too many branches for a copy operation"),
    };

    let old_full = format!("refs/heads/{old}");
    let new_full = format!("refs/heads/{new}");

    if !valid_branch_name(&new) {
        let code = fatal(format!("'{new}' is not a valid branch name"))?;
        ref_syntax_hints(repo);
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut old_ref = match repo.try_find_reference(old_full.as_str())? {
        Some(r) => r,
        None => return fatal(format!("no branch named '{old}'")),
    };
    if old_full != new_full && repo.try_find_reference(new_full.as_str())?.is_some() && !o.force {
        return fatal(format!("a branch named '{new}' already exists"));
    }
    let target = old_ref.peel_to_id()?.detach();

    let new_name: FullName = new_full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{new}': {e}"))?;

    // Copy the reflog file first so the update below appends its "copied" entry to
    // the carried-over history rather than starting a fresh log.
    if old_full != new_full {
        let logs = repo.git_dir().join("logs");
        let from = logs.join(&old_full);
        let to = logs.join(&new_full);
        if from.exists() {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to)?;
        }
    }

    let message = format!("Branch: copied {old_full} to {new_full}");
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: o.create_reflog,
                message: message.clone().into(),
            },
            expected: if o.force {
                PreviousValue::Any
            } else {
                PreviousValue::MustNotExist
            },
            new: Target::Object(target),
        },
        name: new_name.clone(),
        deref: false,
    })?;
    // Copying a branch onto its own name changes no value, so the update above logged
    // nothing; git's copy ends in the same unconditional `commit_ref_update()` a rename
    // does and records the entry regardless.
    if old_full == new_full {
        log_unchanged_rename(repo, &new_name, target, &message, o.create_reflog)?;
    }

    // git duplicates the branch's config section into the new name.
    if old_full != new_full {
        move_branch_config(repo, &old, &new, false)?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Copy every `branch.<old>.*` value into `branch.<new>.*` in the local config.
/// When `remove_old`, the old subsection is deleted afterward (a rename); a copy
/// leaves it in place. Mirrors git's `git_config_copy_section` /
/// `git_config_rename_section` for the `branch.<name>` section.
fn move_branch_config(
    repo: &gix::Repository,
    old: &str,
    new: &str,
    remove_old: bool,
) -> Result<()> {
    let path = repo.common_dir().join("config");
    let mut file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;

    // Gather the old subsection's key/value pairs in order, as owned data so the
    // immutable borrow ends before the mutation below.
    let mut pairs: Vec<(String, String)> = Vec::new();
    if let Some(iter) = file.sections_by_name("branch") {
        for section in iter {
            if section.header().subsection_name() == Some(BStr::new(old.as_bytes())) {
                for name in section.value_names() {
                    for value in section.values(&name) {
                        pairs.push((name.clone(), value.to_str_lossy().into_owned()));
                    }
                }
            }
        }
    }

    if pairs.is_empty() && !remove_old {
        return Ok(());
    }

    if remove_old {
        // `git_config_rename_section()` rewrites the *header* and leaves the section where it is,
        // so `branch -m` keeps the file's section order. Appending a new section and deleting the
        // old one moves it to the end instead, which shows up in `git config --list` order and in
        // any diff of `.git/config`.
        let renamed = file.rename_section(
            "branch",
            Some(BStr::new(old.as_bytes())),
            "branch",
            Some(gix::bstr::BString::from(new.as_bytes())),
        );
        // A branch with no config section of its own has nothing to rename, which is not an error.
        if let Err(err) = renamed {
            if !matches!(
                err,
                gix::config::file::rename_section::Error::Lookup(
                    gix::config::lookup::existing::Error::SectionMissing
                        | gix::config::lookup::existing::Error::SubSectionMissing
                        | gix::config::lookup::existing::Error::KeyMissing
                )
            ) {
                return Err(err.into());
            }
        }
    } else if !pairs.is_empty() {
        // `-c`/`-C` copies the section rather than moving it, so the copy is appended — which is
        // where `git config --list` shows it after `git branch -c`.
        let sub = BStr::new(new.as_bytes());
        let mut section = file.section_mut_or_create_new("branch", Some(sub))?;
        for (key, value) in &pairs {
            section.push(key.as_str(), value.as_str())?;
        }
    }

    write_config(&path, &file)?;
    Ok(())
}

/// Serialize `file` to `path` atomically: write a sibling temp file, then rename
/// over the target so a crash never leaves a half-written config.
fn write_config(path: &std::path::Path, file: &ConfigFile) -> Result<()> {
    let bytes = file.to_bstring();
    let tmp = path.with_extension("zvcs-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Delete one or more local branches. Without `-D`, a branch not reachable from
/// HEAD (not fully merged) is refused. The currently checked-out branch cannot
/// be deleted. Successfully deleted branches are reported as
/// `Deleted branch <name> (was <abbrev>).` unless `-q`; git stops at the first
/// failure with exit 1, leaving earlier deletions committed.
fn delete_branches(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    if o.names.is_empty() {
        return fatal("branch name required");
    }

    // Serialize all deletions through the repo coordinator, held across the loop.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // ```c
    // switch (kinds) {
    // case FILTER_REFS_REMOTES:
    //         fmt = "refs/remotes/%s";
    //         remote_branch = 1;
    //         force = 1;
    //         break;
    // case FILTER_REFS_BRANCHES:
    //         fmt = "refs/heads/%s";
    // ```
    // (builtin/branch.c.) `-r` deletes remote-tracking refs, and force is implied there: a
    // remote-tracking branch has no upstream of its own to be "not fully merged" with.
    let remote_branch = o.mode == ListMode::Remotes;
    let force = o.force || remote_branch;
    let kind_word = if remote_branch { "remote-tracking branch" } else { "branch" };
    // git reports each failure and carries on to the next operand (`ret = 1; continue;`), so one
    // missing name does not hide the deletion of the ones that follow it.
    let mut status = ExitCode::SUCCESS;

    for name in &o.names {
        let full = match remote_branch {
            true => format!("refs/remotes/{name}"),
            false => format!("refs/heads/{name}"),
        };

        // `delete_branches()` (builtin/branch.c) refuses when `branch_checked_out(name)`
        // names a worktree: any worktree's `HEAD`, not just this one's, and the branch an
        // interrupted rebase or bisect will return to as well. A bare worktree contributes
        // nothing to that map — a bare repository's `HEAD` is a default for future clones,
        // not a checkout — so deleting the branch it names is allowed. That is the one way
        // `branch -d` reaches the deletion-of-`HEAD`'s referent path, where
        // `split_head_update()` then logs `<old> <null>` into `logs/HEAD` with no message —
        // `refs_delete_refs()` is called with a null `logmsg`.
        //
        // The reported path is the worktree's, which git derives absolutely from the common
        // dir; `repo.workdir()` is relative whenever the repository was discovered from the
        // current directory, and printed `.` or `../..` where git prints the checkout's
        // full path.
        // `if (kinds == FILTER_REFS_BRANCHES)`: the check is on local branches only, since no
        // worktree's `HEAD` can be on a remote-tracking ref.
        if !remote_branch {
            if let Some(path) = super::worktree::branch_checked_out(repo, &full)? {
                error_exit(format!(
                    "cannot delete branch '{name}' used by worktree at '{}'",
                    super::worktree::path_to_string(&path)
                ))?;
                status = ExitCode::from(1);
                continue;
            }
        }

        // `refs_resolve_ref_unsafe(..., RESOLVE_REF_READING | RESOLVE_REF_NO_RECURSE |
        // RESOLVE_REF_ALLOW_BAD_NAME, &oid, &flags)`: the ref's *recorded* value, with no
        // dereference and no object read. A branch pointing at an object that is not in the
        // repository is still deletable, which is most of the reason `-D` exists.
        let reference = match repo.try_find_reference(full.as_str())? {
            Some(r) => r,
            None => {
                error_exit(format!("{kind_word} '{name}' not found"))?;
                status = ExitCode::from(1);
                continue;
            }
        };
        let symref_target = reference
            .target()
            .try_name()
            .map(|n| n.as_bstr().to_str_lossy().into_owned());
        let recorded = reference.target().try_id().map(ToOwned::to_owned);

        // `(flags & REF_ISBROKEN) ? "broken" : (flags & REF_ISSYMREF) ? target : find_unique_abbrev()`
        // — the three spellings of `(was …)`.
        let (was, tip) = match (&symref_target, recorded) {
            (Some(target), _) => (target.clone(), None),
            (None, Some(id)) => {
                // `find_unique_abbrev()` on an id whose object is absent still answers: the
                // default length, no disambiguation pass.
                let abbrev = {
                    use gix::prelude::ObjectIdExt as _;
                    id.attach(repo)
                        .shorten()
                        .map(|p| p.to_string())
                        .unwrap_or_else(|_| id.to_hex_with_len(7).to_string())
                };
                (abbrev, Some(id))
            }
            (None, None) => ("broken".to_string(), None),
        };

        // `if (!(flags & (REF_ISSYMREF|REF_ISBROKEN)) && check_branch_commit(...))`: the
        // merged-into-HEAD test needs a commit, so a symbolic or broken ref skips it.
        if !force && tip.is_some() {
            let tip = tip.expect("checked");
            let merged = match repo.head_id() {
                Ok(head_id) => match repo.merge_base(tip, head_id.detach()) {
                    Ok(base) => base.detach() == tip,
                    Err(_) => false, // no common ancestor → not merged
                },
                Err(_) => false, // unborn HEAD → nothing merged into
            };
            if !merged {
                error_exit(format!("the branch '{name}' is not fully merged"))?;
                crate::advice::Advice::ForceDeleteBranch.advise_in(
                    repo,
                    &format!("If you are sure you want to delete it, run 'git branch -D {name}'"),
                );
                status = ExitCode::from(1);
                continue;
            }
        }

        let name_full: FullName = full
            .as_str()
            .try_into()
            .map_err(|e| anyhow!("invalid branch name '{name}': {e}"))?;
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
                message: Default::default(),
            },
            name: name_full,
            deref: false,
        })?;

        if !o.quiet {
            // `printf(remote_branch ? _("Deleted remote-tracking branch %s (was %s).\n") : …)`.
            let what = if remote_branch { "remote-tracking branch" } else { "branch" };
            println!("Deleted {what} {name} (was {was}).");
        }
    }

    Ok(status)
}

/// Point the last reflog entry's *old* id at `old`, which is what a rename records: the
/// ref did not come into being, it changed name while pointing where it already pointed.
fn rewrite_last_reflog_old_id(path: &std::path::Path, old: gix::ObjectId) {
    let Ok(body) = std::fs::read(path) else {
        return;
    };
    // Every line starts with the old id in hex, so only that field is replaced.
    let Some(start) = body
        .iter()
        .rposition(|b| *b == b'\n')
        .map(|nl| {
            // The last line is what follows the second-to-last newline.
            body[..nl]
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(0, |prev| prev + 1)
        })
    else {
        return;
    };
    let hex = old.to_hex().to_string();
    if body.len() < start + hex.len() {
        return;
    }
    let mut out = body.clone();
    out[start..start + hex.len()].copy_from_slice(hex.as_bytes());
    let _ = std::fs::write(path, out);
}

#[cfg(test)]
mod tests {
    use super::{resolve_long, valid_branch_name, Arg, Resolved, LONG_OPTS};

    /// Resolve a name the way the parse loop does, reporting the outcome as a
    /// short string so a whole table of cases reads at a glance.
    fn r(name: &str) -> String {
        match resolve_long(LONG_OPTS, name) {
            Resolved::One(opt, false) => opt.name.to_string(),
            Resolved::One(opt, true) => format!("no-{}", opt.name),
            Resolved::Ambiguous(a, b) => format!("ambiguous:{a}|{b}"),
            Resolved::Unknown => "unknown".to_string(),
        }
    }

    /// The whole point of the table: a name git does not resolve must come back
    /// `Unknown` so the caller can refuse it, rather than falling through to the
    /// positional arm and becoming a branch to create.
    ///
    /// `git branch --bogus` used to exit 0 having created `refs/heads/--bogus`.
    #[test]
    fn an_unresolvable_name_is_unknown_not_an_operand() {
        for name in ["bogus", "zzbogus", "colour", "no-all", "no-remotes", "unknown-thing"] {
            assert_eq!(r(name), "unknown", "--{name} must not resolve");
        }
    }

    /// `-a`/`--all`, `-r`/`--remotes` and the `--contains` family carry
    /// `PARSE_OPT_NONEG`, so their `no-` forms are unknown rather than negations
    /// — which is exactly why `--no-all` cannot be waved through as a negation
    /// of something.
    #[test]
    fn noneg_entries_have_no_negation() {
        assert_eq!(r("all"), "all");
        assert_eq!(r("remotes"), "remotes");
        assert_eq!(r("no-all"), "unknown");
        assert_eq!(r("no-remotes"), "unknown");
        // `no-contains` and `no-merged` are entries in their own right, not
        // negations of `contains`/`merged`.
        assert_eq!(r("no-contains"), "no-contains");
        assert_eq!(r("no-merged"), "no-merged");
    }

    /// Unique-prefix abbreviation, which stock accepts. Getting this wrong in
    /// the rejecting direction is the other half of the bug: `--verb` is a real
    /// spelling of `--verbose`, so answering `unknown option` would be a
    /// fabricated refusal of something git honours.
    #[test]
    fn unique_prefixes_resolve_to_the_full_option() {
        assert_eq!(r("verb"), "verbose");
        assert_eq!(r("ver"), "verbose");
        assert_eq!(r("forc"), "force");
        assert_eq!(r("abbre"), "abbrev");
        assert_eq!(r("omit"), "omit-empty");
        assert_eq!(r("show"), "show-current");
        // Negated abbreviations resolve too.
        assert_eq!(r("no-verb"), "no-verbose");
        assert_eq!(r("no-forc"), "no-force");
    }

    /// The two candidates an ambiguous abbreviation names, and their order, come
    /// from `parse_long_opt()` keeping the last two matches as it walks the
    /// table — so [`super::LONG_OPTS`] has to stay in `builtin/branch.c` order.
    /// Every expectation here was taken from stock git 2.55.0.
    #[test]
    fn ambiguous_prefixes_name_the_last_two_candidates_in_table_order() {
        assert_eq!(r("c"), "ambiguous:create-reflog|column");
        assert_eq!(r("col"), "ambiguous:color|column");
        assert_eq!(r("s"), "ambiguous:show-current|sort");
        assert_eq!(r("wit"), "ambiguous:with|without");
        // A name that is itself a prefix of `no-` makes every negatable option a
        // candidate ("negated and abbreviated very much").
        assert_eq!(r("n"), "ambiguous:no-recurse-submodules|no-format");
        assert_eq!(r("no"), "ambiguous:no-recurse-submodules|no-format");
    }

    /// `--set-upstream`, `--with` and `--without` are `PARSE_OPT_HIDDEN`: absent
    /// from the usage block but resolved like any other, so they must not be
    /// mistaken for unknown names.
    #[test]
    fn hidden_options_still_resolve() {
        assert_eq!(r("set-upstream"), "set-upstream");
        assert_eq!(r("with"), "with");
        assert_eq!(r("without"), "without");
    }

    /// The value shape drives whether a detached value is consumed, so an entry
    /// with the wrong `Arg` silently eats the next operand (or fails to).
    #[test]
    fn value_shapes_match_the_option_table() {
        let arg = |name: &str| match resolve_long(LONG_OPTS, name) {
            Resolved::One(opt, _) => opt.arg,
            _ => panic!("--{name} did not resolve"),
        };
        assert!(arg("force") == Arg::None);
        assert!(arg("sort") == Arg::Required);
        assert!(arg("format") == Arg::Required);
        assert!(arg("points-at") == Arg::Required);
        // PARSE_OPT_OPTARG: an attached value only.
        assert!(arg("track") == Arg::Optional);
        assert!(arg("color") == Arg::Optional);
        assert!(arg("abbrev") == Arg::Optional);
        assert!(arg("column") == Arg::Optional);
        // PARSE_OPT_LASTARG_DEFAULT.
        assert!(arg("contains") == Arg::LastArg);
        assert!(arg("merged") == Arg::LastArg);
        assert!(arg("with") == Arg::LastArg);
    }

    /// `check_branch_ref()` rejects a leading dash before `check_refname_format()`
    /// ever runs. gitoxide only implements the latter, so without the extra rule
    /// `git branch -- -foo` created `refs/heads/-foo` and `git branch -m -- -bad`
    /// renamed the current branch to `-bad`.
    #[test]
    fn a_leading_dash_is_not_a_valid_branch_name() {
        for name in ["-foo", "--bogus", "-", "--", "-Z"] {
            assert!(!valid_branch_name(name), "{name} must be refused");
        }
        // The other two rules `check_branch_ref()` applies.
        assert!(!valid_branch_name("HEAD"));
        assert!(!valid_branch_name("..dots"));
        // …and names that are fine keep working.
        for name in ["main", "topic", "feature/x", "a-b", "wip-2"] {
            assert!(valid_branch_name(name), "{name} must be accepted");
        }
    }

    /// A dash mid-name is not a leading dash.
    #[test]
    fn only_the_first_character_triggers_the_dash_rule() {
        assert!(valid_branch_name("x-y"));
        assert!(valid_branch_name("release-1.0"));
    }
}
