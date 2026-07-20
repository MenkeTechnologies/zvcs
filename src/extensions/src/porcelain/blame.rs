use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;

/// git's smallest permitted abbreviation length.
const MINIMUM_ABBREV: usize = 4;

/// Byte-for-byte reproduction of `git blame`'s usage text, printed on stderr with
/// exit status 129 when no path is given (the only usage error we produce).
const USAGE: &str = concat!(
    "usage: git blame [<options>] [<rev-opts>] [<rev>] [--] <file>\n",
    "\n",
    "    <rev-opts> are documented in git-rev-list(1)\n",
    "\n",
    "    --[no-]incremental    show blame entries as we find them, incrementally\n",
    "    -b                    do not show object names of boundary commits (Default: off)\n",
    "    --[no-]root           do not treat root commits as boundaries (Default: off)\n",
    "    --[no-]show-stats     show work cost statistics\n",
    "    --[no-]progress       force progress reporting\n",
    "    --[no-]score-debug    show output score for blame entries\n",
    "    -f, --[no-]show-name  show original filename (Default: auto)\n",
    "    -n, --[no-]show-number\n",
    "                          show original linenumber (Default: off)\n",
    "    -p, --[no-]porcelain  show in a format designed for machine consumption\n",
    "    --[no-]line-porcelain show porcelain format with per-line commit information\n",
    "    -c                    use the same output mode as git-annotate (Default: off)\n",
    "    -t                    show raw timestamp (Default: off)\n",
    "    -l                    show long commit SHA1 (Default: off)\n",
    "    -s                    suppress author name and timestamp (Default: off)\n",
    "    -e, --[no-]show-email show author email instead of name (Default: off)\n",
    "    -w                    ignore whitespace differences\n",
    "    --diff-algorithm <algorithm>\n",
    "                          choose a diff algorithm\n",
    "    --[no-]ignore-rev <rev>\n",
    "                          ignore <rev> when blaming\n",
    "    --[no-]ignore-revs-file <file>\n",
    "                          ignore revisions from <file>\n",
    "    --[no-]color-lines    color redundant metadata from previous line differently\n",
    "    --[no-]color-by-age   color lines by age\n",
    "    -S <file>             use revisions from <file> instead of calling git-rev-list\n",
    "    --[no-]contents <file>\n",
    "                          use <file>'s contents as the final image\n",
    "    -C[<score>]           find line copies within and across files\n",
    "    -M[<score>]           find line movements within and across files\n",
    "    -L <range>            process only line range <start>,<end> or function :<funcname>\n",
    "    --[no-]abbrev[=<n>]   use <n> digits to display object names\n",
    "\n",
);

/// The synthetic author git attributes not-yet-committed lines to.
const NOT_COMMITTED_NAME: &[u8] = b"Not Committed Yet";
const NOT_COMMITTED_MAIL: &[u8] = b"not.committed.yet";

/// `git blame` — line-by-line last-modifying commit, backed by `gix-blame`.
///
/// Implemented invocation forms, reproducing stock `git blame` byte for byte:
///   * `git blame <file>`                     — blame the working-tree file
///   * `git blame <rev> [--] <file>`          — blame `<rev>:<file>`
///   * `-L <start>,<end>` / `-L <start>` / `-L <start>,+<n>` / `-L ,<end>`
///   * `-l`/`--long`, `-s`, `-e`/`--show-email`, `-f`/`--show-name`,
///     `-n`/`--show-number`, `--abbrev=<n>`
///   * `-p`/`--porcelain`, `--line-porcelain`
///
/// With no `<rev>`, the working-tree copy of the file is blamed the way git does
/// it: a synthetic commit holding the working-tree content sits on top of `HEAD`,
/// so lines that differ from `HEAD` are reported against the all-zero object id
/// with author `Not Committed Yet`.
///
/// Whole-file rename following is on (matching git's default), so the source
/// filename column appears exactly when git would show it. Boundary commits
/// (roots) are prefixed with `^` as git does.
///
/// Flags that are not implemented (`--incremental`, `-M`/`-C` line-move
/// detection, `--reverse`, `-w`, `-b`, `--root`, `-t`, `-c`, `--contents`,
/// `--ignore-rev`, regex/function `-L` forms, …) are rejected with a terse
/// message rather than emitting wrong output.
pub fn blame(args: &[String]) -> Result<ExitCode> {
    let opts = match Options::parse(args)? {
        Parsed::Usage => {
            let mut err = std::io::stderr().lock();
            err.write_all(USAGE.as_bytes())?;
            err.flush()?;
            return Ok(ExitCode::from(129));
        }
        Parsed::Options(opts) => opts,
    };

    let repo = gix::discover(".")?;

    // Resolve the suspect commit (default HEAD), peeling tags to a commit.
    // `head_id` is only required for the working-tree overlay, so an unborn HEAD
    // stays tolerable as long as an explicit revision was given.
    let (suspect, head_id) = match &opts.rev {
        Some(rev) => {
            let id = repo
                .rev_parse_single(rev.as_str())?
                .object()?
                .peel_to_commit()?
                .id()
                .detach();
            (id, repo.head_id().ok().map(|h| h.detach()))
        }
        None => {
            let id = repo.head_id()?.detach();
            (id, Some(id))
        }
    };

    // Translate the user's path (relative to CWD) into a repo-root-relative path.
    let rel_path = repo_relative_path(&repo, &opts.file)?;

    // With no explicit revision git blames the working-tree file. The lines it
    // shares with `HEAD` are blamed normally; the rest belong to a synthetic
    // commit. Read the file first so we know whether that path is available.
    let worktree_content = if opts.rev.is_none() {
        repo.workdir()
            .map(|w| w.join(&rel_path))
            .and_then(|p| std::fs::read(p).ok())
    } else {
        None
    };

    // Blame the full file; `-L` is applied to the result so that the working-tree
    // overlay can be built in working-tree line coordinates, as git does.
    let ranges = if opts.ranges.is_empty() || worktree_content.is_some() {
        gix::blame::BlameRanges::default()
    } else {
        gix::blame::BlameRanges::from_one_based_inclusive_ranges(opts.ranges.clone())
            .map_err(|e| anyhow!("{e}"))?
    };
    let blame_options = gix::repository::blame_file::Options {
        diff_algorithm: None,
        ranges,
        since: None,
        rewrites: Some(gix::diff::Rewrites::default()),
    };

    let outcome = repo
        .blame_file(rel_path.as_bytes().as_bstr(), suspect, blame_options)
        .map_err(|e| anyhow!("{e}"))?;

    let mut lines = materialize_lines(&outcome);

    if let Some(content) = &worktree_content {
        lines = overlay_worktree(&repo, lines, &outcome.blob, content)?;
        if !opts.ranges.is_empty() {
            let keep = |n: u32| opts.ranges.iter().any(|r| r.contains(&n));
            lines.retain(|l| keep(l.final_no));
        }
    }

    if lines.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let null_id = ObjectId::null(repo.object_hash());
    let info = collect_commit_info(&repo, &lines, &opts, &null_id, &rel_path)?;

    if opts.porcelain {
        emit_porcelain(&repo, &lines, &info, &rel_path, head_id, &null_id, &opts)
    } else {
        emit_human(&repo, &lines, &info, &rel_path, &opts)
    }
}

/// One output line: which commit it came from and where it sits in both files.
struct Line {
    commit_id: ObjectId,
    final_no: u32,
    orig_no: u32,
    source_name: Option<Vec<u8>>,
    content: Vec<u8>,
}

/// Flatten `gix-blame`'s hunks into one `Line` per line of the blamed file.
fn materialize_lines(outcome: &gix::blame::Outcome) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    for (entry, tokens) in outcome.entries_with_lines() {
        let blamed_start = entry.start_in_blamed_file;
        let source_start = entry.start_in_source_file;
        let source_name = entry.source_file_name.as_ref().map(|n| n.to_vec());
        for (i, token) in tokens.into_iter().enumerate() {
            let i = i as u32;
            // Line tokens include their trailing '\n'; strip exactly one so the
            // writer below can re-add it (git also terminates a final line that
            // had no newline of its own).
            let mut content = token.to_vec();
            if content.last() == Some(&b'\n') {
                content.pop();
            }
            lines.push(Line {
                commit_id: entry.commit_id,
                final_no: blamed_start + i + 1,
                orig_no: source_start + i + 1,
                source_name: source_name.clone(),
                content,
            });
        }
    }
    lines
}

/// Rebase a `HEAD` blame onto the working-tree content.
///
/// git blames the working tree by putting a synthetic commit holding the
/// working-tree blob on top of `HEAD` and running its usual algorithm; the first
/// diff that commit takes part in is exactly `HEAD:<path>` against the working
/// tree. Lines that survive that diff unchanged carry `HEAD`'s blame result,
/// lines that don't stay with the synthetic commit (the null object id).
fn overlay_worktree(
    repo: &gix::Repository,
    head_lines: Vec<Line>,
    head_blob: &[u8],
    worktree: &[u8],
) -> Result<Vec<Line>> {
    let input = gix::diff::blob::InternedInput::new(head_blob, worktree);
    let algorithm = repo.diff_algorithm()?;
    let mut diff = gix::diff::blob::Diff::compute(algorithm, &input);
    diff.postprocess_lines(&input);

    let after_len = input.after.len() as u32;

    // Map each working-tree line to the `HEAD` line it is unchanged from, if any.
    let mut mapped: Vec<Option<u32>> = vec![None; after_len as usize];
    let (mut before, mut after) = (0u32, 0u32);
    for hunk in diff.hunks() {
        while after < hunk.after.start {
            mapped[after as usize] = Some(before);
            after += 1;
            before += 1;
        }
        after = hunk.after.end;
        before = hunk.before.end;
    }
    while after < after_len {
        mapped[after as usize] = Some(before);
        after += 1;
        before += 1;
    }

    let null_id = ObjectId::null(repo.object_hash());
    let tokens: Vec<&[u8]> = gix::diff::blob::sources::byte_lines(worktree).collect();

    let mut out = Vec::with_capacity(after_len as usize);
    for (i, token) in tokens.into_iter().enumerate() {
        let mut content = token.to_vec();
        if content.last() == Some(&b'\n') {
            content.pop();
        }
        let final_no = i as u32 + 1;
        match mapped[i].and_then(|h| head_lines.get(h as usize)) {
            Some(src) => out.push(Line {
                commit_id: src.commit_id,
                final_no,
                orig_no: src.orig_no,
                source_name: src.source_name.clone(),
                content,
            }),
            None => out.push(Line {
                commit_id: null_id,
                final_no,
                orig_no: final_no,
                source_name: None,
                content,
            }),
        }
    }
    Ok(out)
}

/// Everything about a commit that either output format can need.
struct CommitInfo {
    /// Human-format author column: name, or `<email>` under `-e`.
    display_author: Vec<u8>,
    /// Human-format date column.
    display_date: String,
    boundary: bool,
    hex: String,
    author_name: Vec<u8>,
    author_mail: Vec<u8>,
    author_time: i64,
    author_tz: String,
    committer_name: Vec<u8>,
    committer_mail: Vec<u8>,
    committer_time: i64,
    committer_tz: String,
    summary: Vec<u8>,
}

fn collect_commit_info(
    repo: &gix::Repository,
    lines: &[Line],
    opts: &Options,
    null_id: &ObjectId,
    rel_path: &str,
) -> Result<HashMap<ObjectId, CommitInfo>> {
    let mut info: HashMap<ObjectId, CommitInfo> = HashMap::new();
    for line in lines {
        if info.contains_key(&line.commit_id) {
            continue;
        }
        let ci = if &line.commit_id == null_id {
            not_committed_info(line.commit_id, opts, rel_path)
        } else {
            let commit = repo.find_commit(line.commit_id)?;
            let author = commit.author()?;
            let committer = commit.committer()?;
            let author_time = author.time().ok();
            let committer_time = committer.time().ok();
            // Reduced to owned values before the struct literal: the iterator
            // and the summary both borrow `commit`, which drops at the end of
            // this block while the literal's temporaries are still live.
            let boundary = commit.parent_ids().next().is_none();
            let summary = Vec::from(commit.message()?.summary().into_owned());
            CommitInfo {
                display_author: display_author(author.name, author.email, opts.show_email),
                display_date: author_time
                    .map(|t| t.format_or_unix(gix::date::time::format::ISO8601))
                    .unwrap_or_else(|| author.time.to_string()),
                boundary,
                hex: line.commit_id.to_hex().to_string(),
                author_name: author.name.to_vec(),
                author_mail: author.email.to_vec(),
                author_time: author_time.map(|t| t.seconds).unwrap_or(0),
                author_tz: format_tz(author_time.map(|t| t.offset).unwrap_or(0)),
                committer_name: committer.name.to_vec(),
                committer_mail: committer.email.to_vec(),
                committer_time: committer_time.map(|t| t.seconds).unwrap_or(0),
                committer_tz: format_tz(committer_time.map(|t| t.offset).unwrap_or(0)),
                summary,
            }
        };
        info.insert(line.commit_id, ci);
    }
    Ok(info)
}

/// The synthetic commit git invents for working-tree lines.
fn not_committed_info(id: ObjectId, opts: &Options, rel_path: &str) -> CommitInfo {
    let now = gix::date::Time::now_local_or_utc();
    CommitInfo {
        display_author: display_author(
            NOT_COMMITTED_NAME.as_bstr(),
            NOT_COMMITTED_MAIL.as_bstr(),
            opts.show_email,
        ),
        display_date: now.format_or_unix(gix::date::time::format::ISO8601),
        boundary: false,
        hex: id.to_hex().to_string(),
        author_name: NOT_COMMITTED_NAME.to_vec(),
        author_mail: NOT_COMMITTED_MAIL.to_vec(),
        author_time: now.seconds,
        author_tz: format_tz(now.offset),
        committer_name: NOT_COMMITTED_NAME.to_vec(),
        committer_mail: NOT_COMMITTED_MAIL.to_vec(),
        committer_time: now.seconds,
        committer_tz: format_tz(now.offset),
        summary: format!("Version of {rel_path} from {rel_path}").into_bytes(),
    }
}

fn display_author(name: &gix::bstr::BStr, email: &gix::bstr::BStr, show_email: bool) -> Vec<u8> {
    if show_email {
        let mut v = Vec::with_capacity(email.len() + 2);
        v.push(b'<');
        v.extend_from_slice(email);
        v.push(b'>');
        v
    } else {
        name.to_vec()
    }
}

/// Effective object-name width, following git: `-l` forces the full hash,
/// otherwise `--abbrev`/`core.abbrev` applies and one extra digit is reserved so
/// the boundary caret can take a slot without shrinking the column.
fn object_name_width(repo: &gix::Repository, opts: &Options) -> usize {
    let hexsz = repo.object_hash().len_in_hex();
    let mut width = if opts.long {
        hexsz
    } else {
        match opts.abbrev {
            // `--abbrev=0` means "no abbreviation" to git.
            Some(0) => hexsz,
            Some(n) => n.clamp(MINIMUM_ABBREV, hexsz),
            None => configured_abbrev(repo, hexsz).clamp(MINIMUM_ABBREV, hexsz),
        }
    };
    if width < hexsz {
        width += 1;
    }
    width
}

fn emit_human(
    repo: &gix::Repository,
    lines: &[Line],
    info: &HashMap<ObjectId, CommitInfo>,
    rel_path: &str,
    opts: &Options,
) -> Result<ExitCode> {
    let name_width = object_name_width(repo, opts);

    let show_file = opts.show_name || lines.iter().any(|l| l.source_name.is_some());
    let current_path = rel_path.as_bytes();
    let w_line = decimal_width(lines.iter().map(|l| l.final_no).max().unwrap_or(1));
    let w_orig = decimal_width(lines.iter().map(|l| l.orig_no).max().unwrap_or(1));
    let w_file = if show_file {
        lines
            .iter()
            .map(|l| l.source_name.as_deref().unwrap_or(current_path).len())
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let w_author = if opts.suppress {
        0
    } else {
        lines
            .iter()
            .map(|l| info[&l.commit_id].display_author.len())
            .max()
            .unwrap_or(0)
    };

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut buf: Vec<u8> = Vec::with_capacity(128);

    for line in lines {
        let ci = &info[&line.commit_id];
        buf.clear();

        // Object name. A boundary commit spends one column on the `^` marker,
        // which git takes out of the hash rather than widening the field.
        if ci.boundary {
            buf.push(b'^');
            buf.extend_from_slice(&ci.hex.as_bytes()[..name_width - 1]);
        } else {
            buf.extend_from_slice(&ci.hex.as_bytes()[..name_width]);
        }

        // Source filename column (left-justified).
        if show_file {
            let name = line.source_name.as_deref().unwrap_or(current_path);
            buf.push(b' ');
            buf.extend_from_slice(name);
            pad(&mut buf, w_file.saturating_sub(name.len()));
        }

        // Original line number in the source commit (right-justified).
        if opts.show_number {
            let s = line.orig_no.to_string();
            buf.push(b' ');
            pad(&mut buf, w_orig.saturating_sub(s.len()));
            buf.extend_from_slice(s.as_bytes());
        }

        // Author/date block (omitted entirely by `-s`, mirroring git which then
        // leaves the closing paren of the line-number field unmatched).
        if !opts.suppress {
            buf.extend_from_slice(b" (");
            buf.extend_from_slice(&ci.display_author);
            pad(&mut buf, w_author.saturating_sub(ci.display_author.len()));
            buf.push(b' ');
            buf.extend_from_slice(ci.display_date.as_bytes());
        }

        // Final line number (right-justified) + content.
        let s = line.final_no.to_string();
        buf.push(b' ');
        pad(&mut buf, w_line.saturating_sub(s.len()));
        buf.extend_from_slice(s.as_bytes());
        buf.extend_from_slice(b") ");
        buf.extend_from_slice(&line.content);
        buf.push(b'\n');

        out.write_all(&buf)?;
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn emit_porcelain(
    repo: &gix::Repository,
    lines: &[Line],
    info: &HashMap<ObjectId, CommitInfo>,
    rel_path: &str,
    head_id: Option<ObjectId>,
    null_id: &ObjectId,
    opts: &Options,
) -> Result<ExitCode> {
    let current_path = rel_path.as_bytes();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    // git prints a commit's detail block once per output (`--porcelain`) or once
    // per line (`--line-porcelain`).
    let mut shown: HashSet<ObjectId> = HashSet::new();
    let mut previous_cache: HashMap<(ObjectId, Vec<u8>), Option<(String, Vec<u8>)>> = HashMap::new();

    for group in group_lines(lines) {
        let first = &lines[group.start];
        let ci = &info[&first.commit_id];
        let path = first.source_name.as_deref().unwrap_or(current_path);

        let key = (first.commit_id, path.to_vec());
        if !previous_cache.contains_key(&key) {
            let previous = if &first.commit_id == null_id {
                head_id.map(|h| (h.to_hex().to_string(), current_path.to_vec()))
            } else {
                find_previous(repo, first.commit_id, path)?
            };
            previous_cache.insert(key.clone(), previous);
        }
        let previous = &previous_cache[&key];

        for (i, line) in lines[group.start..group.start + group.len].iter().enumerate() {
            if i == 0 {
                writeln!(
                    out,
                    "{} {} {} {}",
                    ci.hex, line.orig_no, line.final_no, group.len
                )?;
            } else {
                writeln!(out, "{} {} {}", ci.hex, line.orig_no, line.final_no)?;
            }
            if i == 0 || opts.line_porcelain {
                if opts.line_porcelain || shown.insert(first.commit_id) {
                    write_detail(&mut out, ci, previous.as_ref(), path)?;
                }
            }
            out.write_all(b"\t")?;
            out.write_all(&line.content)?;
            out.write_all(b"\n")?;
        }
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn write_detail(
    out: &mut impl Write,
    ci: &CommitInfo,
    previous: Option<&(String, Vec<u8>)>,
    path: &[u8],
) -> Result<()> {
    write_field(out, b"author", &ci.author_name)?;
    out.write_all(b"author-mail <")?;
    out.write_all(&ci.author_mail)?;
    out.write_all(b">\n")?;
    writeln!(out, "author-time {}", ci.author_time)?;
    writeln!(out, "author-tz {}", ci.author_tz)?;
    write_field(out, b"committer", &ci.committer_name)?;
    out.write_all(b"committer-mail <")?;
    out.write_all(&ci.committer_mail)?;
    out.write_all(b">\n")?;
    writeln!(out, "committer-time {}", ci.committer_time)?;
    writeln!(out, "committer-tz {}", ci.committer_tz)?;
    write_field(out, b"summary", &ci.summary)?;
    if ci.boundary {
        out.write_all(b"boundary\n")?;
    }
    if let Some((hex, prev_path)) = previous {
        out.write_all(b"previous ")?;
        out.write_all(hex.as_bytes())?;
        out.write_all(b" ")?;
        out.write_all(&quote_name(prev_path))?;
        out.write_all(b"\n")?;
    }
    out.write_all(b"filename ")?;
    out.write_all(&quote_name(path))?;
    out.write_all(b"\n")?;
    Ok(())
}

fn write_field(out: &mut impl Write, key: &[u8], value: &[u8]) -> Result<()> {
    out.write_all(key)?;
    out.write_all(b" ")?;
    out.write_all(value)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// A run of consecutive output lines sharing one commit, one source path and a
/// contiguous stretch of the source file — git's `blame_coalesce()` rule.
struct Group {
    start: usize,
    len: usize,
}

fn group_lines(lines: &[Line]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let extends = groups.last().is_some_and(|g: &Group| {
            let prev = &lines[g.start + g.len - 1];
            prev.commit_id == line.commit_id
                && prev.source_name == line.source_name
                && prev.orig_no + 1 == line.orig_no
                && prev.final_no + 1 == line.final_no
        });
        if extends {
            let last = groups.len() - 1;
            groups[last].len += 1;
        } else {
            groups.push(Group { start: i, len: 1 });
        }
    }
    groups
}

/// The `previous <commit> <path>` field: the first parent of `commit` in which
/// `path` still exists. git records the origin it found in the first parent it
/// looked at; when the file is not in that parent (the commit added it, or the
/// commit is a root) there is no `previous` line at all.
///
/// A commit that both renamed and modified the file in one step would need
/// rename detection against the parent to name the pre-rename path; that case is
/// not covered here and yields no `previous` line.
fn find_previous(
    repo: &gix::Repository,
    commit: ObjectId,
    path: &[u8],
) -> Result<Option<(String, Vec<u8>)>> {
    let commit = repo.find_commit(commit)?;
    let Some(parent) = commit.parent_ids().next() else {
        return Ok(None);
    };
    let parent_id = parent.detach();
    let Ok(path_str) = std::str::from_utf8(path) else {
        return Ok(None);
    };
    let tree = repo.find_commit(parent_id)?.tree()?;
    if tree
        .lookup_entry_by_path(std::path::Path::new(path_str))?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some((parent_id.to_hex().to_string(), path.to_vec())))
}

/// git's `quote_c_style`: paths containing control, quote, backslash or non-ASCII
/// bytes are emitted as a C-style quoted string, everything else verbatim.
fn quote_name(name: &[u8]) -> Vec<u8> {
    let needs_quoting = name
        .iter()
        .any(|&b| b < 0x20 || b >= 0x7f || b == b'"' || b == b'\\');
    if !needs_quoting {
        return name.to_vec();
    }
    let mut out = Vec::with_capacity(name.len() + 2);
    out.push(b'"');
    for &b in name {
        match b {
            0x07 => out.extend_from_slice(b"\\a"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x0b => out.extend_from_slice(b"\\v"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b if b < 0x20 || b >= 0x7f => out.extend_from_slice(format!("\\{b:03o}").as_bytes()),
            b => out.push(b),
        }
    }
    out.push(b'"');
    out
}

/// Format a UTC offset the way git writes the `author-tz`/`committer-tz` field.
fn format_tz(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let abs = offset_seconds.unsigned_abs();
    format!("{sign}{:02}{:02}", abs / 3600, (abs % 3600) / 60)
}

/// Parsed command line, or a request to print git's usage.
enum Parsed {
    Options(Options),
    Usage,
}

struct Options {
    rev: Option<String>,
    file: String,
    ranges: Vec<RangeInclusive<u32>>,
    long: bool,
    suppress: bool,
    show_email: bool,
    show_name: bool,
    show_number: bool,
    abbrev: Option<usize>,
    porcelain: bool,
    line_porcelain: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Parsed> {
        let mut ranges: Vec<RangeInclusive<u32>> = Vec::new();
        let mut long = false;
        let mut suppress = false;
        let mut show_email = false;
        let mut show_name = false;
        let mut show_number = false;
        let mut abbrev: Option<usize> = None;
        let mut porcelain = false;
        let mut line_porcelain = false;
        let mut positionals: Vec<String> = Vec::new();
        let mut only_paths = false;

        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if only_paths {
                positionals.push(a.to_string());
                i += 1;
                continue;
            }
            match a {
                "--" => only_paths = true,
                "-l" | "--long" => long = true,
                "-s" => suppress = true,
                "-e" | "--show-email" => show_email = true,
                "-f" | "--show-name" => show_name = true,
                "-n" | "--show-number" => show_number = true,
                // git's `--porcelain` and `--line-porcelain` are bit flags on one
                // field, so `--line-porcelain` wins no matter the order.
                "-p" | "--porcelain" => porcelain = true,
                "--line-porcelain" => {
                    porcelain = true;
                    line_porcelain = true;
                }
                "-L" => {
                    i += 1;
                    let spec = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `-L` requires a value"))?;
                    parse_line_range(spec, &mut ranges)?;
                }
                "--abbrev" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--abbrev` requires a value"))?;
                    abbrev = Some(v.parse().map_err(|_| anyhow!("invalid --abbrev value: {v}"))?);
                }
                _ if a.starts_with("-L") => parse_line_range(&a[2..], &mut ranges)?,
                _ if a.starts_with("--abbrev=") => {
                    let v = &a["--abbrev=".len()..];
                    abbrev = Some(v.parse().map_err(|_| anyhow!("invalid --abbrev value: {v}"))?);
                }
                _ if a.starts_with('-') && a.len() > 1 => {
                    bail!("unsupported option: {a}")
                }
                _ => positionals.push(a.to_string()),
            }
            i += 1;
        }

        // `[<rev>] [--] <file>`: the last positional is the path; a single
        // positional before it (if any) is the revision.
        let (rev, file) = match positionals.len() {
            0 => return Ok(Parsed::Usage),
            1 => (None, positionals.pop().unwrap()),
            2 => {
                let file = positionals.pop().unwrap();
                (Some(positionals.pop().unwrap()), file)
            }
            _ => bail!("too many paths given; only a single file is supported"),
        };

        Ok(Parsed::Options(Options {
            rev,
            file,
            ranges,
            long,
            suppress,
            show_email,
            show_name,
            show_number,
            abbrev,
            porcelain,
            line_porcelain,
        }))
    }
}

/// Parse one `-L` spec into a 1-based inclusive range. Only numeric forms are
/// supported; regex (`/re/`) and function (`:name`) forms are rejected.
fn parse_line_range(spec: &str, ranges: &mut Vec<RangeInclusive<u32>>) -> Result<()> {
    if spec.starts_with('/') || spec.starts_with(':') {
        bail!("unsupported -L form: only numeric ranges are supported");
    }
    let (start_part, end_part) = match spec.split_once(',') {
        Some((s, e)) => (s, Some(e)),
        None => (spec, None),
    };

    let start: u32 = if start_part.is_empty() {
        1
    } else {
        start_part
            .parse()
            .map_err(|_| anyhow!("invalid -L range: {spec}"))?
    };
    if start == 0 {
        bail!("invalid -L range: line numbers are 1-based");
    }

    let end: u32 = match end_part {
        None => u32::MAX,
        Some(e) if e.is_empty() => u32::MAX,
        Some(e) if e.starts_with('+') => {
            let count: u32 = e[1..]
                .parse()
                .map_err(|_| anyhow!("invalid -L range: {spec}"))?;
            start.saturating_add(count.saturating_sub(1))
        }
        Some(e) if e.starts_with('-') => {
            bail!("unsupported -L form: relative end offsets are not supported")
        }
        Some(e) => e.parse().map_err(|_| anyhow!("invalid -L range: {spec}"))?,
    };

    ranges.push(start..=end.max(start));
    Ok(())
}

/// git's effective `core.abbrev`: an explicit number, `auto`/absent → derived
/// from the object count, or `no`/`off`/`false` → the full hash length.
fn configured_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    match repo
        .config_snapshot()
        .string("core.abbrev")
        .as_ref()
        .and_then(|v| v.to_str().ok().map(str::to_ascii_lowercase))
    {
        None => auto_abbrev(repo, hexsz),
        Some(v) => match v.as_str() {
            "auto" => auto_abbrev(repo, hexsz),
            "no" | "off" | "false" => hexsz,
            other => other.parse::<usize>().unwrap_or_else(|_| auto_abbrev(repo, hexsz)),
        },
    }
}

/// Auto abbreviation length: `ceil(log2(objects) / 2)`, floored at 7 — the same
/// heuristic `gix` uses for `core.abbrev = auto`.
fn auto_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    let count = repo.objects.packed_object_count().unwrap_or(0);
    let mut len = (64 - count.leading_zeros()) as usize;
    len = len.div_ceil(2);
    len.max(7).min(hexsz)
}

/// Number of decimal digits needed to print `n` (at least 1).
fn decimal_width(n: u32) -> usize {
    n.to_string().len()
}

/// Append `n` spaces to `buf`.
fn pad(buf: &mut Vec<u8>, n: usize) {
    buf.resize(buf.len() + n, b' ');
}

/// Turn a CWD-relative user path into a repo-root-relative path, so blame works
/// from any subdirectory of the worktree (git resolves pathspecs the same way).
fn repo_relative_path(repo: &gix::Repository, user_path: &str) -> Result<String> {
    let joined = match repo.workdir() {
        Some(workdir) => {
            let cwd = std::env::current_dir()?;
            let workdir_abs = workdir.canonicalize().unwrap_or_else(|_| workdir.to_path_buf());
            let cwd_abs = cwd.canonicalize().unwrap_or(cwd);
            match cwd_abs.strip_prefix(&workdir_abs) {
                Ok(prefix) => prefix.join(user_path),
                Err(_) => PathBuf::from(user_path),
            }
        }
        None => PathBuf::from(user_path),
    };

    let s = joined
        .to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {user_path}"))?;
    Ok(s.strip_prefix("./").unwrap_or(s).to_string())
}
