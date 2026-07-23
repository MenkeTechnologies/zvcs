use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::{BStr, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::objs::tree::EntryKind;
use gix::objs::{Kind, TreeRefIter};
use gix::prelude::ObjectIdExt;
use gix::revision::plumbing::Spec as RevSpec;

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
///
/// Revision arguments accept the full walk grammar: plain names are shown directly
/// (deduplicated per commit, in argument order), while ranges (`a..b`), symmetric
/// differences (`a...b`), and exclusions (`^a`) drive a revision walk. Pathspecs
/// after `--` limit each commit's diff by plain path prefix (pathspec magic is not
/// interpreted). Every flag not listed above is rejected explicitly.
pub fn show(args: &[String]) -> Result<ExitCode> {
    let mut specs: Vec<&str> = Vec::new();
    let mut pathspecs: Vec<Vec<u8>> = Vec::new();
    let mut formats = Formats::default();
    let mut pretty = Pretty::Medium;
    let mut after_dashdash = false;
    // Display config shared with `git log`, overridable on the command line. The
    // config defaults are resolved after the repo is discovered; these hold the
    // CLI overrides in the meantime (`None` = fall back to config).
    let mut cli_abbrev: Option<bool> = None;
    let mut cli_date: Option<DateMode> = None;
    let mut force_root = false;
    let mut first_parent = false;
    // Pickaxe search (`-S<string>` / `-G<regex>`), which limits the shown diff to
    // the file pairs whose change text matches — git-fuzzy's in-commit search uses
    // `-G <query>`. `pending_pickaxe` holds the kind while the separate value form
    // (`-G` then the query in the next argv token) waits for that value.
    let mut pickaxe_s: Option<String> = None;
    let mut pickaxe_g: Option<String> = None;
    let mut pending_pickaxe: Option<char> = None;

    for a in args {
        let s = a.as_str();
        // Everything after `--` is a pathspec, even tokens that look like flags:
        // `git show -- --stat` limits by the path `--stat`, it does not enable stat.
        if after_dashdash {
            pathspecs.push(a.as_bytes().to_vec());
            continue;
        }
        if let Some(kind) = pending_pickaxe.take() {
            match kind {
                'S' => pickaxe_s = Some(a.clone()),
                _ => pickaxe_g = Some(a.clone()),
            }
            continue;
        }
        match s {
            "-S" => pending_pickaxe = Some('S'),
            "-G" => pending_pickaxe = Some('G'),
            "--" => after_dashdash = true,
            "-p" | "-u" | "--patch" => formats.patch = true,
            // `-s` resets the diff output format rather than adding to it, which is
            // why `-s --name-only` and `--name-only -s` behave differently.
            "-s" | "--no-patch" => formats = Formats::only_no_output(),
            "--name-only" => formats.name_only = true,
            "--raw" => formats.raw = true,
            "--stat" => formats.stat = true,
            "--oneline" => pretty = Pretty::Oneline,
            // `log.abbrevCommit`/`log.date`/`log.showRoot` overrides, mirroring
            // `git log`. There is no `--no-root`; `--root` only forces it on.
            "--abbrev-commit" => cli_abbrev = Some(true),
            "--no-abbrev-commit" => cli_abbrev = Some(false),
            "--root" => force_root = true,
            // `--first-parent`: follow only the first parent in a walk, and show a
            // merge as a plain diff against its first parent instead of the dense
            // combined (`--cc`) diff. A no-op for a single non-merge commit.
            "--first-parent" => first_parent = true,
            "--no-first-parent" => first_parent = false,
            // We never colorize; accept the flags that request no/auto color.
            "--no-color" | "--color=never" | "--color=auto" => {}
            _ => {
                if let Some(v) = s.strip_prefix("--date=") {
                    match parse_date_mode(v) {
                        Some(m) => cli_date = Some(m),
                        None => return Ok(fatal(&format!("unknown date format {v}\n"))),
                    }
                } else if let Some(spec) = s
                    .strip_prefix("--format=")
                    .or_else(|| s.strip_prefix("--pretty="))
                {
                    // git validates each `--pretty`/`--format` occurrence eagerly,
                    // before resolving any revision, and rejects an invalid one
                    // wherever it appears with exit 128.
                    match parse_pretty(spec) {
                        Some(p) => pretty = p,
                        None => return Ok(fatal(&format!("invalid --pretty format: {spec}\n"))),
                    }
                } else if let Some(v) = s.strip_prefix("-S") {
                    pickaxe_s = Some(v.to_string());
                } else if let Some(v) = s.strip_prefix("-G") {
                    pickaxe_g = Some(v.to_string());
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
    let hex_len = repo.object_hash().len_in_hex();

    // Config supplies the defaults for the display knobs `git show` shares with
    // `git log`; the CLI flags parsed above win where present. git reads these in
    // `git_log_config` and validates `log.date` there — an invalid value is fatal
    // even when `--date` later overrides it, so it is checked unconditionally.
    let (abbrev_commit, date_mode, show_root) = {
        let snap = repo.config_snapshot();
        let cfg_abbrev = snap.boolean("log.abbrevCommit").unwrap_or(false);
        // `log.showRoot` defaults to true: a root commit is shown as a creation
        // event (its diff against the empty tree). `--root` forces it on; there is
        // no `--no-root`, so config is the only way to suppress the root diff.
        let cfg_show_root = snap.boolean("log.showRoot").unwrap_or(true);
        let cfg_date = match snap.string("log.date") {
            Some(v) => {
                let v = v.to_str_lossy();
                match parse_date_mode(&v) {
                    Some(m) => m,
                    None => return Ok(fatal(&format!("unknown date format {v}\n"))),
                }
            }
            None => DateMode::Default,
        };
        (
            cli_abbrev.unwrap_or(cfg_abbrev),
            cli_date.unwrap_or(cfg_date),
            force_root || cfg_show_root,
        )
    };

    // git resolves every revision before rendering anything, so a bad revision
    // produces no stdout at all even when an earlier one was fine. Ranges (`a..b`),
    // symmetric differences (`a...b`), and exclusions (`^a`) turn the request into
    // a revision walk; plain object names are shown directly.
    let mut walk_tips: Vec<ObjectId> = Vec::new();
    let mut walk_hidden: Vec<ObjectId> = Vec::new();
    let mut plain: Vec<(&str, ObjectId)> = Vec::new();
    let mut needs_walk = false;
    for spec in &specs {
        let parsed = match repo.rev_parse(BStr::new(*spec)) {
            Ok(p) => p.detach(),
            Err(_) => return Ok(fatal(&bad_revision_message(spec, hex_len))),
        };
        match parsed {
            RevSpec::Include(id) | RevSpec::ExcludeParents(id) => {
                plain.push((*spec, id));
                walk_tips.push(id);
            }
            RevSpec::Exclude(id) => {
                needs_walk = true;
                walk_hidden.push(id);
            }
            RevSpec::Range { from, to } => {
                needs_walk = true;
                walk_tips.push(to);
                walk_hidden.push(from);
            }
            RevSpec::Merge { theirs, ours } => {
                // `theirs...ours` = reachable from either but not both, which git
                // computes as `theirs ours --not $(merge-base theirs ours)`.
                needs_walk = true;
                walk_tips.push(theirs);
                walk_tips.push(ours);
                for mb in repo.merge_bases_many(theirs, &[ours])? {
                    walk_hidden.push(mb.detach());
                }
            }
            RevSpec::IncludeOnlyParents(id) => {
                needs_walk = true;
                let commit = repo.find_object(id)?.try_into_commit()?;
                for p in commit.parent_ids() {
                    walk_tips.push(p.detach());
                }
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

    // Compile `-G` once, in git's default (basic-regex) dialect, matching `git log`.
    let pickaxe = Pickaxe {
        s: pickaxe_s,
        g: match &pickaxe_g {
            Some(p) => Some(crate::revfilter::build_regex(
                p,
                crate::revfilter::Dialect::Basic,
                false,
            )?),
            None => None,
        },
    };

    let mut out: Vec<u8> = Vec::new();
    // git marks each commit it prints as SHOWN, so a commit named twice (or reached
    // twice by a walk) is printed once. Blobs, trees, and tags are not deduplicated.
    let mut shown: Vec<ObjectId> = Vec::new();
    if needs_walk {
        let mut platform = repo.rev_walk(walk_tips).with_hidden(walk_hidden);
        if first_parent {
            platform = platform.first_parent_only();
        }
        let walk = platform.all()?;
        for info in walk {
            let id = info?.id;
            let disp = DisplayOpts { abbrev_commit, date_mode, show_root, first_parent };
            show_one(&repo, &mut out, &id.to_string(), id, &pretty, selection, &pathspecs, &disp, &pickaxe, &mut shown)?;
        }
    } else {
        for (spec, id) in &plain {
            let disp = DisplayOpts { abbrev_commit, date_mode, show_root, first_parent };
            show_one(&repo, &mut out, spec, *id, &pretty, selection, &pathspecs, &disp, &pickaxe, &mut shown)?;
        }
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

/// Parse a `--pretty`/`--format` value, or `None` when git would reject it with
/// `fatal: invalid --pretty format: <spec>`. git's rule: a `format:`/`tformat:`
/// prefix or any `%` placeholder is a user format; the empty string is an empty
/// user format (prints nothing); a known format name is that format; anything
/// else is invalid.
fn parse_pretty(spec: &str) -> Option<Pretty> {
    if let Some(fmt) = spec.strip_prefix("format:").or_else(|| spec.strip_prefix("tformat:")) {
        return Some(Pretty::User(fmt.to_string()));
    }
    match spec {
        "" => Some(Pretty::User(String::new())),
        "oneline" => Some(Pretty::Oneline),
        "medium" => Some(Pretty::Medium),
        _ if spec.contains('%') => Some(Pretty::User(spec.to_string())),
        _ => None,
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

/// The display knobs `git show` shares with `git log`, resolved once from config
/// and the command line.
#[derive(Clone, Copy)]
struct DisplayOpts {
    /// `log.abbrevCommit` / `--abbrev-commit`: abbreviate the `commit <id>` line.
    abbrev_commit: bool,
    /// `log.date` / `--date=<mode>`: the format of the `Date:` line.
    date_mode: DateMode,
    /// `log.showRoot` / `--root`: whether a root commit's diff against the empty
    /// tree is shown (default true).
    show_root: bool,
    /// `--first-parent`: render a merge as a plain diff against its first parent
    /// rather than the dense combined (`--cc`) diff.
    first_parent: bool,
}

/// Pickaxe search (`-S`/`-G`): limits the shown diff to file pairs whose change
/// text matches, as git's `diffcore-pickaxe` does. Filtering is per file, so a
/// commit that touched several files shows only the ones that match.
struct Pickaxe {
    /// `-S<string>`: a filepair hits when the string's count differs between the
    /// two sides (a net add or remove).
    s: Option<String>,
    /// `-G<regex>`: a filepair hits when any added/removed line matches.
    g: Option<regex::bytes::Regex>,
}

impl Pickaxe {
    fn active(&self) -> bool {
        self.s.is_some() || self.g.is_some()
    }
}

/// Render the object `id` (named `spec` on the command line), peeling annotated
/// tags to their target after printing the tag header.
fn show_one(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    spec: &str,
    id: ObjectId,
    pretty: &Pretty,
    selection: Selection,
    pathspecs: &[Vec<u8>],
    disp: &DisplayOpts,
    pickaxe: &Pickaxe,
    shown: &mut Vec<ObjectId>,
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
                // git prints a given commit at most once (the SHOWN flag).
                if shown.contains(&obj.id) {
                    break;
                }
                shown.push(obj.id);
                let commit = obj.try_into_commit()?;
                show_commit(repo, out, &commit, pretty, selection, pathspecs, disp, pickaxe)?;
                break;
            }
            Kind::Tag => {
                let target = show_tag(out, &obj, disp.date_mode)?;
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
fn show_tag(out: &mut Vec<u8>, obj: &gix::Object<'_>, date_mode: DateMode) -> Result<ObjectId> {
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
        let t = tagger.time()?;
        let date = format_date(t.seconds, t.offset, date_mode);
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
    pathspecs: &[Vec<u8>],
    disp: &DisplayOpts,
    pickaxe: &Pickaxe,
) -> Result<()> {
    let parents: Vec<_> = commit.parent_ids().collect();
    let is_merge = parents.len() > 1;
    // An empty user format (`--format=`) prints no header at all, and git then
    // omits the blank line that would separate the header from the diff.
    let header_empty = matches!(pretty, Pretty::User(f) if f.is_empty());

    match pretty {
        Pretty::Oneline => {
            out.extend_from_slice(commit.id().shorten_or_id().to_string().as_bytes());
            out.push(b' ');
            out.extend_from_slice(&subject(commit.message_raw()?));
            out.push(b'\n');
        }
        Pretty::User(fmt) => {
            expand_format(out, commit, fmt)?;
            // A `tformat` (the default for `--format=`) terminates each non-empty
            // entry with a newline; the empty format terminates nothing.
            if !fmt.is_empty() {
                out.push(b'\n');
            }
        }
        Pretty::Medium => {
            // `log.abbrevCommit`/`--abbrev-commit` shortens the `commit` line; the
            // `Merge:` parents are always abbreviated, as in git.
            if disp.abbrev_commit {
                writeln!(out, "commit {}", commit.id().shorten_or_id())?;
            } else {
                writeln!(out, "commit {}", commit.id())?;
            }
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
            let t = author.time()?;
            let date = format_date(t.seconds, t.offset, disp.date_mode);
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

    // `log.showRoot=false` (with no `--root`) suppresses a root commit's diff
    // against the empty tree: the header prints, but no separator and no diff.
    if parents.is_empty() && !disp.show_root {
        return Ok(());
    }

    // Resolve the file-level changes up front: whether any survive the pathspec
    // filter decides whether a separator is printed at all.
    let mut files = collect_changes(repo, commit, parents.first().map(|p| p.detach()))?;
    if !pathspecs.is_empty() {
        files.retain(|f| matches_pathspec(&f.path, pathspecs));
    }
    // `-S`/`-G` (pickaxe): keep only files whose own change text matches, rendering
    // each file's patch and testing it exactly as `git log` tests a commit's patch.
    // Applies to the first-parent / non-merge path; a merge's combined `--cc` diff
    // (never paired with pickaxe by git-fuzzy) is left unfiltered.
    if pickaxe.active() && !(is_merge && !disp.first_parent) {
        files.retain(|f| {
            let mut buf = Vec::new();
            emit_patch(repo, &mut buf, f).is_ok()
                && super::log::pickaxe_hit(&buf, pickaxe.s.as_deref(), pickaxe.g.as_ref())
        });
    }

    if is_merge && !disp.first_parent {
        // git shows a blank line after a merge's message regardless of format,
        // then — by default — the dense combined diff (`--cc`) against all
        // parents, plus `--stat` (against the first parent) when requested. The
        // empty user format prints neither the blank line nor a header.
        // `--first-parent` opts out: the merge falls through to the plain
        // single-parent path below, diffing against `parents[0]` like any commit.
        if !header_empty {
            out.push(b'\n');
        }
        match selection {
            Selection::Blocks { stat, patch, .. } => {
                let mut wrote = false;
                if stat {
                    emit_stat(out, &files)?;
                    wrote = true;
                }
                if patch {
                    // Dense combined diff of the merge's tree against every
                    // parent tree, rendered by the shared `diff --cc` engine.
                    let result_tree = commit.tree()?;
                    let mut parent_trees = Vec::with_capacity(parents.len());
                    for p in &parents {
                        parent_trees.push(repo.find_commit(p.detach())?.tree()?);
                    }
                    let ps: Vec<String> = pathspecs
                        .iter()
                        .map(|p| String::from_utf8_lossy(p).into_owned())
                        .collect();
                    let cc = super::diff::combined_trees_patch(
                        repo,
                        &result_tree,
                        &parent_trees,
                        &ps,
                        3,
                    )?;
                    if wrote && !cc.is_empty() {
                        out.push(b'\n');
                    }
                    out.extend_from_slice(&cc);
                }
            }
            _ => {}
        }
        return Ok(());
    }

    // A pathspec that matched nothing leaves the message with no diff and, like git,
    // no trailing separator.
    if files.is_empty() {
        return Ok(());
    }

    // Separator between the message and the diff output. `--oneline` and the empty
    // user format get none; a combined stat-plus-patch gets `---`; otherwise a
    // blank line.
    if !header_empty {
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
            if raw {
                emit_raw(repo, out, &files)?;
                wrote_block = true;
            }
            if stat {
                emit_stat(out, &files)?;
                wrote_block = true;
            }
            if patch {
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

/// git limits a commit's diff to paths matching the pathspecs after `--`. Without
/// pathspec magic (`:(glob)`, `:!`, …), a pathspec matches a path when they are
/// equal or the path lies under the pathspec directory. `.` matches everything.
fn matches_pathspec(path: &[u8], pathspecs: &[Vec<u8>]) -> bool {
    pathspecs.iter().any(|spec| {
        let spec: &[u8] = spec.strip_suffix(b"/").unwrap_or(spec.as_slice());
        if spec.is_empty() || spec == b"." {
            return true;
        }
        path == spec
            || (path.len() > spec.len() && path.starts_with(spec) && path[spec.len()] == b'/')
    })
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
    // git abbreviates the `index` line to `core.abbrev` (default auto), and pads
    // the all-zero/submodule side to that same width — never a hardcoded 7.
    let abbrev = crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex())
        .max(MINIMUM_ABBREV);
    if id.is_null() {
        return Ok("0".repeat(abbrev));
    }
    if plain {
        return Ok(id.to_hex_with_len(abbrev).to_string());
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

// ---------------------------------------------------------------------------
// Dates (shared with `git log`; see log.rs for the same machinery)
// ---------------------------------------------------------------------------

/// The `log.date` / `--date=<mode>` output modes rendered byte-for-byte, plus
/// `relative`, measured against the current wall clock. The remaining zone- or
/// process-time-dependent modes (`human`, `local`) are rejected rather than faked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DateMode {
    /// git's `DATE_NORMAL`: `Www Mmm D HH:MM:SS YYYY +ZZZZ`.
    Default,
    /// `short`: `YYYY-MM-DD`.
    Short,
    /// `iso`/`iso8601`: `YYYY-MM-DD HH:MM:SS +ZZZZ`.
    Iso,
    /// `iso-strict`/`iso8601-strict`: `YYYY-MM-DDTHH:MM:SS+ZZ:ZZ`.
    IsoStrict,
    /// `rfc`/`rfc2822`: `Www, D Mmm YYYY HH:MM:SS +ZZZZ`.
    Rfc,
    /// `unix`: the raw epoch seconds, no timezone.
    Unix,
    /// `raw`: `<seconds> +ZZZZ`.
    Raw,
    /// `relative`: `N <unit> ago`, measured against the current time.
    Relative,
}

/// Map a `log.date` / `--date=` value to a [`DateMode`]. `None` for a value git
/// accepts but renders time/zone-dependently (surfaced terse) or does not know.
fn parse_date_mode(spec: &str) -> Option<DateMode> {
    Some(match spec {
        "default" | "normal" => DateMode::Default,
        "short" => DateMode::Short,
        "iso" | "iso8601" => DateMode::Iso,
        "iso-strict" | "iso8601-strict" => DateMode::IsoStrict,
        "rfc" | "rfc2822" => DateMode::Rfc,
        "unix" => DateMode::Unix,
        "raw" => DateMode::Raw,
        "relative" => DateMode::Relative,
        _ => return None,
    })
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Format a timestamp in the requested [`DateMode`], matching git byte-for-byte.
fn format_date(seconds: i64, offset: i32, mode: DateMode) -> String {
    match mode {
        DateMode::Default => format_git_date(seconds, offset),
        DateMode::Relative => format_relative(seconds, now_secs()),
        DateMode::Unix => format!("{seconds}"),
        DateMode::Raw => {
            let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
            format!("{seconds} {sign}{:02}{:02}", off / 3600, (off % 3600) / 60)
        }
        DateMode::Short | DateMode::Iso | DateMode::IsoStrict | DateMode::Rfc => {
            let local = seconds + offset as i64;
            let days = local.div_euclid(86_400);
            let secs = local.rem_euclid(86_400);
            let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);
            let weekday = ((days.rem_euclid(7)) + 4).rem_euclid(7) as usize;
            let (year, month, day) = civil_from_days(days);
            let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
            let (oh, om) = (off / 3600, (off % 3600) / 60);
            match mode {
                DateMode::Short => format!("{year}-{month:02}-{day:02}"),
                DateMode::Iso => format!(
                    "{year}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02} {sign}{oh:02}{om:02}"
                ),
                DateMode::IsoStrict => {
                    // git's `iso-strict` renders a zero (UTC) offset as `Z`, not
                    // `+00:00`; a non-zero offset uses the `±HH:MM` form.
                    let zone = if offset == 0 {
                        "Z".to_string()
                    } else {
                        format!("{sign}{oh:02}:{om:02}")
                    };
                    format!("{year}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}{zone}")
                }
                DateMode::Rfc => format!(
                    "{}, {day} {} {year} {hour:02}:{min:02}:{sec:02} {sign}{oh:02}{om:02}",
                    WEEKDAYS[weekday],
                    MONTHS[(month - 1) as usize],
                ),
                _ => unreachable!(),
            }
        }
    }
}

/// git's default (`DATE_NORMAL`) commit-time rendering: `Www Mmm D HH:MM:SS YYYY
/// +ZZZZ` in the commit's own timezone. The day is an unpadded decimal (git's
/// `%d`), matching a single-digit day to one space — unlike a `%e`-style pad.
fn format_git_date(seconds: i64, offset: i32) -> String {
    let local = seconds + offset as i64;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    // 1970-01-01 (day 0) was a Thursday, index 4 with Sunday = 0.
    let weekday = ((days.rem_euclid(7)) + 4).rem_euclid(7) as usize;
    let (year, month, day) = civil_from_days(days);
    let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
    let (off_h, off_m) = (off / 3600, (off % 3600) / 60);
    format!(
        "{} {} {} {:02}:{:02}:{:02} {} {}{:02}{:02}",
        WEEKDAYS[weekday],
        MONTHS[(month - 1) as usize],
        day,
        hour,
        min,
        sec,
        year,
        sign,
        off_h,
        off_m,
    )
}

/// Convert a day count since the Unix epoch into a civil `(year, month, day)`,
/// month and day 1-based (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month as u32, day)
}

/// Current time in epoch seconds, for relative dates.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// git's `show_date_relative`: render `then` as "N units ago" relative to `now`,
/// with the same unit thresholds and rounding.
fn format_relative(then: i64, now: i64) -> String {
    if now < then {
        return "in the future".to_string();
    }
    let mut diff = (now - then) as u64;
    if diff < 90 {
        return unit_ago(diff, "second");
    }
    diff = (diff + 30) / 60; // minutes
    if diff < 90 {
        return unit_ago(diff, "minute");
    }
    diff = (diff + 30) / 60; // hours
    if diff < 36 {
        return unit_ago(diff, "hour");
    }
    diff = (diff + 12) / 24; // days
    if diff < 14 {
        return unit_ago(diff, "day");
    }
    if diff < 70 {
        return unit_ago((diff + 3) / 7, "week");
    }
    if diff < 365 {
        return unit_ago((diff + 15) / 30, "month");
    }
    if diff < 1825 {
        let totalmonths = diff * 12 * 10 / 365;
        let years = totalmonths / 120;
        let months = (totalmonths % 120) / 10;
        if months > 0 {
            return format!("{}, {} ago", unit(years, "year"), unit(months, "month"));
        }
        return unit_ago(years, "year");
    }
    unit_ago((diff + 183) / 365, "year")
}

/// `"N unit ago"` / `"N units ago"` with git's singular/plural rule.
fn unit_ago(n: u64, name: &str) -> String {
    format!("{} ago", unit(n, name))
}

/// `"1 unit"` or `"N units"`.
fn unit(n: u64, name: &str) -> String {
    if n == 1 {
        format!("1 {name}")
    } else {
        format!("{n} {name}s")
    }
}
