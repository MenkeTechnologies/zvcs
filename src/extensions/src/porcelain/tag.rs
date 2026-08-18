//! `git tag` — list, create (lightweight and annotated), delete, and filter tags.
//!
//! Served natively via the vendored gitoxide crates so tools on PATH observe
//! the same ref store. Implemented forms (matching stock `git tag`):
//!
//! ```text
//!   * `git tag`                       → list every tag, one short name per line,
//!                                       sorted ascending by refname.
//!   * `git tag -l|--list [<pattern>…]`→ list, keeping tags whose *short* name
//!                                       matches any pattern (git's `wildmatch`
//!                                       without `WM_PATHNAME`, so `*` spans `/`).
//!   * `git tag -n[<num>]`             → append the first `<num>` lines (default 1)
//!                                       of each tag's message; implies listing.
//!   * `git tag --sort=[-][version:]<field>` → multi-level sort. Fields backed:
//!                                       `refname`, `version:refname`/`v:refname`,
//!                                       `taggerdate`/`committerdate`/`authordate`/
//!                                       `creatordate`, `objectname`/`objecttype`/
//!                                       `objectsize`, `taggername`/`committername`/
//!                                       `authorname`, the matching `*email`,
//!                                       `creator`, `subject`/`body`/`contents`.
//!   * `git tag --format=<fmt>`        → render each tag through `<fmt>`.
//!   * `git tag --contains/--no-contains/--merged/--no-merged/--points-at`
//!                                       → ancestry / points-at listing filters
//!                                       (`--with`/`--without` are hidden aliases
//!                                       for `--contains`/`--no-contains`).
//!   * `git tag --no-<flag>`             → the negations git's parse-options accepts:
//!                                       `--no-annotate`/`--no-force`/`--no-ignore-case`/
//!                                       `--no-omit-empty`/`--no-color`/`--no-file`/
//!                                       `--no-format`/`--no-cleanup`/`--no-sign`/
//!                                       `--no-local-user`/`--no-edit`/`--no-column`/
//!                                       `--no-points-at` (drops the points-at filter) and
//!                                       `--no-sort` (clears every CLI *and* `tag.sort`
//!                                       config sort key, falling back to refname order).
//!   * `git tag --column[=<opts>] | --no-column` → lay the tag list out in columns
//!                                       through the same engine `git column` uses
//!                                       (padding 2); honors `column.ui`/`column.tag`,
//!                                       resolves `auto` against the terminal, and is
//!                                       mutually exclusive with `-n`.
//!   * `git tag -i|--ignore-case`      → case-insensitive match and sort.
//!   * `git tag --omit-empty`          → drop refs whose `--format` output is empty.
//!   * `git tag <name> [<commit>]`     → create a lightweight tag at `<commit>`.
//!   * `git tag -a|-m|-F …`            → create an annotated tag object.
//!   * `git tag --cleanup=<mode>`      → `verbatim`/`whitespace`/`strip` message
//!                                       cleanup for `-m`/`-F`.
//!   * `git tag --trailer <tok>[(=|:)<val>]…` → append/merge trailers into the tag
//!                                       message before `--cleanup` runs, through
//!                                       the very engine `git interpret-trailers`
//!                                       drives (with `--no-divider`, so a `---`
//!                                       line in the body never ends it). Honors
//!                                       every `trailer.*` config key that command
//!                                       does, and implies an annotated tag.
//!                                       `--no-trailer` drops the trailers gathered
//!                                       so far.
//!   * `git tag -f …`                  → force, printing the `Updated tag` line.
//!   * `git tag --create-reflog …`     → force-create the tag's reflog, writing git's
//!                                       `tag: tagging <abbrev> (<subject>, <date>)` line.
//!   * `git tag -d <name>…`            → delete each tag.
//! ```
//!
//! Exit codes follow git: fatal errors exit 128, a bad object name for a filter
//! exits 129, and a failed delete exits 1.
//!
//! Config read here: the multi-valued `tag.sort` (default listing order), and the
//! two signing switches `tag.gpgSign` / `tag.forceSignAnnotated` — both of which
//! ask for a GPG-signed tag object, so they are honored by refusing exactly as an
//! explicit `-s` does rather than by quietly writing an unsigned tag.
//!
//! Genuinely not backed here, and refused rather than faked: cryptographic
//! signing (`-s`, `-u`, `tag.gpgSign`, `tag.forceSignAnnotated`) and verification
//! (`-v`), an editor-supplied message (`-a` with neither `-m` nor `-F`, `-e`),
//! forced ANSI color (`--color`/`--color=always`), the git gecos identity
//! fallback, and `--format` atoms outside the set handled by [`render_atom`]
//! (`align`, `describe`, `upstream`, relative/custom dates, …).

use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::glob::wildmatch;
use gix::glob::wildmatch::Mode;
use gix::hash::ObjectId;
use gix::objs::{CommitRef, Kind, TagRef};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use crate::refsort::{self, Prereleases};

/// `usage_with_options()` over `builtin/tag.c`'s option table.
/// `cmd_tag()`'s `struct option options[]` (builtin/tag.c), in table order, as
/// [`super::resolve_long`] reads it.
///
/// The `OPT_CMDMODE`s (`--list`, `--delete`, `--verify`), `-m`/`--message`, and
/// the `--contains`/`--no-contains`/`--with`/`--without`/`--merged`/`--no-merged`
/// family all carry `PARSE_OPT_NONEG`, so none of them has a `--no-` spelling.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "list",                        neg: false, arg: super::Arg::None },
    // `{ .type = OPTION_INTEGER, .short_name = 'n' }` — short-only, no entry.
    super::LongOpt { name: "delete",                      neg: false, arg: super::Arg::None },
    super::LongOpt { name: "verify",                      neg: false, arg: super::Arg::None },
    super::LongOpt { name: "annotate",                    neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "message",                     neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "file",                        neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "trailer",                     neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "edit",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "sign",                        neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "cleanup",                     neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "local-user",                  neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "force",                       neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "create-reflog",               neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "column",                      neg: true,  arg: super::Arg::Optional },
    super::LongOpt { name: "contains",                    neg: false, arg: super::Arg::LastArg },
    super::LongOpt { name: "no-contains",                 neg: false, arg: super::Arg::LastArg },
    super::LongOpt { name: "with",                        neg: false, arg: super::Arg::LastArg },
    super::LongOpt { name: "without",                     neg: false, arg: super::Arg::LastArg },
    super::LongOpt { name: "merged",                      neg: false, arg: super::Arg::LastArg },
    super::LongOpt { name: "no-merged",                   neg: false, arg: super::Arg::LastArg },
    super::LongOpt { name: "omit-empty",                  neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "sort",                        neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "points-at",                   neg: true,  arg: super::Arg::LastArg },
    super::LongOpt { name: "format",                      neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "color",                       neg: true,  arg: super::Arg::Optional },
    super::LongOpt { name: "ignore-case",                 neg: true,  arg: super::Arg::None },
];

const USAGE: &str = r"usage: git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]
               [(--trailer <token>[(=|:)<value>])...]
               <tagname> [<commit> | <object>]
   or: git tag -d <tagname>...
   or: git tag [-n[<num>]] -l [--contains <commit>] [--no-contains <commit>]
               [--points-at <object>] [--column[=<options>] | --no-column]
               [--create-reflog] [--sort=<key>] [--format=<format>]
               [--merged <commit>] [--no-merged <commit>] [<pattern>...]
   or: git tag -v [--format=<format>] <tagname>...

    -l, --list            list tag names
    -n[<n>]               print <n> lines of each tag message
    -d, --delete          delete tags
    -v, --verify          verify tags

Tag creation options
    -a, --[no-]annotate   annotated tag, needs a message
    -m, --message <message>
                          tag message
    -F, --[no-]file <file>
                          read message from file
    --[no-]trailer <trailer>
                          add custom trailer(s)
    -e, --[no-]edit       force edit of tag message
    -s, --[no-]sign       annotated and GPG-signed tag
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    -u, --[no-]local-user <key-id>
                          use another key to sign the tag
    -f, --[no-]force      replace the tag if exists
    --[no-]create-reflog  create a reflog

Tag listing options
    --[no-]column[=<style>]
                          show tag list in columns
    --contains <commit>   print only tags that contain the commit
    --no-contains <commit>
                          print only tags that don't contain the commit
    --merged <commit>     print only tags that are merged
    --no-merged <commit>  print only tags that are not merged
    --[no-]omit-empty     do not output a newline after empty formatted refs
    --[no-]sort <key>     field name to sort on
    --[no-]points-at <object>
                          print only tags of the object
    --[no-]format <format>
                          format to use for the output
    --[no-]color[=<when>] respect format colors
    -i, --[no-]ignore-case
                          sorting and filtering are case insensitive

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--with`, `--without`.
/// Captured byte-for-byte from stock git 2.55.0's `git tag --help-all`.
const USAGE_ALL: &str = r#"usage: git tag [-a | -s | -u <key-id>] [-f] [-m <msg> | -F <file>] [-e]
               [(--trailer <token>[(=|:)<value>])...]
               <tagname> [<commit> | <object>]
   or: git tag -d <tagname>...
   or: git tag [-n[<num>]] -l [--contains <commit>] [--no-contains <commit>]
               [--points-at <object>] [--column[=<options>] | --no-column]
               [--create-reflog] [--sort=<key>] [--format=<format>]
               [--merged <commit>] [--no-merged <commit>] [<pattern>...]
   or: git tag -v [--format=<format>] <tagname>...

    -l, --list            list tag names
    -n[<n>]               print <n> lines of each tag message
    -d, --delete          delete tags
    -v, --verify          verify tags

Tag creation options
    -a, --[no-]annotate   annotated tag, needs a message
    -m, --message <message>
                          tag message
    -F, --[no-]file <file>
                          read message from file
    --[no-]trailer <trailer>
                          add custom trailer(s)
    -e, --[no-]edit       force edit of tag message
    -s, --[no-]sign       annotated and GPG-signed tag
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    -u, --[no-]local-user <key-id>
                          use another key to sign the tag
    -f, --[no-]force      replace the tag if exists
    --[no-]create-reflog  create a reflog

Tag listing options
    --[no-]column[=<style>]
                          show tag list in columns
    --contains <commit>   print only tags that contain the commit
    --no-contains <commit>
                          print only tags that don't contain the commit
    --with <commit>       print only tags that contain the commit
    --without <commit>    print only tags that don't contain the commit
    --merged <commit>     print only tags that are merged
    --no-merged <commit>  print only tags that are not merged
    --[no-]omit-empty     do not output a newline after empty formatted refs
    --[no-]sort <key>     field name to sort on
    --[no-]points-at <object>
                          print only tags of the object
    --[no-]format <format>
                          format to use for the output
    --[no-]color[=<when>] respect format colors
    -i, --[no-]ignore-case
                          sorting and filtering are case insensitive

"#;

/// A parsed actor identity captured from a tag/commit header.
#[derive(Clone)]
struct Sig {
    name: BString,
    email: BString,
    time: gix::date::Time,
}

/// Everything about one tag ref needed to sort and render it, gathered once.
struct Facts {
    /// Full ref name, e.g. `refs/tags/v1.0`.
    full: BString,
    /// Short name, e.g. `v1.0`.
    short: BString,
    /// The ref's own target — the tag object for an annotated tag.
    id: ObjectId,
    dir_kind: Kind,
    dir_size: u64,
    /// The ultimate non-tag object reached by peeling, set only when `id` is a tag.
    peel_id: Option<ObjectId>,
    peel_kind: Option<Kind>,
    peel_size: Option<u64>,
    /// Tagger — only present when the direct object is an annotated tag.
    tagger: Option<Sig>,
    /// Committer/author — only present when the direct object is a commit.
    committer: Option<Sig>,
    author: Option<Sig>,
    /// The direct object's message (tag or commit message), empty otherwise.
    message: Vec<u8>,
    /// The peeled commit's committer/author/message, set only when peeling a tag
    /// reaches a commit — powers the `*`-dereference format/sort atoms.
    peel_committer: Option<Sig>,
    peel_author: Option<Sig>,
    peel_message: Vec<u8>,
    /// Precomputed sort keys, aligned with the parsed `--sort` list.
    keys: Vec<SortVal>,
}

/// The set of resolved listing filters.
#[derive(Default)]
struct Filters {
    points_at: Option<ObjectId>,
    contains: Option<ObjectId>,
    no_contains: Option<ObjectId>,
    merged: Option<ObjectId>,
    no_merged: Option<ObjectId>,
}

impl Filters {
    fn any(&self) -> bool {
        self.points_at.is_some()
            || self.contains.is_some()
            || self.no_contains.is_some()
            || self.merged.is_some()
            || self.no_merged.is_some()
    }
}

pub fn tag(args: &[String]) -> Result<ExitCode> {
    let mut delete = false;
    let mut list = false;
    let mut force = false;
    let mut annotate = false;
    let mut ignore_case = false;
    let mut omit_empty = false;
    let mut create_reflog = false;
    let mut want_color = false;
    // Column layout state, seeded from `column.ui` / `column.tag` before the
    // command line is parsed so a `--column` flag overrides the config (git's
    // `git_column_config` runs during config, `parseopt_column_callback` after).
    let mut colopts: u32 = super::column::DISABLED;
    if let Err(msg) = super::column::config_colopts(&mut colopts, "tag") {
        eprint!("{msg}");
        return Ok(ExitCode::from(128));
    }
    let mut lines: Option<usize> = None;
    let mut sorts: Vec<String> = Vec::new();
    // Set once `--no-sort` is seen: git's `OPT_STRING_LIST` negation clears the
    // accumulated sort list *and* the `tag.sort` config values already loaded, so
    // the config fallback below must be suppressed too.
    let mut sort_negated = false;
    let mut format: Option<String> = None;
    let mut cleanup: Option<String> = None;
    let mut messages: Vec<Vec<u8>> = Vec::new();
    let mut message_file: Option<String> = None;
    // `--trailer` arguments in command-line order; git keeps them in a `strvec`
    // and hands the whole list to the trailer engine once the message is final.
    let mut trailers: Vec<String> = Vec::new();
    let mut positionals: Vec<&str> = Vec::new();
    let mut operands_only = false;

    // Raw (unresolved) filter operands, resolved once the repository is open.
    let mut points_at: Option<String> = None;
    let mut contains: Option<String> = None;
    let mut no_contains: Option<String> = None;
    let mut merged: Option<String> = None;
    let mut no_merged: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let typed = args[i].as_str();
        let a = typed;
        i += 1;
        if operands_only || !a.starts_with('-') || a == "-" {
            positionals.push(typed);
            continue;
        }
        // Respell a unique abbreviation as the name it resolves to, so an
        // abbreviation lands on the arm its full spelling lands on.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        match a {
            "--" => operands_only = true,
            // parse_options_step()'s `internal_help`: the block on stdout at
            // 129, with no `error:` line ahead of it.
            "-h" => return Ok(super::show_usage(USAGE)),
            // `if (internal_help && !strcmp(arg + 2, "help-all"))`
            // (parse-options.c:1122): an exact match, never an abbreviation and
            // never with an `=<value>`, rendering `USAGE_FULL`.
            "--help-all" => return Ok(super::show_usage(USAGE_ALL)),
            "-d" | "--delete" => delete = true,
            "-l" | "--list" => list = true,
            "-f" | "--force" => force = true,
            "--no-force" => force = false,
            "-a" | "--annotate" => annotate = true,
            "--no-annotate" => annotate = false,
            "-i" | "--ignore-case" => ignore_case = true,
            "--no-ignore-case" => ignore_case = false,
            "--omit-empty" => omit_empty = true,
            "--no-omit-empty" => omit_empty = false,
            "--create-reflog" => create_reflog = true,
            "--no-create-reflog" => create_reflog = false,
            "--color" => want_color = true,
            "--no-color" => want_color = false,
            // Negations of unsupported creation flags: the feature they toggle is
            // off already, so git accepts these and does nothing. Turning an option
            // off is never faking work, so unlike the positive `-s`/`-u`/`-e` these
            // are honored silently.
            "--no-sign" | "--no-local-user" | "--no-edit" => {}
            // `--column[=<opts>]` / `--no-column`: the list is laid out through the
            // same engine `git column` uses (see `list_tags`). `--no-column` is git's
            // "never", which leaves the default one-name-per-line output.
            "--column" => super::column::parseopt_column(&mut colopts, None, false)
                .map_err(|m| anyhow!("{m}"))?,
            "--no-column" => {
                let _ = super::column::parseopt_column(&mut colopts, None, true);
            }
            // Clear an accumulated message/file/format/cleanup the way git's
            // OPT_FILENAME / OPT_STRING negations do (each sets its target back to
            // NULL when unset).
            "--no-file" => message_file = None,
            // git's `OPT_STRVEC` unset empties the accumulated trailer list.
            "--no-trailer" => trailers.clear(),
            "--no-format" => format = None,
            "--no-cleanup" => cleanup = None,
            // git's `points-at` is a `parse_opt_object_name` callback whose unset
            // branch clears the oid array, dropping the filter entirely.
            "--no-points-at" => points_at = None,
            // git's `OPT_STRING_LIST` unset (`string_list_clear`) empties every
            // sort key gathered so far, CLI and `tag.sort` config alike.
            "--no-sort" => {
                sorts.clear();
                sort_negated = true;
            }
            // Hidden `--with`/`--without` aliases for `--contains`/`--no-contains`
            // (git's OPT_WITH/OPT_WITHOUT), same `LASTARG_DEFAULT` HEAD semantics.
            "--with" => contains = Some(optarg(args, &mut i)),
            "--without" => no_contains = Some(optarg(args, &mut i)),
            "-s" | "--sign" | "-u" | "--local-user" => {
                bail!("signed tags ({a}) are not supported")
            }
            "-v" | "--verify" => bail!("tag verification (-v) is not supported"),
            "-e" | "--edit" => bail!("editing tag messages (-e) is not supported"),
            "-n" => lines = Some(1),
            "--points-at" => points_at = Some(optarg(args, &mut i)),
            "--contains" => contains = Some(optarg(args, &mut i)),
            "--no-contains" => no_contains = Some(optarg(args, &mut i)),
            "--merged" => merged = Some(optarg(args, &mut i)),
            "--no-merged" => no_merged = Some(optarg(args, &mut i)),
            _ => {
                if let Some(rest) = a.strip_prefix("--sort=") {
                    sorts.push(rest.to_string());
                } else if a == "--sort" {
                    sorts.push(take_value(args, &mut i, "sort")?.to_string());
                } else if let Some(rest) = a.strip_prefix("--format=") {
                    format = Some(rest.to_string());
                } else if a == "--format" {
                    format = Some(take_value(args, &mut i, "format")?.to_string());
                } else if let Some(rest) = a.strip_prefix("--cleanup=") {
                    cleanup = Some(rest.to_string());
                } else if a == "--cleanup" {
                    cleanup = Some(take_value(args, &mut i, "cleanup")?.to_string());
                } else if let Some(rest) = a.strip_prefix("--column=") {
                    super::column::parseopt_column(&mut colopts, Some(rest), false)
                        .map_err(|m| anyhow!("{m}"))?;
                } else if let Some(rest) = a.strip_prefix("--color=") {
                    match rest {
                        "never" | "auto" => want_color = false,
                        _ => want_color = true,
                    }
                } else if let Some(rest) = a.strip_prefix("--points-at=") {
                    points_at = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--contains=") {
                    contains = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--with=") {
                    contains = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--no-contains=") {
                    no_contains = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--without=") {
                    no_contains = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--merged=") {
                    merged = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--no-merged=") {
                    no_merged = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--trailer=") {
                    trailers.push(rest.to_string());
                } else if a == "--trailer" {
                    trailers.push(take_value(args, &mut i, "trailer")?.to_string());
                } else if let Some(rest) = a.strip_prefix("--message=") {
                    messages.push(rest.as_bytes().to_vec());
                } else if a == "--message" || a == "-m" {
                    messages.push(take_value(args, &mut i, "message")?.as_bytes().to_vec());
                } else if let Some(rest) = a.strip_prefix("-m") {
                    messages.push(rest.as_bytes().to_vec());
                } else if let Some(rest) = a.strip_prefix("--file=") {
                    message_file = Some(rest.to_string());
                } else if a == "--file" || a == "-F" {
                    message_file = Some(take_value(args, &mut i, "file")?.to_string());
                } else if let Some(rest) = a.strip_prefix("-F") {
                    message_file = Some(rest.to_string());
                // A long name no table entry claims is `parse_options()`' own refusal
                // — the `error:` line and the block, both on stderr, exit 129 — not a
                // gap in this port. It is decided against the table rather than by
                // spelling, because the `OPT_CMDMODE`s, `-m`/`--message` and the
                // `--contains`/`--merged` family are `PARSE_OPT_NONEG` and so have no
                // `--no-` form for parse-options to resolve.
                } else if a.starts_with("--")
                    && matches!(
                        super::resolve_long(LONG_OPTS, &a[2..]),
                        super::Resolved::Unknown
                    )
                {
                    eprintln!("error: unknown option `{}'", &a[2..]);
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                } else if let Some(rest) = a.strip_prefix("-n") {
                    let n: usize = rest
                        .parse()
                        .map_err(|_| anyhow!("unsupported option {a:?}"))?;
                    lines = Some(n);
                } else if a.len() > 2
                    && !a.starts_with("--")
                    && a[1..2].chars().all(|c| "fadli".contains(c))
                {
                    // Bundled short flags, e.g. `-fam <msg>` = `-f -a -m <msg>`.
                    // git's parse-options treats each char as its own option; a
                    // value-taking one (`-m`/`-F`/`-n`) consumes the rest of the
                    // cluster, or the next argv element when it ends the cluster.
                    let cluster: Vec<char> = a[1..].chars().collect();
                    let mut ci = 0;
                    while ci < cluster.len() {
                        match cluster[ci] {
                            'f' => force = true,
                            'a' => annotate = true,
                            'd' => delete = true,
                            'l' => list = true,
                            'i' => ignore_case = true,
                            's' | 'u' => bail!("signed tags (-{}) are not supported", cluster[ci]),
                            'e' => bail!("editing tag messages (-e) is not supported"),
                            'v' => bail!("tag verification (-v) is not supported"),
                            c @ ('m' | 'F' | 'n') => {
                                let rest: String = cluster[ci + 1..].iter().collect();
                                let val = if rest.is_empty() {
                                    take_value(args, &mut i, "message")?.to_string()
                                } else {
                                    rest
                                };
                                match c {
                                    'm' => messages.push(val.into_bytes()),
                                    'F' => message_file = Some(val),
                                    _ => {
                                        lines = Some(if val.is_empty() {
                                            1
                                        } else {
                                            val.parse()
                                                .map_err(|_| anyhow!("unsupported option {a:?}"))?
                                        })
                                    }
                                }
                                break; // the value flag consumed the rest of the cluster
                            }
                            _ => bail!("unsupported option {a:?}"),
                        }
                        ci += 1;
                    }
                } else {
                    bail!("unsupported option {a:?}")
                }
            }
        }
    }

    // Without any `--sort` on the CLI, git falls back to the multi-valued
    // `tag.sort` config — each value adds a sort level — validated below exactly
    // like a CLI `--sort`.
    if sorts.is_empty() && !sort_negated {
        if let Ok(repo) = gix::discover(".") {
            sorts = repo
                .config_snapshot()
                .plumbing()
                .values::<gix::bstr::BString>("tag.sort")
                .unwrap_or_default()
                .into_iter()
                .map(|v| v.to_string())
                .collect();
        }
    }

    // git validates every `--sort` field name while parsing options, dying on the
    // first syntactically invalid key with exit 128. Reproduce that here so a bad
    // sort key fails the same way regardless of mode.
    let sort_keys = match resolve_sort(&sorts) {
        Err(SortErr::Fatal(msg)) => return fatal(&msg),
        Err(SortErr::Unsupported(spec)) => {
            bail!("--sort={spec} is not supported by this port")
        }
        Ok(keys) => keys,
    };

    // git renders `%(color:…)` only with color forced on. Emitting a byte-exact
    // ANSI stream would require porting git's whole color table, so the honest
    // move is to refuse forced color rather than fake it. The default (color off)
    // path strips color atoms exactly as git does when writing to a pipe.
    if want_color {
        bail!(
            "forced ANSI color (--color) is not supported: `%(color:<spec>)` needs \
             color.c's `color_parse_mem` name/attribute table to turn a spec into the \
             exact escape sequence, which is not ported"
        )
    }

    // Resolve `auto` against the terminal (git's `finalize_colopts(&colopts, -1)`).
    // A piped stdout leaves columns off, so the default output is unchanged. `-n`
    // and columns are mutually exclusive: an explicit `--column` is fatal, a config
    // -only "always" is silently downgraded (git's `explicitly_enable_column`).
    super::column::finalize(&mut colopts);
    if lines.is_some() && super::column::active(colopts) {
        if super::column::explicitly_enabled(colopts) {
            return fatal("options '--column' and '-n' cannot be used together");
        }
        colopts = super::column::DISABLED;
    }

    // The object this writes carries an identity, and git fills the halves
    // the user did not give rather than refusing — except under
    // `user.useConfigOnly`, which is the one case it says so.
    let mut repo = gix::discover(".")?;
    if let Some(code) = crate::ensure_object_identity(&mut repo, "Committer") {
        return Ok(code);
    }

    if delete {
        return delete_tags(&repo, &positionals);
    }

    // Resolve the listing filters now that the object database is open. Each
    // option keeps its own `parse_options()` callback's diagnostic and status:
    // `--points-at` is `parse_opt_object_name` (no odb lookup at all, so an
    // absent id is a filter that matches nothing), `--contains`/`--no-contains`
    // are `parse_opt_commits`, and `--merged`/`--no-merged` are
    // `parse_opt_merge_filter`, whose unresolvable-name case is a `die()`.
    let mut filters = Filters::default();
    if let Some(spec) = &points_at {
        match crate::objname::parse_opt_object_name(&repo, spec) {
            Ok(id) => filters.points_at = Some(id),
            Err(e) => return Ok(e.report()),
        }
    }
    for (raw, slot) in [
        (&contains, &mut filters.contains),
        (&no_contains, &mut filters.no_contains),
    ] {
        if let Some(spec) = raw {
            match crate::objname::parse_opt_commits(&repo, spec) {
                Ok(id) => *slot = Some(id),
                Err(e) => return Ok(e.report()),
            }
        }
    }
    for (raw, slot, long_name) in [
        (&merged, &mut filters.merged, "merged"),
        (&no_merged, &mut filters.no_merged, "no-merged"),
    ] {
        if let Some(spec) = raw {
            match crate::objname::parse_opt_merge_filter(&repo, spec, long_name) {
                Ok(id) => *slot = Some(id),
                Err(e) => return Ok(e.report()),
            }
        }
    }

    // git switches to listing when there is nothing to create, when a listing-only
    // option (`-l`, `-n`) was given, or when a listing filter was given.
    if list || lines.is_some() || filters.any() || positionals.is_empty() {
        return list_tags(
            &repo,
            &positionals,
            lines,
            format.as_deref(),
            &sort_keys,
            &filters,
            ignore_case,
            omit_empty,
            colopts,
        );
    }

    // git's `create_tag_object`: anything that fills a message body — `-a`, `-m`,
    // `-F` or `--trailer` — turns the lightweight ref into a real tag object.
    let annotate_given = annotate;
    let annotate = annotate || !messages.is_empty() || message_file.is_some() || !trailers.is_empty();

    // Both signing config keys ask for a GPG-signed tag object. `tag.gpgSign`
    // sets `opt.sign` before options are parsed (so it also implies `annotate`);
    // `tag.forceSignAnnotated` signs a tag object only when `-a` was *not* spelled
    // out. Neither can be honored without a signing implementation, so they take
    // the same refusal an explicit `-s` gets rather than silently producing an
    // unsigned tag.
    if repo.config_snapshot().boolean("tag.gpgSign") == Some(true) {
        bail!("signed tags (tag.gpgSign) are not supported")
    }
    if annotate
        && !annotate_given
        && repo.config_snapshot().boolean("tag.forceSignAnnotated") == Some(true)
    {
        bail!("signed tags (tag.forceSignAnnotated) are not supported")
    }

    create_tag(
        &repo,
        &positionals,
        force,
        annotate,
        &messages,
        message_file.as_deref(),
        cleanup.as_deref(),
        &trailers,
        create_reflog,
    )
}

/// git's `--contains`/`--merged`/`--points-at` use `PARSE_OPT_LASTARG_DEFAULT`: a
/// separated argument, when present, is consumed unconditionally; otherwise the
/// option defaults to `HEAD`.
fn optarg(args: &[String], i: &mut usize) -> String {
    match args.get(*i) {
        Some(v) => {
            *i += 1;
            v.clone()
        }
        None => "HEAD".to_string(),
    }
}

/// Consume the value of a separated long/short option, or explain what is missing.
fn take_value<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str> {
    let v = args
        .get(*i)
        .ok_or_else(|| anyhow!("option `{flag}' requires a value"))?;
    *i += 1;
    Ok(v.as_str())
}

/// One resolved `--sort` key.
struct SortKey {
    reverse: bool,
    kind: SortKind,
}

/// What a `--sort` key extracts and how it compares.
enum SortKind {
    /// Compare the full refname with git's `versioncmp`.
    Version,
    /// Compare a `long` numerically (dates by seconds, size by bytes).
    Numeric(NumField),
    /// Render this atom to bytes and compare bytewise.
    Rendered(String),
}

enum NumField {
    TaggerDate,
    CommitterDate,
    AuthorDate,
    CreatorDate,
    Size,
    StarSize,
}

/// A precomputed, comparable value for one sort key on one ref.
enum SortVal {
    Num(i64),
    Bytes(Vec<u8>),
    Version(Vec<u8>),
}

impl SortVal {
    fn cmp(&self, other: &SortVal, pre: &Prereleases<'_>) -> Ordering {
        match (self, other) {
            (SortVal::Num(a), SortVal::Num(b)) => a.cmp(b),
            (SortVal::Bytes(a), SortVal::Bytes(b)) => a.cmp(b),
            (SortVal::Version(a), SortVal::Version(b)) => refsort::versioncmp(a, b, pre),
            _ => Ordering::Equal,
        }
    }
}

/// Why `--sort` resolution failed.
enum SortErr {
    /// A field name git itself rejects: emit `fatal: {0}` and exit 128.
    Fatal(String),
    /// A field git accepts but this port cannot sort by.
    Unsupported(String),
}

/// Validate and interpret every `--sort` key. git dies on the first syntactically
/// invalid key in the order given, so that is checked first; only then is this
/// port's narrower support considered.
fn resolve_sort(sorts: &[String]) -> Result<Vec<SortKey>, SortErr> {
    for key in sorts {
        if let Some(msg) = refsort::sort_error(key) {
            return Err(SortErr::Fatal(msg));
        }
    }
    let mut keys = Vec::with_capacity(sorts.len());
    for key in sorts {
        let (reverse, version, star, atom) = refsort::parse_sort_key(key);
        let field = atom.split(':').next().unwrap_or(atom);
        let kind = if version {
            if field == "refname" && !star {
                SortKind::Version
            } else {
                return Err(SortErr::Unsupported(key.clone()));
            }
        } else {
            match field {
                "refname" if !star => SortKind::Rendered("refname".to_string()),
                "taggerdate" if !star => SortKind::Numeric(NumField::TaggerDate),
                "committerdate" if !star => SortKind::Numeric(NumField::CommitterDate),
                "authordate" if !star => SortKind::Numeric(NumField::AuthorDate),
                "creatordate" if !star => SortKind::Numeric(NumField::CreatorDate),
                "objectsize" => SortKind::Numeric(if star {
                    NumField::StarSize
                } else {
                    NumField::Size
                }),
                "objectname" | "objecttype" | "type" | "taggername" | "committername"
                | "authorname" | "taggeremail" | "committeremail" | "authoremail" | "creator"
                | "subject" | "body" | "contents" => {
                    let mut a = String::new();
                    if star {
                        a.push('*');
                    }
                    a.push_str(atom);
                    SortKind::Rendered(a)
                }
                _ => return Err(SortErr::Unsupported(key.clone())),
            }
        };
        keys.push(SortKey { reverse, kind });
    }
    Ok(keys)
}

/// Build a [`Sig`] from a parsed header signature, tolerating a broken date.
fn sig_from(s: gix::actor::SignatureRef<'_>) -> Sig {
    let time = s
        .time()
        .unwrap_or_else(|_| gix::date::Time::new(s.seconds(), 0));
    Sig {
        name: BString::from(s.name.to_vec()),
        email: BString::from(s.email.to_vec()),
        time,
    }
}

/// Gather every fact about one tag ref, decoding the direct object (and, for an
/// annotated tag, the peeled object) so both sorting and rendering can be exact.
fn gather(repo: &gix::Repository, full: BString, short: BString, id: ObjectId) -> Result<Facts> {
    let obj = repo.find_object(id)?;
    let dir_kind = obj.kind;
    let dir_size = obj.data.len() as u64;
    let mut tagger = None;
    let mut committer = None;
    let mut author = None;
    let mut message = Vec::new();
    let mut peel_id = None;
    let mut peel_kind = None;
    let mut peel_size = None;
    let mut peel_committer = None;
    let mut peel_author = None;
    let mut peel_message = Vec::new();

    match dir_kind {
        Kind::Tag => {
            let t = TagRef::from_bytes(&obj.data, id.kind())?;
            if let Some(s) = t.tagger()? {
                tagger = Some(sig_from(s));
            }
            message = t.message.to_vec();
            let peeled = repo.find_object(id)?.peel_tags_to_end()?;
            peel_id = Some(peeled.id);
            peel_kind = Some(peeled.kind);
            peel_size = Some(peeled.data.len() as u64);
            if peeled.kind == Kind::Commit {
                let c = CommitRef::from_bytes(&peeled.data, peeled.id.kind())?;
                peel_committer = Some(sig_from(c.committer()?));
                peel_author = Some(sig_from(c.author()?));
                peel_message = c.message.to_vec();
            }
        }
        Kind::Commit => {
            let c = CommitRef::from_bytes(&obj.data, id.kind())?;
            committer = Some(sig_from(c.committer()?));
            author = Some(sig_from(c.author()?));
            message = c.message.to_vec();
        }
        _ => {}
    }

    Ok(Facts {
        full,
        short,
        id,
        dir_kind,
        dir_size,
        peel_id,
        peel_kind,
        peel_size,
        tagger,
        committer,
        author,
        message,
        peel_committer,
        peel_author,
        peel_message,
        keys: Vec::new(),
    })
}

/// Compute the precomputed sort value for one key on one ref.
fn sort_value(
    repo: &gix::Repository,
    facts: &Facts,
    key: &SortKey,
    ignore_case: bool,
) -> Result<SortVal> {
    Ok(match &key.kind {
        SortKind::Version => SortVal::Version(facts.full.to_vec()),
        SortKind::Numeric(field) => {
            let n = match field {
                NumField::TaggerDate => facts.tagger.as_ref().map_or(0, |s| s.time.seconds),
                NumField::CommitterDate => facts.committer.as_ref().map_or(0, |s| s.time.seconds),
                NumField::AuthorDate => facts.author.as_ref().map_or(0, |s| s.time.seconds),
                NumField::CreatorDate => creator_sig(facts, false).map_or(0, |s| s.time.seconds),
                NumField::Size => facts.dir_size as i64,
                NumField::StarSize => facts.peel_size.unwrap_or(0) as i64,
            };
            SortVal::Num(n)
        }
        SortKind::Rendered(atom) => {
            let mut buf = Vec::new();
            render_atom(repo, facts, atom, None, &mut buf)?;
            if ignore_case {
                buf.make_ascii_lowercase();
            }
            SortVal::Bytes(buf)
        }
    })
}

/// The creator signature: the tagger of an annotated tag, else the committer of a
/// commit. `star` reads the peeled commit instead of the direct object.
fn creator_sig(facts: &Facts, star: bool) -> Option<&Sig> {
    if star {
        facts.peel_committer.as_ref()
    } else {
        facts.tagger.as_ref().or(facts.committer.as_ref())
    }
}

/// List tags, honoring pattern operands, filters, `--sort`, and rendering.
#[allow(clippy::too_many_arguments)]
fn list_tags(
    repo: &gix::Repository,
    patterns: &[&str],
    lines: Option<usize>,
    format: Option<&str>,
    sort_keys: &[SortKey],
    filters: &Filters,
    ignore_case: bool,
    omit_empty: bool,
    colopts: u32,
) -> Result<ExitCode> {
    let match_mode = if ignore_case {
        Mode::IGNORE_CASE
    } else {
        Mode::empty()
    };

    let head_name: Option<BString> = repo
        .head_ref()
        .ok()
        .flatten()
        .map(|r| BString::from(r.name().as_bstr().to_vec()));

    // A plain `git tag -l` prints refnames and sorts by refname, so nothing about
    // the objects those refs point at is ever consulted. Reading them anyway cost
    // an object decode per tag — and a second one per annotated tag, to peel it —
    // which is the whole cost of the command on a repository with many tags.
    // Anything that DOES look at the object (a --format, `-n`, a filter, or a
    // sort key other than the default refname order) takes the full path below.
    let names_only = format.is_none() && lines.is_none() && !filters.any() && sort_keys.is_empty();
    if names_only {
        let mut names: Vec<(BString, BString)> = Vec::new();
        for r in repo.references()?.tags()? {
            let r = r.map_err(|e| anyhow!("failed to read a tag reference: {e}"))?;
            let short = BString::from(r.name().shorten().to_vec());
            if !patterns.is_empty()
                && !patterns
                    .iter()
                    .any(|p| wildmatch(p.as_bytes().as_bstr(), short.as_bstr(), match_mode))
            {
                continue;
            }
            names.push((BString::from(r.name().as_bstr().to_vec()), short));
        }
        // git's implicit key: ascending full refname.
        names.sort_by(|a, b| a.0.cmp(&b.0));
        return write_lines(names.into_iter().map(|(_, short)| short.into()).collect(), colopts);
    }

    let mut entries: Vec<Facts> = Vec::new();
    for r in repo.references()?.tags()? {
        let r = r.map_err(|e| anyhow!("failed to read a tag reference: {e}"))?;
        let Some(id) = r.try_id().map(|id| id.detach()) else {
            continue;
        };
        let short = BString::from(r.name().shorten().to_vec());
        if !patterns.is_empty()
            && !patterns
                .iter()
                .any(|p| wildmatch(p.as_bytes().as_bstr(), short.as_bstr(), match_mode))
        {
            continue;
        }
        let full = BString::from(r.name().as_bstr().to_vec());
        let mut facts = gather(repo, full, short, id)?;
        if !passes_filters(repo, &facts, filters) {
            continue;
        }
        let keys = sort_keys
            .iter()
            .map(|k| sort_value(repo, &facts, k, ignore_case))
            .collect::<Result<Vec<_>>>()?;
        facts.keys = keys;
        entries.push(facts);
    }

    // git's most-significant key is the last `--sort` given; ties always fall
    // through to an implicit ascending refname comparison.
    // git seeds `versioncmp`'s prerelease list from config once, lazily.
    let prereleases = Prereleases::new(repo);
    entries.sort_by(|a, b| {
        for (idx, key) in sort_keys.iter().enumerate().rev() {
            let mut ord = a.keys[idx].cmp(&b.keys[idx], &prereleases);
            if key.reverse {
                ord = ord.reverse();
            }
            if ord != Ordering::Equal {
                return ord;
            }
        }
        a.full.cmp(&b.full)
    });

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // When columns are active every rendered line becomes one table cell (git wraps
    // list_tags' stdout in a `git column` filter with padding 2); otherwise each
    // line is written straight out, one per line.
    let column_on = super::column::active(colopts);
    let mut cells: Vec<Vec<u8>> = Vec::new();
    for e in &entries {
        let mut line: Vec<u8> = Vec::new();
        if let Some(fmt) = format {
            render_format(repo, &mut line, e, fmt, head_name.as_ref().map(|b| b.as_bstr()))?;
            if omit_empty && line.is_empty() {
                continue;
            }
        } else if let Some(n) = lines {
            // git renders `-n` as `%(align:15)%(refname:lstrip=2)%(end) %(contents:lines=N)`.
            line.extend_from_slice(&e.short);
            let width = e.short.to_str_lossy().chars().count();
            if width < 15 {
                line.resize(line.len() + (15 - width), b' ');
            }
            line.push(b' ');
            append_lines(&mut line, &e.message, n);
        } else {
            line.extend_from_slice(&e.short);
        }
        if column_on {
            cells.push(line);
        } else {
            line.push(b'\n');
            out.write_all(&line)?;
        }
    }
    if column_on {
        write_cells(&mut out, &cells, colopts)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Write already-rendered lines, through the column filter when it is active.
fn write_lines(lines: Vec<Vec<u8>>, colopts: u32) -> Result<ExitCode> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if super::column::active(colopts) {
        write_cells(&mut out, &lines, colopts)?;
    } else {
        for mut line in lines {
            line.push(b'\n');
            out.write_all(&line)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// git's `run_column_filter(colopts, &copts)` with `copts.padding = 2`, every
/// other field zero (indent "", nl "\n", width from the terminal).
fn write_cells(out: &mut impl std::io::Write, cells: &[Vec<u8>], colopts: u32) -> std::io::Result<()> {
    let opts = super::column::ColumnOptions {
        width: 0,
        padding: 2,
        indent: None,
        nl: None,
    };
    out.write_all(&super::column::layout(cells, colopts, &opts))
}

/// The commit a tag ultimately names, if any (its peel for an annotated tag, or
/// itself for a lightweight tag on a commit). `None` for tags on trees/blobs.
fn tag_commit(facts: &Facts) -> Option<ObjectId> {
    match facts.dir_kind {
        Kind::Commit => Some(facts.id),
        Kind::Tag if facts.peel_kind == Some(Kind::Commit) => facts.peel_id,
        _ => None,
    }
}

/// Apply the resolved listing filters, AND-combined as git does.
fn passes_filters(repo: &gix::Repository, facts: &Facts, filters: &Filters) -> bool {
    if let Some(target) = filters.points_at {
        if facts.id != target && facts.peel_id != Some(target) {
            return false;
        }
    }
    let commit = tag_commit(facts);
    // `filter_ref()` (`ref-filter.c`) discards a ref that does not peel to a
    // commit *before* any reachability test, and does so for the whole family at
    // once:
    //
    // ```c
    // if (filter->reachable_from || filter->unreachable_from ||
    //     filter->with_commit || filter->no_commit || filter->verbose) {
    //         commit = lookup_commit_reference_gently(the_repository, ref->oid, 1);
    //         if (!commit)
    //                 return NULL;
    // ```
    //
    // So a tag on a tree or a blob is absent from `--no-contains`/`--no-merged`
    // output too, even though those read as "keep what does not match".
    // `--points-at` is deliberately not in that list: it compares ids and keeps
    // non-commit refs.
    let reachability_asked =
        [filters.contains, filters.no_contains, filters.merged, filters.no_merged]
            .iter()
            .any(Option::is_some);
    if reachability_asked && commit.is_none() {
        return false;
    }
    if let Some(c) = filters.contains {
        match commit {
            Some(tc) if is_ancestor(repo, c, tc) => {}
            _ => return false,
        }
    }
    if let Some(c) = filters.no_contains {
        if let Some(tc) = commit {
            if is_ancestor(repo, c, tc) {
                return false;
            }
        }
    }
    if let Some(m) = filters.merged {
        match commit {
            Some(tc) if is_ancestor(repo, tc, m) => {}
            _ => return false,
        }
    }
    if let Some(m) = filters.no_merged {
        if let Some(tc) = commit {
            if is_ancestor(repo, tc, m) {
                return false;
            }
        }
    }
    true
}

/// Whether `ancestor` is reachable from `descendant` (i.e. is an ancestor of, or
/// equal to, it). Computed via the best merge base.
fn is_ancestor(repo: &gix::Repository, ancestor: ObjectId, descendant: ObjectId) -> bool {
    if ancestor == descendant {
        return true;
    }
    match repo.merge_base(descendant, ancestor) {
        Ok(base) => base.detach() == ancestor,
        Err(_) => false,
    }
}


/// Expand a `--format` string for one tag, supporting `%(if)…%(then)…%(else)…%(end)`.
///
/// `%%` is a literal percent and `%xx` a hex byte, as in `ref-filter.c`; `%(…)` is
/// delegated to [`render_atom`]. Anything else is refused rather than passed
/// through, so a format this module cannot honor never looks like a success.
fn render_format(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    e: &Facts,
    fmt: &str,
    head: Option<&BStr>,
) -> Result<()> {
    // A stack of open `%(if)` frames; the active output sink is the top frame's
    // current branch, or `out` when the stack is empty.
    let mut frames: Vec<IfFrame> = Vec::new();
    let b = fmt.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            push_byte(out, &mut frames, b[i]);
            i += 1;
            continue;
        }
        match b.get(i + 1) {
            Some(b'%') => {
                push_byte(out, &mut frames, b'%');
                i += 2;
            }
            Some(b'(') => {
                let Some(end) = b[i + 2..].iter().position(|&c| c == b')') else {
                    bail!("format string has an unmatched '%('")
                };
                let atom = std::str::from_utf8(&b[i + 2..i + 2 + end])
                    .map_err(|_| anyhow!("format atom is not valid UTF-8"))?;
                handle_atom(repo, out, &mut frames, e, atom, head)?;
                i += 2 + end + 1;
            }
            _ => {
                let hex = b
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(byte) => {
                        push_byte(out, &mut frames, byte);
                        i += 3;
                    }
                    None => anyhow::bail!("unsupported '%' escape in --format"),
                }
            }
        }
    }
    if !frames.is_empty() {
        crate::git_fatal!("format string has an unclosed '%(if)'");
    }
    Ok(())
}

/// One open `%(if)` control block.
struct IfFrame {
    kind: IfKind,
    branch: IfBranch,
    cond: Vec<u8>,
    then_buf: Vec<u8>,
    else_buf: Vec<u8>,
}

enum IfKind {
    Truthy,
    Equals(String),
    NotEquals(String),
}

#[derive(PartialEq)]
enum IfBranch {
    Cond,
    Then,
    Else,
}

/// Append one byte to the currently active output sink.
fn push_byte(out: &mut Vec<u8>, frames: &mut [IfFrame], byte: u8) {
    sink(out, frames).push(byte);
}

/// The buffer that literal/atom output currently flows into.
fn sink<'a>(out: &'a mut Vec<u8>, frames: &'a mut [IfFrame]) -> &'a mut Vec<u8> {
    match frames.last_mut() {
        None => out,
        Some(f) => match f.branch {
            IfBranch::Cond => &mut f.cond,
            IfBranch::Then => &mut f.then_buf,
            IfBranch::Else => &mut f.else_buf,
        },
    }
}

/// Dispatch a `%(atom)`: control-flow atoms drive the `%(if)` stack, everything
/// else renders into the active sink.
fn handle_atom(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    frames: &mut Vec<IfFrame>,
    e: &Facts,
    atom: &str,
    head: Option<&BStr>,
) -> Result<()> {
    let (name, arg) = match atom.split_once(':') {
        Some((n, r)) => (n, Some(r)),
        None => (atom, None),
    };
    match name {
        "if" => {
            let kind = match arg {
                None => IfKind::Truthy,
                Some(a) => {
                    if let Some(v) = a.strip_prefix("equals=") {
                        IfKind::Equals(v.to_string())
                    } else if let Some(v) = a.strip_prefix("notequals=") {
                        IfKind::NotEquals(v.to_string())
                    } else {
                        bail!("--format atom %(if:{a}) is not supported")
                    }
                }
            };
            frames.push(IfFrame {
                kind,
                branch: IfBranch::Cond,
                cond: Vec::new(),
                then_buf: Vec::new(),
                else_buf: Vec::new(),
            });
        }
        "then" => {
            let f = frames
                .last_mut()
                .filter(|f| f.branch == IfBranch::Cond)
                .ok_or_else(|| anyhow!("format: %(then) without %(if)"))?;
            f.branch = IfBranch::Then;
        }
        "else" => {
            let f = frames
                .last_mut()
                .filter(|f| f.branch == IfBranch::Then)
                .ok_or_else(|| anyhow!("format: %(else) without %(then)"))?;
            f.branch = IfBranch::Else;
        }
        "end" => {
            let f = frames
                .pop()
                .ok_or_else(|| anyhow!("format: %(end) without %(if)"))?;
            let taken = match &f.kind {
                IfKind::Truthy => !f.cond.is_empty(),
                IfKind::Equals(v) => f.cond.as_slice() == v.as_bytes(),
                IfKind::NotEquals(v) => f.cond.as_slice() != v.as_bytes(),
            };
            let chosen = if taken { f.then_buf } else { f.else_buf };
            sink(out, frames).extend_from_slice(&chosen);
        }
        _ => {
            let mut buf = Vec::new();
            render_atom(repo, e, atom, head, &mut buf)?;
            sink(out, frames).extend_from_slice(&buf);
        }
    }
    Ok(())
}

/// Render one `%(<atom>)` field into `out`.
fn render_atom(
    repo: &gix::Repository,
    e: &Facts,
    atom: &str,
    head: Option<&BStr>,
    out: &mut Vec<u8>,
) -> Result<()> {
    let star = atom.starts_with('*');
    let body = atom.strip_prefix('*').unwrap_or(atom);
    let (name, arg) = match body.split_once(':') {
        Some((n, r)) => (n, Some(r)),
        None => (body, None),
    };

    match name {
        "refname" => match arg {
            None => out.extend_from_slice(&e.full),
            Some("short") => out.extend_from_slice(&e.short),
            Some(a) => {
                if let Some(n) = a.strip_prefix("lstrip=") {
                    out.extend_from_slice(&refsort::strip_components(&e.full, parse_i64(atom, n)?, true));
                } else if let Some(n) = a.strip_prefix("rstrip=") {
                    out.extend_from_slice(&refsort::strip_components(&e.full, parse_i64(atom, n)?, false));
                } else {
                    bail!("--format atom %({atom}) is not supported")
                }
            }
        },
        "objectname" => {
            let id = if star { e.peel_id } else { Some(e.id) };
            if let Some(id) = id {
                render_objectname(repo, id, arg, atom, out)?;
            }
        }
        "objecttype" | "type" => {
            let kind = if star { e.peel_kind } else { Some(e.dir_kind) };
            if let Some(k) = kind {
                out.extend_from_slice(k.as_bytes());
            }
        }
        "objectsize" => {
            if arg.is_some() {
                bail!("--format atom %({atom}) is not supported");
            }
            let size = if star { e.peel_size } else { Some(e.dir_size) };
            if let Some(s) = size {
                out.extend_from_slice(s.to_string().as_bytes());
            }
        }
        "taggername" | "taggeremail" | "taggerdate" => {
            let sig = if star { None } else { e.tagger.as_ref() };
            render_person(name, arg, atom, sig, out)?;
        }
        "committername" | "committeremail" | "committerdate" => {
            let sig = if star {
                e.peel_committer.as_ref()
            } else {
                e.committer.as_ref()
            };
            render_person(name, arg, atom, sig, out)?;
        }
        "authorname" | "authoremail" | "authordate" => {
            let sig = if star {
                e.peel_author.as_ref()
            } else {
                e.author.as_ref()
            };
            render_person(name, arg, atom, sig, out)?;
        }
        "creator" => {
            if let Some(s) = creator_sig(e, star) {
                out.extend_from_slice(&s.name);
                out.extend_from_slice(b" <");
                out.extend_from_slice(&s.email);
                out.extend_from_slice(b"> ");
                out.extend_from_slice(fmt_date(s.time, "raw")?.as_slice());
            }
        }
        "creatordate" => {
            if let Some(s) = creator_sig(e, star) {
                out.extend_from_slice(&fmt_date(s.time, arg.unwrap_or(""))?);
            }
        }
        "subject" => {
            if arg.is_some() {
                bail!("--format atom %({atom}) is not supported");
            }
            let msg = message_of(e, star);
            out.extend_from_slice(&subject_of(msg));
        }
        "body" => {
            if arg.is_some() {
                bail!("--format atom %({atom}) is not supported");
            }
            let msg = message_of(e, star);
            out.extend_from_slice(&body_of(msg));
        }
        "contents" => {
            let msg = message_of(e, star);
            match arg {
                None => out.extend_from_slice(msg),
                Some("subject") => out.extend_from_slice(&subject_of(msg)),
                Some("body") => out.extend_from_slice(&body_of(msg)),
                Some(a) => {
                    if let Some(n) = a.strip_prefix("lines=") {
                        let n: usize = n
                            .parse()
                            .map_err(|_| anyhow!("--format atom %({atom}) has a bad line count"))?;
                        append_lines(out, msg, n);
                    } else {
                        bail!("--format atom %({atom}) is not supported")
                    }
                }
            }
        }
        "HEAD" => {
            let here = head.map(|h| h == e.full.as_bstr()).unwrap_or(false);
            out.push(if here { b'*' } else { b' ' });
        }
        "color" => {
            // Color is off on this (piped) path, so a color atom produces nothing,
            // exactly as git does when not writing to a terminal.
        }
        _ => bail!("--format atom %({atom}) is not supported"),
    }
    Ok(())
}

/// The message backing `%(subject)`/`%(body)`/`%(contents)` — the peeled commit's
/// message for a `*`-dereferenced atom, else the direct object's message.
fn message_of(e: &Facts, star: bool) -> &[u8] {
    if star {
        &e.peel_message
    } else {
        &e.message
    }
}

/// Render `%(objectname)` / `:short` / `:short=<n>`.
fn render_objectname(
    repo: &gix::Repository,
    id: ObjectId,
    arg: Option<&str>,
    atom: &str,
    out: &mut Vec<u8>,
) -> Result<()> {
    match arg {
        None => out.extend_from_slice(id.to_hex().to_string().as_bytes()),
        Some("short") => {
            // git's dynamic abbreviation, widened by the odb to stay unambiguous.
            out.extend_from_slice(short_hex(repo, id).as_bytes());
        }
        Some(a) => {
            if let Some(n) = a.strip_prefix("short=") {
                let n: usize = n
                    .parse()
                    .map_err(|_| anyhow!("--format atom %({atom}) has a non-numeric argument"))?;
                out.extend_from_slice(id.to_hex_with_len(n).to_string().as_bytes());
            } else {
                bail!("--format atom %({atom}) is not supported")
            }
        }
    }
    Ok(())
}

/// Render a `*name` / `*email[:trim|:localpart]` / `*date[:<fmt>]` person atom.
fn render_person(
    name: &str,
    arg: Option<&str>,
    atom: &str,
    sig: Option<&Sig>,
    out: &mut Vec<u8>,
) -> Result<()> {
    let Some(sig) = sig else {
        return Ok(());
    };
    if name.ends_with("name") {
        if arg.is_some() {
            bail!("--format atom %({atom}) is not supported");
        }
        out.extend_from_slice(&sig.name);
    } else if name.ends_with("email") {
        match arg {
            None => {
                out.push(b'<');
                out.extend_from_slice(&sig.email);
                out.push(b'>');
            }
            Some("trim") => out.extend_from_slice(&sig.email),
            Some("localpart") => {
                let local = match sig.email.iter().position(|&b| b == b'@') {
                    Some(p) => &sig.email[..p],
                    None => &sig.email[..],
                };
                out.extend_from_slice(local);
            }
            Some(_) => bail!("--format atom %({atom}) is not supported"),
        }
    } else {
        // *date
        out.extend_from_slice(&fmt_date(sig.time, arg.unwrap_or(""))?);
    }
    Ok(())
}

/// Format a time for a date atom's suffix, matching git's named formats. Formats
/// that are not deterministic (`relative`, `human`, `local`) or need a custom
/// strftime string are refused rather than faked.
fn fmt_date(time: gix::date::Time, spec: &str) -> Result<Vec<u8>> {
    use gix::date::time::format as f;
    let s = match spec {
        "" | "default" => time.format_or_unix(f::DEFAULT),
        "short" => time.format_or_unix(f::SHORT),
        "iso" | "iso8601" => time.format_or_unix(f::ISO8601),
        "iso-strict" | "iso8601-strict" => time.format_or_unix(f::ISO8601_STRICT),
        "rfc" | "rfc2822" => time.format_or_unix(f::GIT_RFC2822),
        "unix" => time.seconds.to_string(),
        "raw" => time.format_or_unix(f::RAW),
        other => bail!("--format date option :{other} is not supported"),
    };
    Ok(s.into_bytes())
}

/// git's subject: the first paragraph, with internal newlines folded to spaces.
fn subject_of(msg: &[u8]) -> Vec<u8> {
    let trimmed = {
        let end = msg
            .iter()
            .rposition(|&b| b != b'\n')
            .map_or(0, |i| i + 1);
        &msg[..end]
    };
    let sub_end = trimmed
        .windows(2)
        .position(|w| w == b"\n\n")
        .unwrap_or(trimmed.len());
    trimmed[..sub_end]
        .iter()
        .map(|&b| if b == b'\n' { b' ' } else { b })
        .collect()
}

/// git's body: everything after the blank line that ends the subject.
fn body_of(msg: &[u8]) -> Vec<u8> {
    match msg.windows(2).position(|w| w == b"\n\n") {
        Some(p) => msg[p + 2..].to_vec(),
        None => Vec::new(),
    }
}

/// Parse a signed integer atom argument (`lstrip=<n>`), or explain the failure.
fn parse_i64(atom: &str, rest: &str) -> Result<i64> {
    rest.parse::<i64>()
        .map_err(|_| anyhow!("--format atom %({atom}) has a non-numeric argument"))
}

/// Port of git's `append_lines`: the first `lines` lines of `buf`, with every line
/// after the first prefixed by a newline and four spaces.
fn append_lines(out: &mut Vec<u8>, buf: &[u8], lines: usize) {
    let mut sp = 0;
    for i in 0..lines {
        if sp >= buf.len() {
            break;
        }
        if i > 0 {
            out.extend_from_slice(b"\n    ");
        }
        match buf[sp..].iter().position(|&b| b == b'\n') {
            Some(nl) => {
                out.extend_from_slice(&buf[sp..sp + nl]);
                sp += nl + 1;
            }
            None => {
                out.extend_from_slice(&buf[sp..]);
                break;
            }
        }
    }
}

/// Port of git's `create_reflog_msg`: the reflog line written under `--create-reflog`,
/// describing the *target* object (never the new tag object). `GIT_REFLOG_ACTION`, when
/// set, replaces the whole message; otherwise it is `tag: tagging <abbrev> (<detail>)`,
/// where `<detail>` is the target commit's first subject line plus its committer date in
/// `SHORT` (`%Y-%m-%d`, UTC — git passes tz 0 to `show_date`), or a fixed phrase for a
/// tree/blob/tag/unknown target.
fn reflog_message(repo: &gix::Repository, target: ObjectId) -> Result<BString> {
    if let Ok(rla) = std::env::var("GIT_REFLOG_ACTION") {
        return Ok(BString::from(rla));
    }
    let mut sb = format!("tag: tagging {} (", short_hex(repo, target)).into_bytes();
    let obj = repo.find_object(target)?;
    match obj.kind {
        Kind::Commit => {
            let c = CommitRef::from_bytes(&obj.data, target.kind())?;
            // git's `find_commit_subject`: the first physical line of the message,
            // taken verbatim (no whitespace folding).
            let message: &[u8] = c.message;
            let subject_end = message.iter().position(|&b| b == b'\n').unwrap_or(message.len());
            sb.extend_from_slice(&message[..subject_end]);
            let committed = sig_from(c.committer()?);
            // `show_date(c->date, 0, DATE_MODE(SHORT))` — the committer timestamp at UTC.
            let utc = gix::date::Time::new(committed.time.seconds, 0);
            sb.extend_from_slice(b", ");
            sb.extend_from_slice(&fmt_date(utc, "short")?);
        }
        Kind::Tree => sb.extend_from_slice(b"tree object"),
        Kind::Blob => sb.extend_from_slice(b"blob object"),
        Kind::Tag => sb.extend_from_slice(b"other tag object"),
    }
    sb.push(b')');
    Ok(BString::from(sb))
}

/// Create a lightweight or annotated tag `<name>` at `[<commit>]` (default `HEAD`).
#[allow(clippy::too_many_arguments)]
fn create_tag(
    repo: &gix::Repository,
    positionals: &[&str],
    force: bool,
    annotate: bool,
    messages: &[Vec<u8>],
    message_file: Option<&str>,
    cleanup: Option<&str>,
    trailers: &[String],
    create_reflog: bool,
) -> Result<ExitCode> {
    if positionals.len() > 2 {
        return fatal("too many arguments");
    }
    let name = positionals[0];
    let spec = positionals.get(1).copied().unwrap_or("HEAD");

    // `cmd_tag()` only dies with `Failed to resolve` when `repo_get_oid()` itself
    // fails. A full-length hex name resolves without the odb being consulted, so
    // an absent object gets all the way to the ref update and is refused there
    // instead — see the `nonexistent object` check below.
    let Some(target) = crate::objname::resolve(repo, spec) else {
        return fatal(&format!("Failed to resolve '{spec}' as a valid ref."));
    };

    let ref_name = format!("refs/tags/{name}");
    if FullName::try_from(ref_name.as_str()).is_err() {
        return fatal(&format!("'{name}' is not a valid tag name."));
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let prev = repo
        .try_find_reference(ref_name.as_str())?
        .and_then(|r| r.try_id().map(|id| id.detach()));

    if prev.is_some() && !force {
        return fatal(&format!("tag '{name}' already exists"));
    }
    let constraint = if force {
        PreviousValue::Any
    } else {
        PreviousValue::MustNotExist
    };

    let new_id = if annotate {
        if !messages.is_empty() && message_file.is_some() {
            return fatal("options '-F' and '-m' cannot be used together");
        }
        // Validate `--cleanup` the way git does, before touching the object db.
        let mode = match cleanup {
            None => CleanupMode::Strip,
            Some("strip" | "default") => CleanupMode::Strip,
            Some("whitespace") => CleanupMode::Whitespace,
            Some("verbatim") => CleanupMode::Verbatim,
            Some(other) => return fatal(&format!("Invalid cleanup mode {other}")),
        };
        // `create_tag()` opens with `odb_read_object_info()` and refuses anything
        // it cannot type, which is the first thing an absent full-length hex name
        // hits on the annotated path.
        if repo.find_header(target).is_err() {
            return fatal("bad object type.");
        }
        let raw = match message_file {
            Some(path) => read_message_file(path)?,
            None if messages.is_empty() => {
                bail!("`-a` without `-m`/`-F` needs an editor, which is not supported")
            }
            None => match mode {
                // git's `-m` under `verbatim` uses each chunk literally.
                CleanupMode::Verbatim => join_verbatim(messages),
                _ => join_messages(messages),
            },
        };
        // git amends the message with `--trailer`s first and only then runs
        // `--cleanup`, so a trailer's own trailing whitespace and the blank lines
        // the trailer engine leaves behind are stripped exactly like body text —
        // and `--cleanup=verbatim` (git's `CLEANUP_NONE`) keeps both verbatim.
        let raw = match amend_with_trailers(repo, raw, trailers)? {
            Some(amended) => amended,
            None => return Ok(ExitCode::from(128)),
        };
        let message = match mode {
            CleanupMode::Verbatim => raw,
            // `whitespace` and `strip` coincide for `-m`/`-F` input (no comment
            // stripping happens without an editor), both mapping to stripspace.
            CleanupMode::Whitespace | CleanupMode::Strip => stripspace(&raw),
        };

        let tagger = repo
            .committer()
            .ok_or_else(|| {
                anyhow!(
                    "no committer identity configured (set user.name/user.email or \
                     GIT_COMMITTER_NAME/GIT_COMMITTER_EMAIL); git's gecos fallback is not ported"
                )
            })??
            .to_owned()?;

        let target_kind = repo.find_header(target)?.kind();
        // `create_tag` (builtin/tag.c) warns when the object being tagged is
        // itself a tag, naming the user's own `<object>` argument so the
        // suggested command is a copy-paste fix. Lightweight tags never reach
        // `create_tag`, so they are silent, and the hint precedes the ref update.
        if target_kind == Kind::Tag {
            crate::advice::Advice::NestedTag.advise_in(
                repo,
                &format!(
                    "You have created a nested tag. The object referred to by your new tag is\n\
                     already a tag. If you meant to tag the object that it points to, use:\n\
                     \n\
                     \tgit tag -f {name} {spec}^{{}}"
                ),
            );
        }
        let object = gix::objs::Tag {
            target,
            target_kind,
            name: BString::from(name.as_bytes().to_vec()),
            tagger: Some(tagger),
            message: BString::from(message),
            pgp_signature: None,
        };
        repo.write_object(&object)?.detach()
    } else {
        target
    };

    // `ref_transaction_update()` (`refs.c`) verifies the new value before the
    // transaction is allowed to proceed:
    //
    // ```c
    // struct object *o = parse_object(transaction->ref_store->repo, new_oid);
    // if (!o) {
    //         strbuf_addf(err, _("trying to write ref '%s' with nonexistent object %s"),
    //                     refname, oid_to_hex(new_oid));
    // ```
    //
    // and `cmd_tag()` turns that message straight into a `die()`. This is where a
    // lightweight tag on a well-formed but absent object name lands; `gix`'s ref
    // edit does not look the object up, so the check is made here.
    if repo.find_header(new_id).is_err() {
        return fatal(&format!(
            "trying to write ref '{ref_name}' with nonexistent object {new_id}"
        ));
    }

    // git always computes a reflog message describing the *target* object (built
    // before the tag object exists), then writes the ref with `REF_FORCE_CREATE_REFLOG`
    // only under `--create-reflog`. Mirror that: force the reflog via an explicit
    // transaction when asked, else keep the plain `tag_reference` path (whose default
    // `LogChange` never forces a tag reflog), matching stock git for both cases.
    if create_reflog {
        let message = reflog_message(repo, target)?;
        let full: FullName = ref_name
            .as_str()
            .try_into()
            .map_err(|e| anyhow!("invalid tag name {name:?}: {e}"))?;
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: true,
                    message,
                },
                expected: constraint,
                new: Target::Object(new_id),
            },
            name: full,
            deref: false,
        })?;
    } else {
        repo.tag_reference(name, new_id, constraint)?;
    }

    if let Some(old) = prev {
        if old != new_id {
            println!("Updated tag '{name}' (was {})", short_hex(repo, old));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Port of `validate_trailer_args()` (trailer.c). Before any trailer is applied
/// git checks each `--trailer` argument for two things: it must be non-empty, and
/// `find_separator()` must not land on offset 0. The latter happens exactly when
/// the argument's first byte is itself a separator (`find_separator` returns the
/// index of the first separator byte it meets, so 0 is reachable only from byte
/// 0), which leaves no key in front of it. git's separator set here is `=` plus
/// `trailer.separators` (default `:`). Returns git's `error:` text for the first
/// offending argument.
fn validate_trailer_args(repo: &gix::Repository, trailers: &[String]) -> Option<String> {
    let configured = repo
        .config_snapshot()
        .string("trailer.separators")
        .map(|v| v.to_vec());
    let mut separators: Vec<u8> = vec![b'='];
    separators.extend_from_slice(configured.as_deref().unwrap_or(b":"));

    for t in trailers {
        let Some(&first) = t.as_bytes().first() else {
            return Some("empty --trailer argument".to_string());
        };
        if separators.contains(&first) {
            return Some(format!(
                "invalid trailer '{t}': missing key before separator"
            ));
        }
    }
    None
}

/// Port of `amend_file_with_trailers()` (trailer.c) — the helper `git tag` and
/// `git commit` share for `--trailer`. git validates the arguments, then runs the
/// finished message through the trailer engine with `--no-divider` set, so a
/// `---` line in the body never ends the log message. A failure of either step is
/// reported as `fatal: unable to pass trailers to --trailers` and no tag object
/// is written.
///
/// The message round-trips through `$GIT_DIR/TAG_EDITMSG` — the same file git
/// writes and unlinks — because that lets this port hand the work to its own
/// [`super::interpret_trailers`] implementation unchanged: same engine, same
/// `trailer.*` configuration, no second copy of the trailer rules. `Ok(None)`
/// means the diagnostics were printed already.
fn amend_with_trailers(
    repo: &gix::Repository,
    message: Vec<u8>,
    trailers: &[String],
) -> Result<Option<Vec<u8>>> {
    if trailers.is_empty() {
        return Ok(Some(message));
    }
    if let Some(err) = validate_trailer_args(repo, trailers) {
        eprintln!("error: {err}");
        eprintln!("fatal: unable to pass trailers to --trailers");
        return Ok(None);
    }

    let path = repo.git_dir().join("TAG_EDITMSG");
    std::fs::write(&path, &message)
        .map_err(|e| anyhow!("could not write '{}': {e}", path.display()))?;

    let mut argv = vec![
        "--in-place".to_string(),
        "--no-divider".to_string(),
        path.to_string_lossy().into_owned(),
    ];
    for t in trailers {
        argv.push("--trailer".to_string());
        argv.push(t.clone());
    }
    // The argument vector is built here and already validated, so the engine's
    // remaining failure modes are unreadable `trailer.*` config and I/O on the
    // file just written — both surface as `Err`.
    let outcome = super::interpret_trailers::interpret_trailers(&argv);
    let amended = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);

    if let Err(e) = outcome {
        eprintln!("error: {e}");
        eprintln!("fatal: unable to pass trailers to --trailers");
        return Ok(None);
    }
    match amended {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) => {
            eprintln!("error: could not read '{}': {e}", path.display());
            eprintln!("fatal: unable to pass trailers to --trailers");
            Ok(None)
        }
    }
}

/// Message cleanup modes accepted for `-m`/`-F`.
enum CleanupMode {
    Verbatim,
    Whitespace,
    Strip,
}

/// Read a `-F <file>` message, or stdin for `-`.
fn read_message_file(path: &str) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    if path == "-" {
        std::io::stdin().lock().read_to_end(&mut buf)?;
    } else {
        buf = std::fs::read(path).map_err(|e| anyhow!("could not read '{path}': {e}"))?;
    }
    Ok(buf)
}

/// Port of git's `opt_parse_m`: each `-m` chunk is newline-terminated, and a
/// further newline separates it from the previous one.
fn join_messages(messages: &[Vec<u8>]) -> Vec<u8> {
    let mut buf = Vec::new();
    for chunk in messages {
        if !buf.is_empty() {
            buf.push(b'\n');
        }
        buf.extend_from_slice(chunk);
        if buf.last() != Some(&b'\n') {
            buf.push(b'\n');
        }
    }
    buf
}

/// Under `--cleanup=verbatim` git keeps `-m` chunks exactly, joining multiple with
/// a single newline and adding no trailing newline.
fn join_verbatim(messages: &[Vec<u8>]) -> Vec<u8> {
    let mut buf = Vec::new();
    for (idx, chunk) in messages.iter().enumerate() {
        if idx > 0 {
            buf.push(b'\n');
        }
        buf.extend_from_slice(chunk);
    }
    buf
}

/// Port of git's `strbuf_stripspace(buf, NULL)`: trailing whitespace removed from
/// every line, runs of blank lines collapsed to one, leading/trailing blank lines
/// dropped, and a non-empty result ended with a newline.
fn stripspace(input: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    let mut pending_blank = false;
    for line in input.split(|&b| b == b'\n') {
        let end = line
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |i| i + 1);
        let trimmed = &line[..end];
        if trimmed.is_empty() {
            pending_blank = !out.is_empty();
            continue;
        }
        if pending_blank {
            out.push(b'\n');
            pending_blank = false;
        }
        out.extend_from_slice(trimmed);
        out.push(b'\n');
    }
    out
}

/// Delete each named tag, printing `Deleted tag '<name>' (was <short>)`.
fn delete_tags(repo: &gix::Repository, positionals: &[&str]) -> Result<ExitCode> {
    if positionals.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut had_failure = false;
    for name in positionals {
        let ref_name = format!("refs/tags/{name}");
        let found = if FullName::try_from(ref_name.as_str()).is_err() {
            None
        } else {
            repo.try_find_reference(ref_name.as_str())?
        };
        let Some(r) = found else {
            eprintln!("error: tag '{name}' not found.");
            had_failure = true;
            continue;
        };
        let old = r.try_id().map(|id| id.detach());

        let full: FullName = ref_name
            .as_str()
            .try_into()
            .map_err(|e| anyhow!("invalid tag name {name:?}: {e}"))?;
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::MustExist,
                log: RefLog::AndReference,
                message: Default::default(),
            },
            name: full,
            deref: false,
        })?;

        match old {
            Some(id) => println!("Deleted tag '{name}' (was {})", short_hex(repo, id)),
            None => println!("Deleted tag '{name}'"),
        }
    }

    Ok(if had_failure {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Report a git `fatal:` failure on stderr and yield git's exit code for it.
fn fatal(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// Abbreviated hex for `id`, honoring the repo's shortening rules.
fn short_hex(repo: &gix::Repository, id: ObjectId) -> String {
    use gix::prelude::ObjectIdExt;
    id.attach(repo).shorten_or_id().to_string()
}
