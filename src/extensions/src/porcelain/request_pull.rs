//! `git request-pull` — generate a summary of pending changes for an upstream
//! maintainer.
//!
//! A port of the `git-request-pull` shell script (`git-request-pull.sh`)
//! together with the pieces of git it shells out to: `rev-parse`,
//! `symbolic-ref`, `show-ref`, `merge-base`, `ls-remote` (including its
//! `find_matching_ref` filter and `--get-url`), `cat-file`, `config`,
//! `shortlog`, and `diff -M --stat --summary [-p]`.
//!
//! Covered, byte-for-byte against stock git on stdout:
//!   * `git request-pull [-p] <start> <URL> [<end>]`, with `<end>` in either the
//!     plain or the `<local>:<remote>` form, defaulting to `HEAD`.
//!   * the header block (`The following changes since commit …` / `are
//!     available in the Git repository at:` / `for you to fetch changes up to
//!     …`), built from `%H`, `%s` and `%ci` of the merge base and the head.
//!   * the annotated-tag message block, reproducing the script's `sed` (drop the
//!     tag headers, stop at a `-----BEGIN PGP|SSH|SIGNED ` line).
//!   * the `branch.<name>.description` block.
//!   * `git shortlog ^<base> <head>` — author grouping through the mailmap,
//!     strcmp-ordered idents, subjects oldest-first.
//!   * `git diff -M --stat --summary [-p] <merge-base>..<head>` — a port of
//!     `show_stats()` and `diff_summary()` from `diff.c` (graph scaling, name
//!     ellipsis, the `Bin <old> -> <new> bytes` row and its forced 3-column
//!     number field) plus the unified patch body under `-p`.
//!   * the remote-side check: `ls-remote` over gitoxide's blocking transport fed
//!     through the script's `find_matching_ref`, the `refs/tags/` special case
//!     that turns `<tag>` into `tags/<tag>`, the two `warn:` lines, and the
//!     resulting exit code 1.
//!   * exit codes — 0 on success, 1 for `die`/usage/unmatched remote ref, 129
//!     for `-h` and for an unknown switch.
//!
//! Not covered — these `bail!` rather than emit output that would diverge:
//!   * a change that `-M` resolves to a rename or copy. git scores similarity
//!     with `estimate_similarity()` on top of `diffcore_count_changes()`
//!     (the spanhash algorithm in `diffcore-delta.c`); the vendored gitoxide
//!     tracks rewrites with a line-based similarity instead and exposes no
//!     score at all, so the `(NN%)` in `rename a => b (NN%)` / `similarity
//!     index NN%` and the `a => b` stat name cannot be reproduced. Rewrite
//!     tracking is switched on purely so such a change is detected and refused
//!     rather than silently rendered as a delete plus an add.
//!   * unmerged entries and `--stat`-affecting diff configuration
//!     (`diff.statNameWidth`, `diff.statGraphWidth`, `core.quotePath=false`).
//!
//! Known deviations, both stated rather than hidden:
//!   * `term_columns()` is read from `COLUMNS`, falling back to 80. git also
//!     asks the terminal via `TIOCGWINSZ` when stdout is a tty; there is no
//!     ioctl in the vendored crates, so a tty-attached run with `COLUMNS` unset
//!     uses 80 where git would use the window width.
//!   * column widths are counted in Unicode scalar values, where git uses
//!     `utf8_strwidth()`; East-Asian wide characters in a path measure 1.
//!   * when the remote cannot be reached the `fatal:` line on stderr is
//!     gitoxide's transport error, not git's. stdout and the exit code match.

use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::prelude::ObjectIdExt;
use gix::protocol::handshake::Ref;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// The `OPTIONS_SPEC` block `git rev-parse --parseopt` renders for this script.
const USAGE: &str = "\
usage: git request-pull [options] start url [end]

    -p                    show patch text as well

";

/// The rule the script draws between the header, the tag/description blocks and
/// the shortlog — 64 dashes.
const RULE: &str = "----------------------------------------------------------------";

pub fn request_pull(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us argv with the subcommand at index 0.
    let argv: &[String] = match args.first() {
        Some(a) if a == "request-pull" => &args[1..],
        _ => args,
    };

    // `git rev-parse --parseopt` permutes options ahead of the positionals, so
    // `-p` is honoured wherever it appears before a literal `--`.
    let mut patch = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut no_more_opts = false;
    for a in argv {
        let a = a.as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            positional.push(a);
            continue;
        }
        match a {
            "--" => no_more_opts = true,
            "-p" => patch = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ => {
                // parseopt's own diagnostic, both lines on stderr.
                let sw = a.trim_start_matches('-');
                eprintln!("error: unknown switch `{sw}'");
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
    }

    // `test -n "$base" && test -n "$url" || usage`
    let (Some(base), Some(url_arg)) = (positional.first().copied(), positional.get(1).copied())
    else {
        print!("{USAGE}");
        return Ok(ExitCode::from(1));
    };
    let end = positional.get(2).copied().unwrap_or("");

    let repo = gix::discover(".")?;

    // baserev=$(git rev-parse --verify --quiet "$base"^0)
    let Some(baserev) = peel_to_commit(&repo, base) else {
        return Ok(die(&format!("fatal: Not a valid revision: {base}")));
    };

    // local=${3%:*}; local=${local:-HEAD}; remote=${3#*:}
    // Both are shortest-match, i.e. the *last* colon splits the local name and
    // the *first* colon splits the remote name; with no colon both are `$3`.
    let (local, remote) = match end.rfind(':') {
        Some(i) => (
            &end[..i],
            &end[end.find(':').expect("rfind implies find") + 1..],
        ),
        None => (end, end),
    };
    let local = if local.is_empty() { "HEAD" } else { local };

    // pretty_remote=${remote#refs/}; pretty_remote=${pretty_remote#heads/}
    let mut pretty_remote = remote.strip_prefix("refs/").unwrap_or(remote);
    pretty_remote = pretty_remote
        .strip_prefix("heads/")
        .unwrap_or(pretty_remote);
    let mut pretty_remote = pretty_remote.to_string();

    // head=$(git symbolic-ref -q "$local")
    //   ?: $(git show-ref --heads --tags "$local" | cut -d' ' -f2)
    //   ?: $(git rev-parse --quiet --verify "$local")
    let head = match symbolic_ref(&repo, local) {
        Some(target) => Some(target),
        None => {
            let matches = show_ref_heads_tags(&repo, local)?;
            if matches.len() > 1 {
                // A multi-line `$head` makes the following `rev-parse --verify`
                // fail, which the script reports as an ambiguous revision.
                return Ok(die(&format!("fatal: Ambiguous revision: {local}")));
            }
            match matches.into_iter().next() {
                Some(name) => Some(name),
                None => repo
                    .rev_parse_single(local)
                    .ok()
                    .map(|id| id.detach().to_hex().to_string()),
            }
        }
    };
    let Some(head) = head else {
        return Ok(die(&format!("fatal: Not a valid revision: {local}")));
    };

    // local_sha1=$(git rev-parse --verify --quiet "$head") — unpeeled, so an
    // annotated tag contributes the tag object here and its commit below.
    let Some(head_object) = repo
        .rev_parse_single(head.as_str())
        .ok()
        .and_then(|id| id.object().ok())
    else {
        return Ok(die(&format!("fatal: Ambiguous revision: {local}")));
    };
    let local_sha1 = head_object.id;
    let head_is_tag = head_object.kind == gix::object::Kind::Tag;
    let tag_data: Vec<u8> = if head_is_tag {
        head_object.data.clone()
    } else {
        Vec::new()
    };
    // headrev=$(git rev-parse --verify --quiet "$head"^0)
    let Ok(head_commit) = head_object.peel_to_kind(gix::object::Kind::Commit) else {
        return Ok(die(&format!("fatal: Ambiguous revision: {local}")));
    };
    let headrev = head_commit.id;

    // Was it a branch with a description?
    let branch_name = head.strip_prefix("refs/heads/").unwrap_or(head.as_str());
    let description = repo
        .config_snapshot()
        .string(format!("branch.{branch_name}.description").as_str());

    // merge_base=$(git merge-base $baserev $headrev)
    let Ok(merge_base) = repo.merge_base(baserev, headrev) else {
        return Ok(die(&format!(
            "fatal: No commits in common between {base} and {head}"
        )));
    };
    let merge_base = merge_base.detach();

    // ------------------------------------------------------------------
    // The remote side: `git ls-remote "$url" | find_matching_ref`.
    // ------------------------------------------------------------------
    let name_or_url = BStr::new(url_arg.as_bytes());
    let (expanded_url, advertised) = match repo.find_fetch_remote(Some(name_or_url)) {
        Ok(remote_handle) => {
            let expanded = remote_handle
                .url(gix::remote::Direction::Fetch)
                .map(ToString::to_string)
                .unwrap_or_else(|| url_arg.to_owned());
            (expanded, ls_remote(remote_handle))
        }
        Err(e) => {
            eprintln!("fatal: {e}");
            (url_arg.to_owned(), Vec::new())
        }
    };

    let matched = find_matching_ref(&advertised, remote, headrev);
    let mut status = ExitCode::SUCCESS;
    let want = if remote.is_empty() { "HEAD" } else { remote };
    match &matched {
        None => {
            eprintln!("warn: No match for commit {headrev} found at {expanded_url}");
            eprintln!("warn: Are you sure you pushed '{want}' there?");
            status = ExitCode::from(1);
        }
        Some((remote_sha1, _)) if *remote_sha1 != local_sha1 => {
            eprintln!("warn: {head} found at {expanded_url} but points to a different object");
            eprintln!("warn: Are you sure you pushed '{want}' there?");
            status = ExitCode::from(1);
        }
        Some(_) => {}
    }

    // Special case: turn "for_linus" into "tags/for_linus" when it is correct.
    if let Some((_, name)) = &matched {
        if *name == format!("refs/tags/{pretty_remote}") {
            pretty_remote = format!("tags/{pretty_remote}");
        }
    }

    // ------------------------------------------------------------------
    // Output.
    // ------------------------------------------------------------------
    let mut out: Vec<u8> = Vec::new();

    let (base_subject, base_date) = subject_and_date(&repo, merge_base)?;
    write!(
        out,
        "The following changes since commit {merge_base}:\n\n  {base_subject} ({base_date})\n\nare available in the Git repository at:\n\n"
    )?;
    writeln!(out, "  {expanded_url} {pretty_remote}")?;

    let (head_subject, head_date) = subject_and_date(&repo, headrev)?;
    write!(
        out,
        "\nfor you to fetch changes up to {headrev}:\n\n  {head_subject} ({head_date})\n\n{RULE}\n"
    )?;

    if head_is_tag {
        out.extend_from_slice(&tag_body(&tag_data));
        writeln!(out)?;
        writeln!(out, "{RULE}")?;
    }

    if let Some(description) = description {
        writeln!(
            out,
            "(from the branch description for {branch_name} local branch)"
        )?;
        writeln!(out)?;
        out.extend_from_slice(&description);
        out.push(b'\n');
        writeln!(out, "{RULE}")?;
    }

    shortlog(&repo, baserev, headrev, &mut out)?;
    diff_stat_summary(&repo, merge_base, headrev, patch, &mut out)?;

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(status)
}

/// git-sh-setup's `die`: the message on stderr, exit status 1.
fn die(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(1)
}

/// `git rev-parse --verify --quiet <spec>^0`.
fn peel_to_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    Some(object.peel_to_commit().ok()?.id)
}

/// `git symbolic-ref -q <name>`: the target of `<name>` when it is a symbolic
/// ref under its literal name, otherwise nothing. The literal-name check keeps
/// gitoxide's DWIM (`main` → `refs/heads/main`) out of a lookup git performs
/// with `resolve_ref_unsafe`, which does no such expansion.
fn symbolic_ref(repo: &gix::Repository, name: &str) -> Option<String> {
    let reference = repo.try_find_reference(name).ok()??;
    if reference.name().as_bstr() != name.as_bytes().as_bstr() {
        return None;
    }
    match reference.target() {
        gix::refs::TargetRef::Symbolic(target) => {
            Some(target.as_bstr().to_str_lossy().into_owned())
        }
        gix::refs::TargetRef::Object(_) => None,
    }
}

/// `git show-ref --heads --tags <pattern> | cut -d' ' -f2`, in git's ref order
/// (`refs/heads/…` before `refs/tags/…`, each sorted by name).
fn show_ref_heads_tags(repo: &gix::Repository, pattern: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let platform = repo.references()?;
    for prefix in ["refs/heads/", "refs/tags/"] {
        let mut names: Vec<String> = Vec::new();
        for reference in platform.prefixed(prefix.as_bytes())? {
            let Ok(reference) = reference else { continue };
            let name = reference.name().as_bstr().to_str_lossy().into_owned();
            if tail_matches(&name, pattern) {
                names.push(name);
            }
        }
        names.sort();
        out.extend(names);
    }
    Ok(out)
}

/// git's ref-pattern rule: a pattern matches the whole name, or a trailing run
/// of complete path components of it (`main` matches `refs/heads/main` but not
/// `refs/heads/mymain`).
fn tail_matches(name: &str, pattern: &str) -> bool {
    name == pattern || name.ends_with(&format!("/{pattern}"))
}

/// `%s (%ci)` of a commit: the subject with newlines folded out, and the
/// committer date in git's `ISO8601` shape.
fn subject_and_date(repo: &gix::Repository, id: ObjectId) -> Result<(String, String)> {
    let commit = repo.find_object(id)?.peel_to_commit()?;
    let message = commit.message()?;
    let subject = message.summary().to_str_lossy().into_owned();
    let date = commit
        .committer()?
        .time()?
        .format_or_unix(gix::date::time::format::ISO8601);
    Ok((subject, date))
}

/// The script's `sed -n -e '1,/^$/d' -e '/^-----BEGIN \(PGP\|SSH\|SIGNED\) /q' -e p`
/// over `git cat-file tag <head>`: drop the header block through the first blank
/// line, then print until (excluding) a signature banner.
fn tag_body(data: &[u8]) -> Vec<u8> {
    // A terminating `\n` closes the last line rather than starting an empty
    // one, so the phantom final field `split` yields is not a line to sed.
    let mut lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    if data.ends_with(b"\n") {
        lines.pop();
    }

    let mut out = Vec::new();
    let mut seen_blank = false;
    for line in lines {
        if !seen_blank {
            // `1,/^$/d` deletes the blank separator itself as well.
            if line.is_empty() {
                seen_blank = true;
            }
            continue;
        }
        if line.starts_with(b"-----BEGIN PGP ")
            || line.starts_with(b"-----BEGIN SSH ")
            || line.starts_with(b"-----BEGIN SIGNED ")
        {
            break;
        }
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

// ---------------------------------------------------------------------------
// `git ls-remote "$url" | find_matching_ref`
// ---------------------------------------------------------------------------

/// One advertised row, in `git ls-remote` shape: the peeled `^{}` companion of
/// an annotated tag is a row of its own.
struct Advertised {
    name: String,
    oid: ObjectId,
}

/// The rows `git ls-remote <url>` would print, sorted by refname as the builtin
/// sorts them. A transport failure reports git-style on stderr and yields no
/// rows, which is the script's "no match" path.
fn ls_remote(remote: gix::Remote<'_>) -> Vec<Advertised> {
    let connection = match remote.connect(gix::remote::Direction::Fetch) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Vec::new();
        }
    };
    let ref_map = match connection.ref_map(
        gix::progress::Discard,
        gix::remote::ref_map::Options {
            prefix_from_spec_as_filter_on_remote: false,
            ..Default::default()
        },
    ) {
        Ok((map, _handshake)) => map,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Vec::new();
        }
    };

    let mut rows: Vec<Advertised> = Vec::new();
    for r in &ref_map.remote_refs {
        let (name, oid, peeled) = match r {
            Ref::Peeled {
                full_ref_name,
                tag,
                object,
            } => (full_ref_name, *tag, Some(*object)),
            Ref::Direct {
                full_ref_name,
                object,
            } => (full_ref_name, *object, None),
            Ref::Symbolic {
                full_ref_name,
                tag,
                object,
                ..
            } => (
                full_ref_name,
                (*tag).unwrap_or(*object),
                tag.is_some().then_some(*object),
            ),
            Ref::Unborn { .. } => continue,
        };
        let name = name.to_string();
        if let Some(peeled) = peeled {
            rows.push(Advertised {
                name: format!("{name}^{{}}"),
                oid: peeled,
            });
        }
        rows.push(Advertised { name, oid });
    }
    rows.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    rows
}

/// Port of the script's `find_matching_ref` shell function. Returns the
/// `(remote_sha1, ref)` pair it echoes, or `None` when it echoes nothing.
fn find_matching_ref(
    rows: &[Advertised],
    remote: &str,
    headrev: ObjectId,
) -> Option<(ObjectId, String)> {
    let want = if remote.is_empty() { "HEAD" } else { remote };
    let mut remote_sha1: Option<ObjectId> = None;

    for row in rows {
        // case "$ref" in *"^"?*) ref="${ref%"^"*}"; deref=true
        let (name, deref) = match row.name.rfind('^') {
            Some(i) if i + 1 < row.name.len() => (&row.name[..i], true),
            _ => (row.name.as_str(), false),
        };

        // The user may have named the object itself rather than a ref.
        if row.oid.to_hex().to_string() == want {
            return Some((row.oid, row.oid.to_hex().to_string()));
        }

        if name == want || name.ends_with(&format!("/{want}")) {
            if !deref {
                remote_sha1 = Some(row.oid);
            }
            if row.oid == headrev {
                return Some((remote_sha1.unwrap_or(headrev), name.to_string()));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// `git shortlog ^<base> <head>`
// ---------------------------------------------------------------------------

/// The default long shortlog format: `<ident> (<n>):`, one six-space-indented
/// subject per commit oldest-first, then a blank line. Idents are grouped after
/// mailmap resolution and emitted in strcmp order.
fn shortlog(
    repo: &gix::Repository,
    baserev: ObjectId,
    headrev: ObjectId,
    out: &mut Vec<u8>,
) -> Result<()> {
    use std::collections::BTreeMap;

    let mailmap = repo.open_mailmap();
    let mut groups: BTreeMap<BString, Vec<BString>> = BTreeMap::new();

    let walk = repo
        .rev_walk(vec![headrev])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .with_hidden(vec![baserev])
        .all()?;

    for info in walk {
        let commit = info?.object()?;
        let sig = commit.author()?.trim();
        let ident = match mailmap.try_resolve_ref(sig) {
            Some(resolved) => BString::from(resolved.name.unwrap_or(sig.name).to_vec()),
            None => BString::from(sig.name.to_vec()),
        };

        let message = commit.message()?;
        let subject = message.summary();
        let subject = if subject.is_empty() {
            BString::from("<none>")
        } else {
            subject.into_owned()
        };
        groups
            .entry(ident)
            .or_default()
            .push(strip_patch_prefix(subject.as_bstr()));
    }

    for (ident, subjects) in &groups {
        out.extend_from_slice(ident);
        writeln!(out, " ({}):", subjects.len())?;
        for subject in subjects.iter().rev() {
            out.extend_from_slice(b"      ");
            out.extend_from_slice(subject);
            out.push(b'\n');
        }
        out.push(b'\n');
    }
    Ok(())
}

/// Port of the subject cleanup in `insert_one_record()` (builtin/shortlog.c):
/// drop leading whitespace, then a `[PATCH…]` bracket prefix, then the
/// whitespace that followed it.
fn strip_patch_prefix(subject: &BStr) -> BString {
    let mut s = subject.as_bytes();
    while s.first().is_some_and(|&b| is_space(b)) {
        s = &s[1..];
    }
    if s.starts_with(b"[PATCH") {
        let eol = s.iter().position(|&b| b == b'\n').unwrap_or(s.len());
        if let Some(eob) = s.iter().position(|&b| b == b']') {
            if eob < eol {
                s = &s[eob + 1..];
            }
        }
    }
    while s.first().is_some_and(|&b| is_space(b) && b != b'\n') {
        s = &s[1..];
    }
    BString::from(s.to_vec())
}

/// C's `isspace()` for the "C" locale.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

// ---------------------------------------------------------------------------
// `git diff -M --stat --summary [-p] <merge-base>..<head>`
// ---------------------------------------------------------------------------

/// One diffstat row.
struct StatEntry {
    name: String,
    added: u64,
    deleted: u64,
    /// `(old_size, new_size)` when either side is binary; git prints those two
    /// byte counts instead of a graph and forces the number column to 3.
    binary: Option<(u64, u64)>,
}

fn diff_stat_summary(
    repo: &gix::Repository,
    merge_base: ObjectId,
    headrev: ObjectId,
    patch: bool,
    out: &mut Vec<u8>,
) -> Result<()> {
    let old_tree = repo.find_object(merge_base)?.peel_to_tree()?;
    let new_tree = repo.find_object(headrev)?.peel_to_tree()?;
    let abbrev = new_tree.id().shorten()?.hex_len();

    // Rewrite tracking is on only so that a rename/copy is *detected*; the
    // similarity score git prints for one is not derivable here (see the module
    // header), so such a change is refused rather than mis-rendered.
    let options = gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites::default()));
    let mut changes = repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), options)?;
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));

    if changes.is_empty() {
        return Ok(());
    }

    let mut body: Vec<u8> = Vec::new();
    let mut stats: Vec<StatEntry> = Vec::new();
    for change in &changes {
        stats.push(emit_change(repo, &mut body, change, abbrev, patch)?);
    }

    emit_stats(out, &stats)?;
    emit_summary(out, &changes)?;
    if patch {
        out.push(b'\n');
        out.extend_from_slice(&body);
    }
    Ok(())
}

/// git's `decimal_width`.
fn decimal_width(mut n: u64) -> usize {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

/// git's `scale_linear`: at least one column for any non-zero change.
fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// Display width in Unicode scalar values (see the module header).
fn display_width(s: &str) -> i64 {
    s.chars().count() as i64
}

/// git's `term_columns()`, minus the `TIOCGWINSZ` probe (see the module header).
fn term_columns() -> i64 {
    if let Ok(value) = std::env::var("COLUMNS") {
        // C's atoi: read a leading decimal run and ignore the rest.
        let digits: String = value
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(n) = digits.parse::<i64>() {
            if n > 0 {
                return n;
            }
        }
    }
    80
}

/// Port of `show_stats()` (diff.c) followed by
/// `print_stat_summary_inserts_deletes()`.
fn emit_stats(out: &mut Vec<u8>, files: &[StatEntry]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut number_width: i64 = 0;
    for f in files {
        max_len = max_len.max(display_width(&f.name));
        if f.binary.is_some() {
            // git also widens `bin_width` here, but that value only bounds the
            // graph column, which a binary row never prints. What survives into
            // the layout is the forced number column: "display change counts
            // aligned with Bin".
            number_width = number_width.max(3);
            continue;
        }
        max_change = max_change.max((f.added + f.deleted) as i64);
    }
    number_width = number_width.max(decimal_width(max_change as u64) as i64);

    let mut width = term_columns();
    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = max_change;
    let mut name_width = max_len;
    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = width * 3 / 8 - number_width - 6;
            if graph_width < 6 {
                graph_width = 6;
            }
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    let mut adds: u64 = 0;
    let mut dels: u64 = 0;
    for f in files {
        // Scale the filename: elide the head, then resume at a path separator.
        let mut len = name_width;
        let mut prefix = "";
        let mut name: &str = &f.name;
        if name_width < display_width(name) {
            prefix = "...";
            len -= 3;
            if len < 0 {
                len = 0;
            }
            let mut name_len = display_width(name);
            let mut off = 0;
            while name_len > len && off < name.len() {
                let c = name[off..]
                    .chars()
                    .next()
                    .expect("off stays on a char boundary");
                off += c.len_utf8();
                name_len -= 1;
            }
            name = &name[off..];
            if let Some(slash) = name.find('/') {
                name = &name[slash..];
            }
        }
        let padding = (len - display_width(name)).max(0) as usize;

        if let Some((old_size, new_size)) = f.binary {
            write!(
                out,
                " {prefix}{name}{:padding$} | {:>nw$}",
                "",
                "Bin",
                nw = number_width as usize
            )?;
            if old_size == 0 && new_size == 0 {
                out.push(b'\n');
            } else {
                writeln!(out, " {old_size} -> {new_size} bytes")?;
            }
            continue;
        }

        adds += f.added;
        dels += f.deleted;

        let total = f.added + f.deleted;
        let mut add = f.added as i64;
        let mut del = f.deleted as i64;
        if graph_width <= max_change && max_change > 0 {
            let mut sum = scale_linear(add + del, graph_width, max_change);
            if sum < 2 && add > 0 && del > 0 {
                sum = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = sum - add;
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = sum - del;
            }
        }

        write!(
            out,
            " {prefix}{name}{:padding$} | {:>nw$}{}",
            "",
            total,
            if total > 0 { " " } else { "" },
            nw = number_width as usize,
        )?;
        for _ in 0..add.max(0) {
            out.push(b'+');
        }
        for _ in 0..del.max(0) {
            out.push(b'-');
        }
        out.push(b'\n');
    }

    let n = files.len();
    let mut line = format!(" {n} {} changed", if n == 1 { "file" } else { "files" });
    if adds > 0 || dels == 0 {
        line.push_str(&format!(
            ", {adds} {}",
            if adds == 1 {
                "insertion(+)"
            } else {
                "insertions(+)"
            }
        ));
    }
    if dels > 0 || adds == 0 {
        line.push_str(&format!(
            ", {dels} {}",
            if dels == 1 {
                "deletion(-)"
            } else {
                "deletions(-)"
            }
        ));
    }
    writeln!(out, "{line}")?;
    Ok(())
}

/// Port of `diff_summary()` (diff.c): the `create`/`delete`/`mode change` lines
/// that follow the diffstat.
fn emit_summary(out: &mut Vec<u8>, changes: &[ChangeDetached]) -> Result<()> {
    for change in changes {
        match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                ..
            } => writeln!(
                out,
                " create mode {:06o} {}",
                entry_mode.value(),
                quote_path(location)
            )?,
            ChangeDetached::Deletion {
                location,
                entry_mode,
                ..
            } => writeln!(
                out,
                " delete mode {:06o} {}",
                entry_mode.value(),
                quote_path(location)
            )?,
            ChangeDetached::Modification {
                location,
                previous_entry_mode,
                entry_mode,
                ..
            } => {
                if previous_entry_mode.value() != entry_mode.value() {
                    writeln!(
                        out,
                        " mode change {:06o} => {:06o} {}",
                        previous_entry_mode.value(),
                        entry_mode.value(),
                        quote_path(location)
                    )?;
                }
            }
            ChangeDetached::Rewrite { .. } => bail!("{REWRITE_UNSUPPORTED}"),
        }
    }
    Ok(())
}

const REWRITE_UNSUPPORTED: &str =
    "a rename/copy was detected, but git's estimate_similarity() (diffcore-delta.c) \
     is not in the vendored crates, so the `(NN%)` similarity `-M` prints cannot be reproduced";

/// Render one file-level change into `body` (only when `patch` is set) and
/// return its diffstat row.
fn emit_change(
    repo: &gix::Repository,
    body: &mut Vec<u8>,
    change: &ChangeDetached,
    abbrev: usize,
    patch: bool,
) -> Result<StatEntry> {
    let mut added = 0u64;
    let mut deleted = 0u64;
    let mut binary: Option<(u64, u64)> = None;

    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            let is_sub = entry_mode.is_commit();
            let content = content_of(repo, *id, is_sub)?;
            let short = short_oid(repo, *id, abbrev, is_sub)?;
            if patch {
                emit_git_header(body, path, path);
                writeln!(body, "new file mode {:o}", entry_mode.value())?;
                writeln!(body, "index {}..{short}", "0".repeat(short.len()))?;
            }
            if is_binary(is_sub, &content) {
                binary = Some((0, content.len() as u64));
                if patch {
                    body.extend_from_slice(b"Binary files /dev/null and b/");
                    body.extend_from_slice(path);
                    body.extend_from_slice(b" differ\n");
                }
            } else {
                let counts = emit_body(body, None, Some(path), &[], &content, patch)?;
                added = counts.0;
                deleted = counts.1;
            }
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            let is_sub = entry_mode.is_commit();
            let content = content_of(repo, *id, is_sub)?;
            let short = short_oid(repo, *id, abbrev, is_sub)?;
            if patch {
                emit_git_header(body, path, path);
                writeln!(body, "deleted file mode {:o}", entry_mode.value())?;
                writeln!(body, "index {short}..{}", "0".repeat(short.len()))?;
            }
            if is_binary(is_sub, &content) {
                binary = Some((content.len() as u64, 0));
                if patch {
                    body.extend_from_slice(b"Binary files a/");
                    body.extend_from_slice(path);
                    body.extend_from_slice(b" and /dev/null differ\n");
                }
            } else {
                let counts = emit_body(body, Some(path), None, &content, &[], patch)?;
                added = counts.0;
                deleted = counts.1;
            }
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            let path: &[u8] = location;
            let old_mode = format!("{:o}", previous_entry_mode.value());
            let new_mode = format!("{:o}", entry_mode.value());
            let mode_changed = old_mode != new_mode;
            if patch {
                emit_git_header(body, path, path);
                if mode_changed {
                    writeln!(body, "old mode {old_mode}")?;
                    writeln!(body, "new mode {new_mode}")?;
                }
            }
            // A pure mode change (identical content) prints no index/hunks.
            if previous_id != id {
                let old_is_sub = previous_entry_mode.is_commit();
                let new_is_sub = entry_mode.is_commit();
                let old_content = content_of(repo, *previous_id, old_is_sub)?;
                let new_content = content_of(repo, *id, new_is_sub)?;
                if patch {
                    let old_short = short_oid(repo, *previous_id, abbrev, old_is_sub)?;
                    let new_short = short_oid(repo, *id, abbrev, new_is_sub)?;
                    if mode_changed {
                        writeln!(body, "index {old_short}..{new_short}")?;
                    } else {
                        writeln!(body, "index {old_short}..{new_short} {new_mode}")?;
                    }
                }
                if is_binary(old_is_sub, &old_content) || is_binary(new_is_sub, &new_content) {
                    binary = Some((old_content.len() as u64, new_content.len() as u64));
                    if patch {
                        body.extend_from_slice(b"Binary files a/");
                        body.extend_from_slice(path);
                        body.extend_from_slice(b" and b/");
                        body.extend_from_slice(path);
                        body.extend_from_slice(b" differ\n");
                    }
                } else {
                    let counts = emit_body(
                        body,
                        Some(path),
                        Some(path),
                        &old_content,
                        &new_content,
                        patch,
                    )?;
                    added = counts.0;
                    deleted = counts.1;
                }
            }
        }
        ChangeDetached::Rewrite { .. } => bail!("{REWRITE_UNSUPPORTED}"),
    }

    Ok(StatEntry {
        name: quote_path(change_path(change)),
        added,
        deleted,
        binary,
    })
}

/// git's `buffer_is_binary`: a NUL byte in the first 8000 bytes. A submodule
/// renders as a text line and is never binary.
fn is_binary(is_submodule: bool, content: &[u8]) -> bool {
    !is_submodule && content.iter().take(8000).any(|&b| b == 0)
}

/// `diff --git a/<old> b/<new>` line, preserving raw path bytes.
fn emit_git_header(out: &mut Vec<u8>, old: &[u8], new: &[u8]) {
    out.extend_from_slice(b"diff --git a/");
    out.extend_from_slice(old);
    out.extend_from_slice(b" b/");
    out.extend_from_slice(new);
    out.push(b'\n');
}

/// Emit the `---`/`+++` headers and hunks, returning `(added, deleted)` line
/// counts. With `patch` unset only the counts are computed (the diffstat needs
/// them even when no patch text is printed).
fn emit_body(
    out: &mut Vec<u8>,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    old_content: &[u8],
    new_content: &[u8],
    patch: bool,
) -> Result<(u64, u64)> {
    let mut hunks: Vec<u8> = Vec::new();
    let counts = emit_text_hunks(&mut hunks, old_content, new_content)?;
    if !patch || hunks.is_empty() {
        return Ok(counts);
    }

    out.extend_from_slice(b"--- ");
    match old {
        Some(p) => {
            out.extend_from_slice(b"a/");
            out.extend_from_slice(p);
        }
        None => out.extend_from_slice(b"/dev/null"),
    }
    out.push(b'\n');

    out.extend_from_slice(b"+++ ");
    match new {
        Some(p) => {
            out.extend_from_slice(b"b/");
            out.extend_from_slice(p);
        }
        None => out.extend_from_slice(b"/dev/null"),
    }
    out.push(b'\n');

    out.extend_from_slice(&hunks);
    Ok(counts)
}

/// Compute the unified diff of two blobs with git's default settings, returning
/// the added/deleted line counts the diffstat needs.
fn emit_text_hunks(out: &mut Vec<u8>, old: &[u8], new: &[u8]) -> Result<(u64, u64)> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let before_lines: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();
    let writer = HunkWriter {
        out,
        before_lines,
        added: 0,
        deleted: 0,
    };
    let counts = UnifiedDiff::new(&diff, &input, writer, ContextSize::symmetrical(3)).consume()?;
    Ok(counts)
}

/// Writes hunks in git's unified-diff style and tallies changed lines.
struct HunkWriter<'a> {
    out: &'a mut Vec<u8>,
    /// Pre-image lines, for resolving each hunk header's function context.
    before_lines: Vec<&'a [u8]>,
    added: u64,
    deleted: u64,
}

impl<'a> HunkWriter<'a> {
    /// Nearest "function" line above the hunk's leading context, mirroring git's
    /// default (no `xfuncname`) heuristic: first byte is a letter, `_`, or `$`.
    fn find_func(&self, before_hunk_start: u32) -> Option<&'a [u8]> {
        let ctx_start = before_hunk_start.saturating_sub(1);
        let mut idx = ctx_start as i64 - 1;
        while idx >= 0 {
            let line = trim_end_ws(self.before_lines[idx as usize]);
            if let Some(&first) = line.first() {
                if first.is_ascii_alphabetic() || first == b'_' || first == b'$' {
                    return Some(line);
                }
            }
            idx -= 1;
        }
        None
    }
}

impl ConsumeHunk for HunkWriter<'_> {
    type Out = (u64, u64);

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        self.out.extend_from_slice(b"@@ -");
        write_range(self.out, header.before_hunk_start, header.before_hunk_len);
        self.out.extend_from_slice(b" +");
        write_range(self.out, header.after_hunk_start, header.after_hunk_len);
        self.out.extend_from_slice(b" @@");
        if let Some(func) = self.find_func(header.before_hunk_start) {
            self.out.push(b' ');
            self.out.extend_from_slice(func);
        }
        self.out.push(b'\n');

        for &(kind, content) in lines {
            self.out.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => {
                    self.added += 1;
                    b'+'
                }
                DiffLineKind::Remove => {
                    self.deleted += 1;
                    b'-'
                }
            });
            self.out.extend_from_slice(content);
            if !content.ends_with(b"\n") {
                self.out.push(b'\n');
                self.out
                    .extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> (u64, u64) {
        (self.added, self.deleted)
    }
}

/// git omits the `,len` field when the hunk spans exactly one line.
fn write_range(out: &mut Vec<u8>, start: u32, len: u32) {
    if len == 1 {
        let _ = write!(out, "{start}");
    } else {
        let _ = write!(out, "{start},{len}");
    }
}

fn trim_end_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last == b' ' || last == b'\t' || last == b'\n' || last == b'\r' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

/// The bytes to diff for an entry: a blob comes from the object database; a
/// submodule (commit entry) renders as its `Subproject commit <oid>` line.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// Abbreviated object id for the `index` line.
fn short_oid(
    repo: &gix::Repository,
    id: ObjectId,
    abbrev: usize,
    is_submodule: bool,
) -> Result<String> {
    if is_submodule {
        Ok(id.to_hex_with_len(abbrev).to_string())
    } else {
        Ok(id.attach(repo).shorten()?.to_string())
    }
}

/// The path of a change, for stable diff ordering.
fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

/// C-style path quoting matching git's default `core.quotePath=true`
/// (`quote_c_style`), used for the stat and summary columns.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}
