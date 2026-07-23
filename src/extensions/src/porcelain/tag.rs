//! `git tag` — list, create (lightweight and annotated), delete, and filter tags.
//!
//! Served natively via the vendored gitoxide crates so tools on PATH observe
//! the same ref store. Implemented forms (matching stock `git tag`):
//!
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
//!                                       → ancestry / points-at listing filters.
//!   * `git tag -i|--ignore-case`      → case-insensitive match and sort.
//!   * `git tag --omit-empty`          → drop refs whose `--format` output is empty.
//!   * `git tag <name> [<commit>]`     → create a lightweight tag at `<commit>`.
//!   * `git tag -a|-m|-F …`            → create an annotated tag object.
//!   * `git tag --cleanup=<mode>`      → `verbatim`/`whitespace`/`strip` message
//!                                       cleanup for `-m`/`-F`.
//!   * `git tag -f …`                  → force, printing the `Updated tag` line.
//!   * `git tag -d <name>…`            → delete each tag.
//!
//! Exit codes follow git: fatal errors exit 128, a bad object name for a filter
//! exits 129, and a failed delete exits 1.
//!
//! Genuinely not backed here, and refused rather than faked: cryptographic
//! signing (`-s`, `-u`) and verification (`-v`), an editor-supplied message (`-a`
//! with neither `-m` nor `-F`, `-e`), forced ANSI color (`--color`/`--color=always`),
//! `--column`, `--create-reflog`, the git gecos identity fallback, and `--format`
//! atoms outside the set handled by [`render_atom`] (`align`, `describe`,
//! `upstream`, relative/custom dates, …).

use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::glob::wildmatch;
use gix::glob::wildmatch::Mode;
use gix::hash::ObjectId;
use gix::objs::{CommitRef, Kind, TagRef};
use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};
use gix::refs::FullName;

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
    let mut want_color = false;
    let mut lines: Option<usize> = None;
    let mut sorts: Vec<String> = Vec::new();
    let mut format: Option<String> = None;
    let mut cleanup: Option<String> = None;
    let mut messages: Vec<Vec<u8>> = Vec::new();
    let mut message_file: Option<String> = None;
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
        let a = args[i].as_str();
        i += 1;
        if operands_only || !a.starts_with('-') || a == "-" {
            positionals.push(a);
            continue;
        }
        match a {
            "--" => operands_only = true,
            "-d" | "--delete" => delete = true,
            "-l" | "--list" => list = true,
            "-f" | "--force" => force = true,
            "-a" | "--annotate" => annotate = true,
            "-i" | "--ignore-case" => ignore_case = true,
            "--omit-empty" => omit_empty = true,
            "--color" => want_color = true,
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
                } else if let Some(rest) = a.strip_prefix("--color=") {
                    match rest {
                        "never" | "auto" => want_color = false,
                        _ => want_color = true,
                    }
                } else if let Some(rest) = a.strip_prefix("--points-at=") {
                    points_at = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--contains=") {
                    contains = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--no-contains=") {
                    no_contains = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--merged=") {
                    merged = Some(rest.to_string());
                } else if let Some(rest) = a.strip_prefix("--no-merged=") {
                    no_merged = Some(rest.to_string());
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
    if sorts.is_empty() {
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
        bail!("forced ANSI color (--color) is not supported")
    }

    let repo = gix::discover(".")?;

    if delete {
        return delete_tags(&repo, &positionals);
    }

    // Resolve the listing filters now that the object database is open.
    let mut filters = Filters::default();
    if let Some(spec) = &points_at {
        match resolve_object(&repo, spec) {
            Some(id) => filters.points_at = Some(id),
            None => return malformed(spec),
        }
    }
    for (raw, slot) in [
        (&contains, &mut filters.contains),
        (&no_contains, &mut filters.no_contains),
        (&merged, &mut filters.merged),
        (&no_merged, &mut filters.no_merged),
    ] {
        if let Some(spec) = raw {
            match resolve_commit(&repo, spec) {
                Some(id) => *slot = Some(id),
                None => return malformed(spec),
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
        );
    }

    let annotate = annotate || !messages.is_empty() || message_file.is_some();
    create_tag(
        &repo,
        &positionals,
        force,
        annotate,
        &messages,
        message_file.as_deref(),
        cleanup.as_deref(),
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

/// git's `ref-filter.c` `valid_atom[]` field names. Membership is only used to
/// tell git-rejects-it apart from git-accepts-it while validating `--sort`.
const VALID_SORT_ATOMS: &[&str] = &[
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
    "describe",
];

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
    fn cmp(&self, other: &SortVal) -> Ordering {
        match (self, other) {
            (SortVal::Num(a), SortVal::Num(b)) => a.cmp(b),
            (SortVal::Bytes(a), SortVal::Bytes(b)) => a.cmp(b),
            (SortVal::Version(a), SortVal::Version(b)) => versioncmp(a, b),
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

/// Split a `--sort` key into its `-` (descending), `version:`/`v:` and `*`
/// (dereference) markers and the remaining field atom.
fn parse_sort_key(key: &str) -> (bool, bool, bool, &str) {
    let mut s = key;
    let mut reverse = false;
    if let Some(rest) = s.strip_prefix('-') {
        reverse = true;
        s = rest;
    }
    let mut version = false;
    if let Some(rest) = s.strip_prefix("version:").or_else(|| s.strip_prefix("v:")) {
        version = true;
        s = rest;
    }
    let mut star = false;
    if let Some(rest) = s.strip_prefix('*') {
        star = true;
        s = rest;
    }
    (reverse, version, star, s)
}

/// git's `parse_ref_filter_atom`: an empty atom is a `malformed field name`, and
/// a field name outside `valid_atom[]` is an `unknown field name`.
fn git_sort_error(key: &str) -> Option<String> {
    let (_, _, _, atom) = parse_sort_key(key);
    if atom.is_empty() {
        return Some(format!("malformed field name: {atom}"));
    }
    let name = atom.split(':').next().unwrap_or(atom);
    if !VALID_SORT_ATOMS.contains(&name) {
        return Some(format!("unknown field name: {atom}"));
    }
    None
}

/// Validate and interpret every `--sort` key. git dies on the first syntactically
/// invalid key in the order given, so that is checked first; only then is this
/// port's narrower support considered.
fn resolve_sort(sorts: &[String]) -> Result<Vec<SortKey>, SortErr> {
    for key in sorts {
        if let Some(msg) = git_sort_error(key) {
            return Err(SortErr::Fatal(msg));
        }
    }
    let mut keys = Vec::with_capacity(sorts.len());
    for key in sorts {
        let (reverse, version, star, atom) = parse_sort_key(key);
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

/// git's `versioncmp` (a modified glibc `strverscmp`), byte for byte.
fn versioncmp(s1: &[u8], s2: &[u8]) -> Ordering {
    const S_N: usize = 0;
    const S_I: usize = 3;
    const S_F: usize = 6;
    const S_Z: usize = 9;
    const CMP: i8 = 2;
    const LEN: i8 = 3;
    #[rustfmt::skip]
    const NEXT_STATE: [usize; 12] = [
        S_N, S_I, S_Z,
        S_N, S_I, S_I,
        S_N, S_F, S_F,
        S_N, S_F, S_Z,
    ];
    #[rustfmt::skip]
    const RESULT_TYPE: [i8; 36] = [
        CMP, CMP, CMP, CMP, LEN, CMP, CMP, CMP, CMP,
        CMP, -1,  -1,   1,  LEN, LEN,  1,  LEN, LEN,
        CMP, CMP, CMP, CMP, CMP, CMP, CMP, CMP, CMP,
        CMP,  1,   1,  -1,  CMP, CMP, -1,  CMP, CMP,
    ];

    let get = |p: &[u8], i: usize| -> u8 { p.get(i).copied().unwrap_or(0) };
    let digit = |c: u8| -> usize { usize::from(c.is_ascii_digit()) };
    let zero = |c: u8| -> usize { usize::from(c == b'0') };

    let mut i1 = 0usize;
    let mut i2 = 0usize;
    let mut c1 = get(s1, i1);
    i1 += 1;
    let mut c2 = get(s2, i2);
    i2 += 1;
    let mut state = S_N + zero(c1) + digit(c1);
    let mut diff;
    loop {
        diff = c1 as i32 - c2 as i32;
        if diff != 0 {
            break;
        }
        if c1 == 0 {
            return Ordering::Equal;
        }
        state = NEXT_STATE[state];
        c1 = get(s1, i1);
        i1 += 1;
        c2 = get(s2, i2);
        i2 += 1;
        state += zero(c1) + digit(c1);
    }

    let rt = RESULT_TYPE[state * 3 + zero(c2) + digit(c2)];
    match rt {
        CMP => diff.cmp(&0),
        LEN => {
            loop {
                let d1 = get(s1, i1);
                i1 += 1;
                if !d1.is_ascii_digit() {
                    break;
                }
                let d2 = get(s2, i2);
                i2 += 1;
                if !d2.is_ascii_digit() {
                    return Ordering::Greater;
                }
            }
            if get(s2, i2).is_ascii_digit() {
                Ordering::Less
            } else {
                diff.cmp(&0)
            }
        }
        other => (other as i32).cmp(&0),
    }
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
    entries.sort_by(|a, b| {
        for (idx, key) in sort_keys.iter().enumerate().rev() {
            let mut ord = a.keys[idx].cmp(&b.keys[idx]);
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
        line.push(b'\n');
        out.write_all(&line)?;
    }
    Ok(ExitCode::SUCCESS)
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

/// Resolve a filter operand to an object id, or `None` if it is not a valid name.
fn resolve_object(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    repo.rev_parse_single(BStr::new(spec))
        .ok()
        .map(|o| o.detach())
}

/// Resolve a filter operand to the commit it names (peeling tags), or `None`.
fn resolve_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let id = repo.rev_parse_single(BStr::new(spec)).ok()?.detach();
    let peeled = repo.find_object(id).ok()?.peel_tags_to_end().ok()?;
    (peeled.kind == Kind::Commit).then_some(peeled.id)
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
                    None => bail!("unsupported '%' escape in --format"),
                }
            }
        }
    }
    if !frames.is_empty() {
        bail!("format string has an unclosed '%(if)'");
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
                    out.extend_from_slice(&strip_components(&e.full, parse_i64(atom, n)?, true));
                } else if let Some(n) = a.strip_prefix("rstrip=") {
                    out.extend_from_slice(&strip_components(&e.full, parse_i64(atom, n)?, false));
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

/// `%(refname:lstrip=<n>)` / `%(refname:rstrip=<n>)`.
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

/// Create a lightweight or annotated tag `<name>` at `[<commit>]` (default `HEAD`).
fn create_tag(
    repo: &gix::Repository,
    positionals: &[&str],
    force: bool,
    annotate: bool,
    messages: &[Vec<u8>],
    message_file: Option<&str>,
    cleanup: Option<&str>,
) -> Result<ExitCode> {
    if positionals.len() > 2 {
        return fatal("too many arguments");
    }
    let name = positionals[0];
    let spec = positionals.get(1).copied().unwrap_or("HEAD");

    let Ok(target) = repo.rev_parse_single(BStr::new(spec)) else {
        return fatal(&format!("Failed to resolve '{spec}' as a valid ref."));
    };
    let target = target.detach();

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

        let object = gix::objs::Tag {
            target,
            target_kind: repo.find_header(target)?.kind(),
            name: BString::from(name.as_bytes().to_vec()),
            tagger: Some(tagger),
            message: BString::from(message),
            pgp_signature: None,
        };
        let id = repo.write_object(&object)?.detach();
        repo.tag_reference(name, id, constraint)?;
        id
    } else {
        repo.tag_reference(name, target, constraint)?;
        target
    };

    if let Some(old) = prev {
        if old != new_id {
            println!("Updated tag '{name}' (was {})", short_hex(repo, old));
        }
    }
    Ok(ExitCode::SUCCESS)
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

/// Report a bad filter object name the way git does, at its exit code 129.
fn malformed(spec: &str) -> Result<ExitCode> {
    eprintln!("error: malformed object name {spec}");
    Ok(ExitCode::from(129))
}

/// Abbreviated hex for `id`, honoring the repo's shortening rules.
fn short_hex(repo: &gix::Repository, id: ObjectId) -> String {
    use gix::prelude::ObjectIdExt;
    id.attach(repo).shorten_or_id().to_string()
}
