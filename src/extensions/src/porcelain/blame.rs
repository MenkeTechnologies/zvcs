use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
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
/// Also implemented: `-b`, `--root`, `-t`, `-c` (annotate-compat), `-l`,
/// `--contents <file>` (and `--contents -` from stdin), `--diff-algorithm`, and
/// `--date=relative`.
///
/// The `--[no-]` negation forms git advertises are honored with git's exact
/// bit-clearing semantics: `--no-show-name`, `--no-show-number`, `--no-porcelain`
/// (clears the porcelain bit only), `--no-line-porcelain` (clears both porcelain
/// bits, so it also cancels a preceding `-p`), and `--no-abbrev` (equivalent to
/// `--abbrev=0`, i.e. the full hash). The `--no-` forms of the unimplemented
/// options (`--no-incremental`, `--no-show-stats`, `--no-progress`,
/// `--no-score-debug`, `--no-color-lines`, `--no-color-by-age`,
/// `--no-ignore-rev`, `--no-ignore-revs-file`) each select git's default, which
/// this port already produces, so they are accepted as no-ops.
///
/// Flags that are not implemented (`--incremental`, `-M`/`-C` line-move
/// detection, `--reverse`, `-w`, `--ignore-rev`, `--ignore-revs-file`, `-S`,
/// regex/function `-L` forms, `--date=human`, the `-local` date variants, …)
/// are rejected with a terse message rather than emitting wrong output.
pub fn blame(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // git reads blame.showEmail as the default for `-e`/`--show-email`, still
    // overridable on the command line (including `--no-show-email`).
    let show_email_default = repo.config_snapshot().boolean("blame.showEmail") == Some(true);

    // git reads blame.showRoot / blame.blankBoundary as the defaults for `--root`
    // and `-b`, still overridable on the command line.
    let show_root_default = repo.config_snapshot().boolean("blame.showRoot") == Some(true);
    let blank_boundary_default =
        repo.config_snapshot().boolean("blame.blankBoundary") == Some(true);

    // git reads blame.date as the default date mode for the human-format
    // timestamp column, still overridable by `--date=<mode>`. git validates the
    // config value at read time (before argument parsing), so an invalid mode
    // there is fatal even when a valid `--date` is also on the command line.
    let date_default = match repo.config_snapshot().string("blame.date") {
        Some(v) => match resolve_date_mode(&v.to_str_lossy())? {
            DateOutcome::Mode(m) => m,
            DateOutcome::Fatal(code) => return Ok(code),
        },
        None => DateMode::Iso8601,
    };

    let mut opts = Options::parse(
        args,
        show_email_default,
        show_root_default,
        blank_boundary_default,
    )?;

    // `--date=<mode>` overrides blame.date; git validates it the same way.
    opts.date_mode = match opts.date_arg.take() {
        Some(s) => match resolve_date_mode(&s)? {
            DateOutcome::Mode(m) => m,
            DateOutcome::Fatal(code) => return Ok(code),
        },
        None => date_default,
    };
    // `-t` (OUTPUT_RAW_TIMESTAMP) makes git's `format_time` ignore the date mode
    // and print the raw `<seconds> <tz>`. Modelling it as the raw mode reproduces
    // that byte-for-byte, including the fixed column width.
    if opts.raw_timestamp {
        opts.date_mode = DateMode::Raw;
    }

    // Split the positional arguments into a revision and a single path following
    // git blame's DWIM grammar, then resolve the revision. This may short-circuit
    // with git's usage text (129) or a `bad revision` / `More than one commit`
    // fatal (128); those cases print to stderr and return the code here.
    match resolve_targets(&repo, &mut opts)? {
        Targets::Usage => return print_usage(),
        Targets::Fatal(code) => return Ok(code),
        Targets::Resolved => {}
    }

    // Resolve the suspect commit (default HEAD). The overlay (working tree or
    // `--contents`) is layered on top of the suspect, so `head_id` — the commit a
    // not-yet-committed line points back to via the porcelain `previous` field —
    // is the suspect itself. An unborn HEAD stays tolerable as long as an explicit
    // revision was given.
    let (suspect, head_id) = match opts.suspect_id {
        Some(id) => (id, Some(id)),
        None => {
            let id = repo.head_id()?.detach();
            (id, Some(id))
        }
    };

    // Translate the user's path (relative to CWD) into a repo-root-relative path.
    let rel_path = repo_relative_path(&repo, &opts.file)?;

    // The final image is overlaid on top of the suspect when either no revision
    // was given (git blames the working-tree copy) or `--contents` supplies an
    // explicit image. `--contents -` reads standard input; `--contents <file>`
    // reads that file. Lines shared with the suspect keep its blame; the rest
    // belong to a synthetic commit (the null object id).
    let worktree_content = if let Some(from) = &opts.contents {
        let bytes = if from == "-" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut buf)?;
            buf
        } else {
            std::fs::read(from).map_err(|e| anyhow!("Cannot open '{from}': {e}"))?
        };
        Some(bytes)
    } else if opts.rev.is_none() {
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
        diff_algorithm: opts.diff_algorithm,
        ranges,
        since: None,
        rewrites: Some(gix::diff::Rewrites::default()),
    };

    let outcome = repo
        .blame_file(rel_path.as_bytes().as_bstr(), suspect, blame_options)
        .map_err(|e| anyhow!("{e}"))?;

    let mut lines = materialize_lines(&outcome);

    if let Some(content) = &worktree_content {
        lines = overlay_worktree(&repo, lines, &outcome.blob, content, opts.diff_algorithm)?;
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
    diff_algorithm: Option<gix::diff::blob::Algorithm>,
) -> Result<Vec<Line>> {
    let input = gix::diff::blob::InternedInput::new(head_blob, worktree);
    // `--diff-algorithm` applies to the fake-commit diff too, matching git which
    // threads its `xdl_opts` through every diff in the blame.
    let algorithm = match diff_algorithm {
        Some(a) => a,
        None => repo.diff_algorithm()?,
    };
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
            // `--root` (git's `show_root`) stops root commits counting as
            // boundaries, dropping both the `^` marker and the porcelain
            // `boundary` field for them.
            let boundary = !opts.show_root && commit.parent_ids().next().is_none();
            let summary = Vec::from(commit.message()?.summary().into_owned());
            CommitInfo {
                display_author: display_author(author.name, author.email, opts.show_email),
                display_date: author_time
                    .map(|t| opts.date_mode.format_time(t.seconds, t.offset))
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

/// The synthetic commit git invents for the final image (working tree, or the
/// `--contents` file). git's `fake_working_tree_commit` uses a different author
/// identity and message `from` field when `--contents` supplies the image.
fn not_committed_info(id: ObjectId, opts: &Options, rel_path: &str) -> CommitInfo {
    let now = gix::date::Time::now_local_or_utc();
    // git: `"External file (--contents)" / "external.file"` for `--contents`,
    // else `"Not Committed Yet" / "not.committed.yet"`.
    let (name, mail): (&[u8], &[u8]) = if opts.contents.is_some() {
        (b"External file (--contents)", b"external.file")
    } else {
        (NOT_COMMITTED_NAME, NOT_COMMITTED_MAIL)
    };
    // git's message: `"Version of %s from %s"` where the second `%s` is the path,
    // or the `--contents` argument (`"standard input"` for `-`).
    let from = match opts.contents.as_deref() {
        Some("-") => "standard input".to_string(),
        Some(f) => f.to_string(),
        None => rel_path.to_string(),
    };
    CommitInfo {
        display_author: display_author(name.as_bstr(), mail.as_bstr(), opts.show_email),
        display_date: opts.date_mode.format_time(now.seconds, now.offset),
        boundary: false,
        hex: id.to_hex().to_string(),
        author_name: name.to_vec(),
        author_mail: mail.to_vec(),
        author_time: now.seconds,
        author_tz: format_tz(now.offset),
        committer_name: name.to_vec(),
        committer_mail: mail.to_vec(),
        committer_time: now.seconds,
        committer_tz: format_tz(now.offset),
        summary: format!("Version of {rel_path} from {from}").into_bytes(),
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

/// Emit the object-name column into `buf`, following git's `print_marks` +
/// `emit_other` interplay:
///   * `-b` (`blank_boundary`) blanks a boundary commit's name to spaces — and
///     also suppresses the `^` marker, so the whole column is `name_width` spaces.
///   * otherwise a boundary commit takes one column for `^` (never in
///     annotate-compat mode, which prints no marker) and `name_width - 1` hex digits.
///   * a normal commit prints `name_width` hex digits.
fn emit_object_name(buf: &mut Vec<u8>, ci: &CommitInfo, name_width: usize, opts: &Options) {
    if ci.boundary && opts.blank_boundary {
        pad(buf, name_width);
    } else if ci.boundary && !opts.annotate_compat {
        buf.push(b'^');
        buf.extend_from_slice(&ci.hex.as_bytes()[..name_width - 1]);
    } else {
        buf.extend_from_slice(&ci.hex.as_bytes()[..name_width]);
    }
}

/// Right-justify `s` into `buf` to a minimum byte width of `min`, matching C's
/// `%*s` (spaces on the left, no truncation).
fn pad_left(buf: &mut Vec<u8>, s: &[u8], min: usize) {
    pad(buf, min.saturating_sub(s.len()));
    buf.extend_from_slice(s);
}

/// git-annotate-compatible output (`-c` / OUTPUT_ANNOTATE_COMPAT): one line per
/// blamed line, `<name>` right-justified to 10, the date to 10, tab-separated, and
/// the final 1-based line number, all inside a single `(...)`. `-f`/`-n`/`-s` do
/// not apply in this mode.
fn emit_annotate_compat(
    lines: &[Line],
    info: &HashMap<ObjectId, CommitInfo>,
    name_width: usize,
    opts: &Options,
) -> Result<ExitCode> {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut buf: Vec<u8> = Vec::with_capacity(128);

    for line in lines {
        let ci = &info[&line.commit_id];
        buf.clear();

        emit_object_name(&mut buf, ci, name_width, opts);

        // format_time pads the date to `blame_date_width` first; the trailing
        // `%10s` then never fires because every mode's width is >= 10.
        let mut date = ci.display_date.clone().into_bytes();
        pad(&mut date, opts.date_mode.width().saturating_sub(ci.display_date.chars().count()));

        buf.extend_from_slice(b"\t(");
        pad_left(&mut buf, &ci.display_author, 10);
        buf.push(b'\t');
        pad_left(&mut buf, &date, 10);
        buf.push(b'\t');
        buf.extend_from_slice(line.final_no.to_string().as_bytes());
        buf.push(b')');
        buf.extend_from_slice(&line.content);
        buf.push(b'\n');

        out.write_all(&buf)?;
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn emit_human(
    repo: &gix::Repository,
    lines: &[Line],
    info: &HashMap<ObjectId, CommitInfo>,
    rel_path: &str,
    opts: &Options,
) -> Result<ExitCode> {
    let name_width = object_name_width(repo, opts);

    if opts.annotate_compat {
        return emit_annotate_compat(lines, info, name_width, opts);
    }

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

        // Object name (boundary `^` marker, `-b` blanking).
        emit_object_name(&mut buf, ci, name_width, opts);

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
            // The date column is left-justified in a fixed, per-mode width
            // (git's `blame_date_width`), so shorter renderings are padded out.
            buf.extend_from_slice(ci.display_date.as_bytes());
            pad(
                &mut buf,
                opts.date_mode
                    .width()
                    .saturating_sub(ci.display_date.chars().count()),
            );
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

/// Print git blame's usage text on stderr and yield its exit status (129).
fn print_usage() -> Result<ExitCode> {
    let mut err = std::io::stderr().lock();
    err.write_all(USAGE.as_bytes())?;
    err.flush()?;
    Ok(ExitCode::from(129))
}

/// Outcome of splitting the positionals into `[<rev>...] <file>` and resolving
/// the revision, mirroring `cmd_blame`'s argument grammar in git.
enum Targets {
    /// The positional shape is not a valid blame invocation: print usage (129).
    Usage,
    /// A fatal error (`bad revision` / `More than one commit`) was already
    /// written to stderr; return this exit code (128).
    Fatal(ExitCode),
    /// `opts.file`, `opts.rev` and `opts.suspect_id` are now populated.
    Resolved,
}

/// git's `is_a_rev`: the name resolves to some object in the repository.
fn is_a_rev(repo: &gix::Repository, name: &str) -> bool {
    repo.rev_parse_single(name).is_ok()
}

/// Resolve a revision to the commit it names (peeling tags), or `None` if it is
/// not a valid revision — git's `get_oid` followed by a peel to commit.
fn resolve_commit(repo: &gix::Repository, rev: &str) -> Option<ObjectId> {
    repo.rev_parse_single(rev)
        .ok()?
        .object()
        .ok()?
        .peel_to_commit()
        .ok()
        .map(|c| c.id().detach())
}

/// Split the collected positionals into `[<rev>...] <file>` following git
/// blame's DWIM rules, then resolve the revision. Reproduces `cmd_blame`'s
/// argument handling for the presence/absence of the `--` separator.
fn resolve_targets(repo: &gix::Repository, opts: &mut Options) -> Result<Targets> {
    // Determine the revision arguments (in order) and the single path.
    let (revs, file): (Vec<String>, String) = match opts.post.take() {
        // `--` was present: everything after it is a pathspec. blame accepts
        // exactly one path; a trailing second token is DWIM'd as a revision.
        Some(post) => {
            let pre = std::mem::take(&mut opts.pre);
            match post.len() {
                0 => return Ok(Targets::Usage),
                1 => (pre, post.into_iter().next().unwrap()),
                // `blame -- <file> <rev>`: only legal with no revs before `--`.
                2 if pre.is_empty() => {
                    let mut it = post.into_iter();
                    let file = it.next().unwrap();
                    let rev = it.next().unwrap();
                    (vec![rev], file)
                }
                _ => return Ok(Targets::Usage),
            }
        }
        // No `--`: the last positional is the path, the rest are revisions.
        None => {
            let mut pos = std::mem::take(&mut opts.pre);
            match pos.len() {
                0 => return Ok(Targets::Usage),
                1 => (vec![], pos.pop().unwrap()),
                // Two positionals: `blame <path> <rev>` if the last is a rev,
                // otherwise `blame <rev> <path>`.
                2 => {
                    if is_a_rev(repo, &pos[1]) {
                        let rev = pos.pop().unwrap();
                        let file = pos.pop().unwrap();
                        (vec![rev], file)
                    } else {
                        let file = pos.pop().unwrap();
                        let rev = pos.pop().unwrap();
                        (vec![rev], file)
                    }
                }
                _ => {
                    let file = pos.pop().unwrap();
                    (pos, file)
                }
            }
        }
    };

    // Resolve the revisions in order, matching git: the first that fails to
    // resolve is a `bad revision`; a second one that succeeds is `More than one
    // commit to dig from`.
    let mut suspect: Option<(String, ObjectId)> = None;
    for r in &revs {
        match resolve_commit(repo, r) {
            Some(id) => {
                if let Some((first, _)) = &suspect {
                    let mut err = std::io::stderr().lock();
                    writeln!(err, "fatal: More than one commit to dig from {first} and {r}?")?;
                    err.flush()?;
                    return Ok(Targets::Fatal(ExitCode::from(128)));
                }
                suspect = Some((r.clone(), id));
            }
            None => {
                let mut err = std::io::stderr().lock();
                writeln!(err, "fatal: bad revision '{r}'")?;
                err.flush()?;
                return Ok(Targets::Fatal(ExitCode::from(128)));
            }
        }
    }

    opts.rev = suspect.as_ref().map(|(n, _)| n.clone());
    opts.suspect_id = suspect.map(|(_, id)| id);
    opts.file = file;
    Ok(Targets::Resolved)
}

/// The date-formatting modes zvcs blame reproduces byte-for-byte from git's
/// `show_date`. git accepts a few more (`human`, `format:<strftime>`, and every
/// `-local` variant); those need machinery blame.rs does not have (an strftime
/// renderer, per-timestamp local-timezone conversion), so they are rejected
/// rather than emitting wrong bytes — matching this file's policy for
/// unimplemented features. `relative` is fully supported.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DateMode {
    /// git `DATE_NORMAL` (`default`): `Thu Oct 19 16:00:04 2006 -0700`.
    Normal,
    /// git `DATE_ISO8601` (`iso`/`iso8601`): `2006-10-19 16:00:04 -0700`. blame's default.
    Iso8601,
    /// git `DATE_ISO8601_STRICT` (`iso-strict`): `2006-10-19T16:00:04-07:00` (`Z` at UTC).
    Iso8601Strict,
    /// git `DATE_RFC2822` (`rfc`): `Thu, 19 Oct 2006 16:00:04 -0700`.
    Rfc2822,
    /// git `DATE_SHORT` (`short`): `2006-10-19`.
    Short,
    /// git `DATE_RAW` (`raw`): `1161298804 -0700`.
    Raw,
    /// git `DATE_UNIX` (`unix`): `1161298804`.
    Unix,
    /// git `DATE_RELATIVE` (`relative`): `3 days ago`, computed against the current
    /// time. Independent of the recorded timezone offset.
    Relative,
}

impl DateMode {
    /// git's fixed `blame_date_width` per mode: the width the date column is
    /// left-justified into (`sizeof(reference) - 1`, i.e. the reference length).
    fn width(self) -> usize {
        match self {
            DateMode::Normal => "Thu Oct 19 16:00:04 2006 -0700".len(),
            DateMode::Iso8601 => "2006-10-19 16:00:04 -0700".len(),
            DateMode::Iso8601Strict => "2006-10-19T16:00:04-07:00".len(),
            DateMode::Rfc2822 => "Thu, 19 Oct 2006 16:00:04 -0700".len(),
            DateMode::Short => "2006-10-19".len(),
            DateMode::Raw => "1161298804 -0700".len(),
            DateMode::Unix => "1161298804".len(),
            // git: `utf8_strwidth("4 years, 11 months ago") + 1`, then the shared
            // `blame_date_width -= 1` (strip the NUL) leaves the string width.
            DateMode::Relative => "4 years, 11 months ago".len(),
        }
    }

    /// Render `<seconds> @ <offset>` the way git's `show_date` does for this mode.
    fn format_time(self, seconds: i64, offset: i32) -> String {
        use gix::date::time::format;
        let t = gix::date::Time { seconds, offset };
        match self {
            DateMode::Normal => t.format_or_unix(format::DEFAULT),
            DateMode::Iso8601 => t.format_or_unix(format::ISO8601),
            DateMode::Iso8601Strict => {
                // git prints `Z` for a zero UTC offset; jiff's `%:z` (used by gix)
                // would print `+00:00`, so fix that one case up to match.
                let s = t.format_or_unix(format::ISO8601_STRICT);
                if offset == 0 {
                    if let Some(head) = s.strip_suffix("+00:00") {
                        return format!("{head}Z");
                    }
                }
                s
            }
            // gix's `RFC2822` zero-pads the day; git's `%-d` form (`GIT_RFC2822`)
            // matches git's `show_date` exactly.
            DateMode::Rfc2822 => t.format_or_unix(format::GIT_RFC2822),
            DateMode::Short => t.format_or_unix(format::SHORT),
            DateMode::Raw => t.format_or_unix(format::RAW),
            DateMode::Unix => t.format_or_unix(format::UNIX),
            DateMode::Relative => show_date_relative(seconds),
        }
    }
}

/// git's `show_date_relative` (date.c), via the shared port — so `--date=relative`
/// in blame honors `GIT_TEST_DATE_NOW` and matches every other command exactly.
/// The recorded timezone offset is irrelevant, as in git.
fn show_date_relative(seconds: i64) -> String {
    crate::date::show_date_relative(seconds, crate::date::now_seconds())
}

/// git's `date_mode_type`, restricted to what the parser needs to classify.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DateType {
    Relative,
    Human,
    IsoStrict,
    Iso,
    Rfc,
    Short,
    Normal,
    Raw,
    Unix,
    Strftime,
}

/// The result of classifying a `--date` / blame.date value against git's grammar.
enum DateClass {
    /// A mode blame.rs renders byte-for-byte.
    Supported(DateMode),
    /// A mode git accepts but blame.rs does not implement; carries the effective
    /// format string for the diagnostic.
    Unsupported(String),
    /// Not a recognized git date format → `fatal: unknown date format <s>`.
    UnknownFormat(String),
    /// A `format`/`format-local` mode with no `:` → git's missing-colon fatal.
    MissingColon(String),
}

/// Classify a date-format string exactly the way git's `parse_date_format` /
/// `parse_date_type` do: prefix-match the type in git's order, consume an
/// optional `-local` suffix, then require the remainder to be empty (or a `:`
/// for `format`). `auto:` and the `local` alias are handled first as git does.
fn classify_date(input: &str) -> DateClass {
    // `auto:foo` → foo when stdout is a terminal, else `default`.
    let format = if let Some(rest) = input.strip_prefix("auto:") {
        if std::io::stdout().is_terminal() {
            rest.to_string()
        } else {
            "default".to_string()
        }
    } else {
        input.to_string()
    };
    // Historical alias: `local` means `default-local`.
    let format = if format == "local" {
        "default-local".to_string()
    } else {
        format
    };

    // parse_date_type: first matching prefix wins, in git's exact order.
    let f = format.as_str();
    let (ty, rest) = if let Some(r) = f.strip_prefix("relative") {
        (DateType::Relative, r)
    } else if let Some(r) = f.strip_prefix("iso8601-strict").or_else(|| f.strip_prefix("iso-strict"))
    {
        (DateType::IsoStrict, r)
    } else if let Some(r) = f.strip_prefix("iso8601").or_else(|| f.strip_prefix("iso")) {
        (DateType::Iso, r)
    } else if let Some(r) = f.strip_prefix("rfc2822").or_else(|| f.strip_prefix("rfc")) {
        (DateType::Rfc, r)
    } else if let Some(r) = f.strip_prefix("short") {
        (DateType::Short, r)
    } else if let Some(r) = f.strip_prefix("default") {
        (DateType::Normal, r)
    } else if let Some(r) = f.strip_prefix("human") {
        (DateType::Human, r)
    } else if let Some(r) = f.strip_prefix("raw") {
        (DateType::Raw, r)
    } else if let Some(r) = f.strip_prefix("unix") {
        (DateType::Unix, r)
    } else if let Some(r) = f.strip_prefix("format") {
        (DateType::Strftime, r)
    } else {
        return DateClass::UnknownFormat(format);
    };

    // Optional `-local` suffix sets local mode on any type.
    let (local, rest) = match rest.strip_prefix("-local") {
        Some(r) => (true, r),
        None => (false, rest),
    };

    if ty == DateType::Strftime {
        // `format:<strftime>` requires a colon; the strftime renderer is not
        // implemented, so a valid one is still "unsupported".
        if !rest.starts_with(':') {
            return DateClass::MissingColon(format);
        }
        return DateClass::Unsupported(format);
    }

    // Any other trailing text is not a valid format.
    if !rest.is_empty() {
        return DateClass::UnknownFormat(format);
    }

    // `-local` needs timezone conversion blame.rs does not do.
    if local {
        return DateClass::Unsupported(format);
    }

    match ty {
        DateType::Iso => DateClass::Supported(DateMode::Iso8601),
        DateType::IsoStrict => DateClass::Supported(DateMode::Iso8601Strict),
        DateType::Rfc => DateClass::Supported(DateMode::Rfc2822),
        DateType::Short => DateClass::Supported(DateMode::Short),
        DateType::Normal => DateClass::Supported(DateMode::Normal),
        DateType::Raw => DateClass::Supported(DateMode::Raw),
        DateType::Unix => DateClass::Supported(DateMode::Unix),
        DateType::Relative => DateClass::Supported(DateMode::Relative),
        // `human` needs a time-relative renderer that also folds the current
        // time into local-timezone broken-down form; not implemented.
        DateType::Human => DateClass::Unsupported(format),
        DateType::Strftime => unreachable!("strftime handled above"),
    }
}

/// Outcome of resolving a date-format value: a mode to use, or a fatal exit
/// (git's `128` for a malformed format, already reported to stderr).
enum DateOutcome {
    Mode(DateMode),
    Fatal(ExitCode),
}

/// Resolve a `--date` / blame.date value, reproducing git's fatal messages and
/// exit code for malformed formats and rejecting valid-but-unimplemented modes.
fn resolve_date_mode(input: &str) -> Result<DateOutcome> {
    match classify_date(input) {
        DateClass::Supported(m) => Ok(DateOutcome::Mode(m)),
        DateClass::Unsupported(f) => bail!("unsupported --date mode: {f}"),
        DateClass::UnknownFormat(f) => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "fatal: unknown date format {f}")?;
            err.flush()?;
            Ok(DateOutcome::Fatal(ExitCode::from(128)))
        }
        DateClass::MissingColon(f) => {
            let mut err = std::io::stderr().lock();
            writeln!(err, "fatal: date format missing colon separator: {f}")?;
            err.flush()?;
            Ok(DateOutcome::Fatal(ExitCode::from(128)))
        }
    }
}

struct Options {
    rev: Option<String>,
    file: String,
    suspect_id: Option<ObjectId>,
    /// Positionals before the `--` separator (or all of them if absent).
    pre: Vec<String>,
    /// Positionals after `--`; `None` when no `--` was given.
    post: Option<Vec<String>>,
    ranges: Vec<RangeInclusive<u32>>,
    long: bool,
    suppress: bool,
    show_email: bool,
    show_name: bool,
    show_number: bool,
    abbrev: Option<usize>,
    porcelain: bool,
    line_porcelain: bool,
    /// `-b`: blank the object name of boundary commits instead of showing it.
    blank_boundary: bool,
    /// `--root`: do not treat root commits as boundaries.
    show_root: bool,
    /// `-t`: show the raw timestamp (overrides the resolved date mode).
    raw_timestamp: bool,
    /// `-c`: git-annotate-compatible output format.
    annotate_compat: bool,
    /// `--diff-algorithm=<algo>`; `None` falls back to `diff.algorithm`.
    diff_algorithm: Option<gix::diff::blob::Algorithm>,
    /// `--contents <file>`: use `<file>`'s contents (or stdin for `-`) as the
    /// final image, on top of the suspect commit (default HEAD).
    contents: Option<String>,
    /// Raw `--date` value before repo-side validation; `None` if not given.
    date_arg: Option<String>,
    /// Resolved date mode for the human-format timestamp column, after applying
    /// blame.date and any `--date` override.
    date_mode: DateMode,
}

impl Options {
    fn parse(
        args: &[String],
        show_email_default: bool,
        show_root_default: bool,
        blank_boundary_default: bool,
    ) -> Result<Options> {
        let mut ranges: Vec<RangeInclusive<u32>> = Vec::new();
        let mut long = false;
        let mut suppress = false;
        let mut show_email = show_email_default;
        let mut show_name = false;
        let mut show_number = false;
        let mut abbrev: Option<usize> = None;
        let mut porcelain = false;
        let mut line_porcelain = false;
        let mut blank_boundary = blank_boundary_default;
        let mut show_root = show_root_default;
        let mut raw_timestamp = false;
        let mut annotate_compat = false;
        let mut diff_algorithm: Option<gix::diff::blob::Algorithm> = None;
        let mut contents: Option<String> = None;
        // Raw `--date` value (last one wins); resolved against the repo in `blame`.
        let mut date_arg: Option<String> = None;
        // Positionals before the first `--`; `post` collects those after it.
        // `post.is_some()` means a `--` separator was seen.
        let mut pre: Vec<String> = Vec::new();
        let mut post: Option<Vec<String>> = None;

        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if let Some(paths) = post.as_mut() {
                paths.push(a.to_string());
                i += 1;
                continue;
            }
            match a {
                // The first `--` ends option parsing; everything after it is a
                // pathspec, including a further `--`.
                "--" => post = Some(Vec::new()),
                "-l" | "--long" => long = true,
                "-s" => suppress = true,
                "-e" | "--show-email" => show_email = true,
                "--no-show-email" => show_email = false,
                "-f" | "--show-name" => show_name = true,
                // git's `--[no-]show-name` clears OUTPUT_SHOW_NAME; auto-detection
                // in `find_alignment` still re-shows the column when a rename put a
                // differing source path on a line, exactly as git does.
                "--no-show-name" => show_name = false,
                "-n" | "--show-number" => show_number = true,
                "--no-show-number" => show_number = false,
                // `-b` blanks boundary object names; there is no `--no-b`.
                "-b" => blank_boundary = true,
                // `--root` stops treating root commits as boundaries.
                "--root" => show_root = true,
                "--no-root" => show_root = false,
                // `-t` forces the raw timestamp regardless of the date mode.
                "-t" => raw_timestamp = true,
                // `-c` selects git-annotate-compatible output.
                "-c" => annotate_compat = true,
                "-w" => bail!("unsupported option: -w (ignore whitespace is not implemented)"),
                // git's `--porcelain` and `--line-porcelain` are bit flags on one
                // field, so `--line-porcelain` wins no matter the order.
                "-p" | "--porcelain" => porcelain = true,
                // git's `--no-porcelain` clears only the OUTPUT_PORCELAIN bit,
                // leaving OUTPUT_LINE_PORCELAIN untouched; the output selector keys
                // off OUTPUT_PORCELAIN, so this drops back to the human format.
                "--no-porcelain" => porcelain = false,
                "--line-porcelain" => {
                    porcelain = true;
                    line_porcelain = true;
                }
                // `--line-porcelain`'s OPT_BIT value is OUTPUT_PORCELAIN |
                // OUTPUT_LINE_PORCELAIN, so its `--no-` form clears BOTH bits — even
                // after a bare `-p` (verified against stock git: `-p
                // --no-line-porcelain` yields the human format).
                "--no-line-porcelain" => {
                    porcelain = false;
                    line_porcelain = false;
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
                // `--date <mode>` / `--date=<mode>` set the default date format for
                // the human-format timestamp column (validated against the repo in
                // `blame`, so the last one wins here and errors surface there).
                "--date" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--date` requires a value"))?;
                    date_arg = Some(v.clone());
                }
                _ if a.starts_with("--date=") => {
                    date_arg = Some(a["--date=".len()..].to_string());
                }
                "--diff-algorithm" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--diff-algorithm` requires a value"))?;
                    diff_algorithm = Some(parse_diff_algorithm(v)?);
                }
                _ if a.starts_with("--diff-algorithm=") => {
                    diff_algorithm = Some(parse_diff_algorithm(&a["--diff-algorithm=".len()..])?);
                }
                "--contents" => {
                    i += 1;
                    let v = args
                        .get(i)
                        .ok_or_else(|| anyhow!("option `--contents` requires a value"))?;
                    contents = Some(v.clone());
                }
                _ if a.starts_with("--contents=") => {
                    contents = Some(a["--contents=".len()..].to_string());
                }
                "--no-contents" => contents = None,
                // git's OPT__ABBREV `--no-abbrev` sets abbrev to 0, which its
                // post-parse `else if (!abbrev) abbrev = hexsz` turns into the full
                // hash. `object_name_width` already treats `Some(0)` as "no
                // abbreviation", so `--no-abbrev` is exactly `--abbrev=0` (verified
                // identical to `-l` on stock git).
                "--no-abbrev" => abbrev = Some(0),
                // The `--no-` forms of options whose positive form needs substrate
                // this port does not have (`--incremental`, `--show-stats`,
                // `--progress`, `--score-debug`, `--color-lines`, `--color-by-age`,
                // `--ignore-rev`, `--ignore-revs-file`). Each positive default is
                // off/empty, so the negated form requests exactly the behavior this
                // port already produces; stock git emits byte-identical stdout for
                // them (verified), so they are accepted as no-ops rather than
                // rejected. The positive forms remain refused below.
                "--no-incremental"
                | "--no-show-stats"
                | "--no-progress"
                | "--no-score-debug"
                | "--no-color-lines"
                | "--no-color-by-age"
                | "--no-ignore-rev"
                | "--no-ignore-revs-file" => {}
                _ if a.starts_with("-L") => parse_line_range(&a[2..], &mut ranges)?,
                _ if a.starts_with("--abbrev=") => {
                    let v = &a["--abbrev=".len()..];
                    abbrev = Some(v.parse().map_err(|_| anyhow!("invalid --abbrev value: {v}"))?);
                }
                _ if a.starts_with('-') && a.len() > 1 => {
                    bail!("unsupported option: {a}")
                }
                _ => pre.push(a.to_string()),
            }
            i += 1;
        }

        // The revision/path split and its validation happen against the repo in
        // `resolve_targets`, since git's DWIM (`is_a_rev`) needs the object db.
        Ok(Options {
            rev: None,
            file: String::new(),
            suspect_id: None,
            pre,
            post,
            ranges,
            long,
            suppress,
            show_email,
            show_name,
            show_number,
            abbrev,
            porcelain,
            line_porcelain,
            blank_boundary,
            show_root,
            raw_timestamp,
            annotate_compat,
            diff_algorithm,
            contents,
            date_arg,
            // Overwritten in `blame` once blame.date / `--date` are resolved.
            date_mode: DateMode::Iso8601,
        })
    }
}

/// git's `diff-algorithm` value parser: the names git accepts for `-A/--diff-algorithm`.
/// `patience` is a valid git algorithm the vendored `gix-diff` does not implement, so
/// it is reported as such rather than silently substituted.
fn parse_diff_algorithm(name: &str) -> Result<gix::diff::blob::Algorithm> {
    use gix::diff::blob::Algorithm;
    if name.eq_ignore_ascii_case("myers") || name.eq_ignore_ascii_case("default") {
        Ok(Algorithm::Myers)
    } else if name.eq_ignore_ascii_case("minimal") {
        Ok(Algorithm::MyersMinimal)
    } else if name.eq_ignore_ascii_case("histogram") {
        Ok(Algorithm::Histogram)
    } else if name.eq_ignore_ascii_case("patience") {
        bail!("diff algorithm 'patience' is not implemented")
    } else {
        bail!(
            "option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\""
        )
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

use crate::abbrev::configured_abbrev;

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
