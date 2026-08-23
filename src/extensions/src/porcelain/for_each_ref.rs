//! `git for-each-ref` — iterate refs and render them through a `--format` template.
//!
//! Covered, byte-identically with stock git: ref discovery and default
//! `refname`-ordered listing, the `<pattern>` / `--exclude` matching rules
//! (literal path-prefix *or* `wildmatch` with pathname semantics), `--count`
//! (including git's "0 means unlimited"), `--sort` (repeatable, `-` for
//! descending, last key primary, `version:`/`v:` for version ordering),
//! `--points-at`, `--merged`/`--no-merged`, `--contains`/`--no-contains`,
//! `--start-after`, `--stdin`, `--color`, `--omit-empty`,
//! `--ignore-case`, `--include-root-refs`, the `--shell`/`--perl`/`--python`/
//! `--tcl` quoting styles (mutually exclusive, `--no-<style>` clears one), and
//! the format language's `%%` / `%xx` escapes plus the `%(...)` atoms listed on
//! [`parse_atom`].
//!
//! Exit codes follow git: 128 for the `die()` paths (a bad `--merged` operand, a
//! format that fails verification, the `--start-after` conflicts) and 129 for the
//! `parse-options` paths (a missing option value, a bad `--contains` or
//! `--points-at` operand, an unknown option).
//!
//! The conditional `%(if[:equals=<v>|:notequals=<v>])` / `%(then)` / `%(else)` /
//! `%(end)` runs on the same formatting stack git uses, so the quoting rules
//! follow too: literals are never quoted, an atom is quoted only outside any
//! container, and a container's whole output is quoted when it closes.
//! Unbalanced containers are reported while formatting a ref, exactly as git
//! does, which is why a repository with no refs accepts a format missing its
//! `%(end)`.
//!
//! `%(upstream)` and `%(push)` cover the full option set (`:track`,
//! `:trackshort`, `:nobracket`, `:remotename`, `:remoteref`, and the
//! `%(refname)` modifiers). `%(describe)` runs the `describe` subcommand as a
//! subprocess, as git does, so its options stay on one implementation.
//!
//! `%(trailers)` / `%(contents:trailers)` reuse `trailer.c`'s parser out of
//! [`super::interpret_trailers`], including both halves of
//! `format_trailers_from_commit`: the verbatim-block fast path when no
//! rendering option is set, and `format_trailers()` re-rendering the parsed
//! items when one is. The option list is `format_set_trailers_options()`
//! (pretty.c:1288-1330) — `only`, `unfold`, `keyonly`, `valueonly`, `key=`,
//! `separator=`, `key_value_separator=` — parsed by [`parse_trailer_opts`],
//! which keeps git's two distinct rejections and its habit of blaming the text
//! *after* the option that consumed it.
//!
//! Dates go through [`crate::showdate`], this crate's port of `show_date()` and
//! `parse_date_format()`, so `%(authordate:<fmt>)` takes the whole `--date=`
//! vocabulary: the fixed shapes, `relative`, `human`, any `<fmt>-local`, and
//! `format:<strftime>` / `format-local:<strftime>`, whose format reaches the
//! platform `strftime(3)` with git's own `%s`/`%z`/`%Z` substitutions applied
//! first. `grab_date()` re-types a date atom as `FIELD_STR` the moment a format
//! is spelled out, so `--sort=authordate` compares seconds while
//! `--sort=authordate:default` compares the rendered string.
//!
//! The contents atoms are `grab_sub_body_contents()`'s arms, and they are not
//! interchangeable: `%(body)` (`C_BODY_DEP`) keeps a trailing signature block
//! and `%(contents:body)` (`C_BODY`) does not, `%(contents:signature)` is that
//! block alone, `%(contents:size)` measures from the subject rather than from
//! the end of the header, and `%(contents:lines=<n>)` indents continued lines by
//! four spaces. A commit's signature lives in a `gpgsig` header rather than in
//! the message, so those three signature-aware spans only differ on a signed
//! *tag*.
//!
//! `%(authorname:mailmap)` / `%(authoremail:…mailmap…)` rewrite the header line
//! through `.mailmap` before slicing it, as `apply_mailmap_to_header()` does.
//! The email options are a bit set, not alternatives: `mailmap` alone keeps the
//! angle brackets, and `trim,mailmap` is both.
//!
//! Every `%(signature)` option comes from [`crate::gitsig`], this crate's port
//! of `gpg-interface.c`: it runs the checker git runs and keeps the whole of
//! `struct signature_check`, so the bare form renders `sigc->output` (the
//! checker's own report) and `:fingerprint` / `:primarykeyfingerprint` render
//! the `VALIDSIG` line's fields. On an unsigned object every option renders what
//! a zeroed `signature_check` would.
//!
//! `%(deltabase)` is `oid_object_info_extended()`'s `OBJECT_INFO_DELTA_BASE`:
//! the pack entry's base for a deltified object — read from the header for a
//! `OBJ_REF_DELTA` and resolved through the pack index for an `OBJ_OFS_DELTA` —
//! and the null oid for anything else, loose objects included.
//!
//! `%(is-base:<committish>)` is `filter_is_base()` (ref-filter.c:3236), a
//! whole-array pass that runs before the sort and marks exactly one ref per
//! atom. Its `get_branch_base_for_tip()` walk needs commit-graph generation
//! numbers; with no commit-graph file present they are computed the way
//! `ensure_generations_valid()` computes them, as corrected commit dates.
//!
//! `%(rest)` is refused the way git refuses it for this command — that is git's
//! own `reject_atom`, not a gap.
//!
//! `%(ahead-behind:<committish>)` is computed in git's two stages, not one: the
//! atom's operand is peeled when the format is parsed, and then
//! `filter_ahead_behind()` (ref-filter.c:3187) peels **every ref in the array**
//! before the sort. Both peels are `lookup_commit_reference…()` with `quiet = 0`,
//! so a ref that does not peel to a commit produces
//! `error: object <id> is a <kind>, not a commit` on stderr even though its own
//! atom simply renders empty and the command still exits 0. The pass runs ahead
//! of `--count` (applied in `print_formatted_ref_array()`) and is triggered by a
//! `--sort=ahead-behind:<x>` key as much as by a format atom.
//!
//! A second divergence, in stock's favour: with `--include-root-refs`, stock
//! git *drops* `HEAD` and the other root refs from the listing as soon as an
//! option or atom resolves a ref name before the iteration — `%(ahead-behind:…)`,
//! `%(is-base:…)`, `--merged`, `--contains`, `--points-at`. The root refs are
//! only added while the loose-ref cache is first built
//! (`add_root_refs()` under `get_loose_ref_cache()`, refs/files-backend.c:421-451),
//! so a lookup that warms that cache earlier leaves them out for the rest of the
//! process. This port lists them regardless, i.e. it lists what
//! `--include-root-refs` asked for.
//!
//! One known divergence: the `:short` renderings (`%(objectname:short)`,
//! `%(tree:short)`, `%(parent:short)`) take their length from gitoxide's
//! abbreviation logic, which honours `core.abbrev` but, when it is unset,
//! auto-scales off the packed-object count alone where git also counts loose
//! objects, and which does not extend a `:short=<n>` prefix to keep it unique
//! the way git's `find_unique_abbrev` does. The full forms match byte-for-byte.

use anyhow::{anyhow, bail, Result};
use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::glob::wildmatch;
use gix::hash::ObjectId;
use gix::objs::{CommitRef, Kind, TagRef};
use gix::prelude::ObjectIdExt;

use super::{Arg, LongOpt};
use crate::refsort::{self, Prereleases};

/// `cmd_for_each_ref()`'s `struct option opts[]` (builtin/for-each-ref.c:23-53),
/// in table order, as [`super::resolve_long`] reads it.
///
/// The four filter entries come from the ref-filter macros and carry
/// `PARSE_OPT_LASTARG_DEFAULT | PARSE_OPT_NONEG` (`_OPT_MERGED_NO_MERGED`,
/// ref-filter.h:119-130; `_OPT_CONTAINS_OR_WITH`, parse-options.h:609-620), so
/// they take the next argument or their `HEAD` default and have no `--no-`
/// spelling. `--no-merged` and `--no-contains` are entries in their own right,
/// which is why they appear here rather than being generated negations.
pub(super) const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "shell", neg: true, arg: Arg::None },
    LongOpt { name: "perl", neg: true, arg: Arg::None },
    LongOpt { name: "python", neg: true, arg: Arg::None },
    LongOpt { name: "tcl", neg: true, arg: Arg::None },
    LongOpt { name: "omit-empty", neg: true, arg: Arg::None },
    LongOpt { name: "count", neg: true, arg: Arg::Required },
    LongOpt { name: "format", neg: true, arg: Arg::Required },
    LongOpt { name: "start-after", neg: true, arg: Arg::Required },
    LongOpt { name: "color", neg: true, arg: Arg::Optional },
    LongOpt { name: "exclude", neg: true, arg: Arg::Required },
    LongOpt { name: "sort", neg: true, arg: Arg::Required },
    LongOpt { name: "points-at", neg: true, arg: Arg::Required },
    LongOpt { name: "merged", neg: false, arg: Arg::LastArg },
    LongOpt { name: "no-merged", neg: false, arg: Arg::LastArg },
    LongOpt { name: "contains", neg: false, arg: Arg::LastArg },
    LongOpt { name: "no-contains", neg: false, arg: Arg::LastArg },
    LongOpt { name: "ignore-case", neg: true, arg: Arg::None },
    LongOpt { name: "stdin", neg: true, arg: Arg::None },
    LongOpt { name: "include-root-refs", neg: true, arg: Arg::None },
];

/// `usage_with_options()` over `builtin/for-each-ref.c`'s option table.
const USAGE: &str = r"usage: git for-each-ref [--count=<count>] [--shell|--perl|--python|--tcl]
                                [(--sort=<key>)...] [--format=<format>]
                                [--include-root-refs] [--points-at=<object>]
                                [--merged[=<object>]] [--no-merged[=<object>]]
                                [--contains[=<object>]] [--no-contains[=<object>]]
                                [(--exclude=<pattern>)...] [--start-after=<marker>]
                                [ --stdin | (<pattern>...)]

    -s, --[no-]shell      quote placeholders suitably for shells
    -p, --[no-]perl       quote placeholders suitably for perl
    --[no-]python         quote placeholders suitably for python
    --[no-]tcl            quote placeholders suitably for Tcl
    --[no-]omit-empty     do not output a newline after empty formatted refs

    --[no-]count <n>      show only <n> matched refs
    --[no-]format <format>
                          format to use for the output
    --[no-]start-after <marker>
                          start iteration after the provided marker
    --[no-]color[=<when>] respect format colors
    --[no-]exclude <pattern>
                          exclude refs which match pattern
    --[no-]sort <key>     field name to sort on
    --[no-]points-at <object>
                          print only refs which points at the given object
    --merged <commit>     print only refs that are merged
    --no-merged <commit>  print only refs that are not merged
    --contains <commit>   print only refs which contain the commit
    --no-contains <commit>
                          print only refs which don't contain the commit
    --[no-]ignore-case    sorting and filtering are case insensitive
    --[no-]stdin          read reference patterns from stdin
    --[no-]include-root-refs
                          also include HEAD ref and pseudorefs

";

/// The `%(...)` fields this module can evaluate.
#[derive(Clone)]
pub(super) enum Field {
    RefName(NameMod),
    SymRef(NameMod),
    ObjectName(NameLen),
    ObjectType,
    ObjectSize,
    /// `%(is-base:<committish>)` — the operand, kept so the winning ref can
    /// render `(<committish>)`, alongside the commit it resolved to.
    IsBase(String, ObjectId),
    /// `%(deltabase)` — the object this one is stored as a delta against inside
    /// its pack, or the null oid when it is not deltified (or not packed).
    DeltaBase,
    /// `%(tree)` — a commit's tree, with `%(objectname)`-style abbreviation.
    Tree(NameLen),
    /// `%(parent)` — a commit's parents, space-joined, each abbreviated per the
    /// modifier.
    Parent(NameLen),
    /// `%(numparent)` — a commit's parent count.
    NumParent,
    /// `%(object)` — the object a tag points at (empty for a non-tag).
    TargetName,
    /// `%(type)` — the type of the object a tag points at (empty for a non-tag).
    TargetType,
    /// `%(tag)` — a tag object's own tag name (empty for a non-tag).
    TagName,
    Head,
    Person(Who, PersonPart),
    Contents(ContentPart),
    /// `%(color:<spec>)`, pre-rendered: the escape sequence, or empty when
    /// colour is off for this run.
    Color(Vec<u8>),
    /// `%(upstream[:<opts>])` — the remote-tracking ref a local branch tracks.
    Upstream(RemoteRef),
    /// `%(push[:<opts>])` — where a local branch would push.
    Push(RemoteRef),
    /// `%(flag)` — `symref` and/or `packed`, comma-joined.
    Flag,
    /// `%(worktreepath)` — the working tree that has this branch checked out.
    WorktreePath,
    /// `%(describe[:<opts>])`, carrying the argument vector git hands to the
    /// `describe` subprocess.
    Describe(Vec<String>),
    /// `%(ahead-behind:<committish>)` — the resolved base commit.
    AheadBehind(ObjectId),
    /// `%(objectsize:disk)`.
    ObjectSizeDisk,
    /// `%(raw)` (`false`) and `%(raw:size)` (`true`).
    Raw(bool),
    /// `%(signature[:<option>])` — a commit's signature verification result.
    Signature(SigOption),
}

/// git's `signature` atom options, in `parse_signature_option`'s order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SigOption {
    /// `%(signature)` — `sigc->output`, gpg's own human-readable report.
    Bare,
    /// `%(signature:signer)` — `sigc->signer`.
    Signer,
    /// `%(signature:grade)` — `sigc->result`, with a good-but-untrusted `G`
    /// folded to `U`.
    Grade,
    /// `%(signature:key)` — `sigc->key`.
    Key,
    /// `%(signature:fingerprint)` — `sigc->fingerprint`.
    Fingerprint,
    /// `%(signature:primarykeyfingerprint)` — `sigc->primary_key_fingerprint`.
    PrimaryKeyFingerprint,
    /// `%(signature:trustlevel)` — `gpg_trust_level_to_str(sigc->trust_level)`.
    TrustLevel,
}

/// git's `remote_ref` atom options, shared by `%(upstream)` and `%(push)`.
#[derive(Clone)]
#[cfg_attr(test, derive(Debug))]
pub(super) struct RemoteRef {
    option: RrOption,
    /// `nobracket` — drop the `[...]` around a `:track` rendering.
    nobracket: bool,
    /// Set by `remotename` / `remoteref`, which name the remote rather than the
    /// tracking ref, so `%(push:remotename)` never resolves a push destination.
    push_remote: bool,
}

/// Which rendering a `%(upstream)` / `%(push)` atom asks for.
#[derive(Clone)]
#[cfg_attr(test, derive(Debug))]
pub(super) enum RrOption {
    /// The default: the tracking refname itself, under `%(refname)`'s modifiers.
    Ref(NameMod),
    /// `:track` — `ahead N`/`behind N`/`ahead N, behind M`/`gone`.
    Track,
    /// `:trackshort` — `=`, `<`, `>` or `<>`.
    TrackShort,
    /// `:remotename` — the configured remote's name.
    RemoteName,
    /// `:remoteref` — the ref name on the remote side.
    RemoteRefName,
}

/// `%(if)`'s comparison mode (git's `cmp_status`).
#[derive(Clone)]
pub(super) enum Cmp {
    /// Bare `%(if)`: any non-blank content satisfies the condition.
    None,
    /// `%(if:equals=<v>)`.
    Equal(String),
    /// `%(if:notequals=<v>)`.
    Unequal(String),
}

/// Modifiers accepted by `%(refname)` and `%(symref)`.
#[derive(Clone)]
#[cfg_attr(test, derive(Debug))]
pub(super) enum NameMod {
    Full,
    Short,
    /// `:lstrip=<n>` (`:strip=` is a synonym).
    LStrip(i64),
    /// `:rstrip=<n>`.
    RStrip(i64),
}

/// Modifiers accepted by `%(objectname)`.
#[derive(Clone, Debug)]
pub(super) enum NameLen {
    Full,
    /// `:short` — length from `core.abbrev`, auto-scaled when unset.
    Auto,
    /// `:short=<n>`.
    Fixed(usize),
}

/// Which name-email-date header a person atom reads.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Who {
    Author,
    Committer,
    Tagger,
    /// `committer` on commits, `tagger` on tags.
    Creator,
}

/// Which component of a name-email-date tuple an atom extracts.
#[derive(Clone)]
pub(super) enum PersonPart {
    /// The whole `Name <email> <secs> <tz>` tuple.
    Full,
    /// `%(authorname[:mailmap])`, git's `N_RAW` / `N_MAILMAP`.
    Name { mailmap: bool },
    /// `%(authoremail[:<opts>])`, git's `EO_*` bit set.
    Email(EmailOpt),
    /// `%(authordate[:<fmt>])`. `None` is the *no colon at all* case, which is
    /// the only one git leaves as `FIELD_TIME` for sorting (`grab_date()` sets
    /// `v->atom->type = FIELD_STR` the moment a format is spelled out, even
    /// `:default`).
    Date(Option<crate::showdate::DateMode>),
}

/// `email_atom_option_parser`'s bit set: `EO_RAW` is the empty one.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct EmailOpt(u8);

impl EmailOpt {
    const TRIM: u8 = 1 << 0;
    const LOCALPART: u8 = 1 << 1;
    const MAILMAP: u8 = 1 << 2;

    fn has(self, bit: u8) -> bool {
        self.0 & bit != 0
    }
}

/// Which slice of a commit/tag message a contents atom extracts — git's
/// `enum contents_option`, whose arms are *not* interchangeable: `%(body)` and
/// `%(contents:body)` are different options (`C_BODY_DEP` vs `C_BODY`) and
/// differ on whether a signature block belongs to the body.
#[derive(Clone)]
pub(super) enum ContentPart {
    /// `%(contents)` — `C_BARE`: the message from the subject on, signature
    /// included.
    Bare,
    /// `%(subject)` — `C_SUB`.
    Subject,
    /// `%(subject:sanitize)` — `C_SUB_SANITIZE`.
    SubjectSanitize,
    /// `%(body)` — `C_BODY_DEP`: everything after the subject, signature
    /// included. The `_DEP` in git's name is "deprecated", not "dependent".
    BodyDep,
    /// `%(contents:body)` — `C_BODY`: the same span with the signature cut off.
    Body,
    /// `%(contents:size)` — `C_LENGTH`, the length of the whole message from the
    /// subject on.
    Size,
    /// `%(contents:signature)` — `C_SIG`.
    Sig,
    /// `%(contents:lines=<n>)` — `C_LINES`.
    Lines(u32),
    /// `%(trailers[:<opts>])` / `%(contents:trailers[:<opts>])` — `C_TRAILERS`.
    Trailers(super::interpret_trailers::PrettyOpts),
}

/// One `%(...)` atom: an optional leading `*` (evaluate against the peeled
/// object) plus the field itself.
#[derive(Clone)]
pub(super) struct Atom {
    pub(super) deref: bool,
    pub(super) field: Field,
}

/// A parsed format string is a sequence of literal runs, atoms, and the
/// `%(align:…)` / `%(end)` container markers that pad the content between them.
pub(super) enum Item {
    Lit(Vec<u8>),
    Atom(Atom),
    AlignStart(AlignSpec),
    /// `%(if[:equals=<v>|:notequals=<v>])`.
    IfStart(Cmp),
    Then,
    Else,
    End,
}

/// `%(align:<width>,<position>)` — pad the enclosed content to `width` display
/// columns; content already at or over `width` is left untouched (never cut).
#[derive(Clone)]
pub(super) struct AlignSpec {
    pub(super) width: usize,
    pub(super) position: AlignPos,
}

#[derive(Clone, Copy)]
pub(super) enum AlignPos {
    Left,
    Right,
    Middle,
}

/// A sort key: an atom, its direction, and whether to compare with `versioncmp`
/// (the `version:` / `v:` prefix).
pub(super) struct SortKey {
    pub(super) atom: Atom,
    pub(super) descending: bool,
    pub(super) versioned: bool,
}

/// Everything known about one object referenced during a run.
pub(super) struct ObjInfo {
    pub(super) id: ObjectId,
    pub(super) kind: Kind,
    pub(super) size: u64,
    /// Full object data, loaded only when a person/contents atom needs it.
    pub(super) data: Option<Vec<u8>>,
}

/// One ref, resolved and ready to render.
pub(super) struct RefInfo {
    pub(super) refname: Vec<u8>,
    /// `%(refname:short)`, computed only when the format or a sort key asks.
    pub(super) short: Vec<u8>,
    /// The target of a symbolic ref, empty for a direct one.
    pub(super) symref: Vec<u8>,
    /// `%(symref:short)`, computed only when asked for.
    pub(super) symref_short: Vec<u8>,
    pub(super) obj: ObjInfo,
    /// Present only when `obj` is a tag object, holding its fully peeled target.
    pub(super) peeled: Option<ObjInfo>,
    pub(super) is_head: bool,
    /// git's `REF_ISPACKED`: the ref has no loose file, so it was read out of
    /// `packed-refs`. Feeds `%(flag)`.
    pub(super) packed: bool,
    /// `ref_array_item.is_base`: the `%(is-base:<committish>)` operands that
    /// chose *this* ref, filled in by [`filter_is_base`] before sorting. Every
    /// other ref keeps an empty list and renders those atoms empty.
    pub(super) is_base: Vec<String>,
    /// `FILTER_REFS_DETACHED_HEAD`: `git branch --list` adds a pseudo entry for a
    /// detached HEAD whose `%(refname)` is `get_head_description()` rather than
    /// the ref name, under *every* `refname` modifier:
    ///
    /// ```c
    /// static const char *get_refname(struct used_atom *atom, struct ref_array_item *ref)
    /// {
    ///         if (ref->kind & FILTER_REFS_DETACHED_HEAD)
    ///                 return get_head_description();
    ///         return show_ref(&atom->u.refname, ref->refname);
    /// }
    /// ```
    ///
    /// `for-each-ref` never sets it; it is `None` for every ordinary ref.
    pub(super) head_desc: Option<Vec<u8>>,
}

/// Everything `parse_atom` needs beyond the atom text itself.
///
/// `repo` is `None` only in unit tests, which never parse a repository-dependent
/// atom; an atom that needs one reports the failure git reports when the operand
/// cannot be resolved rather than quietly accepting it.
pub(super) struct AtomCtx<'a> {
    pub(super) repo: Option<&'a gix::Repository>,
    pub(super) color_on: bool,
    /// git's `format->quote_style`, which `%(raw)` is rejected under. Sort keys
    /// parse through a fresh `REF_FORMAT_INIT`, i.e. `QuoteStyle::None`.
    pub(super) quote_style: QuoteStyle,
}

/// Which exit code a format/sort parse failure maps onto.
///
/// git splits the first two: `verify_ref_format` failures `die()` (128) and a
/// malformed `%(` reaches the `parse-options` usage path (129). The third is not
/// a git failure at all — it is this port saying it has not built something —
/// and [`report_atom_error`] keeps it out of git's voice.
#[cfg_attr(test, derive(Debug))]
pub(super) enum ErrKind {
    Fatal,
    Usage,
    Unported,
}

/// A format or sort-key parse failure, carrying the exit code it implies.
#[cfg_attr(test, derive(Debug))]
pub(super) struct AtomError {
    pub(super) kind: ErrKind,
    pub(super) msg: String,
}

fn fatal_atom(msg: impl Into<String>) -> AtomError {
    AtomError {
        kind: ErrKind::Fatal,
        msg: msg.into(),
    }
}

fn usage_atom(msg: impl Into<String>) -> AtomError {
    AtomError {
        kind: ErrKind::Usage,
        msg: msg.into(),
    }
}

fn unported_atom(msg: impl Into<String>) -> AtomError {
    AtomError {
        kind: ErrKind::Unported,
        msg: msg.into(),
    }
}

/// `strtoul_ui()` (git-compat-util.h:962-976): a `10`-base unsigned int, with
/// git's own extra rejections — any `-` anywhere is refused outright (rather
/// than wrapping the way `strtoul` would), the whole string must be consumed,
/// and the value must fit an `unsigned int`.
fn strtoul_ui(s: &str) -> Option<u32> {
    if s.contains('-') {
        return None;
    }
    // `strtoul` skips leading whitespace and allows one leading `+`.
    let t = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let t = t.strip_prefix('+').unwrap_or(t);
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<u32>().ok()
}

/// `git_parse_maybe_bool()` (parse.c:166-192) restricted to what a placeholder
/// argument can carry: the three true and three false spellings, then any
/// integer (non-zero is true). `None` is git's `-1`.
///
/// Distinct from [`maybe_bool`], which is `match_atom_bool_arg`'s much narrower
/// `true`/`false`-only test — `%(describe:tags=yes)` and `%(trailers:only=yes)`
/// really do disagree.
fn git_parse_maybe_bool(value: Option<&str>) -> Option<bool> {
    let Some(v) = value else { return Some(true) };
    if v.is_empty() {
        return Some(false);
    }
    for (words, answer) in [(["true", "yes", "on"], true), (["false", "no", "off"], false)] {
        if words.iter().any(|w| w.eq_ignore_ascii_case(v)) {
            return Some(answer);
        }
    }
    v.parse::<i64>().ok().map(|n| n != 0)
}

/// `match_placeholder_arg_value()` (pretty.c:1195-1224): peel `<candidate>`,
/// optionally `=<value>`, off the head of a `,`-separated list that ends at `)`.
///
/// `Some((value, rest))` on a match, where `value` is `None` for a bare option
/// and the value itself runs to the next `,` or `)`. A name that is a *prefix*
/// of the text but is not followed by `=`, `,` or `)` does not match at all,
/// which is what keeps `only` from claiming `onlyx`.
fn match_placeholder<'a>(to_parse: &'a str, candidate: &str) -> Option<(Option<&'a str>, &'a str)> {
    let p = to_parse.strip_prefix(candidate)?;
    let (value, p) = match p.strip_prefix('=') {
        Some(v) => {
            let len = v.find([',', ')']).unwrap_or(v.len());
            (Some(&v[..len]), &v[len..])
        }
        None => {
            if !p.starts_with(',') && !p.starts_with(')') {
                return None;
            }
            (None, p)
        }
    };
    if let Some(rest) = p.strip_prefix(',') {
        return Some((value, rest));
    }
    if p.starts_with(')') {
        return Some((value, p));
    }
    None
}

/// Which boolean `%(trailers:...)` flag a name matched.
#[derive(Clone, Copy)]
enum Flag {
    Only,
    Unfold,
    KeyOnly,
    ValueOnly,
}

/// `format_set_trailers_options()` (pretty.c:1288-1330), as reached from
/// `trailers_atom_parser` (ref-filter.c:570-610).
///
/// git appends a `)` to the argument and parses until it, so the terminator is
/// part of the grammar rather than of the caller's string; that is reproduced
/// here so a value stops at `,` or `)` exactly as it does there.
///
/// The two failure messages are distinct and both are git's:
///
///   * `key` with no `=<value>` is `expected %(trailers:key=<value>)`;
///   * anything else is `unknown %(trailers) argument: <text>`, where the text
///     is `strcspn(*arg, ",)")` of whatever is left *after* the options that did
///     match — empty when a recognised option was given an unparseable boolean.
fn parse_trailer_opts(
    arg: Option<&str>,
) -> std::result::Result<super::interpret_trailers::PrettyOpts, AtomError> {
    let mut opts = super::interpret_trailers::PrettyOpts::default();
    let Some(arg) = arg else { return Ok(opts) };

    let owned = format!("{arg})");
    let mut to_parse = owned.as_str();
    loop {
        if to_parse.starts_with(')') {
            return Ok(opts);
        }
        if let Some((value, rest)) = match_placeholder(to_parse, "key") {
            let Some(value) = value else {
                return Err(fatal_atom("expected %(trailers:key=<value>)"));
            };
            // A key spelled with its separator attached matches the same
            // trailer; git shortens the comparison length rather than the text.
            let key = value.strip_suffix(':').unwrap_or(value);
            opts.keys.push(key.as_bytes().to_vec());
            opts.only = true;
            to_parse = rest;
            continue;
        }
        if let Some((value, rest)) = match_placeholder(to_parse, "separator") {
            opts.separator = Some(expand_string_arg(value.unwrap_or("")));
            to_parse = rest;
            continue;
        }
        if let Some((value, rest)) = match_placeholder(to_parse, "key_value_separator") {
            opts.key_value_separator = Some(expand_string_arg(value.unwrap_or("")));
            to_parse = rest;
            continue;
        }

        // `match_placeholder_bool_arg()` (pretty.c:1226-1251) for the four flags.
        // A recognised name consumes its text *before* its value is judged — git
        // advances `*arg` inside `match_placeholder_arg_value` and only then
        // fails on the boolean — so `%(trailers:only=bogus)` reports an empty
        // argument name, taken from what is left after `only=bogus`.
        let bools: [(&str, Flag); 4] = [
            ("only", Flag::Only),
            ("unfold", Flag::Unfold),
            ("keyonly", Flag::KeyOnly),
            ("valueonly", Flag::ValueOnly),
        ];
        let hit = bools
            .into_iter()
            .find_map(|(name, flag)| match_placeholder(to_parse, name).map(|m| (flag, m)));
        let Some((flag, (value, rest))) = hit else {
            return Err(unknown_trailer_arg(to_parse));
        };
        to_parse = rest;
        let Some(v) = git_parse_maybe_bool(value) else {
            return Err(unknown_trailer_arg(to_parse));
        };
        match flag {
            Flag::Only => opts.only = v,
            Flag::Unfold => opts.unfold = v,
            Flag::KeyOnly => opts.keyonly = v,
            Flag::ValueOnly => opts.valueonly = v,
        }
    }
}

/// `strbuf_addf(err, _("unknown %%(trailers) argument: %s"), invalid_arg)` with
/// `invalid_arg = xstrndup(*arg, strcspn(*arg, ",)"))`.
fn unknown_trailer_arg(rest: &str) -> AtomError {
    let len = rest.find([',', ')']).unwrap_or(rest.len());
    fatal_atom(format!("unknown %(trailers) argument: {}", &rest[..len]))
}

/// `expand_string_arg()` (pretty.c:1267-1285): the `%n`, `%xNN` and `%%` escapes
/// a separator value may use. Anything else keeps its literal `%`.
fn expand_string_arg(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some(b'%') => {
                out.push(b'%');
                i += 2;
            }
            Some(b'n') => {
                out.push(b'\n');
                i += 2;
            }
            Some(b'x') => {
                let hex = bytes
                    .get(i + 2..i + 4)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 4;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b'%');
                i += 1;
            }
        }
    }
    out
}

/// `email_atom_option_parser()` (ref-filter.c:769-779) driven by
/// `person_email_atom_parser`'s loop: options are matched by *prefix*, OR-ed
/// into one bit set, and separated by commas.
fn parse_email_opts(
    dname: &str,
    arg: Option<&str>,
) -> std::result::Result<EmailOpt, AtomError> {
    let Some(mut arg) = arg else { return Ok(EmailOpt::default()) };
    let mut opt = EmailOpt::default();
    loop {
        // `EO_RAW` is only reachable for a NULL argument, handled above.
        let hit = [
            ("trim", EmailOpt::TRIM),
            ("localpart", EmailOpt::LOCALPART),
            ("mailmap", EmailOpt::MAILMAP),
        ]
        .into_iter()
        .find_map(|(name, bit)| arg.strip_prefix(name).map(|rest| (bit, rest)));
        let Some((bit, rest)) = hit else {
            return Err(fatal_atom(format!("unrecognized %({dname}) argument: {arg}")));
        };
        opt.0 |= bit;
        arg = rest;
        if arg.is_empty() {
            return Ok(opt);
        }
        match arg.strip_prefix(',') {
            Some(next) => arg = next,
            // `bad_arg` is the text as it stood *after* the successful prefix,
            // so `%(authoremail:trimx)` reports `x`.
            None => return Err(fatal_atom(format!("unrecognized %({dname}) argument: {arg}"))),
        }
    }
}

/// Turn a parse failure into the exit code git would produce.
///
/// The third arm is deliberately *not* one of git's: `fatal: …` at 128 is a
/// claim that this is what git does here, and a gap in this port is not that.
/// It keeps the `zvcs: <verb>: …` prefix and exit 1 that mark the port speaking
/// for itself — see `crate::fatal`.
pub(super) fn report_atom_error(e: AtomError) -> Result<ExitCode> {
    match e.kind {
        ErrKind::Fatal => Ok(fatal(&e.msg)),
        ErrKind::Usage => Ok(usage_error(&e.msg)),
        ErrKind::Unported => bail!("{}", e.msg),
    }
}

/// git's `die()`: message on stderr, exit 128.
pub(super) fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// The rejections that reach `usage_with_options()`: the `error:` line **and**
/// the usage block, both on stderr, exit 129.
///
/// parse-options splits its two failure shapes by which one prints the block,
/// and the split is not cosmetic — it is which C path produced the failure:
///
/// ```c
/// case PARSE_OPT_ERROR:
///         exit(129);                                  /* no block */
/// ...
/// case PARSE_OPT_UNKNOWN:
///         ...
///         error(_("unknown switch `%c'"), *ctx.opt);
///         usage_with_options(usagestr, options);      /* block */
/// ```
///
/// `PARSE_OPT_ERROR` is what `get_arg()` and a rejecting option callback return
/// (`PARSE_OPT_ERROR = -1, /* must be the same as error() */`), so a bad option
/// *value* gets one line — that is [`option_error`]. Everything else here —
/// an unknown option or switch, and the two checks `cmd_for_each_ref()` runs
/// itself after `parse_options()` returns (`invalid --count argument` and
/// `more than one quoting style?`, builtin/for-each-ref.c:63-69) — calls
/// `usage_with_options()` and gets the block.
///
/// Verified against stock 2.55.0 in a one-commit repo, stderr only:
/// `for-each-ref -Z` 1923 bytes, `--count=-1` 1935, `--shell --perl` 1933,
/// `--format=%(refname` 1938; against `--count` 39, `--sort` 38 and
/// `--points-at zzz` 35 for the value errors.
fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// The `PARSE_OPT_ERROR` shape: the `error:` line alone, exit 129, no usage
/// block. See [`usage_error`] for why the two are different.
fn option_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(129)
}

/// When `%(color:...)` atoms should emit escape sequences.
#[derive(Clone, Copy, PartialEq)]
enum ColorWhen {
    Always,
    Never,
    Auto,
}

/// How `--shell`/`--perl`/`--python`/`--tcl` quote each rendered atom.
///
/// git tracks the four styles as independent bits (see the `Q_*` masks) so that
/// repeating one style is harmless but requesting two distinct ones is
/// "more than one quoting style?". `--no-<style>` clears its bit.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum QuoteStyle {
    None,
    Shell,
    Perl,
    Python,
    Tcl,
}

const Q_SHELL: u8 = 1;
const Q_PERL: u8 = 2;
const Q_PYTHON: u8 = 4;
const Q_TCL: u8 = 8;

/// The reachability filters, each a list of commits combined with "any".
#[derive(Default)]
pub(super) struct Filters {
    pub(super) contains: Vec<ObjectId>,
    pub(super) no_contains: Vec<ObjectId>,
    pub(super) merged: Vec<ObjectId>,
    pub(super) no_merged: Vec<ObjectId>,
}

impl Filters {
    pub(super) fn active(&self) -> bool {
        !self.contains.is_empty()
            || !self.no_contains.is_empty()
            || !self.merged.is_empty()
            || !self.no_merged.is_empty()
    }
}

/// `git for-each-ref` — output information on each ref.
///
/// Refs are collected from `refs/` (root refs such as `HEAD` are excluded, as
/// stock git does without `--include-root-refs`), filtered, sorted, truncated to
/// `--count`, and rendered through `--format`, which defaults to
/// `%(objectname) %(objecttype)\t%(refname)`.
///
/// A run that matches no refs prints nothing and exits 0, matching stock git.
pub fn for_each_ref(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail, but tolerate the subcommand
    // being present at index 0 so both calling conventions behave the same.
    let args = match args.first() {
        Some(a) if a == "for-each-ref" => &args[1..],
        _ => args,
    };

    // git resolves `--points-at` / `--contains` / `--merged` operands *while*
    // parsing options, so a bad one is reported before the format is verified.
    // That ordering is observable, so the repository has to be open first.
    let repo = gix::discover(".")?;

    // What `--no-format` restores, and the one place this port deliberately does
    // *not* reproduce stock byte for byte.
    //
    // `for_each_ref_core()` assigns this default at builtin/for-each-ref.c:55,
    // which is *before* `parse_options()` at :62, and never re-checks it
    // afterwards. `OPTION_STRING`'s unset arm is
    // `*(const char **)opt->value = NULL`, so `--no-format` leaves `format.format`
    // NULL and `verify_ref_format()` (ref-filter.c:1390) opens with
    // `for (cp = format->format; *cp && ...)` — an unguarded NULL dereference.
    // Verified against stock 2.55.0: `git for-each-ref --no-format` dies of
    // SIGSEGV (shell status 139), as does `--format=<x> --no-format`.
    //
    // Reproducing a segfault is not parity with anything worth having, so this
    // restores the built-in default instead: the value the field held before
    // `--no-format` was seen, which is what the option plainly means and what git
    // would do had the assignment sat after the parse. Name resolution is
    // unaffected either way — `--no-format` and every prefix of it resolve here
    // exactly as they do in stock, which is what the abbreviation census checks.
    const DEFAULT_FORMAT: &[u8] = b"%(objectname) %(objecttype)\t%(refname)";

    let mut format: Vec<u8> = DEFAULT_FORMAT.to_vec();
    let mut count: i64 = 0;
    let mut sort_specs: Vec<String> = Vec::new();
    let mut patterns: Vec<String> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut points_at: Option<ObjectId> = None;
    let mut start_after: Option<String> = None;
    let mut color_when = ColorWhen::Auto;
    let mut filters = Filters::default();
    let mut omit_empty = false;
    let mut ignore_case = false;
    let mut include_root_refs = false;
    let mut from_stdin = false;
    // Each quoting style is an independent bit, mirroring git's `OPT_BIT`.
    let mut quote_bits: u8 = 0;

    let mut i = 0;
    let mut only_patterns = false;

    // Pull the value of `--opt=<v>` or the following argument of `--opt <v>`.
    let take_value = |i: &mut usize, rest: Option<&str>| -> Option<String> {
        match rest {
            Some(v) => Some(v.to_string()),
            None => {
                *i += 1;
                args.get(*i).cloned()
            }
        }
    };

    // `--opt <v>`, or git's usage error when the value is missing.
    macro_rules! value {
        ($rest:expr, $name:expr) => {
            match take_value(&mut i, $rest) {
                Some(v) => v,
                None => return Ok(option_error(&format!("option `{}' requires a value", $name))),
            }
        };
    }

    // The `PARSE_OPT_LASTARG_DEFAULT` operand shared by the four reachability
    // filters: `--opt=<v>` uses `<v>`, a trailing bare `--opt` defaults to
    // `HEAD`, and otherwise the next argument is consumed whatever it looks like.
    macro_rules! commit_operand {
        ($rest:expr) => {
            match $rest {
                Some(v) => v.to_string(),
                None if i + 1 >= args.len() => "HEAD".to_string(),
                None => {
                    i += 1;
                    args[i].clone()
                }
            }
        };
    }

    while i < args.len() {
        let typed = args[i].as_str();
        let a = typed;
        if only_patterns {
            patterns.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            only_patterns = true;
            i += 1;
            continue;
        }
        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1124) is a bare `strcmp` sitting ahead of
        // `parse_long_opt()`, so the name neither abbreviates nor takes a value:
        // `--help-a` and `--help-all=x` are both unknown options. Matching it
        // after the `=` split accepted `--help-all=x` as a help request.
        if a == "--help-all" {
            return Ok(super::show_usage(USAGE));
        }
        let resolved = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        let a = resolved.as_ref();
        let (name, rest) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (a, None),
        };
        // `get_value()` decides whether an attached value is legal from the
        // entry's *type*, not per arm: an `OPTION_SET_INT`/`OPTION_BIT`/
        // `OPTION_BOOL` never takes one, and neither does the unset sense of
        // anything, because that sense is a pure boolean whatever the entry is.
        // Refusing it is `PARSE_OPT_ERROR`, so the line goes out alone with no
        // usage block. Deriving this from the table rather than repeating it in
        // fifteen arms is what keeps `--no-sort=x` from slipping through when
        // `--no-sort` is right.
        if rest.is_some() {
            if let Some(body) = name.strip_prefix("--") {
                if let super::Resolved::One(opt, unset) = super::resolve_long(LONG_OPTS, body) {
                    if unset || opt.arg == super::Arg::None {
                        // `optname()`: the table's own spelling, `no-`-prefixed
                        // for the unset sense.
                        eprintln!("error: option `{}' takes no value", &body);
                        return Ok(ExitCode::from(129));
                    }
                }
            }
        }
        match name {
            // parse_options_step()'s `internal_help`: the block on stdout at
            // 129, with no `error:` line ahead of it.
            "-h" => return Ok(super::show_usage(USAGE)),
            "--format" => format = value!(rest, "format").into_bytes(),
            "--count" => {
                let v = value!(rest, "count");
                count =
                    match parse_count(&v) {
                        Some(n) => n,
                        None => return Ok(option_error(
                            "option `count' expects an integer value with an optional k/m/g suffix",
                        )),
                    };
            }
            "--sort" => sort_specs.push(value!(rest, "sort")),
            "--exclude" => excludes.push(value!(rest, "exclude")),
            "--start-after" => start_after = Some(value!(rest, "start-after")),
            // The unset sense of each value-taking entry, which every one of
            // these has because none carries `PARSE_OPT_NONEG`. What it does is
            // fixed by the entry's *type*, not by the builtin:
            //   * `OPTION_STRING`  → `NULL` (parse-options.c, `do_get_value`)
            //   * `OPTION_INTEGER` → `0`, which `cmd_for_each_ref` reads as
            //     "no limit"
            //   * `OPT_STRVEC`      → `strvec_clear()` (parse_opt_strvec)
            //   * `OPT_STRING_LIST` → `string_list_clear()`
            //     (parse_opt_string_list)
            //   * `parse_opt_object_name` → `oid_array_clear()`
            "--no-format" => format = DEFAULT_FORMAT.to_vec(),
            "--no-count" => count = 0,
            "--no-start-after" => start_after = None,
            "--no-exclude" => excludes.clear(),
            "--no-sort" => sort_specs.clear(),
            "--no-points-at" => points_at = None,
            "--points-at" => {
                let v = value!(rest, "points-at");
                // `parse_opt_object_name`: the id is recorded without ever asking
                // the odb whether that object exists, because `--points-at` only
                // ever compares it against ref tips. An absent full-length hex id
                // is therefore a filter that matches nothing at exit 0.
                points_at = match crate::objname::parse_opt_object_name(&repo, &v) {
                    Ok(id) => Some(id),
                    Err(e) => return Ok(e.report()),
                };
            }
            "--contains" | "--no-contains" => {
                let v = commit_operand!(rest);
                let id = match crate::objname::parse_opt_commits(&repo, &v) {
                    Ok(id) => id,
                    Err(e) => return Ok(e.report()),
                };
                if name == "--contains" {
                    filters.contains.push(id);
                } else {
                    filters.no_contains.push(id);
                }
            }
            "--merged" | "--no-merged" => {
                let v = commit_operand!(rest);
                // `parse_opt_merge_filter` reports the same two failures as
                // `parse_opt_commits` but not with the same severity, and names
                // the option rather than the operand; `&name[2..]` is git's
                // `opt->long_name`.
                let id = match crate::objname::parse_opt_merge_filter(&repo, &v, &name[2..]) {
                    Ok(id) => id,
                    Err(e) => return Ok(e.report()),
                };
                if name == "--merged" {
                    filters.merged.push(id);
                } else {
                    filters.no_merged.push(id);
                }
            }
            // `OPT__COLOR` is `PARSE_OPT_OPTARG`: a bare `--color` never eats
            // the next argument, it just means "always".
            "--color" => {
                color_when = match rest {
                    None | Some("always") => ColorWhen::Always,
                    Some("never") => ColorWhen::Never,
                    Some("auto") => ColorWhen::Auto,
                    Some(_) => {
                        return Ok(option_error(
                            "option `color' expects \"always\", \"auto\", or \"never\"",
                        ))
                    }
                }
            }
            "--no-color" => color_when = ColorWhen::Never,
            "--omit-empty" => omit_empty = true,
            "--no-omit-empty" => omit_empty = false,
            "--ignore-case" => ignore_case = true,
            "--no-ignore-case" => ignore_case = false,
            "--include-root-refs" => include_root_refs = true,
            "--no-include-root-refs" => include_root_refs = false,
            "--shell" | "-s" => quote_bits |= Q_SHELL,
            "--no-shell" => quote_bits &= !Q_SHELL,
            "--perl" | "-p" => quote_bits |= Q_PERL,
            "--no-perl" => quote_bits &= !Q_PERL,
            "--python" => quote_bits |= Q_PYTHON,
            "--no-python" => quote_bits &= !Q_PYTHON,
            "--tcl" => quote_bits |= Q_TCL,
            "--no-tcl" => quote_bits &= !Q_TCL,
            "--stdin" => from_stdin = true,
            "--no-stdin" => from_stdin = false,
            // `PARSE_OPT_UNKNOWN` (parse-options.c:889-898) names an *option*
            // for a `--` spelling and a *switch* for a short one.
            // The message names the argument as typed, `=<value>` and all, so it
            // has to come from `typed` rather than from the split `name`.
            s if s.starts_with("--") => {
                return Ok(usage_error(&format!("unknown option `{}'", &typed[2..])))
            }
            s if s.starts_with('-') && s.len() > 1 => {
                let c = s[1..].chars().next().unwrap_or_default();
                return Ok(usage_error(&match c.is_ascii() {
                    true => format!("unknown switch `{c}'"),
                    false => format!("unknown non-ascii option in string: `{s}'"),
                }));
            }
            s => patterns.push(s.to_string()),
        }
        i += 1;
    }

    if count < 0 {
        return Ok(usage_error(&format!("invalid --count argument: `{count}'")));
    }
    // git treats 0 as "no limit".
    let count = if count > 0 {
        Some(count as usize)
    } else {
        None
    };

    // git rejects two *distinct* styles right after option parsing; repeating
    // one (or clearing it with `--no-<style>`) leaves a single bit or none.
    if quote_bits.count_ones() > 1 {
        return Ok(usage_error("more than one quoting style?"));
    }
    let quote_style = match quote_bits {
        0 => QuoteStyle::None,
        Q_SHELL => QuoteStyle::Shell,
        Q_PERL => QuoteStyle::Perl,
        Q_PYTHON => QuoteStyle::Python,
        Q_TCL => QuoteStyle::Tcl,
        _ => unreachable!("count_ones() <= 1 leaves a single style bit"),
    };

    let color_on = match color_when {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => std::io::stdout().is_terminal(),
    };

    // git's order after option parsing: verify the format, then reject the
    // `--start-after` combinations, then parse the sort keys.
    let fmt_ctx = AtomCtx {
        repo: Some(&repo),
        color_on,
        quote_style,
    };
    let (items, color_reset_at_eol) = match parse_format(&format, &fmt_ctx) {
        Ok(v) => v,
        Err(e) => return report_atom_error(e),
    };

    // git checks the custom-sort conflict before parsing the sort keys, and the
    // pattern conflict only after `--stdin` has populated the pattern list.
    if start_after.is_some() && !sort_specs.is_empty() {
        return Ok(fatal("cannot use --start-after with custom sort options"));
    }

    let mut sorts: Vec<SortKey> = Vec::new();
    for spec in &sort_specs {
        // git strips a leading `-` (descending) first, then the `version:`/`v:`
        // prefix, then parses the remainder as an ordinary sorting atom.
        let (spec, descending) = match spec.strip_prefix('-') {
            Some(r) => (r, true),
            None => (spec.as_str(), false),
        };
        let (spec, versioned) = match spec
            .strip_prefix("version:")
            .or_else(|| spec.strip_prefix("v:"))
        {
            Some(r) => (r, true),
            None => (spec, false),
        };
        // git parses a sort key through a fresh `REF_FORMAT_INIT`, so the
        // format's quoting style does not apply to it.
        let sort_ctx = AtomCtx {
            repo: Some(&repo),
            color_on,
            quote_style: QuoteStyle::None,
        };
        match parse_atom(spec, &sort_ctx) {
            Ok(atom) => sorts.push(SortKey {
                atom,
                descending,
                versioned,
            }),
            Err(e) => return report_atom_error(e),
        }
    }
    // Later `--sort` options take precedence, so the last given key sorts first.
    sorts.reverse();

    // `--stdin`: git dies if any positional patterns were also given, then reads
    // newline-delimited patterns from stdin into the pattern list.
    if from_stdin {
        if !patterns.is_empty() {
            return Ok(fatal("unknown arguments supplied with --stdin"));
        }
        patterns = read_stdin_patterns()?;
    }

    // git rejects `--start-after` combined with patterns only after `--stdin`
    // has been folded in, so stdin-supplied patterns trigger it too.
    if start_after.is_some() && !patterns.is_empty() {
        return Ok(fatal("cannot use --start-after with patterns"));
    }

    // Version sorting reads its prerelease-suffix ordering from config once, on
    // the first comparison that needs it (`versioncmp.c:162-172`).
    let prereleases = Prereleases::new(&repo);

    let atoms = || {
        items
            .iter()
            .filter_map(|it| match it {
                Item::Atom(a) => Some(a),
                Item::Lit(_)
                | Item::AlignStart(_)
                | Item::IfStart(_)
                | Item::Then
                | Item::Else
                | Item::End => None,
            })
            .chain(sorts.iter().map(|s| &s.atom))
    };
    let needs_data = atoms().any(|a| {
        matches!(
            a.field,
            Field::Person(..)
                | Field::Contents(_)
                | Field::Tree(_)
                | Field::Parent(_)
                | Field::NumParent
                | Field::TargetName
                | Field::TargetType
                | Field::TagName
                | Field::Raw(_)
                | Field::Signature(_)
        )
    });
    let needs_peel = atoms().any(|a| a.deref);
    let needs_short = atoms().any(|a| matches!(a.field, Field::RefName(NameMod::Short)));
    let needs_symref_short = atoms().any(|a| matches!(a.field, Field::SymRef(NameMod::Short)));

    let head_name = repo.head_name()?.map(|n| n.as_bstr().to_vec());

    // Materialise every ref name first: the iterator holds the packed-refs
    // buffer, which would block the per-ref object lookups below.
    let mut names: Vec<Vec<u8>> = Vec::new();
    for r in repo.references()?.all()? {
        let r = r.map_err(|e| anyhow!("{e}"))?;
        names.push(r.name().as_bstr().to_vec());
    }
    // `--include-root-refs` also lists HEAD and the pseudorefs in the git dir
    // that git's `is_root_ref` accepts. They live directly under `$GIT_DIR`, so
    // the loose scan there finds them; `sort_refs` re-orders everything by name.
    if include_root_refs {
        for entry in std::fs::read_dir(repo.git_dir())? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let file_name = entry.file_name();
            if let Some(name) = file_name.to_str() {
                if is_root_ref(name.as_bytes()) {
                    names.push(name.as_bytes().to_vec());
                }
            }
        }
    }
    // The `:short` disambiguation rules test candidate names against every ref.
    let all_names: HashSet<Vec<u8>> = names.iter().cloned().collect();

    let filters_active = filters.active();
    let mut refs: Vec<RefInfo> = Vec::new();
    for refname in names {
        // `--start-after` seeks inside the `refs/` iteration, so a marker that
        // does not name a ref under `refs/` has no effect at all.
        if let Some(marker) = &start_after {
            if marker.starts_with("refs/") && refname.as_slice() <= marker.as_bytes() {
                continue;
            }
        }
        if !patterns.is_empty()
            && !patterns
                .iter()
                .any(|p| pattern_matches(p, &refname, ignore_case))
        {
            continue;
        }
        if excludes
            .iter()
            .any(|p| pattern_matches(p, &refname, ignore_case))
        {
            continue;
        }

        let name_str = refname
            .to_str()
            .map_err(|_| anyhow!("ref name is not valid utf-8: {:?}", refname.as_bstr()))?;
        let mut reference = repo.find_reference(name_str)?;
        let symref = reference
            .target()
            .try_name()
            .map(|n| n.as_bstr().to_vec())
            .unwrap_or_default();
        // `do_for_each_ref()` drops a ref that does not resolve, so a symbolic
        // ref whose target is gone — `refs/remotes/<remote>/HEAD` still naming a
        // renamed default branch — is simply not among the refs iterated. It is
        // not an error, and listing every other ref is not optional.
        let Ok(id) = reference.follow_to_object().map(|id| id.detach()) else {
            continue;
        };

        // The chain of tag targets, so `--points-at`, the reachability filters
        // and `*`-atoms agree with git. Skipped entirely when nothing needs it,
        // as peeling reads objects.
        let chain = if points_at.is_some() || needs_peel || filters_active {
            peel_chain(&repo, id)?
        } else {
            Vec::new()
        };
        if let Some(target) = points_at {
            if id != target && !chain.contains(&target) {
                continue;
            }
        }
        if filters_active && !passes_filters(&repo, &filters, *chain.last().unwrap_or(&id))? {
            continue;
        }

        let obj = load(&repo, id, needs_data)?;
        let peeled = match (needs_peel, obj.kind, chain.last()) {
            (true, Kind::Tag, Some(&last)) => Some(load(&repo, last, needs_data)?),
            _ => None,
        };
        let short = if needs_short {
            short_name(&repo, &refname, &all_names)
        } else {
            Vec::new()
        };
        let symref_short = if needs_symref_short && !symref.is_empty() {
            short_name(&repo, &symref, &all_names)
        } else {
            Vec::new()
        };

        // git's `REF_ISPACKED`. `refs_resolve_ref_unsafe` accumulates flags along
        // the whole symref chain, so the bit reflects where the object id
        // finally lives: a loose symref pointing at a packed ref reports
        // `symref,packed`.
        let packed = is_packed(&repo, name_str);

        refs.push(RefInfo {
            is_head: head_name.as_deref() == Some(refname.as_slice()),
            head_desc: None,
            refname,
            short,
            symref,
            symref_short,
            obj,
            peeled,
            packed,
            is_base: Vec::new(),
        });
    }

    // `filter_ahead_behind()` (ref-filter.c:3187), which `cmd_for_each_ref()` runs
    // between `filter_refs()` and the sort:
    //
    // ```c
    // if (!array->nr)
    //         return;
    // for (size_t i = bases_nr = 0; i < used_atom_cnt; i++)
    //         if (used_atom[i].atom_type == ATOM_AHEADBEHIND)
    //                 bases_nr++;
    // if (!bases_nr)
    //         return;
    // …
    // for (size_t i = 0; i < array->nr; i++) {
    //         const char *name = array->items[i]->refname;
    //         commits[commits_nr] = lookup_commit_reference_by_name(name);
    //         if (!commits[commits_nr])
    //                 continue;
    //         …
    // }
    // ```
    //
    // `lookup_commit_reference_by_name()` ends in `lookup_commit_reference_gently(r,
    // &oid, 0)` — quiet is *0* — so a ref that does not peel to a commit prints
    // `error: object %s is a %s, not a %s` right here. Three consequences the lazy
    // per-ref peel in the formatter does not have:
    //
    // * the line appears for a ref whose own `%(ahead-behind:…)` operand is
    //   perfectly good, purely because that ref is in the array;
    // * every line is emitted before the first output line, in array (pre-sort)
    //   order, not interleaved with the formatted output;
    // * `--count` is applied later, in `print_formatted_ref_array()`, so refs
    //   past the limit are still walked and still report.
    //
    // A `--sort=ahead-behind:<x>` key is a `used_atom` as much as a format atom is,
    // which is why the pass can fire for a format that names no such atom at all.
    if !refs.is_empty() && atoms().any(|a| matches!(a.field, Field::AheadBehind(_))) {
        for info in &refs {
            // The array item's id is what `repo_get_oid_committish()` answers for
            // the item's refname: `get_oid_basic()` resolves a full refname
            // through the ref store and does no peeling of its own.
            let found = crate::objname::lookup_commit_reference(&repo, info.obj.id);
            if let Some(note) = found.type_error() {
                eprintln!("error: {note}");
            }
        }
    }

    // `filter_is_base(the_repository, &array)` (ref-filter.c:3440) runs on the
    // filtered array *before* `ref_array_sort`, so the choice is made over the
    // refs the filters kept, in their pre-sort order, and `--count` cannot hide
    // a candidate from it.
    let is_base_atoms: Vec<(String, ObjectId)> = atoms()
        .filter_map(|a| match &a.field {
            Field::IsBase(name, tip) => Some((name.clone(), *tip)),
            _ => None,
        })
        .collect();
    let mut refs = refs;
    filter_is_base(&repo, &mut refs, &is_base_atoms);

    let ctx = RenderCtx {
        repo: &repo,
        worktrees: std::cell::OnceCell::new(),
    };

    let mut refs = sort_refs(&ctx, refs, &sorts, ignore_case, &prereleases)?;
    if let Some(n) = count {
        refs.truncate(n);
    }

    let mut out: Vec<u8> = Vec::new();
    for info in &refs {
        let line = match format_ref(&ctx, &items, info, quote_style, color_reset_at_eol)? {
            Ok(line) => line,
            // Every stack error git raises while formatting reaches `die()`.
            Err(msg) => return Ok(fatal(&msg)),
        };
        if omit_empty && line.is_empty() {
            continue;
        }
        out.extend_from_slice(&line);
        out.push(b'\n');
    }

    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// Repository-wide state the renderers share, built at most once per run.
pub(super) struct RenderCtx<'a> {
    pub(super) repo: &'a gix::Repository,
    /// git's `ref_to_worktree_map`, lazily initialised exactly as
    /// `lazy_init_worktree_map` does.
    pub(super) worktrees: std::cell::OnceCell<std::collections::HashMap<gix::bstr::BString, String>>,
}

impl RenderCtx<'_> {
    fn worktrees(&self) -> &std::collections::HashMap<gix::bstr::BString, String> {
        self.worktrees
            .get_or_init(|| super::branch::worktree_map(self.repo))
    }
}

/// What closing a formatting-stack frame does — git's `at_end` handler slot.
enum AtEnd {
    /// The base frame, which `%(end)` may not close.
    None,
    Align(AlignSpec),
    /// An index into the run's `%(if)` state list; the `%(then)` and `%(else)`
    /// frames of one conditional share it.
    If(usize),
}

/// The mutable half of one `%(if)…%(end)` conditional (git's `struct if_then_else`).
struct IfState {
    cmp: Cmp,
    then_seen: bool,
    else_seen: bool,
    satisfied: bool,
}

/// One formatting-stack frame (git's `struct ref_formatting_stack`).
struct Frame {
    output: Vec<u8>,
    at_end: AtEnd,
}

/// git's `format_ref_array_item`: run the item stream over one ref.
///
/// `Ok(Err(msg))` is a stack error — a `%(then)` with no `%(if)`, an unbalanced
/// `%(end)` — which git turns into `die()`.
pub(super) fn format_ref(
    ctx: &RenderCtx<'_>,
    items: &[Item],
    info: &RefInfo,
    quote_style: QuoteStyle,
    color_reset_at_eol: bool,
) -> Result<std::result::Result<Vec<u8>, String>> {
    macro_rules! stack_err {
        ($($arg:tt)*) => {
            return Ok(Err(format!($($arg)*)))
        };
    }

    let mut stack: Vec<Frame> = vec![Frame {
        output: Vec::new(),
        at_end: AtEnd::None,
    }];
    let mut ifs: Vec<IfState> = Vec::new();

    // git's `append_atom`: an atom is quoted only while the base frame is the
    // only one on the stack. Inside a container the raw bytes accumulate and the
    // whole frame is quoted when it closes.
    let append_value = |stack: &mut Vec<Frame>, value: Vec<u8>| {
        let quoted = if stack.len() == 1 {
            quote(&value, quote_style)
        } else {
            value
        };
        stack
            .last_mut()
            .expect("the base frame is never popped")
            .output
            .extend_from_slice(&quoted);
    };

    for item in items {
        match item {
            // `append_literal` writes straight into the current frame: literal
            // text is never quoted.
            Item::Lit(bytes) => stack
                .last_mut()
                .expect("base frame")
                .output
                .extend_from_slice(bytes),
            Item::Atom(atom) => {
                let value = render(ctx, atom, info)?;
                append_value(&mut stack, value);
            }
            Item::AlignStart(spec) => stack.push(Frame {
                output: Vec::new(),
                at_end: AtEnd::Align(spec.clone()),
            }),
            Item::IfStart(cmp) => {
                ifs.push(IfState {
                    cmp: cmp.clone(),
                    then_seen: false,
                    else_seen: false,
                    satisfied: false,
                });
                stack.push(Frame {
                    output: Vec::new(),
                    at_end: AtEnd::If(ifs.len() - 1),
                });
            }
            Item::Then => {
                let cur = stack.last_mut().expect("base frame");
                let AtEnd::If(idx) = cur.at_end else {
                    stack_err!("format: %(then) atom used without a %(if) atom");
                };
                let st = &mut ifs[idx];
                if st.then_seen {
                    stack_err!("format: %(then) atom used more than once");
                }
                if st.else_seen {
                    stack_err!("format: %(then) atom used after %(else)");
                }
                st.then_seen = true;
                // The condition is whatever the `%(if)` frame accumulated.
                st.satisfied = match &st.cmp {
                    Cmp::Equal(s) => s.as_bytes() == cur.output.as_slice(),
                    Cmp::Unequal(s) => s.as_bytes() != cur.output.as_slice(),
                    // git's `is_empty`, i.e. C `isspace` — which counts the
                    // vertical tab that Rust's `is_ascii_whitespace` omits.
                    Cmp::None => !cur
                        .output
                        .iter()
                        .all(|b| b.is_ascii_whitespace() || *b == 0x0b),
                };
                cur.output.clear();
            }
            Item::Else => {
                let AtEnd::If(idx) = stack.last().expect("base frame").at_end else {
                    stack_err!("format: %(else) atom used without a %(if) atom");
                };
                if !ifs[idx].then_seen {
                    stack_err!("format: %(else) atom used without a %(then) atom");
                }
                if ifs[idx].else_seen {
                    stack_err!("format: %(else) atom used more than once");
                }
                ifs[idx].else_seen = true;
                // The `%(else)` branch collects into its own frame, sharing the
                // conditional's state with the `%(then)` frame beneath it.
                stack.push(Frame {
                    output: Vec::new(),
                    at_end: AtEnd::If(idx),
                });
            }
            Item::End => {
                match stack.last().expect("base frame").at_end {
                    AtEnd::None => {
                        stack_err!("format: %(end) atom used without corresponding atom")
                    }
                    AtEnd::Align(_) => {
                        let cur = stack.last_mut().expect("base frame");
                        let AtEnd::Align(spec) = &cur.at_end else {
                            unreachable!("matched above")
                        };
                        cur.output = pad_align(&cur.output, spec);
                    }
                    AtEnd::If(idx) => {
                        if !ifs[idx].then_seen {
                            stack_err!("format: %(if) atom used without a %(then) atom");
                        }
                        if ifs[idx].else_seen {
                            // Two frames are open: `%(then)`'s and `%(else)`'s.
                            // Exactly one survives.
                            let else_branch = stack.pop().expect("the %(else) frame");
                            let then_frame = stack.last_mut().expect("the %(then) frame");
                            if !ifs[idx].satisfied {
                                then_frame.output = else_branch.output;
                            }
                        } else if !ifs[idx].satisfied {
                            stack.last_mut().expect("base frame").output.clear();
                        }
                    }
                }
                // Quote the closed frame when it sat directly on the base frame;
                // a nested one is quoted later, as part of its parent.
                let cur = stack.pop().expect("a container frame was open");
                let content = if stack.len() == 1 {
                    quote(&cur.output, quote_style)
                } else {
                    cur.output
                };
                stack
                    .last_mut()
                    .expect("base frame")
                    .output
                    .extend_from_slice(&content);
            }
        }
    }

    if color_reset_at_eol {
        append_value(&mut stack, b"\x1b[m".to_vec());
    }
    if stack.len() > 1 {
        stack_err!("format: %(end) atom missing");
    }
    Ok(Ok(stack.pop().expect("base frame").output))
}

/// Apply one of the four `--shell`/`--perl`/`--python`/`--tcl` quoting styles.
fn quote(value: &[u8], style: QuoteStyle) -> Vec<u8> {
    match style {
        QuoteStyle::None => value.to_vec(),
        QuoteStyle::Shell => sq_quote(value),
        QuoteStyle::Perl => perl_quote(value),
        QuoteStyle::Python => python_quote(value),
        QuoteStyle::Tcl => tcl_quote(value),
    }
}

/// git's `OPT_INTEGER` operand: a decimal count with an optional `k`/`m`/`g`
/// scaling suffix.
fn parse_count(v: &str) -> Option<i64> {
    crate::optint::integer(&crate::optint::long_opt("count"), v).ok()
}

/// Whether `tip` survives the reachability filters.
///
/// A ref that does not peel to a commit is dropped by every one of them, as git
/// does when `lookup_commit_reference_gently` comes back empty.
pub(super) fn passes_filters(repo: &gix::Repository, filters: &Filters, tip: ObjectId) -> Result<bool> {
    if repo.find_header(tip)?.kind() != Kind::Commit {
        return Ok(false);
    }
    // `--contains=<c>`: the ref must be a descendant of `<c>`.
    if !filters.contains.is_empty() {
        let mut any = false;
        for &c in &filters.contains {
            if is_ancestor(repo, c, tip)? {
                any = true;
                break;
            }
        }
        if !any {
            return Ok(false);
        }
    }
    for &c in &filters.no_contains {
        if is_ancestor(repo, c, tip)? {
            return Ok(false);
        }
    }
    // `--merged=<m>`: the ref must be reachable from `<m>`.
    if !filters.merged.is_empty() {
        let mut any = false;
        for &m in &filters.merged {
            if is_ancestor(repo, tip, m)? {
                any = true;
                break;
            }
        }
        if !any {
            return Ok(false);
        }
    }
    for &m in &filters.no_merged {
        if is_ancestor(repo, tip, m)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// git's `repo_in_merge_bases`: whether `ancestor` is reachable from `descendant`.
fn is_ancestor(repo: &gix::Repository, ancestor: ObjectId, descendant: ObjectId) -> Result<bool> {
    if ancestor == descendant {
        return Ok(true);
    }
    let bases = repo.merge_bases_many(descendant, &[ancestor])?;
    Ok(bases.into_iter().any(|b| b.detach() == ancestor))
}

/// Split a format string into literal runs and atoms, expanding `%%` and `%xx`.
///
/// A `%` that starts neither `%%`, `%(` nor a two-digit hex escape is literal,
/// as it is in git.
///
/// Returns the item stream plus git's `need_color_reset_at_eol`: a format whose
/// last `%(color:…)` names anything but `reset` gets an implicit reset appended
/// to every line, but only while colour is actually on.
pub(super) fn parse_format(
    fmt: &[u8],
    ctx: &AtomCtx<'_>,
) -> std::result::Result<(Vec<Item>, bool), AtomError> {
    let mut items = Vec::new();
    let mut lit: Vec<u8> = Vec::new();
    let mut i = 0;
    let mut need_color_reset = false;

    while i < fmt.len() {
        if fmt[i] != b'%' {
            lit.push(fmt[i]);
            i += 1;
            continue;
        }
        match fmt.get(i + 1) {
            Some(b'%') => {
                lit.push(b'%');
                i += 2;
            }
            Some(b'(') => {
                let start = i + 2;
                let Some(offset) = fmt[start..].iter().position(|&b| b == b')') else {
                    return Err(usage_atom(format!(
                        "malformed format string {}",
                        fmt[i..].as_bstr()
                    )));
                };
                let end = start + offset;
                let spec = std::str::from_utf8(&fmt[start..end])
                    .map_err(|_| fatal_atom("format atom is not valid utf-8"))?;
                if !lit.is_empty() {
                    items.push(Item::Lit(std::mem::take(&mut lit)));
                }
                // The container atoms drive the formatting stack rather than
                // producing a value, so they are items in their own right.
                // Everything else is a normal atom. Balance is *not* checked
                // here: git only discovers it while formatting a ref, so a
                // format missing its `%(end)` is silently fine in a repository
                // with no refs to render.
                let (name, arg) = match spec.split_once(':') {
                    Some((n, a)) => (n, Some(a)),
                    None => (spec, None),
                };
                // `parse_ref_filter_atom()` nulls an empty sub-argument list
                // before it reaches any parser, so `%(align:)` is `%(align)` —
                // `expected format: %(align:<width>,<position>)`, not an
                // unrecognized-argument error (`ref-filter.c:1092-1101`).
                let arg = arg.filter(|a| !a.is_empty());
                match name {
                    "end" => items.push(Item::End),
                    "align" => items.push(Item::AlignStart(parse_align(arg)?)),
                    "if" => items.push(Item::IfStart(parse_if(arg)?)),
                    // `then` and `else` carry no parser in git's atom table, so
                    // a trailing `:arg` on them is silently ignored.
                    "then" => items.push(Item::Then),
                    "else" => items.push(Item::Else),
                    _ => {
                        items.push(Item::Atom(parse_atom(spec, ctx)?));
                        if name == "color" && !spec.starts_with('*') {
                            need_color_reset = arg != Some("reset");
                        }
                    }
                }
                i = end + 1;
            }
            _ => {
                let hex = fmt
                    .get(i + 1..i + 3)
                    .filter(|h| h.iter().all(u8::is_ascii_hexdigit));
                match hex {
                    Some(hex) => {
                        // Both bytes are ASCII hex digits, so neither conversion can fail.
                        let s = std::str::from_utf8(hex).expect("ascii");
                        lit.push(u8::from_str_radix(s, 16).expect("two hex digits"));
                        i += 3;
                    }
                    None => {
                        lit.push(b'%');
                        i += 1;
                    }
                }
            }
        }
    }
    if !lit.is_empty() {
        items.push(Item::Lit(lit));
    }
    Ok((items, need_color_reset && ctx.color_on))
}

/// git's `if_atom_parser`: a bare `%(if)`, or one of the two comparisons.
fn parse_if(arg: Option<&str>) -> std::result::Result<Cmp, AtomError> {
    Ok(match arg {
        None => Cmp::None,
        Some(a) => match a.strip_prefix("equals=") {
            Some(v) => Cmp::Equal(v.to_string()),
            None => match a.strip_prefix("notequals=") {
                Some(v) => Cmp::Unequal(v.to_string()),
                None => return Err(fatal_atom(format!("unrecognized %(if) argument: {a}"))),
            },
        },
    })
}

/// Parse `%(align:<opts>)` options: a width and an optional position, given
/// positionally (`25,left`) or by key (`width=25,position=left`), in any order.
fn parse_align(opts: Option<&str>) -> std::result::Result<AlignSpec, AtomError> {
    let missing = || fatal_atom("expected format: %(align:<width>,<position>)");
    let opts = opts.ok_or_else(missing)?;
    let mut width: Option<usize> = None;
    let mut position = AlignPos::Left;
    for tok in opts.split(',') {
        if let Some(w) = tok.strip_prefix("width=") {
            width = Some(w.parse().map_err(|_| missing())?);
        } else if let Some(p) = tok.strip_prefix("position=") {
            position = parse_align_pos(p)?;
        } else if let Ok(w) = tok.parse::<usize>() {
            width = Some(w);
        } else {
            position = parse_align_pos(tok)?;
        }
    }
    let width = width.ok_or_else(missing)?;
    Ok(AlignSpec { width, position })
}

fn parse_align_pos(p: &str) -> std::result::Result<AlignPos, AtomError> {
    match p {
        "left" => Ok(AlignPos::Left),
        "right" => Ok(AlignPos::Right),
        "middle" => Ok(AlignPos::Middle),
        other => Err(fatal_atom(format!("unrecognized %(align) argument: {other}"))),
    }
}

/// Pad `content` to `spec.width` display columns per the position; content at or
/// over the width is returned unchanged (git never truncates). Display width is
/// the char count — exact for the ASCII refnames this pads in practice.
fn pad_align(content: &[u8], spec: &AlignSpec) -> Vec<u8> {
    let cols = String::from_utf8_lossy(content).chars().count();
    if cols >= spec.width {
        return content.to_vec();
    }
    let pad = spec.width - cols;
    let (left, right) = match spec.position {
        AlignPos::Left => (0, pad),
        AlignPos::Right => (pad, 0),
        AlignPos::Middle => (pad / 2, pad - pad / 2),
    };
    let mut out = vec![b' '; left];
    out.extend_from_slice(content);
    out.extend(std::iter::repeat_n(b' ', right));
    out
}

/// Every atom name stock git accepts, so an unrecognised one can be told apart
/// from one this module simply has not built.
const KNOWN_ATOMS: &[&str] = &[
    "refname",
    "objecttype",
    "objectsize",
    "objectname",
    "deltabase",
    "tree",
    "parent",
    "numparent",
    "object",
    "type",
    "tag",
    "author",
    "authorname",
    "authoremail",
    "authordate",
    "committer",
    "committername",
    "committeremail",
    "committerdate",
    "tagger",
    "taggername",
    "taggeremail",
    "taggerdate",
    "creator",
    "creatordate",
    "describe",
    "subject",
    "body",
    "trailers",
    "contents",
    "signature",
    "raw",
    "upstream",
    "push",
    "symref",
    "flag",
    "HEAD",
    "color",
    "worktreepath",
    "align",
    "end",
    "if",
    "then",
    "else",
    "rest",
    "ahead-behind",
    "is-base",
];

/// Parse one atom body (the text between `%(` and `)`), also used for sort keys.
///
/// Every entry of `valid_atom[]` (ref-filter.c:947-993) is understood, with the
/// modifiers each atom's parser takes:
///
///   * `refname` / `symref`, with `:short`, `:lstrip=<n>`, `:rstrip=<n>`,
///     `:strip=<n>`;
///   * `objectname` / `tree` / `parent`, with `:short[=<n>]`; `numparent`,
///     `object`, `type`, `tag`, `objecttype`, `objectsize[:disk]`, `deltabase`,
///     `raw[:size]`;
///   * `author` / `committer` / `tagger` / `creator`, their `name[:mailmap]`,
///     `email` (a bit set of `trim`, `localpart`, `mailmap`) and `date` forms,
///     the last taking the whole `--date=` vocabulary through
///     [`crate::showdate::parse_date_format`];
///   * `subject[:sanitize]`, `body`, `contents[:subject|:body|:size|:signature|
///     :lines=<n>|:trailers[:<opts>]]`, `trailers[:<opts>]`;
///   * `signature[:signer|:grade|:key|:fingerprint|:primarykeyfingerprint|
///     :trustlevel]`;
///   * `upstream` / `push` with the remote-ref option list, `flag`, `HEAD`,
///     `color:<spec>`, `worktreepath`, `describe[:<opts>]`,
///     `ahead-behind:<committish>`, `is-base:<committish>`;
///   * the container atoms `align`, `if`, `then`, `else`, `end`, and `rest`,
///     which this command refuses the way git refuses it.
///
/// A leading `*` evaluates the atom against the object a tag peels to.
pub(super) fn parse_atom(spec: &str, ctx: &AtomCtx<'_>) -> std::result::Result<Atom, AtomError> {
    let (body, deref) = match spec.strip_prefix('*') {
        Some(rest) => (rest, true),
        None => (spec, false),
    };
    // ```c
    // if (ep <= sp)
    //         return strbuf_addf_ret(err, -1, _("malformed field name: %.*s"),
    //                                (int)(ep-atom), atom);
    // ```
    // (`ref-filter.c:1041-1043`) — an empty name, with or without the deref `*`,
    // is *malformed*, which is a different message from a name that simply is not
    // in the table.
    if body.is_empty() {
        return Err(fatal_atom(format!("malformed field name: {spec}")));
    }
    let (name, m) = match body.split_once(':') {
        Some((n, m)) => (n, Some(m)),
        None => (body, None),
    };
    // ```c
    // if (arg) {
    //         arg = used_atom[at].name + (arg - atom) + 1;
    //         if (!*arg) {
    //                 /*
    //                  * Treat empty sub-arguments list as NULL (i.e.,
    //                  * "%(atom:)" is equivalent to "%(atom)").
    //                  */
    //                 arg = NULL;
    //         }
    // }
    // ```
    // (`ref-filter.c:1092-1101`). Every atom parser therefore sees `%(refname:)` as
    // `%(refname)`, including the ones whose no-argument case is itself a fatal —
    // `%(align:)` is `expected format: %(align:<width>,<position>)`, not an
    // unrecognized-argument error.
    let m = m.filter(|a| !a.is_empty());
    // `err_bad_arg()` cuts the atom at its first colon but keeps the deref `*`:
    // `%(*parent:bogus)` is `unrecognized %(*parent) argument: bogus`
    // (`ref-filter.c:272-278`).
    let dname = if deref {
        format!("*{name}")
    } else {
        name.to_string()
    };

    // Reject a modifier on an atom that takes none, naming the offending atom.
    let bare = |m: Option<&str>| -> std::result::Result<(), AtomError> {
        match m {
            None => Ok(()),
            Some(m) => Err(fatal_atom(format!("unrecognized %({dname}) argument: {m}"))),
        }
    };

    let field = match name {
        "refname" | "symref" => {
            let m = parse_name_mod(name, m)?;
            if name == "refname" {
                Field::RefName(m)
            } else {
                Field::SymRef(m)
            }
        }
        // `%(objectname)`, `%(tree)` and `%(parent)` share `oid_atom_parser`.
        "objectname" => Field::ObjectName(parse_oid_mod(spec, &dname, m)?),
        "objecttype" => {
            bare(m)?;
            Field::ObjectType
        }
        "objectsize" => match m {
            None => Field::ObjectSize,
            Some("disk") => Field::ObjectSizeDisk,
            Some(m) => {
                return Err(fatal_atom(format!(
                    "unrecognized %(objectsize) argument: {m}"
                )))
            }
        },
        // git's `raw_atom_parser`. `%(raw)` is byte-exact object data, which the
        // three quoting styles that cannot represent NUL reject outright.
        "raw" => {
            let size = match m {
                None => false,
                Some("size") => true,
                Some(m) => return Err(fatal_atom(format!("unrecognized %(raw) argument: {m}"))),
            };
            if !size
                && matches!(
                    ctx.quote_style,
                    QuoteStyle::Python | QuoteStyle::Shell | QuoteStyle::Tcl
                )
            {
                return Err(fatal_atom(format!(
                    "--format={spec} cannot be used with --python, --shell, --tcl"
                )));
            }
            Field::Raw(size)
        }
        "upstream" | "push" => {
            let rr = parse_remote_ref(name, m)?;
            if name == "upstream" {
                Field::Upstream(rr)
            } else {
                Field::Push(rr)
            }
        }
        "flag" => {
            bare(m)?;
            Field::Flag
        }
        "worktreepath" => {
            bare(m)?;
            Field::WorktreePath
        }
        "describe" => Field::Describe(parse_describe(m)?),
        "ahead-behind" => {
            let Some(arg) = m else {
                return Err(fatal_atom(
                    "expected format: %(ahead-behind:<committish>)",
                ));
            };
            // `lookup_commit_reference_by_name()`: `get_oid_committish()` — whose
            // full-hex branch answers without the odb — then the same non-quiet
            // peel the filter options use, then `die("failed to find '%s'")`.
            let base = ctx
                .repo
                .and_then(|r| {
                    let id = crate::objname::resolve(r, arg)?;
                    let found = crate::objname::lookup_commit_reference(r, id);
                    if let Some(note) = found.type_error() {
                        eprintln!("error: {note}");
                    }
                    match found {
                        crate::objname::CommitRef::Commit(id) => Some(id),
                        _ => None,
                    }
                })
                .ok_or_else(|| fatal_atom(format!("failed to find '{arg}'")))?;
            Field::AheadBehind(base)
        }
        // `deltabase_atom_parser` (ref-filter.c:504-515) rejects an argument
        // outright and otherwise asks `oid_object_info_extended()` for
        // `OBJECT_INFO_DELTA_BASE`.
        "deltabase" => {
            bare(m).map_err(|_| fatal_atom("%(deltabase) does not take arguments"))?;
            Field::DeltaBase
        }
        // `is_base_atom_parser` (ref-filter.c:913-926): the operand is mandatory
        // and is peeled by `lookup_commit_reference_by_name()`, whose failure is
        // a `die()` rather than an "unknown atom". The answer itself is not a
        // per-ref rendering at all — `filter_is_base()` picks one ref out of the
        // whole array before sorting, which [`filter_is_base`] does here.
        "is-base" => {
            let Some(arg) = m else {
                return Err(fatal_atom("expected format: %(is-base:<committish>)"));
            };
            let resolved = ctx.repo.and_then(|r| {
                let id = crate::objname::resolve(r, arg)?;
                match crate::objname::lookup_commit_reference(r, id) {
                    crate::objname::CommitRef::Commit(id) => Some(id),
                    _ => None,
                }
            });
            let Some(tip) = resolved else {
                return Err(fatal_atom(format!("failed to find '{arg}'")));
            };
            Field::IsBase(arg.to_string(), tip)
        }
        // `verify_ref_format`'s `reject_atom`: `for-each-ref` has no "rest of the
        // line" to report, so the atom parses and is then refused.
        "rest" => {
            bare(m)
                .map_err(|_| fatal_atom("%(rest) does not take arguments"))?;
            return Err(fatal_atom(format!("this command reject atom %({spec})")));
        }
        "HEAD" => {
            bare(m)?;
            Field::Head
        }
        "color" => match m {
            None => return Err(fatal_atom("expected format: %(color:<color>)")),
            Some(spec) => match parse_color(spec) {
                Some(escape) => Field::Color(if ctx.color_on { escape } else { Vec::new() }),
                None => return Err(fatal_atom(format!("invalid color value: {spec}"))),
            },
        },
        "author" | "committer" | "tagger" | "creator" => {
            bare(m)?;
            Field::Person(who(name), PersonPart::Full)
        }
        // `person_name_atom_parser` (ref-filter.c:755-767): `N_RAW` or `N_MAILMAP`.
        "authorname" | "committername" | "taggername" => {
            let mailmap = match m {
                None => false,
                Some("mailmap") => true,
                Some(m) => return Err(fatal_atom(format!("unrecognized %({dname}) argument: {m}"))),
            };
            Field::Person(who(name.trim_end_matches("name")), PersonPart::Name { mailmap })
        }
        // `person_email_atom_parser` (ref-filter.c:781-802): a comma-separated
        // list whose options are *bits*, not alternatives, so `trim,mailmap`
        // means both. `email_atom_option_parser` matches by `skip_prefix`, so the
        // offending text a typo reports is the tail from the failure onwards, not
        // the whole argument: `%(authoremail:trim,bogus)` names `bogus`, and
        // `%(authoremail:trimx)` names `x`.
        "authoremail" | "committeremail" | "taggeremail" => {
            let opt = parse_email_opts(&dname, m)?;
            Field::Person(who(name.trim_end_matches("email")), PersonPart::Email(opt))
        }
        // `grab_date` (ref-filter.c:1677-1720) reads the format straight off the
        // atom name and hands it to `parse_date_format()`, so a date atom takes
        // the whole `--date=` vocabulary, `format:<strftime>` included, and both
        // of that function's `die()`s reach the user verbatim.
        "authordate" | "committerdate" | "taggerdate" | "creatordate" => {
            let mode = match m {
                None => None,
                Some(spec) => Some(
                    crate::showdate::parse_date_format(spec)
                        .map_err(|e| fatal_atom(e.to_string()))?,
                ),
            };
            Field::Person(who(name.trim_end_matches("date")), PersonPart::Date(mode))
        }
        // `subject_atom_parser` (ref-filter.c:527-538).
        "subject" => Field::Contents(match m {
            None => ContentPart::Subject,
            Some("sanitize") => ContentPart::SubjectSanitize,
            Some(m) => return Err(fatal_atom(format!("unrecognized %(subject) argument: {m}"))),
        }),
        // `body_atom_parser` (ref-filter.c:517-525) sets `C_BODY_DEP`, which is
        // *not* `%(contents:body)`: it keeps a trailing signature block.
        "body" => {
            bare(m)?;
            Field::Contents(ContentPart::BodyDep)
        }
        // `trailers_atom_parser` (ref-filter.c:570-610).
        "trailers" => Field::Contents(ContentPart::Trailers(parse_trailer_opts(m)?)),
        // `contents_atom_parser` (ref-filter.c:612-641).
        "contents" => Field::Contents(match m {
            None => ContentPart::Bare,
            Some("subject") => ContentPart::Subject,
            Some("body") => ContentPart::Body,
            Some("size") => ContentPart::Size,
            Some("signature") => ContentPart::Sig,
            // Forwarded to `trailers_atom_parser`, splitting `trailers:<args>`
            // at the first colon. `%(contents:trailers:)` reaches that parser
            // with an *empty* argument rather than none, which parses the same.
            Some("trailers") => ContentPart::Trailers(parse_trailer_opts(None)?),
            Some(m) if m.starts_with("trailers:") => {
                ContentPart::Trailers(parse_trailer_opts(Some(&m["trailers:".len()..]))?)
            }
            Some(m) if m.starts_with("lines=") => {
                let arg = &m["lines=".len()..];
                let n = strtoul_ui(arg).ok_or_else(|| {
                    fatal_atom(format!("positive value expected contents:lines={arg}"))
                })?;
                ContentPart::Lines(n)
            }
            Some(m) => {
                return Err(fatal_atom(format!(
                    "unrecognized %(contents) argument: {m}"
                )))
            }
        }),
        // `signature_atom_parser`.
        "signature" => Field::Signature(match m {
            None => SigOption::Bare,
            Some("signer") => SigOption::Signer,
            Some("grade") => SigOption::Grade,
            Some("key") => SigOption::Key,
            Some("fingerprint") => SigOption::Fingerprint,
            Some("primarykeyfingerprint") => SigOption::PrimaryKeyFingerprint,
            Some("trustlevel") => SigOption::TrustLevel,
            Some(m) => {
                return Err(fatal_atom(format!(
                    "unrecognized %(signature) argument: {m}"
                )))
            }
        }),
        // `%(tree)` / `%(parent)` share `%(objectname)`'s `oid_atom_parser`, so
        // they take the same `:short` / `:short=<n>` modifiers.
        "tree" => Field::Tree(parse_oid_mod(spec, &dname, m)?),
        "parent" => Field::Parent(parse_oid_mod(spec, &dname, m)?),
        // These four carry no `parser` in git's atom table, so git silently
        // ignores any `:arg` on them (`%(type:foo)` == `%(type)`).
        "numparent" => Field::NumParent,
        "object" => Field::TargetName,
        "type" => Field::TargetType,
        "tag" => Field::TagName,
        // Unreachable for git 2.55's `valid_atom[]`, every entry of which is
        // handled above — kept so that an atom a newer git adds to [`KNOWN_ATOMS`]
        // is an honest gap in this port's own voice rather than the "unknown
        // field name" git reserves for a typo.
        n if KNOWN_ATOMS.contains(&n) => {
            return Err(unported_atom(format!("%({n}) is not ported")))
        }
        // `unknown field name: %.*s` measures `ep - atom`, i.e. from the start of
        // the atom *including* the deref `*`, so `%(*bogus)` keeps it.
        _ => return Err(fatal_atom(format!("unknown field name: {spec}"))),
    };

    if deref && matches!(field, Field::RefName(_) | Field::SymRef(_) | Field::Head) {
        return Err(fatal_atom(format!("`*` has no meaning on %({dname})")));
    }
    Ok(Atom { deref, field })
}

/// git's `remote_ref_atom_parser`: a comma-separated option list in which the
/// last recognised rendering wins, `nobracket` is an independent flag, and any
/// unrecognised token falls back to being read as a `%(refname)` modifier
/// applied to the *whole* argument (which is where a typo is reported).
fn parse_remote_ref(name: &str, arg: Option<&str>) -> std::result::Result<RemoteRef, AtomError> {
    let Some(arg) = arg else {
        return Ok(RemoteRef {
            option: RrOption::Ref(NameMod::Full),
            nobracket: false,
            push_remote: false,
        });
    };
    let mut rr = RemoteRef {
        option: RrOption::Ref(NameMod::Full),
        nobracket: false,
        push_remote: false,
    };
    for token in arg.split(',') {
        match token {
            "track" => rr.option = RrOption::Track,
            "trackshort" => rr.option = RrOption::TrackShort,
            "nobracket" => rr.nobracket = true,
            "remotename" => {
                rr.option = RrOption::RemoteName;
                rr.push_remote = true;
            }
            "remoteref" => {
                rr.option = RrOption::RemoteRefName;
                rr.push_remote = true;
            }
            // git re-parses the *entire* argument here, not just this token.
            _ => rr.option = RrOption::Ref(parse_name_mod(name, Some(arg))?),
        }
    }
    Ok(rr)
}

/// git's `match_atom_arg_value`: peel one `key[=value]` off the head of a
/// comma-separated option list. `Some((value, rest))` on an exact key match,
/// where `value` is `None` for a bare key; `None` when the list does not start
/// with `key` followed by `=`, `,` or the end.
fn match_arg_value<'a>(to_parse: &'a str, key: &str) -> Option<(Option<&'a str>, &'a str)> {
    let atom = to_parse.strip_prefix(key)?;
    let (value, rest) = match atom.as_bytes().first() {
        Some(b'=') => {
            let v = &atom[1..];
            match v.find(',') {
                Some(i) => (Some(&v[..i]), &v[i + 1..]),
                None => (Some(v), ""),
            }
        }
        Some(b',') => (None, &atom[1..]),
        None => (None, ""),
        // The key is only a prefix of a longer one ("tagsfoo").
        Some(_) => return None,
    };
    Some((value, rest))
}

/// git's `git_parse_maybe_bool` restricted to the spellings the atom parsers see.
fn maybe_bool(v: &str) -> Option<bool> {
    match v {
        "1" | "yes" | "true" => Some(true),
        "0" | "no" | "false" => Some(false),
        _ => None,
    }
}

/// git's `describe_atom_parser`: translate `%(describe:<opts>)` into the
/// argument vector handed to the `describe` subprocess. Each iteration retries
/// the whole option list against every known key, so an unrecognised key is
/// reported with the entire unparsed remainder, exactly as `err_bad_arg` does.
fn parse_describe(arg: Option<&str>) -> std::result::Result<Vec<String>, AtomError> {
    let mut args: Vec<String> = Vec::new();
    let mut rest = arg.unwrap_or("");
    while !rest.is_empty() {
        let bad = rest;
        if let Some((v, next)) = match_arg_value(rest, "tags") {
            let on = match v {
                None => true,
                Some(v) => match maybe_bool(v) {
                    Some(b) => b,
                    // An unparseable boolean makes the key not match at all.
                    None => return Err(fatal_atom(format!("unrecognized %(describe) argument: {bad}"))),
                },
            };
            args.push(if on { "--tags".into() } else { "--no-tags".into() });
            rest = next;
            continue;
        }
        if let Some((v, next)) = match_arg_value(rest, "abbrev") {
            let v = v.unwrap_or("");
            if v.is_empty() {
                return Err(fatal_atom("argument expected for describe:abbrev"));
            }
            match v.parse::<i64>() {
                Ok(n) if n >= 0 => args.push(format!("--abbrev={v}")),
                Ok(_) => {
                    return Err(fatal_atom(format!(
                        "positive value expected describe:abbrev={v}"
                    )))
                }
                Err(_) => {
                    return Err(fatal_atom(format!("cannot fully parse describe:abbrev={v}")))
                }
            }
            rest = next;
            continue;
        }
        if let Some((v, next)) = match_arg_value(rest, "match") {
            let v = v.unwrap_or("");
            if v.is_empty() {
                return Err(fatal_atom("value expected describe:match="));
            }
            args.push(format!("--match={v}"));
            rest = next;
            continue;
        }
        if let Some((v, next)) = match_arg_value(rest, "exclude") {
            let v = v.unwrap_or("");
            if v.is_empty() {
                return Err(fatal_atom("value expected describe:exclude="));
            }
            args.push(format!("--exclude={v}"));
            rest = next;
            continue;
        }
        return Err(fatal_atom(format!(
            "unrecognized %(describe) argument: {bad}"
        )));
    }
    Ok(args)
}

/// The `:short` / `:lstrip=` / `:rstrip=` family shared by `%(refname)` and
/// `%(symref)`.
fn parse_name_mod(name: &str, m: Option<&str>) -> std::result::Result<NameMod, AtomError> {
    Ok(match m {
        None => NameMod::Full,
        Some("short") => NameMod::Short,
        Some(m) => {
            let bad = || fatal_atom(format!("unrecognized %({name}) argument: {m}"));
            if let Some(n) = m
                .strip_prefix("lstrip=")
                .or_else(|| m.strip_prefix("strip="))
            {
                NameMod::LStrip(n.parse::<i64>().map_err(|_| bad())?)
            } else if let Some(n) = m.strip_prefix("rstrip=") {
                NameMod::RStrip(n.parse::<i64>().map_err(|_| bad())?)
            } else {
                return Err(bad());
            }
        }
    })
}

/// git's minimum abbreviation length (`minimum_abbrev`, default 4): `:short=<n>`
/// values below it are raised to it, as `oid_atom_parser` does.
const MINIMUM_ABBREV: usize = 4;

/// git's `oid_atom_parser`, shared by `%(objectname)`, `%(tree)` and
/// `%(parent)`: `:short` picks the configured abbreviation, `:short=<n>` a fixed
/// length (a positive integer, floored to `MINIMUM_ABBREV`).
fn parse_oid_mod(
    spec: &str,
    name: &str,
    m: Option<&str>,
) -> std::result::Result<NameLen, AtomError> {
    Ok(match m {
        None => NameLen::Full,
        Some("short") => NameLen::Auto,
        Some(m) => match m.strip_prefix("short=") {
            Some(n) => {
                let len = n
                    .parse::<usize>()
                    .ok()
                    .filter(|&v| v != 0)
                    .ok_or_else(|| {
                        // `_("positive value expected '%s' in %%(%s)"), arg, atom->name`
                        // — `atom->name` is the atom exactly as written, argument
                        // and deref `*` included.
                        fatal_atom(format!("positive value expected '{n}' in %({spec})"))
                    })?;
                NameLen::Fixed(len.max(MINIMUM_ABBREV))
            }
            None => {
                return Err(fatal_atom(format!("unrecognized %({name}) argument: {m}")))
            }
        },
    })
}

/// git's `color_parse`, reduced to the spellings `%(color:...)` actually sees:
/// `reset`, attribute words, colour names (with a `bright` prefix), 0-255
/// palette indices and `#rrggbb`, in git's "attributes, foreground, background"
/// order.
fn parse_color(spec: &str) -> Option<Vec<u8>> {
    if spec == "reset" {
        return Some(b"\x1b[m".to_vec());
    }
    let mut attrs: Vec<String> = Vec::new();
    let mut colors: Vec<String> = Vec::new();
    for token in spec.split_whitespace() {
        if let Some(code) = attribute_code(token) {
            attrs.push(code.to_string());
            continue;
        }
        if colors.len() >= 2 {
            return None;
        }
        let background = colors.len() == 1;
        match color_code(token, background) {
            // `normal` names "whatever the terminal already uses", which git
            // renders by emitting nothing for that slot.
            Some(None) => colors.push(String::new()),
            Some(Some(code)) => colors.push(code),
            None => return None,
        }
    }
    let codes: Vec<String> = attrs
        .into_iter()
        .chain(colors.into_iter().filter(|c| !c.is_empty()))
        .collect();
    if codes.is_empty() {
        return Some(Vec::new());
    }
    Some(format!("\x1b[{}m", codes.join(";")).into_bytes())
}

/// The SGR code for a git attribute word, if `token` is one.
fn attribute_code(token: &str) -> Option<&'static str> {
    Some(match token {
        "bold" => "1",
        "dim" => "2",
        "italic" => "3",
        "ul" | "underline" => "4",
        "blink" => "5",
        "reverse" => "7",
        "strike" => "9",
        "nobold" => "22",
        "nodim" => "22",
        "noitalic" => "23",
        "noul" | "nounderline" => "24",
        "noblink" => "25",
        "noreverse" => "27",
        "nostrike" => "29",
        _ => return None,
    })
}

/// The SGR code for one colour token. `Some(None)` is `normal`, which prints
/// nothing; `None` is a parse failure.
fn color_code(token: &str, background: bool) -> Option<Option<String>> {
    const NAMES: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    let base = if background { 40 } else { 30 };

    if token == "normal" {
        return Some(None);
    }
    if token == "default" {
        return Some(Some((base + 9).to_string()));
    }
    if let Some(rest) = token.strip_prefix("bright") {
        let idx = NAMES.iter().position(|n| *n == rest)?;
        return Some(Some((base + 60 + idx as i32).to_string()));
    }
    if let Some(idx) = NAMES.iter().position(|n| *n == token) {
        return Some(Some((base + idx as i32).to_string()));
    }
    if let Some(hex) = token.strip_prefix('#') {
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let c = |r: std::ops::Range<usize>| u8::from_str_radix(&hex[r], 16).expect("hex");
            return Some(Some(format!(
                "{};2;{};{};{}",
                base + 8,
                c(0..2),
                c(2..4),
                c(4..6)
            )));
        }
        return None;
    }
    match token.parse::<u16>() {
        Ok(n) if n <= 255 => Some(Some(format!("{};5;{n}", base + 8))),
        _ => None,
    }
}

/// Map a person atom's stem onto the header it reads.
fn who(stem: &str) -> Who {
    match stem {
        "author" => Who::Author,
        "committer" => Who::Committer,
        "tagger" => Who::Tagger,
        _ => Who::Creator,
    }
}

/// Read `id`'s header, and its full data when `with_data` is set.
pub(super) fn load(repo: &gix::Repository, id: ObjectId, with_data: bool) -> Result<ObjInfo> {
    let header = repo.find_header(id)?;
    let data = if with_data {
        Some(repo.find_object(id)?.data.clone())
    } else {
        None
    };
    Ok(ObjInfo {
        id,
        kind: header.kind(),
        size: header.size(),
        data,
    })
}

/// The chain of objects reached by dereferencing tags, starting *after* `id`.
///
/// Empty when `id` is not a tag; otherwise each element is one dereference
/// deeper, so the last entry is the fully peeled object.
pub(super) fn peel_chain(repo: &gix::Repository, id: ObjectId) -> Result<Vec<ObjectId>> {
    let mut chain = Vec::new();
    let mut current = id;
    loop {
        let object = repo.find_object(current)?;
        if object.kind != Kind::Tag {
            return Ok(chain);
        }
        let next = object.try_to_tag_ref()?.target();
        chain.push(next);
        current = next;
    }
}

/// Whether `refname` is selected by `pattern`, using git's ref-filter rules:
/// a literal match that ends on a path boundary, or a `wildmatch` in which
/// `*` does not cross `/`.
pub(super) fn pattern_matches(pattern: &str, refname: &[u8], ignore_case: bool) -> bool {
    let p = pattern.as_bytes();
    // ```c
    // if ((plen <= namelen) &&
    //     !strncmp(refname, p, plen) &&
    //     (refname[plen] == '\0' || refname[plen] == '/' || p[plen-1] == '/'))
    //         return 1;
    // if (!wildmatch(p, refname, flags))
    //         return 1;
    // ```
    // (`ref-filter.c:2709-2721`). `WM_CASEFOLD` is set on the *wildmatch* only —
    // the path-prefix test is a plain `strncmp` and stays case-sensitive even
    // under `--ignore-case`, so `git for-each-ref --ignore-case refs/HEADS/`
    // matches nothing.
    if !p.is_empty() && p.len() <= refname.len() {
        let head = &refname[..p.len()];
        if head == p
            && (refname.len() == p.len() || refname[p.len()] == b'/' || p[p.len() - 1] == b'/')
        {
            return true;
        }
    }
    let mut mode = wildmatch::Mode::NO_MATCH_SLASH_LITERAL;
    if ignore_case {
        mode |= wildmatch::Mode::IGNORE_CASE;
    }
    gix::glob::wildmatch(p.as_bstr(), refname.as_bstr(), mode)
}

/// `%(refname:short)`'s shortening: `refs_shorten_unambiguous_ref()` with
/// `strict` = `core.warnAmbiguousRefs`.
///
/// ```c
/// if (atom->option == R_SHORT)
///         return refs_shorten_unambiguous_ref(get_main_ref_store(the_repository),
///                                             refname,
///                                             repo_settings_get_warn_ambiguous_refs(the_repository));
/// ```
/// (`ref-filter.c:2230-2233`)
///
/// Strict is what makes both halves of an ambiguous pair keep a component:
/// `refs/tags/dup` alongside `refs/heads/dup` renders as `tags/dup`, not `dup`.
pub(super) fn short_name(repo: &gix::Repository, refname: &[u8], all: &HashSet<Vec<u8>>) -> Vec<u8> {
    crate::refname::shorten_unambiguous_in_set(
        repo,
        refname,
        crate::refname::warn_ambiguous_refs(repo),
        all,
    )
}

/// Order `refs` by the sort chain, falling back to refname as git does.
pub(super) fn sort_refs(
    ctx: &RenderCtx<'_>,
    refs: Vec<RefInfo>,
    sorts: &[SortKey],
    ignore_case: bool,
    prereleases: &Prereleases<'_>,
) -> Result<Vec<RefInfo>> {
    // Precompute each ref's key values: rendering can fail, and a comparator
    // cannot propagate errors.
    let mut rows: Vec<(Vec<Key>, RefInfo)> = Vec::with_capacity(refs.len());
    for info in refs {
        let mut keys = Vec::with_capacity(sorts.len());
        for s in sorts {
            keys.push(key_of(ctx, s, &info)?);
        }
        rows.push((keys, info));
    }

    rows.sort_by(|(ka, a), (kb, b)| {
        for (n, s) in sorts.iter().enumerate() {
            // The `version:`/`v:` prefix compares the string value with
            // git's `versioncmp`, regardless of the atom's natural type.
            let ord = if s.versioned {
                versioncmp_key(&ka[n], &kb[n], prereleases)
            } else {
                compare(&ka[n], &kb[n], ignore_case)
            };
            let ord = if s.descending { ord.reverse() } else { ord };
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        compare_bytes(&a.refname, &b.refname, ignore_case)
    });

    Ok(rows.into_iter().map(|(_, info)| info).collect())
}

/// A comparable sort value: numeric for sizes and bare timestamps, bytes else.
enum Key {
    Num(i64),
    Str(Vec<u8>),
}

/// Compute the sort value of `key`'s atom for `info`.
fn key_of(ctx: &RenderCtx<'_>, key: &SortKey, info: &RefInfo) -> Result<Key> {
    let repo = ctx.repo;
    let atom = &key.atom;
    // A version-sorted key always compares the atom's rendered string, matching
    // git's `versioncmp(va->s, vb->s)` even for otherwise-numeric atoms.
    if key.versioned {
        return Ok(Key::Str(render(ctx, atom, info)?));
    }
    match &atom.field {
        Field::ObjectSize => Ok(Key::Num(object_of(atom, info).map_or(0, |o| o.size as i64))),
        // The rest of git's `FIELD_ULONG` atoms — `objectsize:disk`
        // (valid_atom's `FIELD_ULONG`), `raw:size` (`raw_atom_parser`,
        // ref-filter.c:728) and `contents:size` (`contents_atom_parser`,
        // ref-filter.c:620). All three render as decimal, so the number they
        // sort by is that rendering read back.
        Field::ObjectSizeDisk
        | Field::Raw(true)
        | Field::Contents(ContentPart::Size)
        // `numparent` is `FIELD_ULONG` too; a non-commit (empty rendering) sorts
        // as the 0 git leaves in `v->value`.
        | Field::NumParent => {
            let s = render(ctx, atom, info)?;
            let n = std::str::from_utf8(&s)
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            Ok(Key::Num(n))
        }
        // `grab_date()` (ref-filter.c:1690-1697) flips the atom to `FIELD_STR`
        // the moment a `:<format>` is spelled out — even `:default`, whose
        // rendering is the same — so only the bare atom sorts numerically.
        Field::Person(w, PersonPart::Date(None)) => {
            let Some(obj) = object_of(atom, info) else {
                return Ok(Key::Num(0));
            };
            let seconds = with_signature(repo, obj, *w, |sig| sig.seconds())?.unwrap_or(0);
            Ok(Key::Num(seconds))
        }
        _ => Ok(Key::Str(render(ctx, atom, info)?)),
    }
}

/// Compare two sort values; mismatched kinds cannot occur for a single key.
fn compare(a: &Key, b: &Key, ignore_case: bool) -> std::cmp::Ordering {
    match (a, b) {
        (Key::Num(a), Key::Num(b)) => a.cmp(b),
        (Key::Str(a), Key::Str(b)) => compare_bytes(a, b, ignore_case),
        _ => std::cmp::Ordering::Equal,
    }
}

/// Byte comparison, ASCII-case-insensitive under `--ignore-case`.
fn compare_bytes(a: &[u8], b: &[u8], ignore_case: bool) -> std::cmp::Ordering {
    if ignore_case {
        let lower = |s: &[u8]| s.to_ascii_lowercase();
        lower(a).cmp(&lower(b))
    } else {
        a.cmp(b)
    }
}

/// Compare two sort values with `versioncmp`; both are strings for a version key.
fn versioncmp_key(a: &Key, b: &Key, prereleases: &Prereleases<'_>) -> std::cmp::Ordering {
    match (a, b) {
        (Key::Str(a), Key::Str(b)) => refsort::versioncmp(a, b, prereleases),
        _ => std::cmp::Ordering::Equal,
    }
}

/// Read `--stdin` patterns the way git's `strbuf_getline` loop does: one pattern
/// per newline-delimited line, stripping a trailing `\r` so CRLF input works.
fn read_stdin_patterns() -> Result<Vec<String>> {
    use std::io::Read;
    let mut data = Vec::new();
    std::io::stdin().read_to_end(&mut data)?;
    let mut patterns = Vec::new();
    let mut rest: &[u8] = &data;
    while !rest.is_empty() {
        let (line, next) = match rest.iter().position(|&b| b == b'\n') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        let line = match line.last() {
            Some(b'\r') => &line[..line.len() - 1],
            _ => line,
        };
        patterns.push(
            String::from_utf8(line.to_vec())
                .map_err(|_| anyhow!("pattern from stdin is not valid utf-8"))?,
        );
        rest = next;
    }
    Ok(patterns)
}

/// The object an atom reads: the peeled target for `*` atoms (absent unless the
/// ref names a tag object), the ref's own object otherwise.
fn object_of<'a>(atom: &Atom, info: &'a RefInfo) -> Option<&'a ObjInfo> {
    if atom.deref {
        info.peeled.as_ref()
    } else {
        Some(&info.obj)
    }
}

/// Render one atom for one ref.
fn render(ctx: &RenderCtx<'_>, atom: &Atom, info: &RefInfo) -> Result<Vec<u8>> {
    let repo = ctx.repo;
    match &atom.field {
        // git's "fill in specials first" pass: these atoms are answered from the
        // ref itself, so a leading `*` has no effect on them.
        Field::Upstream(rr) => return render_upstream(ctx, &info.refname, rr, false),
        Field::Push(rr) => return render_upstream(ctx, &info.refname, rr, true),
        Field::Flag => {
            let mut parts: Vec<&str> = Vec::new();
            if !info.symref.is_empty() {
                parts.push("symref");
            }
            if info.packed {
                parts.push("packed");
            }
            return Ok(parts.join(",").into_bytes());
        }
        Field::WorktreePath => {
            // `FILTER_REFS_BRANCHES` only: a tag never names a working tree.
            if !info.refname.starts_with(b"refs/heads/") {
                return Ok(Vec::new());
            }
            let key = gix::bstr::BString::from(info.refname.clone());
            return Ok(ctx
                .worktrees()
                .get(&key)
                .map(|p| p.as_bytes().to_vec())
                .unwrap_or_default());
        }
        // `populate_value`'s `ATOM_ISBASE` arm (ref-filter.c:2586-2594): the ref
        // [`filter_is_base`] chose renders `(<committish>)`, every other ref
        // renders empty.
        Field::IsBase(name, _) => {
            return Ok(if info.is_base.iter().any(|n| n == name) {
                format!("({name})").into_bytes()
            } else {
                Vec::new()
            })
        }
        Field::AheadBehind(base) => {
            let Some(tip) = commit_tip(repo, info.obj.id)? else {
                // Not a commit: git leaves the atom empty.
                return Ok(Vec::new());
            };
            let (ahead, behind) = ahead_behind(repo, tip, *base)?;
            return Ok(format!("{ahead} {behind}").into_bytes());
        }
        Field::RefName(m) => {
            // `get_refname()` (ref-filter.c:2337-2342) short-circuits ahead of
            // `show_ref()`, so the description is not lstripped or shortened.
            if let Some(desc) = &info.head_desc {
                return Ok(desc.clone());
            }
            return Ok(match m {
                NameMod::Full => info.refname.clone(),
                NameMod::Short => info.short.clone(),
                NameMod::LStrip(n) => refsort::strip_components(&info.refname, *n, true),
                NameMod::RStrip(n) => refsort::strip_components(&info.refname, *n, false),
            })
        }
        Field::SymRef(m) => {
            if info.symref.is_empty() {
                return Ok(Vec::new());
            }
            return Ok(match m {
                NameMod::Full => info.symref.clone(),
                NameMod::Short => info.symref_short.clone(),
                NameMod::LStrip(n) => refsort::strip_components(&info.symref, *n, true),
                NameMod::RStrip(n) => refsort::strip_components(&info.symref, *n, false),
            });
        }
        Field::Color(escape) => return Ok(escape.clone()),
        Field::Head => {
            return Ok(if info.is_head {
                b"*".to_vec()
            } else {
                b" ".to_vec()
            })
        }
        _ => {}
    }

    let Some(obj) = object_of(atom, info) else {
        return Ok(Vec::new());
    };

    match &atom.field {
        Field::ObjectName(len) => Ok(format_oid(repo, obj.id, len)),
        Field::ObjectType => Ok(obj.kind.as_bytes().to_vec()),
        Field::ObjectSize => Ok(obj.size.to_string().into_bytes()),
        // `%(tree)` / `%(parent)` / `%(numparent)` are commit-only; git leaves
        // them empty for any other object kind.
        Field::Tree(len) => Ok(match commit_of(repo, obj)? {
            Some(commit) => format_oid(repo, commit.tree(), len),
            None => Vec::new(),
        }),
        Field::Parent(len) => Ok(match commit_of(repo, obj)? {
            Some(commit) => {
                let mut out: Vec<u8> = Vec::new();
                for (n, parent) in commit.parents().enumerate() {
                    if n > 0 {
                        out.push(b' ');
                    }
                    out.extend_from_slice(&format_oid(repo, parent, len));
                }
                out
            }
            None => Vec::new(),
        }),
        Field::NumParent => Ok(match commit_of(repo, obj)? {
            Some(commit) => commit.parents().count().to_string().into_bytes(),
            None => Vec::new(),
        }),
        // `%(object)` / `%(type)` / `%(tag)` are tag-only; empty otherwise.
        Field::TargetName => Ok(match tag_of(repo, obj)? {
            Some(tag) => tag.target().to_hex().to_string().into_bytes(),
            None => Vec::new(),
        }),
        Field::TargetType => Ok(match tag_of(repo, obj)? {
            Some(tag) => tag.target_kind.as_bytes().to_vec(),
            None => Vec::new(),
        }),
        Field::TagName => Ok(match tag_of(repo, obj)? {
            Some(tag) => tag.name.to_vec(),
            None => Vec::new(),
        }),
        Field::Person(w, part) => render_person(repo, obj, *w, part),
        Field::Contents(part) => render_contents(obj, part),
        Field::Signature(option) => render_signature(obj, *option),
        // `oi.info.delta_base_oid` — `oid_object_info_extended()`'s
        // `OBJECT_INFO_DELTA_BASE`. A non-delta, a loose object and an object
        // this repository cannot locate all render the null oid, which is what
        // git prints from its zeroed `oi.delta_base_oid`.
        Field::DeltaBase => Ok(delta_base(repo, obj.id)
            .unwrap_or_else(|| repo.object_hash().null())
            .to_string()
            .into_bytes()),
        // One implementation of `oi.disk_sizep` for the whole port: it has to try
        // packs before loose files, which the `cat-file` copy already does.
        Field::ObjectSizeDisk => {
            Ok(super::cat_file::disk_size(repo, obj.id)?.to_string().into_bytes())
        }
        Field::Raw(size) => Ok(match (size, obj.data.as_deref()) {
            (true, _) => obj.size.to_string().into_bytes(),
            (false, Some(data)) => data.to_vec(),
            (false, None) => Vec::new(),
        }),
        // git shells `describe` out as a subprocess; doing the same keeps every
        // option (`--tags`, `--abbrev=`, `--match=`, `--exclude=`) on the one
        // implementation instead of a second, drifting copy.
        // `grab_describe_values()` is reached from `grab_values()`'s `OBJ_TAG` and
        // `OBJ_COMMIT` arms only (ref-filter.c:2135, 2150); the `OBJ_TREE` and
        // `OBJ_BLOB` arms call `grab_sub_body_contents()` and nothing else, so a
        // ref pointing straight at a tree or blob renders `%(describe)` empty
        // rather than running `git describe` on it.
        Field::Describe(args) => match obj.kind {
            Kind::Tag | Kind::Commit => run_describe(args, obj.id),
            Kind::Tree | Kind::Blob => Ok(Vec::new()),
        },
        Field::RefName(_)
        | Field::SymRef(_)
        | Field::Head
        | Field::Color(_)
        | Field::Upstream(_)
        | Field::Push(_)
        | Field::Flag
        | Field::WorktreePath
        | Field::IsBase(..)
        | Field::AheadBehind(_) => unreachable!("handled above"),
    }
}

/// git's `%(upstream)` / `%(push)` branch of `populate_value`, plus the
/// `fill_remote_ref_details` it delegates to.
///
/// Only local branches have either relationship, so everything else renders
/// empty. `for_push` selects `branch_get_push` over `branch_get_upstream` and is
/// also what `stat_tracking_info` measures against.
fn render_upstream(
    ctx: &RenderCtx<'_>,
    refname: &[u8],
    rr: &RemoteRef,
    for_push: bool,
) -> Result<Vec<u8>> {
    use super::branch;
    let repo = ctx.repo;
    if !refname.starts_with(b"refs/heads/") {
        return Ok(Vec::new());
    }
    let full = refname.as_bstr();
    let short = full
        .strip_prefix(b"refs/heads/".as_slice())
        .expect("checked above")
        .as_bstr();

    // `remotename` / `remoteref` never resolve a tracking ref, so they answer
    // even for a branch that has no push destination at all.
    let tracking = if rr.push_remote {
        None
    } else {
        let t = if for_push {
            branch::push_ref(repo, full)
        } else {
            branch::upstream_ref(repo, full)
        };
        match t {
            Some(t) => Some(t),
            None => return Ok(Vec::new()),
        }
    };

    let local = repo
        .try_find_reference(full)
        .ok()
        .flatten()
        .and_then(|r| r.into_fully_peeled_id().ok());
    // `:track` and `:trackshort` always measure the atom's own direction, which
    // is not necessarily the one that produced `tracking`.
    let measured = if for_push {
        branch::push_ref(repo, full)
    } else {
        branch::upstream_ref(repo, full)
    };
    let counts = measured
        .as_ref()
        .and_then(|t| branch::stat_tracking_info(repo, local, t));

    let value = match &rr.option {
        RrOption::Ref(m) => {
            let name = tracking.expect("a non-push_remote option resolved one");
            let name = name.as_bstr().to_vec();
            return Ok(match m {
                NameMod::Full => name,
                // `shorten_unambiguous_ref` against the live ref set, exactly as
                // `%(refname:short)` does.
                NameMod::Short => {
                    let mut all: HashSet<Vec<u8>> = HashSet::new();
                    for r in repo.references()?.all()? {
                        let r = r.map_err(|e| anyhow!("{e}"))?;
                        all.insert(r.name().as_bstr().to_vec());
                    }
                    short_name(repo, &name, &all)
                }
                NameMod::LStrip(n) => refsort::strip_components(&name, *n, true),
                NameMod::RStrip(n) => refsort::strip_components(&name, *n, false),
            });
        }
        RrOption::Track => {
            let text = match counts {
                None => "gone".to_string(),
                Some((0, 0)) => String::new(),
                Some((0, t)) => format!("behind {t}"),
                Some((o, 0)) => format!("ahead {o}"),
                Some((o, t)) => format!("ahead {o}, behind {t}"),
            };
            if rr.nobracket || text.is_empty() {
                text
            } else {
                format!("[{text}]")
            }
        }
        RrOption::TrackShort => match counts {
            None => String::new(),
            Some((0, 0)) => "=".into(),
            Some((0, _)) => "<".into(),
            Some((_, 0)) => ">".into(),
            Some(_) => "<>".into(),
        },
        // git's `remote_for_branch` / `pushremote_for_branch`: the name is only
        // reported when it was set explicitly, never the `origin` default.
        RrOption::RemoteName => {
            let dir = if for_push {
                gix::remote::Direction::Push
            } else {
                gix::remote::Direction::Fetch
            };
            repo.branch_remote_name(short, dir)
                .map(|n| n.as_bstr().to_string())
                .unwrap_or_default()
        }
        // git's `remote_ref_for_branch`: the ref name on the *remote* side.
        RrOption::RemoteRefName => {
            let name = gix::refs::FullName::try_from(full.to_owned()).ok();
            let resolved = if for_push {
                // git consults *only* the remote's explicit push refspecs here;
                // with none configured the atom is empty, even though the same
                // branch would still push somewhere under `push.default`.
                name.and_then(|n| push_refspec_dest(repo, &n))
            } else {
                // A fetch reports `branch.<name>.merge` verbatim.
                name.and_then(|n| {
                    repo.branch_remote_ref_name(n.as_ref(), gix::remote::Direction::Fetch)
                })
                .and_then(|r| r.ok())
            };
            resolved.map(|n| n.as_bstr().to_string()).unwrap_or_default()
        }
    };
    Ok(value.into_bytes())
}

/// Whether the ref that ultimately holds `name`'s object id came out of
/// `packed-refs` rather than a loose file, following symrefs to the leaf.
pub(super) fn is_packed(repo: &gix::Repository, name: &str) -> bool {
    let loose = |n: &str| repo.common_dir().join(n).is_file() || repo.git_dir().join(n).is_file();
    let mut current = name.to_string();
    // A cycle is impossible in a well-formed store; the bound keeps a corrupt
    // one from spinning, matching git's own symref-following limit.
    for _ in 0..5 {
        let Ok(r) = repo.find_reference(current.as_str()) else {
            return false;
        };
        match r.target().try_name() {
            Some(next) => current = next.as_bstr().to_string(),
            None => return !loose(&current),
        }
    }
    false
}

/// git's `remote_ref_for_branch(branch, for_push=1)`: run `branch` through the
/// push remote's *configured* push refspecs. `None` when the branch has no push
/// remote, that remote declares no push refspec, or none of them match.
fn push_refspec_dest(
    repo: &gix::Repository,
    branch: &gix::refs::FullName,
) -> Option<gix::refs::FullName> {
    let name = repo.branch_remote_name(branch.shorten(), gix::remote::Direction::Push)?;
    let remote = repo.try_find_remote(name.as_bstr())?.ok()?;
    let specs = remote.refspecs(gix::remote::Direction::Push);
    if specs.is_empty() {
        return None;
    }
    let group = gix::refspec::MatchGroup {
        specs: specs
            .iter()
            .map(gix::refspec::RefSpec::to_ref)
            .filter(|s| s.source().is_some() && s.destination().is_some())
            .collect(),
    };
    let null = repo.object_hash().null();
    let out = group.match_lhs(std::iter::once(gix::refspec::match_group::Item {
        full_ref_name: branch.as_bstr(),
        target: &null,
        object: None,
    }));
    out.mappings
        .into_iter()
        .next()
        .and_then(|m| m.rhs)
        .and_then(|n| gix::refs::FullName::try_from(n.into_owned()).ok())
}

/// git's `grab_describe_values`: run `describe <atom args> <oid>` and take its
/// trailing-whitespace-stripped stdout. A failed run leaves the atom empty.
fn run_describe(args: &[String], id: ObjectId) -> Result<Vec<u8>> {
    let exe = crate::hosted::git_exe()?;
    let out = std::process::Command::new(exe)
        .arg("describe")
        .args(args)
        .arg(id.to_string())
        .output();
    let Ok(out) = out else {
        eprintln!("error: failed to run 'describe'");
        return Ok(Vec::new());
    };
    let mut stdout = out.stdout;
    while stdout.last().is_some_and(|b| b.is_ascii_whitespace()) {
        stdout.pop();
    }
    Ok(stdout)
}

/// Peel `id` to a commit, or `None` when it does not name one — git's
/// `lookup_commit_reference_gently`.
fn commit_tip(repo: &gix::Repository, id: ObjectId) -> Result<Option<ObjectId>> {
    let chain = peel_chain(repo, id)?;
    let tip = *chain.last().unwrap_or(&id);
    Ok((repo.find_header(tip)?.kind() == Kind::Commit).then_some(tip))
}

/// git's `ahead_behind()`: how many commits `tip` has that `base` does not, and
/// the reverse.
fn ahead_behind(repo: &gix::Repository, tip: ObjectId, base: ObjectId) -> Result<(usize, usize)> {
    let count = |from: ObjectId, hidden: ObjectId| -> usize {
        match repo.rev_walk(Some(from)).with_hidden(Some(hidden)).all() {
            Ok(walk) => walk.take_while(Result::is_ok).count(),
            Err(_) => 0,
        }
    };
    Ok((count(tip, base), count(base, tip)))
}

/// Render an object id per an `%(objectname)`-style length modifier.
///
/// The `:short` / `:short=<n>` renderings take their length from gitoxide's
/// abbreviation logic, which does not extend a prefix to guarantee uniqueness
/// the way git's `find_unique_abbrev` does — the divergence the module header
/// notes for `%(objectname:short)` applies to `%(tree)` / `%(parent)` too.
fn format_oid(repo: &gix::Repository, id: ObjectId, len: &NameLen) -> Vec<u8> {
    match len {
        NameLen::Full => id.to_hex().to_string().into_bytes(),
        NameLen::Auto => id.attach(repo).shorten_or_id().to_string().into_bytes(),
        NameLen::Fixed(n) => id.to_hex_with_len(*n).to_string().into_bytes(),
    }
}

/// Parse `obj` as a commit, or `None` when it is another kind (or its data was
/// not loaded). Mirrors git only running `grab_commit_values` on commits.
fn commit_of<'a>(
    repo: &gix::Repository,
    obj: &'a ObjInfo,
) -> Result<Option<CommitRef<'a>>> {
    if obj.kind != Kind::Commit {
        return Ok(None);
    }
    let Some(data) = obj.data.as_deref() else {
        return Ok(None);
    };
    Ok(Some(CommitRef::from_bytes(data, repo.object_hash())?))
}

/// `filter_is_base()` (ref-filter.c:3236-3287) — the whole-array pass that
/// decides, for each `%(is-base:<committish>)` atom, which one ref that
/// committish was branched from.
///
/// This cannot be a per-ref rendering: the answer is a *choice among* the refs
/// in the array, so it has to be made once, before sorting and before `--count`
/// trims anything. Refs whose name does not resolve to a commit are skipped and
/// keep their positions out of the candidate list, exactly as git's `back_index`
/// does.
pub(super) fn filter_is_base(repo: &gix::Repository, refs: &mut [RefInfo], atoms: &[(String, ObjectId)]) {
    if refs.is_empty() || atoms.is_empty() {
        return;
    }

    // `lookup_commit_reference_by_name_gently(name, 1)` over each item, keeping
    // the array positions that answered.
    let mut back_index: Vec<usize> = Vec::with_capacity(refs.len());
    let mut bases: Vec<ObjectId> = Vec::with_capacity(refs.len());
    for (i, info) in refs.iter().enumerate() {
        let Some(id) = crate::objname::resolve(repo, &String::from_utf8_lossy(&info.refname))
        else {
            continue;
        };
        if let crate::objname::CommitRef::Commit(id) =
            crate::objname::lookup_commit_reference(repo, id)
        {
            back_index.push(i);
            bases.push(id);
        }
    }

    let mut graph = CommitGraph::new(repo);
    for (name, tip) in atoms {
        if let Some(best) = graph.branch_base_for_tip(*tip, &bases) {
            refs[back_index[best]].is_base.push(name.clone());
        }
    }
}

/// The slice of a commit that `get_branch_base_for_tip()` needs: its first
/// parent, its committer date, and the generation number the walk orders by.
struct CommitNode {
    first_parent: Option<ObjectId>,
    date: i64,
    /// `commit_graph_generation()`. `None` is git's `GENERATION_NUMBER_ZERO`,
    /// i.e. "not computed yet".
    generation: Option<i64>,
}

/// A lazily-filled commit cache with the corrected-commit-date generation
/// numbers `ensure_generations_valid()` computes when no commit-graph file is
/// available (commit-graph.c:1792-1809 → `compute_reachable_generation_numbers`).
///
/// Only version 2 (corrected commit dates) is implemented: it is
/// `get_configured_generation_version()`'s default, and version 1 exists solely
/// to write an older on-disk format that this port never writes.
struct CommitGraph<'a> {
    repo: &'a gix::Repository,
    nodes: std::collections::HashMap<ObjectId, CommitNode>,
}

impl<'a> CommitGraph<'a> {
    fn new(repo: &'a gix::Repository) -> Self {
        CommitGraph { repo, nodes: std::collections::HashMap::new() }
    }

    /// `repo_parse_commit()`: read the first parent and the committer date once.
    fn parse(&mut self, id: ObjectId) {
        if self.nodes.contains_key(&id) {
            return;
        }
        let node = (|| {
            let obj = self.repo.find_object(id).ok()?;
            let commit = obj.try_into_commit().ok()?;
            let (first_parent, date) = {
                let decoded = commit.decode().ok()?;
                let mut parents = decoded.parents();
                let first = parents.next();
                drop(parents);
                (
                    first,
                    // `CommitRef::committer` is the raw header bytes; the
                    // seconds after the `"> "` are what git's `commit->date`
                    // holds.
                    gix::actor::SignatureRef::from_bytes(decoded.committer)
                        .ok()
                        .and_then(|sig| sig.time().ok())
                        .map_or(0, |t| t.seconds),
                )
            };
            Some(CommitNode { first_parent, date, generation: None })
        })()
        .unwrap_or(CommitNode { first_parent: None, date: 0, generation: None });
        self.nodes.insert(id, node);
    }

    fn first_parent(&mut self, id: ObjectId) -> Option<ObjectId> {
        self.parse(id);
        self.nodes[&id].first_parent
    }

    /// `compute_reachable_generation_numbers()` (commit-graph.c) for one commit:
    /// walk to the ancestors whose generation is still unknown, then fold each
    /// back up as `max(parent generations, own date - 1) + 1`.
    ///
    /// git walks *all* parents here, not just the first, because a generation
    /// number has to bound every ancestor for the priority order to be sound.
    fn generation(&mut self, id: ObjectId) -> i64 {
        self.parse(id);
        if let Some(gen) = self.nodes[&id].generation {
            return gen;
        }

        let mut stack = vec![id];
        while let Some(&current) = stack.last() {
            self.parse(current);
            let parents = self.parents_of(current);

            let mut all_computed = true;
            let mut max_gen = 0i64;
            for parent in parents {
                self.parse(parent);
                match self.nodes[&parent].generation {
                    None => {
                        all_computed = false;
                        stack.push(parent);
                        break;
                    }
                    Some(gen) => max_gen = max_gen.max(gen),
                }
            }

            if all_computed {
                stack.pop();
                // `compute_generation_from_max()`, version 2: corrected commit date.
                let date = self.nodes[&current].date;
                if date != 0 && date > max_gen {
                    max_gen = date - 1;
                }
                self.nodes.get_mut(&current).expect("just parsed").generation = Some(max_gen + 1);
            }
        }
        self.nodes[&id].generation.expect("the walk fills the root last")
    }

    /// Every parent of `id`, which the generation walk needs in full.
    fn parents_of(&mut self, id: ObjectId) -> Vec<ObjectId> {
        (|| {
            let obj = self.repo.find_object(id).ok()?;
            let commit = obj.try_into_commit().ok()?;
            Some(commit.parent_ids().map(|p| p.detach()).collect::<Vec<_>>())
        })()
        .unwrap_or_default()
    }

    /// `get_branch_base_for_tip()` (commit-reach.c:1317-1425): the index into
    /// `bases` of the ref `tip` was branched from, or `None`.
    ///
    /// Each commit carries a "best" mark: `-1` for "reachable from the tip",
    /// `i + 1` for "reached from base `i`", `0` for unmarked. Walking first
    /// parents in generation-then-date order, the first place a tip-marked chain
    /// meets a base-marked one names the branch point, and the smallest base
    /// index that reaches it wins.
    fn branch_base_for_tip(&mut self, tip: ObjectId, bases: &[ObjectId]) -> Option<usize> {
        if bases.is_empty() {
            return None;
        }

        let mut best: std::collections::HashMap<ObjectId, i64> =
            std::collections::HashMap::new();
        let mut queue: Vec<ObjectId> = Vec::new();
        let mut best_index: i64 = -1;
        let mut branch_point: Option<ObjectId> = None;

        best.insert(tip, -1);
        queue.push(tip);

        for (i, &c) in bases.iter().enumerate() {
            match best.get(&c).copied() {
                // Already marked by another commit.
                Some(-1) => return Some(i),
                Some(_) => continue,
                None => {}
            }
            best.insert(c, i as i64 + 1);
            queue.push(c);
        }

        while !queue.is_empty() {
            // `prio_queue_get` under `compare_commits_by_gen_then_commit_date`:
            // the newest generation first, commit date breaking a tie.
            let mut pick = 0;
            for i in 1..queue.len() {
                let (a, b) = (queue[i], queue[pick]);
                let (ga, gb) = (self.generation(a), self.generation(b));
                let newer = ga > gb || (ga == gb && self.date_of(a) > self.date_of(b));
                if newer {
                    pick = i;
                }
            }
            let c = queue.swap_remove(pick);

            // A known branch point is optimal; nothing earlier can beat it.
            if Some(c) == branch_point {
                break;
            }

            let Some(parent) = self.first_parent(c) else { continue };
            let best_for_c = best.get(&c).copied().unwrap_or(0);
            let best_for_p = best.get(&parent).copied().unwrap_or(0);

            if best_for_p == 0 {
                best.insert(parent, best_for_c);
                queue.push(parent);
                continue;
            }
            if best_for_p > 0 && best_for_c > 0 {
                // Collision among bases: keep the smaller index.
                if best_for_c < best_for_p {
                    best.insert(parent, best_for_c);
                }
                continue;
            }

            // Exactly one is positive, by the initial conditions.
            let positive = if best_for_c < 0 { best_for_p } else { best_for_c };
            if best_index < 0 || positive < best_index {
                best_index = positive;
            }
            best.insert(parent, -1);
            branch_point = Some(parent);
        }

        (best_index > 0).then(|| best_index as usize - 1)
    }

    fn date_of(&mut self, id: ObjectId) -> i64 {
        self.parse(id);
        self.nodes[&id].date
    }
}

/// `oid_object_info_extended()`'s `OBJECT_INFO_DELTA_BASE` (`packfile.c`'s
/// `packed_object_info()`): the object a packed entry is stored as a delta
/// against, or `None` when it is not a delta — which covers loose objects, whole
/// packed objects, and anything this repository cannot locate at all.
///
/// A pack stores the base two ways and both have to be answered:
///
///   * `OBJ_REF_DELTA` carries the base's object id in the entry header, so it
///     is read straight out.
///   * `OBJ_OFS_DELTA` carries a *backwards distance* instead, so the base's
///     position is known but its name is not. git resolves that through the
///     pack's reverse index; here the pack index is scanned for the entry that
///     sits at the computed offset, which is the same mapping built the slow
///     way and is only ever walked for an object that really is a delta.
fn delta_base(repo: &gix::Repository, id: gix::hash::ObjectId) -> Option<gix::hash::ObjectId> {
    use gix::odb::pack::Find as _;

    let mut buf = Vec::new();
    // `location_by_oid` hands back a location that is only valid while the pack
    // stays mapped, so the handle has to opt out of unloading first.
    let mut odb = repo.objects.clone();
    odb.prevent_pack_unload();
    let loc = odb.location_by_oid(id.as_ref(), &mut buf)?;
    let entry = odb.entry_by_location(&loc)?;
    let header =
        gix::odb::pack::data::Entry::from_bytes(&entry.data, loc.pack_offset, repo.object_hash())
            .ok()?
            .header;

    match header {
        gix::odb::pack::data::entry::Header::RefDelta { base_id } => Some(base_id),
        gix::odb::pack::data::entry::Header::OfsDelta { base_distance } => {
            let base_offset = loc.pack_offset.checked_sub(base_distance)?;
            oid_at_pack_offset(repo, id, base_offset)
        }
        _ => None,
    }
}

/// The object id of the entry that begins at `offset` in whichever pack holds
/// `neighbour`.
///
/// The pack is identified by asking each index in the object database whether it
/// knows `neighbour` — `location_by_oid`'s `pack_id` is an internal slot number
/// with no path attached, so the index has to be found by content rather than by
/// name. Only the one index that answers is then scanned.
fn oid_at_pack_offset(
    repo: &gix::Repository,
    neighbour: gix::hash::ObjectId,
    offset: u64,
) -> Option<gix::hash::ObjectId> {
    use gix::odb::store::structure::Record;

    let records = repo.objects.store_ref().structure().ok()?;
    for record in records {
        let Record::Index { path, .. } = record else { continue };
        let Ok(index) = gix::odb::pack::index::File::at(&path, repo.object_hash()) else {
            continue;
        };
        if index.lookup(neighbour).is_none() {
            continue;
        }
        for i in 0..index.num_objects() {
            if index.pack_offset_at_index(i) == offset {
                return Some(index.oid_at_index(i).to_owned());
            }
        }
        return None;
    }
    None
}

/// Parse `obj` as a tag, or `None` when it is another kind (or its data was not
/// loaded). Mirrors git only running `grab_tag_values` on tag objects.
fn tag_of<'a>(repo: &gix::Repository, obj: &'a ObjInfo) -> Result<Option<TagRef<'a>>> {
    if obj.kind != Kind::Tag {
        return Ok(None);
    }
    let Some(data) = obj.data.as_deref() else {
        return Ok(None);
    };
    Ok(Some(TagRef::from_bytes(data, repo.object_hash())?))
}

/// Render a name-email-date atom, or nothing when the object has no such header
/// — `grab_person()` (ref-filter.c:1724-1800).
///
/// git works on the raw header line ("wholine"), not on a parsed identity, and
/// the atoms slice it with `strchr`; that is reproduced here because the slices
/// are not equivalent to a parsed `Name`/`email` pair for a malformed ident, and
/// because `:mailmap` rewrites the line and then re-slices it.
///
/// One deliberate divergence, and it is git's bug rather than this port's gap.
/// `grab_person()` keeps `wholine` in a variable declared *outside* its atom
/// loop, and the `:mailmap` branch points it into a `struct strbuf mailmap_buf`
/// declared *inside* that loop — which `strbuf_release(&mailmap_buf)` frees at
/// the end of the same iteration (ref-filter.c:1737, 1759, 1775). The
/// "creator"/"creatordate" tail below the loop then reuses that freed pointer
/// instead of re-deriving it (`if (!wholine) wholine = find_wholine(...)` sees a
/// non-NULL dangling pointer and keeps it, ref-filter.c:1783-1785). Measured on
/// stock git 2.55.0:
///
/// ```text
/// $ git for-each-ref --count=1 --format='%(committername)|%(creator)'
/// C O Mitter|C O Mitter <committer@example.com> 1700000000 +0000
/// $ git for-each-ref --count=1 --format='%(committername:mailmap)|%(creator)'
/// C O Mitter|
/// ```
///
/// `%(creator)` and `%(creatordate)` go empty whenever a `:mailmap` name or
/// email atom for the *same* header is present in the format — and only then;
/// `%(authorname:mailmap)` does not do it, because `grab_person("author")` is a
/// separate call that never reaches the creator tail. That output is a read of
/// freed memory, so it is not a specification: reproducing it would encode one
/// allocator's behaviour as this port's contract. `%(creator)` is answered here.

fn render_person(
    repo: &gix::Repository,
    obj: &ObjInfo,
    w: Who,
    part: &PersonPart,
) -> Result<Vec<u8>> {
    let Some(data) = obj.data.as_deref() else {
        return Ok(Vec::new());
    };
    if !matches!(obj.kind, Kind::Commit | Kind::Tag) {
        return Ok(Vec::new());
    }
    // `%(creator*)` reads `committer` on a commit and `tagger` on a tag; the
    // other three name their header outright, and a header the object does not
    // carry (`author` on a tag) leaves the atom empty.
    let header: &[u8] = match (w, obj.kind) {
        (Who::Author, Kind::Commit) => b"author",
        (Who::Committer, Kind::Commit) | (Who::Creator, Kind::Commit) => b"committer",
        (Who::Tagger, Kind::Tag) | (Who::Creator, Kind::Tag) => b"tagger",
        _ => return Ok(Vec::new()),
    };

    let wants_mailmap = match part {
        PersonPart::Name { mailmap } => *mailmap,
        PersonPart::Email(opt) => opt.has(EmailOpt::MAILMAP),
        _ => false,
    };

    let mapped;
    let wholine = match find_wholine(header, data) {
        None => return Ok(Vec::new()),
        Some(line) if wants_mailmap => {
            mapped = apply_mailmap_to_line(repo, line);
            &mapped[..]
        }
        Some(line) => line,
    };

    Ok(match part {
        // `copy_line()`: the header line as it stands.
        PersonPart::Full => wholine.to_vec(),
        PersonPart::Name { .. } => copy_name(wholine),
        PersonPart::Email(opt) => copy_email(wholine, *opt),
        PersonPart::Date(mode) => grab_date(wholine, mode.as_ref()),
    })
}

/// `find_wholine()` (ref-filter.c:1581-1598): the bytes of the `<who> ` header
/// line after its name and space, up to but not including the newline.
///
/// The scan stops at the blank line that ends the header block, so a body line
/// that happens to start with `committer ` is never mistaken for the header.
fn find_wholine<'a>(who: &[u8], buf: &'a [u8]) -> Option<&'a [u8]> {
    let mut at = 0;
    while at < buf.len() {
        if buf[at..].starts_with(who) && buf.get(at + who.len()) == Some(&b' ') {
            let start = at + who.len() + 1;
            let end = buf[start..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(buf.len(), |nl| start + nl);
            return Some(&buf[start..end]);
        }
        let Some(nl) = buf[at..].iter().position(|&b| b == b'\n') else {
            return None;
        };
        at += nl + 1;
        // A second newline is the end of the header block.
        if buf.get(at) == Some(&b'\n') {
            return None;
        }
    }
    None
}

/// `copy_name()` (ref-filter.c:1602-1610): everything before the first `" <"`,
/// and the empty string if the line has none.
fn copy_name(wholine: &[u8]) -> Vec<u8> {
    for i in 0..wholine.len() {
        if wholine[i..].starts_with(b" <") {
            return wholine[..i].to_vec();
        }
    }
    Vec::new()
}

/// `copy_email()` (ref-filter.c:1641-1657) with `find_end_of_email()`
/// (ref-filter.c:1612-1639).
///
/// The bits are not alternatives. `EO_TRIM` and `EO_LOCALPART` both move the
/// start past the `<`; `EO_MAILMAP` alone leaves the angle brackets on, which is
/// why `%(authoremail:mailmap)` prints `<a@b>` and `%(authoremail:trim,mailmap)`
/// prints `a@b`.
fn copy_email(wholine: &[u8], opt: EmailOpt) -> Vec<u8> {
    let Some(lt) = wholine.iter().position(|&b| b == b'<') else {
        return Vec::new();
    };
    let trimming = opt.has(EmailOpt::LOCALPART) || opt.has(EmailOpt::TRIM);
    let email = if trimming { lt + 1 } else { lt };
    let rest = &wholine[email..];

    let end = if opt.has(EmailOpt::LOCALPART) {
        rest.iter()
            .position(|&b| b == b'@')
            .or_else(|| rest.iter().position(|&b| b == b'>'))
    } else if opt.has(EmailOpt::TRIM) {
        rest.iter().position(|&b| b == b'>')
    } else {
        // The raw and raw-mailmap cases keep the closing bracket.
        rest.iter().position(|&b| b == b'>').map(|i| i + 1)
    };
    match end {
        Some(end) => rest[..end].to_vec(),
        None => Vec::new(),
    }
}

/// `grab_date()` (ref-filter.c:1677-1720): read the `<secs> <tz>` that follows
/// the `"> "` on the header line, then render it through
/// [`crate::showdate::show_date`]. A line that carries no parsable timestamp
/// renders empty, which is git's `goto bad`.
fn grab_date(wholine: &[u8], mode: Option<&crate::showdate::DateMode>) -> Vec<u8> {
    let Some(at) = find_bytes(wholine, b"> ") else {
        return Vec::new();
    };
    let rest = &wholine[at + 2..];
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return Vec::new();
    }
    let Ok(seconds) = std::str::from_utf8(&rest[..digits]).unwrap_or("").parse::<i64>() else {
        return Vec::new();
    };
    // `strtol(zone, NULL, 10)` over whatever follows, whitespace skipped.
    let zone = std::str::from_utf8(&rest[digits..]).unwrap_or("").trim_start();
    let tz = parse_leading_i32(zone);

    let owned;
    let mode = match mode {
        Some(m) => m,
        None => {
            owned = crate::showdate::DateMode::new(crate::showdate::DateType::Normal);
            &owned
        }
    };
    crate::showdate::show_date(seconds, tz, mode, crate::date::now_seconds()).into_bytes()
}

/// `strtol(s, NULL, 10)`: an optional sign and the digits that follow, stopping
/// at the first byte that is not one. Anything unparsable is `0`.
fn parse_leading_i32(s: &str) -> i32 {
    let bytes = s.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    s[..end].parse::<i32>().unwrap_or(0)
}

/// `apply_mailmap_to_header()` / `rewrite_ident_line()` (ident.c:354-392) for one
/// header line: look the `Name <email>` half up in `.mailmap` and splice the
/// replacement back in, leaving the `<secs> <tz>` tail alone.
///
/// git rewrites the whole header buffer and re-runs `find_wholine()` over it;
/// rewriting the one line it would have found is the same thing, because
/// `map_user()` only ever replaces the `Name <email>` span.
fn apply_mailmap_to_line(repo: &gix::Repository, wholine: &[u8]) -> Vec<u8> {
    let Some(lt) = wholine.iter().position(|&b| b == b'<') else {
        return wholine.to_vec();
    };
    let Some(gt) = wholine[lt..].iter().position(|&b| b == b'>').map(|i| lt + i) else {
        return wholine.to_vec();
    };
    // `split_ident_line()` trims the run of blanks before the `<`.
    let name_end = wholine[..lt]
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    let name = &wholine[..name_end];
    let email = &wholine[lt + 1..gt];

    let mailmap = repo.open_mailmap();
    let sig = gix::actor::SignatureRef {
        name: name.into(),
        email: email.into(),
        time: "0 +0000",
    };
    let (mapped_name, mapped_email) = match mailmap.try_resolve_ref(sig) {
        Some(resolved) => (resolved.name, resolved.email),
        None => return wholine.to_vec(),
    };

    let mut out = mapped_name.unwrap_or(name.into()).to_vec();
    out.extend_from_slice(b" <");
    out.extend_from_slice(mapped_email.unwrap_or(email.into()));
    out.push(b'>');
    out.extend_from_slice(&wholine[gt + 1..]);
    out
}

/// Run `f` over the signature `w` names on `obj`, or return `None` when the
/// object kind carries no such header (e.g. `author` on a tag).
fn with_signature<T>(
    repo: &gix::Repository,
    obj: &ObjInfo,
    w: Who,
    f: impl FnOnce(gix::actor::SignatureRef<'_>) -> T,
) -> Result<Option<T>> {
    let Some(data) = obj.data.as_deref() else {
        return Ok(None);
    };
    match obj.kind {
        Kind::Commit => {
            let commit = CommitRef::from_bytes(data, repo.object_hash())?;
            let sig = match w {
                Who::Author => commit.author()?,
                Who::Committer | Who::Creator => commit.committer()?,
                Who::Tagger => return Ok(None),
            };
            Ok(Some(f(sig)))
        }
        Kind::Tag => {
            let tag = TagRef::from_bytes(data, repo.object_hash())?;
            match w {
                Who::Tagger | Who::Creator => Ok(tag.tagger()?.map(f)),
                Who::Author | Who::Committer => Ok(None),
            }
        }
        Kind::Blob | Kind::Tree => Ok(None),
    }
}

/// The four spans `find_subpos()` (ref-filter.c:1899-1938) carves a commit or
/// tag object into, as offsets into the object's raw bytes.
///
/// They overlap on purpose, because git's contents atoms slice the same message
/// four different ways: `sub` starts at the subject and runs to the end of the
/// object, `body` starts after the subject's blank line, `nonsig_len` stops the
/// body at the signature, and `sig` is the signature alone.
pub(super) struct SubPos {
    /// The first non-blank byte after the header block.
    pub(super) sub: usize,
    /// The subject's length, trailing `\n`/`\r` already dropped.
    pub(super) sub_len: usize,
    /// The first byte after the subject's blank line.
    pub(super) body: usize,
    /// From `body` to the end of the object.
    pub(super) body_len: usize,
    /// From `body` to the start of the signature.
    pub(super) nonsig_len: usize,
    /// Where a trailing signature block begins, or the end of the object.
    pub(super) sig: usize,
    /// From `sig` to the end of the object.
    pub(super) sig_len: usize,
}

/// `parse_signed_buffer()` (gpg-interface.c): the offset of the *last* line in
/// `buf` that opens a signature block, or `buf.len()` when there is none.
///
/// The "last" is not a slip: git rescans every line and keeps overwriting its
/// match, so a message body that quotes a signature header does not shadow the
/// real trailing block.
fn parse_signed_buffer(buf: &[u8]) -> usize {
    let mut len = 0;
    let mut matched = buf.len();
    while len < buf.len() {
        if crate::gitsig::format_by_sig(&buf[len..]).is_some() {
            matched = len;
        }
        len = match buf[len..].iter().position(|&b| b == b'\n') {
            Some(nl) => len + nl + 1,
            None => buf.len(),
        };
    }
    matched
}

/// `find_subpos()` (ref-filter.c:1899-1938), byte-for-byte.
pub(super) fn find_subpos(buf: &[u8]) -> SubPos {
    let end = buf.len();

    // Skip past the header until we hit an empty line.
    let mut at = 0;
    while at < end && buf[at] != b'\n' {
        at = match buf[at..].iter().position(|&b| b == b'\n') {
            Some(nl) => at + nl + 1,
            None => end,
        };
    }
    // Skip any empty lines.
    while at < end && buf[at] == b'\n' {
        at += 1;
    }

    // Parse the signature first; we might not even have a subject line.
    let sig = at + parse_signed_buffer(&buf[at..]);
    let sig_len = end - sig;

    let sub = at;
    // The subject goes to the first empty line before the signature begins.
    let eol = find_bytes(&buf[sub..], b"\n\n")
        .or_else(|| find_bytes(&buf[sub..], b"\r\n\r\n"))
        .map_or(sig, |off| (sub + off).min(sig));

    let mut sub_len = eol - sub;
    while sub_len > 0 && matches!(buf[sub + sub_len - 1], b'\n' | b'\r') {
        sub_len -= 1;
    }

    // Skip any empty lines.
    let mut at = eol;
    while at < end && matches!(buf[at], b'\n' | b'\r') {
        at += 1;
    }
    let body = at;

    SubPos {
        sub,
        sub_len,
        body,
        body_len: end - body,
        nonsig_len: sig.saturating_sub(body),
        sig,
        sig_len,
    }
}

/// The offset of the first occurrence of `needle` in `hay`.
fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `copy_subject()` (ref-filter.c:1659-1674): fold the subject onto one line by
/// turning each `\n` into a space and dropping the `\r` of a `\r\n`. Note that
/// this does *not* trim a line's trailing whitespace — `"a  \nb"` becomes
/// `"a   b"`, three spaces.
pub(super) fn copy_subject(sub: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(sub.len());
    for (i, &c) in sub.iter().enumerate() {
        if c == b'\r' && sub.get(i + 1) == Some(&b'\n') {
            continue; /* ignore CR in CRLF */
        }
        out.push(if c == b'\n' { b' ' } else { c });
    }
    out
}

/// `format_sanitized_subject()` (pretty.c:947-973): keep only "title"
/// characters, collapse every run of anything else into a single `-`, squeeze a
/// run of `.` after a `.`, then trim trailing `.`/`-`.
fn format_sanitized_subject(sub: &[u8]) -> Vec<u8> {
    // `istitlechar()` (pretty.c:941-945).
    let istitlechar = |c: u8| c.is_ascii_alphanumeric() || c == b'.' || c == b'_';

    let mut out: Vec<u8> = Vec::with_capacity(sub.len());
    // git starts at 2 so a leading non-title run does *not* emit a `-`: only the
    // transition from "saw a separator after real content" (1) does.
    let mut space = 2u8;
    let mut i = 0;
    while i < sub.len() {
        let c = sub[i];
        if istitlechar(c) {
            if space == 1 {
                out.push(b'-');
            }
            space = 0;
            out.push(c);
            if c == b'.' {
                while sub.get(i + 1) == Some(&b'.') {
                    i += 1;
                }
            }
        } else {
            space |= 1;
        }
        i += 1;
    }

    while out.last().is_some_and(|&c| c == b'.' || c == b'-') {
        out.pop();
    }
    out
}

/// `append_lines()` (ref-filter.c:1943-1961): the first `lines` lines of `buf`,
/// joined by a newline *and four spaces* — the indent `git log --format` uses
/// for a continued body, which `%(contents:lines=<n>)` inherits.
fn append_lines(buf: &[u8], lines: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut sp = 0;
    for i in 0..lines as usize {
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
    out
}

/// Render `%(contents...)`, `%(subject)`, `%(body)` and `%(trailers)` from an
/// object's message — `grab_sub_body_contents()` (ref-filter.c:2013-2110).
///
/// Every arm slices the spans [`find_subpos`] found; the reason this is not one
/// "get the message, then cut it" helper is that git's arms genuinely disagree
/// about where the message ends. `%(body)` keeps a trailing signature block and
/// `%(contents:body)` does not; `%(contents)` keeps it and `%(trailers)` cuts it
/// off before looking for a trailer block.
fn render_contents(obj: &ObjInfo, part: &ContentPart) -> Result<Vec<u8>> {
    let Some(data) = obj.data.as_deref() else {
        return Ok(Vec::new());
    };
    if !matches!(obj.kind, Kind::Commit | Kind::Tag) {
        return Ok(Vec::new());
    }
    let p = find_subpos(data);
    let subject = &data[p.sub..p.sub + p.sub_len];

    match part {
        // `C_BARE`: `xstrdup(subpos)` — to the end of the object, signature and all.
        ContentPart::Bare => Ok(data[p.sub..].to_vec()),
        // `C_LENGTH`: `strlen(subpos)`, which is that same span's length.
        ContentPart::Size => Ok((data.len() - p.sub).to_string().into_bytes()),
        ContentPart::Subject => Ok(copy_subject(subject)),
        ContentPart::SubjectSanitize => Ok(format_sanitized_subject(subject)),
        // `C_BODY_DEP`: `xmemdupz(bodypos, bodylen)`.
        ContentPart::BodyDep => Ok(data[p.body..p.body + p.body_len].to_vec()),
        // `C_BODY`: `xmemdupz(bodypos, nonsiglen)`.
        ContentPart::Body => Ok(data[p.body..p.body + p.nonsig_len].to_vec()),
        // `C_SIG`: `xmemdupz(sigpos, siglen)`.
        ContentPart::Sig => Ok(data[p.sig..p.sig + p.sig_len].to_vec()),
        // `C_LINES`: measured from the *subject*, over the message minus its
        // signature.
        ContentPart::Lines(n) => Ok(append_lines(&data[p.sub..p.body + p.nonsig_len], *n)),
        // `C_TRAILERS`: the message with any signature block cut off, handed to
        // `format_trailers_from_commit()`.
        ContentPart::Trailers(opts) => {
            let msg = if p.sig_len > 0 {
                &data[p.sub..p.sig]
            } else {
                &data[p.sub..]
            };
            // `format_trailers_from_commit()`'s fast path: with every rendering
            // option off, the answer is the block's bytes verbatim, spacing and
            // folded continuation lines included, rather than the parsed items
            // rebuilt.
            if trailers_verbatim(opts) {
                super::interpret_trailers::trailer_block_of(msg)
            } else {
                Ok(super::interpret_trailers::format_pretty(msg, opts))
            }
        }
    }
}

/// `format_trailers_from_commit()`'s fast-path test (trailer.c): no option that
/// changes the rendering is set, so the block can be copied out untouched.
/// `trim_empty` is deliberately absent — git does not consult it here.
fn trailers_verbatim(o: &super::interpret_trailers::PrettyOpts) -> bool {
    !o.only
        && !o.unfold
        && o.keys.is_empty()
        && o.separator.is_none()
        && !o.keyonly
        && !o.valueonly
        && o.key_value_separator.is_none()
}

/// git's `grab_signature`: verify a commit's signature once, then render the
/// field the atom asked for.
///
/// Two distinct "nothing to say" cases, and they render *differently*:
///
///   * the object is not a commit. `grab_values()` calls `grab_signature()` from
///     the `case OBJ_COMMIT:` arm alone (`ref-filter.c:2146`); `OBJ_TAG`,
///     `OBJ_TREE` and `OBJ_BLOB` never reach it, so the atom keeps its
///     `ATOM_VALUE_INIT` empty string. Every `%(signature:…)` on an annotated
///     tag is empty in stock 2.55.0, signed or not — measured on a tag object
///     carrying a real `-----BEGIN PGP SIGNATURE-----` block.
///   * the object is a commit with no signature. `check_commit_signature()` does
///     run and leaves `sigc` zeroed apart from `result = 'N'` and
///     `trust_level = TRUST_UNDEFINED`, so the string fields are empty but
///     `:grade` is `N` and `:trustlevel` is `undefined`.
///
/// `%(*signature:…)` is unaffected either way: the deref pass runs `grab_values()`
/// on the *peeled* object, which is the commit.
///
/// [`crate::gitsig`] is this crate's `check_signature()`, so every field of
/// git's `struct signature_check` is available here — the checker's own report
/// behind the bare `%(signature)` included, and the `VALIDSIG` fingerprints
/// behind `:fingerprint` / `:primarykeyfingerprint`.
fn render_signature(obj: &ObjInfo, option: SigOption) -> Result<Vec<u8>> {
    if obj.kind != Kind::Commit {
        return Ok(Vec::new());
    }
    let data = obj.data.as_deref().unwrap_or_default();
    if crate::gitsig::split_signed(data).is_none() {
        return Ok(match option {
            SigOption::Grade => b"N".to_vec(),
            SigOption::TrustLevel => trust_level_str(crate::gitsig::Trust::Undefined).into(),
            _ => Vec::new(),
        });
    }

    let check = crate::gitsig::evaluate_full(data);
    Ok(match option {
        SigOption::Signer => check.signer.into_bytes(),
        SigOption::Key => check.key.into_bytes(),
        SigOption::Grade => vec![check.pretty_status().code() as u8],
        SigOption::TrustLevel => trust_level_str(check.trust).into(),
        SigOption::Bare => check.output,
        SigOption::Fingerprint => check.fingerprint.into_bytes(),
        SigOption::PrimaryKeyFingerprint => check.primary_key_fingerprint.into_bytes(),
    })
}

/// git's `gpg_trust_level_to_str()`: the lowercase `TRUST_*` suffix.
fn trust_level_str(trust: crate::gitsig::Trust) -> &'static [u8] {
    use crate::gitsig::Trust;
    match trust {
        Trust::Undefined => b"undefined",
        Trust::Never => b"never",
        Trust::Marginal => b"marginal",
        Trust::Fully => b"fully",
        Trust::Ultimate => b"ultimate",
    }
}

/// git's `is_root_ref`: which files directly under `$GIT_DIR` count as root
/// refs for `--include-root-refs`.
///
/// The name must be `is_root_ref_syntax` (uppercase letters, `-`, `_`), must not
/// be one of the special multi-valued refs the ref backend never iterates
/// (`FETCH_HEAD`, `MERGE_HEAD`), and must then be `HEAD`, end with `_HEAD`, or
/// be one of the irregular pseudorefs git lists explicitly.
///
/// Shared with the reference-database check in [`super::fsck`]: `files_fsck` in
/// `refs/files-backend.c` uses it both to pick the root refs it walks and to
/// waive the refname-format check on them.
pub(crate) fn is_root_ref(name: &[u8]) -> bool {
    const IRREGULAR: [&[u8]; 5] = [
        b"AUTO_MERGE",
        b"BISECT_EXPECTED_REV",
        b"NOTES_MERGE_PARTIAL",
        b"NOTES_MERGE_REF",
        b"MERGE_AUTOSTASH",
    ];
    if name.is_empty()
        || !name
            .iter()
            .all(|&b| b.is_ascii_uppercase() || b == b'-' || b == b'_')
    {
        return false;
    }
    if name == b"FETCH_HEAD" || name == b"MERGE_HEAD" {
        return false;
    }
    name == b"HEAD" || name.ends_with(b"_HEAD") || IRREGULAR.contains(&name)
}

/// git's `sq_quote_buf`: always single-quoted, with `'` and `!` escaped so the
/// result is safe to `eval` in a shell.
fn sq_quote(value: &[u8]) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &b in value {
        match b {
            b'\'' => out.extend_from_slice(b"'\\''"),
            b'!' => out.extend_from_slice(b"'\\!'"),
            _ => out.push(b),
        }
    }
    out.push(b'\'');
    out
}

/// git's `perl_quote_buf`: single-quoted, backslash-escaping `'` and `\`.
fn perl_quote(value: &[u8]) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &b in value {
        if b == b'\'' || b == b'\\' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b'\'');
    out
}

/// git's `python_quote_buf`: like perl, but also rendering newlines as `\n`.
fn python_quote(value: &[u8]) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &b in value {
        match b {
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\'' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            _ => out.push(b),
        }
    }
    out.push(b'\'');
    out
}

/// git's `tcl_quote_buf`: double-quoted, backslash-escaping the Tcl metacharacters
/// and rendering the control bytes `\f \r \n \t \v` as two-character escapes.
fn tcl_quote(value: &[u8]) -> Vec<u8> {
    let mut out = vec![b'"'];
    for &b in value {
        match b {
            b'[' | b']' | b'{' | b'}' | b'$' | b'\\' | b'"' => {
                out.push(b'\\');
                out.push(b);
            }
            b'\x0c' => out.extend_from_slice(b"\\f"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\x0b' => out.extend_from_slice(b"\\v"),
            _ => out.push(b),
        }
    }
    out.push(b'"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A parse context for the atoms below, none of which reads the repository
    /// or the quoting style.
    fn test_ctx() -> AtomCtx<'static> {
        AtomCtx {
            repo: None,
            color_on: false,
            quote_style: QuoteStyle::None,
        }
    }

    // Values verified against git 2.39: `%(tree)`/`%(parent)` route through
    // `oid_atom_parser`, which accepts `:short` / `:short=<n>` and floors a
    // sub-minimum length to `minimum_abbrev` (4).
    #[test]
    fn oid_mod_parses_like_oid_atom_parser() {
        assert!(matches!(parse_oid_mod("tree", "tree", None), Ok(NameLen::Full)));
        assert!(matches!(
            parse_oid_mod("tree", "tree", Some("short")),
            Ok(NameLen::Auto)
        ));
        // Above the floor, the length is used verbatim.
        assert!(matches!(
            parse_oid_mod("parent:short=10", "parent", Some("short=10")),
            Ok(NameLen::Fixed(10))
        ));
        // git floors `minimum_abbrev` (4): `short=2` becomes a length of 4.
        assert!(matches!(
            parse_oid_mod("parent:short=2", "parent", Some("short=2")),
            Ok(NameLen::Fixed(4))
        ));
    }

    // Measured against stock 2.55.0: `git for-each-ref --format='%(tree:short=0)'`
    // is `fatal: positive value expected '0' in %(tree:short=0)`. `err_bad_arg`
    // truncates the atom at its first colon, but the `positive value expected`
    // die does not — it quotes `atom->name`, the atom exactly as written.
    #[test]
    fn oid_mod_rejects_zero_and_garbage() {
        let e = parse_oid_mod("tree:short=0", "tree", Some("short=0")).unwrap_err();
        assert!(matches!(e.kind, ErrKind::Fatal));
        assert_eq!(e.msg, "positive value expected '0' in %(tree:short=0)");

        let e = parse_oid_mod("tree:short=xy", "tree", Some("short=xy")).unwrap_err();
        assert!(matches!(e.kind, ErrKind::Fatal));
        assert_eq!(e.msg, "positive value expected 'xy' in %(tree:short=xy)");

        let e = parse_oid_mod("parent:bogus", "parent", Some("bogus")).unwrap_err();
        assert!(matches!(e.kind, ErrKind::Fatal));
        assert_eq!(e.msg, "unrecognized %(parent) argument: bogus");
    }

    // `remote_ref_atom_parser` keeps only the *last* rendering named, treats
    // `nobracket` as an independent flag, and re-reads the whole argument as a
    // refname modifier the moment a token is not one of its keywords — so the
    // error names the entire argument, not the offending token. Verified against
    // `git for-each-ref --format='%(upstream:bogus)'` (git 2.55).
    #[test]
    fn remote_ref_options_parse_like_git() {
        let rr = parse_remote_ref("upstream", Some("track,nobracket")).unwrap();
        assert!(matches!(rr.option, RrOption::Track));
        assert!(rr.nobracket);
        assert!(!rr.push_remote);

        // The last rendering wins.
        let rr = parse_remote_ref("push", Some("track,trackshort")).unwrap();
        assert!(matches!(rr.option, RrOption::TrackShort));

        // `remotename`/`remoteref` also set the flag that suppresses the
        // push-destination lookup.
        let rr = parse_remote_ref("push", Some("remoteref")).unwrap();
        assert!(matches!(rr.option, RrOption::RemoteRefName));
        assert!(rr.push_remote);

        // A non-keyword falls back to `%(refname)`'s modifiers over the whole arg.
        let rr = parse_remote_ref("upstream", Some("lstrip=2")).unwrap();
        assert!(matches!(rr.option, RrOption::Ref(NameMod::LStrip(2))));

        let e = parse_remote_ref("upstream", Some("bogus")).unwrap_err();
        assert!(matches!(e.kind, ErrKind::Fatal));
        assert_eq!(e.msg, "unrecognized %(upstream) argument: bogus");
    }

    // `describe_atom_parser` walks the comma-separated list one key at a time and
    // reports an unrecognised key with the whole *unparsed remainder*, which is
    // what `err_bad_arg(err, "describe", bad_arg)` receives. `tags` is a boolean
    // key, so `tags=false` becomes `--no-tags`.
    #[test]
    fn describe_options_become_subcommand_args() {
        assert_eq!(parse_describe(None).unwrap(), Vec::<String>::new());
        assert_eq!(parse_describe(Some("tags")).unwrap(), vec!["--tags"]);
        assert_eq!(parse_describe(Some("tags=false")).unwrap(), vec!["--no-tags"]);
        assert_eq!(
            parse_describe(Some("tags,abbrev=8,match=v*,exclude=rc*")).unwrap(),
            vec!["--tags", "--abbrev=8", "--match=v*", "--exclude=rc*"]
        );

        let e = parse_describe(Some("tags,bogus,more")).unwrap_err();
        assert_eq!(e.msg, "unrecognized %(describe) argument: bogus,more");
        assert_eq!(
            parse_describe(Some("abbrev=x")).unwrap_err().msg,
            "cannot fully parse describe:abbrev=x"
        );
        assert_eq!(
            parse_describe(Some("match=")).unwrap_err().msg,
            "value expected describe:match="
        );
    }

    // git's atom table gives `object`/`type`/`tag`/`numparent` no parser, so a
    // trailing `:arg` is silently ignored (e.g. `%(type:foo)` == `%(type)`),
    // while `tree`/`parent` take an oid modifier.
    #[test]
    fn commit_and_tag_atoms_parse() {
        assert!(matches!(
            parse_atom("tree:short", &test_ctx()),
            Ok(Atom {
                deref: false,
                field: Field::Tree(NameLen::Auto)
            })
        ));
        assert!(matches!(
            parse_atom("parent", &test_ctx()),
            Ok(Atom {
                field: Field::Parent(NameLen::Full),
                ..
            })
        ));
        assert!(matches!(
            parse_atom("numparent", &test_ctx()),
            Ok(Atom {
                field: Field::NumParent,
                ..
            })
        ));
        assert!(matches!(
            parse_atom("object", &test_ctx()),
            Ok(Atom {
                field: Field::TargetName,
                ..
            })
        ));
        // A modifier on a no-parser atom is ignored, not an error.
        assert!(matches!(
            parse_atom("type:foo", &test_ctx()),
            Ok(Atom {
                field: Field::TargetType,
                ..
            })
        ));
        assert!(matches!(
            parse_atom("tag", &test_ctx()),
            Ok(Atom {
                field: Field::TagName,
                ..
            })
        ));
        // The `*` deref form is allowed on these object atoms.
        assert!(matches!(
            parse_atom("*tree", &test_ctx()),
            Ok(Atom {
                deref: true,
                field: Field::Tree(NameLen::Full)
            })
        ));
    }
}
