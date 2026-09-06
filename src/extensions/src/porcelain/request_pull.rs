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
//!   * the `-M` rename/copy pass. The tree diff is taken *without* gitoxide's
//!     own rewrite tracking and the resulting filepairs are run through the
//!     `diffcore-delta.c`/`diffcore-rename.c` port in
//!     [`super::diffcore_rename`], so the `{a => b}` stat name
//!     ([`super::diff_pairs::pprint_rename`]) and the `(NN%)` in
//!     `rename a => b (NN%)` are git's own `estimate_similarity()` score
//!     rather than an approximation.
//!
//! Not covered — these `bail!` rather than emit output that would diverge:
//!   * `-p` over a range whose diff contains a rename or copy. The stat block
//!     above renders it, but the patch body comes from
//!     [`super::diff::commit_patch`], which is not run with `-M`, so it would
//!     spell the same change as a delete plus an add and disagree with the
//!     stat directly above it.
//!   * unmerged entries and the `diff.statNameWidth`/`diff.statGraphWidth`
//!     configuration. (`core.quotePath` *is* honoured — the names go through
//!     `quote_c_style()` — and the column arithmetic is the shared
//!     [`super::diffstat`] port, so it measures display columns.)
//!
//! Known deviations, both stated rather than hidden:
//!   * `term_columns()` is read from `COLUMNS`, falling back to 80. git also
//!     asks the terminal via `TIOCGWINSZ` when stdout is a tty; there is no
//!     ioctl in the vendored crates, so a tty-attached run with `COLUMNS` unset
//!     uses 80 where git would use the window width.
//!   * when the remote cannot be reached the `fatal:` line on stderr is
//!     gitoxide's transport error, not git's. stdout and the exit code match.

use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};

use super::diffstat::{self, StatWidths};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
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
            // parse_options_step() tests `--help-all` with a `strcmp()` of its
            // own, ahead of parse_long_opt() and after the `--` break above, so
            // the name never abbreviates and never takes an `=<value>`. The
            // `OPTIONS_SPEC` declares no hidden entry, so `USAGE_FULL` renders
            // the same block `-h` prints.
            "-h" | "--help" | "--help-all" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // `git rev-parse --parseopt` runs the rejected argument through
            // parse-options, which names an *option* for a `--` spelling and a
            // *switch* for a short one (parse-options.c:889-898). The
            // `OPTIONS_SPEC` in git-request-pull.sh declares only `p`, so no
            // long name resolves and every `--x` is unknown by name.
            _ => {
                let _ = match a.strip_prefix("--") {
                    Some(body) => eprintln!("error: unknown option `{body}'"),
                    None => {
                        let c = a[1..].chars().next().unwrap_or_default();
                        match c.is_ascii() {
                            true => eprintln!("error: unknown switch `{c}'"),
                            false => {
                                eprintln!("error: unknown non-ascii option in string: `{a}'")
                            }
                        }
                    }
                };
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

    let repo = crate::setup::discover()?;

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
                // `git rev-parse --quiet --verify "$local"`, which prints an id
                // for a full-length hex whether or not the repository has that
                // object (see [`crate::objname`]). So `$head` is non-empty and
                // the script gets past its `Not a valid revision` to the
                // `"$head"^0` line below, which is the one that fails — that is
                // why an absent full hex is reported as an *ambiguous* revision.
                None => crate::objname::resolve(&repo, local).map(|id| id.to_hex().to_string()),
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

    // No rewrite tracking here: gitoxide's own is line-based and carries no
    // score, and the `-M` the script asks for is git's. The raw
    // addition/deletion/modification filepairs go through the
    // `diffcore-rename.c` port below instead, which is the code `git diff -M`
    // itself runs.
    let options = gix::diff::Options::default();
    let mut changes = repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), options)?;
    // `gix_diff::tree_with_rewrites` recurses into subdirectories but *also*
    // reports the containing tree entries themselves as changes. git's
    // `diff_tree_oid()` runs with `DIFF_OPT_RECURSIVE` and no
    // `DIFF_OPT_TREE_IN_RECURSIVE`, so `show_stats()`/`diff_summary()` only ever
    // see blob-, symlink- and gitlink-level filepairs. Dropping the tree entries
    // here reproduces that: without it a range touching `src/lib.rs` also lists
    // `src` as a 34-byte binary file (and `-p` emits a bogus
    // `Binary files a/src and b/src differ`). `diff.rs`'s `collect_tree_change`
    // applies the same `is_tree()` filter for the same reason.
    changes.retain(|c| !change_entry_mode(c).is_tree());
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));

    if changes.is_empty() {
        return Ok(());
    }

    let pairs = detect_renames(repo, &changes)?;

    // A rename in the stat block and a delete-plus-add in the patch body under
    // it would be two different accounts of one change, so `-p` refuses instead.
    if patch && pairs.iter().any(|p| p.status == b'R' || p.status == b'C') {
        anyhow::bail!("{REWRITE_UNSUPPORTED}");
    }

    let mut stats: Vec<StatEntry> = Vec::new();
    for p in &pairs {
        stats.push(stat_of(repo, p, abbrev)?);
    }

    emit_stats(out, &stats)?;
    emit_summary(out, &pairs)?;
    if patch {
        // The script's `$patch` is a plain `-p` on the same `git diff` this
        // function is reproducing, so the body is `git diff <base>..<head>` —
        // rendered by [`super::diff::commit_patch`] rather than by a second
        // patch writer here. That renderer is the one `git diff` and `git log
        // -p` already use, so it carries their `quote_c_style` path quoting (a
        // `"`-containing or non-ASCII name is quoted and octal-escaped), the
        // terminating tab after a name holding a space, and `@@ -0,0` rather
        // than `@@ -1,0` for a file created from nothing. This module's own
        // writer had none of the three.
        out.push(b'\n');
        let head = repo.find_object(headrev)?.peel_to_commit()?;
        out.extend_from_slice(&super::diff::commit_patch(repo, &head, Some(merge_base), 3)?);
    }
    Ok(())
}

/// One filepair as `diff_flush()` sees it after `diffcore_std()` has run: the two
/// sides plus the status letter and similarity `diff_resolve_rename_copy()`
/// assigned.
struct RenderPair {
    status: u8,
    /// `p->score`, in [`super::diffcore_rename::MAX_SCORE`] units.
    score: u32,
    old_path: BString,
    new_path: BString,
    old_mode: u32,
    new_mode: u32,
    old_id: ObjectId,
    new_id: ObjectId,
}

impl RenderPair {
    /// The name the stat block prints: a rename or copy is the only pair whose
    /// two sides differ, and git factors their common prefix and suffix into
    /// `pfx{old => new}sfx` (`pprint_rename()`, diff.c).
    fn stat_name(&self) -> String {
        if self.old_path == self.new_path {
            quote_path(self.new_path.as_slice())
        } else {
            String::from_utf8_lossy(&super::diff_pairs::pprint_rename(
                &self.old_path,
                &self.new_path,
            ))
            .into_owned()
        }
    }
}

/// Reads a filespec's blob for [`super::diffcore_rename`]. Both sides of a
/// tree-to-tree pair name an object in the database, so
/// `diff_populate_filespec()` here is just an odb lookup.
struct OdbContent<'a> {
    repo: &'a gix::Repository,
}

impl super::diffcore_rename::Content for OdbContent<'_> {
    fn size(&mut self, spec: &super::diffcore_rename::FileSpec) -> Option<u64> {
        // `check_size_only = 1`: the odb header answers without inflating the blob.
        let header = self.repo.find_header(spec.oid).ok()?;
        (header.kind() == gix::object::Kind::Blob).then(|| header.size())
    }

    fn data(&mut self, spec: &super::diffcore_rename::FileSpec) -> Option<Vec<u8>> {
        self.repo.find_object(spec.oid).ok().map(|o| o.detach().data)
    }
}

/// The `diffcore_std()` rename pass the script's `git diff -M` runs
/// (`diffcore_rename()` followed by `diff_resolve_rename_copy()`, diff.c).
///
/// `-M` is `DIFF_DETECT_RENAME` with no score argument, which
/// `diffcore_rename()` reads as `DEFAULT_RENAME_SCORE`; no `-C`, so copies are
/// only ever found where a rename source was reused, and no `-B`, so break
/// detection is off.
fn detect_renames(
    repo: &gix::Repository,
    changes: &[ChangeDetached],
) -> Result<Vec<RenderPair>> {
    use super::diffcore_rename::{self as dcr, FileSpec};

    let hash = repo.object_hash();
    let null = ObjectId::null(hash);
    let mut q = dcr::Queue::default();
    for change in changes {
        let path = BString::from(change_path(change).to_vec());
        let (one, two) = match change {
            ChangeDetached::Addition { entry_mode, id, .. } => (
                FileSpec::absent(path.clone()),
                FileSpec::new(path, entry_mode.value() as u32, *id, true),
            ),
            ChangeDetached::Deletion { entry_mode, id, .. } => (
                FileSpec::new(path.clone(), entry_mode.value() as u32, *id, true),
                FileSpec::absent(path),
            ),
            ChangeDetached::Modification {
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
                ..
            } => (
                FileSpec::new(
                    path.clone(),
                    previous_entry_mode.value() as u32,
                    *previous_id,
                    true,
                ),
                FileSpec::new(path, entry_mode.value() as u32, *id, true),
            ),
            // Rewrite tracking is off (see `diff_stat_summary`), so gitoxide
            // never produces this variant here.
            ChangeDetached::Rewrite { .. } => anyhow::bail!("{REWRITE_UNSUPPORTED}"),
        };
        let one = q.add_spec(one);
        let two = q.add_spec(two);
        q.add_pair(one, two);
    }

    let opts = dcr::Options {
        detect_rename: dcr::DETECT_RENAME,
        hash_kind: hash,
        ..Default::default()
    };
    let mut content = OdbContent { repo };
    dcr::run(&mut q, &opts, &mut content);
    dcr::resolve_rename_copy(&mut q);

    Ok(q
        .pairs
        .iter()
        .map(|p| {
            let one = &q.specs[p.one];
            let two = &q.specs[p.two];
            RenderPair {
                // A pair that reached the flush with no status is git's
                // `check_pair_status()` fatal case; `-M` always resolves one, so
                // an unset letter can only be a modification.
                status: if p.status == 0 { b'M' } else { p.status },
                score: p.score,
                old_path: one.path.clone(),
                new_path: two.path.clone(),
                old_mode: one.mode,
                new_mode: two.mode,
                old_id: if one.valid() { one.oid } else { null },
                new_id: if two.valid() { two.oid } else { null },
            }
        })
        .collect())
}

/// The rows [`super::diffstat::show_stats`] renders.
fn stat_rows(files: &[StatEntry]) -> Vec<diffstat::StatFile> {
    files
        .iter()
        .map(|f| match f.binary {
            Some((old, new)) => diffstat::StatFile {
                print_name: f.name.clone().into_bytes(),
                added: new,
                deleted: old,
                binary: true,
                is_unmerged: false,
            },
            None => diffstat::StatFile::text(f.name.clone().into_bytes(), f.added, f.deleted),
        })
        .collect()
}

/// The diffstat block, which is the `git diff -M --stat --summary` the script
/// runs. That child is `builtin/diff.c`, so it has run `init_diffstat_widths()`
/// and scales to `term_columns()`.
fn emit_stats(out: &mut Vec<u8>, files: &[StatEntry]) -> Result<()> {
    diffstat::show_stats(
        out,
        &stat_rows(files),
        &StatWidths::default(),
        &super::diff_color::DiffColors::disabled(),
    );
    Ok(())
}

/// Port of `diff_summary()` (diff.c): the `create`/`delete`/`rename`/`copy`/
/// `mode change` lines that follow the diffstat.
fn emit_summary(out: &mut Vec<u8>, pairs: &[RenderPair]) -> Result<()> {
    for p in pairs {
        match p.status {
            // `show_file_mode_name()`.
            b'A' => writeln!(
                out,
                " create mode {:06o} {}",
                p.new_mode,
                quote_path(p.new_path.as_slice())
            )?,
            b'D' => writeln!(
                out,
                " delete mode {:06o} {}",
                p.old_mode,
                quote_path(p.old_path.as_slice())
            )?,
            // `show_rename_copy()`: the factored name plus
            // `similarity_index(p)`, then the mode-change line without a name.
            b'R' | b'C' => {
                writeln!(
                    out,
                    " {} {} ({}%)",
                    if p.status == b'C' { "copy" } else { "rename" },
                    p.stat_name(),
                    super::diffcore_rename::similarity_index(p.score)
                )?;
                emit_mode_change(out, p, false)?;
            }
            _ => emit_mode_change(out, p, true)?,
        }
    }
    Ok(())
}

/// `show_mode_change()`: the ` mode change <old> => <new>` line, with the path
/// appended only for a plain modification (a rename or copy has already named
/// both sides on the line above).
fn emit_mode_change(out: &mut Vec<u8>, p: &RenderPair, show_name: bool) -> Result<()> {
    if p.old_mode != 0 && p.new_mode != 0 && p.old_mode != p.new_mode {
        write!(out, " mode change {:06o} => {:06o}", p.old_mode, p.new_mode)?;
        if show_name {
            write!(out, " {}", quote_path(p.new_path.as_slice()))?;
        }
        out.push(b'\n');
    }
    Ok(())
}

const REWRITE_UNSUPPORTED: &str =
    "a rename or copy is in the range, and the `-p` body would spell it as a delete \
     plus an add: `commit_patch` is not run with `-M`, so it would contradict the \
     `rename a => b (NN%)` the stat block above it prints";

/// The diffstat row for one filepair: git's added/deleted line counts, or the
/// pre-/post-image byte sizes when either side is binary.
///
/// Only the counts are computed here. The `-p` body is rendered by
/// [`super::diff::commit_patch`] over the same tree pair, so the two never
/// disagree about a path's spelling or a hunk's header.
fn stat_of(repo: &gix::Repository, p: &RenderPair, abbrev: usize) -> Result<StatEntry> {
    let _ = abbrev;
    let mut added = 0u64;
    let mut deleted = 0u64;
    let mut binary: Option<(u64, u64)> = None;

    // git's `S_ISGITLINK`: a gitlink renders as its `Subproject commit <oid>`
    // line rather than as the commit object it names.
    let old_is_sub = p.old_mode == 0o160000;
    let new_is_sub = p.new_mode == 0o160000;
    let old_content = if p.old_mode == 0 {
        Vec::new()
    } else {
        content_of(repo, p.old_id, old_is_sub)?
    };
    let new_content = if p.new_mode == 0 {
        Vec::new()
    } else {
        content_of(repo, p.new_id, new_is_sub)?
    };

    // A pure mode change (identical content) contributes no counts.
    if p.old_id != p.new_id || p.old_mode == 0 || p.new_mode == 0 {
        if (p.old_mode != 0 && is_binary(old_is_sub, &old_content))
            || (p.new_mode != 0 && is_binary(new_is_sub, &new_content))
        {
            binary = Some((old_content.len() as u64, new_content.len() as u64));
        } else {
            let counts = text_counts(&old_content, &new_content)?;
            added = counts.0;
            deleted = counts.1;
        }
    }

    Ok(StatEntry {
        name: p.stat_name(),
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

/// The added/deleted line counts of two blobs under git's default diff settings,
/// which is all the diffstat needs. The hunk text itself is produced and
/// discarded: `UnifiedDiff` reports the counts through its consumer, and the
/// consumer only sees a line once it has been emitted.
fn text_counts(old: &[u8], new: &[u8]) -> Result<(u64, u64)> {
    let mut sink: Vec<u8> = Vec::new();
    let out = &mut sink;
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


/// The path of a change, for stable diff ordering.
fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

/// The post-image entry mode of a change, used to drop the tree-level entries
/// `gix_diff` reports alongside the recursed blobs (git's non-`TREE_IN_RECURSIVE`
/// behavior). A `Rewrite` carries the mode of its destination entry.
fn change_entry_mode(change: &ChangeDetached) -> gix::object::tree::EntryMode {
    match change {
        ChangeDetached::Addition { entry_mode, .. }
        | ChangeDetached::Deletion { entry_mode, .. }
        | ChangeDetached::Modification { entry_mode, .. }
        | ChangeDetached::Rewrite { entry_mode, .. } => *entry_mode,
    }
}

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    crate::quote::quoted_name_string(path.as_ref())
}
