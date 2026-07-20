//! `git for-each-ref` — iterate refs and render them through a `--format` template.
//!
//! Covered, byte-identically with stock git: ref discovery and default
//! `refname`-ordered listing, the `<pattern>` / `--exclude` matching rules
//! (literal path-prefix *or* `wildmatch` with pathname semantics), `--count`
//! (including git's "0 means unlimited"), `--sort` (repeatable, `-` for
//! descending, last key primary), `--points-at`, `--merged`/`--no-merged`,
//! `--contains`/`--no-contains`, `--start-after`, `--color`, `--omit-empty`,
//! `--ignore-case`, `--shell` quoting, and the format language's `%%` / `%xx`
//! escapes plus the `%(...)` atoms listed on [`parse_atom`].
//!
//! Exit codes follow git: 128 for the `die()` paths (a bad `--merged` operand, a
//! format that fails verification, the `--start-after` conflicts) and 129 for the
//! `parse-options` paths (a missing option value, a bad `--contains` or
//! `--points-at` operand, an unknown option).
//!
//! Not covered — each rejected rather than silently producing divergent output:
//! `--stdin`, `--include-root-refs`, the `--perl`/`--python`/`--tcl` quoting
//! modes, and the atoms that need substrate this module does not build
//! (`%(upstream)`, `%(push)`, `%(align)`, `%(if)`, `%(describe)`,
//! `%(worktreepath)`, `%(trailers)`, `%(signature)`, `%(raw)`, `%(deltabase)`,
//! `%(ahead-behind)`, `%(is-base)`, `%(objectsize:disk)`, `version:`/`v:` sort
//! keys). Those names are still recognised as *valid* git atoms, so an unknown
//! field name is reported the way git reports it.
//!
//! One known divergence: `%(objectname:short)` takes its length from gitoxide's
//! abbreviation logic, which honours `core.abbrev` but, when it is unset,
//! auto-scales off the packed-object count alone where git also counts loose
//! objects. Set `core.abbrev`, or use `:short=<n>`, for an exact match.

use anyhow::{anyhow, bail, Result};
use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::glob::wildmatch;
use gix::hash::ObjectId;
use gix::objs::{CommitRef, Kind, TagRef};
use gix::prelude::ObjectIdExt;

/// The `%(...)` fields this module can evaluate.
#[derive(Clone)]
enum Field {
    RefName(NameMod),
    SymRef(NameMod),
    ObjectName(NameLen),
    ObjectType,
    ObjectSize,
    Head,
    Person(Who, PersonPart),
    Contents(ContentPart),
    /// `%(color:<spec>)`, pre-rendered: the escape sequence, or empty when
    /// colour is off for this run.
    Color(Vec<u8>),
}

/// Modifiers accepted by `%(refname)` and `%(symref)`.
#[derive(Clone)]
enum NameMod {
    Full,
    Short,
    /// `:lstrip=<n>` (`:strip=` is a synonym).
    LStrip(i64),
    /// `:rstrip=<n>`.
    RStrip(i64),
}

/// Modifiers accepted by `%(objectname)`.
#[derive(Clone)]
enum NameLen {
    Full,
    /// `:short` — length from `core.abbrev`, auto-scaled when unset.
    Auto,
    /// `:short=<n>`.
    Fixed(usize),
}

/// Which name-email-date header a person atom reads.
#[derive(Clone, Copy, PartialEq)]
enum Who {
    Author,
    Committer,
    Tagger,
    /// `committer` on commits, `tagger` on tags.
    Creator,
}

/// Which component of a name-email-date tuple an atom extracts.
#[derive(Clone)]
enum PersonPart {
    /// The whole `Name <email> <secs> <tz>` tuple.
    Full,
    Name,
    Email,
    EmailTrim,
    EmailLocal,
    Date(DateFmt),
}

/// The date renderings `%(*date[:<fmt>])` understands.
#[derive(Clone, Copy, PartialEq)]
enum DateFmt {
    Default,
    Short,
    Iso,
    IsoStrict,
    Rfc2822,
    Unix,
    Raw,
}

/// Which slice of a commit/tag message a contents atom extracts.
#[derive(Clone)]
enum ContentPart {
    All,
    Subject,
    Body,
    Size,
}

/// One `%(...)` atom: an optional leading `*` (evaluate against the peeled
/// object) plus the field itself.
#[derive(Clone)]
struct Atom {
    deref: bool,
    field: Field,
}

/// A parsed format string is a sequence of literal runs and atoms.
enum Item {
    Lit(Vec<u8>),
    Atom(Atom),
}

/// A sort key: an atom plus its direction.
struct SortKey {
    atom: Atom,
    descending: bool,
}

/// Everything known about one object referenced during a run.
struct ObjInfo {
    id: ObjectId,
    kind: Kind,
    size: u64,
    /// Full object data, loaded only when a person/contents atom needs it.
    data: Option<Vec<u8>>,
}

/// One ref, resolved and ready to render.
struct RefInfo {
    refname: Vec<u8>,
    /// `%(refname:short)`, computed only when the format or a sort key asks.
    short: Vec<u8>,
    /// The target of a symbolic ref, empty for a direct one.
    symref: Vec<u8>,
    /// `%(symref:short)`, computed only when asked for.
    symref_short: Vec<u8>,
    obj: ObjInfo,
    /// Present only when `obj` is a tag object, holding its fully peeled target.
    peeled: Option<ObjInfo>,
    is_head: bool,
}

/// Which exit code a format/sort parse failure maps onto.
///
/// git splits these: `verify_ref_format` failures `die()` (128), a malformed
/// `%(` reaches the `parse-options` usage path (129), and anything this module
/// simply has not built is reported as an ordinary error.
enum ErrKind {
    Fatal,
    Usage,
    Unported,
}

/// A format or sort-key parse failure, carrying the exit code it implies.
struct AtomError {
    kind: ErrKind,
    msg: String,
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

/// Turn a parse failure into the exit code git would produce.
fn report_atom_error(e: AtomError) -> Result<ExitCode> {
    match e.kind {
        ErrKind::Fatal => Ok(fatal(&e.msg)),
        ErrKind::Usage => Ok(usage_error(&e.msg)),
        ErrKind::Unported => bail!("{}", e.msg),
    }
}

/// git's `die()`: message on stderr, exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// git's `parse-options` failure: message on stderr, exit 129.
fn usage_error(msg: &str) -> ExitCode {
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

/// The reachability filters, each a list of commits combined with "any".
#[derive(Default)]
struct Filters {
    contains: Vec<ObjectId>,
    no_contains: Vec<ObjectId>,
    merged: Vec<ObjectId>,
    no_merged: Vec<ObjectId>,
}

impl Filters {
    fn active(&self) -> bool {
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

    let mut format: Vec<u8> = b"%(objectname) %(objecttype)\t%(refname)".to_vec();
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
    let mut shell_quote = false;

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
                None => return Ok(usage_error(&format!("option `{}' requires a value", $name))),
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
        let a = args[i].as_str();
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
        let (name, rest) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (a, None),
        };
        match name {
            "--format" => format = value!(rest, "format").into_bytes(),
            "--count" => {
                let v = value!(rest, "count");
                count =
                    match parse_count(&v) {
                        Some(n) => n,
                        None => return Ok(usage_error(
                            "option `count' expects an integer value with an optional k/m/g suffix",
                        )),
                    };
            }
            "--sort" => sort_specs.push(value!(rest, "sort")),
            "--exclude" => excludes.push(value!(rest, "exclude")),
            "--start-after" => start_after = Some(value!(rest, "start-after")),
            "--points-at" => {
                let v = value!(rest, "points-at");
                points_at = match repo.rev_parse_single(v.as_str()) {
                    Ok(id) => Some(id.detach()),
                    // git quotes the operand here but not in the filter options.
                    Err(_) => return Ok(usage_error(&format!("malformed object name '{v}'"))),
                };
            }
            "--contains" | "--no-contains" => {
                let v = commit_operand!(rest);
                let Some(id) = resolve_commit(&repo, &v) else {
                    return Ok(usage_error(&format!("malformed object name {v}")));
                };
                if name == "--contains" {
                    filters.contains.push(id);
                } else {
                    filters.no_contains.push(id);
                }
            }
            "--merged" | "--no-merged" => {
                let v = commit_operand!(rest);
                // These go through `parse_opt_merge_filter`, which dies rather
                // than returning a usage error, so the exit code is 128 here.
                let Some(id) = resolve_commit(&repo, &v) else {
                    return Ok(fatal(&format!("malformed object name {v}")));
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
                        return Ok(usage_error(
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
            "--shell" | "-s" => shell_quote = true,
            "--perl" | "--python" | "--tcl" | "-p" => {
                bail!("unsupported flag {name:?} (ported quoting mode: --shell)")
            }
            "--stdin" | "--include-root-refs" => {
                bail!(
                    "unsupported flag {name:?} (ported: --format, --count, --sort, --exclude, \
                     --points-at, --start-after, --color, --merged, --no-merged, --contains, \
                     --no-contains, --omit-empty, --ignore-case, --shell)"
                )
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Ok(usage_error(&format!(
                    "unknown option `{}'",
                    s.trim_start_matches('-')
                )))
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

    let color_on = match color_when {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => std::io::stdout().is_terminal(),
    };

    // git's order after option parsing: verify the format, then reject the
    // `--start-after` combinations, then parse the sort keys.
    let items = match parse_format(&format, color_on) {
        Ok(items) => items,
        Err(e) => return report_atom_error(e),
    };

    if start_after.is_some() {
        if !sort_specs.is_empty() {
            return Ok(fatal("cannot use --start-after with custom sort options"));
        }
        if !patterns.is_empty() {
            return Ok(fatal("cannot use --start-after with patterns"));
        }
    }

    let mut sorts: Vec<SortKey> = Vec::new();
    for spec in &sort_specs {
        let (spec, descending) = match spec.strip_prefix('-') {
            Some(r) => (r, true),
            None => (spec.as_str(), false),
        };
        if spec.starts_with("version:") || spec.starts_with("v:") {
            bail!("unsupported sort key {spec:?} (version sorting is not ported)");
        }
        match parse_atom(spec, color_on) {
            Ok(atom) => sorts.push(SortKey { atom, descending }),
            Err(e) => return report_atom_error(e),
        }
    }
    // Later `--sort` options take precedence, so the last given key sorts first.
    sorts.reverse();

    let atoms = || {
        items
            .iter()
            .filter_map(|it| match it {
                Item::Atom(a) => Some(a),
                Item::Lit(_) => None,
            })
            .chain(sorts.iter().map(|s| &s.atom))
    };
    let needs_data = atoms().any(|a| matches!(a.field, Field::Person(..) | Field::Contents(_)));
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
        let id = reference.follow_to_object()?.detach();

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
            short_name(&refname, &all_names)
        } else {
            Vec::new()
        };
        let symref_short = if needs_symref_short && !symref.is_empty() {
            short_name(&symref, &all_names)
        } else {
            Vec::new()
        };

        refs.push(RefInfo {
            is_head: head_name.as_deref() == Some(refname.as_slice()),
            refname,
            short,
            symref,
            symref_short,
            obj,
            peeled,
        });
    }

    let mut refs = sort_refs(&repo, refs, &sorts, ignore_case)?;
    if let Some(n) = count {
        refs.truncate(n);
    }

    let mut out: Vec<u8> = Vec::new();
    for info in &refs {
        let mut line: Vec<u8> = Vec::new();
        for item in &items {
            match item {
                Item::Lit(bytes) => line.extend_from_slice(bytes),
                Item::Atom(atom) => {
                    let value = render(&repo, atom, info)?;
                    // Colour escapes are emitted verbatim; git does not quote them.
                    if shell_quote && !matches!(atom.field, Field::Color(_)) {
                        line.extend_from_slice(&sq_quote(&value));
                    } else {
                        line.extend_from_slice(&value);
                    }
                }
            }
        }
        if omit_empty && line.is_empty() {
            continue;
        }
        line.push(b'\n');
        out.extend_from_slice(&line);
    }

    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// git's `OPT_INTEGER` operand: a decimal count with an optional `k`/`m`/`g`
/// scaling suffix.
fn parse_count(v: &str) -> Option<i64> {
    let (digits, scale): (&str, i64) = match v.as_bytes().last().copied() {
        Some(b'k') | Some(b'K') => (&v[..v.len() - 1], 1024),
        Some(b'm') | Some(b'M') => (&v[..v.len() - 1], 1024 * 1024),
        Some(b'g') | Some(b'G') => (&v[..v.len() - 1], 1024 * 1024 * 1024),
        _ => (v, 1),
    };
    digits.parse::<i64>().ok()?.checked_mul(scale)
}

/// Resolve a filter operand the way `parse_opt_commits` does: parse the
/// revision, then peel it to a commit. `None` covers both failures, which git
/// reports with the same exit code per option.
fn resolve_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let id = repo.rev_parse_single(spec).ok()?.detach();
    let chain = peel_chain(repo, id).ok()?;
    let tip = *chain.last().unwrap_or(&id);
    (repo.find_header(tip).ok()?.kind() == Kind::Commit).then_some(tip)
}

/// Whether `tip` survives the reachability filters.
///
/// A ref that does not peel to a commit is dropped by every one of them, as git
/// does when `lookup_commit_reference_gently` comes back empty.
fn passes_filters(repo: &gix::Repository, filters: &Filters, tip: ObjectId) -> Result<bool> {
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
fn parse_format(fmt: &[u8], color_on: bool) -> std::result::Result<Vec<Item>, AtomError> {
    let mut items = Vec::new();
    let mut lit: Vec<u8> = Vec::new();
    let mut i = 0;

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
                items.push(Item::Atom(parse_atom(spec, color_on)?));
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
    Ok(items)
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
/// Understood: `refname[:short|:lstrip=<n>|:strip=<n>|:rstrip=<n>]`, `symref`
/// with the same modifiers, `objectname[:short[=<n>]]`, `objecttype`,
/// `objectsize`, `HEAD`, `color:<spec>`, `author`/`committer`/`tagger`/`creator`
/// and their `name`, `email` (`:trim`, `:localpart`) and `date` (`:short`,
/// `:iso8601`, `:iso8601-strict`, `:rfc2822`, `:unix`, `:raw`, `:default`)
/// forms, `subject`, `body`, and `contents[:subject|:body|:size]`. A leading `*`
/// evaluates the atom against the object a tag peels to.
fn parse_atom(spec: &str, color_on: bool) -> std::result::Result<Atom, AtomError> {
    let (body, deref) = match spec.strip_prefix('*') {
        Some(rest) => (rest, true),
        None => (spec, false),
    };
    let (name, m) = match body.split_once(':') {
        Some((n, m)) => (n, Some(m)),
        None => (body, None),
    };

    // Reject a modifier on an atom that takes none, naming the offending atom.
    let bare = |m: Option<&str>| -> std::result::Result<(), AtomError> {
        match m {
            None => Ok(()),
            Some(m) => Err(fatal_atom(format!("unrecognized %({name}) argument: {m}"))),
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
        "objectname" => Field::ObjectName(match m {
            None => NameLen::Full,
            Some("short") => NameLen::Auto,
            Some(m) => match m.strip_prefix("short=") {
                Some(n) => NameLen::Fixed(n.parse::<usize>().map_err(|_| {
                    fatal_atom(format!("unrecognized %(objectname) argument: {m}"))
                })?),
                None => {
                    return Err(fatal_atom(format!(
                        "unrecognized %(objectname) argument: {m}"
                    )))
                }
            },
        }),
        "objecttype" => {
            bare(m)?;
            Field::ObjectType
        }
        "objectsize" => match m {
            None => Field::ObjectSize,
            Some("disk") => return Err(unported_atom("%(objectsize:disk) is not ported")),
            Some(m) => {
                return Err(fatal_atom(format!(
                    "unrecognized %(objectsize) argument: {m}"
                )))
            }
        },
        "HEAD" => {
            bare(m)?;
            Field::Head
        }
        "color" => match m {
            None => return Err(fatal_atom("expected format: %(color:<color>)")),
            Some(spec) => match parse_color(spec) {
                Some(escape) => Field::Color(if color_on { escape } else { Vec::new() }),
                None => return Err(fatal_atom(format!("invalid color value: {spec}"))),
            },
        },
        "author" | "committer" | "tagger" | "creator" => {
            bare(m)?;
            Field::Person(who(name), PersonPart::Full)
        }
        "authorname" | "committername" | "taggername" => {
            bare(m)?;
            Field::Person(who(name.trim_end_matches("name")), PersonPart::Name)
        }
        "authoremail" | "committeremail" | "taggeremail" => {
            let part = match m {
                None => PersonPart::Email,
                Some("trim") => PersonPart::EmailTrim,
                Some("localpart") => PersonPart::EmailLocal,
                Some(m) => return Err(fatal_atom(format!("unrecognized %({name}) argument: {m}"))),
            };
            Field::Person(who(name.trim_end_matches("email")), part)
        }
        "authordate" | "committerdate" | "taggerdate" | "creatordate" => {
            let fmt = match m {
                None | Some("default") => DateFmt::Default,
                Some("short") => DateFmt::Short,
                Some("iso8601") | Some("iso") => DateFmt::Iso,
                Some("iso8601-strict") | Some("iso-strict") => DateFmt::IsoStrict,
                Some("rfc2822") => DateFmt::Rfc2822,
                Some("unix") => DateFmt::Unix,
                Some("raw") => DateFmt::Raw,
                Some(m) => {
                    return Err(unported_atom(format!(
                        "date format `:{m}` is not ported (ported: default, short, iso8601, \
                         iso8601-strict, rfc2822, unix, raw)"
                    )))
                }
            };
            Field::Person(who(name.trim_end_matches("date")), PersonPart::Date(fmt))
        }
        "subject" => {
            bare(m)?;
            Field::Contents(ContentPart::Subject)
        }
        "body" => {
            bare(m)?;
            Field::Contents(ContentPart::Body)
        }
        "contents" => Field::Contents(match m {
            None => ContentPart::All,
            Some("subject") => ContentPart::Subject,
            Some("body") => ContentPart::Body,
            Some("size") => ContentPart::Size,
            Some(m) => {
                return Err(fatal_atom(format!(
                    "unrecognized %(contents) argument: {m}"
                )))
            }
        }),
        // A name git knows but this module does not evaluate is an honest gap,
        // not the "unknown field name" git reserves for a typo.
        n if KNOWN_ATOMS.contains(&n) => {
            return Err(unported_atom(format!("%({n}) is not ported")))
        }
        n => return Err(fatal_atom(format!("unknown field name: {n}"))),
    };

    if deref && matches!(field, Field::RefName(_) | Field::SymRef(_) | Field::Head) {
        return Err(fatal_atom(format!("`*` has no meaning on %({name})")));
    }
    Ok(Atom { deref, field })
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
fn load(repo: &gix::Repository, id: ObjectId, with_data: bool) -> Result<ObjInfo> {
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
fn peel_chain(repo: &gix::Repository, id: ObjectId) -> Result<Vec<ObjectId>> {
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
fn pattern_matches(pattern: &str, refname: &[u8], ignore_case: bool) -> bool {
    let p = pattern.as_bytes();
    if !p.is_empty() && p.len() <= refname.len() {
        let head = &refname[..p.len()];
        let literal = if ignore_case {
            head.eq_ignore_ascii_case(p)
        } else {
            head == p
        };
        if literal
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

/// git's `shorten_unambiguous_ref` in strict mode: try the rev-parse rules from
/// the most specific down, and accept the first candidate that no *other* rule
/// could expand into an existing ref.
fn short_name(refname: &[u8], all: &HashSet<Vec<u8>>) -> Vec<u8> {
    // Mirrors `ref_rev_parse_rules`. The first rule (the bare name) is only ever
    // used to test a candidate for ambiguity, never to produce one, and the
    // `refs/remotes/<name>/HEAD` rule is unreachable because git's `%s` scan is
    // greedy to end-of-string.
    const PREFIXES: [&[u8]; 4] = [b"refs/", b"refs/tags/", b"refs/heads/", b"refs/remotes/"];

    for (i, prefix) in PREFIXES.iter().enumerate().rev() {
        let Some(candidate) = refname.strip_prefix(*prefix) else {
            continue;
        };
        if candidate.is_empty() {
            continue;
        }
        // Rule 0 expands to the bare candidate; the rest re-prefix it.
        let ambiguous = std::iter::once(candidate.to_vec())
            .chain(
                PREFIXES
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, p)| [*p, candidate].concat()),
            )
            .any(|name| all.contains(&name));
        if !ambiguous {
            return candidate.to_vec();
        }
    }
    refname.to_vec()
}

/// Order `refs` by the sort chain, falling back to refname as git does.
fn sort_refs(
    repo: &gix::Repository,
    refs: Vec<RefInfo>,
    sorts: &[SortKey],
    ignore_case: bool,
) -> Result<Vec<RefInfo>> {
    // Precompute each ref's key values: rendering can fail, and a comparator
    // cannot propagate errors.
    let mut rows: Vec<(Vec<Key>, RefInfo)> = Vec::with_capacity(refs.len());
    for info in refs {
        let mut keys = Vec::with_capacity(sorts.len());
        for s in sorts {
            keys.push(key_of(repo, &s.atom, &info)?);
        }
        rows.push((keys, info));
    }

    rows.sort_by(|(ka, a), (kb, b)| {
        for (n, s) in sorts.iter().enumerate() {
            let ord = compare(&ka[n], &kb[n], ignore_case);
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

/// Compute the sort value of `atom` for `info`.
fn key_of(repo: &gix::Repository, atom: &Atom, info: &RefInfo) -> Result<Key> {
    match &atom.field {
        Field::ObjectSize => Ok(Key::Num(object_of(atom, info).map_or(0, |o| o.size as i64))),
        Field::Person(w, PersonPart::Date(DateFmt::Default)) => {
            let Some(obj) = object_of(atom, info) else {
                return Ok(Key::Num(0));
            };
            let seconds = with_signature(repo, obj, *w, |sig| sig.seconds())?.unwrap_or(0);
            Ok(Key::Num(seconds))
        }
        _ => Ok(Key::Str(render(repo, atom, info)?)),
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
fn render(repo: &gix::Repository, atom: &Atom, info: &RefInfo) -> Result<Vec<u8>> {
    match &atom.field {
        Field::RefName(m) => {
            return Ok(match m {
                NameMod::Full => info.refname.clone(),
                NameMod::Short => info.short.clone(),
                NameMod::LStrip(n) => strip_components(&info.refname, *n, true),
                NameMod::RStrip(n) => strip_components(&info.refname, *n, false),
            })
        }
        Field::SymRef(m) => {
            if info.symref.is_empty() {
                return Ok(Vec::new());
            }
            return Ok(match m {
                NameMod::Full => info.symref.clone(),
                NameMod::Short => info.symref_short.clone(),
                NameMod::LStrip(n) => strip_components(&info.symref, *n, true),
                NameMod::RStrip(n) => strip_components(&info.symref, *n, false),
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
        Field::ObjectName(len) => Ok(match len {
            NameLen::Full => obj.id.to_hex().to_string().into_bytes(),
            NameLen::Auto => obj.id.attach(repo).shorten_or_id().to_string().into_bytes(),
            NameLen::Fixed(n) => obj.id.to_hex_with_len(*n).to_string().into_bytes(),
        }),
        Field::ObjectType => Ok(obj.kind.as_bytes().to_vec()),
        Field::ObjectSize => Ok(obj.size.to_string().into_bytes()),
        Field::Person(w, part) => render_person(repo, obj, *w, part),
        Field::Contents(part) => Ok(render_contents(obj, part)),
        Field::RefName(_) | Field::SymRef(_) | Field::Head | Field::Color(_) => {
            unreachable!("handled above")
        }
    }
}

/// `%(refname:lstrip=<n>)` / `%(refname:rstrip=<n>)`.
///
/// A positive `n` drops `n` components from the given end; a negative `n` keeps
/// `-n` components at that end. Over-stripping yields an empty string for
/// positive counts and the full name for negative ones — never an error.
fn strip_components(name: &[u8], n: i64, from_left: bool) -> Vec<u8> {
    let parts: Vec<&[u8]> = name.split(|&b| b == b'/').collect();
    let len = parts.len() as i64;
    let kept: &[&[u8]] = if n >= 0 {
        if n >= len {
            &[]
        } else if from_left {
            &parts[n as usize..]
        } else {
            &parts[..(len - n) as usize]
        }
    } else {
        let keep = -n;
        if keep >= len {
            &parts[..]
        } else if from_left {
            &parts[(len - keep) as usize..]
        } else {
            &parts[..keep as usize]
        }
    };
    kept.join(&b'/')
}

/// Render a name-email-date atom, or nothing when the object has no such header.
fn render_person(
    repo: &gix::Repository,
    obj: &ObjInfo,
    w: Who,
    part: &PersonPart,
) -> Result<Vec<u8>> {
    let rendered = with_signature(repo, obj, w, |sig| match part {
        PersonPart::Full => {
            let mut out = sig.name.to_vec();
            out.extend_from_slice(b" <");
            out.extend_from_slice(sig.email);
            out.extend_from_slice(b"> ");
            out.extend_from_slice(sig.time.as_bytes());
            out
        }
        PersonPart::Name => sig.name.to_vec(),
        PersonPart::Email => {
            let mut out = b"<".to_vec();
            out.extend_from_slice(sig.email);
            out.push(b'>');
            out
        }
        PersonPart::EmailTrim => sig.email.to_vec(),
        PersonPart::EmailLocal => match sig.email.iter().position(|&b| b == b'@') {
            Some(at) => sig.email[..at].to_vec(),
            None => sig.email.to_vec(),
        },
        PersonPart::Date(fmt) => match sig.time() {
            Ok(time) => format_date(time, *fmt).into_bytes(),
            Err(_) => sig.time.as_bytes().to_vec(),
        },
    })?;
    Ok(rendered.unwrap_or_default())
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

/// Format a timestamp the way `git log --date=<fmt>` does.
fn format_date(time: gix::date::Time, fmt: DateFmt) -> String {
    use gix::date::time::format;
    match fmt {
        DateFmt::Default => time.format_or_unix(format::DEFAULT),
        DateFmt::Short => time.format_or_unix(format::SHORT),
        DateFmt::Iso => time.format_or_unix(format::ISO8601),
        DateFmt::IsoStrict => time.format_or_unix(format::ISO8601_STRICT),
        DateFmt::Rfc2822 => time.format_or_unix(format::GIT_RFC2822),
        DateFmt::Unix => time.format_or_unix(format::UNIX),
        DateFmt::Raw => time.format_or_unix(format::RAW),
    }
}

/// Render `%(contents...)`, `%(subject)` and `%(body)` from an object's message.
fn render_contents(obj: &ObjInfo, part: &ContentPart) -> Vec<u8> {
    let Some(data) = obj.data.as_deref() else {
        return Vec::new();
    };
    if !matches!(obj.kind, Kind::Commit | Kind::Tag) {
        return Vec::new();
    }
    // git takes everything after the header block; a header continuation line
    // always starts with a space, so the first blank line ends the headers.
    let contents = match data.windows(2).position(|w| w == &b"\n\n"[..]) {
        Some(i) => &data[i + 2..],
        None => &data[..0],
    };

    match part {
        ContentPart::All => contents.to_vec(),
        ContentPart::Size => contents.len().to_string().into_bytes(),
        ContentPart::Subject | ContentPart::Body => {
            // Signatures belong to neither the subject nor the body.
            let body = &contents[..signature_start(contents)];
            let body = trim_leading_newlines(body);
            let (subject, rest) = match body.windows(2).position(|w| w == &b"\n\n"[..]) {
                Some(i) => (&body[..i], trim_leading_newlines(&body[i..])),
                None => (body, &body[body.len()..]),
            };
            match part {
                ContentPart::Subject => fold_subject(subject),
                _ => rest.to_vec(),
            }
        }
    }
}

/// The offset of a line-anchored signature block, or `msg.len()` if absent.
fn signature_start(msg: &[u8]) -> usize {
    const MARKERS: [&[u8]; 2] = [
        b"-----BEGIN PGP SIGNATURE-----",
        b"-----BEGIN SSH SIGNATURE-----",
    ];
    let mut line_start = 0;
    while line_start <= msg.len() {
        if MARKERS.iter().any(|m| msg[line_start..].starts_with(m)) {
            return line_start;
        }
        match msg[line_start..].iter().position(|&b| b == b'\n') {
            Some(nl) => line_start += nl + 1,
            None => break,
        }
    }
    msg.len()
}

/// Drop leading blank lines, as git does before locating the subject.
fn trim_leading_newlines(msg: &[u8]) -> &[u8] {
    let start = msg.iter().position(|&b| b != b'\n').unwrap_or(msg.len());
    &msg[start..]
}

/// Fold the subject paragraph into a single line: each line is right-trimmed of
/// whitespace and the lines are joined with a space, stopping at the first blank.
fn fold_subject(subject: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for line in subject.split(|&b| b == b'\n') {
        let end = line
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |i| i + 1);
        if end == 0 {
            break;
        }
        if !out.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(&line[..end]);
    }
    out
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
