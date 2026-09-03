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
//!   * `git tag --sort=[-][version:]<field>` → multi-level sort over any
//!                                       `ref-filter` atom, parsed by the very
//!                                       function `--format` parses with, so a
//!                                       key means the same thing here, in
//!                                       `git branch` and in `git for-each-ref`.
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
//!   * `git tag -s|-u <key-id> …`      → create a GPG/SSH-signed tag object through
//!                                       [`crate::gitsig::Signer`] — the same backend
//!                                       `git commit -S` goes through, so `openpgp`,
//!                                       `x509` and `ssh` all work and the argument
//!                                       vector is git's.
//!   * `git tag -v <name>…`            → verify each tag, sharing
//!                                       [`super::verify_tag`]'s checker so the two
//!                                       commands cannot drift apart.
//! ```
//!
//! Exit codes follow git: fatal errors exit 128, a bad object name for a filter
//! exits 129, a usage error 129, and a failed delete or verify 1.
//!
//! Config read here: the multi-valued `tag.sort` (default listing order), and the
//! two signing switches `tag.gpgSign` / `tag.forceSignAnnotated`, whose precedence
//! against the command line is exactly git's and is not intuitive — see the
//! `opt.sign` resolution in [`tag`] and the `force_sign_annotate` application in
//! [`create_tag`]'s caller. In short, measured against stock: `-u` beats
//! `--no-sign` from either side, `-s`/`--no-sign` is ordinary last-one-wins,
//! `tag.gpgSign` loses to `--no-sign`, and `tag.forceSignAnnotated` beats it while
//! deliberately leaving an explicit `-a` unsigned.
//!
//! A tag's signature is **body text, not a header**: it is appended to the object
//! after the message with no separating newline (builtin/tag.c:191), which is why
//! the signed object is assembled by hand rather than through gix's
//! `Tag::pgp_signature` field — that one writes an extra newline, which would be a
//! different object id and a payload stock git cannot verify. Signing fails closed:
//! no ref and no tag object are written unless a signature was produced.
//!
//! Listing goes through [`super::ref_filter`], the shared port of
//! `ref-filter.c`, rather than a `tag`-local atom table: `list_tags()`
//! (builtin/tag.c:54-80) is nothing but a default format string —
//! `%(refname:lstrip=2)`, or `%(align:15)%(refname:lstrip=2)%(end)
//! %(contents:lines=<n>)` under `-n<num>` — handed to
//! `filter_and_format_refs()`. So `-n` is not an output mode of its own, and
//! every atom, `%(if)`/`%(align)` container and sort key `git for-each-ref`
//! accepts is accepted here, evaluated by the same code, from the same per-ref
//! model. `git tag -v --format=<fmt>` reaches the same evaluator through
//! [`super::ref_filter::pretty_print_ref`].
//!
//! `--color` comes along with that: `OPT__COLOR(&format.use_color, …)`
//! (builtin/tag.c:535) is the whole of it, resolved against `color.ui` and
//! stdout by `want_color()`, and `%(color:<spec>)` is rendered by the shared
//! evaluator's port of `color_parse_mem`.
//!
//! An editor-supplied message is ported: `-e`/`--edit` reopens a message that was
//! given, and `-a`/`-s`/`tag.gpgSign` with neither `-m` nor `-F` writes
//! `$GIT_DIR/TAG_EDITMSG` — the previous tag's body when `-f` replaces one, else
//! git's commented prompt — runs the configured editor on it and cleans up what
//! comes back, which is what makes an editor that saves nothing `fatal: no tag
//! message?`.
//!
//! Genuinely not backed here, and refused rather than faked: the git gecos
//! identity fallback.

use anyhow::{anyhow, bail, Result};
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::glob::wildmatch;
use gix::glob::wildmatch::Mode;
use gix::hash::ObjectId;
use gix::objs::{CommitRef, Kind, Write as _};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};


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
    /// The four reachability filters in the shape `ref-filter` takes them.
    /// `--points-at` is not one of them: `apply_ref_filter()` tests it against
    /// the ref's own id before any commit lookup happens.
    fn shared(&self) -> super::for_each_ref::Filters {
        super::for_each_ref::Filters {
            contains: self.contains.into_iter().collect(),
            no_contains: self.no_contains.into_iter().collect(),
            merged: self.merged.into_iter().collect(),
            no_merged: self.no_merged.into_iter().collect(),
        }
    }

    fn any(&self) -> bool {
        self.points_at.is_some()
            || self.contains.is_some()
            || self.no_contains.is_some()
            || self.merged.is_some()
            || self.no_merged.is_some()
    }
}

/// git's `cmdmode` for `tag`: the `OPT_CMDMODE` value of `-l`/`-d`/`-v`, or 0.
///
/// It is a single variable in C for a reason: the three are mutually exclusive
/// (parse-options refuses a second, different one), it gates the `tag.gpgSign`
/// default, and `(create_tag_object || force) && cmdmode` is git's usage error.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CmdMode {
    /// No `-l`/`-d`/`-v` yet — creation mode unless something implies listing.
    None,
    List,
    Delete,
    Verify,
}

pub fn tag(args: &[String]) -> Result<ExitCode> {
    let mut cmdmode = CmdMode::None;
    // The spelling `-l`/`-d`/`-v` was typed with, for the conflict message.
    let mut cmdmode_spelling = "";
    let mut force = false;
    let mut annotate = false;
    // `opt.sign`, an `OPT_BOOL` left at -1 until the command line speaks: unset
    // (`None`) is what lets `tag.gpgSign` decide, which an eager `false` would not.
    let mut sign: Option<bool> = None;
    // `-u <key-id>` / `--local-user`; `set_signing_key()` on a non-NULL value.
    let mut keyid: Option<String> = None;
    let mut edit_flag = false;
    let mut ignore_case = false;
    let mut omit_empty = false;
    let mut create_reflog = false;
    // `OPT__COLOR(&format.use_color, …)` (builtin/tag.c:535). Unset falls through
    // to `color.ui`, whose default is `auto`, which `want_color()` resolves
    // against stdout.
    let mut color_when: Option<String> = None;
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
            "-d" | "--delete" => {
                if let Some(code) = set_cmdmode(&mut cmdmode, &mut cmdmode_spelling, CmdMode::Delete, typed) {
                    return Ok(code);
                }
            }
            "-l" | "--list" => {
                if let Some(code) = set_cmdmode(&mut cmdmode, &mut cmdmode_spelling, CmdMode::List, typed) {
                    return Ok(code);
                }
            }
            "-v" | "--verify" => {
                if let Some(code) = set_cmdmode(&mut cmdmode, &mut cmdmode_spelling, CmdMode::Verify, typed) {
                    return Ok(code);
                }
            }
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
            "--color" => color_when = Some("always".to_string()),
            "--no-color" => color_when = Some("never".to_string()),
            // `OPT_BOOL`'s unset writes 0 — which is *not* the same as leaving
            // `opt.sign` at -1, because a written 0 is what stops `tag.gpgSign`
            // from turning signing back on.
            "--no-sign" => sign = Some(false),
            // `OPT_STRING`'s unset sets `keyid` back to NULL, so the
            // `if (keyid) opt.sign = 1` below never fires and nothing is signed.
            "--no-local-user" => keyid = None,
            "--no-edit" => edit_flag = false,
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
            "-s" | "--sign" => sign = Some(true),
            "-u" | "--local-user" => keyid = Some(super::take_value(args, &mut i, a)?.to_string()),
            "-e" | "--edit" => edit_flag = true,
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
                    sorts.push(super::take_value(args, &mut i, a)?.to_string());
                } else if let Some(rest) = a.strip_prefix("--format=") {
                    format = Some(rest.to_string());
                } else if a == "--format" {
                    format = Some(super::take_value(args, &mut i, a)?.to_string());
                } else if let Some(rest) = a.strip_prefix("--cleanup=") {
                    cleanup = Some(rest.to_string());
                } else if a == "--cleanup" {
                    cleanup = Some(super::take_value(args, &mut i, a)?.to_string());
                } else if let Some(rest) = a.strip_prefix("--column=") {
                    super::column::parseopt_column(&mut colopts, Some(rest), false)
                        .map_err(|m| anyhow!("{m}"))?;
                } else if let Some(rest) = a.strip_prefix("--color=") {
                    color_when = Some(rest.to_string());
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
                    trailers.push(super::take_value(args, &mut i, a)?.to_string());
                } else if let Some(rest) = a.strip_prefix("--message=") {
                    messages.push(rest.as_bytes().to_vec());
                } else if a == "--message" || a == "-m" {
                    messages.push(super::take_value(args, &mut i, a)?.as_bytes().to_vec());
                } else if let Some(rest) = a.strip_prefix("-m") {
                    messages.push(rest.as_bytes().to_vec());
                } else if let Some(rest) = a.strip_prefix("--file=") {
                    message_file = Some(rest.to_string());
                } else if a == "--file" || a == "-F" {
                    message_file = Some(super::take_value(args, &mut i, a)?.to_string());
                } else if let Some(rest) = a.strip_prefix("-F") {
                    message_file = Some(rest.to_string());
                // `-u<key-id>` and `--local-user=<key-id>`: an `OPT_STRING` takes
                // its value stuck to the short name or after the `=`, and an empty
                // one is a key git passes to gpg unchanged (which answers
                // `gpg: skipped "": Invalid user ID`) rather than a missing one.
                } else if let Some(rest) = a.strip_prefix("--local-user=") {
                    keyid = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("-u") {
                    keyid = Some(rest.to_string());
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
                    lines = Some(tag_lines(rest)?);
                } else if !a.starts_with("--") {
                    // `parse_short_opt()` (parse-options.c:426-461) driven from
                    // `parse_options_step()`'s cluster loop (:1061-1107): every
                    // character of a `-<chars>` token is its own option, and a
                    // value-taking one swallows the rest of the cluster — or, when
                    // it ends the cluster, the next argv element. So `-fam <msg>`
                    // is `-f -a -m <msg>`.
                    //
                    // The character parsing stops at is what a refusal names, not
                    // the one the token began with, because the C rewrites
                    // `argv[0]` before reporting:
                    //
                    // ```c
                    //         ctx->argv[0] = xstrdup(ctx->opt - 1);
                    //         *(char *)ctx->argv[0] = '-';
                    //         goto unknown;
                    // ```
                    //
                    // (parse-options.c:1095-1097). `git tag -aé` therefore says
                    // ``unknown non-ascii option in string: `-é'`` and not `-aé`.
                    // Walking `char_indices()` rather than byte slices is what
                    // keeps a multi-byte character from splitting mid-codepoint;
                    // the old `a[1..2]` gate panicked on exactly that input.
                    for (off, c) in a.char_indices().skip(1) {
                        // The one-character spelling parse-options would name in a
                        // cmdmode conflict, since only the cluster knows it.
                        let short = format!("-{c}");
                        match c {
                            'f' => force = true,
                            'a' => annotate = true,
                            'i' => ignore_case = true,
                            's' => sign = Some(true),
                            'e' => edit_flag = true,
                            'd' | 'l' | 'v' => {
                                let mode = match c {
                                    'd' => CmdMode::Delete,
                                    'l' => CmdMode::List,
                                    _ => CmdMode::Verify,
                                };
                                if let Some(code) =
                                    set_cmdmode(&mut cmdmode, &mut cmdmode_spelling, mode, &short)
                                {
                                    return Ok(code);
                                }
                            }
                            // `-n` is `OPTION_INTEGER` with `PARSE_OPT_OPTARG`, so
                            // it never reaches for the next argv element: `!p->opt`
                            // means "no attached value" and takes `defval`. That is
                            // why `git tag -ln` lists with one line each instead of
                            // complaining that `-n` wants a value.
                            'n' => {
                                lines = Some(tag_lines(&a[off + c.len_utf8()..])?);
                                break;
                            }
                            c @ ('m' | 'F' | 'u') => {
                                let rest = &a[off + c.len_utf8()..];
                                let val = match rest.is_empty() {
                                    true => crate::parseopt::get_arg(
                                        args,
                                        &mut i,
                                        crate::parseopt::OptName::Short(c),
                                    )?
                                    .to_string(),
                                    false => rest.to_string(),
                                };
                                match c {
                                    'm' => messages.push(val.into_bytes()),
                                    'F' => message_file = Some(val),
                                    _ => keyid = Some(val),
                                }
                                break; // the value flag consumed the rest of the cluster
                            }
                            // `if (internal_help && *ctx->opt == 'h') goto show_usage`
                            // (parse-options.c:1087-1088): a cluster asks for help
                            // exactly when the first character the table does *not*
                            // define is `h`, so `git tag -fh` prints the block on
                            // stdout while `git tag -Zh` reports `Z`.
                            'h' => return Ok(super::show_usage(USAGE)),
                            _ => return Ok(super::unknown_option(&format!("-{}", &a[off..]), USAGE)),
                        }
                    }
                } else {
                    bail!("unsupported option {a:?}")
                }
            }
        }
    }

    // `git_tag_config()` (builtin/tag.c:210-237) runs before `parse_options()`, so
    // every key it reads is in hand by the time the command line is weighed against
    // it. `tag.gpgSign` is `config_sign_tag`, unspecified until it appears.
    let mut config_sign_tag: Option<bool> = None;
    let mut force_sign_annotate = false;
    let mut config_sorts: Vec<String> = Vec::new();
    if let Ok(repo) = crate::setup::discover() {
        config_sorts = repo
            .config_snapshot()
            .plumbing()
            .values::<gix::bstr::BString>("tag.sort")
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.to_string())
            .collect();
        config_sign_tag = repo.config_snapshot().boolean("tag.gpgSign");
        force_sign_annotate =
            repo.config_snapshot().boolean("tag.forceSignAnnotated") == Some(true);
    }

    // ```c
    // repo_config(the_repository, git_tag_config, &sorting_options);
    // if (!sorting_options.nr)
    //         string_list_append(&sorting_options, "refname");
    // ```
    // (builtin/tag.c:549-551), *before* `parse_options()`. So `tag.sort` comes
    // first, an implicit `refname` stands in when there is none, and every CLI
    // `--sort` appends after them — ending up most significant while the config
    // keys still break ties. `--no-sort` is `OPT_STRING_LIST`'s unset callback,
    // which clears the whole list, config and implicit key included, leaving
    // `ref_sorting_options()` to return NULL and `ref_array_sort()` to do nothing.
    let cli_sorts = std::mem::take(&mut sorts);
    let mut sorts: Vec<String> = if sort_negated {
        Vec::new()
    } else {
        if config_sorts.is_empty() {
            config_sorts.push("refname".to_string());
        }
        config_sorts
    };
    sorts.extend(cli_sorts);

    // ```c
    // if (!cmdmode) {
    //         if (argc == 0)                                    cmdmode = 'l';
    //         else if (filter.with_commit || … || filter.lines != -1) cmdmode = 'l';
    // }
    // ```
    // (builtin/tag.c:559-566). The filters are tested as *given*, before any of
    // them is resolved against the odb, so this reads the raw operands.
    if cmdmode == CmdMode::None
        && (positionals.is_empty()
            || lines.is_some()
            || contains.is_some()
            || no_contains.is_some()
            || merged.is_some()
            || no_merged.is_some()
            || points_at.is_some())
    {
        cmdmode = CmdMode::List;
    }

    // ```c
    // if (opt.sign == -1)   opt.sign = cmdmode ? 0 : config_sign_tag > 0;
    // if (keyid) { opt.sign = 1; set_signing_key(keyid); }
    // create_tag_object = (opt.sign || annotate || msg.given || msgfile ||
    //                      edit_flag || trailer_args.nr);
    // if ((create_tag_object || force) && (cmdmode != 0))
    //         usage_with_options(git_tag_usage, options);
    // ```
    // (builtin/tag.c:574-585). Two consequences worth stating, both measured
    // against stock: `-u <key>` turns signing on *unconditionally*, so it beats a
    // `--no-sign` on either side of it; and `tag.gpgSign` is consulted only when
    // no mode flag is in play, which is why `git tag -l` in a repository that sets
    // it still just lists.
    let mut sign = sign.unwrap_or(cmdmode == CmdMode::None && config_sign_tag == Some(true));
    if keyid.is_some() {
        sign = true;
    }
    let create_tag_object = sign
        || annotate
        || !messages.is_empty()
        || message_file.is_some()
        || edit_flag
        || !trailers.is_empty();
    if (create_tag_object || force) && cmdmode != CmdMode::None {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
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
    let mut repo = crate::setup::discover()?;
    if let Some(code) = crate::ensure_object_identity(&mut repo, "Committer") {
        return Ok(code);
    }

    // `want_color(format.use_color)`: the option when it was given, else
    // `color.ui`, with `auto` decided by stdout. `git tag` has no `color.tag`
    // slot of its own.
    let color_on = match color_when.as_deref() {
        Some(v) => super::color::want_color_stdout_raw(&repo, Some(v)),
        None => super::color::want_color_stdout(&repo, "ui"),
    };

    // `sorting = ref_sorting_options(&sorting_options);` (builtin/tag.c:593) runs
    // unconditionally, after the `--column`/`-n` check and before the mode
    // dispatch, so an invalid `--sort` is fatal even for `git tag -d`. The keys go
    // through `ref-filter`'s own atom parser, which is what makes `--sort` mean
    // exactly the same thing here, in `git branch` and in `git for-each-ref`.
    if let Err(e) = super::ref_filter::check_sort(&repo, &sorts) {
        return super::ref_filter::report(e);
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

    // The mode `cmdmode` settled on above, which already folds in the listing git
    // infers from an empty argv, a `-n`, or any filter.
    if cmdmode == CmdMode::List {
        return list_tags(
            &repo,
            &positionals,
            lines,
            format.as_deref(),
            &sorts,
            &filters,
            ignore_case,
            omit_empty,
            colopts,
            color_on,
        );
    }

    // `only_in_list` (builtin/tag.c:610-623): a listing-only option that survived
    // to here means the mode is `-d` or `-v`, and git names the *first* such option
    // in its own fixed order rather than the order they were typed in.
    let only_in_list = if lines.is_some() {
        Some("-n")
    } else if contains.is_some() {
        Some("--contains")
    } else if no_contains.is_some() {
        Some("--no-contains")
    } else if points_at.is_some() {
        Some("--points-at")
    } else if merged.is_some() {
        Some("--merged")
    } else if no_merged.is_some() {
        Some("--no-merged")
    } else {
        None
    };
    if let Some(name) = only_in_list {
        return fatal(&format!("the '{name}' option is only allowed in list mode"));
    }

    if cmdmode == CmdMode::Delete {
        return delete_tags(&repo, &positionals);
    }
    if cmdmode == CmdMode::Verify {
        return verify_tags(&repo, &positionals, format.as_deref());
    }

    // ```c
    // if (create_tag_object) {
    //         if (force_sign_annotate && !annotate)  opt.sign = 1;
    // ```
    // (builtin/tag.c:683-685). The `!annotate` is the bare `-a` flag, *not*
    // `create_tag_object`, so `tag.forceSignAnnotated` signs the tag `-m` alone
    // implied and pointedly leaves the one `-a` asked for unsigned — and, because
    // this runs after the command line has been folded in, it also overrides an
    // explicit `--no-sign`. Both are stock behavior, measured, not guesses.
    if create_tag_object && force_sign_annotate && !annotate {
        sign = true;
    }

    // `get_signing_key()`'s `configured_signing_key` is `set_signing_key(keyid)`
    // when `-u` was given and `user.signingKey` otherwise, which is exactly what
    // overwriting the resolved signer's key expresses.
    let signer = sign.then(|| {
        let mut signer = crate::gitsig::Signer::resolve(&repo);
        if let Some(key) = keyid {
            signer.key = Some(key);
        }
        signer
    });

    create_tag(
        &repo,
        &positionals,
        force,
        create_tag_object,
        edit_flag,
        &messages,
        message_file.as_deref(),
        cleanup.as_deref(),
        &trailers,
        create_reflog,
        signer.as_ref(),
    )
}

/// The three lines and the exit code that follow a signature git could not
/// produce, in order (builtin/tag.c:269-278 and 378-383):
///
/// ```c
/// if (sign && do_sign(buf, …) < 0)   return error(_("unable to sign the tag"));
/// …
/// if (build_tag_object(buf, opt->sign, result) < 0) {
///         if (path) fprintf(stderr, _("The tag message has been left in %s\n"), path);
///         exit(128);
/// }
/// ```
///
/// `path` is non-NULL for every `create_tag_object` call, so the second line is
/// printed even when nothing was ever written to `TAG_EDITMSG` — a `-m` message
/// with no `--trailer` and no editor never creates the file. That is stock's own
/// behavior, verified against `git tag -u NOPE -m m` (the message names the file,
/// and the file does not exist), and it is reproduced rather than corrected.
///
/// The backend's own report has already reached stderr in every case; what is left
/// is the `error: ` prefix `sign_buffer` did not add, and the two lines above.
/// A `die()` inside `get_signing_key()` (an ssh signer with neither
/// `user.signingKey` nor `gpg.ssh.defaultKeyCommand`) ends the command on the spot,
/// so it gets neither.
fn sign_failed(repo: &gix::Repository, e: crate::gitsig::SignFailure) -> ExitCode {
    match e {
        crate::gitsig::SignFailure::Silent => return ExitCode::from(128),
        crate::gitsig::SignFailure::Fatal(m) => {
            eprintln!("{}", crate::gitsig::report("fatal: ", &m));
            return ExitCode::from(128);
        }
        crate::gitsig::SignFailure::Error(m) => {
            eprintln!("{}", crate::gitsig::report("error: ", &m));
        }
    }
    eprintln!("error: unable to sign the tag");
    // `repo_git_path(the_repository, "TAG_EDITMSG")`. `setup_git_directory()` has
    // already moved to the top of the work tree, so an ordinary repository names
    // its git directory `.git` however deep the command was run — which is why this
    // cannot simply print `repo.git_dir()`.
    let git_dir = repo.git_dir();
    let shown = match repo.workdir() {
        Some(top) if git_dir == top.join(".git") => std::path::Path::new(".git"),
        _ => git_dir,
    };
    eprintln!(
        "The tag message has been left in {}",
        shown.join("TAG_EDITMSG").display()
    );
    ExitCode::from(128)
}

/// `OPT_CMDMODE`'s duplicate handling (parse-options.c:404-423): a second, *different*
/// mode is `error(_("options '%s' and '%s' cannot be used together"))` naming the
/// option just seen first and the one already recorded second, then exit 129 with no
/// usage block. Repeating the same mode is accepted silently.
fn set_cmdmode(
    cmdmode: &mut CmdMode,
    spelling: &mut &'static str,
    want: CmdMode,
    typed: &str,
) -> Option<ExitCode> {
    if *cmdmode != CmdMode::None && *cmdmode != want {
        eprintln!("error: options '{typed}' and '{spelling}' cannot be used together");
        return Some(ExitCode::from(129));
    }
    *cmdmode = want;
    // The spelling is needed only to name this option in a *later* conflict, and
    // the two forms of each are fixed, so no borrow of `args` has to outlive here.
    *spelling = match (want, typed.starts_with("--")) {
        (CmdMode::List, false) => "-l",
        (CmdMode::List, true) => "--list",
        (CmdMode::Delete, false) => "-d",
        (CmdMode::Delete, true) => "--delete",
        (CmdMode::Verify, false) => "-v",
        (CmdMode::Verify, true) => "--verify",
        (CmdMode::None, _) => "",
    };
    None
}

/// `cmdmode == 'v'` (builtin/tag.c:628-633): verify each name, through
/// `for_each_tag_name()` — which resolves `refs/tags/<name>` and nothing else, so
/// an object id or `HEAD` is "not found" here even though `git verify-tag` takes
/// both.
///
/// The per-name callback is `verify_tag()` (builtin/tag.c:142-159), whose flags are
/// `GPG_VERIFY_VERBOSE` — the tag payload goes to stdout, unlike bare
/// `git verify-tag` — replaced outright by `GPG_VERIFY_OMIT_STATUS` when
/// `--format` is given, which is why a formatted verify prints neither the payload
/// nor gpg's report.
fn verify_tags(
    repo: &gix::Repository,
    names: &[&str],
    format: Option<&str>,
) -> Result<ExitCode> {
    // `verify_ref_format()` runs before the first tag is read, and a malformed
    // `%(` shows *`git tag`'s* usage block, not `git verify-tag`'s.
    let tokens = match format
        .map(|f| super::ref_filter::parse_one_format(repo, f, USAGE))
        .transpose()
    {
        Ok(t) => t,
        Err(code) => return Ok(code),
    };
    let verbose = tokens.is_none();

    let mut had_error = false;
    for name in names {
        // `refs_read_ref(…, "refs/tags/<name>")`, so a missing ref is reported and
        // the remaining names are still tried.
        let id = repo
            .try_find_reference(&format!("refs/tags/{name}"))
            .ok()
            .flatten()
            .and_then(|r| r.try_id().map(|id| id.detach()));
        let Some(id) = id else {
            eprintln!("error: tag '{name}' not found.");
            had_error = true;
            continue;
        };
        if !super::verify_tag::verify_resolved(repo, name, id, verbose, false, tokens.as_deref())? {
            had_error = true;
        }
    }
    Ok(if had_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// git's `--contains`/`--merged`/`--points-at` use `PARSE_OPT_LASTARG_DEFAULT`: a
/// separated argument, when present, is consumed unconditionally; otherwise the
/// option defaults to `HEAD`.
fn optarg(args: &[String], i: &mut usize) -> String {
    crate::parseopt::get_arg_lastarg(args, i, "HEAD").to_string()
}

/// `-n[<num>]`'s value.
///
/// ```c
/// { .type = OPTION_INTEGER, .short_name = 'n', .value = &filter.lines,
///   .precision = sizeof(filter.lines), .argh = N_("n"),
///   .help = N_("print <n> lines of each tag message"),
///   .flags = PARSE_OPT_OPTARG, .defval = 1 },
/// ```
/// (builtin/tag.c's `options[]`). Two consequences the plain `str::parse` this
/// replaced got wrong: an empty `rest` is `PARSE_OPT_OPTARG`'s "no attached
/// value" and takes `defval`, so `git tag -ln` lists rather than refusing; and a
/// value that is not a number is `git_parse_signed()`'s complaint, named for the
/// *switch*, with no usage block behind it — not a `zvcs:` gap message at exit 1.
///
/// The base-0 grammar comes with it, so `-n0x2` is two lines the way stock reads
/// it. A negative value is accepted by parse-options (it is inside an `int`'s
/// range) and is meaningless downstream, so it clamps to zero here rather than
/// wrapping into a huge `usize`.
fn tag_lines(rest: &str) -> Result<usize> {
    if rest.is_empty() {
        return Ok(1);
    }
    match crate::optint::integer(&crate::optint::short_opt('n'), rest) {
        Ok(n) => Ok(n.max(0) as usize),
        Err(e) => {
            eprintln!("error: {e}");
            Err(crate::parseopt::silent(crate::parseopt::USAGE_ERROR))
        }
    }
}


/// List tags, honoring pattern operands, filters, `--sort`, and rendering.
#[allow(clippy::too_many_arguments)]
fn list_tags(
    repo: &gix::Repository,
    patterns: &[&str],
    lines: Option<usize>,
    format: Option<&str>,
    sorts: &[String],
    filters: &Filters,
    ignore_case: bool,
    omit_empty: bool,
    colopts: u32,
    color_on: bool,
) -> Result<ExitCode> {
    // A plain `git tag -l` prints refnames and sorts by refname, so nothing about
    // the objects those refs point at is ever consulted. Reading them anyway costs
    // an object decode per tag — and a second one per annotated tag, to peel it —
    // which is the whole cost of the command on a repository with many tags.
    // Anything that DOES look at the object (a `--format`, `-n`, a filter, or a
    // sort key other than the default refname order) takes the full path below.
    // Every one of `[]`, `["refname"]` and a repeated `refname` is plain ascending
    // refname order, which is also the order the ref store hands names back in.
    let refname_order = sorts.iter().all(|s| s == "refname");
    let names_only = format.is_none() && lines.is_none() && !filters.any() && refname_order;
    if names_only {
        let match_mode = if ignore_case {
            Mode::IGNORE_CASE
        } else {
            Mode::empty()
        };
        let mut names: Vec<(BString, BString)> = Vec::new();
        for r in repo.references()?.tags()? {
            // `warning: ignoring broken ref %s` — `ref_resolves_to_object()`'s
            // refusal is a warning git walks past, not an error that ends the
            // listing. A ref whose id cannot even be read for the repository's
            // hash algorithm (a sha1 file under `extensions.objectFormat=sha256`)
            // is exactly that case, and the exit stays 0.
            let r = match r {
                Ok(r) => r,
                Err(e) => {
                    use gix::refs::file::iter::loose_then_packed::Error as IterError;
                    match e.downcast_ref::<IterError>() {
                        Some(IterError::ReferenceCreation { relative_path, .. }) => {
                            eprintln!(
                                "warning: ignoring broken ref {}",
                                relative_path.to_string_lossy().replace('\\', "/")
                            );
                            continue;
                        }
                        _ => return Err(anyhow!("failed to read a tag reference: {e}")),
                    }
                }
            };
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

    // ```c
    // if (!format->format) {
    //         if (filter->lines) {
    //                 to_free = xstrfmt("%s %%(contents:lines=%d)",
    //                                   "%(align:15)%(refname:lstrip=2)%(end)",
    //                                   filter->lines);
    //                 format->format = to_free;
    //         } else
    //                 format->format = "%(refname:lstrip=2)";
    // }
    // ```
    // (builtin/tag.c:62-70). `-n<num>` is not a separate output mode: it is that
    // one format string, which is why it pads to 15 columns with `%(align)`'s rule
    // (never truncating) and why `--format` and `-n` cannot both apply.
    //
    // `filter->lines` is an `int`, and `list_tags()` opens with `if (filter->lines
    // == -1) filter->lines = 0;` — so `-n0` leaves it at 0, `if (filter->lines)`
    // is false, and `git tag -n0` prints bare names with no `%(align:15)` padding.
    // It is still enough to force list mode (`filter.lines != -1`).
    let default_format = match lines.filter(|n| *n > 0) {
        Some(n) => format!("%(align:15)%(refname:lstrip=2)%(end) %(contents:lines={n})"),
        None => "%(refname:lstrip=2)".to_string(),
    };
    let built = |_: &[super::ref_filter::Candidate]| -> Vec<u8> { default_format.clone().into_bytes() };

    let spec = super::ref_filter::ListSpec {
        repo,
        format: match format {
            Some(f) => super::ref_filter::Format::Fixed(f.as_bytes().to_vec()),
            None => super::ref_filter::Format::Built(&built),
        },
        sort_specs: sorts.to_vec(),
        kinds: super::ref_filter::kind::TAGS,
        patterns: patterns.iter().map(|p| p.to_string()).collect(),
        ignore_case,
        points_at: filters.points_at.into_iter().collect(),
        filters: filters.shared(),
        omit_empty,
        color_on,
        // `FILTER_REFS_TAGS` only; the detached-HEAD pseudo entry is `git
        // branch --list`'s alone.
        head_desc: None,
        // `list_tags()` goes through `filter_and_format_refs()`, which does call
        // `filter_is_base()` (ref-filter.c:3440).
        run_is_base: true,
        // `FILTER_REFS_TAGS` never produces one, and `cmd_tag()` does not set the
        // flag anyway.
        detached_head_first: false,
        // `git tag` has no `-v` listing flag of its own; `filter.verbose` stays 0.
        verbose: false,
    };

    let out_lines = match super::ref_filter::filter_and_format(&spec)? {
        super::ref_filter::Listing::Lines(l) => l,
        super::ref_filter::Listing::Exit(code) => return Ok(code),
    };
    write_lines(out_lines, colopts)
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
            // `show_date(c->date, 0, DATE_MODE(SHORT))` — the committer timestamp at UTC.
            let utc = gix::date::Time::new(c.committer()?.seconds(), 0);
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
    edit_flag: bool,
    messages: &[Vec<u8>],
    message_file: Option<&str>,
    cleanup: Option<&str>,
    trailers: &[String],
    create_reflog: bool,
    signer: Option<&crate::gitsig::Signer>,
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
        let message_given = message_file.is_some() || !messages.is_empty();
        let raw = match message_file {
            Some(path) => read_message_file(path)?,
            None if messages.is_empty() => Vec::new(),
            None => match mode {
                // git's `-m` under `verbatim` uses each chunk literally.
                CleanupMode::Verbatim => join_verbatim(messages),
                _ => join_messages(messages),
            },
        };
        // ```c
        // if (!opt->message_given || opt->use_editor) {
        //         ... write TAG_EDITMSG ...
        //         if (launch_editor(path, buf, NULL)) { ...; exit(1); }
        //         unlink_or_warn(path);
        // }
        // ```
        //
        // (builtin/tag.c:create_tag.) `-e`/`--edit` opens the editor on a message
        // that *was* given; no `-m`/`-F` at all opens it on a template — the body
        // of the tag being replaced when there is one, otherwise a wholly
        // commented prompt that the cleanup below reduces to nothing.
        let raw = if !message_given || edit_flag {
            let path = repo.git_dir().join("TAG_EDITMSG");
            let seed: Vec<u8> = if message_given {
                raw
            } else if let Some(prev) = previous_tag_body(repo, &format!("refs/tags/{name}"))? {
                prev
            } else {
                let mut b = vec![b'\n'];
                let c = super::commit::comment_prefix(&repo.config_snapshot());
                b.extend_from_slice(
                    format!(
                        "{c} Write a message for tag:\n{c}   {name}\n\
                         {c} Lines starting with '{c}' will be ignored.\n"
                    )
                    .as_bytes(),
                );
                b
            };
            std::fs::write(&path, &seed)?;
            if super::commit::launch_editor(&repo.config_snapshot(), &path).is_err() {
                eprintln!("Please supply the message using either -m or -F option.");
                return Ok(ExitCode::from(1));
            }
            let edited = std::fs::read(&path)?;
            let _ = std::fs::remove_file(&path);
            edited
        } else {
            raw
        };
        // git amends the message with `--trailer`s first and only then runs
        // `--cleanup`, so a trailer's own trailing whitespace and the blank lines
        // the trailer engine leaves behind are stripped exactly like body text —
        // and `--cleanup=verbatim` (git's `CLEANUP_NONE`) keeps both verbatim.
        let raw = match amend_with_trailers(repo, raw, trailers)? {
            Some(amended) => amended,
            None => return Ok(ExitCode::from(128)),
        };
        // ```c
        // if (opt->cleanup_mode != CLEANUP_NONE)
        //         strbuf_stripspace(buf,
        //                 opt->cleanup_mode == CLEANUP_ALL ? comment_line_str : NULL);
        // ```
        //
        // (builtin/tag.c:create_tag.) The comment string is passed on the default
        // (`strip`) mode whether or not an editor ran, so `-m`/`-F` text loses its
        // `#` lines too; `whitespace` runs the same pass without them.
        let message = match mode {
            CleanupMode::Verbatim => raw,
            CleanupMode::Whitespace => super::stripspace::strip_space(&raw, None),
            CleanupMode::Strip => {
                let comment = super::commit::comment_prefix(&repo.config_snapshot());
                super::stripspace::strip_space(&raw, Some(comment.as_bytes()))
            }
        };
        // `if (!opt->message_given && !buf->len) die(_("no tag message?"));` — an
        // editor that saved nothing over the template leaves an empty message,
        // while an explicitly empty `-m ''` is accepted.
        if !message_given && message.is_empty() {
            return fatal("no tag message?");
        }

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
            // Never this field: gix writes a newline between the message and the
            // signature, and git writes none — `do_sign()` ends with a bare
            // `strbuf_addbuf(buffer, &sig)` onto a buffer the message already
            // terminated. One extra byte is a different object id and a payload
            // stock git cannot verify, so the block is appended by hand below.
            pgp_signature: None,
        };
        // `build_tag_object()` (builtin/tag.c:269-278): the buffer that is signed
        // is the finished object minus the signature, and the signature is
        // appended to it — a tag's signature is body text, not a header, which is
        // what makes the payload byte-identical to the one `parse_signature()`
        // recovers on the way back in. Both the signed and the unsigned tag are
        // therefore serialised here rather than handed to `write_object`.
        let mut buf = Vec::new();
        gix::objs::WriteTo::write_to(&object, &mut buf)?;
        // git's header ends `"tagger %s\n\n"` unconditionally, so the blank line
        // between the headers and the message is there even when the message is
        // empty; gix writes that newline only along with a non-empty message. One
        // byte, and therefore a different tag id, for every empty-message tag.
        if object.message.is_empty() {
            buf.push(b'\n');
        }
        if let Some(signer) = signer {
            match signer.sign(&buf) {
                Ok(sig) => buf.extend_from_slice(&sig),
                Err(e) => return Ok(sign_failed(repo, e)),
            }
        }
        // `odb_write_object_ext()`'s failure is `error(_("unable to write tag
        // file"))`, which `create_tag` then turns into the same exit-128 pair a
        // signing failure gets.
        repo.objects
            .write_buf(Kind::Tag, &buf)
            .map_err(|_| anyhow!("unable to write tag file"))?
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
///
/// `strbuf_read_file(&buf, file, 0) < 0` is a `die_errno(_("could not open or
/// read '%s'"), file)` in `parse_msg_arg()`, so the wording, the bare strerror
/// tail and the 128 all come from git rather than from this port's own voice.
fn read_message_file(path: &str) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    if path == "-" {
        std::io::stdin().lock().read_to_end(&mut buf)?;
    } else {
        buf = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => crate::git_fatal!(
                "could not open or read '{path}': {}",
                crate::external::strerror(&e)
            ),
        };
    }
    Ok(buf)
}

/// Port of `builtin/tag.c:write_tag_body()` — the seed an editor is given when
/// `-f` replaces an annotated tag and no new message was supplied: the previous
/// tag's message with its signature block, if any, left off.
fn previous_tag_body(repo: &gix::Repository, ref_name: &str) -> Result<Option<Vec<u8>>> {
    let Ok(full) = FullName::try_from(ref_name) else { return Ok(None) };
    let Some(r) = repo.try_find_reference(full.as_ref())? else { return Ok(None) };
    let Some(id) = r.target().try_id().map(gix::hash::oid::to_owned) else { return Ok(None) };
    let Ok(obj) = repo.find_object(id) else { return Ok(None) };
    if obj.kind != Kind::Tag {
        return Ok(None);
    }
    let tag = gix::objs::TagRef::from_bytes(&obj.data, repo.object_hash())?;
    let body = tag.message;
    let end = match tag.pgp_signature {
        Some(sig) => body.len().saturating_sub(sig.len()),
        None => body.len(),
    };
    Ok(Some(body[..end].to_vec()))
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

/// Delete each named tag, printing `Deleted tag '<name>' (was <short>)`.
fn delete_tags(repo: &gix::Repository, positionals: &[&str]) -> Result<ExitCode> {
    if positionals.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // `delete_tags()` collects the refs and hands them to `repo_delete_refs()`,
    // which is one transaction: naming the same tag twice is
    // `multiple updates for ref '<ref>' not allowed`, and *nothing* is deleted —
    // not even the tags named only once. The check is on the refs that exist, so
    // a repeated name that resolves to no tag still reports "not found" instead.
    {
        let mut seen: Vec<String> = Vec::new();
        for name in positionals {
            let ref_name = format!("refs/tags/{name}");
            if FullName::try_from(ref_name.as_str()).is_err()
                || repo.try_find_reference(ref_name.as_str())?.is_none()
            {
                continue;
            }
            if seen.contains(&ref_name) {
                eprintln!(
                    "error: could not delete references: multiple updates for ref '{ref_name}' not allowed"
                );
                return Ok(ExitCode::FAILURE);
            }
            seen.push(ref_name);
        }
    }

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
