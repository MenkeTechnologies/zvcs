//! `git cherry` — report which commits in `<limit>..<head>` have no
//! patch-equivalent in `<head>..<upstream>`.
//!
//! A port of `cmd_cherry()` (`builtin/log.c`) together with the parts of
//! `patch-ids.c` and `diff_get_patch_id()` (`diff.c`) it depends on. The two
//! revision walks, the `max_parents = 1` filter, the "head and upstream are the
//! same object" short circuit and the reversal of the listed commits are
//! reproduced step for step, so stdout matches stock git byte for byte:
//! `<sign> <object-name>` per commit, oldest first, with the subject appended
//! under `-v`.
//!
//! ### Equivalence
//!
//! Two commits are equivalent when their patch IDs match. The ID is *never*
//! printed by `cherry` — only the `+`/`-` sign it decides — so what has to hold
//! is that the same relation is computed, not that the same digest is. The
//! header portion (`diff--git`, `a/`/`b/` paths, `newfilemode` / `deletedfilemode`
//! / `oldmode` + `newmode`, the `---`/`+++` markers, and the object names of a
//! binary change) is hashed exactly as `diff_get_patch_id()` orders it, and the
//! body is the unified diff at context 3 with hunk headers suppressed
//! (upstream's `XDL_EMIT_NO_HUNK_HDR`), every line whitespace-stripped by git's
//! own `isspace` set, `\ No newline at end of file` lines dropped — the rule in
//! `patch_id_consume()`.
//!
//! The one deliberate divergence: the diff body comes from `gix-diff`'s Myers
//! implementation with slider heuristics rather than upstream's `xdiff`. Those
//! agree on hunk content for the inputs that matter here — a commit and its
//! cherry-pick produce identical diff text under either — but a digest computed
//! by this module is not interchangeable with one from `git patch-id`.
//!
//! ### Covered
//!
//! * `cherry [<upstream> [<head> [<limit>]]]`, with `<head>` defaulting to
//!   `HEAD` and `<upstream>` to the current branch's tracked remote branch
//!   (including `branch.<name>.remote = .`)
//! * `-v` / `--verbose` / `--no-verbose` — append the one-line subject
//! * `--abbrev` / `--abbrev=<n>` / `--no-abbrev`, with git's clamping to
//!   `[4, hexsz]`, `0` meaning the full name, and disambiguation against the
//!   object database
//! * `--` to end option parsing; more than three positionals fall through to the
//!   tracked-branch lookup, exactly as upstream's `switch (argc)` default arm
//! * exit codes: `0` normally, `128` for `fatal: unknown commit <rev>`, `129`
//!   for `-h`, an unknown option, and the missing-upstream usage
//!
//! ### Not covered
//!
//! * `--help` (upstream renders the man page) — bails rather than fake it
//! * `i18n.logOutputEncoding` re-encoding of the `-v` subject
//! * `.gitattributes` `-diff` / `core.bigFileThreshold` as binary triggers; only
//!   the NUL-byte scan of `buffer_is_binary()` is applied
//! * textconv filters, which upstream's patch-id path also ignores

use anyhow::{anyhow, bail, Result};
use std::collections::HashSet;
use std::io::Write;
use std::process::ExitCode;

use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::{Hasher, ObjectId};
use gix::object::tree::diff::ChangeDetached;
use gix::prelude::ObjectIdExt;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// git's own usage block for `cherry`, reproduced verbatim.
const USAGE: &str = "\
usage: git cherry [-v] [<upstream> [<head> [<limit>]]]

    --[no-]abbrev[=<n>]   use <n> digits to display object names
    -v, --[no-]verbose    be verbose

";

/// `MINIMUM_ABBREV` — the shortest object-name prefix git will print.
const MINIMUM_ABBREV: usize = 4;

/// How the object names are rendered, mirroring upstream's single `int abbrev`
/// with its `0` / `DEFAULT_ABBREV` / explicit-length encoding.
#[derive(Clone, Copy)]
enum Abbrev {
    /// `0` — the full object name; also what `--no-abbrev` selects.
    Full,
    /// `DEFAULT_ABBREV` — whatever `core.abbrev` resolves to, `auto` by default.
    Configured,
    /// An explicit `--abbrev=<n>`, already clamped to `[MINIMUM_ABBREV, hexsz]`.
    Len(usize),
}

/// `git cherry` — find commits yet to be applied upstream.
pub fn cherry(args: &[String]) -> Result<ExitCode> {
    // `run_builtin()` answers a lone `-h` before `setup_git_directory()`, so this
    // form works outside a repository.
    if args.len() == 2 && args[1] == "-h" {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;
    let hexsz = repo.object_hash().len_in_hex();

    let mut verbose = false;
    let mut abbrev = Abbrev::Full;
    let mut positional: Vec<&str> = Vec::new();
    let mut end_of_options = false;

    for a in args.iter() {
        let a = a.as_str();
        if end_of_options {
            positional.push(a);
            continue;
        }
        match a {
            "--" => end_of_options = true,
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "--abbrev" => abbrev = Abbrev::Configured,
            "--no-abbrev" => abbrev = Abbrev::Full,
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--help" => bail!(
                "unsupported flag \"--help\" (ported: -h, -v, --verbose, --no-verbose, \
                 --abbrev, --abbrev=<n>, --no-abbrev)"
            ),
            _ if a.starts_with("--abbrev=") => {
                match parse_abbrev(&a["--abbrev=".len()..], hexsz) {
                    Some(v) => abbrev = v,
                    None => {
                        eprintln!("error: option `abbrev' expects a numerical value");
                        eprint!("{USAGE}");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if a.starts_with("--") => {
                eprintln!("error: unknown option `{}'", &a[2..]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // A bare "-" is a non-option to git's parse_options, like any other.
            _ if a.starts_with('-') && a.len() > 1 => {
                // Short options bundle: only `-v` exists, so any other letter is
                // an unknown switch.
                let bad = a[1..].chars().find(|&c| c != 'v');
                match bad {
                    Some(c) => {
                        eprintln!("error: unknown switch `{c}'");
                        eprint!("{USAGE}");
                        return Ok(ExitCode::from(129));
                    }
                    None => verbose = true,
                }
            }
            _ => positional.push(a),
        }
    }

    // Upstream's `switch (argc)`: one, two or three revisions, and anything else
    // (none, or more than three) falls back to the tracked remote branch.
    let (upstream, head, limit) = match positional.as_slice() {
        [u] => (u.to_string(), "HEAD".to_string(), None),
        [u, h] => (u.to_string(), h.to_string(), None),
        [u, h, l] => (u.to_string(), h.to_string(), Some(l.to_string())),
        _ => match tracked_upstream(&repo)? {
            Some(u) => (u, "HEAD".to_string(), None),
            None => {
                eprintln!(
                    "Could not find a tracked remote branch, please specify <upstream> manually."
                );
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        },
    };

    // `add_pending_commit()` order: head first, then upstream, then the limit.
    let Some(head_id) = resolve_commit(&repo, &head) else {
        eprintln!("fatal: unknown commit {head}");
        return Ok(ExitCode::from(128));
    };
    let Some(upstream_id) = resolve_commit(&repo, &upstream) else {
        eprintln!("fatal: unknown commit {upstream}");
        return Ok(ExitCode::from(128));
    };

    // "Don't say anything if head and upstream are the same."
    if head_id == upstream_id {
        return Ok(ExitCode::SUCCESS);
    }

    // `get_patch_ids()`: the patch IDs of `head..upstream`, non-merges only. The
    // limit is applied only to the listing walk, never to this one.
    let mut ids: HashSet<ObjectId> = HashSet::new();
    for id in walk(&repo, &[upstream_id], &[head_id])? {
        if let Some(pid) = commit_patch_id(&repo, id)? {
            ids.insert(pid);
        }
    }

    let mut hidden = vec![upstream_id];
    if let Some(limit) = &limit {
        let Some(limit_id) = resolve_commit(&repo, limit) else {
            eprintln!("fatal: unknown commit {limit}");
            return Ok(ExitCode::from(128));
        };
        hidden.push(limit_id);
    }

    // The listing walk yields newest first; upstream reverses it by pushing each
    // commit onto the front of a list, so the output runs oldest first.
    let mut list = walk(&repo, &[head_id], &hidden)?;
    list.reverse();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for id in list {
        let equivalent = match commit_patch_id(&repo, id)? {
            Some(pid) => ids.contains(&pid),
            None => false,
        };
        let sign = if equivalent { b'-' } else { b'+' };
        out.write_all(&[sign, b' '])?;
        out.write_all(format_id(&repo, id, abbrev, hexsz).as_bytes())?;
        if verbose {
            out.write_all(b" ")?;
            out.write_all(&subject_of(&repo, id)?)?;
        }
        out.write_all(b"\n")?;
    }
    out.flush()?;

    Ok(ExitCode::SUCCESS)
}

/// Port of `parse_opt_abbrev_cb()` for an attached `--abbrev=<n>`: a bare number,
/// `0` meaning "print the whole name", anything else clamped to `[4, hexsz]`.
/// `None` reports the non-numeric case upstream turns into a usage error.
fn parse_abbrev(value: &str, hexsz: usize) -> Option<Abbrev> {
    let n: i64 = value.parse().ok()?;
    Some(if n == 0 {
        Abbrev::Full
    } else if n < MINIMUM_ABBREV as i64 {
        Abbrev::Len(MINIMUM_ABBREV)
    } else if n > hexsz as i64 {
        Abbrev::Full
    } else {
        Abbrev::Len(n as usize)
    })
}

/// Port of `branch_get_upstream()`: the remote-tracking ref of the checked-out
/// branch, or `None` on a detached HEAD or an unconfigured branch.
///
/// `branch.<name>.remote = .` names the local repository, for which `set_merge()`
/// keeps `branch.<name>.merge` itself as the upstream instead of mapping it
/// through a fetch refspec.
fn tracked_upstream(repo: &gix::Repository) -> Result<Option<String>> {
    let Some(head) = repo.head_ref()? else {
        return Ok(None);
    };
    let name = head.name();
    let short = name.shorten().to_string();
    let snapshot = repo.config_snapshot();

    let remote_key = format!("branch.{short}.remote");
    let is_local = snapshot
        .string(&remote_key)
        .is_some_and(|v| v.as_slice() == b".");
    if is_local {
        let merge_key = format!("branch.{short}.merge");
        return Ok(snapshot.string(&merge_key).map(|v| v.to_string()));
    }

    match repo.branch_remote_tracking_ref_name(name, gix::remote::Direction::Fetch) {
        Some(Ok(r)) => Ok(Some(r.as_bstr().to_string())),
        Some(Err(e)) => Err(anyhow!("{e}")),
        None => Ok(None),
    }
}

/// `repo_get_oid()` followed by `lookup_commit_reference()`: resolve a revision
/// and peel it to a commit, or report the failure the caller turns into
/// `fatal: unknown commit`.
fn resolve_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    Some(object.peel_to_commit().ok()?.id)
}

/// A revision walk over `tips` excluding everything reachable from `hidden`,
/// keeping only the commits `revs.max_parents = 1` admits — that is, dropping
/// merges while leaving root commits in. The filter is on what the walk
/// *returns*, never on where it goes, matching `get_commit_action()`.
fn walk(repo: &gix::Repository, tips: &[ObjectId], hidden: &[ObjectId]) -> Result<Vec<ObjectId>> {
    let platform = repo
        .rev_walk(tips.iter().copied())
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .with_hidden(hidden.iter().copied());

    let mut out = Vec::new();
    for info in platform.all()? {
        let id = info?.id;
        if repo.find_commit(id)?.parent_ids().count() <= 1 {
            out.push(id);
        }
    }
    Ok(out)
}

/// Port of `commit_patch_id()`: hash the diff of a commit against its first
/// parent (against the empty tree for a root commit). `None` for a merge, which
/// `patch_id_defined()` refuses.
fn commit_patch_id(repo: &gix::Repository, id: ObjectId) -> Result<Option<ObjectId>> {
    let commit = repo.find_commit(id)?;
    let mut parents = commit.parent_ids();
    let first = parents.next();
    if parents.next().is_some() {
        return Ok(None);
    }

    let new_tree = commit.tree()?;
    let old_tree = match first {
        Some(p) => Some(p.object()?.try_into_commit()?.tree()?),
        None => None,
    };

    // `init_patch_ids()` turns rename detection off and recursion on, which is
    // what `Options::default()` already is.
    let mut changes =
        repo.diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), gix::diff::Options::default())?;
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));

    let mut ctx = gix::hash::hasher(repo.object_hash());
    for change in &changes {
        hash_change(repo, &mut ctx, change)?;
    }
    let digest = ctx
        .try_finalize()
        .map_err(|e| anyhow!("hashing the patch of {id}: {e}"))?;
    Ok(Some(digest))
}

/// Port of one loop iteration of `diff_get_patch_id()`: the header fields in
/// upstream's order, then either the two object names of a binary change or the
/// `---`/`+++` markers followed by the diff body.
fn hash_change(repo: &gix::Repository, ctx: &mut Hasher, change: &ChangeDetached) -> Result<()> {
    let path = change_path(change);
    ctx.update(b"diff--git");
    ctx.update(b"a/");
    update_stripped(ctx, path);
    ctx.update(b"b/");
    update_stripped(ctx, path);

    // The `(oid, is-gitlink)` of each side; `None` is upstream's mode-0 "this
    // side does not exist".
    let (old, new) = match change {
        ChangeDetached::Addition {
            entry_mode, id, ..
        } => {
            ctx.update(b"newfilemode");
            update_mode(ctx, entry_mode.value());
            (None, Some((*id, entry_mode.is_commit())))
        }
        ChangeDetached::Deletion {
            entry_mode, id, ..
        } => {
            ctx.update(b"deletedfilemode");
            update_mode(ctx, entry_mode.value());
            (Some((*id, entry_mode.is_commit())), None)
        }
        ChangeDetached::Modification {
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => {
            if previous_entry_mode.value() != entry_mode.value() {
                ctx.update(b"oldmode");
                update_mode(ctx, previous_entry_mode.value());
                ctx.update(b"newmode");
                update_mode(ctx, entry_mode.value());
            }
            (
                Some((*previous_id, previous_entry_mode.is_commit())),
                Some((*id, entry_mode.is_commit())),
            )
        }
        // Never produced: rewrite tracking is off via `Options::default()`.
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    };

    let old_content = match old {
        Some((id, is_sub)) => content_of(repo, id, is_sub)?,
        None => Vec::new(),
    };
    let new_content = match new {
        Some((id, is_sub)) => content_of(repo, id, is_sub)?,
        None => Vec::new(),
    };

    if is_binary(&old_content) || is_binary(&new_content) {
        // `diff_fill_oid_info()` leaves the absent side's name all zeroes.
        let null = repo.object_hash().null();
        let one = old.map_or(null, |(id, _)| id);
        let two = new.map_or(null, |(id, _)| id);
        ctx.update(one.to_hex().to_string().as_bytes());
        ctx.update(two.to_hex().to_string().as_bytes());
        return Ok(());
    }

    match (old.is_some(), new.is_some()) {
        (false, _) => {
            ctx.update(b"---/dev/null");
            ctx.update(b"+++b/");
            update_stripped(ctx, path);
        }
        (_, false) => {
            ctx.update(b"---a/");
            update_stripped(ctx, path);
            ctx.update(b"+++/dev/null");
        }
        _ => {
            ctx.update(b"---a/");
            update_stripped(ctx, path);
            ctx.update(b"+++b/");
            update_stripped(ctx, path);
        }
    }

    hash_body(ctx, &old_content, &new_content)?;
    Ok(())
}

/// Hash the unified diff of two blobs at context 3, without hunk headers.
fn hash_body(ctx: &mut Hasher, old: &[u8], new: &[u8]) -> Result<()> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let sink = PatchIdSink {
        ctx,
        line: Vec::new(),
    };
    UnifiedDiff::new(&diff, &input, sink, ContextSize::symmetrical(3)).consume()?;
    Ok(())
}

/// Port of `patch_id_consume()`: every emitted diff line, whitespace-stripped,
/// folded into the running digest. Hunk headers never reach it because upstream
/// sets `XDL_EMIT_NO_HUNK_HDR`, and the `\ No newline at end of file` marker is
/// dropped by the `len > 12 && starts_with("\\ ")` test — so neither is emitted
/// here either.
struct PatchIdSink<'a> {
    ctx: &'a mut Hasher,
    /// Reused buffer for the stripped `<prefix><content>` of one line.
    line: Vec<u8>,
}

impl ConsumeHunk for PatchIdSink<'_> {
    type Out = ();

    fn consume_hunk(
        &mut self,
        _header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        for &(kind, content) in lines {
            let prefix = match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            };
            self.line.clear();
            // `remove_space()` runs over the whole line, prefix included, so a
            // context line's leading space is dropped along with the rest.
            if !is_git_space(prefix) {
                self.line.push(prefix);
            }
            self.line
                .extend(content.iter().copied().filter(|&c| !is_git_space(c)));
            self.ctx.update(&self.line);
        }
        Ok(())
    }

    fn finish(self) {}
}

/// git's `isspace`, which `git-compat-util.h` redefines over `ctype.c`'s
/// `sane_ctype` table: `GIT_SPACE` covers only tab, newline, carriage return and
/// space. Vertical tab and form feed are kept.
fn is_git_space(c: u8) -> bool {
    matches!(c, b'\t' | b'\n' | b'\r' | b' ')
}

/// `remove_space()` applied while feeding the digest.
fn update_stripped(ctx: &mut Hasher, bytes: &[u8]) {
    let stripped: Vec<u8> = bytes.iter().copied().filter(|&c| !is_git_space(c)).collect();
    ctx.update(&stripped);
}

/// Port of `patch_id_add_mode()`: the mode as six octal digits.
fn update_mode(ctx: &mut Hasher, mode: u16) {
    ctx.update(format!("{mode:06o}").as_bytes());
}

/// `buffer_is_binary()`: a NUL in the first 8000 bytes.
fn is_binary(content: &[u8]) -> bool {
    content.iter().take(8000).any(|&b| b == 0)
}

/// The bytes `diff_populate_filespec()` hands the diff machinery. A gitlink has
/// no blob, so upstream synthesizes the `Subproject commit` line instead.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// The path of a change, for a stable diff order.
fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

/// Render an object name the way `repo_find_unique_abbrev()` would: the full
/// name for `0`, otherwise the shortest unambiguous prefix at least as long as
/// the requested length.
fn format_id(repo: &gix::Repository, id: ObjectId, abbrev: Abbrev, hexsz: usize) -> String {
    match abbrev {
        Abbrev::Full => id.to_hex().to_string(),
        Abbrev::Configured => id
            .attach(repo)
            .shorten()
            .map_or_else(|_| id.to_hex().to_string(), |p| p.to_string()),
        Abbrev::Len(n) if n >= hexsz => id.to_hex().to_string(),
        Abbrev::Len(n) => {
            let Ok(candidate) = gix::odb::store::prefix::disambiguate::Candidate::new(id, n) else {
                return id.to_hex().to_string();
            };
            match repo.objects.disambiguate_prefix(candidate) {
                Ok(Some(prefix)) => prefix.to_string(),
                _ => id.to_hex().to_string(),
            }
        }
    }
}

/// `pp_commit_easy(CMIT_FMT_ONELINE, ...)`: the first paragraph of the message
/// joined into one line with spaces, then right-trimmed.
fn subject_of(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>> {
    let commit = repo.find_commit(id)?;
    let message = commit.message_raw()?.to_vec();
    let mut subject = format_subject(skip_blank_lines(&message));
    while subject
        .last()
        .is_some_and(|&b| b.is_ascii_whitespace())
    {
        subject.pop();
    }
    Ok(subject)
}

/// git's `get_one_line()`: the length of the next line, newline included.
fn one_line(msg: &[u8]) -> usize {
    match msg.iter().position(|&b| b == b'\n') {
        Some(i) => i + 1,
        None => msg.len(),
    }
}

/// Strip trailing ASCII whitespace (git's `isspace` set).
fn trim_end_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last.is_ascii_whitespace() {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

/// git's `skip_blank_lines()`: advance past leading blank lines.
fn skip_blank_lines(mut msg: &[u8]) -> &[u8] {
    loop {
        let len = one_line(msg);
        if len == 0 || !trim_end_ws(&msg[..len]).is_empty() {
            return msg;
        }
        msg = &msg[len..];
    }
}

/// git's `format_subject()` with a `" "` separator: join the first paragraph
/// into a single line.
fn format_subject(mut msg: &[u8]) -> Vec<u8> {
    let mut title: Vec<u8> = Vec::new();
    let mut first = true;
    loop {
        let len = one_line(msg);
        if len == 0 {
            break;
        }
        let trimmed = trim_end_ws(&msg[..len]);
        if trimmed.is_empty() {
            break;
        }
        msg = &msg[len..];
        if !first {
            title.push(b' ');
        }
        title.extend_from_slice(trimmed);
        first = false;
    }
    title
}
