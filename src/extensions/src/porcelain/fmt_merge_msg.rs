//! `git fmt-merge-msg` — turn a `FETCH_HEAD`-shaped list of merged tips into a
//! merge commit message.
//!
//! A port of git's `builtin/fmt-merge-msg.c` (option parsing, input slurping)
//! together with `fmt-merge-msg.c` itself (`find_merge_parents`, `handle_line`,
//! `fmt_merge_msg_title`, `fmt_merge_msg_sigs`, `shortlog`, `credit_people`).
//! The commit walk, merge-base reduction, object access and configuration all
//! come from the vendored gitoxide crates.
//!
//! Covered, byte-for-byte against stock git:
//!   * the title line — `Merge branch 'x'`, `Merge branches 'x' and 'y'`,
//!     `Merge tag 'v1'`, `Merge remote-tracking branch 'origin/x'`,
//!     `Merge commit '<desc>'`, `Merge HEAD`, the `, ` / `; ` separators between
//!     categories and source repositories, and the ` of <url>` suffix.
//!   * ` into <branch>` suppression via `merge.suppressDest` (`wildmatch` with
//!     pathname semantics), defaulting to `main` and `master` when unset.
//!   * `--into-name <name>`, and the detached-HEAD spelling (`HEAD`).
//!   * `-m`/`--message <text>` — replaces the title and suppresses it.
//!   * `--log[=<n>]` / `--no-log` / `--summary[=<n>]` / `--no-summary`, and the
//!     `merge.log` / `merge.summary` configuration fallback (`true` = 20).
//!   * the per-origin shortlog block: `* <name>:` (or `* <name>: (<n> commits)`
//!     when the count exceeds the limit), two-space-indented subjects in
//!     commit-date order newest first, merges counted but not listed, the
//!     trailing `  ...` line, and the commit id substituted for an empty subject.
//!   * the `<comment> By ...` / `<comment> Via ...` credit lines, including the
//!     count suffixes, `and others`, and the suppression when the sole author is
//!     the configured `user.name`. `core.commentChar` / `core.commentString` are
//!     honoured.
//!   * annotated (unsigned) tag bodies spliced in ahead of the shortlog, and the
//!     commented origin headers git inserts once a second tag shows up.
//!   * `merge.branchdesc` (`branch.<name>.description`, rendered as `  : ` lines).
//!   * `-F`/`--file <path>` (`-` means stdin), lines marked `not-for-merge`, and
//!     tips already reachable from `HEAD`, which `reduce_heads` drops.
//!   * exit codes: 0 on success, 128 for a malformed input line and for an
//!     unborn `HEAD`, 129 for `-h` (usage on stdout) and for a bad or extra
//!     argument (usage on stderr).
//!
//! Not covered — these `bail!` rather than emitting output that would diverge:
//!   * signed tags. git runs the payload through `check_signature()` and
//!     interleaves gpg's verdict into the message as commented lines; there is
//!     no signature-verification driver in the vendored crates.
//!
//! Known deviations, both on inputs stock git treats specially:
//!   * option abbreviation (`--int` for `--into-name`) is not accepted; git's
//!     `parse_options` allows unambiguous prefixes, and `--help` is rejected as
//!     an unknown option rather than opening the manual page.
//!   * the wording of the `error: ...` diagnostics that precede the usage block
//!     is approximate; only stderr is affected, and the exit code still matches.
//!   * `merge.log` and `merge.summary` are resolved as "last `merge.log` wins,
//!     otherwise last `merge.summary`". git lets the two interleave in a single
//!     configuration order, so a `merge.summary` written after a `merge.log`
//!     wins there and loses here.

use anyhow::{bail, Result};
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice, ByteVec};
use gix::hash::ObjectId;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;
use gix::Repository;

/// git's `DEFAULT_MERGE_LOG_LEN`.
const DEFAULT_MERGE_LOG_LEN: i64 = 20;

/// The `usage_with_options` block, verbatim from `fmt_merge_msg_usage`.
const USAGE: &str = "\
usage: git fmt-merge-msg [-m <message>] [--log[=<n>] | --no-log] [--file <file>]

    --[no-]log[=<n>]      populate log with at most <n> entries from shortlog
    -m, --[no-]message <text>
                          use <text> as start of message
    --[no-]into-name <name>
                          use <name> instead of the real target branch
    -F, --[no-]file <file>
                          file to read from
";

/// Signature block openers `parse_signed_buffer()` recognises.
const SIG_MARKERS: &[&str] = &[
    "-----BEGIN PGP SIGNATURE-----",
    "-----BEGIN PGP MESSAGE-----",
    "-----BEGIN SIGNED MESSAGE-----",
    "-----BEGIN SSH SIGNATURE-----",
];

/// git's `struct src_data`: everything merged from one source repository.
#[derive(Default)]
struct SrcData {
    branch: Vec<BString>,
    tag: Vec<BString>,
    r_branch: Vec<BString>,
    generic: Vec<BString>,
    /// Bit 1: a bare `HEAD` was pulled. Bit 2: something named was pulled.
    head_status: u8,
}

/// git's `struct origin_data`.
struct OriginData {
    oid: ObjectId,
    is_local_branch: bool,
}

/// git's `struct merge_parent`: the id as given on the line, and the commit it
/// peels to.
struct MergeParent {
    given: ObjectId,
    commit: ObjectId,
}

/// The `merge.*` knobs `fmt_merge_msg_config()` reads.
struct MergeConfig {
    /// `merge.log` / `merge.summary`, already folded to a length (`true` = 20).
    log_len: i64,
    /// `merge.branchdesc`.
    branch_desc: bool,
    /// `merge.suppressDest`, in configuration order, with the git default
    /// (`main`, `master`) already substituted when the variable never appeared.
    suppress_dest: Vec<BString>,
}

/// `git fmt-merge-msg` — see the module documentation for the covered surface.
pub fn fmt_merge_msg(args: &[String]) -> Result<ExitCode> {
    let mut inpath: Option<String> = None;
    let mut message: Option<String> = None;
    let mut into_name: Option<String> = None;
    // -1 is git's "not given on the command line" sentinel.
    let mut shortlog_len: i64 = -1;

    // `handle_builtin()` answers a lone `-h` before any repository setup.
    if args.len() == 2 && args[1] == "-h" {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    // Everything else needs the repository first: `git_config()` runs before
    // `parse_options()`, so a bad flag outside a repository still reports the
    // missing repository.
    let repo = gix::discover(".")?;

    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--log" | "--summary" => shortlog_len = DEFAULT_MERGE_LOG_LEN,
            "--no-log" | "--no-summary" => shortlog_len = 0,
            _ if a.starts_with("--log=") || a.starts_with("--summary=") => {
                let (flag, value) = a.split_once('=').expect("checked above");
                let Ok(n) = value.parse::<i64>() else {
                    return Ok(usage_error(&format!(
                        "options `{}' expects a numerical value",
                        flag.trim_start_matches('-')
                    )));
                };
                shortlog_len = n;
            }
            "-m" | "--message" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(usage_error("switch `m' requires a value"));
                };
                message = Some(v.clone());
            }
            _ if a.starts_with("--message=") => message = Some(a["--message=".len()..].into()),
            _ if a.starts_with("-m") && a.len() > 2 => message = Some(a[2..].into()),
            "--into-name" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(usage_error("option `into-name' requires a value"));
                };
                into_name = Some(v.clone());
            }
            _ if a.starts_with("--into-name=") => {
                into_name = Some(a["--into-name=".len()..].into());
            }
            "-F" | "--file" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(usage_error("switch `F' requires a value"));
                };
                inpath = Some(v.clone());
            }
            _ if a.starts_with("--file=") => inpath = Some(a["--file=".len()..].into()),
            _ if a.starts_with("-F") && a.len() > 2 => inpath = Some(a[2..].into()),
            "--" => {
                // parse_options stops here; anything left is a positional, and
                // `fmt-merge-msg` takes none.
                if i + 1 < args.len() {
                    return Ok(usage(&mut std::io::stderr()));
                }
            }
            _ if a.starts_with("--") => {
                return Ok(usage_error(&format!("unknown option `{}'", &a[2..])));
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                let c = a[1..].chars().next().expect("non-empty");
                return Ok(usage_error(&format!("unknown switch `{c}'")));
            }
            // `argc > 0` after parse_options.
            _ => return Ok(usage(&mut std::io::stderr())),
        }
        i += 1;
    }

    let config = merge_config(&repo);
    if shortlog_len < 0 {
        shortlog_len = if config.log_len > 0 {
            config.log_len
        } else {
            0
        };
    }

    let mut input = Vec::new();
    match inpath.as_deref() {
        Some(path) if path != "-" => {
            let mut file = std::fs::File::open(path)
                .map_err(|e| anyhow::anyhow!("cannot open '{path}': {e}"))?;
            file.read_to_end(&mut input)?;
        }
        _ => {
            std::io::stdin().lock().read_to_end(&mut input)?;
        }
    }

    let mut out: Vec<u8> = Vec::new();
    if let Some(message) = &message {
        out.extend_from_slice(message.as_bytes());
    }

    // `opts.add_title = !message`, `opts.credit_people = 1`.
    if let Some(code) = build(
        &repo,
        &config,
        &input,
        message.is_none(),
        shortlog_len,
        into_name.as_deref(),
        &mut out,
    )? {
        return Ok(code);
    }

    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// git's `fmt_merge_msg()`: append the whole message to `out`. `Some(code)` is
/// one of git's `die()` paths, where nothing is written to stdout.
#[allow(clippy::too_many_arguments)]
fn build(
    repo: &Repository,
    config: &MergeConfig,
    input: &[u8],
    add_title: bool,
    shortlog_len: i64,
    into_name: Option<&str>,
    out: &mut Vec<u8>,
) -> Result<Option<ExitCode>> {
    // `refs_resolve_refdup("HEAD", RESOLVE_REF_READING, ...)`: the fully
    // resolved ref name plus the id it points at.
    let head = repo.head()?;
    if head.is_unborn() {
        eprintln!("fatal: No current branch");
        return Ok(Some(ExitCode::from(128)));
    }
    let head_oid = repo.head_id()?.detach();
    // `--into-name` is used verbatim; only the resolved ref name is shortened.
    let current_branch: BString = match into_name {
        Some(name) => name.into(),
        None => {
            // A detached HEAD resolves to the literal `HEAD`.
            let resolved: BString = match head.referent_name() {
                Some(name) => name.as_bstr().to_owned(),
                None => "HEAD".into(),
            };
            strip_prefix(resolved.as_bstr(), b"refs/heads/")
        }
    };
    drop(head);

    let hexsz = repo.object_hash().len_in_hex();
    let parents = find_merge_parents(repo, input, head_oid, hexsz)?;

    let mut srcs: Vec<(BString, SrcData)> = Vec::new();
    let mut origins: Vec<(BString, OriginData)> = Vec::new();
    for (n, line) in split_lines(input).into_iter().enumerate() {
        if !handle_line(line, &parents, hexsz, &mut srcs, &mut origins) {
            eprintln!(
                "fatal: error in line {}: {}",
                n + 1,
                line.as_bstr().to_str_lossy()
            );
            return Ok(Some(ExitCode::from(128)));
        }
    }

    if add_title && !srcs.is_empty() {
        fmt_merge_msg_title(&srcs, current_branch.as_bstr(), &config.suppress_dest, out);
    }

    let comment = comment_string(repo);
    if !origins.is_empty() {
        fmt_merge_msg_sigs(repo, &origins, comment.as_bstr(), out)?;
    }

    if shortlog_len != 0 {
        // git keeps the limit signed, so a negative `--log=<n>` lists nothing.
        let limit = shortlog_len;
        complete_line(out);
        for (name, origin) in &origins {
            shortlog(
                repo,
                config,
                name.as_bstr(),
                origin,
                head_oid,
                limit,
                comment.as_bstr(),
                out,
            )?;
        }
    }

    complete_line(out);
    Ok(None)
}

/// git's `find_merge_parents()`: the mergeable tips on `input`, reduced to those
/// that survive `reduce_heads()` once `HEAD` joins them.
fn find_merge_parents(
    repo: &Repository,
    input: &[u8],
    head: ObjectId,
    hexsz: usize,
) -> Result<Vec<MergeParent>> {
    let mut table: Vec<MergeParent> = Vec::new();
    let mut heads: Vec<ObjectId> = Vec::new();

    for line in split_lines(input) {
        // `parse_oid_hex(p, &oid, &q) || q[0] != '\t' || q[1] != '\t'` — the
        // second tab is what distinguishes a mergeable tip from `not-for-merge`.
        if line.len() < hexsz + 2 || line[hexsz] != b'\t' || line[hexsz + 1] != b'\t' {
            continue;
        }
        let Ok(given) = ObjectId::from_hex(&line[..hexsz]) else {
            continue;
        };
        let Ok(object) = repo.find_object(given) else {
            continue;
        };
        let Ok(commit) = object.peel_to_commit() else {
            continue;
        };
        heads.push(commit.id);
        if !table
            .iter()
            .any(|p| p.given == given && p.commit == commit.id)
        {
            table.push(MergeParent {
                given,
                commit: commit.id,
            });
        }
    }

    // `lookup_commit(head)`: only joined in when it really is a commit.
    if let Ok(object) = repo.find_object(head) {
        if object.peel_to_commit().is_ok() {
            heads.push(head);
        }
    }

    let reduced = reduce_heads(repo, &heads)?;
    table.retain(|p| reduced.contains(&p.commit));
    Ok(table)
}

/// git's `reduce_heads()`: de-duplicate (first occurrence wins) and drop every
/// commit reachable from another one in the list.
fn reduce_heads(repo: &Repository, commits: &[ObjectId]) -> Result<Vec<ObjectId>> {
    let mut unique: Vec<ObjectId> = Vec::with_capacity(commits.len());
    for id in commits {
        if !unique.contains(id) {
            unique.push(*id);
        }
    }

    let mut out = Vec::with_capacity(unique.len());
    for (i, candidate) in unique.iter().enumerate() {
        let mut redundant = false;
        for (j, other) in unique.iter().enumerate() {
            if i != j && is_ancestor(repo, *candidate, *other)? {
                redundant = true;
                break;
            }
        }
        if !redundant {
            out.push(*candidate);
        }
    }
    Ok(out)
}

/// git's `in_merge_bases()`: is `one` reachable from `two`?
fn is_ancestor(repo: &Repository, one: ObjectId, two: ObjectId) -> Result<bool> {
    Ok(repo
        .merge_bases_many(one, &[two])?
        .into_iter()
        .any(|id| id.detach() == one))
}

/// git's `handle_line()`. Returns false where git returns non-zero and dies.
fn handle_line(
    line: &[u8],
    parents: &[MergeParent],
    hexsz: usize,
    srcs: &mut Vec<(BString, SrcData)>,
    origins: &mut Vec<(BString, OriginData)>,
) -> bool {
    if line.len() < hexsz + 3 || line[hexsz] != b'\t' {
        return false;
    }
    if line[hexsz + 1..].starts_with(b"not-for-merge") {
        return true;
    }
    if line[hexsz + 1] != b'\t' {
        return false;
    }
    let Ok(oid) = ObjectId::from_hex(&line[..hexsz]) else {
        return false;
    };
    // Subsumed by another parent.
    if !parents.iter().any(|p| p.given == oid) {
        return true;
    }

    // e.g. `branch 'frotz' of git://that/repository.git`; the part before the
    // first ` of ` names what was merged, the part after names the repository.
    let desc = &line[hexsz + 2..];
    let (what, src, pulling_head) = match desc.find(" of ") {
        Some(at) => (&desc[..at], &desc[at + 4..], false),
        None => (desc, desc, true),
    };

    // `unsorted_string_list_lookup`: first match in insertion order.
    let existing = srcs.iter().position(|(name, _)| name == src);
    let idx = match existing {
        Some(idx) => idx,
        None => {
            srcs.push((src.into(), SrcData::default()));
            srcs.len() - 1
        }
    };
    let src_data = &mut srcs[idx].1;

    let mut is_local_branch = false;
    let mut origin: BString = if pulling_head {
        src_data.head_status |= 1;
        src.into()
    } else if let Some(rest) = what.strip_prefix(b"branch ".as_slice()) {
        is_local_branch = true;
        src_data.branch.push(rest.into());
        src_data.head_status |= 2;
        rest.into()
    } else if let Some(rest) = what.strip_prefix(b"tag ".as_slice()) {
        src_data.tag.push(rest.into());
        src_data.head_status |= 2;
        // For tags the origin keeps the `tag ` prefix, unlike the other cases.
        what.into()
    } else if let Some(rest) = what.strip_prefix(b"remote-tracking branch ".as_slice()) {
        src_data.r_branch.push(rest.into());
        src_data.head_status |= 2;
        rest.into()
    } else {
        src_data.generic.push(what.into());
        src_data.head_status |= 2;
        src.into()
    };

    if src == b"." || src == origin.as_slice() {
        if origin.len() >= 2 && origin[0] == b'\'' && origin[origin.len() - 1] == b'\'' {
            let unquoted = BString::from(&origin[1..origin.len() - 1]);
            origin = unquoted;
        }
    } else {
        let mut joined = origin.clone();
        joined.push_str(" of ");
        joined.push_str(src);
        origin = joined;
    }
    if src != b"." {
        is_local_branch = false;
    }

    origins.push((
        origin,
        OriginData {
            oid,
            is_local_branch,
        },
    ));
    true
}

/// git's `fmt_merge_msg_title()`.
fn fmt_merge_msg_title(
    srcs: &[(BString, SrcData)],
    current_branch: &BStr,
    suppress_dest: &[BString],
    out: &mut Vec<u8>,
) {
    out.extend_from_slice(b"Merge ");
    let mut sep: &[u8] = b"";
    for (name, src) in srcs {
        out.extend_from_slice(sep);
        sep = b"; ";
        let mut subsep: &[u8] = b"";

        if src.head_status == 1 {
            out.extend_from_slice(name);
            continue;
        }
        if src.head_status == 3 {
            subsep = b", ";
            out.extend_from_slice(b"HEAD");
        }
        for (list, singular, plural) in [
            (&src.branch, "branch ", "branches "),
            (
                &src.r_branch,
                "remote-tracking branch ",
                "remote-tracking branches ",
            ),
            (&src.tag, "tag ", "tags "),
            (&src.generic, "commit ", "commits "),
        ] {
            if list.is_empty() {
                continue;
            }
            out.extend_from_slice(subsep);
            subsep = b", ";
            print_joined(singular, plural, list, out);
        }
        if name != "." {
            out.extend_from_slice(b" of ");
            out.extend_from_slice(name);
        }
    }

    // `dest_suppressed()`: `wildmatch(pattern, dest, WM_PATHNAME)`.
    let suppressed = suppress_dest.iter().any(|p| {
        gix::glob::wildmatch(
            p.as_bstr(),
            current_branch,
            gix::glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL,
        )
    });
    if !suppressed {
        out.extend_from_slice(b" into ");
        out.extend_from_slice(current_branch);
    }
    out.push(b'\n');
}

/// git's `print_joined()`: `a`, `a and b`, `a, b and c`.
fn print_joined(singular: &str, plural: &str, list: &[BString], out: &mut Vec<u8>) {
    match list {
        [] => {}
        [only] => {
            out.extend_from_slice(singular.as_bytes());
            out.extend_from_slice(only);
        }
        [head @ .., last] => {
            out.extend_from_slice(plural.as_bytes());
            for (i, item) in head.iter().enumerate() {
                if i > 0 {
                    out.extend_from_slice(b", ");
                }
                out.extend_from_slice(item);
            }
            out.extend_from_slice(b" and ");
            out.extend_from_slice(last);
        }
    }
}

/// git's `fmt_merge_msg_sigs()`: splice in the body of every merged tag.
fn fmt_merge_msg_sigs(
    repo: &Repository,
    origins: &[(BString, OriginData)],
    comment: &BStr,
    out: &mut Vec<u8>,
) -> Result<()> {
    let mut tagbuf: Vec<u8> = Vec::new();
    let mut tag_number = 0usize;
    let mut first_tag = 0usize;

    for (i, (name, origin)) in origins.iter().enumerate() {
        let Ok(object) = repo.find_object(origin.oid) else {
            continue;
        };
        if object.kind != gix::object::Kind::Tag {
            continue;
        }
        // `parse_signature()` splits a trailing signature block off the payload;
        // git then hands it to `check_signature()`.
        if find_signature(&object.data).is_some() {
            bail!(
                "signed tag {:?} is not supported (no signature-verification driver \
                 in the vendored crates)",
                name.to_str_lossy()
            );
        }

        if tag_number == 0 {
            first_tag = i;
        } else {
            if tag_number == 1 {
                // The first tag's header is only added once a second one shows up.
                let mut tagline: Vec<u8> = vec![b'\n'];
                add_commented_lines(&mut tagline, &origins[first_tag].0, comment);
                tagbuf.splice(0..0, tagline);
            }
            tagbuf.push(b'\n');
            add_commented_lines(&mut tagbuf, name, comment);
        }
        tag_number += 1;
        fmt_tag_signature(&object.data, &mut tagbuf);
    }

    if !tagbuf.is_empty() {
        out.push(b'\n');
        out.extend_from_slice(&tagbuf);
    }
    Ok(())
}

/// git's `fmt_tag_signature()` for the unsigned case: everything after the tag
/// header block, newline-completed.
fn fmt_tag_signature(data: &[u8], tagbuf: &mut Vec<u8>) {
    if let Some(at) = data.find(b"\n\n") {
        tagbuf.extend_from_slice(&data[at + 2..]);
    }
    complete_line(tagbuf);
}

/// git's `strbuf_add_commented_lines()`: prefix every line with `comment`, plus
/// a space unless the line starts with a newline or a tab.
fn add_commented_lines(out: &mut Vec<u8>, buf: &[u8], comment: &BStr) {
    let mut rest = buf;
    while !rest.is_empty() {
        let end = match rest.find_byte(b'\n') {
            Some(at) => at + 1,
            None => rest.len(),
        };
        out.extend_from_slice(comment);
        if rest[0] != b'\n' && rest[0] != b'\t' {
            out.push(b' ');
        }
        out.extend_from_slice(&rest[..end]);
        rest = &rest[end..];
    }
    complete_line(out);
}

/// git's `shortlog()`: the `* <name>:` block for one merged tip.
#[allow(clippy::too_many_arguments)]
fn shortlog(
    repo: &Repository,
    config: &MergeConfig,
    name: &BStr,
    origin: &OriginData,
    head: ObjectId,
    limit: i64,
    comment: &BStr,
    out: &mut Vec<u8>,
) -> Result<()> {
    // `deref_tag(...)`, then `branch->type != OBJ_COMMIT` bails out silently.
    let Ok(object) = repo.find_object(origin.oid) else {
        return Ok(());
    };
    let Ok(tip) = object.peel_to_commit() else {
        return Ok(());
    };
    let tip = tip.id;

    let mut count = 0usize;
    let mut subjects: Vec<BString> = Vec::new();
    let mut authors: Vec<(BString, usize)> = Vec::new();
    let mut committers: Vec<(BString, usize)> = Vec::new();

    let walk = repo
        .rev_walk([tip])
        .with_hidden([head])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    for info in walk.all()? {
        let commit = info?.object()?;

        if commit.parent_ids().count() > 1 {
            // Merges are not listed, but their committer is still credited.
            record_person(&mut committers, commit.committer()?.trim().name);
            continue;
        }
        if count == 0 {
            // The tip committer.
            record_person(&mut committers, commit.committer()?.trim().name);
        }
        record_person(&mut authors, commit.author()?.trim().name);
        count += 1;
        if subjects.len() as i64 > limit {
            continue;
        }

        let message = commit.message()?;
        let subject = message.summary();
        if subject.is_empty() {
            subjects.push(commit.id.to_hex().to_string().into());
        } else {
            subjects.push(subject.into_owned());
        }
    }

    add_people_info(repo, &mut authors, &mut committers, comment, out);

    if count as i64 > limit {
        out.extend_from_slice(b"\n* ");
        out.extend_from_slice(name);
        out.extend_from_slice(format!(": ({count} commits)\n").as_bytes());
    } else {
        out.extend_from_slice(b"\n* ");
        out.extend_from_slice(name);
        out.extend_from_slice(b":\n");
    }

    if origin.is_local_branch && config.branch_desc {
        add_branch_desc(repo, name, out);
    }

    for (i, subject) in subjects.iter().enumerate() {
        if i as i64 >= limit {
            out.extend_from_slice(b"  ...\n");
        } else {
            out.extend_from_slice(b"  ");
            out.extend_from_slice(subject);
            out.push(b'\n');
        }
    }
    Ok(())
}

/// git's `record_person()`: count one appearance, keeping the list ordered by
/// name (git uses a sorted `string_list`).
fn record_person(people: &mut Vec<(BString, usize)>, name: &BStr) {
    match people.binary_search_by(|(known, _)| known.as_bstr().cmp(name)) {
        Ok(at) => people[at].1 += 1,
        Err(at) => people.insert(at, (name.to_owned(), 1)),
    }
}

/// git's `add_people_info()` plus `credit_people()`.
fn add_people_info(
    repo: &Repository,
    authors: &mut Vec<(BString, usize)>,
    committers: &mut Vec<(BString, usize)>,
    comment: &BStr,
    out: &mut Vec<u8>,
) {
    // The lists arrive sorted by name; git then sorts by descending count.
    authors.sort_by(|a, b| b.1.cmp(&a.1));
    committers.sort_by(|a, b| b.1.cmp(&a.1));

    let me_author = identity(repo.author());
    let me_committer = identity(repo.committer());
    credit_people(
        authors,
        "By",
        me_author.as_ref().map(|me| me.as_bstr()),
        comment,
        out,
    );
    credit_people(
        committers,
        "Via",
        me_committer.as_ref().map(|me| me.as_bstr()),
        comment,
        out,
    );
}

/// `git_author_info(IDENT_NO_DATE)` / `git_committer_info(IDENT_NO_DATE)`.
fn identity(
    configured: Option<Result<gix::actor::SignatureRef<'_>, gix::config::time::Error>>,
) -> Option<BString> {
    let signature = configured?.ok()?;
    let mut me: BString = signature.name.to_owned();
    me.push_str(" <");
    me.push_str(signature.email);
    me.push_str(">");
    Some(me)
}

/// git's `credit_people()`: skip the line entirely when nobody, or only the
/// configured identity, is credited.
fn credit_people(
    people: &[(BString, usize)],
    label: &str,
    me: Option<&BStr>,
    comment: &BStr,
    out: &mut Vec<u8>,
) {
    let only_me = people.len() == 1
        && me.is_some_and(|me| {
            me.strip_prefix(people[0].0.as_slice())
                .is_some_and(|rest| rest.starts_with(b" <"))
        });
    if people.is_empty() || only_me {
        return;
    }
    out.push(b'\n');
    out.extend_from_slice(comment);
    out.extend_from_slice(format!(" {label} ").as_bytes());
    add_people_count(people, out);
}

/// git's `add_people_count()`.
fn add_people_count(people: &[(BString, usize)], out: &mut Vec<u8>) {
    match people {
        [] => {}
        [(name, _)] => out.extend_from_slice(name),
        [(a, an), (b, bn)] => {
            out.extend_from_slice(a);
            out.extend_from_slice(format!(" ({an}) and ").as_bytes());
            out.extend_from_slice(b);
            out.extend_from_slice(format!(" ({bn})").as_bytes());
        }
        [(a, an), ..] => {
            out.extend_from_slice(a);
            out.extend_from_slice(format!(" ({an}) and others").as_bytes());
        }
    }
}

/// git's `add_branch_desc()`: `branch.<name>.description`, one `  : ` line each.
fn add_branch_desc(repo: &Repository, name: &BStr, out: &mut Vec<u8>) {
    let Ok(key) = name.to_str() else { return };
    let snapshot = repo.config_snapshot();
    let Some(desc) = snapshot.string(format!("branch.{key}.description").as_str()) else {
        return;
    };
    let mut rest = desc.as_slice();
    while !rest.is_empty() {
        let end = match rest.find_byte(b'\n') {
            Some(at) => at + 1,
            None => rest.len(),
        };
        out.extend_from_slice(b"  : ");
        out.extend_from_slice(&rest[..end]);
        rest = &rest[end..];
    }
    complete_line(out);
}

/// Read the `merge.*` variables `fmt_merge_msg_config()` handles.
fn merge_config(repo: &Repository) -> MergeConfig {
    let snapshot = repo.config_snapshot();
    let plumbing = snapshot.plumbing();

    let log_values = plumbing.values::<BString>("merge.log").unwrap_or_default();
    let summary_values = plumbing
        .values::<BString>("merge.summary")
        .unwrap_or_default();
    let log_len = log_values
        .last()
        .or(summary_values.last())
        .and_then(|v| bool_or_int(v.as_bstr()))
        .unwrap_or(0);

    let branch_desc = snapshot.boolean("merge.branchdesc").unwrap_or(false);

    // An empty value clears everything accumulated so far.
    let raw = plumbing
        .values::<BString>("merge.suppressDest")
        .unwrap_or_default();
    let seen = !raw.is_empty();
    let mut suppress_dest: Vec<BString> = Vec::new();
    for value in raw {
        if value.is_empty() {
            suppress_dest.clear();
        } else {
            suppress_dest.push(value);
        }
    }
    if !seen {
        suppress_dest = vec!["main".into(), "master".into()];
    }

    MergeConfig {
        log_len,
        branch_desc,
        suppress_dest,
    }
}

/// git's `git_config_bool_or_int()` folded into a shortlog length: an integer is
/// taken as-is, a true boolean becomes 20, a false one becomes 0.
fn bool_or_int(value: &BStr) -> Option<i64> {
    let text = value.to_str().ok()?;
    if let Ok(n) = text.trim().parse::<i64>() {
        return Some(n);
    }
    match text.to_ascii_lowercase().as_str() {
        "" | "true" | "yes" | "on" => Some(DEFAULT_MERGE_LOG_LEN),
        "false" | "no" | "off" => Some(0),
        _ => None,
    }
}

/// `core.commentChar` / `core.commentString` — equivalent knobs in git 2.55, so
/// the last one written in configuration order wins. `auto` and absent are `#`.
fn comment_string(repo: &Repository) -> BString {
    let snapshot = repo.config_snapshot();
    let mut chosen: BString = "#".into();

    for section in snapshot.plumbing().sections() {
        let header = section.header();
        if header.subsection_name().is_some()
            || !header.name().to_string().eq_ignore_ascii_case("core")
        {
            continue;
        }
        // `value_names()` yields one entry per occurrence in order, and each
        // name's `values()` are in order too, so per-name cursors interleave the
        // two spellings correctly.
        let body = section.body();
        let chars = body.values("commentChar");
        let strings = body.values("commentString");
        let (mut char_at, mut string_at) = (0usize, 0usize);

        for value_name in body.value_names() {
            let value = if value_name.eq_ignore_ascii_case("commentChar") {
                let value = chars.get(char_at);
                char_at += 1;
                value
            } else if value_name.eq_ignore_ascii_case("commentString") {
                let value = strings.get(string_at);
                string_at += 1;
                value
            } else {
                continue;
            };
            let Some(value) = value else { continue };
            if value.is_empty() {
                continue;
            }
            chosen = if value == "auto" {
                "#".into()
            } else {
                value.clone()
            };
        }
    }
    chosen
}

/// Split `input` the way `fmt_merge_msg()` walks it: on `\n`, with the trailing
/// newline dropped and a final unterminated record still yielded.
fn split_lines(input: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < input.len() {
        let rest = &input[pos..];
        match rest.find_byte(b'\n') {
            Some(at) => {
                out.push(&rest[..at]);
                pos += at + 1;
            }
            None => {
                out.push(rest);
                pos += rest.len();
            }
        }
    }
    out
}

/// The offset of the first signature block that starts a line, if any.
fn find_signature(data: &[u8]) -> Option<usize> {
    SIG_MARKERS
        .iter()
        .filter_map(|marker| find_at_line_start(data, marker.as_bytes()))
        .min()
}

/// First occurrence of `needle` in `haystack` that begins a line.
fn find_at_line_start(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|&i| (i == 0 || haystack[i - 1] == b'\n') && &haystack[i..i + needle.len()] == needle)
}

/// git's `strbuf_complete_line()`.
fn complete_line(out: &mut Vec<u8>) {
    if !out.is_empty() && out[out.len() - 1] != b'\n' {
        out.push(b'\n');
    }
}

/// Drop `prefix` from `value` when present, as git does for `refs/heads/`.
fn strip_prefix(value: &BStr, prefix: &[u8]) -> BString {
    match value.strip_prefix(prefix) {
        Some(rest) => rest.into(),
        None => value.to_owned(),
    }
}

/// `usage_with_options()` — usage plus the option list, exit 129.
fn usage(to: &mut impl Write) -> ExitCode {
    let _ = write!(to, "{USAGE}");
    ExitCode::from(129)
}

/// `parse_options`' error path: the diagnostic, then usage, both on stderr.
fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    usage(&mut std::io::stderr())
}
