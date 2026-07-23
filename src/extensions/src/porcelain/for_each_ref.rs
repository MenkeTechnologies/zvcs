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
//! Not covered — rejected rather than silently producing divergent output: the
//! atoms that need substrate this module does not build (`%(upstream)`,
//! `%(push)`, `%(if)`, `%(describe)`, `%(worktreepath)`, `%(trailers)`,
//! `%(signature)`, `%(raw)`, `%(deltabase)`, `%(ahead-behind)`, `%(is-base)`,
//! `%(objectsize:disk)`). Those names are still recognised as *valid* git
//! atoms, so an unknown field name is reported the way git reports it.
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

/// A parsed format string is a sequence of literal runs, atoms, and the
/// `%(align:…)` / `%(end)` container markers that pad the content between them.
enum Item {
    Lit(Vec<u8>),
    Atom(Atom),
    AlignStart(AlignSpec),
    End,
}

/// `%(align:<width>,<position>)` — pad the enclosed content to `width` display
/// columns; content already at or over `width` is left untouched (never cut).
#[derive(Clone)]
struct AlignSpec {
    width: usize,
    position: AlignPos,
}

#[derive(Clone, Copy)]
enum AlignPos {
    Left,
    Right,
    Middle,
}

/// A sort key: an atom, its direction, and whether to compare with `versioncmp`
/// (the `version:` / `v:` prefix).
struct SortKey {
    atom: Atom,
    descending: bool,
    versioned: bool,
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

/// How `--shell`/`--perl`/`--python`/`--tcl` quote each rendered atom.
///
/// git tracks the four styles as independent bits (see the `Q_*` masks) so that
/// repeating one style is harmless but requesting two distinct ones is
/// "more than one quoting style?". `--no-<style>` clears its bit.
#[derive(Clone, Copy, PartialEq)]
enum QuoteStyle {
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
    let items = match parse_format(&format, color_on) {
        Ok(items) => items,
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
        match parse_atom(spec, color_on) {
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

    // Version sorting reads its prerelease-suffix ordering from config once.
    let prereleases = if sorts.iter().any(|s| s.versioned) {
        read_prereleases(&repo)
    } else {
        Vec::new()
    };

    let atoms = || {
        items
            .iter()
            .filter_map(|it| match it {
                Item::Atom(a) => Some(a),
                Item::Lit(_) | Item::AlignStart(_) | Item::End => None,
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

    let mut refs = sort_refs(&repo, refs, &sorts, ignore_case, &prereleases)?;
    if let Some(n) = count {
        refs.truncate(n);
    }

    let mut out: Vec<u8> = Vec::new();
    for info in &refs {
        let mut line: Vec<u8> = Vec::new();
        // `%(align:…)…%(end)` buffers its content so it can be padded on close;
        // nested aligns stack, and the innermost buffer is the current target.
        let mut align_stack: Vec<(AlignSpec, Vec<u8>)> = Vec::new();
        for item in &items {
            match item {
                Item::AlignStart(spec) => align_stack.push((spec.clone(), Vec::new())),
                Item::End => {
                    // Balance is guaranteed by parse_format, so a pop always succeeds.
                    let (spec, buf) = align_stack.pop().expect("balanced by parse");
                    let padded = pad_align(&buf, &spec);
                    let target = align_stack
                        .last_mut()
                        .map(|(_, b)| b)
                        .unwrap_or(&mut line);
                    target.extend_from_slice(&padded);
                }
                Item::Lit(bytes) => {
                    let target = align_stack
                        .last_mut()
                        .map(|(_, b)| b)
                        .unwrap_or(&mut line);
                    target.extend_from_slice(bytes);
                }
                Item::Atom(atom) => {
                    let value = render(&repo, atom, info)?;
                    // Colour escapes are emitted verbatim; git does not quote them.
                    let rendered = if matches!(atom.field, Field::Color(_)) {
                        value
                    } else {
                        match quote_style {
                            QuoteStyle::None => value,
                            QuoteStyle::Shell => sq_quote(&value),
                            QuoteStyle::Perl => perl_quote(&value),
                            QuoteStyle::Python => python_quote(&value),
                            QuoteStyle::Tcl => tcl_quote(&value),
                        }
                    };
                    let target = align_stack
                        .last_mut()
                        .map(|(_, b)| b)
                        .unwrap_or(&mut line);
                    target.extend_from_slice(&rendered);
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
    let mut align_depth = 0usize;

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
                // `%(align:…)` / `%(end)` are containers handled here rather than
                // as value atoms; everything else is a normal atom.
                if spec == "end" {
                    if align_depth == 0 {
                        return Err(fatal_atom(
                            "format: %(end) atom used without corresponding atom",
                        ));
                    }
                    align_depth -= 1;
                    items.push(Item::End);
                } else if spec == "align" || spec.starts_with("align:") {
                    let opts = spec.strip_prefix("align:");
                    items.push(Item::AlignStart(parse_align(opts)?));
                    align_depth += 1;
                } else {
                    items.push(Item::Atom(parse_atom(spec, color_on)?));
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
    if align_depth != 0 {
        return Err(fatal_atom("format: %(end) atom missing"));
    }
    Ok(items)
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
    out.extend(std::iter::repeat(b' ').take(right));
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
    prereleases: &[Vec<u8>],
) -> Result<Vec<RefInfo>> {
    // Precompute each ref's key values: rendering can fail, and a comparator
    // cannot propagate errors.
    let mut rows: Vec<(Vec<Key>, RefInfo)> = Vec::with_capacity(refs.len());
    for info in refs {
        let mut keys = Vec::with_capacity(sorts.len());
        for s in sorts {
            keys.push(key_of(repo, s, &info)?);
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
fn key_of(repo: &gix::Repository, key: &SortKey, info: &RefInfo) -> Result<Key> {
    let atom = &key.atom;
    // A version-sorted key always compares the atom's rendered string, matching
    // git's `versioncmp(va->s, vb->s)` even for otherwise-numeric atoms.
    if key.versioned {
        return Ok(Key::Str(render(repo, atom, info)?));
    }
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

/// Compare two sort values with `versioncmp`; both are strings for a version key.
fn versioncmp_key(a: &Key, b: &Key, prereleases: &[Vec<u8>]) -> std::cmp::Ordering {
    match (a, b) {
        (Key::Str(a), Key::Str(b)) => versioncmp(a, b, prereleases),
        _ => std::cmp::Ordering::Equal,
    }
}

/// The prerelease-suffix ordering `versioncmp` consults, read the way git's
/// `versioncmp` reads it: `versionsort.suffix` wins over the deprecated
/// `versionsort.prereleasesuffix`, and setting both warns.
fn read_prereleases(repo: &gix::Repository) -> Vec<Vec<u8>> {
    let snapshot = repo.config_snapshot();
    let config = snapshot.plumbing();
    let newl = config.strings("versionsort.suffix");
    let oldl = config.strings("versionsort.prereleasesuffix");
    match (newl, oldl) {
        (Some(new), Some(_)) => {
            eprintln!(
                "warning: ignoring versionsort.prereleasesuffix because \
                 versionsort.suffix is set"
            );
            new.into_iter().map(|s| s.to_vec()).collect()
        }
        (Some(new), None) => new.into_iter().map(|s| s.to_vec()).collect(),
        (None, Some(old)) => old.into_iter().map(|s| s.to_vec()).collect(),
        (None, None) => Vec::new(),
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

/// A partial match of a configured prerelease suffix within a version string.
struct SuffixMatch {
    conf_pos: i32,
    start: i32,
    len: i32,
}

/// git's `find_better_matching_suffix`: try to improve `match` with an earlier
/// (or same-offset-but-longer) placement of `suffix` in `tagname`.
fn find_better_matching_suffix(
    tagname: &[u8],
    suffix: &[u8],
    conf_pos: i32,
    start: i32,
    m: &mut SuffixMatch,
) {
    let suffix_len = suffix.len() as i32;
    // A better match either starts earlier or starts at the same offset but is
    // longer.
    let end = if m.len < suffix_len { m.start } else { m.start - 1 };
    let mut i = start;
    while i <= end {
        let at = i as usize;
        if at <= tagname.len() && tagname[at..].starts_with(suffix) {
            m.conf_pos = conf_pos;
            m.start = i;
            m.len = suffix_len;
            break;
        }
        i += 1;
    }
}

/// git's `swap_prereleases`: when a configured prerelease suffix straddles the
/// first differing offset `off`, force the string carrying the earlier-ranked
/// suffix to sort on top. Returns `Some(diff)` when it decides the order.
fn swap_prereleases(
    s1: &[u8],
    s2: &[u8],
    off: i32,
    prereleases: &[Vec<u8>],
) -> Option<i32> {
    let mut match1 = SuffixMatch {
        conf_pos: -1,
        start: off,
        len: -1,
    };
    let mut match2 = SuffixMatch {
        conf_pos: -1,
        start: off,
        len: -1,
    };

    for (i, suffix) in prereleases.iter().enumerate() {
        let suffix_len = suffix.len() as i32;
        let start = if suffix_len < off {
            off - suffix_len
        } else {
            0
        };
        find_better_matching_suffix(s1, suffix, i as i32, start, &mut match1);
        find_better_matching_suffix(s2, suffix, i as i32, start, &mut match2);
    }
    if match1.conf_pos == -1 && match2.conf_pos == -1 {
        return None;
    }
    if match1.conf_pos == match2.conf_pos {
        // The same suffix in both (e.g. "-rc" in "v1.0-rcX" and "v1.0-rcY"):
        // let the caller decide from what follows.
        return None;
    }
    let diff = if match1.conf_pos >= 0 && match2.conf_pos >= 0 {
        match1.conf_pos - match2.conf_pos
    } else if match1.conf_pos >= 0 {
        -1
    } else {
        1
    };
    Some(diff)
}

/// git's `versioncmp` (glibc `strverscmp` plus git's prerelease-suffix rule):
/// compare two byte strings as version numbers.
fn versioncmp(s1: &[u8], s2: &[u8], prereleases: &[Vec<u8>]) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    // States S_N=0, S_I=3, S_F=6, S_Z=9; columns x=0 (other), d=1 (1-9), 0=2.
    const NEXT_STATE: [u8; 12] = [
        /* S_N */ 0, 3, 9, //
        /* S_I */ 0, 3, 3, //
        /* S_F */ 0, 6, 6, //
        /* S_Z */ 0, 6, 9, //
    ];
    // CMP=2, LEN=3; every other cell is the literal result (-1 or +1).
    const RESULT_TYPE: [i8; 36] = [
        /* S_N */ 2, 2, 2, 2, 3, 2, 2, 2, 2, //
        /* S_I */ 2, -1, -1, 1, 3, 3, 1, 3, 3, //
        /* S_F */ 2, 2, 2, 2, 2, 2, 2, 2, 2, //
        /* S_Z */ 2, 1, 1, -1, 2, 2, -1, 2, 2, //
    ];

    // git operates on NUL-terminated strings; reads past the end return 0.
    let byte = |s: &[u8], i: usize| -> u8 { s.get(i).copied().unwrap_or(0) };
    let col = |c: u8| -> usize { (c == b'0') as usize + (c.is_ascii_digit() as usize) };

    let mut i1 = 0usize;
    let mut i2 = 0usize;
    let mut c1 = byte(s1, i1);
    i1 += 1;
    let mut c2 = byte(s2, i2);
    i2 += 1;
    // Hint: '0' is a digit too.
    let mut state = col(c1);

    let mut diff = c1 as i32 - c2 as i32;
    while diff == 0 {
        if c1 == 0 {
            return Ordering::Equal;
        }
        state = NEXT_STATE[state] as usize;
        c1 = byte(s1, i1);
        i1 += 1;
        c2 = byte(s2, i2);
        i2 += 1;
        state += col(c1);
        diff = c1 as i32 - c2 as i32;
    }

    // A configured prerelease suffix straddling the first difference can flip
    // the order outright.
    if !prereleases.is_empty() {
        if let Some(d) = swap_prereleases(s1, s2, (i1 - 1) as i32, prereleases) {
            return d.cmp(&0);
        }
    }

    match RESULT_TYPE[state * 3 + col(c2)] {
        2 => diff.cmp(&0), // CMP
        3 => {
            // LEN: the longer run of leading digits is the larger number.
            loop {
                let a = byte(s1, i1);
                i1 += 1;
                if !a.is_ascii_digit() {
                    break;
                }
                let b = byte(s2, i2);
                i2 += 1;
                if !b.is_ascii_digit() {
                    return Ordering::Greater;
                }
            }
            if byte(s2, i2).is_ascii_digit() {
                Ordering::Less
            } else {
                diff.cmp(&0)
            }
        }
        d => (d as i32).cmp(&0),
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

/// git's `is_root_ref`: which files directly under `$GIT_DIR` count as root
/// refs for `--include-root-refs`.
///
/// The name must be `is_root_ref_syntax` (uppercase letters, `-`, `_`), must not
/// be one of the special multi-valued refs the ref backend never iterates
/// (`FETCH_HEAD`, `MERGE_HEAD`), and must then be `HEAD`, end with `_HEAD`, or
/// be one of the irregular pseudorefs git lists explicitly.
fn is_root_ref(name: &[u8]) -> bool {
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
