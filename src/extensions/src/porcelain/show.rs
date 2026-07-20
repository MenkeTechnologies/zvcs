use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::BStr;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::objs::tree::EntryKind;
use gix::objs::{Kind, TreeRefIter};
use gix::prelude::ObjectIdExt;

/// git's floor for an abbreviated object id, used for the all-zero side of an
/// `index`/raw line where there is no real object to disambiguate against.
const MINIMUM_ABBREV: usize = 7;

/// The terminal width git assumes for `--stat` when stdout is not a terminal.
const STAT_TERM_WIDTH: usize = 80;

/// `git show` — show one or more objects (commit, tree, blob, or annotated tag).
///
/// Implemented invocation forms:
///   * `git show [<commit>]`  → a commit header in the selected pretty format,
///     followed by the selected diff output against the first parent (root commits
///     diff against the empty tree).
///   * `git show <blob>`      → the raw blob bytes.
///   * `git show <tree>`      → `tree <name-as-given>` then the top-level entry
///     names, directories suffixed with `/`.
///   * `git show <tag>`       → the annotated-tag header, then the object it points to.
///
/// Pretty formats: the default `medium`, `--oneline`, and `--format=`/`--pretty=`
/// with the placeholder subset listed in [`expand_format`]. Any other placeholder
/// is rejected rather than silently dropped.
///
/// Diff output formats: `-p`/`--patch`, `--stat`, `--raw`, `--name-only`, and
/// `-s`/`--no-patch`. Their interaction is git's, reproduced in [`Formats`].
///
/// The patch uses git's default settings: Myers diff with the indent (slider)
/// heuristic, three lines of context, `@@`-hunk function-context, binary-file
/// detection, and the `\ No newline at end of file` marker. Output is never
/// colorized (equivalent to `git --no-color show` / a non-tty pipe).
///
/// Deviations, surfaced rather than faked:
///   * Rename/copy detection is disabled, so a renamed file shows as a delete plus
///     an add instead of git's default `rename from`/`rename to`. Everything emitted
///     is still a correct patch.
///   * Non-ASCII/special paths are emitted verbatim rather than `core.quotePath`-quoted,
///     and `--stat` measures a path in `char`s rather than display columns.
///   * `--stat` assumes an 80-column terminal; `COLUMNS` and `diff.statGraphWidth`
///     are not consulted.
///   * Pathspec limiting and every flag not listed above are rejected explicitly.
pub fn show(args: &[String]) -> Result<ExitCode> {
    let mut specs: Vec<&str> = Vec::new();
    let mut formats = Formats::default();
    let mut pretty = Pretty::Medium;
    let mut after_dashdash = false;

    for a in args {
        if after_dashdash {
            bail!("pathspec limiting is not supported");
        }
        let s = a.as_str();
        match s {
            "--" => after_dashdash = true,
            "-p" | "-u" | "--patch" => formats.patch = true,
            // `-s` resets the diff output format rather than adding to it, which is
            // why `-s --name-only` and `--name-only -s` behave differently.
            "-s" | "--no-patch" => formats = Formats::only_no_output(),
            "--name-only" => formats.name_only = true,
            "--raw" => formats.raw = true,
            "--stat" => formats.stat = true,
            "--oneline" => pretty = Pretty::Oneline,
            // We never colorize; accept the flags that request no/auto color.
            "--no-color" | "--color=never" | "--color=auto" => {}
            _ => {
                if let Some(spec) = s
                    .strip_prefix("--format=")
                    .or_else(|| s.strip_prefix("--pretty="))
                {
                    pretty = parse_pretty(spec)?;
                } else if s.starts_with('-') {
                    bail!("unsupported option {s}");
                } else {
                    specs.push(s);
                }
            }
        }
    }
    if let Pretty::User(fmt) = &pretty {
        // Reject unknown placeholders before any output is produced.
        check_format(fmt)?;
    }
    if specs.is_empty() {
        specs.push("HEAD");
    }

    let repo = gix::discover(".")?;

    // git resolves every revision before rendering anything, so a bad revision
    // produces no stdout at all even when an earlier one was fine.
    let mut resolved: Vec<(&str, ObjectId)> = Vec::with_capacity(specs.len());
    for spec in &specs {
        match repo.rev_parse_single(BStr::new(*spec)) {
            Ok(id) => resolved.push((*spec, id.detach())),
            Err(_) => {
                let hex_len = repo.object_hash().len_in_hex();
                return Ok(fatal(&bad_revision_message(spec, hex_len)));
            }
        }
    }

    let selection = match formats.resolve() {
        Ok(sel) => sel,
        Err(FormatConflict) => {
            return Ok(fatal(
                "options '--name-only', '--name-status', '--check', and '-s' cannot be used together\n",
            ))
        }
    };

    let mut out: Vec<u8> = Vec::new();
    for (spec, id) in &resolved {
        show_one(&repo, &mut out, spec, *id, &pretty, selection)?;
    }

    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(&out).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        // A downstream `| head` closing the pipe is not an error.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        Err(e) => Err(e.into()),
    }
}

/// Print `fatal: <msg>` on stderr and yield git's fatal exit code.
fn fatal(msg: &str) -> ExitCode {
    eprint!("fatal: {msg}");
    ExitCode::from(128)
}

/// git distinguishes a well-formed but absent object id from an unresolvable name:
/// the former is a "bad object", the latter an "ambiguous argument".
fn bad_revision_message(spec: &str, hex_len: usize) -> String {
    if spec.len() == hex_len && spec.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("bad object {spec}\n")
    } else {
        format!(
            "ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'git <command> [<revision>...] -- [<file>...]'\n"
        )
    }
}

// ---------------------------------------------------------------------------
// Pretty formats
// ---------------------------------------------------------------------------

enum Pretty {
    /// git's default `medium`: `commit`/`Merge`/`Author`/`Date` and an indented message.
    Medium,
    /// `<abbrev> <subject>` on one line.
    Oneline,
    /// A `--format=` string with `%` placeholders.
    User(String),
}

fn parse_pretty(spec: &str) -> Result<Pretty> {
    if let Some(fmt) = spec.strip_prefix("format:").or_else(|| spec.strip_prefix("tformat:")) {
        return Ok(Pretty::User(fmt.to_string()));
    }
    match spec {
        "oneline" => Ok(Pretty::Oneline),
        "medium" => Ok(Pretty::Medium),
        _ if spec.contains('%') => Ok(Pretty::User(spec.to_string())),
        _ => bail!("unsupported pretty format {spec}"),
    }
}

/// Reject any placeholder [`expand_format`] does not implement, so an unsupported
/// format fails loudly instead of expanding to something plausible but wrong.
fn check_format(fmt: &str) -> Result<()> {
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            continue;
        }
        match it.next() {
            Some('H' | 'h' | 'T' | 't' | 'P' | 'p' | 's' | 'n' | '%') => {}
            Some('a') => match it.next() {
                Some('n' | 'e') => {}
                Some(x) => bail!("unsupported format placeholder %a{x}"),
                None => bail!("unsupported trailing % in format"),
            },
            Some(x) => bail!("unsupported format placeholder %{x}"),
            None => bail!("unsupported trailing % in format"),
        }
    }
    Ok(())
}

/// Expand the placeholders accepted by [`check_format`] for `commit`.
fn expand_format(out: &mut Vec<u8>, commit: &gix::Commit<'_>, fmt: &str) -> Result<()> {
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match it.next() {
            Some('H') => out.extend_from_slice(commit.id().to_string().as_bytes()),
            Some('h') => out.extend_from_slice(commit.id().shorten_or_id().to_string().as_bytes()),
            Some('T') => out.extend_from_slice(commit.tree_id()?.to_string().as_bytes()),
            Some('t') => {
                out.extend_from_slice(commit.tree_id()?.shorten_or_id().to_string().as_bytes());
            }
            Some('P') => write_parents(out, commit, false),
            Some('p') => write_parents(out, commit, true),
            Some('s') => out.extend_from_slice(&subject(commit.message_raw()?)),
            Some('n') => out.push(b'\n'),
            Some('%') => out.push(b'%'),
            Some('a') => {
                let author = commit.author()?;
                match it.next() {
                    Some('n') => out.extend_from_slice(author.name),
                    Some('e') => out.extend_from_slice(author.email),
                    _ => unreachable!("check_format rejected this already"),
                }
            }
            _ => unreachable!("check_format rejected this already"),
        }
    }
    Ok(())
}

/// Space-separated parent ids, abbreviated for `%p` and full for `%P`.
fn write_parents(out: &mut Vec<u8>, commit: &gix::Commit<'_>, abbrev: bool) {
    for (i, p) in commit.parent_ids().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        let text = if abbrev {
            p.shorten_or_id().to_string()
        } else {
            p.to_string()
        };
        out.extend_from_slice(text.as_bytes());
    }
}

/// git's subject: the first paragraph of the message, folded onto one line.
fn subject(msg: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for line in msg.split(|&b| b == b'\n') {
        let line = trim_end_ws(line);
        if line.is_empty() {
            break;
        }
        if !out.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
    out
}

// ---------------------------------------------------------------------------
// Diff output format selection
// ---------------------------------------------------------------------------

/// The diff output formats requested on the command line, before git's
/// precedence rules are applied.
#[derive(Default, Clone, Copy)]
struct Formats {
    no_output: bool,
    name_only: bool,
    raw: bool,
    stat: bool,
    patch: bool,
}

/// `--name-only` together with `-s` and nothing else: git rejects this outright.
struct FormatConflict;

/// What actually gets rendered after git's precedence rules.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Selection {
    /// `-s` alone: no diff output and no separator after the message.
    Disabled,
    /// `--name-only` wins over every other diff format, whatever the order.
    Names,
    /// Any combination of raw, stat, and patch, rendered in that order.
    Blocks { raw: bool, stat: bool, patch: bool },
}

impl Formats {
    fn only_no_output() -> Self {
        Formats {
            no_output: true,
            ..Formats::default()
        }
    }

    fn any_set(self) -> bool {
        self.no_output || self.name_only || self.raw || self.stat || self.patch
    }

    /// Apply git's precedence: `-s` suppresses output only when it is the sole
    /// format; `--name-only` beats raw/stat/patch; naming both `-s` and
    /// `--name-only` with no third format is an error.
    fn resolve(mut self) -> Result<Selection, FormatConflict> {
        if !self.any_set() {
            self.patch = true;
        }
        if self.no_output && self.name_only && !self.raw && !self.stat && !self.patch {
            return Err(FormatConflict);
        }
        if self.no_output && !self.name_only && !self.raw && !self.stat && !self.patch {
            return Ok(Selection::Disabled);
        }
        if self.name_only {
            return Ok(Selection::Names);
        }
        Ok(Selection::Blocks {
            raw: self.raw,
            stat: self.stat,
            patch: self.patch,
        })
    }
}

// ---------------------------------------------------------------------------
// Object rendering
// ---------------------------------------------------------------------------

/// Render the object `id` (named `spec` on the command line), peeling annotated
/// tags to their target after printing the tag header.
fn show_one(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    spec: &str,
    id: ObjectId,
    pretty: &Pretty,
    selection: Selection,
) -> Result<()> {
    let mut obj = repo.find_object(id)?;
    loop {
        match obj.kind {
            Kind::Blob => {
                out.extend_from_slice(&obj.data);
                break;
            }
            Kind::Tree => {
                show_tree(out, &obj, spec)?;
                break;
            }
            Kind::Commit => {
                let commit = obj.try_into_commit()?;
                show_commit(repo, out, &commit, pretty, selection)?;
                break;
            }
            Kind::Tag => {
                let target = show_tag(out, &obj)?;
                obj = repo.find_object(target)?;
            }
        }
    }
    Ok(())
}

/// `tree <name>` header followed by the top-level entry names. git echoes the name
/// as it was written on the command line, not the resolved object id.
fn show_tree(out: &mut Vec<u8>, obj: &gix::Object<'_>, name: &str) -> Result<()> {
    out.extend_from_slice(b"tree ");
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b"\n\n");
    for entry in TreeRefIter::from_bytes(&obj.data, obj.id.kind()) {
        let entry = entry?;
        out.extend_from_slice(entry.filename);
        if entry.mode.is_tree() {
            out.push(b'/');
        }
        out.push(b'\n');
    }
    Ok(())
}

/// Annotated-tag header. Returns the id of the object the tag points to so the
/// caller can continue rendering the target.
fn show_tag(out: &mut Vec<u8>, obj: &gix::Object<'_>) -> Result<ObjectId> {
    let tag = obj.try_to_tag_ref()?;

    out.extend_from_slice(b"tag ");
    out.extend_from_slice(tag.name);
    out.push(b'\n');

    if let Some(tagger) = tag.tagger()? {
        out.extend_from_slice(b"Tagger: ");
        out.extend_from_slice(tagger.name);
        out.extend_from_slice(b" <");
        out.extend_from_slice(tagger.email);
        out.extend_from_slice(b">\n");
        let date = tagger.time()?.format(gix::date::time::format::DEFAULT)?;
        writeln!(out, "Date:   {date}")?;
    }

    out.push(b'\n');
    // The tag message is printed verbatim (not indented), followed by a blank line.
    out.extend_from_slice(tag.message);
    if !tag.message.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.push(b'\n');

    Ok(tag.target())
}

/// The commit header in the selected pretty format, then the separator, then the
/// selected diff output against the first parent.
fn show_commit(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    pretty: &Pretty,
    selection: Selection,
) -> Result<()> {
    let parents: Vec<_> = commit.parent_ids().collect();
    let is_merge = parents.len() > 1;

    match pretty {
        Pretty::Oneline => {
            out.extend_from_slice(commit.id().shorten_or_id().to_string().as_bytes());
            out.push(b' ');
            out.extend_from_slice(&subject(commit.message_raw()?));
            out.push(b'\n');
        }
        Pretty::User(fmt) => {
            expand_format(out, commit, fmt)?;
            out.push(b'\n');
        }
        Pretty::Medium => {
            writeln!(out, "commit {}", commit.id())?;
            if is_merge {
                out.extend_from_slice(b"Merge:");
                for p in &parents {
                    out.push(b' ');
                    out.extend_from_slice(p.shorten_or_id().to_string().as_bytes());
                }
                out.push(b'\n');
            }

            let author = commit.author()?;
            out.extend_from_slice(b"Author: ");
            out.extend_from_slice(author.name);
            out.extend_from_slice(b" <");
            out.extend_from_slice(author.email);
            out.extend_from_slice(b">\n");
            let date = author.time()?.format(gix::date::time::format::DEFAULT)?;
            writeln!(out, "Date:   {date}")?;
            out.push(b'\n');

            // Message, each line indented four spaces (blank lines become four
            // spaces), with trailing blank lines stripped.
            let msg = commit.message_raw()?;
            for line in trim_trailing_newlines(msg).split(|&b| b == b'\n') {
                out.extend_from_slice(b"    ");
                out.extend_from_slice(line);
                out.push(b'\n');
            }
        }
    }

    if selection == Selection::Disabled {
        return Ok(());
    }

    // Separator between the message and the diff output. For a merge git always
    // uses a blank line; otherwise `--oneline` gets none, and a combined
    // stat-plus-patch gets `---`.
    if is_merge {
        out.push(b'\n');
    } else {
        match (pretty, selection) {
            (Pretty::Oneline, _) => {}
            (
                _,
                Selection::Blocks {
                    stat: true,
                    patch: true,
                    ..
                },
            ) => out.extend_from_slice(b"---\n"),
            _ => out.push(b'\n'),
        }
    }

    // git's default omits everything but `--stat` for a merge commit; what it does
    // show is measured against the first parent.
    if is_merge && !matches!(selection, Selection::Blocks { stat: true, .. }) {
        return Ok(());
    }

    let files = collect_changes(repo, commit, parents.first().map(|p| p.detach()))?;

    match selection {
        Selection::Disabled => {}
        Selection::Names => {
            for f in &files {
                out.extend_from_slice(&f.path);
                out.push(b'\n');
            }
        }
        Selection::Blocks { raw, stat, patch } => {
            let mut wrote_block = false;
            if raw && !is_merge {
                emit_raw(repo, out, &files)?;
                wrote_block = true;
            }
            if stat {
                emit_stat(out, &files)?;
                wrote_block = true;
            }
            if patch && !is_merge {
                if wrote_block {
                    out.push(b'\n');
                }
                for f in &files {
                    emit_patch(repo, out, f)?;
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Change collection
// ---------------------------------------------------------------------------

/// One file-level change, with everything the four output formats need resolved
/// once so the blob contents are read at most a single time.
struct FileChange {
    path: Vec<u8>,
    /// `A`, `D`, `M`, or `T`, as used by `--raw`.
    status: u8,
    /// Octal entry modes; `None` on the side where the path does not exist.
    old_mode: Option<u32>,
    new_mode: Option<u32>,
    old_id: ObjectId,
    new_id: ObjectId,
    old_content: Vec<u8>,
    new_content: Vec<u8>,
    old_is_sub: bool,
    new_is_sub: bool,
    is_binary: bool,
    /// Set when only the mode changed, which suppresses the `index` line and hunks.
    mode_only: bool,
    added: usize,
    deleted: usize,
}

/// Diff `commit`'s tree against `parent`'s (or the empty tree for a root commit),
/// dropping the directory entries gix reports alongside the files it recurses into.
fn collect_changes(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: Option<ObjectId>,
) -> Result<Vec<FileChange>> {
    let new_tree = commit.tree()?;
    let old_tree = match parent {
        Some(pid) => Some(repo.find_object(pid)?.try_into_commit()?.tree()?),
        None => None,
    };

    let mut changes = repo.diff_tree_to_tree(
        old_tree.as_ref(),
        Some(&new_tree),
        gix::diff::Options::default(),
    )?;
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));

    let mut out = Vec::with_capacity(changes.len());
    for change in &changes {
        if let Some(f) = prepare_change(repo, change)? {
            out.push(f);
        }
    }
    Ok(out)
}

/// Turn one gix change into a [`FileChange`], or `None` for the directory entries
/// git does not report (gix emits those *and* recurses into them).
fn prepare_change(repo: &gix::Repository, change: &ChangeDetached) -> Result<Option<FileChange>> {
    let null = ObjectId::null(repo.object_hash());
    let mut f = match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            let is_sub = entry_mode.is_commit();
            FileChange {
                path: location.to_vec(),
                status: b'A',
                old_mode: None,
                new_mode: Some(entry_mode.value().into()),
                old_id: null,
                new_id: *id,
                old_content: Vec::new(),
                new_content: content_of(repo, *id, is_sub)?,
                old_is_sub: false,
                new_is_sub: is_sub,
                is_binary: false,
                mode_only: false,
                added: 0,
                deleted: 0,
            }
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            let is_sub = entry_mode.is_commit();
            FileChange {
                path: location.to_vec(),
                status: b'D',
                old_mode: Some(entry_mode.value().into()),
                new_mode: None,
                old_id: *id,
                new_id: null,
                old_content: content_of(repo, *id, is_sub)?,
                new_content: Vec::new(),
                old_is_sub: is_sub,
                new_is_sub: false,
                is_binary: false,
                mode_only: false,
                added: 0,
                deleted: 0,
            }
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            // A directory whose contents changed; the changed files themselves are
            // reported separately by the recursive walk.
            if entry_mode.is_tree() && previous_entry_mode.is_tree() {
                return Ok(None);
            }
            let old_is_sub = previous_entry_mode.is_commit();
            let new_is_sub = entry_mode.is_commit();
            let status = if type_class(previous_entry_mode.kind()) == type_class(entry_mode.kind()) {
                b'M'
            } else {
                b'T'
            };
            FileChange {
                path: location.to_vec(),
                status,
                old_mode: Some(previous_entry_mode.value().into()),
                new_mode: Some(entry_mode.value().into()),
                old_id: *previous_id,
                new_id: *id,
                old_content: content_of(repo, *previous_id, old_is_sub)?,
                new_content: content_of(repo, *id, new_is_sub)?,
                old_is_sub,
                new_is_sub,
                is_binary: false,
                mode_only: previous_id == id,
                added: 0,
                deleted: 0,
            }
        }
        // Never produced: rewrite tracking is disabled via Options::default().
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    };

    f.is_binary = (!f.old_is_sub && is_binary(&f.old_content))
        || (!f.new_is_sub && is_binary(&f.new_content));
    if !f.is_binary && !f.mode_only {
        let (added, deleted) = count_changed_lines(&f.old_content, &f.new_content)?;
        f.added = added;
        f.deleted = deleted;
    }
    Ok(Some(f))
}

/// git's status letters distinguish a change of file *type* (`T`) from a change of
/// contents or permissions (`M`); regular and executable files are the same type.
fn type_class(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Tree => 0,
        EntryKind::Blob | EntryKind::BlobExecutable => 1,
        EntryKind::Link => 2,
        EntryKind::Commit => 3,
    }
}

// ---------------------------------------------------------------------------
// --raw
// ---------------------------------------------------------------------------

/// `:<old mode> <new mode> <old sha> <new sha> <status>\t<path>`.
fn emit_raw(repo: &gix::Repository, out: &mut Vec<u8>, files: &[FileChange]) -> Result<()> {
    for f in files {
        write!(out, ":{:06o} {:06o} ", f.old_mode.unwrap_or(0), f.new_mode.unwrap_or(0))?;
        let old = short_oid(repo, &f.old_id, f.old_mode.is_none() || f.old_is_sub)?;
        let new = short_oid(repo, &f.new_id, f.new_mode.is_none() || f.new_is_sub)?;
        out.extend_from_slice(old.as_bytes());
        out.push(b' ');
        out.extend_from_slice(new.as_bytes());
        out.push(b' ');
        out.push(f.status);
        out.push(b'\t');
        out.extend_from_slice(&f.path);
        out.push(b'\n');
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// --stat
// ---------------------------------------------------------------------------

/// git's `--stat`: a right-aligned change count and a `+`/`-` bar per file, scaled
/// to fit an 80-column terminal, then a summary line.
fn emit_stat(out: &mut Vec<u8>, files: &[FileChange]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    let mut max_len = 0usize;
    let mut max_change = 0usize;
    let mut number_width = 0usize;
    for f in files {
        max_len = max_len.max(display_width(&f.path));
        if f.is_binary {
            // Change counts are aligned with the literal "Bin" for binary files.
            number_width = 3;
            continue;
        }
        max_change = max_change.max(f.added + f.deleted);
    }
    number_width = number_width.max(decimal_width(max_change));

    let width = STAT_TERM_WIDTH;
    let mut name_width = max_len;
    let mut graph_width = max_change;
    // Fixed overhead per line is 6 columns: " ", " | ", and " " before the bar.
    if name_width + number_width + 6 + graph_width > width {
        let graph_cap = (width * 3 / 8).saturating_sub(number_width + 6);
        if graph_width > graph_cap {
            graph_width = graph_cap.max(6);
        }
        let name_cap = width.saturating_sub(number_width + 6 + graph_width);
        if name_width > name_cap {
            name_width = name_cap;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    let mut total_added = 0usize;
    let mut total_deleted = 0usize;
    for f in files {
        let (prefix, name) = elide_name(&f.path, name_width);
        let padding = name_width.saturating_sub(prefix.len() + display_width(name));
        out.push(b' ');
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&b" ".repeat(padding));
        out.extend_from_slice(b" | ");

        if f.is_binary {
            // For binaries the counts are byte sizes, not lines.
            let old_size = f.old_content.len();
            let new_size = f.new_content.len();
            write!(out, "{:>width$}", "Bin", width = number_width)?;
            if old_size == 0 && new_size == 0 {
                out.push(b'\n');
            } else {
                writeln!(out, " {old_size} -> {new_size} bytes")?;
            }
            continue;
        }

        total_added += f.added;
        total_deleted += f.deleted;
        let change = f.added + f.deleted;
        write!(out, "{change:>number_width$}")?;

        let (mut add, mut del) = (f.added, f.deleted);
        if graph_width < max_change {
            let mut total = scale_linear(add + del, graph_width, max_change);
            if total < 2 && add > 0 && del > 0 {
                total = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = total.saturating_sub(add);
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = total.saturating_sub(del);
            }
        }
        if add > 0 || del > 0 {
            out.push(b' ');
            out.extend_from_slice(&b"+".repeat(add));
            out.extend_from_slice(&b"-".repeat(del));
        }
        out.push(b'\n');
    }

    let n = files.len();
    write!(out, " {n} file{} changed", if n == 1 { "" } else { "s" })?;
    if total_added > 0 || total_deleted == 0 {
        write!(
            out,
            ", {total_added} insertion{}(+)",
            if total_added == 1 { "" } else { "s" }
        )?;
    }
    if total_deleted > 0 || total_added == 0 {
        write!(
            out,
            ", {total_deleted} deletion{}(-)",
            if total_deleted == 1 { "" } else { "s" }
        )?;
    }
    out.push(b'\n');
    Ok(())
}

/// Scale `it` into `width` columns, guaranteeing at least one column for any
/// non-zero value — git widens by one and adds it back for exactly that reason.
fn scale_linear(it: usize, width: usize, max_change: usize) -> usize {
    if it == 0 || max_change == 0 {
        return 0;
    }
    1 + (it * width.saturating_sub(1) / max_change)
}

/// Shorten an over-long path the way git does: a `...` prefix, cut back to a
/// directory boundary when one falls inside the retained tail.
fn elide_name<'p>(path: &'p [u8], name_width: usize) -> (&'static str, &'p [u8]) {
    if display_width(path) <= name_width || name_width < 3 {
        return ("", path);
    }
    let keep = name_width - 3;
    let mut tail = &path[path.len() - keep..];
    if let Some(slash) = tail.iter().position(|&b| b == b'/') {
        tail = &tail[slash..];
    }
    ("...", tail)
}

fn decimal_width(mut n: usize) -> usize {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

/// Approximate display width. Paths are treated as UTF-8 and counted in `char`s,
/// which matches git for everything but wide and combining characters.
fn display_width(path: &[u8]) -> usize {
    String::from_utf8_lossy(path).chars().count()
}

// ---------------------------------------------------------------------------
// -p / --patch
// ---------------------------------------------------------------------------

/// Render one file-level change as a `diff --git` block.
fn emit_patch(repo: &gix::Repository, out: &mut Vec<u8>, f: &FileChange) -> Result<()> {
    emit_git_header(out, &f.path);

    match (f.old_mode, f.new_mode) {
        (None, Some(new)) => writeln!(out, "new file mode {new:o}")?,
        (Some(old), None) => writeln!(out, "deleted file mode {old:o}")?,
        (Some(old), Some(new)) if old != new => {
            writeln!(out, "old mode {old:o}")?;
            writeln!(out, "new mode {new:o}")?;
        }
        _ => {}
    }

    // A pure mode change (identical content) prints no index line and no hunks.
    if f.mode_only {
        return Ok(());
    }

    let old_short = short_oid(repo, &f.old_id, f.old_mode.is_none() || f.old_is_sub)?;
    let new_short = short_oid(repo, &f.new_id, f.new_mode.is_none() || f.new_is_sub)?;
    match (f.old_mode, f.new_mode) {
        // The mode suffix is dropped when a mode change was already reported above.
        (Some(old), Some(new)) if old == new => writeln!(out, "index {old_short}..{new_short} {new:o}")?,
        _ => writeln!(out, "index {old_short}..{new_short}")?,
    }

    let old_path = f.old_mode.map(|_| f.path.as_slice());
    let new_path = f.new_mode.map(|_| f.path.as_slice());
    if f.is_binary {
        emit_binary_line(out, old_path, new_path);
        return Ok(());
    }
    emit_body(out, old_path, new_path, &f.old_content, &f.new_content)
}

/// `diff --git a/<path> b/<path>` line, preserving raw path bytes.
fn emit_git_header(out: &mut Vec<u8>, path: &[u8]) {
    out.extend_from_slice(b"diff --git a/");
    out.extend_from_slice(path);
    out.extend_from_slice(b" b/");
    out.extend_from_slice(path);
    out.push(b'\n');
}

/// `Binary files <a> and <b> differ`, where a `None` side is `/dev/null`.
fn emit_binary_line(out: &mut Vec<u8>, old: Option<&[u8]>, new: Option<&[u8]>) {
    out.extend_from_slice(b"Binary files ");
    match old {
        Some(p) => {
            out.extend_from_slice(b"a/");
            out.extend_from_slice(p);
        }
        None => out.extend_from_slice(b"/dev/null"),
    }
    out.extend_from_slice(b" and ");
    match new {
        Some(p) => {
            out.extend_from_slice(b"b/");
            out.extend_from_slice(p);
        }
        None => out.extend_from_slice(b"/dev/null"),
    }
    out.extend_from_slice(b" differ\n");
}

/// Emit the `---`/`+++` file headers and hunks, but only when there is an actual
/// textual change (an empty-file add/delete produces no header lines, like git).
fn emit_body(
    out: &mut Vec<u8>,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    old_content: &[u8],
    new_content: &[u8],
) -> Result<()> {
    let mut hunks: Vec<u8> = Vec::new();
    emit_text_hunks(&mut hunks, old_content, new_content)?;
    if hunks.is_empty() {
        return Ok(());
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
    Ok(())
}

/// Compute the unified diff of two blobs into `out` using git's default settings.
fn emit_text_hunks(out: &mut Vec<u8>, old: &[u8], new: &[u8]) -> Result<()> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let before_lines: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();
    let writer = HunkWriter { out, before_lines };
    UnifiedDiff::new(&diff, &input, writer, ContextSize::symmetrical(3)).consume()?;
    Ok(())
}

/// Total added and removed lines, for `--stat`. Uses the same hunk machinery as
/// the patch so the two can never disagree about what changed.
fn count_changed_lines(old: &[u8], new: &[u8]) -> Result<(usize, usize)> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let counter = LineCounter {
        added: 0,
        deleted: 0,
    };
    Ok(UnifiedDiff::new(&diff, &input, counter, ContextSize::symmetrical(3)).consume()?)
}

/// Counts changed lines, ignoring context.
struct LineCounter {
    added: usize,
    deleted: usize,
}

impl ConsumeHunk for LineCounter {
    type Out = (usize, usize);

    fn consume_hunk(&mut self, _header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        for &(kind, _) in lines {
            match kind {
                DiffLineKind::Add => self.added += 1,
                DiffLineKind::Remove => self.deleted += 1,
                DiffLineKind::Context => {}
            }
        }
        Ok(())
    }

    fn finish(self) -> (usize, usize) {
        (self.added, self.deleted)
    }
}

/// Writes each hunk in git's unified-diff style: `@@ -a +b @@ <func>` headers with
/// the git length-1 abbreviation, per-line prefixes, and the no-newline marker.
struct HunkWriter<'a> {
    out: &'a mut Vec<u8>,
    /// Pre-image lines, for resolving the function context of each hunk header.
    before_lines: Vec<&'a [u8]>,
}

impl<'a> HunkWriter<'a> {
    /// Find the nearest "function" line above the hunk's leading context, mirroring
    /// git's default (no `xfuncname`) heuristic: a line whose first byte is a letter,
    /// `_`, or `$`. Returns the trimmed line, or `None` if none is found.
    fn find_func(&self, before_hunk_start: u32) -> Option<&'a [u8]> {
        // 0-based index of the hunk's first shown line; scan strictly above it.
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

impl<'a> ConsumeHunk for HunkWriter<'a> {
    type Out = ();

    fn consume_hunk(&mut self, header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
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
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
            self.out.extend_from_slice(content);
            if !content.ends_with(b"\n") {
                self.out.push(b'\n');
                self.out.extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) {}
}

/// git omits the `,len` field when the hunk spans exactly one line, and points an
/// empty side at the line *before* the change rather than at a line that is not there.
fn write_range(out: &mut Vec<u8>, start: u32, len: u32) {
    match len {
        0 => {
            let _ = write!(out, "{},0", start.saturating_sub(1));
        }
        1 => {
            let _ = write!(out, "{start}");
        }
        _ => {
            let _ = write!(out, "{start},{len}");
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// The bytes to diff for an entry: a real blob is read from the object database; a
/// submodule (commit entry) is rendered as its `Subproject commit <oid>` line.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// Abbreviated object id for the `index` and raw lines. Real objects are
/// disambiguated against the odb, as git's `diff_unique_abbrev` does; an absent
/// side is all zeros, and a submodule commit (which this odb does not have) is
/// plainly truncated.
fn short_oid(repo: &gix::Repository, id: &ObjectId, plain: bool) -> Result<String> {
    if id.is_null() {
        return Ok("0".repeat(MINIMUM_ABBREV));
    }
    if plain {
        return Ok(id.to_hex_with_len(MINIMUM_ABBREV).to_string());
    }
    Ok(id.attach(repo).shorten()?.to_string())
}

/// git's binary heuristic: a NUL byte within the first 8000 bytes.
fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
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

/// Strip trailing newlines (`\n`/`\r`) — used to trim a commit message before indenting.
fn trim_trailing_newlines(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last == b'\n' || last == b'\r' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

/// Strip trailing whitespace (git trims the function-context line this way).
fn trim_end_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last == b'\n' || last == b'\r' || last == b' ' || last == b'\t' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}
