use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::BStr;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::prelude::ObjectIdExt;
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::objs::{Kind, TreeRefIter};

/// `git show` — show one or more objects (commit, tree, blob, or annotated tag).
///
/// Implemented invocation forms, matching stock `git show` byte-for-byte for the
/// common cases:
///   * `git show [<commit>]`  → `commit`/`Author`/`Date` header, indented message,
///     and the patch against the first parent (root commits diff against the empty
///     tree). Merge commits print the `Merge:` line and message but no diff, exactly
///     like git's default.
///   * `git show <blob>`      → the raw blob bytes.
///   * `git show <tree>`      → `tree <oid>` header then the top-level entry names,
///     directories suffixed with `/`.
///   * `git show <tag>`       → the annotated-tag header, then the object it points to.
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
///   * Non-ASCII/special paths are emitted verbatim rather than `core.quotePath`-quoted.
///   * Pathspec limiting and most flags are unsupported and rejected explicitly.
pub fn show(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let mut specs: Vec<&str> = Vec::new();
    let mut with_diff = true;
    let mut after_dashdash = false;
    for a in args {
        if after_dashdash {
            bail!("pathspec limiting is not supported");
        }
        match a.as_str() {
            "--" => after_dashdash = true,
            "-p" | "-u" | "--patch" => with_diff = true,
            "-s" | "--no-patch" => with_diff = false,
            // We never colorize; accept the flags that request no/auto color.
            "--no-color" | "--color=never" | "--color=auto" => {}
            s if s.starts_with('-') => bail!("unsupported option {s}"),
            s => specs.push(s),
        }
    }
    if specs.is_empty() {
        specs.push("HEAD");
    }

    let mut out: Vec<u8> = Vec::new();
    for spec in &specs {
        show_one(&repo, &mut out, spec, with_diff)?;
    }

    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(&out).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        // A downstream `| head` closing the pipe is not an error.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        Err(e) => Err(e.into()),
    }
}

/// Resolve `spec` and render the object it names, peeling annotated tags to their
/// target after printing the tag header.
fn show_one(repo: &gix::Repository, out: &mut Vec<u8>, spec: &str, with_diff: bool) -> Result<()> {
    let id = repo.rev_parse_single(BStr::new(spec))?;
    let mut obj = id.object()?;
    loop {
        match obj.kind {
            Kind::Blob => {
                out.extend_from_slice(&obj.data);
                break;
            }
            Kind::Tree => {
                show_tree(out, &obj)?;
                break;
            }
            Kind::Commit => {
                let commit = obj.try_into_commit()?;
                show_commit(repo, out, &commit, with_diff)?;
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

/// `tree <oid>` header followed by the top-level entry names.
fn show_tree(out: &mut Vec<u8>, obj: &gix::Object<'_>) -> Result<()> {
    out.extend_from_slice(b"tree ");
    out.extend_from_slice(obj.id.to_hex().to_string().as_bytes());
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

/// `commit`/`Author`/`Date` header, indented message, and (for a non-merge commit
/// when `with_diff`) the patch against the first parent.
fn show_commit(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    with_diff: bool,
) -> Result<()> {
    writeln!(out, "commit {}", commit.id())?;

    let parents: Vec<_> = commit.parent_ids().collect();
    if parents.len() > 1 {
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

    // Message, each line indented four spaces (blank lines become four spaces),
    // trailing blank lines stripped, then a single blank line before the patch.
    let msg = commit.message_raw()?;
    for line in trim_trailing_newlines(msg).split(|&b| b == b'\n') {
        out.extend_from_slice(b"    ");
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out.push(b'\n');

    // git's default omits the diff for merge commits.
    if with_diff && parents.len() <= 1 {
        let new_tree = commit.tree()?;
        let old_tree = match parents.first() {
            Some(pid) => Some(pid.object()?.try_into_commit()?.tree()?),
            None => None,
        };
        let abbrev = new_tree.id().shorten()?.hex_len();

        let mut changes =
            repo.diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), gix::diff::Options::default())?;
        changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));
        for change in &changes {
            emit_change(repo, out, change, abbrev)?;
        }
    }

    Ok(())
}

/// Render one file-level change as a `diff --git` block.
fn emit_change(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    change: &ChangeDetached,
    abbrev: usize,
) -> Result<()> {
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path);
            writeln!(out, "new file mode {:o}", entry_mode.value())?;
            let is_sub = entry_mode.is_commit();
            let content = content_of(repo, *id, is_sub)?;
            let short = short_oid(repo, *id, abbrev, is_sub)?;
            writeln!(out, "index {}..{}", "0".repeat(short.len()), short)?;
            if !is_sub && is_binary(&content) {
                emit_binary_line(out, None, Some(path));
                return Ok(());
            }
            emit_body(out, None, Some(path), &[], &content)?;
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path);
            writeln!(out, "deleted file mode {:o}", entry_mode.value())?;
            let is_sub = entry_mode.is_commit();
            let content = content_of(repo, *id, is_sub)?;
            let short = short_oid(repo, *id, abbrev, is_sub)?;
            writeln!(out, "index {}..{}", short, "0".repeat(short.len()))?;
            if !is_sub && is_binary(&content) {
                emit_binary_line(out, Some(path), None);
                return Ok(());
            }
            emit_body(out, Some(path), None, &content, &[])?;
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path);
            let old_mode = format!("{:o}", previous_entry_mode.value());
            let new_mode = format!("{:o}", entry_mode.value());
            let mode_changed = old_mode != new_mode;
            if mode_changed {
                writeln!(out, "old mode {old_mode}")?;
                writeln!(out, "new mode {new_mode}")?;
            }
            // A pure mode change (identical content) prints no index/hunks.
            if previous_id == id {
                return Ok(());
            }

            let old_is_sub = previous_entry_mode.is_commit();
            let new_is_sub = entry_mode.is_commit();
            let old_content = content_of(repo, *previous_id, old_is_sub)?;
            let new_content = content_of(repo, *id, new_is_sub)?;
            let old_short = short_oid(repo, *previous_id, abbrev, old_is_sub)?;
            let new_short = short_oid(repo, *id, abbrev, new_is_sub)?;
            // The mode suffix on the index line is dropped when a mode change was
            // already reported via `old mode`/`new mode`.
            if mode_changed {
                writeln!(out, "index {old_short}..{new_short}")?;
            } else {
                writeln!(out, "index {old_short}..{new_short} {new_mode}")?;
            }

            let binary = (!old_is_sub && is_binary(&old_content)) || (!new_is_sub && is_binary(&new_content));
            if binary {
                emit_binary_line(out, Some(path), Some(path));
                return Ok(());
            }
            emit_body(out, Some(path), Some(path), &old_content, &new_content)?;
        }
        // Never produced: rewrite tracking is disabled via Options::default().
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    }
    Ok(())
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

/// git omits the `,len` field when the hunk spans exactly one line.
fn write_range(out: &mut Vec<u8>, start: u32, len: u32) {
    if len == 1 {
        let _ = write!(out, "{start}");
    } else {
        let _ = write!(out, "{start},{len}");
    }
}

/// The bytes to diff for an entry: a real blob is read from the object database; a
/// submodule (commit entry) is rendered as its `Subproject commit <oid>` line.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// Abbreviated object id for the `index` line. Real objects are disambiguated
/// against the odb; a submodule commit (absent from this odb) is plainly truncated
/// to the diff's abbreviation length, matching git.
fn short_oid(repo: &gix::Repository, id: ObjectId, abbrev: usize, is_submodule: bool) -> Result<String> {
    if is_submodule {
        Ok(id.to_hex_with_len(abbrev).to_string())
    } else {
        Ok(id.attach(repo).shorten()?.to_string())
    }
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
