//! `git for-each-ref` — iterate refs and render them through a `--format` template.
//!
//! Covered, byte-identically with stock git: ref discovery and default
//! `refname`-ordered listing, the `<pattern>` / `--exclude` matching rules
//! (literal path-prefix *or* `wildmatch` with pathname semantics), `--count`,
//! `--sort` (repeatable, `-` for descending, last key primary), `--points-at`,
//! `--omit-empty`, `--ignore-case`, `--shell` quoting, and the format language's
//! `%%` / `%xx` escapes plus the `%(...)` atoms listed on [`parse_atom`].
//!
//! Not covered — each rejected with a precise message rather than silently
//! producing divergent output: `--stdin`, `--include-root-refs`,
//! `--start-after`, `--color`, `--merged`/`--no-merged`/`--contains`/
//! `--no-contains`, the `--perl`/`--python`/`--tcl` quoting modes, and the
//! atoms that need substrate this module does not build (`%(upstream)`,
//! `%(push)`, `%(align)`, `%(if)`, `%(color)`, `%(describe)`, `%(worktreepath)`,
//! `%(trailers)`, `%(signature)`, `%(raw)`, `%(deltabase)`, `%(ahead-behind)`,
//! `%(is-base)`, `%(symref)`, `%(objectsize:disk)`, `version:`/`v:` sort keys).
//!
//! One known divergence: `%(objectname:short)` takes its length from gitoxide's
//! abbreviation logic, which honours `core.abbrev` but, when it is unset,
//! auto-scales off the packed-object count alone where git also counts loose
//! objects. Set `core.abbrev`, or use `:short=<n>`, for an exact match.

use anyhow::{anyhow, bail, Result};
use std::collections::HashSet;
use std::io::Write;
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
    ObjectName(NameLen),
    ObjectType,
    ObjectSize,
    Head,
    Person(Who, PersonPart),
    Contents(ContentPart),
}

/// Modifiers accepted by `%(refname)`.
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
    obj: ObjInfo,
    /// Present only when `obj` is a tag object, holding its fully peeled target.
    peeled: Option<ObjInfo>,
    is_head: bool,
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

    let mut format: Vec<u8> = b"%(objectname) %(objecttype)\t%(refname)".to_vec();
    let mut count: Option<usize> = None;
    let mut sort_specs: Vec<String> = Vec::new();
    let mut patterns: Vec<String> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut points_at: Option<String> = None;
    let mut omit_empty = false;
    let mut ignore_case = false;
    let mut shell_quote = false;

    // Pull the value of `--opt=<v>` or the following argument of `--opt <v>`.
    let mut i = 0;
    let take_value = |i: &mut usize, rest: Option<&str>, name: &str| -> Result<String> {
        match rest {
            Some(v) => Ok(v.to_string()),
            None => {
                *i += 1;
                args.get(*i)
                    .cloned()
                    .ok_or_else(|| anyhow!("option `{name}` requires a value"))
            }
        }
    };

    while i < args.len() {
        let a = args[i].as_str();
        let (name, rest) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (a, None),
        };
        match name {
            "--format" => format = take_value(&mut i, rest, "--format")?.into_bytes(),
            "--count" => {
                let v = take_value(&mut i, rest, "--count")?;
                count = Some(
                    v.parse::<usize>()
                        .map_err(|_| anyhow!("invalid count `{v}` for `--count`"))?,
                );
            }
            "--sort" => sort_specs.push(take_value(&mut i, rest, "--sort")?),
            "--exclude" => excludes.push(take_value(&mut i, rest, "--exclude")?),
            "--points-at" => points_at = Some(take_value(&mut i, rest, "--points-at")?),
            "--omit-empty" => omit_empty = true,
            "--ignore-case" => ignore_case = true,
            "--shell" => shell_quote = true,
            "--perl" | "--python" | "--tcl" => {
                bail!("unsupported flag {name:?} (ported quoting mode: --shell)")
            }
            "--stdin"
            | "--include-root-refs"
            | "--start-after"
            | "--color"
            | "--merged"
            | "--no-merged"
            | "--contains"
            | "--no-contains" => {
                bail!(
                    "unsupported flag {name:?} (ported: --format, --count, --sort, --exclude, \
                     --points-at, --omit-empty, --ignore-case, --shell)"
                )
            }
            s if s.starts_with('-') && s.len() > 1 => bail!("unknown option {s:?}"),
            s => patterns.push(s.to_string()),
        }
        i += 1;
    }

    let items = parse_format(&format)?;
    let mut sorts: Vec<SortKey> = Vec::new();
    for spec in &sort_specs {
        let (spec, descending) = match spec.strip_prefix('-') {
            Some(r) => (r, true),
            None => (spec.as_str(), false),
        };
        if spec.starts_with("version:") || spec.starts_with("v:") {
            bail!("unsupported sort key {spec:?} (version sorting is not ported)");
        }
        sorts.push(SortKey {
            atom: parse_atom(spec)?,
            descending,
        });
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

    let repo = gix::discover(".")?;
    let head_name = repo.head_name()?.map(|n| n.as_bstr().to_vec());
    let points_at = match &points_at {
        Some(spec) => Some(repo.rev_parse_single(spec.as_str())?.detach()),
        None => None,
    };

    // Materialise every ref name first: the iterator holds the packed-refs
    // buffer, which would block the per-ref object lookups below.
    let mut names: Vec<Vec<u8>> = Vec::new();
    for r in repo.references()?.all()? {
        let r = r.map_err(|e| anyhow!("{e}"))?;
        names.push(r.name().as_bstr().to_vec());
    }
    // The `:short` disambiguation rules test candidate names against every ref.
    let all_names: HashSet<Vec<u8>> = names.iter().cloned().collect();

    let mut refs: Vec<RefInfo> = Vec::new();
    for refname in names {
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
        let id = reference.follow_to_object()?.detach();

        // The chain of tag targets, so `--points-at` and `*`-atoms agree with
        // git. Skipped entirely when nothing needs it, as peeling reads objects.
        let chain = if points_at.is_some() || needs_peel {
            peel_chain(&repo, id)?
        } else {
            Vec::new()
        };
        if let Some(target) = points_at {
            if id != target && !chain.contains(&target) {
                continue;
            }
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

        refs.push(RefInfo {
            is_head: head_name.as_deref() == Some(refname.as_slice()),
            refname,
            short,
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
                    if shell_quote {
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

/// Split a format string into literal runs and atoms, expanding `%%` and `%xx`.
fn parse_format(fmt: &[u8]) -> Result<Vec<Item>> {
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
                let end = start
                    + fmt[start..]
                        .iter()
                        .position(|&b| b == b')')
                        .ok_or_else(|| anyhow!("format string is missing a closing `)`"))?;
                let spec = std::str::from_utf8(&fmt[start..end])
                    .map_err(|_| anyhow!("format atom is not valid utf-8"))?;
                if !lit.is_empty() {
                    items.push(Item::Lit(std::mem::take(&mut lit)));
                }
                items.push(Item::Atom(parse_atom(spec)?));
                i = end + 1;
            }
            _ => {
                let hex = fmt
                    .get(i + 1..i + 3)
                    .filter(|h| h.iter().all(u8::is_ascii_hexdigit));
                let Some(hex) = hex else {
                    bail!("unrecognized format directive at byte {i}");
                };
                // Both bytes are ASCII hex digits, so neither conversion can fail.
                let s = std::str::from_utf8(hex).expect("ascii");
                lit.push(u8::from_str_radix(s, 16).expect("two hex digits"));
                i += 3;
            }
        }
    }
    if !lit.is_empty() {
        items.push(Item::Lit(lit));
    }
    Ok(items)
}

/// Parse one atom body (the text between `%(` and `)`), also used for sort keys.
///
/// Understood: `refname[:short|:lstrip=<n>|:strip=<n>|:rstrip=<n>]`,
/// `objectname[:short[=<n>]]`, `objecttype`, `objectsize`, `HEAD`,
/// `author`/`committer`/`tagger`/`creator` and their `name`, `email`
/// (`:trim`, `:localpart`) and `date` (`:short`, `:iso8601`, `:iso8601-strict`,
/// `:rfc2822`, `:unix`, `:raw`, `:default`) forms, `subject`, `body`, and
/// `contents[:subject|:body|:size]`. A leading `*` evaluates the atom against
/// the object a tag peels to.
fn parse_atom(spec: &str) -> Result<Atom> {
    let (body, deref) = match spec.strip_prefix('*') {
        Some(rest) => (rest, true),
        None => (spec, false),
    };
    let (name, m) = match body.split_once(':') {
        Some((n, m)) => (n, Some(m)),
        None => (body, None),
    };

    // Reject a modifier on an atom that takes none, naming the offending atom.
    let bare = |m: Option<&str>| -> Result<()> {
        match m {
            None => Ok(()),
            Some(m) => bail!("unsupported modifier `:{m}` on %({name})"),
        }
    };

    let field = match name {
        "refname" => Field::RefName(match m {
            None => NameMod::Full,
            Some("short") => NameMod::Short,
            Some(m) => {
                if let Some(n) = m
                    .strip_prefix("lstrip=")
                    .or_else(|| m.strip_prefix("strip="))
                {
                    NameMod::LStrip(parse_i64(n, spec)?)
                } else if let Some(n) = m.strip_prefix("rstrip=") {
                    NameMod::RStrip(parse_i64(n, spec)?)
                } else {
                    bail!("unsupported modifier `:{m}` on %(refname)")
                }
            }
        }),
        "objectname" => Field::ObjectName(match m {
            None => NameLen::Full,
            Some("short") => NameLen::Auto,
            Some(m) => match m.strip_prefix("short=") {
                Some(n) => NameLen::Fixed(
                    n.parse::<usize>()
                        .map_err(|_| anyhow!("invalid length in %({spec})"))?,
                ),
                None => bail!("unsupported modifier `:{m}` on %(objectname)"),
            },
        }),
        "objecttype" => {
            bare(m)?;
            Field::ObjectType
        }
        "objectsize" => {
            bare(m)?;
            Field::ObjectSize
        }
        "HEAD" => {
            bare(m)?;
            Field::Head
        }
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
                Some(m) => bail!("unsupported modifier `:{m}` on %({name})"),
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
                Some(m) => bail!(
                    "unsupported date format `:{m}` (ported: default, short, iso8601, \
                     iso8601-strict, rfc2822, unix, raw)"
                ),
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
            Some(m) => bail!("unsupported modifier `:{m}` on %(contents)"),
        }),
        _ => bail!(
            "unsupported field name {name:?} (ported: refname, objectname, objecttype, \
             objectsize, HEAD, author*, committer*, tagger*, creator*, subject, body, contents)"
        ),
    };

    if deref && matches!(field, Field::RefName(_) | Field::Head) {
        bail!("`*` has no meaning on %({name})");
    }
    Ok(Atom { deref, field })
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

/// Parse a possibly negative strip count, blaming the whole atom on failure.
fn parse_i64(s: &str, spec: &str) -> Result<i64> {
    s.parse::<i64>()
        .map_err(|_| anyhow!("invalid strip count in %({spec})"))
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
        Field::RefName(_) | Field::Head => unreachable!("handled above"),
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
